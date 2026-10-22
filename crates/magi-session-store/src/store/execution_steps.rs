use super::{
    CanonicalItemCommitNotice, CanonicalTurnMutation, SessionStore, sidecar::CanonicalCommitPlan,
};
use crate::{
    CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnVisibility,
    execution_steps::{
        EXECUTION_STEPS_KEY, ExecutionSteps, UpdateExecutionStepsParams, prepare_steps,
        read_execution_steps,
    },
};
use magi_core::{DomainError, DomainResult, SessionId, TaskId, UtcMillis};
use std::collections::HashMap;
fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::InvalidState {
        message: message.into(),
    }
}
impl SessionStore {
    pub fn execution_steps_for_task(
        &self,
        session: &SessionId,
        turn_id: &str,
        task: &TaskId,
    ) -> DomainResult<Option<ExecutionSteps>> {
        let state = self.state.read().expect("session state read lock");
        let Some(turn) = state
            .canonical_turns
            .iter()
            .find(|t| &t.session_id == session && t.turn_id == turn_id)
        else {
            return Ok(None);
        };
        turn.items
            .iter()
            .filter(|i| i.worker.as_ref().and_then(|w| w.task_id.as_ref()) == Some(task))
            .find_map(|i| match read_execution_steps(i) {
                Ok(None) => None,
                other => Some(other),
            })
            .unwrap_or(Ok(None))
    }
    /// item 归属只返回小字段，流式进度不复制整轮正文。
    pub fn execution_step_for_item(
        &self,
        session: &SessionId,
        turn_id: &str,
        item_id: &str,
    ) -> Option<String> {
        let state = self.state.read().expect("session state read lock");
        state
            .canonical_turns
            .iter()
            .find(|t| &t.session_id == session && t.turn_id == turn_id)
            .and_then(|t| t.items.iter().find(|i| i.item_id == item_id))
            .and_then(|i| i.metadata.get(crate::execution_steps::EXECUTION_STEP_KEY))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    /// 与所有 canonical 写入共用 session 锁、durable 事务和 itemVersion。
    pub fn update_execution_steps(
        &self,
        session: &SessionId,
        turn_id: &str,
        task: &TaskId,
        input: UpdateExecutionStepsParams,
    ) -> DomainResult<CanonicalItemCommitNotice> {
        let result = self.commit_canonical_transaction(
            session,
            |state| {
                let previous = state
                    .canonical_turns
                    .iter()
                    .find(|t| &t.session_id == session && t.turn_id == turn_id)
                    .ok_or_else(|| invalid("执行步骤来源 Turn 不存在"))?;
                if previous.status.is_terminal() || previous.completed_at.is_some() {
                    return Err(invalid("已结束 Turn 不能更新执行步骤"));
                }
                super::agent_turns::validate_agent_turn_admission(self, state, previous)?;
                if previous.agent_turn_binding()?.is_none()
                    && !state
                        .execution_sidecar_store
                        .runtime_sidecars
                        .iter()
                        .any(|s| {
                            &s.session_id == session
                                && s.current_turn
                                    .as_ref()
                                    .is_some_and(|t| t.turn_id == turn_id)
                        })
                {
                    return Err(invalid("旧主线 Turn 不能更新执行步骤"));
                }
                let source = previous
                    .items
                    .iter()
                    .rev()
                    .find(|i| i.worker.as_ref().and_then(|w| w.task_id.as_ref()) == Some(task))
                    .ok_or_else(|| invalid("当前 Task 不属于这个 Turn"))?;
                let index = previous.items.iter().position(|i| {
                    i.metadata.contains_key(EXECUTION_STEPS_KEY)
                        && i.worker.as_ref().and_then(|w| w.task_id.as_ref()) == Some(task)
                });
                let old = index
                    .map(|i| read_execution_steps(&previous.items[i]))
                    .transpose()?
                    .flatten();
                let snapshot = prepare_steps(previous, task.as_str(), input, old.as_ref())?;
                for step in &snapshot.steps {
                    if let Some(plan_id) = &step.plan_item_id {
                        if !state.plans.iter().any(|p| {
                            &p.session_id == session
                                && p.items.iter().any(|i| i.item_id.as_str() == plan_id)
                        }) {
                            return Err(invalid("关联的计划项不存在"));
                        }
                    }
                }
                let now = UtcMillis(snapshot.updated_at);
                let mut item =
                    index
                        .map(|i| previous.items[i].clone())
                        .unwrap_or_else(|| CanonicalTurnItem {
                            session_id: session.clone(),
                            turn_id: turn_id.into(),
                            turn_seq: previous.turn_seq,
                            item_id: snapshot.execution_id.clone(),
                            item_seq: previous.items.iter().map(|i| i.item_seq).max().unwrap_or(0)
                                + 1,
                            kind: CanonicalTurnItemKind::SystemNotice,
                            created_at: previous.accepted_at,
                            status: CanonicalTurnItemStatus::Running,
                            item_version: Some(0),
                            updated_at: now,
                            title: None,
                            content: None,
                            blocks: vec![],
                            tool: None,
                            worker: source.worker.clone(),
                            source_thread_id: source.source_thread_id.clone(),
                            visibility: CanonicalTurnVisibility::default(),
                            metadata: HashMap::new(),
                        });
                // 卡片 item 始终可更新，嵌套步骤表达各自终态；Turn 关闭时统一结算。
                item.metadata
                    .insert("noticeKind".into(), serde_json::json!("execution_steps"));
                item.metadata.insert(
                    EXECUTION_STEPS_KEY.into(),
                    serde_json::to_value(&snapshot).map_err(|e| invalid(e.to_string()))?,
                );
                item.item_version = Some(item.item_version.unwrap_or(0) + 1);
                item.updated_at = now;
                let mut next = previous.clone();
                if let Some(i) = index {
                    next.items[i] = item.clone();
                } else {
                    next.items.push(item.clone());
                }
                next.validate_update_from(previous)?;
                let notice = CanonicalItemCommitNotice {
                    turn: next.clone(),
                    item,
                    workspace_id: state
                        .sessions
                        .iter()
                        .find(|s| &s.session_id == session)
                        .and_then(|s| s.workspace_id.clone()),
                };
                Ok(CanonicalCommitPlan {
                    mutations: vec![CanonicalTurnMutation {
                        previous: Some(previous.clone()),
                        next,
                    }],
                    value: notice,
                    acceptance: None,
                })
            },
            |state, notice| {
                // canonical 是权威；同步已有运行态投影，避免下一次工具写回删除新增卡片。
                for sidecar in &mut state.execution_sidecar_store.runtime_sidecars {
                    if &sidecar.session_id != session {
                        continue;
                    }
                    if sidecar
                        .current_turn
                        .as_ref()
                        .is_some_and(|t| t.turn_id == notice.turn.turn_id)
                    {
                        sidecar.current_turn =
                            Some(super::sidecar::active_turn_projection(&notice.turn));
                        sidecar.updated_at = notice.item.updated_at;
                    }
                    if let Some(chain) = sidecar.active_execution_chain.as_mut()
                        && chain
                            .current_turn
                            .as_ref()
                            .is_some_and(|t| t.turn_id == notice.turn.turn_id)
                    {
                        chain.current_turn =
                            Some(super::sidecar::active_turn_projection(&notice.turn));
                    }
                }
                notice
            },
        );
        if result.is_ok() {
            self.mark_sidecar_dirty_for_session(
                session,
                crate::SessionSidecarFlushReason::AppendCurrentTurnItem,
            );
        }
        result
    }
}
