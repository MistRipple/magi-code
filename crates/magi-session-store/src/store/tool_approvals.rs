use super::user_questions::advance_item_version;
use super::{CanonicalTurnMutation, SessionStore};
use crate::tool_approval::{
    ApprovalExecutionClaim, ClaimedToolApproval, DurableToolApproval, PendingToolApproval,
    TOOL_APPROVAL_METADATA_KEY, ToolApprovalDecision,
};
use crate::{CanonicalTurn, CanonicalTurnItem, CanonicalTurnStatus, SessionStoreState};
use magi_core::{DomainError, DomainResult, SessionId, TaskId, UtcMillis};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::InvalidState {
        message: message.into(),
    }
}
fn arguments(tool: &crate::CanonicalToolCall) -> String {
    match &tool.arguments {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => "{}".into(),
    }
}
fn live_turn(store: &SessionStore, state: &SessionStoreState, turn: &CanonicalTurn) -> bool {
    super::agent_turns::interaction_turn_is_current_or_recoverable(store, state, turn)
        .unwrap_or(false)
}
fn resolvable(
    store: &SessionStore,
    state: &SessionStoreState,
    turn: &CanonicalTurn,
    item: &CanonicalTurnItem,
) -> bool {
    if !live_turn(store, state, turn) {
        return false;
    }
    if !turn.status.is_terminal() && turn.completed_at.is_none() {
        return !item.status.is_terminal();
    }
    turn.status == CanonicalTurnStatus::Interrupted
        && item
            .tool
            .as_ref()
            .and_then(|tool| tool.result.as_ref())
            .and_then(|result| result.get("status"))
            .and_then(Value::as_str)
            == Some("awaiting_approval")
        && (turn.agent_turn_binding().ok().flatten().is_some()
            || turn.items.iter().any(|item| {
                item.metadata.get("noticeKind").and_then(Value::as_str)
                    == Some("session_interrupted")
                    && item.metadata.get("recoveryState").and_then(Value::as_str) == Some("ready")
            }))
}

impl CanonicalTurnItem {
    pub fn tool_approval(&self) -> DomainResult<Option<DurableToolApproval>> {
        let approval: Option<DurableToolApproval> = self
            .metadata
            .get(TOOL_APPROVAL_METADATA_KEY)
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| invalid(format!("审批持久状态无效：{error}")))?;
        if let Some(approval) = &approval {
            let request = &approval.request;
            if approval.execution.as_ref().is_some_and(|execution| {
                execution.task_id.as_str().trim().is_empty()
                    || execution.turn_id.trim().is_empty()
                    || execution.item_id.trim().is_empty()
                    || execution.tool_call_id.trim().is_empty()
                    || approval
                        .decided_at
                        .is_none_or(|at| execution.started_at.0 < at.0)
            }) || approval
                .decided_at
                .is_some_and(|at| at.0 < request.requested_at.0)
                || request.approval_id.trim().is_empty()
                || self.tool.as_ref().is_some_and(|tool| {
                    magi_tool_runtime::normalized_tool_arguments(&arguments(tool))
                        != approval.normalized_arguments
                })
                || request.session_id != self.session_id
                || request.turn_id != self.turn_id
                || self
                    .worker
                    .as_ref()
                    .and_then(|worker| worker.task_id.as_ref())
                    != Some(&request.task_id)
                || self.tool.as_ref().is_none_or(|tool| {
                    tool.call_id != request.tool_call_id || tool.name != request.tool_name
                })
                || approval
                    .execution_root
                    .as_ref()
                    .is_some_and(|path| !path.is_absolute())
                || approval.decided_at.is_some_and(|at| {
                    at.0 >= request
                        .requested_at
                        .0
                        .saturating_add(crate::tool_approval::TOOL_APPROVAL_TTL_MILLIS)
                })
                || approval.decision.is_some() != approval.decided_at.is_some()
                || approval.cancelled_at.is_some() && approval.decision.is_some()
                || approval.execution.is_some()
                    && !matches!(
                        approval.decision,
                        Some(ToolApprovalDecision::AllowOnce | ToolApprovalDecision::AllowForTurn)
                    )
            {
                return Err(invalid("审批与来源工具或决定不匹配"));
            }
        }
        Ok(approval)
    }
}

impl SessionStore {
    pub fn register_tool_approval(
        &self,
        request: PendingToolApproval,
        execution_root: Option<PathBuf>,
        policy: Value,
        decision: Option<ToolApprovalDecision>,
    ) -> DomainResult<()> {
        let session = request.session_id.clone();
        self.commit_tool_interaction(&session, |state| {
            let previous = state
                .canonical_turns
                .iter()
                .find(|turn| turn.session_id == session && turn.turn_id == request.turn_id)
                .ok_or_else(|| invalid("审批来源 Turn 不存在"))?;
            if !live_turn(self, state, previous)
                || previous.status.is_terminal()
                || previous.completed_at.is_some()
            {
                return Err(invalid("审批来源 Turn 已关闭"));
            }
            let mut next = previous.clone();
            let item = next
                .items
                .iter_mut()
                .rev()
                .find(|item| {
                    item.worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        == Some(&request.task_id)
                        && item.tool.as_ref().is_some_and(|tool| {
                            tool.call_id == request.tool_call_id && tool.name == request.tool_name
                        })
                })
                .ok_or_else(|| invalid("审批缺少 canonical 工具 item"))?;
            if item.status.is_terminal() || item.tool_approval()?.is_some() {
                return Err(invalid("审批工具调用已关闭或已登记"));
            }
            let normalized_arguments = magi_tool_runtime::normalized_tool_arguments(&arguments(
                item.tool.as_ref().expect("approval tool"),
            ));
            let approval = DurableToolApproval {
                request: request.clone(),
                normalized_arguments,
                execution_root,
                policy_snapshot: policy,
                decision,
                decided_at: decision.map(|_| UtcMillis::now()),
                cancelled_at: None,
                execution: None,
            };
            item.metadata.insert(
                TOOL_APPROVAL_METADATA_KEY.into(),
                serde_json::to_value(approval).expect("approval serializable"),
            );
            item.tool.as_mut().expect("approval tool").result = Some(serde_json::json!({
                "tool":request.tool_name, "status":"awaiting_approval",
                "approval_id":request.approval_id, "approval":request,
            }));
            item.tool_approval()?;
            advance_item_version(item)?;
            Ok((
                vec![CanonicalTurnMutation {
                    previous: Some(previous.clone()),
                    next,
                }],
                (),
            ))
        })
    }

    pub fn pending_tool_approvals(
        &self,
        session: &SessionId,
    ) -> DomainResult<Vec<PendingToolApproval>> {
        let state = self.state.read().expect("session state lock");
        pending_in_state(self, &state, session)
    }

    /// 同一参数的本轮批准仍逐 item 结算；整个批次持久成功后运行时才能唤醒。
    pub fn resolve_tool_approvals(
        &self,
        session: &SessionId,
        ids: &[String],
        decision: ToolApprovalDecision,
    ) -> DomainResult<()> {
        self.commit_tool_interaction(session, |state| {
            let now = UtcMillis::now();
            let mut mutations = Vec::new();
            let mut found = std::collections::HashSet::new();
            for previous in state
                .canonical_turns
                .iter()
                .filter(|turn| &turn.session_id == session)
            {
                let mut next = previous.clone();
                for (index, item) in previous.items.iter().enumerate() {
                    let Some(mut approval) = item.tool_approval()? else {
                        continue;
                    };
                    if !ids.contains(&approval.request.approval_id) {
                        continue;
                    }
                    if !approval.is_pending(now) || !resolvable(self, state, previous, item) {
                        return Err(invalid("工具授权请求已过期、取消或已经处理"));
                    }
                    found.insert(approval.request.approval_id.clone());
                    approval.decision = Some(decision);
                    approval.decided_at = Some(now);
                    let item = &mut next.items[index];
                    if let Some(result) = approval.denied_result() {
                        item.tool.as_mut().expect("approval tool").result = Some(result);
                    }
                    item.metadata.insert(
                        TOOL_APPROVAL_METADATA_KEY.into(),
                        serde_json::to_value(approval).expect("approval serializable"),
                    );
                    advance_item_version(item)?;
                }
                if next != *previous {
                    mutations.push(CanonicalTurnMutation {
                        previous: Some(previous.clone()),
                        next,
                    });
                }
            }
            if found.len() != ids.len() {
                return Err(invalid("工具授权请求不存在或来源已改变"));
            }
            Ok((mutations, ()))
        })
    }

    pub fn cancel_tool_approval(&self, session: &SessionId, id: &str) -> DomainResult<()> {
        self.commit_tool_interaction(session, |state| {
            for previous in state
                .canonical_turns
                .iter()
                .filter(|turn| &turn.session_id == session)
            {
                for (index, item) in previous.items.iter().enumerate() {
                    let Some(mut approval) = item.tool_approval()? else {
                        continue;
                    };
                    if approval.request.approval_id != id {
                        continue;
                    }
                    let mut next = previous.clone();
                    if approval.decision.is_none() && approval.cancelled_at.is_none() {
                        approval.cancelled_at = Some(UtcMillis::now());
                        let item = &mut next.items[index];
                        item.metadata.insert(
                            TOOL_APPROVAL_METADATA_KEY.into(),
                            serde_json::to_value(approval).expect("approval serializable"),
                        );
                        advance_item_version(item)?;
                    }
                    return Ok((
                        vec![CanonicalTurnMutation {
                            previous: Some(previous.clone()),
                            next,
                        }],
                        (),
                    ));
                }
            }
            Err(invalid("审批不存在"))
        })
    }

    /// 仅领取同一任务恢复链、参数、目录和策略的未开始批准。领取与目标 running 原子提交。
    #[allow(clippy::too_many_arguments)]
    pub fn claim_tool_approval(
        &self,
        session: &SessionId,
        task: &TaskId,
        call_id: &str,
        tool_name: &str,
        tool_arguments: &str,
        execution_root: Option<&Path>,
        policy: &Value,
        sources: &[TaskId],
        approval_id: Option<&str>,
    ) -> DomainResult<Option<ClaimedToolApproval>> {
        self.commit_tool_interaction(session, |state| {
            let target_turn = state.canonical_turns.iter().rev().find(|turn| {
                &turn.session_id == session && live_turn(self, state, turn)
                    && !turn.status.is_terminal() && turn.completed_at.is_none()
                    && turn.items.iter().any(|item| {
                        item.worker.as_ref().and_then(|worker| worker.task_id.as_ref()) == Some(task)
                            && item.tool.as_ref().is_some_and(|tool| tool.call_id == call_id && tool.name == tool_name)
                    })
            }).ok_or_else(|| invalid("审批领取目标 Turn 不可执行"))?;
            let target_index = target_turn.items.iter().rposition(|item| {
                item.worker.as_ref().and_then(|worker| worker.task_id.as_ref()) == Some(task)
                    && item.tool.as_ref().is_some_and(|tool| tool.call_id == call_id && tool.name == tool_name)
            }).ok_or_else(|| invalid("审批领取缺少目标工具 item"))?;
            let target_item = &target_turn.items[target_index];
            let target_arguments = arguments(target_item.tool.as_ref().expect("target tool"));
            let normalized = magi_tool_runtime::normalized_tool_arguments(tool_arguments);
            if magi_tool_runtime::normalized_tool_arguments(&target_arguments) != normalized {
                return Err(invalid("审批领取参数与 canonical 调用不一致"));
            }
            if target_item.status.is_terminal() { return Err(invalid("审批领取目标工具已结束")); }
            let now = UtcMillis::now();
            for source_turn in state.canonical_turns.iter().rev().filter(|turn| &turn.session_id == session) {
                if source_turn.turn_id != target_turn.turn_id && source_turn.status != CanonicalTurnStatus::Interrupted { continue; }
                for (index, item) in source_turn.items.iter().enumerate().rev() {
                    let Some(mut approval) = item.tool_approval()? else { continue; };
                    if approval_id.is_none() && source_turn.turn_id == target_turn.turn_id { continue; }
                    if approval_id.is_some_and(|id| id != approval.request.approval_id)
                        || approval.request.tool_name != tool_name || !sources.contains(&approval.request.task_id)
                        || approval.execution_root.as_deref() != execution_root || &approval.policy_snapshot != policy
                        || approval.execution.is_some() || approval.cancelled_at.is_some()
                        || approval.normalized_arguments != normalized { continue; }
                    let Some(decision) = approval.decision else { continue; };
                    if decision == ToolApprovalDecision::Deny { return Ok((vec![], Some(ClaimedToolApproval {approval_id:approval.request.approval_id,decision}))); }
                    if approval.is_expired(now) { continue; }
                    approval.execution = Some(ApprovalExecutionClaim {
                        task_id: task.clone(), turn_id: target_turn.turn_id.clone(),
                        item_id: target_item.item_id.clone(), tool_call_id: call_id.into(), started_at: now,
                    });
                    let running = serde_json::json!({"tool":tool_name,"status":"running","approval_id":approval.request.approval_id});
                    let mut target = target_turn.clone();
                    let next_target_item = &mut target.items[target_index];
                    next_target_item.tool.as_mut().expect("target tool").result = Some(running.clone());
                    next_target_item.metadata.insert("approvalSource".into(), serde_json::json!({
                        "turnId":source_turn.turn_id,"itemId":item.item_id,"approvalId":approval.request.approval_id,
                    }));
                    advance_item_version(next_target_item)?;
                    let mut source = if source_turn.turn_id == target_turn.turn_id { target.clone() } else { source_turn.clone() };
                    let source_item = &mut source.items[index];
                    source_item.tool.as_mut().expect("approval tool").result = Some(running);
                    let claimed=ClaimedToolApproval{approval_id:approval.request.approval_id.clone(),decision};
                    source_item.metadata.insert(TOOL_APPROVAL_METADATA_KEY.into(), serde_json::to_value(approval).expect("approval serializable"));
                    advance_item_version(source_item)?;
                    let mut mutations = vec![CanonicalTurnMutation { previous:Some(source_turn.clone()), next:source }];
                    if source_turn.turn_id != target_turn.turn_id { mutations.push(CanonicalTurnMutation {previous:Some(target_turn.clone()), next:target}); }
                    return Ok((mutations, Some(claimed)));
                }
            }
            Ok((vec![], None))
        })
    }
}

pub(super) fn pending_in_state(
    store: &SessionStore,
    state: &SessionStoreState,
    session: &SessionId,
) -> DomainResult<Vec<PendingToolApproval>> {
    let mut pending = Vec::new();
    let now = UtcMillis::now();
    for turn in state
        .canonical_turns
        .iter()
        .filter(|turn| &turn.session_id == session)
    {
        for item in &turn.items {
            if let Some(approval) = item.tool_approval()?
                && approval.is_pending(now)
                && resolvable(store, state, turn, item)
            {
                pending.push(approval.request);
            }
        }
    }
    pending.sort_by(|a, b| {
        a.requested_at
            .cmp(&b.requested_at)
            .then(a.approval_id.cmp(&b.approval_id))
    });
    Ok(pending)
}

pub(super) fn validate_execution_links(
    state: &SessionStoreState,
    session: &SessionId,
) -> DomainResult<()> {
    for source in state
        .canonical_turns
        .iter()
        .filter(|turn| &turn.session_id == session)
    {
        for item in &source.items {
            let Some(approval) = item.tool_approval()? else {
                continue;
            };
            let Some(execution) = approval.execution else {
                continue;
            };
            let target = state
                .canonical_turns
                .iter()
                .find(|turn| {
                    turn.session_id == source.session_id && turn.turn_id == execution.turn_id
                })
                .and_then(|turn| {
                    turn.items
                        .iter()
                        .find(|item| item.item_id == execution.item_id)
                })
                .ok_or_else(|| invalid("审批执行领取缺少目标工具事实"))?;
            if target
                .worker
                .as_ref()
                .and_then(|worker| worker.task_id.as_ref())
                != Some(&execution.task_id)
                || target.tool.as_ref().is_none_or(|tool| {
                    tool.call_id != execution.tool_call_id
                        || tool.name != approval.request.tool_name
                        || magi_tool_runtime::normalized_tool_arguments(&arguments(tool))
                            != approval.normalized_arguments
                })
                || target.metadata.get("approvalSource")
                    != Some(&serde_json::json!({
                        "turnId":source.turn_id,"itemId":item.item_id,"approvalId":approval.request.approval_id,
                    }))
            {
                return Err(invalid("审批领取与目标调用身份、参数或来源不匹配"));
            }
        }
    }
    Ok(())
}

impl SessionStore {
    pub fn validate_approval_execution_links(&self, session: &SessionId) -> DomainResult<()> {
        validate_execution_links(&self.state.read().expect("session state lock"), session)
    }
}
