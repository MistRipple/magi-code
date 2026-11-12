use super::*;

impl SessionStore {
    pub fn upsert_current_turn_item_for_turn(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        item: ActiveExecutionTurnItem,
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
                validate_expected_current_turn(session_id, turn, expected_turn_id)?;

                upsert_item_in_turn(turn, item)?;

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
                    value: Some((sidecar_index, candidate)),
                    acceptance: None,
                })
            },
            |state, updated| {
                let (sidecar_index, candidate) = updated?;
                state.execution_sidecar_store.runtime_sidecars[sidecar_index] =
                    candidate.clone().into();
                Some(state.project_sidecar(
                    &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                ))
            },
        )?;
        if updated.is_some() {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::AppendCurrentTurnItem,
            );
        }
        Ok(updated)
    }

    /// 更新当前 Turn 状态并返回本次是否真的产生了 canonical mutation。
    ///
    /// `changed` 是终态事件去重的边界：相同终态的重试仍返回当前 sidecar，
    /// 但调用方只能在 `changed == true` 时发布事实事件。
    pub fn update_current_turn_status_for_turn_with_change(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        status: impl Into<String>,
    ) -> DomainResult<Option<(SessionRuntimeSidecar, bool)>> {
        self.update_current_turn_status_for_turn_with_change_at(
            session_id,
            expected_turn_id,
            status,
            None,
        )
    }

    pub(super) fn update_current_turn_status_for_turn_with_change_at(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        status: impl Into<String>,
        completion_at: Option<UtcMillis>,
    ) -> DomainResult<Option<(SessionRuntimeSidecar, bool)>> {
        let next_status = normalize_stored_current_turn_status(status.into());
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
                validate_expected_current_turn_owner(session_id, turn, expected_turn_id)?;
                if expected_turn_id.is_some() && current_turn_status_is_terminal(&turn.status) {
                    if turn.status != next_status {
                        return Err(DomainError::CurrentTurnConflict {
                            session_id: session_id.to_string(),
                            active_turn_id: turn.turn_id.clone(),
                        });
                    }
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: Some((sidecar_index, candidate, false)),
                        acceptance: None,
                    });
                }
                turn.status = next_status.clone();
                if let Some(item_status) = terminal_item_status_for_turn_status(&turn.status) {
                    settle_active_current_turn_items(&mut turn.items, item_status);
                }
                if turn.completed_at.is_none() && current_turn_status_is_terminal(&turn.status) {
                    turn.completed_at = Some(completion_at.unwrap_or_else(UtcMillis::now));
                }
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
                    if let Some(turn) = candidate.current_turn.as_ref()
                        && turn.status == "completed"
                        && let Some(completed_at) = turn.completed_at
                    {
                        record_session_completion(state, session_id, completed_at);
                    }
                }
                Some((
                    state.project_sidecar(
                        &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                    ),
                    changed,
                ))
            },
        )?;
        if updated.as_ref().is_some_and(|(_, changed)| *changed) {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::UpdateCurrentTurnStatus,
            );
        }
        Ok(updated)
    }

    /// 为 SessionStore 持久化单元测试提供确定性的终态时间写入。
    ///
    /// 生产代码必须通过 `CanonicalTurnEventSink` 写回终态；该入口只在测试编译中存在，
    /// 用于验证 Goal 计时等依赖 `completed_at` 墙钟值的恢复语义，不提供原始 Turn 替换。
    #[cfg(test)]
    pub(crate) fn settle_current_turn_at_for_test(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        status: impl Into<String>,
        completed_at: UtcMillis,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        self.update_current_turn_status_for_turn_with_change_at(
            session_id,
            expected_turn_id,
            status,
            Some(completed_at),
        )
        .map(|updated| updated.map(|(sidecar, _)| sidecar))
    }

    /// 保留 SessionStore 的既有调用契约；需要区分重复提交时使用
    /// `update_current_turn_status_for_turn_with_change`。
    /// 为 SessionStore 持久化单元测试提供不带 changed 标记的状态更新辅助。
    ///
    /// 生产代码必须通过 `CanonicalTurnEventSink` 写回 Turn 状态；该便利包装只在
    /// 测试编译中保留，避免把底层 current Turn 写入口暴露给生产模块。
    #[cfg(test)]
    pub(crate) fn set_current_turn_status_for_test(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        status: impl Into<String>,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        self.update_current_turn_status_for_turn_with_change(session_id, expected_turn_id, status)
            .map(|updated| updated.map(|(sidecar, _)| sidecar))
    }

    pub fn complete_current_turn_from_completed_root_task_for_turn(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        self.complete_current_turn_from_completed_root_task_for_turn_with_change(
            session_id,
            expected_turn_id,
        )
        .map(|updated| updated.map(|(sidecar, _)| sidecar))
    }

    pub fn complete_current_turn_from_completed_root_task_for_turn_with_change(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
    ) -> DomainResult<Option<(SessionRuntimeSidecar, bool)>> {
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
                validate_expected_current_turn(session_id, turn, expected_turn_id)?;
                turn.status = "completed".to_string();
                if turn.completed_at.is_none() {
                    turn.completed_at = Some(UtcMillis::now());
                }
                for item in &mut turn.items {
                    let normalized = item.status.trim().to_ascii_lowercase();
                    if current_turn_item_status_is_active(&item.status) || normalized == "blocked" {
                        item.status = "completed".to_string();
                    }
                    if let Some(tool_status) = item.tool_status.as_deref() {
                        let normalized_tool_status = tool_status.trim().to_ascii_lowercase();
                        if current_turn_item_status_is_active(tool_status)
                            || normalized_tool_status == "blocked"
                        {
                            item.tool_status = Some("completed".to_string());
                        }
                    }
                    normalize_terminal_current_turn_item_metadata(item, "completed");
                }
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
                let changed = !canonical_plan.mutations.is_empty();
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: Some((sidecar_index, candidate, changed)),
                    acceptance: None,
                })
            },
            |state, updated| {
                let (sidecar_index, candidate, changed) = updated?;
                state.execution_sidecar_store.runtime_sidecars[sidecar_index] =
                    candidate.clone().into();
                if let Some(turn) = candidate.current_turn.as_ref()
                    && let Some(completed_at) = turn.completed_at
                {
                    record_session_completion(state, session_id, completed_at);
                }
                Some((
                    state.project_sidecar(
                        &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                    ),
                    changed,
                ))
            },
        )?;
        if updated.is_some() {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::UpdateCurrentTurnStatus,
            );
        }
        Ok(updated)
    }

    pub fn cancel_current_turn(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        self.cancel_current_turn_with_source(session_id, false)
    }

    pub fn interrupt_current_turn_by_user(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        self.cancel_current_turn_with_source(session_id, true)
    }

    /// 将 daemon 异常退出时遗留的活动轮次收敛为可恢复的明确终态。
    ///
    /// 该操作在同一把 session state 写锁内完成轮次终止、活动 item 收敛、恢复提示写入和
    /// canonical turn 同步，避免重启后出现“任务已停止但 UI 仍显示响应中”的分裂状态。
    pub fn interrupt_current_turn_by_daemon_restart(
        &self,
        session_id: &SessionId,
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
                if candidate.active_execution_chain.is_none() {
                    return Err(DomainError::InvalidState {
                        message: format!("session {session_id} 没有可恢复的执行链"),
                    });
                }
                let Some(turn) = candidate.current_turn.as_mut() else {
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: None,
                        acceptance: None,
                    });
                };
                if current_turn_status_is_terminal(&turn.status) {
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: None,
                        acceptance: None,
                    });
                }

                let now = UtcMillis::now();
                let interrupted_at =
                    UtcMillis(candidate.updated_at.0.max(turn.accepted_at.0).min(now.0));
                settle_active_current_turn_items(&mut turn.items, "cancelled");

                let notice_item_id = format!("turn-item-interruption-{}", turn.turn_id);
                let source_thread_id = turn
                    .items
                    .iter()
                    .find(|item| item.kind == "user_message")
                    .or_else(|| turn.items.first())
                    .map(|item| item.source_thread_id.clone())
                    .unwrap_or_else(|| ThreadId::new(format!("thread-orchestrator-{session_id}")));
                let next_item_seq = turn
                    .items
                    .iter()
                    .map(|item| item.item_seq)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                turn.items.push(ActiveExecutionTurnItem {
                    item_id: notice_item_id,
                    item_seq: next_item_seq,
                    kind: "assistant_phase".to_string(),
                    status: "completed".to_string(),
                    source: ORCHESTRATOR_ROLE_ID.to_string(),
                    title: None,
                    content: Some(INTERRUPTION_NOTICE_TEXT.to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: None,
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: HashMap::from([
                        (
                            "noticeKind".to_string(),
                            Value::String(INTERRUPTION_NOTICE_KIND.to_string()),
                        ),
                        ("noticeType".to_string(), Value::String("error".to_string())),
                        (
                            RECOVERY_STATE_METADATA_KEY.to_string(),
                            Value::String(RECOVERY_STATE_READY.to_string()),
                        ),
                        (
                            TURN_INTERRUPTION_SOURCE_METADATA_KEY.to_string(),
                            Value::String(TURN_INTERRUPTION_SOURCE_DAEMON_RESTART.to_string()),
                        ),
                        (
                            TURN_INTERRUPTED_AT_METADATA_KEY.to_string(),
                            Value::from(interrupted_at.0),
                        ),
                    ]),
                    timeline_entry_id: None,
                    source_thread_id,
                });
                turn.status = "interrupted".to_string();
                turn.completed_at = Some(interrupted_at);
                let current_turn_id = turn.turn_id.clone();
                turn.normalize();
                candidate.updated_at = now;
                let continuation_turn_id = candidate
                    .active_execution_chain
                    .as_ref()
                    .map(|chain| chain.root_task_id.to_string())
                    .unwrap_or(current_turn_id);
                let mut canonical_plan = Self::canonical_turn_commit_plan(
                    state,
                    session_id,
                    candidate
                        .current_turn
                        .as_ref()
                        .expect("current turn was set"),
                    None,
                )?;
                canonical_plan
                    .mutations
                    .extend(super::super::agent_turns::interrupted_descendant_turns(
                        self,
                        state,
                        session_id,
                        &candidate
                            .current_turn
                            .as_ref()
                            .expect("current turn")
                            .turn_id,
                    )?);
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: Some((
                        sidecar_index,
                        continuation_turn_id,
                        interrupted_at,
                        candidate,
                    )),
                    acceptance: None,
                })
            },
            |state, updated| {
                let (sidecar_index, continuation_turn_id, interrupted_at, updated) = updated?;
                release_running_goal_continuation(
                    state,
                    session_id,
                    &continuation_turn_id,
                    GOAL_CONTINUATION_DAEMON_RESTART_INTERRUPTED,
                    interrupted_at,
                );
                state.execution_sidecar_store.runtime_sidecars[sidecar_index] = updated.into();
                Some(state.project_sidecar(
                    &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                ))
            },
        )?;
        if updated.is_some() {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::UpdateCurrentTurnStatus,
            );
        }
        Ok(updated)
    }

    pub(super) fn cancel_current_turn_with_source(
        &self,
        session_id: &SessionId,
        interrupted_by_user: bool,
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
                let sidecar = &mut candidate;
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: None,
                        acceptance: None,
                    });
                };
                if !current_turn_status_is_terminal(&turn.status) {
                    let now = UtcMillis::now();
                    if interrupted_by_user
                        && let Some(user_item) = turn
                            .items
                            .iter_mut()
                            .find(|item| item.kind == "user_message")
                    {
                        user_item.metadata.insert(
                            TURN_INTERRUPTION_SOURCE_METADATA_KEY.to_string(),
                            Value::String(TURN_INTERRUPTION_SOURCE_USER.to_string()),
                        );
                        user_item.metadata.insert(
                            TURN_INTERRUPTED_AT_METADATA_KEY.to_string(),
                            Value::from(now.0),
                        );
                    }
                    settle_active_current_turn_items(&mut turn.items, "cancelled");
                    turn.status = "cancelled".to_string();
                    if turn.completed_at.is_none() {
                        turn.completed_at = Some(now);
                    }
                }
                let current_turn_id = turn.turn_id.clone();
                turn.normalize();
                sidecar.updated_at = UtcMillis::now();
                let continuation_turn_id = sidecar
                    .active_execution_chain
                    .as_ref()
                    .map(|chain| chain.root_task_id.to_string())
                    .unwrap_or(current_turn_id);
                let canonical_plan = Self::canonical_turn_commit_plan(
                    state,
                    session_id,
                    sidecar.current_turn.as_ref().expect("current turn was set"),
                    None,
                )?;
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: Some((
                        sidecar_index,
                        continuation_turn_id,
                        sidecar.updated_at,
                        candidate,
                    )),
                    acceptance: None,
                })
            },
            |state, updated| {
                let (sidecar_index, continuation_turn_id, updated_at, updated) = updated?;
                if interrupted_by_user {
                    let goal_index = state.goals.iter().position(|goal| {
                        &goal.session_id == session_id
                            && goal.status == GoalStatus::Active
                            && goal_owns_execution_owner(state, goal, &continuation_turn_id)
                    });
                    if let Some(goal_index) = goal_index {
                        super::super::goals::pause_goal_and_bound_plan_in_state(
                            state, goal_index, updated_at,
                        );
                    }
                } else {
                    release_running_goal_continuation(
                        state,
                        session_id,
                        &continuation_turn_id,
                        GOAL_CONTINUATION_TURN_TERMINAL,
                        updated_at,
                    );
                }
                reconcile_goal_time_used(state);
                state.execution_sidecar_store.runtime_sidecars[sidecar_index] = updated.into();
                Some(state.project_sidecar(
                    &state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                ))
            },
        )?;
        if updated.is_some() {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::UpdateCurrentTurnStatus,
            );
        }
        Ok(updated)
    }
}
