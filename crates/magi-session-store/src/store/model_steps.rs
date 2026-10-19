//! Step 与 Input 消费共用 Session 事务；不建立额外日志或可重放的副作用队列。
use super::inputs::apply_input;
use super::{CanonicalTurnMutation, SessionInputMutation, SessionStore};
use crate::SessionInputState;
use magi_core::{DomainError, DomainResult, SessionId};

/// 一棵 Agent 执行树（主线、子代理、显式继续和恢复）共享的 transport 保险。
/// 这是运行保护，不是用户可配置的任务预算。
pub const MAX_TRANSPORT_ATTEMPTS_PER_EXECUTION_TREE: usize = 10_000;

impl SessionStore {
    pub fn model_step_goal(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Option<crate::SessionGoal> {
        let state = self.state.read().expect("session state lock poisoned");
        let turn = state
            .canonical_turns
            .iter()
            .find(|turn| &turn.session_id == session_id && turn.turn_id == turn_id)?;
        super::agent_turns::execution_goal_in_state(self, &state, turn)
            .ok()
            .flatten()
            .cloned()
    }
    pub fn install_model_step_commit_observer(
        &self,
        observer: std::sync::Arc<dyn Fn(super::ModelStepCommitNotice) + Send + Sync>,
    ) {
        *self
            .model_step_commit_observer
            .write()
            .expect("model step observer lock poisoned") = Some(observer);
    }

    /// 冻结请求与本次明确交付的输入集合一起提交。迟到的 steer 不会被自动领取。
    pub fn begin_model_step(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        mut step: crate::ModelStep,
    ) -> DomainResult<()> {
        step.validate_snapshot()?;
        if step.purpose == Some(crate::ModelStepPurpose::ContextCompaction)
            && !step.input_request_ids.is_empty()
        {
            return Err(DomainError::InvalidState {
                message: "上下文摘要请求不能消费用户输入".into(),
            });
        }
        if step.status != crate::ModelStepStatus::Started || step.attempts.len() != 1 {
            return Err(DomainError::InvalidState {
                message: "新模型 Step 必须从唯一的 Started attempt 开始".into(),
            });
        }
        let request_ids = step.input_request_ids.clone();
        let consumption = crate::InputConsumption {
            step_id: step.step_id.clone(),
            request_fingerprint: step.request_fingerprint.clone(),
            provider: step.provider.clone(),
            message_count: step.message_count,
            tool_count: step.tool_count,
        };
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        let (turn_mutation, mutations) = {
            let state = self.state.read().expect("session state lock poisoned");
            let previous = state
                .canonical_turns
                .iter()
                .find(|turn| &turn.session_id == session_id && turn.turn_id == turn_id)
                .ok_or(DomainError::NotFound {
                    entity: "canonical_turn",
                })?;
            if previous.status.is_terminal()
                || previous.completed_at.is_some()
                || previous
                    .model_steps
                    .iter()
                    .any(|existing| existing.step_id == step.step_id)
            {
                return Err(DomainError::InvalidState {
                    message: "模型 Step 目标已关闭或调用身份已使用，不能重放".into(),
                });
            }
            super::agent_turns::validate_agent_turn_admission(self, &state, previous)?;
            validate_transport_safety_limit(&state, session_id, previous)?;
            if step.goal_id.is_some()
                || step.usage_tokens.is_some()
                || step.goal_tokens_after.is_some()
            {
                return Err(DomainError::InvalidState {
                    message: "新 Step 的 Goal 与用量只能由 SessionStore 绑定和结算".into(),
                });
            }
            if let Some(goal) = super::agent_turns::execution_goal_in_state(self, &state, previous)?
            {
                validate_goal_model_admission(goal)?;
                step.goal_id = Some(goal.goal_id.clone());
            }
            let mut next_turn = previous.clone();
            next_turn.model_steps.push(step.clone());
            next_turn.validate_update_from(previous)?;
            let turn_mutation = CanonicalTurnMutation {
                previous: Some(previous.clone()),
                next: next_turn,
            };
            let mut mutations = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for id in &request_ids {
                if !seen.insert(id) {
                    continue;
                }
                let Some(input) = state
                    .inputs
                    .iter()
                    .find(|input| &input.session_id == session_id && &input.request_id == id)
                else {
                    continue;
                };
                if input.attached_turn_id.as_deref() != Some(turn_id) {
                    return Err(DomainError::InvalidState {
                        message: "模型请求不能引用其他 Turn 的输入".into(),
                    });
                }
                if input.state == SessionInputState::Consumed {
                    continue;
                }
                if input.state != SessionInputState::Attached {
                    return Err(DomainError::InvalidState {
                        message: "模型请求不能领取未绑定或其他 Turn 的输入".into(),
                    });
                }
                let mut next = input.clone();
                next.revision =
                    next.revision
                        .checked_add(1)
                        .ok_or_else(|| DomainError::InvalidState {
                            message: "输入 revision 溢出".into(),
                        })?;
                next.state = SessionInputState::Consumed;
                next.consumption = Some(consumption.clone());
                next.validate_update_from(Some(input))?;
                mutations.push(SessionInputMutation {
                    previous: Some(input.clone()),
                    next,
                });
            }
            (turn_mutation, mutations)
        };
        self.persist_session_transaction(
            session_id,
            std::slice::from_ref(&turn_mutation),
            &mutations,
            None,
        )?;
        let mut state = self.state.write().expect("session state lock poisoned");
        super::sidecar::apply_canonical_turn_in_state(&mut state, turn_mutation.next);
        let inputs_changed = !mutations.is_empty();
        for mutation in mutations {
            apply_input(&mut state.inputs, mutation.next);
        }
        drop(state);
        drop(_guard);
        drop(_barrier);
        if inputs_changed {
            self.notify_input_commit(session_id);
        }
        self.notify_model_step_commit(session_id, turn_id, &step.step_id);
        Ok(())
    }

    /// 结果必须在执行任何模型返回的工具之前结算；失败时调用方停止，不放行副作用。
    pub fn settle_model_step(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        step_id: &str,
        response_fingerprint: Option<String>,
        usage_tokens: Option<u64>,
        executor_failure: Option<&magi_core::ExecutorFailureEvidence>,
    ) -> DomainResult<()> {
        if executor_failure.is_some() && response_fingerprint.is_some()
            || response_fingerprint.is_none()
                && usage_tokens.is_some()
                && executor_failure.is_none()
        {
            return Err(DomainError::InvalidState {
                message: "失败用量必须绑定外部执行证据，不能与成功响应混用".into(),
            });
        }
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        let (mutation, projection) = {
            let state = self.state.read().expect("session state lock poisoned");
            let previous = state
                .canonical_turns
                .iter()
                .find(|turn| &turn.session_id == session_id && turn.turn_id == turn_id)
                .ok_or(DomainError::NotFound {
                    entity: "canonical_turn",
                })?;
            let mut next = previous.clone();
            let step = next
                .model_steps
                .iter_mut()
                .find(|step| step.step_id == step_id)
                .ok_or(DomainError::NotFound {
                    entity: "model_step",
                })?;
            step.status = if response_fingerprint.is_some() {
                crate::ModelStepStatus::ResponseReceived
            } else {
                crate::ModelStepStatus::OutcomeUnknown
            };
            step.response_fingerprint = response_fingerprint;
            step.usage_tokens = usage_tokens;
            step.goal_tokens_after =
                step.goal_id
                    .as_ref()
                    .zip(usage_tokens)
                    .and_then(|(id, used)| {
                        state
                            .goals
                            .iter()
                            .find(|goal| &goal.session_id == session_id && &goal.goal_id == id)
                            .map(|goal| goal.tokens_used.saturating_add(used))
                    });
            // 旧 Goal 已被新 Goal 替换时保留该 Step 的用量，不能记到当前 Goal。
            if step.goal_id.is_some()
                && step.usage_tokens.is_some()
                && step.goal_tokens_after.is_none()
            {
                let previous_total = state
                    .canonical_turns
                    .iter()
                    .filter(|turn| &turn.session_id == session_id)
                    .flat_map(|turn| &turn.model_steps)
                    .filter(|old| old.goal_id == step.goal_id)
                    .filter_map(|old| old.goal_tokens_after)
                    .max()
                    .unwrap_or(0);
                step.goal_tokens_after = step
                    .usage_tokens
                    .map(|used| previous_total.saturating_add(used));
            }
            let last = step.attempts.last_mut().expect("validated model attempts");
            step.settled_at = Some(magi_core::UtcMillis(
                magi_core::UtcMillis::now().0.max(last.started_at.0),
            ));
            last.status = step.status;
            last.settled_at = step.settled_at;
            if let Some(evidence) = executor_failure {
                evidence
                    .validate()
                    .map_err(|message| DomainError::Validation { message })?;
                if step.executor_capabilities.as_ref().is_none_or(|caps| {
                    caps.tools != magi_app_server_protocol::ExecutorToolMode::ExecutorOwned
                }) {
                    return Err(DomainError::InvalidState {
                        message: "原生 Step 不能写入外部执行器失败证据".into(),
                    });
                }
                let now = step.settled_at.expect("settled Step");
                let task_id = step.task_id.clone();
                let thread = step.thread_id.clone();
                let item_id = format!("executor-failure-{step_id}");
                let item_seq = next
                    .items
                    .iter()
                    .map(|item| item.item_seq)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "执行器证据 itemSeq 溢出".into(),
                    })?;
                if next.items.iter().any(|item| item.item_id == item_id) {
                    return Err(DomainError::InvalidState {
                        message: "执行器失败证据已经提交".into(),
                    });
                }
                next.items.push(crate::CanonicalTurnItem {
                    session_id: session_id.clone(), turn_id: turn_id.into(), turn_seq: next.turn_seq,
                    item_id, item_seq, kind: crate::CanonicalTurnItemKind::SystemNotice,
                    created_at: next.accepted_at, status: crate::CanonicalTurnItemStatus::Failed,
                    item_version: Some(1), updated_at: now,
                    title: Some("外部执行器部分执行记录".into()),
                    content: Some("执行器未完整交付；已保留实际执行记录，请先核对产物及未知结果，不要重复执行。".into()),
                    blocks: vec![], tool: None,
                    worker: Some(crate::CanonicalWorkerRef { task_id, worker_id: None, role_id: None, title: None }),
                    source_thread_id: thread,
                    visibility: crate::CanonicalTurnVisibility { renderable: false },
                    metadata: std::collections::HashMap::from([
                        ("modelStepId".into(), serde_json::json!(step_id)),
                        ("executorFailure".into(), serde_json::to_value(evidence).expect("executor evidence serializes")),
                        ("renderable".into(), serde_json::json!(false)),
                    ]),
                });
            }
            next.validate_update_from(previous)?;
            let mutation = CanonicalTurnMutation {
                previous: Some(previous.clone()),
                next,
            };
            // 只有新增 item 才需要刷新 sidecar；独立 Agent Turn 不改变主线当前指针。
            // 与交互写回相同，持久提交前先校验投影，且只读取当前 Turn，避免复制会话历史。
            let mut projection = if executor_failure.is_some() {
                state.execution_sidecar_store.runtime_sidecar(session_id)
            } else {
                None
            };
            projection = projection.filter(|sidecar| {
                sidecar
                    .current_turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_id == turn_id)
            });
            if let Some(sidecar) = projection.as_mut() {
                super::sidecar::rebuild_sidecar_projection_from_canonical(
                    sidecar,
                    std::slice::from_ref(&mutation.next),
                )?;
            }
            (mutation, projection)
        };
        self.persist_session_transaction(session_id, std::slice::from_ref(&mutation), &[], None)?;
        let mut state = self.state.write().expect("session state lock poisoned");
        super::sidecar::apply_canonical_turn_in_state(&mut state, mutation.next);
        // 外部执行器失败证据是不可见的 canonical SystemNotice，但仍属于该 Turn
        // 的持久事实。同步当前 sidecar 投影，否则进程重启时会因 item 集合不一致
        // 拒绝恢复，等待/取消也无法读取同一份原始证据。
        if let Some(projection) = projection
            && let Some(sidecar) = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter_mut()
                .find(|sidecar| sidecar.session_id == *session_id)
        {
            sidecar.current_turn = projection.current_turn;
            if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                chain.current_turn = sidecar.current_turn.clone();
                chain.normalize();
            }
        }
        drop(state);
        drop(_guard);
        drop(_barrier);
        self.notify_model_step_commit(session_id, turn_id, step_id);
        Ok(())
    }

    /// transport 重试仍属于同一冻结请求，但每次尝试有独立身份，且必须先提交再发送。
    pub fn start_model_step_retry(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        step_id: &str,
    ) -> DomainResult<()> {
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        let mutation = {
            let state = self.state.read().expect("session state lock poisoned");
            let previous = state
                .canonical_turns
                .iter()
                .find(|turn| &turn.session_id == session_id && turn.turn_id == turn_id)
                .ok_or(DomainError::NotFound {
                    entity: "canonical_turn",
                })?;
            if previous.status.is_terminal() || previous.completed_at.is_some() {
                return Err(DomainError::InvalidState {
                    message: "拒绝已关闭 Turn 的 transport 重试".into(),
                });
            }
            super::agent_turns::validate_agent_turn_admission(self, &state, previous)?;
            validate_transport_safety_limit(&state, session_id, previous)?;
            let mut next = previous.clone();
            let step = next
                .model_steps
                .iter_mut()
                .find(|step| step.step_id == step_id)
                .ok_or(DomainError::NotFound {
                    entity: "model_step",
                })?;
            if step.status != crate::ModelStepStatus::Started {
                return Err(DomainError::InvalidState {
                    message: "已结算 Step 不能重试".into(),
                });
            }
            if let Some(goal) = step.goal_id.as_ref().and_then(|id| {
                state
                    .goals
                    .iter()
                    .find(|goal| &goal.session_id == session_id && &goal.goal_id == id)
            }) {
                validate_goal_model_admission(goal)?;
            }
            let last = step.attempts.last_mut().expect("validated model attempts");
            let now = magi_core::UtcMillis(magi_core::UtcMillis::now().0.max(last.started_at.0));
            last.status = crate::ModelStepStatus::OutcomeUnknown;
            last.settled_at = Some(now);
            step.attempts.push(crate::ModelAttempt {
                attempt_id: format!("{}:attempt:{}", step_id, step.attempts.len() + 1),
                started_at: now,
                settled_at: None,
                status: crate::ModelStepStatus::Started,
            });
            next.validate_update_from(previous)?;
            CanonicalTurnMutation {
                previous: Some(previous.clone()),
                next,
            }
        };
        self.persist_session_transaction(session_id, std::slice::from_ref(&mutation), &[], None)?;
        super::sidecar::apply_canonical_turn_in_state(
            &mut self.state.write().expect("session state lock poisoned"),
            mutation.next,
        );
        drop(_guard);
        drop(_barrier);
        self.notify_model_step_commit(session_id, turn_id, step_id);
        Ok(())
    }

    fn notify_model_step_commit(&self, session_id: &SessionId, turn_id: &str, step_id: &str) {
        let observer = self
            .model_step_commit_observer
            .read()
            .expect("model step observer lock poisoned")
            .clone();
        let Some(observer) = observer else { return };
        // 只复制已提交的单个 Step；通知不持有提交锁，也不重复传输整个 Turn 正文。
        let notice = {
            let state = self.state.read().expect("session state lock poisoned");
            let Some(turn) = state
                .canonical_turns
                .iter()
                .find(|turn| &turn.session_id == session_id && turn.turn_id == turn_id)
            else {
                return;
            };
            let Some(step) = turn.model_steps.iter().find(|step| step.step_id == step_id) else {
                return;
            };
            super::ModelStepCommitNotice {
                session_id: session_id.clone(),
                workspace_id: state
                    .sessions
                    .iter()
                    .find(|session| &session.session_id == session_id)
                    .and_then(|session| session.workspace_id.clone()),
                // 恢复的独立 Agent 可能尚未在客户端出现；只携带 Turn 基线，
                // 不复制正文或全部 Step，原 item 仍由 canonical item 事件交付。
                turn: crate::CanonicalTurn {
                    items: Vec::new(),
                    model_steps: Vec::new(),
                    session_id: turn.session_id.clone(),
                    turn_id: turn.turn_id.clone(),
                    turn_seq: turn.turn_seq,
                    accepted_at: turn.accepted_at,
                    completed_at: turn.completed_at,
                    status: turn.status,
                    response_duration_ms: turn.response_duration_ms,
                    usage: turn.usage.clone(),
                    metadata: turn.metadata.clone(),
                },
                step: step.clone(),
            }
        };
        observer(notice);
    }
}

fn validate_transport_safety_limit(
    state: &crate::models::SessionStoreState,
    session_id: &SessionId,
    turn: &crate::CanonicalTurn,
) -> DomainResult<()> {
    // 运行保护属于一次执行树，而不是某个物理 Turn。子代理拥有独立 Turn，
    // 显式 continue 也会创建新 Turn；若只统计当前 Turn，拆分或恢复就会
    // 重新获得上限，等价于绕过运行保护。沿 Agent 的不可变 parent_turn_id
    // 解析执行树根，再把同一根下的所有 Step/transport attempt 累计。
    let group_root = execution_group_root_id(state, session_id, &turn.turn_id)?;
    let mut group_turns = Vec::new();
    for candidate in state
        .canonical_turns
        .iter()
        .filter(|candidate| &candidate.session_id == session_id)
    {
        if execution_group_root_id(state, session_id, &candidate.turn_id)? == group_root {
            group_turns.push(candidate);
        }
    }
    let attempts = group_turns
        .into_iter()
        .flat_map(|candidate| &candidate.model_steps)
        .map(|step| step.attempts.len())
        .fold(0_usize, usize::saturating_add);
    if attempts >= MAX_TRANSPORT_ATTEMPTS_PER_EXECUTION_TREE {
        return Err(DomainError::RunSafetyLimit {
            message: format!(
                "当前执行树已达到 {MAX_TRANSPORT_ATTEMPTS_PER_EXECUTION_TREE} 次 transport 尝试安全上限"
            ),
        });
    }
    Ok(())
}

fn execution_group_root_id(
    state: &crate::models::SessionStoreState,
    session_id: &SessionId,
    turn_id: &str,
) -> DomainResult<String> {
    let mut current = turn_id.to_string();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return Err(DomainError::InvalidState {
                message: "Agent Turn 来源链循环，无法计算执行树安全上限".into(),
            });
        }
        let turn = state
            .canonical_turns
            .iter()
            .find(|candidate| &candidate.session_id == session_id && candidate.turn_id == current)
            .ok_or(DomainError::NotFound {
                entity: "canonical_turn",
            })?;
        let Some(binding) = turn.agent_turn_binding()? else {
            return Ok(current);
        };
        current = binding.parent_turn_id;
    }
}

fn validate_goal_model_admission(goal: &crate::SessionGoal) -> DomainResult<()> {
    if matches!(goal.status, crate::GoalStatus::Paused) {
        return Err(DomainError::GoalRejected {
            reason: magi_core::GoalRejection::NotActive,
            message: "run_paused: Goal 已暂停，只有显式恢复后才能发送新的模型请求".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ModelAttempt, ModelStep, ModelStepStatus,
        agent_turn::AGENT_EXECUTION_METADATA_KEY,
        models::{CanonicalTurn, CanonicalTurnStatus, SessionStoreState},
    };
    use magi_core::{MissionId, TaskId, ThreadId, UtcMillis, WorkerId};

    fn step(id: &str, task_id: Option<TaskId>, thread_id: &str, attempts: usize) -> ModelStep {
        let attempts = (1..=attempts)
            .map(|index| ModelAttempt {
                attempt_id: format!("{id}:attempt:{index}"),
                started_at: UtcMillis(index as u64),
                settled_at: (index < attempts).then_some(UtcMillis(index as u64)),
                status: if index < attempts {
                    ModelStepStatus::OutcomeUnknown
                } else {
                    ModelStepStatus::Started
                },
            })
            .collect::<Vec<_>>();
        ModelStep {
            step_id: id.into(),
            attempts,
            task_id,
            goal_id: None,
            usage_tokens: None,
            goal_tokens_after: None,
            thread_id: ThreadId::new(thread_id),
            checkpoint_generation: 0,
            request_fingerprint: "a".repeat(64),
            provider: "test".into(),
            binding: None,
            executor_capabilities: None,
            purpose: Some(crate::ModelStepPurpose::Execution),
            message_count: 1,
            tool_count: 0,
            max_output_tokens: None,
            input_request_ids: vec![],
            started_at: UtcMillis(1),
            settled_at: None,
            status: ModelStepStatus::Started,
            response_fingerprint: None,
        }
    }

    #[test]
    fn safety_limit_is_shared_by_parent_and_agent_turns() {
        let session_id = SessionId::new("transport-group");
        let child_task_id = TaskId::new("child-task");
        let mission_id = MissionId::new("mission");
        let parent = CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: "parent-turn".into(),
            turn_seq: 1,
            accepted_at: UtcMillis(1),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: vec![],
            model_steps: vec![step(
                "parent-step",
                None,
                "parent-thread",
                MAX_TRANSPORT_ATTEMPTS_PER_EXECUTION_TREE - 1,
            )],
            metadata: Default::default(),
        };
        let binding = crate::AgentTurnBinding {
            task_id: child_task_id.clone(),
            root_task_id: TaskId::new("root-task"),
            parent_task_id: TaskId::new("root-task"),
            parent_turn_id: "parent-turn".into(),
            thread_id: ThreadId::new("child-thread"),
            mission_id,
            worker_id: WorkerId::new("worker"),
            role_id: "explorer".into(),
            previous_turn_id: None,
            continuation_operation_id: None,
        };
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            AGENT_EXECUTION_METADATA_KEY.into(),
            serde_json::to_value(binding).unwrap(),
        );
        let child = CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: "turn-agent-child-task".into(),
            turn_seq: 2,
            accepted_at: UtcMillis(2),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: vec![],
            model_steps: vec![step("child-step", Some(child_task_id), "child-thread", 1)],
            metadata,
        };
        let state = SessionStoreState {
            canonical_turns: vec![parent, child],
            ..Default::default()
        };
        let current = state.canonical_turns[1].clone();
        assert!(matches!(
            validate_transport_safety_limit(&state, &session_id, &current),
            Err(DomainError::RunSafetyLimit { .. })
        ));

        // An unrelated top-level Turn starts a new execution group and keeps its own safety headroom.
        let unrelated = CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: "unrelated-turn".into(),
            turn_seq: 3,
            accepted_at: UtcMillis(3),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: vec![],
            model_steps: vec![],
            metadata: Default::default(),
        };
        let mut state = state;
        state.canonical_turns.push(unrelated);
        let unrelated = state.canonical_turns[2].clone();
        assert!(validate_transport_safety_limit(&state, &session_id, &unrelated).is_ok());
    }
}
