use super::{ORCHESTRATOR_ROLE_ID, SessionStore, TimelineEntryInput};
use crate::models::{
    ActiveExecutionBranchSnapshotUpdate, ActiveExecutionChain, ActiveExecutionTurn,
    ActiveExecutionTurnItem, CanonicalToolCall, CanonicalTurn, CanonicalTurnItem,
    CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus, CanonicalTurnVisibility,
    CanonicalWorkerRef, ExecutionThread, ExecutionThreadStatus, GoalContinuationPhase,
    GoalContinuationState, GoalStatus, InterruptedGoalResumeCheckpoint,
    SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState, SessionPlan,
    SessionRuntimeSidecar, SessionSidecarFlushReason, SessionStoreState, ThreadChatMessage,
    ThreadContextCheckpoint, ThreadVisibility, TimelineEntry,
};
use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, GoalId, MissionId, PlanState,
    RecoveryResumeInput, SessionId, TaskExecutionTarget, TaskId, ThreadId, UtcMillis, WorkerId,
};
use magi_tool_runtime::BuiltinToolName;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const TURN_INTERRUPTION_SOURCE_METADATA_KEY: &str = "interruptionSource";
const TURN_INTERRUPTION_SOURCE_USER: &str = "user";
const TURN_INTERRUPTION_SOURCE_DAEMON_RESTART: &str = "daemon_restart";
const TURN_INTERRUPTED_AT_METADATA_KEY: &str = "interruptedAt";
const INTERRUPTION_NOTICE_KIND: &str = "session_interrupted";
const INTERRUPTION_NOTICE_TEXT: &str = "当前对话发生异常中断，是否继续？";
const RECOVERY_STATE_METADATA_KEY: &str = "recoveryState";
const RECOVERY_STATE_READY: &str = "ready";
const RECOVERY_STATE_CLAIMED: &str = "claimed";
const TURN_REPLACES_TURN_ID_METADATA_KEY: &str = "replacesTurnId";
const TURN_SUPERSEDED_AT_METADATA_KEY: &str = "supersededAt";
const TURN_SUPERSEDED_REASON_METADATA_KEY: &str = "supersededReason";
const TURN_SUPERSEDED_BY_TURN_ID_METADATA_KEY: &str = "supersededByTurnId";
const TURN_GOAL_ID_METADATA_KEY: &str = "goalId";
const TURN_RESPONSE_DURATION_SCOPE_METADATA_KEY: &str = "responseDurationScope";
const TURN_RESPONSE_DURATION_SCOPE_GOAL_PROGRESS: &str = "goal_progress";
const GOAL_CONTINUATION_USER_INTERRUPTED: &str = "user_interrupted";
const GOAL_CONTINUATION_DAEMON_RESTART_INTERRUPTED: &str = "daemon_restart_interrupted";
const GOAL_CONTINUATION_TURN_TERMINAL: &str = "goal_continuation_turn_terminal";

fn inherit_current_turn_aliases(turn: &ActiveExecutionTurn, item: &mut ActiveExecutionTurnItem) {
    let Some(alias_source) = turn.items.iter().find(|existing| {
        existing.request_id.is_some()
            || existing.user_message_id.is_some()
            || existing.placeholder_message_id.is_some()
    }) else {
        return;
    };
    if item.request_id.is_none() {
        item.request_id = alias_source.request_id.clone();
    }
    if item.user_message_id.is_none() {
        item.user_message_id = alias_source.user_message_id.clone();
    }
    if item.placeholder_message_id.is_none() {
        item.placeholder_message_id = alias_source.placeholder_message_id.clone();
    }
}

fn current_turn_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "interrupted"
            | "blocked"
            | "cancelled"
            | "canceled"
            | "killed"
            | "superseded"
    )
}

fn current_turn_item_status_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pending"
            | "queued"
            | "running"
            | "started"
            | "streaming"
            | "blocked"
            | "awaiting_approval"
            | "review_required"
            | "repairing"
            | "verifying"
    )
}

fn terminal_item_status_for_turn_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "succeeded" | "success" => Some("completed"),
        "blocked" => Some("blocked"),
        "failed" | "error" => Some("failed"),
        "interrupted" => Some("cancelled"),
        "cancelled" | "canceled" | "killed" => Some("cancelled"),
        _ => None,
    }
}

fn canonical_current_turn_status(status: &str) -> DomainResult<CanonicalTurnStatus> {
    match status.trim().to_ascii_lowercase().as_str() {
        "pending" | "queued" | "accepted" => Ok(CanonicalTurnStatus::Pending),
        "running" | "started" | "streaming" | "awaiting_approval" | "review_required"
        | "repairing" | "verifying" => Ok(CanonicalTurnStatus::Running),
        "completed" | "complete" | "succeeded" | "success" => Ok(CanonicalTurnStatus::Completed),
        "blocked" => Ok(CanonicalTurnStatus::Blocked),
        "failed" | "error" => Ok(CanonicalTurnStatus::Failed),
        "interrupted" => Ok(CanonicalTurnStatus::Interrupted),
        "cancelled" | "canceled" | "killed" => Ok(CanonicalTurnStatus::Cancelled),
        "superseded" => Ok(CanonicalTurnStatus::Superseded),
        _ => Err(DomainError::InvalidState {
            message: format!("unknown current turn status: {status}"),
        }),
    }
}

fn normalize_stored_current_turn_status(status: String) -> String {
    if status.trim().eq_ignore_ascii_case("killed") {
        return "cancelled".to_string();
    }
    status
}

fn canonical_current_turn_item_status(status: &str) -> DomainResult<CanonicalTurnItemStatus> {
    Ok(match canonical_current_turn_status(status)? {
        CanonicalTurnStatus::Pending => CanonicalTurnItemStatus::Pending,
        CanonicalTurnStatus::Running => CanonicalTurnItemStatus::Running,
        CanonicalTurnStatus::Completed => CanonicalTurnItemStatus::Completed,
        CanonicalTurnStatus::Blocked => CanonicalTurnItemStatus::Blocked,
        CanonicalTurnStatus::Failed => CanonicalTurnItemStatus::Failed,
        CanonicalTurnStatus::Interrupted => CanonicalTurnItemStatus::Cancelled,
        CanonicalTurnStatus::Cancelled => CanonicalTurnItemStatus::Cancelled,
        CanonicalTurnStatus::Superseded => CanonicalTurnItemStatus::Cancelled,
    })
}

fn terminal_item_status_for_canonical_turn_status(
    status: CanonicalTurnStatus,
) -> Option<CanonicalTurnItemStatus> {
    match status {
        CanonicalTurnStatus::Completed => Some(CanonicalTurnItemStatus::Completed),
        CanonicalTurnStatus::Blocked => Some(CanonicalTurnItemStatus::Blocked),
        CanonicalTurnStatus::Failed => Some(CanonicalTurnItemStatus::Failed),
        CanonicalTurnStatus::Interrupted => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Cancelled => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Superseded => Some(CanonicalTurnItemStatus::Cancelled),
        CanonicalTurnStatus::Pending | CanonicalTurnStatus::Running => None,
    }
}

fn canonical_current_turn_item_kind(kind: &str) -> DomainResult<CanonicalTurnItemKind> {
    match kind {
        "user_message" => Ok(CanonicalTurnItemKind::UserMessage),
        "assistant_stream" | "assistant_final" | "assistant_error" => {
            Ok(CanonicalTurnItemKind::AssistantText)
        }
        "assistant_thinking" => Ok(CanonicalTurnItemKind::AssistantThinking),
        "assistant_phase" => Ok(CanonicalTurnItemKind::SystemNotice),
        "tool_call_started" | "tool_call_result" => Ok(CanonicalTurnItemKind::ToolCall),
        "task_status" => Ok(CanonicalTurnItemKind::TaskStatus),
        _ => Err(DomainError::InvalidState {
            message: format!("unknown current turn item kind: {kind}"),
        }),
    }
}

fn canonical_tool_value(value: &Option<String>) -> Option<Value> {
    let value = value.as_ref()?.trim();
    if value.is_empty() {
        return None;
    }
    serde_json::from_str(value)
        .ok()
        .or_else(|| Some(Value::String(value.to_string())))
}

fn current_turn_item_to_canonical_tool(
    item: &ActiveExecutionTurnItem,
) -> DomainResult<Option<CanonicalToolCall>> {
    if item.tool_call_id.is_none() && item.tool_name.is_none() {
        return Ok(None);
    }
    let Some(call_id) = item
        .tool_call_id
        .clone()
        .filter(|value| !value.trim().is_empty())
    else {
        return Err(DomainError::InvalidState {
            message: format!("canonical tool item {} missing tool_call_id", item.item_id),
        });
    };
    let Some(name) = item
        .tool_name
        .clone()
        .filter(|value| !value.trim().is_empty())
    else {
        return Err(DomainError::InvalidState {
            message: format!("canonical tool item {} missing tool_name", item.item_id),
        });
    };
    Ok(Some(CanonicalToolCall {
        call_id,
        name,
        arguments: canonical_tool_value(&item.tool_arguments),
        result: canonical_tool_value(&item.tool_result),
        error: item.tool_error.clone(),
    }))
}

fn current_turn_item_to_canonical_worker(
    item: &ActiveExecutionTurnItem,
) -> Option<CanonicalWorkerRef> {
    if item.task_id.is_none() && item.worker_id.is_none() && item.role_id.is_none() {
        return None;
    }
    Some(CanonicalWorkerRef {
        task_id: item.task_id.clone(),
        worker_id: item.worker_id.clone(),
        role_id: item.role_id.clone(),
        title: item.title.clone(),
    })
}

fn current_turn_item_metadata(item: &ActiveExecutionTurnItem) -> HashMap<String, Value> {
    let mut metadata = item.metadata.clone();
    if let Some(output_kind) = match item.kind.as_str() {
        "assistant_stream" => Some("progress"),
        "assistant_final" => Some("final"),
        "assistant_error" => Some("error"),
        _ => None,
    } {
        metadata.insert(
            "assistantOutputKind".to_string(),
            Value::String(output_kind.to_string()),
        );
    }
    if let Some(value) = item
        .request_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert("requestId".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = item
        .user_message_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert("userMessageId".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = item
        .placeholder_message_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert(
            "placeholderMessageId".to_string(),
            Value::String(value.clone()),
        );
    }
    metadata
}

fn current_turn_item_renderable(
    item: &ActiveExecutionTurnItem,
    kind: CanonicalTurnItemKind,
    status: CanonicalTurnItemStatus,
) -> bool {
    if let Some(renderable) = item.requested_renderable() {
        return renderable;
    }
    if kind == CanonicalTurnItemKind::ToolCall
        && item
            .tool_name
            .as_deref()
            .and_then(BuiltinToolName::from_name)
            .is_some_and(|tool| !tool.is_session_timeline_renderable_tool_call())
    {
        return false;
    }
    let has_content = item
        .content
        .as_ref()
        .is_some_and(|content| !content.trim().is_empty());
    if kind == CanonicalTurnItemKind::AssistantText {
        return has_content || !status.is_terminal();
    }
    has_content || item.tool_call_id.is_some() || item.worker_id.is_some() || item.task_id.is_some()
}

fn current_turn_item_to_canonical_item(
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
    item: &ActiveExecutionTurnItem,
) -> DomainResult<CanonicalTurnItem> {
    let kind = canonical_current_turn_item_kind(&item.kind)?;
    let turn_status = canonical_current_turn_status(&turn.status)?;
    let mut status = canonical_current_turn_item_status(&item.status)?;
    if let Some(terminal_item_status) = terminal_item_status_for_canonical_turn_status(turn_status)
        && !status.is_terminal()
    {
        status = terminal_item_status;
    }
    let tool = current_turn_item_to_canonical_tool(item)?;
    if kind == CanonicalTurnItemKind::ToolCall && tool.is_none() {
        return Err(DomainError::InvalidState {
            message: format!("canonical tool item {} missing tool payload", item.item_id),
        });
    }
    Ok(CanonicalTurnItem {
        session_id: session_id.clone(),
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
        item_id: item.item_id.clone(),
        item_seq: item.item_seq,
        kind,
        created_at: turn.accepted_at,
        status,
        item_version: None,
        updated_at: UtcMillis::now(),
        title: item.title.clone(),
        content: item.content.clone(),
        blocks: Vec::new(),
        tool,
        worker: current_turn_item_to_canonical_worker(item),
        source_thread_id: item.source_thread_id.clone(),
        visibility: CanonicalTurnVisibility {
            renderable: current_turn_item_renderable(item, kind, status),
        },
        metadata: current_turn_item_metadata(item),
    })
}

fn current_turn_to_canonical_turn(
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
) -> DomainResult<CanonicalTurn> {
    let items = turn
        .items
        .iter()
        .map(|item| current_turn_item_to_canonical_item(session_id, turn, item))
        .collect::<DomainResult<Vec<_>>>()?;
    let metadata = turn
        .items
        .iter()
        .find(|item| item.kind == "user_message")
        .and_then(|item| item.metadata.get(TURN_REPLACES_TURN_ID_METADATA_KEY))
        .cloned()
        .map(|replaces_turn_id| {
            HashMap::from([(
                TURN_REPLACES_TURN_ID_METADATA_KEY.to_string(),
                replaces_turn_id,
            )])
        })
        .unwrap_or_default();
    let mut canonical_turn = CanonicalTurn {
        session_id: session_id.clone(),
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
        accepted_at: turn.accepted_at,
        completed_at: turn.completed_at,
        status: canonical_current_turn_status(&turn.status)?,
        response_duration_ms: turn
            .completed_at
            .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0)),
        usage: None,
        items,
        metadata,
    };
    canonical_turn.normalize();
    Ok(canonical_turn)
}

fn upsert_canonical_turn_in_state(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
) -> DomainResult<()> {
    let mut incoming = current_turn_to_canonical_turn(session_id, turn)?;
    apply_goal_response_duration_scope(state, &mut incoming);
    incoming.normalize();
    if let Some(existing) = state
        .canonical_turns
        .iter_mut()
        .find(|existing| existing.session_id == *session_id && existing.turn_id == incoming.turn_id)
    {
        incoming.validate_update_from(existing)?;
        for incoming_item in &incoming.items {
            if let Some(existing_item) = existing
                .items
                .iter()
                .find(|existing_item| existing_item.item_id == incoming_item.item_id)
            {
                incoming_item.validate_update_from(existing_item)?;
            }
        }
        *existing = incoming;
    } else {
        state.canonical_turns.push(incoming);
    }
    state.canonical_turns.sort_by(|left, right| {
        left.turn_seq
            .cmp(&right.turn_seq)
            .then_with(|| left.turn_id.cmp(&right.turn_id))
    });
    reconcile_goal_time_used(state);
    Ok(())
}

fn turn_matches_owner_id(turn: &CanonicalTurn, owner_id: &str) -> bool {
    turn.turn_id == owner_id
        || turn.items.iter().any(|item| {
            item.worker
                .as_ref()
                .and_then(|worker| worker.task_id.as_ref())
                .is_some_and(|task_id| task_id.as_str() == owner_id)
        })
}

fn goal_owns_turn(
    state: &SessionStoreState,
    goal: &crate::models::SessionGoal,
    turn: &CanonicalTurn,
) -> bool {
    turn.metadata
        .get(TURN_GOAL_ID_METADATA_KEY)
        .and_then(Value::as_str)
        .is_some_and(|goal_id| goal_id == goal.goal_id.as_str())
        || [
            goal.created_by_turn_id.as_deref(),
            goal.continuation.turn_id.as_deref(),
            goal.completion
                .as_ref()
                .map(|completion| completion.turn_id.as_str()),
            goal.blocker
                .as_ref()
                .map(|blocker| blocker.last_observed_turn_id.as_str()),
        ]
        .into_iter()
        .flatten()
        .any(|owner_id| turn_matches_owner_id(turn, owner_id))
        || state.plans.iter().any(|plan| {
            plan.session_id == turn.session_id
                && plan.goal_id.as_ref() == Some(&goal.goal_id)
                && plan
                    .task_bindings
                    .keys()
                    .any(|task_id| turn_matches_owner_id(turn, task_id.as_str()))
        })
}

fn goal_owns_execution_chain(
    state: &SessionStoreState,
    goal: &crate::models::SessionGoal,
    turn: &CanonicalTurn,
    chain: &ActiveExecutionChain,
) -> bool {
    goal_owns_turn(state, goal, turn)
        || [
            goal.created_by_turn_id.as_deref(),
            goal.continuation.turn_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|owner_id| owner_id == chain.root_task_id.as_str())
        || state.plans.iter().any(|plan| {
            plan.session_id == turn.session_id
                && plan.goal_id.as_ref() == Some(&goal.goal_id)
                && (plan.task_bindings.contains_key(&chain.root_task_id)
                    || chain
                        .branches
                        .iter()
                        .any(|branch| plan.task_bindings.contains_key(&branch.task_id)))
        })
}

fn goal_owns_execution_owner(
    state: &SessionStoreState,
    goal: &crate::models::SessionGoal,
    owner_id: &str,
) -> bool {
    match goal.continuation.phase {
        GoalContinuationPhase::Waiting => false,
        GoalContinuationPhase::Running => {
            goal.continuation.turn_id.as_deref() == Some(owner_id)
                || state.canonical_turns.iter().any(|turn| {
                    turn.session_id == goal.session_id
                        && turn_matches_owner_id(turn, owner_id)
                        && turn
                            .metadata
                            .get(TURN_GOAL_ID_METADATA_KEY)
                            .and_then(Value::as_str)
                            .is_some_and(|goal_id| goal_id == goal.goal_id.as_str())
                })
        }
        GoalContinuationPhase::Idle => {
            goal.created_by_turn_id.as_deref() == Some(owner_id)
                || state.plans.iter().any(|plan| {
                    plan.session_id == goal.session_id
                        && plan.goal_id.as_ref() == Some(&goal.goal_id)
                        && plan
                            .task_bindings
                            .keys()
                            .any(|task_id| task_id.as_str() == owner_id)
                })
                || state.canonical_turns.iter().any(|turn| {
                    turn.session_id == goal.session_id
                        && turn_matches_owner_id(turn, owner_id)
                        && goal_owns_turn(state, goal, turn)
                })
        }
    }
}

impl SessionStore {
    /// 返回当前执行允许观察的计划快照。
    ///
    /// 普通计划即使暂停仍可作为上下文展示；Goal 计划则必须同时满足 Goal active
    /// 且当前执行拥有该 Goal，避免暂停、等待或其他普通 Turn 读取到 Goal 指令。
    pub fn plan_for_execution_observer(
        &self,
        session_id: &SessionId,
        owner_id: &str,
    ) -> Option<SessionPlan> {
        let state = self.state.read().expect("session state read lock poisoned");
        let plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id)?;
        let Some(goal_id) = plan.goal_id.as_ref() else {
            return Some(plan.clone());
        };
        state
            .goals
            .iter()
            .find(|goal| {
                &goal.session_id == session_id
                    && &goal.goal_id == goal_id
                    && goal.status == GoalStatus::Active
                    && goal_owns_execution_owner(&state, goal, owner_id)
            })
            .map(|_| plan.clone())
    }

    /// 返回当前执行实际拥有的 active plan。
    ///
    /// 无 Goal 的普通计划仍由主线执行消费；绑定 Goal 的计划只能由创建该 Goal
    /// 的 Turn、已认领的 continuation Turn 或其明确绑定的任务消费。这样同一会话
    /// 中插入的普通 Turn 不会读取 Goal 计划，也不会被计划 follow-up 劫持。
    pub fn active_plan_for_execution_owner(
        &self,
        session_id: &SessionId,
        owner_id: &str,
    ) -> Option<SessionPlan> {
        self.plan_for_execution_observer(session_id, owner_id)
            .filter(|plan| plan.state == PlanState::Active)
    }

    pub fn active_goal_for_execution_owner(
        &self,
        session_id: &SessionId,
        owner_id: &str,
    ) -> Option<crate::models::SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .find(|goal| {
                &goal.session_id == session_id
                    && goal.status == GoalStatus::Active
                    && goal_owns_execution_owner(&state, goal, owner_id)
            })
            .cloned()
    }
}

fn bind_canonical_turn_to_goal_in_state(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    turn_id: &str,
    goal_id: &GoalId,
) {
    if let Some(turn) = state
        .canonical_turns
        .iter_mut()
        .find(|turn| turn.session_id == *session_id && turn.turn_id == turn_id)
    {
        turn.metadata.insert(
            TURN_GOAL_ID_METADATA_KEY.to_string(),
            Value::String(goal_id.to_string()),
        );
    }
    reconcile_goal_time_used(state);
}

fn is_goal_response_boundary(goal: &crate::models::SessionGoal, turn: &CanonicalTurn) -> bool {
    match goal.status {
        GoalStatus::Complete => goal
            .completion
            .as_ref()
            .is_some_and(|completion| turn_matches_owner_id(turn, &completion.turn_id)),
        GoalStatus::Blocked => goal
            .blocker
            .as_ref()
            .is_some_and(|blocker| turn_matches_owner_id(turn, &blocker.last_observed_turn_id)),
        GoalStatus::Active
        | GoalStatus::Paused
        | GoalStatus::UsageLimited
        | GoalStatus::BudgetLimited => false,
    }
}

fn apply_goal_response_duration_scope(state: &SessionStoreState, turn: &mut CanonicalTurn) {
    let owning_goal = state
        .goals
        .iter()
        .find(|goal| goal.session_id == turn.session_id && goal_owns_turn(state, goal, turn));
    let Some(goal) = owning_goal else {
        return;
    };
    turn.metadata.insert(
        TURN_GOAL_ID_METADATA_KEY.to_string(),
        Value::String(goal.goal_id.to_string()),
    );
    turn.metadata
        .remove(TURN_RESPONSE_DURATION_SCOPE_METADATA_KEY);
    if turn.status.is_terminal() && !is_goal_response_boundary(goal, turn) {
        turn.metadata.insert(
            TURN_RESPONSE_DURATION_SCOPE_METADATA_KEY.to_string(),
            Value::String(TURN_RESPONSE_DURATION_SCOPE_GOAL_PROGRESS.to_string()),
        );
    }
}

pub(super) fn reconcile_goal_time_used(state: &mut SessionStoreState) {
    let timing_by_goal = state
        .goals
        .iter()
        .map(|goal| {
            let elapsed_millis = state
                .canonical_turns
                .iter()
                .filter(|turn| {
                    turn.status != CanonicalTurnStatus::Superseded
                        && turn.completed_at.is_some()
                        && goal_owns_turn(state, goal, turn)
                })
                .filter_map(|turn| {
                    turn.completed_at
                        .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0))
                })
                .fold(0_u64, u64::saturating_add);
            let running_turn = match (goal.status, &goal.continuation.phase) {
                (GoalStatus::Active, GoalContinuationPhase::Waiting) => None,
                (GoalStatus::Active, _) => state
                    .canonical_turns
                    .iter()
                    .filter(|turn| {
                        turn.status != CanonicalTurnStatus::Superseded
                            && turn.completed_at.is_none()
                            && goal_owns_turn(state, goal, turn)
                    })
                    .max_by_key(|turn| (turn.accepted_at, turn.turn_seq)),
                (GoalStatus::Complete, _) => goal.completion.as_ref().and_then(|completion| {
                    state.canonical_turns.iter().find(|turn| {
                        turn.status != CanonicalTurnStatus::Superseded
                            && turn.completed_at.is_none()
                            && turn_matches_owner_id(turn, &completion.turn_id)
                            && goal_owns_turn(state, goal, turn)
                    })
                }),
                (GoalStatus::Paused, _)
                | (GoalStatus::Blocked, _)
                | (GoalStatus::UsageLimited, _)
                | (GoalStatus::BudgetLimited, _) => None,
            };
            (
                goal.goal_id.clone(),
                (
                    elapsed_millis,
                    running_turn.map(|turn| (turn.accepted_at, turn.turn_id.clone())),
                ),
            )
        })
        .collect::<HashMap<_, _>>();

    for goal in &mut state.goals {
        let (elapsed_millis, running_turn) = timing_by_goal
            .get(&goal.goal_id)
            .cloned()
            .unwrap_or_default();
        goal.time_used_millis = elapsed_millis;
        goal.time_used_seconds = elapsed_millis.saturating_div(1000);
        goal.timing_started_at = running_turn.as_ref().map(|(started_at, _)| *started_at);
        goal.timing_turn_id = running_turn.map(|(_, turn_id)| turn_id);
    }
}

pub(super) fn reconcile_goal_response_duration_scopes(state: &mut SessionStoreState) {
    let mut canonical_turns = std::mem::take(&mut state.canonical_turns);
    for turn in &mut canonical_turns {
        apply_goal_response_duration_scope(state, turn);
    }
    state.canonical_turns = canonical_turns;
    reconcile_goal_time_used(state);
}

fn replace_canonical_turn_in_state(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
) -> DomainResult<()> {
    let mut incoming = current_turn_to_canonical_turn(session_id, turn)?;
    apply_goal_response_duration_scope(state, &mut incoming);
    incoming.normalize();
    if let Some(existing) = state
        .canonical_turns
        .iter_mut()
        .find(|existing| existing.session_id == *session_id && existing.turn_id == incoming.turn_id)
    {
        *existing = incoming;
    } else {
        state.canonical_turns.push(incoming);
    }
    state.canonical_turns.sort_by(|left, right| {
        left.turn_seq
            .cmp(&right.turn_seq)
            .then_with(|| left.turn_id.cmp(&right.turn_id))
    });
    reconcile_goal_time_used(state);
    Ok(())
}

fn user_message_item(turn: &ActiveExecutionTurn) -> Option<&ActiveExecutionTurnItem> {
    turn.items.iter().find(|item| item.kind == "user_message")
}

fn validate_turn_replacement_target(
    state: &SessionStoreState,
    session_id: &SessionId,
    replace_turn_id: &str,
) -> DomainResult<usize> {
    let current_turn = state
        .execution_sidecar_store
        .runtime_sidecars
        .iter()
        .find(|sidecar| sidecar.session_id == *session_id)
        .and_then(|sidecar| sidecar.current_turn.as_ref())
        .ok_or(DomainError::InvalidState {
            message: format!("session {session_id} 没有可编辑的最近轮次"),
        })?;
    if current_turn.turn_id != replace_turn_id {
        return Err(DomainError::InvalidState {
            message: format!(
                "session {session_id} 最近轮次已变化: {} != {replace_turn_id}",
                current_turn.turn_id
            ),
        });
    }
    if canonical_current_turn_status(&current_turn.status)? != CanonicalTurnStatus::Cancelled {
        return Err(DomainError::InvalidState {
            message: format!("turn {replace_turn_id} 不是已停止轮次"),
        });
    }
    let interrupted_by_user = user_message_item(current_turn)
        .and_then(|item| item.metadata.get(TURN_INTERRUPTION_SOURCE_METADATA_KEY))
        .and_then(Value::as_str)
        .is_some_and(|source| source == TURN_INTERRUPTION_SOURCE_USER);
    if !interrupted_by_user {
        return Err(DomainError::InvalidState {
            message: format!("turn {replace_turn_id} 不是用户主动停止的轮次"),
        });
    }

    let latest_turn_index = state
        .canonical_turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.session_id == *session_id)
        .max_by(|(_, left), (_, right)| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        })
        .map(|(index, _)| index)
        .ok_or(DomainError::InvalidState {
            message: format!("session {session_id} 缺少 canonical 轮次"),
        })?;
    let latest_turn = &state.canonical_turns[latest_turn_index];
    if latest_turn.turn_id != replace_turn_id {
        return Err(DomainError::InvalidState {
            message: format!("turn {replace_turn_id} 已不是 session {session_id} 的最后一轮"),
        });
    }
    if latest_turn.status != CanonicalTurnStatus::Cancelled {
        return Err(DomainError::InvalidState {
            message: format!("canonical turn {replace_turn_id} 不是已停止状态"),
        });
    }
    Ok(latest_turn_index)
}

fn supersede_canonical_turn(
    turn: &mut CanonicalTurn,
    replacement_turn_id: &str,
    superseded_at: UtcMillis,
) {
    turn.status = CanonicalTurnStatus::Superseded;
    turn.metadata.insert(
        TURN_SUPERSEDED_REASON_METADATA_KEY.to_string(),
        Value::String("user_edit".to_string()),
    );
    turn.metadata.insert(
        TURN_SUPERSEDED_AT_METADATA_KEY.to_string(),
        Value::from(superseded_at.0),
    );
    turn.metadata.insert(
        TURN_SUPERSEDED_BY_TURN_ID_METADATA_KEY.to_string(),
        Value::String(replacement_turn_id.to_string()),
    );
}

fn durable_terminal_turn_should_win(
    state: &SessionStoreState,
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
) -> bool {
    state.canonical_turns.iter().any(|existing| {
        if existing.session_id != *session_id
            || existing.turn_id != turn.turn_id
            || !existing.status.is_terminal()
        {
            return false;
        }
        let has_active_item = existing.items.iter().any(|item| !item.status.is_terminal());
        !has_active_item || !current_turn_status_is_terminal(&turn.status)
    })
}

fn reconcile_assistant_output_kinds_from_sidecar(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
) -> DomainResult<()> {
    let source = current_turn_to_canonical_turn(session_id, turn)?;
    let Some(existing) = state
        .canonical_turns
        .iter_mut()
        .find(|candidate| candidate.session_id == *session_id && candidate.turn_id == turn.turn_id)
    else {
        return Ok(());
    };
    for source_item in source.items {
        let Some(output_kind) = source_item.metadata.get("assistantOutputKind").cloned() else {
            continue;
        };
        if let Some(existing_item) = existing
            .items
            .iter_mut()
            .find(|candidate| candidate.item_id == source_item.item_id)
        {
            existing_item
                .metadata
                .insert("assistantOutputKind".to_string(), output_kind);
        }
    }
    Ok(())
}

pub(super) fn restore_canonical_turns_from_sidecars(
    state: &mut SessionStoreState,
) -> DomainResult<()> {
    let mut seen = HashSet::<(SessionId, String)>::new();
    let mut turns = Vec::<(SessionId, ActiveExecutionTurn)>::new();
    for sidecar in &state.execution_sidecar_store.runtime_sidecars {
        if let Some(turn) = sidecar.current_turn.clone()
            && seen.insert((sidecar.session_id.clone(), turn.turn_id.clone()))
        {
            turns.push((sidecar.session_id.clone(), turn));
        }
        if let Some(turn) = sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.clone())
            && seen.insert((sidecar.session_id.clone(), turn.turn_id.clone()))
        {
            turns.push((sidecar.session_id.clone(), turn));
        }
    }

    for (session_id, turn) in turns {
        if durable_terminal_turn_should_win(state, &session_id, &turn) {
            reconcile_assistant_output_kinds_from_sidecar(state, &session_id, &turn)?;
            continue;
        }
        upsert_canonical_turn_in_state(state, &session_id, &turn)?;
    }
    Ok(())
}

pub(super) fn reconcile_terminal_goal_continuations(state: &mut SessionStoreState) {
    for goal in &mut state.goals {
        if goal.status != GoalStatus::Active
            || goal.continuation.phase != GoalContinuationPhase::Running
        {
            continue;
        }
        let Some(sidecar) = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| sidecar.session_id == goal.session_id)
        else {
            continue;
        };
        let Some(turn) = sidecar.current_turn.as_ref().or_else(|| {
            sidecar
                .active_execution_chain
                .as_ref()
                .and_then(|chain| chain.current_turn.as_ref())
        }) else {
            continue;
        };
        let owner_matches = [
            goal.continuation.turn_id.as_deref(),
            goal.created_by_turn_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|owner_turn_id| {
            turn.turn_id == owner_turn_id
                || turn.items.iter().any(|item| {
                    item.task_id
                        .as_ref()
                        .is_some_and(|id| id.as_str() == owner_turn_id)
                })
                || sidecar
                    .active_execution_chain
                    .as_ref()
                    .is_some_and(|chain| chain.root_task_id.as_str() == owner_turn_id)
        });
        if !owner_matches {
            continue;
        }
        if !current_turn_status_is_terminal(&turn.status) {
            continue;
        }
        let reason = match turn.status.trim().to_ascii_lowercase().as_str() {
            "cancelled" | "canceled" => GOAL_CONTINUATION_USER_INTERRUPTED,
            "interrupted" => GOAL_CONTINUATION_DAEMON_RESTART_INTERRUPTED,
            _ => GOAL_CONTINUATION_TURN_TERMINAL,
        };
        goal.continuation = GoalContinuationState {
            phase: GoalContinuationPhase::Waiting,
            turn_id: None,
            reason: Some(reason.to_string()),
        };
        goal.updated_at = turn.completed_at.unwrap_or(sidecar.updated_at);
    }
}

fn release_running_goal_continuation(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    continuation_turn_id: &str,
    reason: &str,
    updated_at: UtcMillis,
) {
    let Some(goal) = state.goals.iter_mut().find(|goal| {
        &goal.session_id == session_id
            && goal.status == GoalStatus::Active
            && goal.continuation.phase == GoalContinuationPhase::Running
            && (goal.continuation.turn_id.as_deref() == Some(continuation_turn_id)
                || goal.created_by_turn_id.as_deref() == Some(continuation_turn_id))
    }) else {
        return;
    };
    goal.continuation = GoalContinuationState {
        phase: GoalContinuationPhase::Waiting,
        turn_id: None,
        reason: Some(reason.to_string()),
    };
    goal.updated_at = updated_at;
}

fn reject_changed_current_turn_item_field(
    item_id: &str,
    field: &'static str,
    unchanged: bool,
) -> DomainResult<()> {
    if unchanged {
        return Ok(());
    }
    Err(DomainError::InvalidState {
        message: format!(
            "canonical turn item {item_id} attempted to change immutable field {field}"
        ),
    })
}

fn validate_current_turn_item_update(
    existing: &ActiveExecutionTurnItem,
    incoming: &ActiveExecutionTurnItem,
) -> DomainResult<()> {
    reject_changed_current_turn_item_field(
        &incoming.item_id,
        "itemSeq",
        incoming.item_seq == 0 || existing.item_seq == incoming.item_seq,
    )?;
    reject_changed_current_turn_item_field(
        &incoming.item_id,
        "kind",
        canonical_current_turn_item_kind(&existing.kind)?
            == canonical_current_turn_item_kind(&incoming.kind)?,
    )?;
    reject_changed_current_turn_item_field(
        &incoming.item_id,
        "tool.callId",
        existing.tool_call_id == incoming.tool_call_id,
    )?;

    let existing_status = canonical_current_turn_item_status(&existing.status)?;
    let incoming_status = canonical_current_turn_item_status(&incoming.status)?;
    if !existing_status.allows_transition_to(incoming_status) {
        return Err(DomainError::InvalidState {
            message: format!(
                "canonical turn item {} illegal status transition: {:?} -> {:?}",
                incoming.item_id, existing_status, incoming_status
            ),
        });
    }
    Ok(())
}

fn reject_conflicting_active_current_turn(
    session_id: &SessionId,
    existing_turn: Option<&ActiveExecutionTurn>,
    incoming_turn_id: Option<&str>,
) -> DomainResult<()> {
    let Some(existing_turn) = existing_turn else {
        return Ok(());
    };
    if current_turn_status_is_terminal(&existing_turn.status) {
        return Ok(());
    }
    if incoming_turn_id == Some(existing_turn.turn_id.as_str()) {
        return Ok(());
    }
    Err(DomainError::CurrentTurnConflict {
        session_id: session_id.to_string(),
        active_turn_id: existing_turn.turn_id.clone(),
    })
}

fn reject_duplicate_timeline_entry(timeline: &[TimelineEntry], entry_id: &str) -> DomainResult<()> {
    if timeline.iter().any(|entry| entry.entry_id == entry_id) {
        return Err(DomainError::InvalidState {
            message: format!("timeline entry {} already exists", entry_id),
        });
    }
    Ok(())
}

fn upsert_runtime_sidecar_in_state(state: &mut SessionStoreState, sidecar: SessionRuntimeSidecar) {
    if let Some(existing) = state
        .execution_sidecar_store
        .runtime_sidecars
        .iter_mut()
        .find(|existing| existing.session_id == sidecar.session_id)
    {
        *existing = sidecar;
    } else {
        state.execution_sidecar_store.runtime_sidecars.push(sidecar);
    }
}

fn record_session_completion(
    state: &mut SessionStoreState,
    session_id: &SessionId,
    completed_at: UtcMillis,
) {
    let Some(session) = state
        .sessions
        .iter_mut()
        .find(|session| &session.session_id == session_id)
    else {
        return;
    };
    if session
        .last_completed_at
        .is_none_or(|last_completed_at| completed_at > last_completed_at)
    {
        session.last_completed_at = Some(completed_at);
    }
}

fn append_item_to_current_turn(
    sidecar: &mut SessionRuntimeSidecar,
    mut item: ActiveExecutionTurnItem,
) -> DomainResult<Option<SessionRuntimeSidecar>> {
    let Some(turn) = sidecar.current_turn.as_mut() else {
        return Ok(None);
    };
    if let Some(existing) = turn
        .items
        .iter_mut()
        .find(|existing| existing.item_id == item.item_id)
    {
        validate_current_turn_item_update(existing, &item)?;
        if item.item_seq == 0 {
            item.item_seq = existing.item_seq;
        }
        if item.request_id.is_none() {
            item.request_id = existing.request_id.clone();
        }
        if item.user_message_id.is_none() {
            item.user_message_id = existing.user_message_id.clone();
        }
        if item.placeholder_message_id.is_none() {
            item.placeholder_message_id = existing.placeholder_message_id.clone();
        }
        *existing = item;
    } else {
        let next_item_seq = turn
            .items
            .iter()
            .map(|existing| existing.item_seq)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        if item.item_seq == 0 {
            item.item_seq = next_item_seq;
        }
        inherit_current_turn_aliases(turn, &mut item);
        turn.items.push(item);
    }
    turn.normalize();
    if let Some(chain) = sidecar.active_execution_chain.as_mut() {
        chain.current_turn = sidecar.current_turn.clone();
        chain.normalize();
    }
    sidecar.updated_at = UtcMillis::now();
    Ok(Some(sidecar.clone()))
}

impl SessionStore {
    fn sync_session_workspace_binding(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&magi_core::WorkspaceId>,
    ) {
        let Some(workspace_id) = workspace_id else {
            return;
        };
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
        {
            session.workspace_id = Some(workspace_id.to_string());
            session.updated_at = UtcMillis::now();
        }
    }

    fn ownership_from_active_execution_chain(chain: &ActiveExecutionChain) -> ExecutionOwnership {
        let primary_branch = chain.branches.iter().find(|branch| branch.is_primary);
        ExecutionOwnership {
            session_id: Some(chain.session_id.clone()),
            workspace_id: chain.workspace_id.clone(),
            mission_id: Some(chain.mission_id.clone()),
            task_id: primary_branch
                .map(|branch| branch.task_id.clone())
                .or_else(|| chain.active_branch_task_ids.first().cloned())
                .or_else(|| Some(chain.root_task_id.clone())),
            worker_id: primary_branch
                .map(|branch| branch.worker_id.clone())
                .or_else(|| chain.active_worker_bindings.first().cloned()),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        }
    }

    fn upsert_runtime_sidecar_with_reason(
        &self,
        sidecar: SessionRuntimeSidecar,
        reason: SessionSidecarFlushReason,
    ) {
        // 本函数只写 sidecar 元数据（ownership / chain / recovery / status）。
        // sidecar.current_turn 字段对调用方而言是只读快照——调用方在写锁之外
        // 读取它，再传进来仅用于持久化镜像。canonical turn 由显式的 turn 变更
        // 函数（upsert_current_turn_item / update_current_turn_status /
        // complete_current_turn_from_completed_root_task / cancel_current_turn）
        // 在各自的写锁内原子投影；这里若再次投影，会用过期快照对最新 canonical
        // 触发非法状态转换（例如 Failed→Completed），导致 panic 并毒化整个
        // session state RwLock。因此本函数绝不能从 sidecar.current_turn 反向
        // 重投影 canonical。
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        state
            .execution_sidecar_store
            .upsert_runtime_sidecar(sidecar);
        drop(state);
        self.mark_sidecar_dirty(reason);
    }

    fn derive_sidecar_status(
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

    fn build_active_execution_chain_sidecar(
        session_id: SessionId,
        mut active_execution_chain: ActiveExecutionChain,
        existing: Option<SessionRuntimeSidecar>,
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
        let recovery_id = active_execution_chain.recovery_ref.clone().or_else(|| {
            existing
                .as_ref()
                .and_then(|sidecar| sidecar.recovery_id.clone())
        });
        let incoming_execution_chain_ref = active_execution_chain.execution_chain_ref.clone();
        let existing_current_turn = existing
            .as_ref()
            .and_then(|sidecar| sidecar.current_turn.clone());
        let incoming_turn_id = active_execution_chain
            .current_turn
            .as_ref()
            .map(|turn| turn.turn_id.as_str());
        reject_conflicting_active_current_turn(
            &session_id,
            existing_current_turn.as_ref(),
            incoming_turn_id,
        )?;
        let existing_execution_chain_ref = existing.as_ref().and_then(|sidecar| {
            sidecar
                .active_execution_chain
                .as_ref()
                .map(|chain| chain.execution_chain_ref.as_str())
        });
        let current_turn = active_execution_chain.current_turn.clone().or_else(|| {
            (existing_execution_chain_ref == Some(incoming_execution_chain_ref.as_str()))
                .then(|| existing_current_turn.clone())
                .flatten()
        });
        active_execution_chain.current_turn = current_turn.clone();
        let active_execution_chain = Some(active_execution_chain);
        let ownership = active_execution_chain
            .as_ref()
            .map(Self::ownership_from_active_execution_chain)
            .unwrap_or_default();
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

    pub fn upsert_runtime_sidecar(&self, sidecar: SessionRuntimeSidecar) {
        self.upsert_runtime_sidecar_with_reason(
            sidecar,
            SessionSidecarFlushReason::UpsertRuntimeSidecar,
        );
    }

    // ---------------------------------------------------------------------
    // P6a Thread registry（Y 方案）
    // ---------------------------------------------------------------------

    /// 注册新 thread；调用方保证 `thread_id` 唯一。
    pub fn register_thread(&self, thread: ExecutionThread) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if state
            .thread_registry
            .iter()
            .any(|existing| existing.thread_id == thread.thread_id)
        {
            return;
        }
        state.thread_registry.push(thread);
    }

    /// 将 thread 标记为 `Active`，绑定当前 task；同时更新 last_used_at 与 handled_task_ids。
    pub fn activate_thread(&self, thread_id: &ThreadId, task_id: &TaskId, now: UtcMillis) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.status = ExecutionThreadStatus::Active;
            thread.last_used_at = now;
            if !thread.handled_task_ids.iter().any(|id| id == task_id) {
                thread.handled_task_ids.push(task_id.clone());
            }
        }
    }

    /// 将处理指定 task 且仍为 `Active` 的 thread 原子收口为 `Idle`。
    pub fn mark_task_threads_idle(&self, task_id: &TaskId, now: UtcMillis) -> usize {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let mut settled = 0;
        for thread in state.thread_registry.iter_mut().filter(|thread| {
            thread.status == ExecutionThreadStatus::Active
                && thread
                    .handled_task_ids
                    .iter()
                    .any(|handled| handled == task_id)
        }) {
            thread.status = ExecutionThreadStatus::Idle;
            thread.last_used_at = now;
            settled += 1;
        }
        settled
    }

    /// Mission 结束或显式回收时调用：session 下所有 thread 标记为 `Retired`。
    pub fn retire_session_threads(&self, session_id: &SessionId, now: UtcMillis) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        for thread in state.thread_registry.iter_mut() {
            if &thread.session_id == session_id && thread.status != ExecutionThreadStatus::Retired {
                thread.status = ExecutionThreadStatus::Retired;
                thread.last_used_at = now;
            }
        }
    }

    /// 只读快照：用于测试与调试。
    pub fn thread_registry_snapshot(&self, session_id: &SessionId) -> Vec<ExecutionThread> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_registry
            .iter()
            .filter(|thread| &thread.session_id == session_id)
            .cloned()
            .collect()
    }

    /// 查找 session 的 orchestrator 主线 thread。
    ///
    /// 该 thread 由 `ensure_session_mission` 在 session 首次接收 user 输入时
    /// spawn，与 session 共享生命周期。所有归属主线的 item 都以此 thread_id
    /// 作为 `source_thread_id` 锚点。
    pub fn orchestrator_thread_for_session(
        &self,
        session_id: &SessionId,
    ) -> Option<ExecutionThread> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_registry
            .iter()
            .find(|thread| {
                &thread.session_id == session_id && thread.role_id == ORCHESTRATOR_ROLE_ID
            })
            .cloned()
    }

    /// 确保 session 拥有 mission 并 spawn 对应的 orchestrator thread。
    ///
    /// 复用顺序：
    /// 1. session 已存在 orchestrator thread → 返回其 `(mission_id, thread_id)`
    /// 2. session runtime sidecar 已绑定 mission（来自 recovery / 前次 dispatch 的 ownership）
    ///    → 使用该 mission_id，并 spawn orchestrator thread
    /// 3. 否则调用 `mission_id_factory` 生成新 mission_id 并 spawn orchestrator thread
    ///
    /// 此方法是 session 进入"任意工作态"（聊天 / 任务派发 / 运行时 followup）的唯一入口，
    /// 保证"同 session 同 mission 同 orchestrator thread"的不变量。
    pub fn ensure_session_mission(
        &self,
        session_id: &SessionId,
        now: UtcMillis,
        mission_id_factory: impl FnOnce() -> MissionId,
    ) -> (MissionId, ThreadId) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state.thread_registry.iter().find(|thread| {
            &thread.session_id == session_id && thread.role_id == ORCHESTRATOR_ROLE_ID
        }) {
            return (thread.mission_id.clone(), thread.thread_id.clone());
        }
        let existing_mission = state
            .execution_sidecar_store
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.ownership.mission_id.clone());
        let mission_id = existing_mission.unwrap_or_else(mission_id_factory);
        let thread_id = ThreadId::new(format!("thread-orchestrator-{}", session_id));
        state.thread_registry.push(ExecutionThread {
            thread_id: thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: WorkerId::new(format!("worker-orchestrator-{}", session_id)),
            status: ExecutionThreadStatus::Idle,
            created_at: now,
            last_used_at: now,
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
            message_history: Vec::new(),
        });
        (mission_id, thread_id)
    }

    /// 依据 `source_thread_id` 判定 item 的可见性目的地。返回值是"主线"还是
    /// "task 详情"，由 thread 的 `role_id` 决定：
    /// - 该 thread 是 session 的 orchestrator thread → `Main`
    /// - 其他 thread → `TaskDetail { role_id, worker_id }`
    ///
    /// 约束：传入的 `source_thread_id` 必须是本 session 已注册 thread；
    /// 否则返回 `None`，调用方按"未知来源"处理（通常只出现在 P6 之前遗留的
    /// canonical turn，新写入路径不会走到）。
    pub fn resolve_thread_visibility(
        &self,
        session_id: &SessionId,
        source_thread_id: &ThreadId,
    ) -> Option<ThreadVisibility> {
        let state = self.state.read().expect("session state read lock poisoned");
        let thread = state.thread_registry.iter().find(|thread| {
            &thread.session_id == session_id && &thread.thread_id == source_thread_id
        })?;
        if thread.role_id == ORCHESTRATOR_ROLE_ID {
            Some(ThreadVisibility::Main)
        } else {
            Some(ThreadVisibility::TaskDetail {
                role_id: thread.role_id.clone(),
                worker_id: thread.worker_instance_id.clone(),
            })
        }
    }

    /// P6b：读取指定 thread 内部的对话记录。代理 task thread 为单 task 独占，
    /// 因此这里不会把同 role 的历史 task 注入新 task。
    pub fn thread_message_history(&self, thread_id: &ThreadId) -> Vec<ThreadChatMessage> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .map(|thread| thread.message_history.clone())
            .unwrap_or_default()
    }

    /// 返回供模型消费的 thread 上下文视图。检查点覆盖的原始消息不会重复注入，
    /// 但仍完整保留在 transcript 中用于审计、恢复和后续重新压缩。
    pub fn thread_context_history(&self, thread_id: &ThreadId) -> Vec<ThreadChatMessage> {
        let state = self.state.read().expect("session state read lock poisoned");
        let Some(thread) = state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
        else {
            return Vec::new();
        };
        let Some(checkpoint) = state
            .thread_context_checkpoints
            .iter()
            .find(|checkpoint| &checkpoint.thread_id == thread_id)
        else {
            return thread.message_history.clone();
        };
        let source_message_count = checkpoint
            .source_message_count
            .min(thread.message_history.len());
        let mut history = Vec::with_capacity(
            thread
                .message_history
                .len()
                .saturating_sub(source_message_count)
                + 1,
        );
        history.push(checkpoint.summary_message.clone());
        history.extend(
            thread.message_history[source_message_count..]
                .iter()
                .cloned(),
        );
        history
    }

    pub fn thread_context_checkpoint(
        &self,
        thread_id: &ThreadId,
    ) -> Option<ThreadContextCheckpoint> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_context_checkpoints
            .iter()
            .find(|checkpoint| &checkpoint.thread_id == thread_id)
            .cloned()
    }

    pub fn thread_context_window_tokens(&self, thread_id: &ThreadId) -> Option<u64> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .and_then(|thread| thread.observed_context_window_tokens)
    }

    pub fn record_thread_context_window_tokens(
        &self,
        thread_id: &ThreadId,
        context_window_tokens: u64,
        now: UtcMillis,
    ) {
        if context_window_tokens == 0 {
            return;
        }
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.observed_context_window_tokens = Some(context_window_tokens);
            thread.last_used_at = now;
        }
    }

    pub fn install_thread_context_checkpoint(
        &self,
        thread_id: &ThreadId,
        checkpoint: ThreadContextCheckpoint,
        now: UtcMillis,
    ) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let Some(message_count) = state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .map(|thread| thread.message_history.len())
        else {
            return;
        };
        let checkpoint = ThreadContextCheckpoint {
            thread_id: thread_id.clone(),
            source_message_count: checkpoint.source_message_count.min(message_count),
            ..checkpoint
        };
        if let Some(existing) = state
            .thread_context_checkpoints
            .iter_mut()
            .find(|existing| &existing.thread_id == thread_id)
        {
            *existing = checkpoint;
        } else {
            state.thread_context_checkpoints.push(checkpoint);
        }
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.last_used_at = now;
        }
    }

    pub fn clear_thread_context_checkpoint(&self, thread_id: &ThreadId) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        state
            .thread_context_checkpoints
            .retain(|checkpoint| &checkpoint.thread_id != thread_id);
    }

    /// P6b：将本轮 task 的 LLM 对话追加到当前 thread 的审计 / 恢复记录。
    pub fn append_thread_messages(
        &self,
        thread_id: &ThreadId,
        messages: Vec<ThreadChatMessage>,
        now: UtcMillis,
    ) {
        if messages.is_empty() {
            return;
        }
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.message_history.extend(messages);
            thread.last_used_at = now;
        }
    }

    /// 用新的 transcript 替换指定 thread 的原始对话记录。
    ///
    /// 该操作用于用户编辑、恢复分支复制等真实历史变更；已有上下文检查点会失效。
    pub fn replace_thread_messages(
        &self,
        thread_id: &ThreadId,
        messages: Vec<ThreadChatMessage>,
        now: UtcMillis,
    ) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.message_history = messages;
            thread.last_used_at = now;
        }
        state
            .thread_context_checkpoints
            .retain(|checkpoint| &checkpoint.thread_id != thread_id);
    }

    pub fn bind_execution_ownership(&self, session_id: SessionId, ownership: ExecutionOwnership) {
        let session_key = session_id.clone();
        let existing = self.runtime_sidecar(&session_id);
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
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id,
                ownership: ownership.clone(),
                recovery_id: recovery_id.clone(),
                current_turn,
                active_execution_chain,
                status,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::BindExecutionOwnership,
        );
        self.sync_session_workspace_binding(&session_key, ownership.workspace_id.as_ref());
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
        let existing = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| sidecar.session_id == session_id)
            .cloned();
        reject_conflicting_active_current_turn(
            &session_id,
            existing
                .as_ref()
                .and_then(|sidecar| sidecar.current_turn.as_ref()),
            Some(turn.turn_id.as_str()),
        )?;
        reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;

        state.timeline.push(TimelineEntry {
            entry_id: entry_id.clone(),
            session_id: session_id.clone(),
            kind,
            message,
            occurred_at,
        });
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.updated_at = occurred_at;
        }

        let (ownership, recovery_id, active_execution_chain, status) =
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
        let updated = SessionRuntimeSidecar {
            session_id: session_id.clone(),
            ownership,
            recovery_id,
            current_turn: Some(turn),
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        if let Some(turn) = updated.current_turn.as_ref() {
            upsert_canonical_turn_in_state(&mut state, &session_id, turn)?;
        }
        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertCurrentTurn);
        Ok((entry_id, updated))
    }

    pub fn replace_current_turn_with_timeline_entry(
        &self,
        session_id: SessionId,
        replace_turn_id: &str,
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
        reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;
        let replaced_turn_index =
            validate_turn_replacement_target(&state, &session_id, replace_turn_id)?;
        if state
            .canonical_turns
            .iter()
            .any(|existing| existing.session_id == session_id && existing.turn_id == turn.turn_id)
        {
            return Err(DomainError::InvalidState {
                message: format!("canonical turn {} 已存在", turn.turn_id),
            });
        }
        let incoming_canonical_turn = current_turn_to_canonical_turn(&session_id, &turn)?;
        let existing = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| sidecar.session_id == session_id)
            .cloned()
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let updated = SessionRuntimeSidecar {
            session_id: session_id.clone(),
            ownership: existing.ownership,
            recovery_id: existing.recovery_id,
            current_turn: Some(turn),
            active_execution_chain: existing.active_execution_chain,
            status: existing.status,
            updated_at: UtcMillis::now(),
        };

        state.timeline.push(TimelineEntry {
            entry_id: entry_id.clone(),
            session_id: session_id.clone(),
            kind,
            message,
            occurred_at,
        });
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.updated_at = occurred_at;
        }
        supersede_canonical_turn(
            &mut state.canonical_turns[replaced_turn_index],
            &incoming_canonical_turn.turn_id,
            occurred_at,
        );
        let superseded_turn = state.canonical_turns[replaced_turn_index].clone();
        state.canonical_turns.push(incoming_canonical_turn);
        state.canonical_turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertCurrentTurn);
        Ok((entry_id, updated, superseded_turn))
    }

    pub fn accept_active_execution_chain_with_timeline_entry(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
    ) -> DomainResult<(String, SessionRuntimeSidecar)> {
        self.accept_active_execution_chain_with_timeline_entry_inner(
            session_id,
            timeline_entry,
            active_execution_chain,
            None,
        )
    }

    pub fn accept_goal_continuation_with_timeline_entry(
        &self,
        session_id: SessionId,
        goal_id: &GoalId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
    ) -> DomainResult<(String, SessionRuntimeSidecar)> {
        self.accept_active_execution_chain_with_timeline_entry_inner(
            session_id,
            timeline_entry,
            active_execution_chain,
            Some(goal_id),
        )
    }

    fn accept_active_execution_chain_with_timeline_entry_inner(
        &self,
        session_id: SessionId,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
        continuation_goal_id: Option<&GoalId>,
    ) -> DomainResult<(String, SessionRuntimeSidecar)> {
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
        let continuation_turn_id = active_execution_chain.root_task_id.to_string();
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
        let continuation_goal_index = continuation_goal_id
            .map(|goal_id| {
                state
                    .goals
                    .iter()
                    .position(|goal| goal.session_id == session_id && &goal.goal_id == goal_id)
                    .ok_or(DomainError::NotFound { entity: "goal" })
            })
            .transpose()?;
        if let Some(goal_index) = continuation_goal_index {
            let goal = &state.goals[goal_index];
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
            .cloned();
        let updated = Self::build_active_execution_chain_sidecar(
            session_id.clone(),
            active_execution_chain,
            existing,
        )?;
        reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;

        if let Some(turn) = updated.current_turn.as_ref() {
            upsert_canonical_turn_in_state(&mut state, &session_id, turn)?;
            if let Some(goal_id) = continuation_goal_id {
                bind_canonical_turn_to_goal_in_state(
                    &mut state,
                    &session_id,
                    &turn.turn_id,
                    goal_id,
                );
            }
        }

        state.timeline.push(TimelineEntry {
            entry_id: entry_id.clone(),
            session_id: session_id.clone(),
            kind,
            message,
            occurred_at,
        });
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.updated_at = occurred_at;
        }

        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        if let Some(goal_index) = continuation_goal_index {
            let goal = &mut state.goals[goal_index];
            goal.continuation = GoalContinuationState {
                phase: GoalContinuationPhase::Running,
                turn_id: Some(continuation_turn_id),
                reason: None,
            };
            goal.updated_at = occurred_at;
        }
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertActiveExecutionChain);
        self.sync_session_workspace_binding(
            &updated.session_id,
            updated.ownership.workspace_id.as_ref(),
        );
        Ok((entry_id, updated))
    }

    pub fn replace_current_turn_with_active_execution_chain_and_timeline_entry(
        &self,
        session_id: SessionId,
        replace_turn_id: &str,
        timeline_entry: TimelineEntryInput,
        active_execution_chain: ActiveExecutionChain,
    ) -> DomainResult<(String, SessionRuntimeSidecar, CanonicalTurn)> {
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
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
        reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;
        let replaced_turn_index =
            validate_turn_replacement_target(&state, &session_id, replace_turn_id)?;
        let existing = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| sidecar.session_id == session_id)
            .cloned();
        let updated = Self::build_active_execution_chain_sidecar(
            session_id.clone(),
            active_execution_chain,
            existing,
        )?;
        let incoming_turn = updated
            .current_turn
            .as_ref()
            .ok_or(DomainError::InvalidState {
                message: "replacement active execution chain 缺少 current_turn".to_string(),
            })?;
        if state.canonical_turns.iter().any(|existing| {
            existing.session_id == session_id && existing.turn_id == incoming_turn.turn_id
        }) {
            return Err(DomainError::InvalidState {
                message: format!("canonical turn {} 已存在", incoming_turn.turn_id),
            });
        }
        let incoming_canonical_turn = current_turn_to_canonical_turn(&session_id, incoming_turn)?;

        state.timeline.push(TimelineEntry {
            entry_id: entry_id.clone(),
            session_id: session_id.clone(),
            kind,
            message,
            occurred_at,
        });
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.updated_at = occurred_at;
        }
        supersede_canonical_turn(
            &mut state.canonical_turns[replaced_turn_index],
            &incoming_canonical_turn.turn_id,
            occurred_at,
        );
        let superseded_turn = state.canonical_turns[replaced_turn_index].clone();
        state.canonical_turns.push(incoming_canonical_turn);
        state.canonical_turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertActiveExecutionChain);
        self.sync_session_workspace_binding(
            &updated.session_id,
            updated.ownership.workspace_id.as_ref(),
        );
        Ok((entry_id, updated, superseded_turn))
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
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| &sidecar.session_id == session_id)
            .and_then(|sidecar| sidecar.current_turn.as_ref());
        reject_conflicting_active_current_turn(session_id, existing_turn, None)
    }

    pub fn upsert_active_execution_chain(
        &self,
        session_id: SessionId,
        active_execution_chain: ActiveExecutionChain,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let existing = state
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| sidecar.session_id == session_id)
            .cloned();
        let updated = Self::build_active_execution_chain_sidecar(
            session_id.clone(),
            active_execution_chain,
            existing,
        )?;
        if let Some(turn) = updated.current_turn.as_ref() {
            upsert_canonical_turn_in_state(&mut state, &session_id, turn)?;
        }
        upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        drop(state);
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertActiveExecutionChain);
        self.sync_session_workspace_binding(
            &updated.session_id,
            updated.ownership.workspace_id.as_ref(),
        );
        Ok(updated)
    }

    pub fn apply_recovery_resume_input(
        &self,
        session_id: SessionId,
        input: RecoveryResumeInput,
    ) -> DomainResult<()> {
        let existing = self.runtime_sidecar(&session_id);
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
            .and_then(|sidecar| sidecar.active_execution_chain.clone())
            .map(|mut chain| {
                chain.recovery_ref = Some(input.recovery_id.clone());
                chain
            });
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
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership: ownership.clone(),
                recovery_id: Some(input.recovery_id),
                current_turn,
                active_execution_chain,
                status: SessionExecutionSidecarStatus::RecoveryLinked,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::ApplyRecoveryResumeInput,
        );
        self.sync_session_workspace_binding(&session_id, ownership.workspace_id.as_ref());
        Ok(())
    }

    pub fn apply_resume_execution_target(
        &self,
        session_id: &SessionId,
        target: &TaskExecutionTarget,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let recovery_id = target.recovery_id.as_deref();
        if let Some(existing_recovery_id) = existing.recovery_id.as_deref()
            && recovery_id.is_some_and(|value| value != existing_recovery_id)
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session_runtime_sidecar 的 recovery_id 与恢复目标不一致: {existing_recovery_id} != {}",
                    recovery_id.unwrap_or_default()
                ),
            });
        }
        if let Some(execution_chain_ref) = existing.ownership.execution_chain_ref.as_deref()
            && target
                .execution_chain_ref
                .as_deref()
                .is_some_and(|value| value != execution_chain_ref)
        {
            return Err(DomainError::InvalidState {
                message: format!(
                    "session_runtime_sidecar 的 execution_chain_ref 与恢复目标不一致: {execution_chain_ref} != {}",
                    target.execution_chain_ref.as_deref().unwrap_or_default()
                ),
            });
        }
        let active_execution_chain = existing.active_execution_chain.clone().map(|mut chain| {
            if let Some(recovery_ref) = target.recovery_id.clone() {
                chain.recovery_ref = Some(recovery_ref);
            }
            chain
        });
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
            recovery_id: target.recovery_id.clone(),
            current_turn: existing.current_turn,
            active_execution_chain,
            status: SessionExecutionSidecarStatus::Resumed,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::ApplyResumeExecutionTarget,
        );
        self.sync_session_workspace_binding(
            &updated.session_id,
            updated.ownership.workspace_id.as_ref(),
        );
        Ok(updated)
    }

    pub fn attach_recovery_id(
        &self,
        session_id: &SessionId,
        recovery_id: Option<String>,
    ) -> DomainResult<SessionRuntimeSidecar> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let status = Self::derive_sidecar_status(
            &existing.ownership,
            recovery_id.as_deref(),
            Some(&existing.status),
        );
        let active_execution_chain = existing.active_execution_chain.map(|mut chain| {
            chain.recovery_ref = recovery_id.clone();
            chain
        });
        let updated = SessionRuntimeSidecar {
            session_id: existing.session_id,
            ownership: existing.ownership,
            recovery_id,
            current_turn: existing.current_turn,
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        self.upsert_runtime_sidecar_with_reason(
            updated.clone(),
            SessionSidecarFlushReason::AttachRecoveryRef,
        );
        Ok(updated)
    }

    pub fn attach_recovery_ref(
        &self,
        session_id: &SessionId,
        recovery_ref: Option<String>,
    ) -> DomainResult<SessionRuntimeSidecar> {
        self.attach_recovery_id(session_id, recovery_ref)
    }

    pub fn update_active_execution_branch_snapshot(
        &self,
        update: ActiveExecutionBranchSnapshotUpdate,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let ActiveExecutionBranchSnapshotUpdate {
            task_id,
            worker_id,
            stage,
            lease_id,
            execution_intent_ref,
            binding_lifecycle,
            checkpoint_stage,
            next_step_index,
            checkpoint_at,
            resume_mode,
            resume_token,
        } = update;
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = 'updated: {
                for sidecar in &mut state.execution_sidecar_store.runtime_sidecars {
                    let Some(chain) = sidecar.active_execution_chain.as_mut() else {
                        continue;
                    };
                    let Some(branch) = chain
                        .branches
                        .iter_mut()
                        .find(|branch| branch.task_id == task_id)
                    else {
                        continue;
                    };
                    branch.worker_id = worker_id.clone();
                    branch.stage = stage.clone();
                    branch.lease_id = lease_id.clone();
                    branch.execution_intent_ref = execution_intent_ref.clone();
                    branch.binding_lifecycle = binding_lifecycle.clone();
                    branch.checkpoint_stage = checkpoint_stage.clone();
                    branch.next_step_index = next_step_index;
                    branch.checkpoint_at = checkpoint_at;
                    branch.resume_mode = resume_mode.clone();
                    branch.resume_token = resume_token.clone();
                    if let Some(turn) = sidecar.current_turn.as_mut() {
                        turn.normalize();
                    }
                    chain.active_branch_task_ids = chain
                        .branches
                        .iter()
                        .map(|entry| entry.task_id.clone())
                        .collect();
                    chain.active_worker_bindings = chain
                        .branches
                        .iter()
                        .map(|entry| entry.worker_id.clone())
                        .collect();
                    chain.normalize();
                    sidecar.ownership = Self::ownership_from_active_execution_chain(chain);
                    let existing_status = sidecar.status.clone();
                    sidecar.status = Self::derive_sidecar_status(
                        &sidecar.ownership,
                        sidecar.recovery_id.as_deref(),
                        Some(&existing_status),
                    );
                    sidecar.updated_at = UtcMillis::now();
                    break 'updated Some(sidecar.clone());
                }
                None
            };
            if let Some(updated) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, &updated.session_id, turn)?;
            }
            updated
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateActiveExecutionBranchSnapshot);
        }
        Ok(updated)
    }

    pub fn upsert_current_turn(
        &self,
        session_id: SessionId,
        mut turn: ActiveExecutionTurn,
    ) -> DomainResult<SessionRuntimeSidecar> {
        turn.status = normalize_stored_current_turn_status(turn.status);
        turn.normalize();
        let existing = self.runtime_sidecar(&session_id);
        let (ownership, recovery_id, active_execution_chain, status) =
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
        let updated = SessionRuntimeSidecar {
            session_id: session_id.clone(),
            ownership,
            recovery_id,
            current_turn: Some(turn),
            active_execution_chain,
            status,
            updated_at: UtcMillis::now(),
        };
        {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            if let Some(turn) = updated.current_turn.as_ref() {
                upsert_canonical_turn_in_state(&mut state, &session_id, turn)?;
            }
            upsert_runtime_sidecar_in_state(&mut state, updated.clone());
        }
        self.mark_sidecar_dirty(SessionSidecarFlushReason::UpsertCurrentTurn);
        Ok(updated)
    }

    pub fn append_current_turn_item(
        &self,
        session_id: &SessionId,
        item: ActiveExecutionTurnItem,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                append_item_to_current_turn(sidecar, item)?
            };
            if let Some(updated) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
            }
            updated
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::AppendCurrentTurnItem);
        }
        Ok(updated)
    }

    pub fn append_current_turn_item_with_timeline_entry(
        &self,
        session_id: &SessionId,
        timeline_entry: TimelineEntryInput,
        item: ActiveExecutionTurnItem,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let TimelineEntryInput {
            entry_id,
            kind,
            message,
            occurred_at,
        } = timeline_entry;
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(sidecar_index) = state
                .execution_sidecar_store
                .runtime_sidecars
                .iter()
                .position(|sidecar| &sidecar.session_id == session_id)
            else {
                return Err(DomainError::NotFound {
                    entity: "session_runtime_sidecar",
                });
            };
            let Some(turn) = state.execution_sidecar_store.runtime_sidecars[sidecar_index]
                .current_turn
                .as_ref()
            else {
                return Ok(None);
            };
            if let Some(existing) = turn
                .items
                .iter()
                .find(|existing| existing.item_id == item.item_id)
            {
                validate_current_turn_item_update(existing, &item)?;
            }
            reject_duplicate_timeline_entry(&state.timeline, &entry_id)?;
            state.timeline.push(TimelineEntry {
                entry_id,
                session_id: session_id.clone(),
                kind,
                message,
                occurred_at,
            });
            if let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| &session.session_id == session_id)
            {
                session.updated_at = occurred_at;
            }
            let updated = append_item_to_current_turn(
                &mut state.execution_sidecar_store.runtime_sidecars[sidecar_index],
                item,
            )?;
            if let Some(updated) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
            }
            updated
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::AppendCurrentTurnItem);
        }
        Ok(updated)
    }

    pub fn upsert_current_turn_item(
        &self,
        session_id: &SessionId,
        mut item: ActiveExecutionTurnItem,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(None);
                };

                if let Some(existing) = turn
                    .items
                    .iter_mut()
                    .find(|existing| existing.item_id == item.item_id)
                {
                    validate_current_turn_item_update(existing, &item)?;
                    if item.item_seq == 0 {
                        item.item_seq = existing.item_seq;
                    }
                    if item.request_id.is_none() {
                        item.request_id = existing.request_id.clone();
                    }
                    if item.user_message_id.is_none() {
                        item.user_message_id = existing.user_message_id.clone();
                    }
                    if item.placeholder_message_id.is_none() {
                        item.placeholder_message_id = existing.placeholder_message_id.clone();
                    }
                    *existing = item;
                } else {
                    let next_item_seq = turn
                        .items
                        .iter()
                        .map(|existing| existing.item_seq)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1);
                    if item.item_seq == 0 {
                        item.item_seq = next_item_seq;
                    }
                    inherit_current_turn_aliases(turn, &mut item);
                    turn.items.push(item);
                }

                turn.normalize();
                if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                    chain.current_turn = sidecar.current_turn.clone();
                    chain.normalize();
                }
                sidecar.updated_at = UtcMillis::now();
                Some(sidecar.clone())
            };
            if let Some(updated) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
            }
            updated
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::AppendCurrentTurnItem);
        }
        Ok(updated)
    }

    pub fn update_current_turn_status(
        &self,
        session_id: &SessionId,
        status: impl Into<String>,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(None);
                };
                turn.status = normalize_stored_current_turn_status(status.into());
                if let Some(item_status) = terminal_item_status_for_turn_status(&turn.status) {
                    for item in &mut turn.items {
                        if current_turn_item_status_is_active(&item.status) {
                            item.status = item_status.to_string();
                        }
                        if item
                            .tool_status
                            .as_deref()
                            .is_some_and(current_turn_item_status_is_active)
                        {
                            item.tool_status = Some(item_status.to_string());
                        }
                    }
                }
                if turn.completed_at.is_none() && current_turn_status_is_terminal(&turn.status) {
                    turn.completed_at = Some(UtcMillis::now());
                }
                turn.normalize();
                if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                    chain.current_turn = sidecar.current_turn.clone();
                    chain.normalize();
                }
                sidecar.updated_at = UtcMillis::now();
                Some(sidecar.clone())
            };
            let completed_at = updated
                .as_ref()
                .and_then(|sidecar| sidecar.current_turn.as_ref())
                .filter(|turn| turn.status == "completed")
                .and_then(|turn| turn.completed_at);
            if let Some(completed_at) = completed_at {
                record_session_completion(&mut state, session_id, completed_at);
            }
            if let Some(updated) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
            }
            updated
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
        }
        Ok(updated)
    }

    pub fn complete_current_turn_from_completed_root_task(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(None);
                };
                turn.status = "completed".to_string();
                if turn.completed_at.is_none() {
                    turn.completed_at = Some(UtcMillis::now());
                }
                for item in &mut turn.items {
                    let normalized = item.status.trim().to_ascii_lowercase();
                    if current_turn_item_status_is_active(&item.status)
                        || matches!(normalized.as_str(), "cancelled" | "canceled" | "blocked")
                    {
                        item.status = "completed".to_string();
                    }
                    if let Some(tool_status) = item.tool_status.as_deref() {
                        let normalized_tool_status = tool_status.trim().to_ascii_lowercase();
                        if current_turn_item_status_is_active(tool_status)
                            || matches!(
                                normalized_tool_status.as_str(),
                                "cancelled" | "canceled" | "blocked"
                            )
                        {
                            item.tool_status = Some("completed".to_string());
                        }
                    }
                }
                turn.normalize();
                if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                    chain.current_turn = sidecar.current_turn.clone();
                    chain.normalize();
                }
                sidecar.updated_at = UtcMillis::now();
                Some(sidecar.clone())
            };
            if let Some(completed_at) = updated
                .as_ref()
                .and_then(|sidecar| sidecar.current_turn.as_ref())
                .and_then(|turn| turn.completed_at)
            {
                record_session_completion(&mut state, session_id, completed_at);
            }
            if let Some(updated) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                replace_canonical_turn_in_state(&mut state, session_id, turn)?;
            }
            updated
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
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
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                if sidecar.active_execution_chain.is_none() {
                    return Err(DomainError::InvalidState {
                        message: format!("session {session_id} 没有可恢复的执行链"),
                    });
                }
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(None);
                };
                if current_turn_status_is_terminal(&turn.status) {
                    return Ok(None);
                }

                let now = UtcMillis::now();
                let interrupted_at =
                    UtcMillis(sidecar.updated_at.0.max(turn.accepted_at.0).min(now.0));
                for item in &mut turn.items {
                    if current_turn_item_status_is_active(&item.status) {
                        item.status = "cancelled".to_string();
                    }
                    if item
                        .tool_status
                        .as_deref()
                        .is_some_and(current_turn_item_status_is_active)
                    {
                        item.tool_status = Some("cancelled".to_string());
                    }
                }

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
                if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                    chain.current_turn = sidecar.current_turn.clone();
                    chain.normalize();
                }
                sidecar.updated_at = now;
                Some((
                    sidecar
                        .active_execution_chain
                        .as_ref()
                        .map(|chain| chain.root_task_id.to_string())
                        .unwrap_or(current_turn_id),
                    interrupted_at,
                    sidecar.clone(),
                ))
            };
            if let Some((continuation_turn_id, updated_at, updated)) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
                release_running_goal_continuation(
                    &mut state,
                    session_id,
                    continuation_turn_id,
                    GOAL_CONTINUATION_DAEMON_RESTART_INTERRUPTED,
                    *updated_at,
                );
            }
            updated.map(|(_, _, sidecar)| sidecar)
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
        }
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
            .execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| &sidecar.session_id == session_id)
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
            super::goals::activate_paused_plan(&mut state.plans[plan_index], now);
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

    fn has_interrupted_recovery_with_state(&self, session_id: &SessionId, state: &str) -> bool {
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

    fn set_interrupted_recovery_state(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        expected_state: &str,
        next_state: &str,
    ) -> DomainResult<Option<String>> {
        let updated_turn_id = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(None);
                };
                if turn.status != "interrupted" {
                    return Ok(None);
                }
                if expected_turn_id.is_some_and(|expected| expected != turn.turn_id) {
                    return Ok(None);
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
                if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                    chain.current_turn = sidecar.current_turn.clone();
                    chain.normalize();
                }
                sidecar.updated_at = UtcMillis::now();
                Some((turn_id, sidecar.clone()))
            };
            if let Some((_, updated)) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
            }
            updated.map(|(turn_id, _)| turn_id)
        };
        if updated_turn_id.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
        }
        Ok(updated_turn_id)
    }

    fn cancel_current_turn_with_source(
        &self,
        session_id: &SessionId,
        interrupted_by_user: bool,
    ) -> DomainResult<Option<SessionRuntimeSidecar>> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let updated = {
                let Some(sidecar) = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter_mut()
                    .find(|sidecar| &sidecar.session_id == session_id)
                else {
                    return Err(DomainError::NotFound {
                        entity: "session_runtime_sidecar",
                    });
                };
                let Some(turn) = sidecar.current_turn.as_mut() else {
                    return Ok(None);
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
                    for item in &mut turn.items {
                        if current_turn_item_status_is_active(&item.status) {
                            item.status = "cancelled".to_string();
                        }
                        if item
                            .tool_status
                            .as_deref()
                            .is_some_and(current_turn_item_status_is_active)
                        {
                            item.tool_status = Some("cancelled".to_string());
                        }
                    }
                    turn.status = "cancelled".to_string();
                    if turn.completed_at.is_none() {
                        turn.completed_at = Some(now);
                    }
                }
                let current_turn_id = turn.turn_id.clone();
                turn.normalize();
                if let Some(chain) = sidecar.active_execution_chain.as_mut() {
                    chain.current_turn = sidecar.current_turn.clone();
                    chain.normalize();
                }
                sidecar.updated_at = UtcMillis::now();
                let continuation_turn_id = sidecar
                    .active_execution_chain
                    .as_ref()
                    .map(|chain| chain.root_task_id.to_string())
                    .unwrap_or(current_turn_id);
                Some((continuation_turn_id, sidecar.updated_at, sidecar.clone()))
            };
            if let Some((continuation_turn_id, updated_at, updated)) = updated.as_ref()
                && let Some(turn) = updated.current_turn.as_ref()
            {
                upsert_canonical_turn_in_state(&mut state, session_id, turn)?;
                if interrupted_by_user {
                    let goal_index = state.goals.iter().position(|goal| {
                        &goal.session_id == session_id
                            && goal.status == GoalStatus::Active
                            && goal_owns_execution_owner(&state, goal, continuation_turn_id)
                    });
                    if let Some(goal_index) = goal_index {
                        super::goals::pause_goal_and_bound_plan_in_state(
                            &mut state,
                            goal_index,
                            *updated_at,
                        );
                    }
                } else {
                    release_running_goal_continuation(
                        &mut state,
                        session_id,
                        continuation_turn_id,
                        GOAL_CONTINUATION_TURN_TERMINAL,
                        *updated_at,
                    );
                }
                reconcile_goal_time_used(&mut state);
            }
            updated.map(|(_, _, sidecar)| sidecar)
        };
        if updated.is_some() {
            self.mark_sidecar_dirty(SessionSidecarFlushReason::UpdateCurrentTurnStatus);
        }
        Ok(updated)
    }

    pub fn clear_execution_ownership(&self, session_id: &SessionId) -> DomainResult<()> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let recovery_id = existing.recovery_id.clone();
        let current_turn = existing.current_turn.clone();
        let active_execution_chain = existing.active_execution_chain.clone();
        let ownership = active_execution_chain
            .as_ref()
            .map(Self::ownership_from_active_execution_chain)
            .unwrap_or_default();
        let status =
            Self::derive_sidecar_status(&ownership, recovery_id.as_deref(), Some(&existing.status));
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership,
                recovery_id,
                current_turn,
                active_execution_chain,
                status,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::ClearExecutionOwnership,
        );
        Ok(())
    }

    pub fn archive_active_execution_chain(
        &self,
        session_id: &SessionId,
        root_task_id: &TaskId,
    ) -> DomainResult<()> {
        let existing = self
            .runtime_sidecar(session_id)
            .ok_or(DomainError::NotFound {
                entity: "session_runtime_sidecar",
            })?;
        let chain =
            existing
                .active_execution_chain
                .as_ref()
                .ok_or_else(|| DomainError::InvalidState {
                    message: "当前会话没有活跃执行链".to_string(),
                })?;
        if &chain.root_task_id != root_task_id {
            return Err(DomainError::InvalidState {
                message: format!(
                    "归档任务与当前执行链不一致: {} != {}",
                    root_task_id, chain.root_task_id
                ),
            });
        }
        let ownership = ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: chain
                .workspace_id
                .clone()
                .or(existing.ownership.workspace_id),
            ..ExecutionOwnership::default()
        };
        self.upsert_runtime_sidecar_with_reason(
            SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership,
                recovery_id: None,
                current_turn: existing.current_turn,
                active_execution_chain: None,
                status: SessionExecutionSidecarStatus::Detached,
                updated_at: UtcMillis::now(),
            },
            SessionSidecarFlushReason::ArchiveActiveExecutionChain,
        );
        Ok(())
    }

    pub fn flush_execution_sidecars_with<E, F>(&self, persist: F) -> Result<bool, E>
    where
        F: FnOnce(&SessionExecutionSidecarStoreState) -> Result<(), E>,
    {
        let _flush_guard = self
            .sidecar_flush_lock
            .lock()
            .expect("session sidecar flush lock poisoned");
        let version = {
            let flush_state = self
                .sidecar_flush_state
                .read()
                .expect("session sidecar flush state read lock poisoned");
            if flush_state.current_version == flush_state.flushed_version {
                return Ok(false);
            }
            flush_state.current_version
        };
        let snapshot = self.execution_sidecar_store_state();
        persist(&snapshot)?;
        let mut flush_state = self
            .sidecar_flush_state
            .write()
            .expect("session sidecar flush state write lock poisoned");
        flush_state.flushed_version = flush_state.flushed_version.max(version);
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
