use super::*;

impl SessionStore {
    pub fn apply_recovery_resume_input(
        &self,
        session_id: SessionId,
        input: RecoveryResumeInput,
    ) -> DomainResult<()> {
        let _canonical_barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical commit barrier poisoned");
        let session_lock = self.canonical_commit_lock_for(&session_id);
        let _session_guard = session_lock
            .lock()
            .expect("canonical session commit lock poisoned");
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
        let execution_chain_ref = if let Some(existing) = existing.as_ref() {
            if let Some(recovery_id) = existing.recovery_id.as_deref()
                && recovery_id != input.recovery_id.as_str()
            {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "session_runtime_sidecar 的 recovery_id 与 recovery input 不一致: {recovery_id} != {}",
                        input.recovery_id
                    ),
                });
            }
            match (
                existing.ownership.execution_chain_ref.clone(),
                input.ownership.execution_chain_ref.clone(),
            ) {
                (Some(left), Some(right)) if left != right => {
                    return Err(DomainError::InvalidState {
                        message: format!(
                            "session_runtime_sidecar 的 execution_chain_ref 与 recovery input 不一致: {left} != {right}"
                        ),
                    });
                }
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
                (Some(left), Some(_)) => Some(left),
            }
        } else {
            input.ownership.execution_chain_ref.clone()
        };
        let active_execution_chain = existing
            .as_ref()
            .and_then(|sidecar| sidecar.active_execution_chain.clone());
        let current_turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let ownership = if let Some(chain) = active_execution_chain.as_ref() {
            Self::ownership_from_active_execution_chain(chain)
        } else {
            ExecutionOwnership {
                execution_chain_ref,
                ..input.ownership
            }
        };
        upsert_runtime_sidecar_in_state(
            &mut state,
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership,
                recovery_id: Some(input.recovery_id),
                current_turn,
                active_execution_chain,
                status: SessionExecutionSidecarStatus::RecoveryLinked,
                updated_at: UtcMillis::now(),
            },
        );
        drop(state);
        self.mark_sidecar_dirty_for_session(
            &session_id,
            SessionSidecarFlushReason::ApplyRecoveryResumeInput,
        );
        Ok(())
    }

    pub fn apply_resume_execution_target(
        &self,
        session_id: &SessionId,
        target: &TaskExecutionTarget,
    ) -> DomainResult<SessionRuntimeSidecar> {
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
        let existing = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| &sidecar.session_id == session_id)
            .map(|sidecar| state.project_sidecar(sidecar))
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        if existing.recovery_id != target.recovery_id {
            return Err(DomainError::InvalidState {
                message: "session_runtime_sidecar 的 recovery_id 与恢复目标不一致".into(),
            });
        }
        let execution_chain_ref = match (
            existing.ownership.execution_chain_ref.clone(),
            target.execution_chain_ref.clone(),
        ) {
            (Some(left), Some(right)) if left != right => {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "session_runtime_sidecar 的 execution_chain_ref 与恢复目标不一致: {left} != {right}"
                    ),
                });
            }
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
            (Some(left), Some(_)) => Some(left),
        };
        let updated = SessionRuntimeSidecar {
            session_id: session_id.clone(),
            ownership: ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: existing.ownership.workspace_id,
                mission_id: Some(target.mission_id.clone()),
                task_id: Some(target.task_id.clone()),
                worker_id: target.requested_worker_id.clone(),
                execution_chain_ref,
            },
            recovery_id: existing.recovery_id,
            current_turn: existing.current_turn,
            active_execution_chain: existing.active_execution_chain,
            status: SessionExecutionSidecarStatus::Resumed,
            updated_at: UtcMillis::now(),
        };
        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        drop(state);
        self.mark_sidecar_dirty_for_session(
            session_id,
            SessionSidecarFlushReason::ApplyResumeExecutionTarget,
        );
        Ok(updated)
    }

    /// 恢复引用按原 ID 更新，不接收或覆盖调用方持有的 Turn 快照。
    pub fn set_recovery_id(
        &self,
        session_id: &SessionId,
        expected_recovery_id: Option<&str>,
        recovery_id: Option<String>,
    ) -> DomainResult<SessionRuntimeSidecar> {
        self.update_recovery_id(session_id, expected_recovery_id, recovery_id, || Ok(()))
    }

    /// 校验当前引用后消费恢复记录，成功才清理引用。回调只做同步内存提交，
    /// 不执行 IO、不重入 SessionStore；锁序固定为 Session → Workspace。
    pub fn consume_recovery_id(
        &self,
        session_id: &SessionId,
        recovery_id: &str,
        consume: impl FnOnce() -> DomainResult<()>,
    ) -> DomainResult<()> {
        self.update_recovery_id(session_id, Some(recovery_id), None, consume)?;
        Ok(())
    }

    pub(super) fn update_recovery_id(
        &self,
        session_id: &SessionId,
        expected_recovery_id: Option<&str>,
        recovery_id: Option<String>,
        commit: impl FnOnce() -> DomainResult<()>,
    ) -> DomainResult<SessionRuntimeSidecar> {
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
        let sidecar = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter_mut()
            .find(|sidecar| &sidecar.session_id == session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        if sidecar.recovery_id.as_deref() != expected_recovery_id {
            return Err(DomainError::InvalidState {
                message: "恢复记录已变化，拒绝覆盖其他恢复入口".into(),
            });
        }
        commit()?;
        sidecar.status = Self::derive_sidecar_status(
            &sidecar.ownership,
            recovery_id.as_deref(),
            Some(&sidecar.status),
        );
        sidecar.recovery_id = recovery_id;
        sidecar.updated_at = UtcMillis::now();
        let updated = state
            .runtime_sidecar(session_id)
            .expect("validated sidecar");
        drop(state);
        self.mark_sidecar_dirty_for_session(
            session_id,
            SessionSidecarFlushReason::UpdateRecoveryId,
        );
        Ok(updated)
    }

    pub fn has_recovery_ready_interruption(&self, session_id: &SessionId) -> bool {
        self.has_interrupted_recovery_with_state(session_id, RECOVERY_STATE_READY)
    }

    /// 将异常中断执行链关联的 Goal 与 plan 一并恢复，并返回可精确回滚的内存快照。
    ///
    /// 归属必须由 canonical `goalId`、Goal owner 或 plan task binding 证明；同一会话里
    /// 仅仅存在暂停 Goal 不构成关联。预算、用量或 blocker 限制也不会被异常恢复绕过。
    pub fn resume_goal_for_interrupted_execution(
        &self,
        session_id: &SessionId,
        interrupted_turn_id: &str,
        resumed_turn_id: &str,
        now: UtcMillis,
    ) -> DomainResult<Option<InterruptedGoalResumeCheckpoint>> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let sidecar = state
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let current_turn =
            sidecar
                .current_turn
                .as_ref()
                .ok_or_else(|| DomainError::InvalidState {
                    message: format!("session {session_id} 缺少异常中断轮次"),
                })?;
        if current_turn.turn_id != interrupted_turn_id || current_turn.status != "interrupted" {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session {session_id} 的异常中断轮次已变化: {}",
                    current_turn.turn_id
                ),
            });
        }
        let recovery_claimed = current_turn.items.iter().any(|item| {
            item.metadata.get("noticeKind").and_then(Value::as_str)
                == Some(INTERRUPTION_NOTICE_KIND)
                && item
                    .metadata
                    .get(RECOVERY_STATE_METADATA_KEY)
                    .and_then(Value::as_str)
                    == Some(RECOVERY_STATE_CLAIMED)
        });
        if !recovery_claimed {
            return Err(DomainError::InvalidState {
                message: format!("session {session_id} 尚未领取异常中断恢复权"),
            });
        }
        let chain = sidecar
            .active_execution_chain
            .as_ref()
            .ok_or_else(|| DomainError::InvalidState {
                message: format!("session {session_id} 没有可恢复的执行链"),
            })?
            .clone();
        let interrupted_turn = state
            .canonical_turns
            .iter()
            .find(|turn| turn.session_id == *session_id && turn.turn_id == interrupted_turn_id)
            .cloned()
            .ok_or_else(|| DomainError::InvalidState {
                message: format!("异常中断 canonical turn 不存在: {interrupted_turn_id}"),
            })?;
        let Some(goal_index) = state.goals.iter().position(|goal| {
            &goal.session_id == session_id
                && goal.status.is_unfinished()
                && goal_owns_execution_chain(&state, goal, &interrupted_turn, &chain)
        }) else {
            return Ok(None);
        };
        let goal_before = state.goals[goal_index].clone();
        if !matches!(goal_before.status, GoalStatus::Active | GoalStatus::Paused) {
            return Err(DomainError::InvalidState {
                message: format!("关联目标当前状态不允许异常恢复: {:?}", goal_before.status),
            });
        }
        let plan_index = state.plans.iter().position(|plan| {
            &plan.session_id == session_id && plan.goal_id.as_ref() == Some(&goal_before.goal_id)
        });
        let plan_before = plan_index.map(|index| state.plans[index].clone());
        if let Some(plan_index) = plan_index
            && state.plans[plan_index].state == magi_core::PlanState::Paused
        {
            super::super::goals::activate_paused_plan(&mut state.plans[plan_index], now);
        }
        {
            let goal = &mut state.goals[goal_index];
            if goal.status == GoalStatus::Paused {
                goal.status = GoalStatus::Active;
                goal.control_revision = goal.control_revision.saturating_add(1);
            }
            goal.continuation = GoalContinuationState {
                phase: GoalContinuationPhase::Running,
                turn_id: Some(resumed_turn_id.to_string()),
                reason: None,
            };
            goal.updated_at = now;
        }
        let applied_goal_revision = state.goals[goal_index].control_revision;
        let applied_plan_revision = plan_index.map(|index| state.plans[index].revision);
        reconcile_goal_time_used(&mut state);
        Ok(Some(InterruptedGoalResumeCheckpoint {
            goal_before,
            plan_before,
            applied_goal_revision,
            applied_plan_revision,
            resumed_turn_id: resumed_turn_id.to_string(),
        }))
    }

    pub fn rollback_interrupted_goal_resume(
        &self,
        checkpoint: InterruptedGoalResumeCheckpoint,
    ) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| goal.goal_id == checkpoint.goal_before.goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        let current_goal = &state.goals[goal_index];
        if current_goal.control_revision != checkpoint.applied_goal_revision
            || current_goal.continuation.phase != GoalContinuationPhase::Running
            || current_goal.continuation.turn_id.as_deref()
                != Some(checkpoint.resumed_turn_id.as_str())
        {
            return Err(DomainError::InvalidState {
                message: "异常恢复后的 Goal 已被并发修改，拒绝覆盖回滚".to_string(),
            });
        }
        match checkpoint.plan_before {
            Some(plan_before) => {
                let plan_index = state
                    .plans
                    .iter()
                    .position(|plan| plan.plan_id == plan_before.plan_id)
                    .ok_or(DomainError::NotFound { entity: "plan" })?;
                if Some(state.plans[plan_index].revision) != checkpoint.applied_plan_revision {
                    return Err(DomainError::InvalidState {
                        message: "异常恢复后的 plan 已被并发修改，拒绝覆盖回滚".to_string(),
                    });
                }
                state.plans[plan_index] = plan_before;
            }
            None => {
                if state.plans.iter().any(|plan| {
                    plan.session_id == checkpoint.goal_before.session_id
                        && plan.goal_id.as_ref() == Some(&checkpoint.goal_before.goal_id)
                }) {
                    return Err(DomainError::InvalidState {
                        message: "异常恢复后出现新的绑定 plan，拒绝覆盖回滚".to_string(),
                    });
                }
            }
        }
        state.goals[goal_index] = checkpoint.goal_before;
        reconcile_goal_time_used(&mut state);
        Ok(())
    }

    pub fn has_claimed_interrupted_recovery(&self, session_id: &SessionId) -> bool {
        self.has_interrupted_recovery_with_state(session_id, RECOVERY_STATE_CLAIMED)
    }

    pub(super) fn has_interrupted_recovery_with_state(
        &self,
        session_id: &SessionId,
        state: &str,
    ) -> bool {
        self.runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .filter(|turn| turn.status == "interrupted")
            .is_some_and(|turn| {
                turn.items.iter().any(|item| {
                    item.metadata.get("noticeKind").and_then(Value::as_str)
                        == Some(INTERRUPTION_NOTICE_KIND)
                        && item
                            .metadata
                            .get(RECOVERY_STATE_METADATA_KEY)
                            .and_then(Value::as_str)
                            == Some(state)
                })
            })
    }

    /// 原子领取一次异常中断恢复权。返回被领取的旧 turn id；非异常中断恢复返回 `None`。
    pub fn claim_interrupted_recovery(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<Option<String>> {
        self.set_interrupted_recovery_state(
            session_id,
            None,
            RECOVERY_STATE_READY,
            RECOVERY_STATE_CLAIMED,
        )
    }

    /// 恢复启动失败时释放领取权，使用户可再次发起恢复。
    pub fn release_interrupted_recovery_claim(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> DomainResult<Option<String>> {
        self.set_interrupted_recovery_state(
            session_id,
            Some(turn_id),
            RECOVERY_STATE_CLAIMED,
            RECOVERY_STATE_READY,
        )
    }

    pub(super) fn set_interrupted_recovery_state(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        expected_state: &str,
        next_state: &str,
    ) -> DomainResult<Option<String>> {
        let updated_turn_id = self.commit_canonical_transaction(
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
                if turn.status != "interrupted"
                    || expected_turn_id.is_some_and(|expected| expected != turn.turn_id)
                {
                    return Ok(CanonicalCommitPlan {
                        mutations: Vec::new(),
                        value: None,
                        acceptance: None,
                    });
                }
                if next_state == RECOVERY_STATE_CLAIMED
                    && (!super::super::user_questions::pending_user_questions_in_state(
                        self, state, session_id,
                    )?
                    .is_empty()
                        || !super::super::tool_approvals::pending_in_state(self, state, session_id)?
                            .is_empty())
                {
                    return Err(DomainError::InvalidState {
                        message: "请先处理待回答问题和工具审批，再继续执行".into(),
                    });
                }
                let Some(notice) = turn.items.iter_mut().find(|item| {
                    item.metadata.get("noticeKind").and_then(Value::as_str)
                        == Some(INTERRUPTION_NOTICE_KIND)
                }) else {
                    return Err(DomainError::InvalidState {
                        message: format!("session {session_id} 的异常中断轮次缺少恢复提示"),
                    });
                };
                let current_state = notice
                    .metadata
                    .get(RECOVERY_STATE_METADATA_KEY)
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if current_state != expected_state {
                    return Err(DomainError::InvalidState {
                        message: format!("异常中断恢复状态已变化: {current_state}"),
                    });
                }
                notice.metadata.insert(
                    RECOVERY_STATE_METADATA_KEY.to_string(),
                    Value::String(next_state.to_string()),
                );
                let turn_id = turn.turn_id.clone();
                turn.normalize();
                sidecar.updated_at = UtcMillis::now();
                let canonical_plan = Self::canonical_turn_commit_plan(
                    state,
                    session_id,
                    sidecar.current_turn.as_ref().expect("current turn was set"),
                    None,
                )?;
                Ok(CanonicalCommitPlan {
                    mutations: canonical_plan.mutations,
                    value: Some((sidecar_index, turn_id, candidate)),
                    acceptance: None,
                })
            },
            |state, updated| {
                let (sidecar_index, turn_id, candidate) = updated?;
                state.execution_sidecar_store.runtime_sidecars[sidecar_index] = candidate.into();
                Some(turn_id)
            },
        )?;
        if updated_turn_id.is_some() {
            self.mark_sidecar_dirty_for_session(
                session_id,
                SessionSidecarFlushReason::UpdateCurrentTurnStatus,
            );
        }
        Ok(updated_turn_id)
    }
}
