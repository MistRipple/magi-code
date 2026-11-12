use super::*;

impl SessionStore {
    pub(super) fn ownership_from_active_execution_chain(
        chain: &ActiveExecutionChain,
    ) -> ExecutionOwnership {
        let primary_branch = chain.branches.iter().find(|branch| branch.is_primary);
        ExecutionOwnership {
            session_id: Some(chain.session_id.clone()),
            workspace_id: chain.workspace_id.clone(),
            mission_id: Some(chain.mission_id.clone()),
            task_id: primary_branch
                .map(|branch| branch.task_id.clone())
                .or_else(|| Some(chain.root_task_id.clone())),
            worker_id: primary_branch.map(|branch| branch.worker_id.clone()),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        }
    }

    pub(super) fn derive_sidecar_status(
        ownership: &ExecutionOwnership,
        recovery_id: Option<&str>,
        existing_status: Option<&SessionExecutionSidecarStatus>,
    ) -> SessionExecutionSidecarStatus {
        let has_ownership = [
            ownership.session_id.is_some(),
            ownership.workspace_id.is_some(),
            ownership.mission_id.is_some(),
            ownership.task_id.is_some(),
            ownership.worker_id.is_some(),
            ownership.execution_chain_ref.is_some(),
        ]
        .into_iter()
        .any(|field| field);

        if !has_ownership {
            if recovery_id.is_some() {
                SessionExecutionSidecarStatus::RecoveryLinked
            } else {
                SessionExecutionSidecarStatus::Detached
            }
        } else if matches!(
            existing_status,
            Some(SessionExecutionSidecarStatus::Resumed)
        ) {
            SessionExecutionSidecarStatus::Resumed
        } else if recovery_id.is_some() {
            SessionExecutionSidecarStatus::RecoveryLinked
        } else {
            SessionExecutionSidecarStatus::Bound
        }
    }

    pub(super) fn build_active_execution_chain_sidecar(
        session_id: SessionId,
        mut active_execution_chain: ActiveExecutionChain,
        existing: Option<SessionRuntimeSidecar>,
        current_turn: Option<ActiveExecutionTurn>,
    ) -> DomainResult<SessionRuntimeSidecar> {
        if active_execution_chain.session_id != session_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session_runtime_sidecar 的 active_execution_chain.session_id 与 session_id 不一致: {} != {}",
                    active_execution_chain.session_id, session_id
                ),
            });
        }
        active_execution_chain.normalize();
        let recovery_id = existing
            .as_ref()
            .and_then(|sidecar| sidecar.recovery_id.clone());
        let incoming_turn_id = current_turn.as_ref().map(|turn| turn.turn_id.as_str());
        if active_execution_chain.current_turn_id.as_deref() != incoming_turn_id {
            return Err(DomainError::InvalidState {
                message: "执行链与当前 Turn 归属不一致".into(),
            });
        }
        reject_conflicting_active_current_turn(
            &session_id,
            existing
                .as_ref()
                .and_then(|sidecar| sidecar.current_turn.as_ref()),
            incoming_turn_id,
        )?;
        let ownership = Self::ownership_from_active_execution_chain(&active_execution_chain);
        let active_execution_chain = Some(active_execution_chain);
        let status = Self::derive_sidecar_status(
            &ownership,
            recovery_id.as_deref(),
            existing.as_ref().map(|sidecar| &sidecar.status),
        );
        Ok(SessionRuntimeSidecar {
            session_id,
            ownership,
            recovery_id,
            current_turn,
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        })
    }

    // ---------------------------------------------------------------------
    // P6a Thread registry（Y 方案）
    // ---------------------------------------------------------------------

    #[cfg(any(test, feature = "test-support"))]
    pub fn bind_execution_ownership_for_test(
        &self,
        session_id: SessionId,
        ownership: ExecutionOwnership,
    ) {
        // 与 Turn 的 prepare → persist → apply 串行化，避免其中一方覆盖另一方的更新。
        let _canonical_barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical commit barrier poisoned");
        let session_lock = self.canonical_commit_lock_for(&session_id);
        let _session_guard = session_lock
            .lock()
            .expect("canonical session commit lock poisoned");
        let dirty_session_id = session_id.clone();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let existing = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| sidecar.session_id == session_id)
            .map(|sidecar| state.project_sidecar(sidecar));
        let recovery_id = existing
            .as_ref()
            .and_then(|sidecar| sidecar.recovery_id.clone());
        let current_turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let requested_workspace_id = ownership.workspace_id.clone().or_else(|| {
            existing
                .as_ref()
                .and_then(|sidecar| sidecar.ownership.workspace_id.clone())
        });
        let mut active_execution_chain = existing
            .as_ref()
            .and_then(|sidecar| sidecar.active_execution_chain.clone());
        if let Some(chain) = active_execution_chain.as_mut()
            && chain.workspace_id.is_none()
        {
            chain.workspace_id = requested_workspace_id;
        }
        let ownership = if let Some(chain) = active_execution_chain.as_ref() {
            Self::ownership_from_active_execution_chain(chain)
        } else {
            ExecutionOwnership {
                execution_chain_ref: ownership.execution_chain_ref.clone().or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|sidecar| sidecar.ownership.execution_chain_ref.clone())
                }),
                ..ownership
            }
        };
        let status = Self::derive_sidecar_status(
            &ownership,
            recovery_id.as_deref(),
            existing.as_ref().map(|sidecar| &sidecar.status),
        );
        upsert_runtime_sidecar_in_state(
            &mut state,
            SessionRuntimeSidecar {
                session_id,
                ownership,
                recovery_id,
                current_turn,
                active_execution_chain,
                status,
                updated_at: UtcMillis::now(),
            },
        );
        drop(state);
        self.mark_sidecar_dirty_for_session(
            &dirty_session_id,
            SessionSidecarFlushReason::BindExecutionOwnership,
        );
    }

    pub fn accept_current_turn_with_timeline_entry(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        mut turn: ActiveExecutionTurn,
    ) -> DomainResult<(String, SessionRuntimeSidecar)> {
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
        turn.normalize();
        let updated = self.commit_canonical_transaction(
            &session_id,
            |state| {
                if !state
                    .sessions
                    .iter()
                    .any(|session| session.session_id == session_id)
                {
                    return Err(DomainError::NotFound { entity: "session" });
                }
                let existing = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| sidecar.session_id == session_id)
                    .map(|sidecar| state.project_sidecar(sidecar));
                reject_conflicting_active_current_turn(
                    &session_id,
                    existing
                        .as_ref()
                        .and_then(|sidecar| sidecar.current_turn.as_ref()),
                    Some(turn.turn_id.as_str()),
                )?;
                reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;

                let (ownership, recovery_id, mut active_execution_chain, status) =
                    if let Some(existing) = existing {
                        (
                            existing.ownership,
                            existing.recovery_id,
                            existing.active_execution_chain,
                            existing.status,
                        )
                    } else {
                        (
                            ExecutionOwnership {
                                session_id: Some(session_id.clone()),
                                ..ExecutionOwnership::default()
                            },
                            None,
                            None,
                            SessionExecutionSidecarStatus::Detached,
                        )
                    };
                if let Some(chain) = active_execution_chain.as_mut() {
                    // Continue 在接纳事务内更新所属 Turn 身份；链不保存 Turn 正文。
                    chain.current_turn_id = Some(turn.turn_id.clone());
                    chain.normalize();
                }
                let updated = SessionRuntimeSidecar {
                    session_id: session_id.clone(),
                    ownership,
                    recovery_id,
                    current_turn: Some(turn.clone()),
                    active_execution_chain,
                    status,
                    updated_at: UtcMillis::now(),
                };
                let canonical_plan = Self::canonical_turn_commit_plan(
                    state,
                    &session_id,
                    updated.current_turn.as_ref().expect("current turn was set"),
                    None,
                )?;
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: updated,
                    acceptance: None,
                })
            },
            |state, updated| {
                state.timeline.push(TimelineEntry {
                    entry_id: entry_id.clone(),
                    session_id: session_id.clone(),
                    kind: kind.clone(),
                    message: message.clone(),
                    occurred_at,
                });
                if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.session_id == session_id)
                {
                    session.updated_at = occurred_at;
                }
                upsert_runtime_sidecar_in_state(state, updated);
                state
                    .runtime_sidecar(&session_id)
                    .expect("committed sidecar")
            },
        )?;
        self.mark_sidecar_dirty_for_session(
            &session_id,
            SessionSidecarFlushReason::AcceptCurrentTurn,
        );
        Ok((entry_id, updated))
    }

    /// 接受一个不属于 TaskStore 的 Conversation Turn。
    ///
    /// Conversation Turn 与任务 Turn 共用 canonical 写模型，但不会继承旧任务的
    /// execution chain、lease 或 recovery ownership。该边界必须在同一笔 canonical
    /// transaction 内完成，不能先清理旧 ownership 再调用通用 accepted 方法。
    pub fn accept_conversation_turn_with_timeline_entry(
        &self,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        timeline_entry: TimelineEntryInput,
        mut turn: ActiveExecutionTurn,
    ) -> DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn)> {
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
        turn.normalize();
        let (updated, canonical_turn) = self.commit_canonical_transaction(
            &session_id,
            |state| {
                if !state
                    .sessions
                    .iter()
                    .any(|session| session.session_id == session_id)
                {
                    return Err(DomainError::NotFound { entity: "session" });
                }
                let existing = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| sidecar.session_id == session_id)
                    .map(|sidecar| state.project_sidecar(sidecar));
                reject_conflicting_active_current_turn(
                    &session_id,
                    existing
                        .as_ref()
                        .and_then(|sidecar| sidecar.current_turn.as_ref()),
                    Some(turn.turn_id.as_str()),
                )?;
                reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;

                let workspace_id = workspace_id.clone().or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|sidecar| sidecar.ownership.workspace_id.clone())
                });
                let ownership = ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id,
                    ..ExecutionOwnership::default()
                };
                let updated = SessionRuntimeSidecar {
                    session_id: session_id.clone(),
                    ownership,
                    // Conversation Turn 不应沿用任务恢复标记。
                    recovery_id: None,
                    current_turn: Some(turn.clone()),
                    active_execution_chain: None,
                    status: SessionExecutionSidecarStatus::Detached,
                    updated_at: UtcMillis::now(),
                };
                let canonical_plan = Self::canonical_turn_commit_plan(
                    state,
                    &session_id,
                    updated
                        .current_turn
                        .as_ref()
                        .expect("conversation turn was set"),
                    None,
                )?;
                let acceptance = SessionAcceptanceRecord {
                    session: state
                        .sessions
                        .iter()
                        .find(|session| session.session_id == session_id)
                        .expect("session existence was validated")
                        .clone(),
                    timeline_entry: TimelineEntry {
                        entry_id: entry_id.clone(),
                        session_id: session_id.clone(),
                        kind: kind.clone(),
                        message: message.clone(),
                        occurred_at,
                    },
                    superseded_turn: None,
                    canonical_turn: canonical_plan.value.clone(),
                    sidecar: updated.clone().into(),
                };
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: (updated, canonical_plan.value),
                    acceptance: Some((acceptance, None)),
                })
            },
            |state, (updated, canonical_turn)| {
                state.timeline.push(TimelineEntry {
                    entry_id: entry_id.clone(),
                    session_id: session_id.clone(),
                    kind: kind.clone(),
                    message: message.clone(),
                    occurred_at,
                });
                if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.session_id == session_id)
                {
                    session.updated_at = occurred_at;
                }
                upsert_runtime_sidecar_in_state(state, updated);
                (
                    state
                        .runtime_sidecar(&session_id)
                        .expect("committed sidecar"),
                    canonical_turn,
                )
            },
        )?;
        self.mark_sidecar_dirty_for_session(
            &session_id,
            SessionSidecarFlushReason::AcceptCurrentTurn,
        );
        Ok((entry_id, updated, canonical_turn))
    }

    /// 在 Continue 创建新 Turn 前，将仍处于活动态的旧 current Turn 收口为失败。
    ///
    /// 某些恢复场景中根任务已经失败或 daemon 已经停止，但异步终态回调尚未把
    /// session Turn 更新为终态。Continue 不能把用户输入写进一个仍被视为活动的旧
    /// Turn，也不能无条件覆盖另一个并发产生的新 Turn，因此必须带上调用方观察到的
    /// Turn ID 做原子归属校验。
    pub fn finalize_current_turn_for_continue(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = self.commit_canonical_transaction(
            session_id,
            |state| {
                let sidecar_index = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .position(|sidecar| &sidecar.session_id == session_id)
                    .ok_or(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    })?;
                let mut candidate = state.project_sidecar(
                    &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                );
                let Some(turn) = candidate.current_turn.as_mut() else {
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: None,
                        acceptance: None,
                    });
                };
                if turn.turn_id != expected_turn_id {
                    return Err(DomainError::CurrentTurnConflict {
                        session_id: session_id.to_string(),
                        active_turn_id: turn.turn_id.clone(),
                    });
                }
                if current_turn_status_is_terminal(&turn.status) {
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: Some((sidecar_index, candidate, false)),
                        acceptance: None,
                    });
                }

                turn.status = "failed".to_string();
                turn.completed_at.get_or_insert_with(UtcMillis::now);
                settle_active_current_turn_items(&mut turn.items, "failed");
                turn.normalize();
                candidate.updated_at = UtcMillis::now();
                let canonical_plan = Self::canonical_turn_commit_plan(
                    state,
                    session_id,
                    candidate
                        .current_turn
                        .as_ref()
                        .expect("current turn was set"),
                    None,
                )?;
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: Some((sidecar_index, candidate, true)),
                    acceptance: None,
                })
            },
            |state, updated| {
                let (sidecar_index, candidate, changed) = updated?;
                if changed {
                    state.execution_sidecar_store.runtime_sidecars[sidecar_index] =
                        candidate.clone().into();
                }
                Some(state.project_sidecar(
                    &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                ))
            },
        )?;
        if updated
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.as_ref())
            .is_some_and(|turn| turn.status == "failed")
        {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::UpdateCurrentTurnStatus,
            );
        }
        Ok(updated)
    }

    pub fn accept_active_execution_chain_with_timeline_entry(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        turn: ActiveExecutionTurn,
    ) -> DomainResult<(String, SessionRuntimeSidecar)> {
        self.accept_active_execution_chain_with_timeline_entry_inner(
            session_id,
            timeline_entry,
            active_execution_chain,
            turn,
            None,
            None,
        )
        .map(|(entry_id, sidecar, _canonical_turn)| (entry_id, sidecar))
    }

    pub fn accept_active_execution_chain_with_timeline_entry_and_task(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        turn: ActiveExecutionTurn,
        task: &magi_core::Task,
    ) -> DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn)> {
        self.accept_active_execution_chain_with_timeline_entry_inner(
            session_id,
            timeline_entry,
            active_execution_chain,
            turn,
            None,
            Some(task),
        )
    }

    pub fn accept_goal_continuation_with_timeline_entry(
        &self,
        session_id: SessionId,
        goal_id: &GoalId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        turn: ActiveExecutionTurn,
    ) -> DomainResult<(String, SessionRuntimeSidecar)> {
        self.accept_active_execution_chain_with_timeline_entry_inner(
            session_id,
            timeline_entry,
            active_execution_chain,
            turn,
            Some(goal_id),
            None,
        )
        .map(|(entry_id, sidecar, _canonical_turn)| (entry_id, sidecar))
    }

    pub fn accept_goal_continuation_with_timeline_entry_and_task(
        &self,
        session_id: SessionId,
        goal_id: &GoalId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        turn: ActiveExecutionTurn,
        task: &magi_core::Task,
    ) -> DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn)> {
        self.accept_active_execution_chain_with_timeline_entry_inner(
            session_id,
            timeline_entry,
            active_execution_chain,
            turn,
            Some(goal_id),
            Some(task),
        )
    }

    pub(super) fn accept_active_execution_chain_with_timeline_entry_inner(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        mut turn: ActiveExecutionTurn,
        continuation_goal_id: Option<&GoalId>,
        acceptance_task: Option<&magi_core::Task>,
    ) -> DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn)> {
        turn.normalize();
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
        let continuation_turn_id = active_execution_chain.root_task_id.to_string();
        let (updated, canonical_turn) = self.commit_canonical_transaction(
            &session_id,
            |state| {
                if !state
                    .sessions
                    .iter()
                    .any(|session| session.session_id == session_id)
                {
                    return Err(DomainError::NotFound { entity: "session" });
                }
                if let Some(goal_id) = continuation_goal_id {
                    let goal = state
                        .goals
                        .iter()
                        .find(|goal| goal.session_id == session_id && &goal.goal_id == goal_id)
                        .ok_or(DomainError::NotFound { entity: "goal" })?;
                    if goal.status != GoalStatus::Active {
                        return Err(DomainError::InvalidState {
                            message: "only an active goal can start continuation".to_string(),
                        });
                    }
                    if goal.continuation.phase == GoalContinuationPhase::Running {
                        return Err(DomainError::InvalidState {
                            message: "goal continuation is already running".to_string(),
                        });
                    }
                }
                let existing = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| sidecar.session_id == session_id)
                    .map(|sidecar| state.project_sidecar(sidecar));
                let updated = Self::build_active_execution_chain_sidecar(
                    session_id.clone(),
                    active_execution_chain.clone(),
                    existing,
                    Some(turn.clone()),
                )?;
                reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;

                let mut incoming = prepare_canonical_turn_in_state(state, &session_id, &turn)?;
                if let Some(goal_id) = continuation_goal_id {
                    incoming.metadata.insert(
                        TURN_GOAL_ID_METADATA_KEY.to_string(),
                        Value::String(goal_id.to_string()),
                    );
                }
                let acceptance = acceptance_task.map(|task| {
                    (
                        SessionAcceptanceRecord {
                            session: state
                                .sessions
                                .iter()
                                .find(|session| session.session_id == session_id)
                                .expect("session existence was validated")
                                .clone(),
                            timeline_entry: TimelineEntry {
                                entry_id: entry_id.clone(),
                                session_id: session_id.clone(),
                                kind: kind.clone(),
                                message: message.clone(),
                                occurred_at,
                            },
                            superseded_turn: None,
                            canonical_turn: incoming.clone(),
                            sidecar: updated.clone().into(),
                        },
                        Some(task.clone()),
                    )
                });
                let plan = Self::canonical_prepared_turn_commit_plan(
                    state,
                    &session_id,
                    incoming.clone(),
                    acceptance,
                );
                Ok(CanonicalCommitPlan {
                    mutations: plan.mutations,
                    value: (updated, incoming),
                    acceptance: plan.acceptance,
                })
            },
            |state, (updated, canonical_turn)| {
                state.timeline.push(TimelineEntry {
                    entry_id: entry_id.clone(),
                    session_id: session_id.clone(),
                    kind: kind.clone(),
                    message: message.clone(),
                    occurred_at,
                });
                if let Some(session) = state
                    .sessions
                    .iter_mut()
                    .find(|session| session.session_id == session_id)
                {
                    session.updated_at = occurred_at;
                }
                upsert_runtime_sidecar_in_state(state, updated);
                if let Some(goal_id) = continuation_goal_id
                    && let Some(goal) = state
                        .goals
                        .iter_mut()
                        .find(|goal| goal.session_id == session_id && &goal.goal_id == goal_id)
                {
                    goal.continuation = GoalContinuationState {
                        phase: GoalContinuationPhase::Running,
                        turn_id: Some(continuation_turn_id.clone()),
                        reason: None,
                    };
                    goal.updated_at = occurred_at;
                }
                (
                    state
                        .runtime_sidecar(&session_id)
                        .expect("committed sidecar"),
                    canonical_turn,
                )
            },
        )?;
        self.mark_sidecar_dirty_for_session(
            &session_id,
            SessionSidecarFlushReason::AcceptCurrentTurn,
        );
        Ok((entry_id, updated, canonical_turn))
    }

    pub fn replace_current_turn_with_active_execution_chain_and_timeline_entry_and_task(
        &self,
        session_id: SessionId,
        replace_turn_id: &str,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        mut turn: ActiveExecutionTurn,
        task: &magi_core::Task,
    ) -> DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn, CanonicalTurn)> {
        turn.normalize();
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
        let (updated, superseded_turn, accepted_canonical_turn) = self
            .commit_canonical_transaction(
                &session_id,
                |state| {
                    if !state
                        .sessions
                        .iter()
                        .any(|session| session.session_id == session_id)
                    {
                        return Err(DomainError::NotFound { entity: "session" });
                    }
                    reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;
                    let replaced_turn_index =
                        validate_turn_replacement_target(state, &session_id, replace_turn_id)?;
                    let existing = state
                        .execution_sidecar_store
                        .runtime_sidecars
                        .iter()
                        .find(|sidecar| sidecar.session_id == session_id)
                        .map(|sidecar| state.project_sidecar(sidecar));
                    let updated = Self::build_active_execution_chain_sidecar(
                        session_id.clone(),
                        active_execution_chain.clone(),
                        existing,
                        Some(turn.clone()),
                    )?;
                    let incoming_turn = &turn;
                    if state.canonical_turns.iter().any(|existing| {
                        existing.session_id == session_id
                            && existing.turn_id == incoming_turn.turn_id
                    }) {
                        return Err(DomainError::InvalidState {
                            message: format!("canonical turn {} 已存在", incoming_turn.turn_id),
                        });
                    }
                    let incoming_canonical_turn =
                        current_turn_to_canonical_turn(&session_id, incoming_turn)?;
                    let acceptance = Some((
                        SessionAcceptanceRecord {
                            session: state
                                .sessions
                                .iter()
                                .find(|session| session.session_id == session_id)
                                .expect("session existence was validated")
                                .clone(),
                            timeline_entry: TimelineEntry {
                                entry_id: entry_id.clone(),
                                session_id: session_id.clone(),
                                kind: kind.clone(),
                                message: message.clone(),
                                occurred_at,
                            },
                            superseded_turn: None,
                            canonical_turn: incoming_canonical_turn.clone(),
                            sidecar: updated.clone().into(),
                        },
                        Some(task.clone()),
                    ));
                    let plan = Self::canonical_replacement_commit_plan(
                        state,
                        replaced_turn_index,
                        incoming_canonical_turn,
                        occurred_at,
                        acceptance,
                    )?;
                    let accepted_canonical_turn = plan
                        .mutations
                        .last()
                        .map(|mutation| mutation.next.clone())
                        .ok_or(DomainError::InvalidState {
                            message: "replacement canonical transaction 缺少新 Turn".to_string(),
                        })?;
                    Ok(CanonicalCommitPlan {
                        mutations: plan.mutations,
                        value: (updated, plan.value, accepted_canonical_turn),
                        acceptance: plan.acceptance,
                    })
                },
                |state, (updated, superseded_turn, accepted_canonical_turn)| {
                    state.timeline.push(TimelineEntry {
                        entry_id: entry_id.clone(),
                        session_id: session_id.clone(),
                        kind: kind.clone(),
                        message: message.clone(),
                        occurred_at,
                    });
                    if let Some(session) = state
                        .sessions
                        .iter_mut()
                        .find(|session| session.session_id == session_id)
                    {
                        session.updated_at = occurred_at;
                    }
                    upsert_runtime_sidecar_in_state(state, updated);
                    (
                        state
                            .runtime_sidecar(&session_id)
                            .expect("committed sidecar"),
                        superseded_turn,
                        accepted_canonical_turn,
                    )
                },
            )?;
        self.mark_sidecar_dirty_for_session(
            &session_id,
            SessionSidecarFlushReason::AcceptCurrentTurn,
        );
        Ok((entry_id, updated, superseded_turn, accepted_canonical_turn))
    }

    pub fn ensure_current_turn_acceptance_available(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<()> {
        let state = self.state.read().expect("session state read lock poisoned");
        if !state
            .sessions
            .iter()
            .any(|session| &session.session_id == session_id)
        {
            return Err(DomainError::NotFound { entity: "session" });
        }
        let existing_turn = state
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn);
        reject_conflicting_active_current_turn(session_id, existing_turn.as_ref(), None)
    }

    /// 测试夹具装配已有 canonical Turn 的执行元数据；正式运行只经接纳事务创建链。
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_execution_chain_for_test(
        &self,
        session_id: SessionId,
        chain: ActiveExecutionChain,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !state
            .sessions
            .iter()
            .any(|session| session.session_id == session_id)
        {
            return Err(DomainError::NotFound { entity: "session" });
        }
        let existing = state.runtime_sidecar(&session_id);
        if existing
            .as_ref()
            .is_some_and(|sidecar| sidecar.active_execution_chain.is_some())
        {
            return Err(DomainError::InvalidState {
                message: "fixture chain already exists".into(),
            });
        }
        let turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let updated =
            Self::build_active_execution_chain_sidecar(session_id.clone(), chain, existing, turn)?;
        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        drop(state);
        self.mark_sidecar_dirty_for_session(
            &session_id,
            SessionSidecarFlushReason::AcceptCurrentTurn,
        );
        Ok(updated)
    }

    pub fn validate_execution_chain(
        &self,
        session_id: &SessionId,
        expected: &ActiveExecutionChain,
    ) -> DomainResult<()> {
        let state = self.state.read().expect("session state read lock poisoned");
        validate_execution_chain_in_state(&state, session_id, expected).map(|_| ())
    }

    pub fn insert_execution_branch(
        &self,
        session_id: &SessionId,
        expected: &ActiveExecutionChain,
        branch: crate::ActiveExecutionBranch,
    ) -> DomainResult<()> {
        self.commit_canonical_transaction(
            session_id,
            |state| {
                let sidecar = validate_execution_chain_in_state(state, session_id, expected)?;
                let chain = sidecar
                    .active_execution_chain
                    .as_ref()
                    .expect("validated chain");
                if chain.contains_task(&branch.task_id) || branch.is_primary {
                    return Err(DomainError::InvalidState {
                        message: "子代理分支已存在或试图替换主分支".into(),
                    });
                }
                Ok(CanonicalCommitPlan {
                    mutations: vec![],
                    value: branch,
                    acceptance: None,
                })
            },
            |state, branch| {
                let sidecar = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                    .expect("validated sidecar");
                let chain = sidecar
                    .active_execution_chain
                    .as_mut()
                    .expect("validated chain");
                chain.branches.push(branch.clone());
                chain.normalize();
                sidecar.updated_at = UtcMillis::now();
            },
        )?;
        self.mark_sidecar_dirty_for_session(
            session_id,
            SessionSidecarFlushReason::UpdateActiveExecutionBranchSnapshot,
        );
        Ok(())
    }

    pub fn remove_execution_branch_if_current(
        &self,
        session_id: &SessionId,
        expected: &ActiveExecutionChain,
        branch: &crate::ActiveExecutionBranch,
    ) -> DomainResult<()> {
        self.commit_canonical_transaction(
            session_id,
            |state| {
                let sidecar = validate_execution_chain_in_state(state, session_id, expected)?;
                let chain = sidecar
                    .active_execution_chain
                    .as_ref()
                    .expect("validated chain");
                if branch.is_primary || !chain.branches.iter().any(|current| current == branch) {
                    return Err(DomainError::InvalidState {
                        message: "分支已变化，拒绝回滚其他执行的状态".into(),
                    });
                }
                Ok(CanonicalCommitPlan {
                    mutations: vec![],
                    value: (),
                    acceptance: None,
                })
            },
            |state, ()| {
                let sidecar = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                    .expect("validated sidecar");
                sidecar
                    .active_execution_chain
                    .as_mut()
                    .expect("validated chain")
                    .branches
                    .retain(|current| current.task_id != branch.task_id);
                sidecar.updated_at = UtcMillis::now();
            },
        )?;
        self.mark_sidecar_dirty_for_session(
            session_id,
            SessionSidecarFlushReason::UpdateActiveExecutionBranchSnapshot,
        );
        Ok(())
    }

    /// 仅更新原 Turn 的分支 Skill，不从调用方的整条链快照覆盖当前状态。
    pub fn update_execution_branch_active_skill(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        expected_turn_id: &str,
        skill_id: &str,
    ) -> DomainResult<()> {
        if skill_id.trim().is_empty() {
            return Err(DomainError::InvalidState {
                message: "Skill ID 不能为空".into(),
            });
        }
        self.commit_canonical_transaction(
            session_id,
            |state| {
                let previous = state
                    .canonical_turns
                    .iter()
                    .find(|turn| &turn.session_id == session_id && turn.turn_id == expected_turn_id)
                    .filter(|turn| {
                        !turn.status.is_terminal()
                            && turn.completed_at.is_none()
                            && turn
                                .agent_turn_binding()
                                .is_ok_and(|binding| binding.is_none())
                    })
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "分支 Skill 激活的主线 Turn 不存在或已关闭".into(),
                    })?;
                let mut candidate = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| &sidecar.session_id == session_id)
                    .cloned()
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "分支 Skill 激活缺少当前执行链".into(),
                    })?;
                let chain = candidate
                    .active_execution_chain
                    .as_mut()
                    .filter(|chain| {
                        chain.current_turn_id.as_deref() == Some(previous.turn_id.as_str())
                    })
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "分支 Skill 激活的执行链 Turn 已变化".into(),
                    })?;
                let branch = chain
                    .branches
                    .iter_mut()
                    .find(|branch| &branch.task_id == task_id)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "Skill 激活 Task 不在当前执行链".into(),
                    })?;
                branch.skill_name = Some(skill_id.into());
                if &chain.root_task_id == task_id {
                    chain.dispatch_context.skill_name = Some(skill_id.into());
                }
                chain.normalize();
                candidate.updated_at = UtcMillis::now();
                Ok(CanonicalCommitPlan {
                    mutations: vec![],
                    value: candidate,
                    acceptance: None,
                })
            },
            upsert_runtime_sidecar_in_state,
        )?;
        self.mark_sidecar_dirty_for_session(
            session_id,
            SessionSidecarFlushReason::UpdateActiveExecutionBranchSnapshot,
        );
        Ok(())
    }

    /// 归档原 Turn 的执行链；Continue 即使复用同一 root，也不能被旧回调归档。
    pub fn archive_active_execution_chain(
        &self,
        session_id: &SessionId,
        root_task_id: &TaskId,
        expected_turn_id: Option<&str>,
    ) -> DomainResult<()> {
        let _canonical_barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical commit barrier poisoned");
        let session_lock = self.canonical_commit_lock_for(session_id);
        let _session_guard = session_lock
            .lock()
            .expect("canonical session commit lock poisoned");
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let current_turn = state
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn);
        let sidecar = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter_mut()
            .find(|sidecar| &sidecar.session_id == session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let chain =
            sidecar
                .active_execution_chain
                .as_ref()
                .ok_or_else(|| DomainError::InvalidState {
                    message: "当前会话没有活跃执行链".into(),
                })?;
        if &chain.root_task_id != root_task_id
            || chain.current_turn_id.as_deref() != expected_turn_id
            || sidecar.current_turn_id.as_deref() != expected_turn_id
        {
            return Err(DomainError::InvalidState {
                message: "归档目标与当前执行链或 Turn 不一致".into(),
            });
        }
        if current_turn
            .as_ref()
            .is_some_and(|turn| !current_turn_status_is_terminal(&turn.status))
        {
            return Err(DomainError::InvalidState {
                message: "当前 Turn 尚未结束，不能归档执行链".into(),
            });
        }
        sidecar.ownership = ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: chain
                .workspace_id
                .clone()
                .or_else(|| sidecar.ownership.workspace_id.clone()),
            ..ExecutionOwnership::default()
        };
        sidecar.recovery_id = None;
        sidecar.active_execution_chain = None;
        sidecar.status = SessionExecutionSidecarStatus::Detached;
        sidecar.updated_at = UtcMillis::now();
        drop(state);
        self.mark_sidecar_dirty_for_session(
            session_id,
            SessionSidecarFlushReason::ArchiveActiveExecutionChain,
        );
        Ok(())
    }

    /// 捕获脏 session 的最小持久化快照后再执行写盘。
    ///
    /// canonical/state 锁只覆盖快照构造，不覆盖文件 fsync；因此后台中间态
    /// 持久化不会阻塞模型流式写回。持久化锁覆盖整个投影提交，避免旧快照在
    /// 重命名、删除或其他投影事务之后落盘。canonical event 是追加日志，写盘期间若有
    /// 新事件提交，projection builder 会以日志最新事实重新对齐，下一轮继续收敛。
    /// 回调返回实际提交的 session；暂不可写的工作区保留脏版本，不阻断其他会话。
    pub fn flush_execution_sidecar_snapshot_with<E, F>(&self, persist: F) -> Result<bool, E>
    where
        F: FnMut(
            &SessionDurableState,
            &SessionExecutionSidecarStoreState,
            &[SessionId],
        ) -> Result<Vec<SessionId>, E>,
    {
        let _flush_guard = self
            .sidecar_flush_lock
            .lock()
            .expect("session sidecar flush lock poisoned");
        let _persistence_guard = self
            .durable_persistence_lock
            .lock()
            .expect("session durable persistence lock poisoned");
        let (persisted_version, dirty_session_ids, durable, sidecars) = {
            let _canonical_guard = self
                .canonical_commit_barrier
                .write()
                .expect("canonical commit barrier poisoned");
            let state = self.state.read().expect("session state read lock poisoned");
            let flush_state = self
                .sidecar_flush_state
                .read()
                .expect("session sidecar flush state read lock poisoned");
            if flush_state.current_version == flush_state.flushed_version {
                return Ok(false);
            }
            let persisted_version = flush_state.current_version;
            let mut dirty_session_ids = flush_state
                .dirty_session_versions
                .iter()
                .filter_map(|(session_id, version)| {
                    (*version <= persisted_version).then_some(session_id.clone())
                })
                .collect::<Vec<_>>();
            dirty_session_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            let mut durable = SessionDurableState::default();
            for session_id in &dirty_session_ids {
                durable.append_state_without_current(state.durable_state_for_session(session_id));
            }
            let dirty_set = dirty_session_ids.iter().collect::<HashSet<_>>();
            let sidecars = SessionExecutionSidecarStoreState {
                runtime_sidecars: state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .filter(|sidecar| dirty_set.contains(&sidecar.session_id))
                    .cloned()
                    .collect(),
            };
            (persisted_version, dirty_session_ids, durable, sidecars)
        };

        let mut persist = persist;
        let committed = persist(&durable, &sidecars, &dirty_session_ids)?;
        let committed = committed
            .into_iter()
            .filter(|id| dirty_session_ids.contains(id))
            .collect::<HashSet<_>>();
        if !dirty_session_ids.is_empty() && committed.is_empty() {
            return Ok(false);
        }

        let mut flush_state = self
            .sidecar_flush_state
            .write()
            .expect("session sidecar flush state write lock poisoned");
        for session_id in dirty_session_ids
            .into_iter()
            .filter(|id| committed.contains(id))
        {
            if flush_state
                .dirty_session_versions
                .get(&session_id)
                .is_some_and(|version| *version <= persisted_version)
            {
                flush_state.dirty_session_versions.remove(&session_id);
            }
        }
        let committed_through = flush_state
            .dirty_session_versions
            .values()
            .min()
            .map_or(persisted_version, |pending| {
                persisted_version.min(pending.saturating_sub(1))
            });
        flush_state.flushed_version = flush_state.flushed_version.max(committed_through);
        let now = UtcMillis::now();
        flush_state.last_flush_at = Some(now);
        if flush_state.current_version == flush_state.flushed_version {
            flush_state.next_flush_hint = None;
        } else if flush_state.next_flush_hint.is_none() {
            flush_state.next_flush_hint = flush_state.last_dirty_at.or(Some(now));
        }
        Ok(true)
    }
}
