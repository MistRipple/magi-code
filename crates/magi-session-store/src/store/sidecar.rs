use super::{ORCHESTRATOR_ROLE_ID, SessionStore, TimelineEntryInput};
use crate::models::{
    ActiveExecutionChain, ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalToolCall,
    CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnItemStatus,
    CanonicalTurnStatus, CanonicalTurnVisibility, CanonicalWorkerRef, ExecutionThread,
    ExecutionThreadStatus, GoalContinuationPhase, GoalContinuationState, GoalStatus,
    InterruptedGoalResumeCheckpoint, SessionAcceptanceRecord, SessionDurableState,
    SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState, SessionPlan,
    SessionRuntimeSidecar, SessionRuntimeSidecarState, SessionSidecarFlushReason,
    SessionStoreState, ThreadChatMessage, ThreadChatToolCall, ThreadChatToolFunction,
    ThreadContextCheckpoint, ThreadContextSnapshot, ThreadModelProviderContext, ThreadVisibility,
    TimelineEntry,
};
use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, GoalId, MissionId, PlanState,
    RecoveryResumeInput, SessionId, Task, TaskExecutionTarget, TaskId, ThreadId, UtcMillis,
    WorkerId, WorkspaceId,
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
/// 由目标状态在恢复时重新推导的 Turn 标注，不属于事件事实。
///
/// 事件日志按写入当时的目标状态记录它们；目标之后结束或阻塞，
/// `reconcile_goal_response_duration_scopes` 会按最新目标状态重新推导，缓存 projection
/// 因此与冻结的事件日志必然不同。事件与缓存的一致性校验必须忽略这些 key。
pub const GOAL_DERIVED_TURN_METADATA_KEYS: [&str; 2] = [
    TURN_GOAL_ID_METADATA_KEY,
    TURN_RESPONSE_DURATION_SCOPE_METADATA_KEY,
];
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
        "preparing"
            | "pending"
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

pub(super) fn terminal_item_status_for_turn_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "succeeded" | "success" => Some("completed"),
        "blocked" => Some("blocked"),
        "failed" | "error" => Some("failed"),
        "interrupted" => Some("cancelled"),
        "cancelled" | "canceled" | "killed" => Some("cancelled"),
        _ => None,
    }
}

fn normalize_terminal_current_turn_item_metadata(
    item: &mut ActiveExecutionTurnItem,
    terminal_status: &str,
) {
    if item.metadata.get("noticeKind").and_then(Value::as_str) != Some("context_compaction")
        || item.metadata.get("compactionState").and_then(Value::as_str) != Some("running")
    {
        return;
    }
    let Some(compaction_state) = (match terminal_status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "succeeded" | "success" => Some("completed"),
        "failed" | "error" => Some("failed"),
        "blocked" => Some("blocked"),
        "cancelled" | "canceled" | "killed" | "interrupted" => Some("cancelled"),
        _ => None,
    }) else {
        return;
    };
    item.metadata.insert(
        "compactionState".to_string(),
        Value::String(compaction_state.to_string()),
    );
}

pub(super) fn settle_active_current_turn_items(
    items: &mut [ActiveExecutionTurnItem],
    terminal_status: &str,
) {
    // 终态结算前先固定依据：条目状态随后会被改写为终态。只有正常完成才用依据确认步骤完成。
    let proof = if terminal_item_status_for_turn_status(terminal_status) == Some("completed") {
        crate::execution_steps::collect_step_proof(
            items,
            |status| terminal_item_status_for_turn_status(status) == Some("completed"),
            current_turn_item_status_is_active,
        )
    } else {
        Vec::new()
    };
    let terminal_for_steps =
        terminal_item_status_for_turn_status(terminal_status).unwrap_or(terminal_status);
    for item in items {
        crate::execution_steps::settle_steps(item, terminal_for_steps, &proof);
        if current_turn_item_status_is_active(&item.status) {
            item.status = terminal_status.to_string();
        }
        if item
            .tool_status
            .as_deref()
            .is_some_and(current_turn_item_status_is_active)
        {
            item.tool_status = Some(terminal_status.to_string());
        }
        normalize_terminal_current_turn_item_metadata(item, terminal_status);
    }
}

fn canonical_current_turn_status(status: &str) -> DomainResult<CanonicalTurnStatus> {
    match status.trim().to_ascii_lowercase().as_str() {
        "pending" | "queued" | "accepted" => Ok(CanonicalTurnStatus::Pending),
        "preparing" | "running" | "started" | "streaming" | "awaiting_approval"
        | "review_required" | "repairing" | "verifying" => Ok(CanonicalTurnStatus::Running),
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
    if status.trim().eq_ignore_ascii_case("indeterminate") {
        return Ok(CanonicalTurnItemStatus::Indeterminate);
    }
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

pub(super) fn canonical_turn_status_name(status: CanonicalTurnStatus) -> &'static str {
    match status {
        CanonicalTurnStatus::Pending => "pending",
        CanonicalTurnStatus::Running => "running",
        CanonicalTurnStatus::Completed => "completed",
        CanonicalTurnStatus::Blocked => "blocked",
        CanonicalTurnStatus::Failed => "failed",
        CanonicalTurnStatus::Interrupted => "interrupted",
        CanonicalTurnStatus::Cancelled => "cancelled",
        CanonicalTurnStatus::Superseded => "superseded",
    }
}

fn canonical_turn_item_status_name(status: CanonicalTurnItemStatus) -> &'static str {
    match status {
        CanonicalTurnItemStatus::Pending => "pending",
        CanonicalTurnItemStatus::Running => "running",
        CanonicalTurnItemStatus::Completed => "completed",
        CanonicalTurnItemStatus::Blocked => "blocked",
        CanonicalTurnItemStatus::Failed => "failed",
        CanonicalTurnItemStatus::Cancelled => "cancelled",
        CanonicalTurnItemStatus::Indeterminate => "indeterminate",
    }
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

/// canonical 是线程历史的唯一来源；恢复分支只沿已接纳请求中的来源引用读取。
fn project_thread_message_history(
    state: &SessionStoreState,
    thread: &ExecutionThread,
    before_turn_seq: Option<u64>,
) -> DomainResult<Vec<ThreadChatMessage>> {
    fn value_text(value: &Value) -> String {
        match value {
            Value::String(text) => text.clone(),
            _ => value.to_string(),
        }
    }
    let thread_id = &thread.thread_id;
    let mut projected = Vec::new();
    for turn in state.canonical_turns.iter().filter(|turn| {
        turn.session_id == thread.session_id
            && before_turn_seq.is_none_or(|limit| turn.turn_seq <= limit)
            && turn.status != CanonicalTurnStatus::Superseded
            && !turn.is_session_command()
    }) {
        for item in &turn.items {
            let Some(source) = item
                .metadata
                .get("pendingDispatch")
                .and_then(|request| request.get("recovery_checkpoint"))
                .filter(|value| !value.is_null())
                .filter(|_| {
                    item.worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        .is_some_and(|task| thread.handled_task_ids.contains(task))
                })
            else {
                continue;
            };
            let checkpoint: magi_core::TaskRecoveryCheckpoint =
                serde_json::from_value(source.clone()).map_err(|error| {
                    DomainError::InvalidState {
                        message: format!("线程历史恢复来源无效: {error}"),
                    }
                })?;
            let source_turn = state
                .canonical_turns
                .iter()
                .find(|source| {
                    source.session_id == thread.session_id
                        && source.session_id == checkpoint.source_session_id
                        && source.turn_id == checkpoint.source_turn_id
                        && source.turn_seq < turn.turn_seq
                        && source.status == CanonicalTurnStatus::Cancelled
                        && source.items.iter().any(|item| {
                            item.worker
                                .as_ref()
                                .and_then(|worker| worker.task_id.as_ref())
                                == Some(&checkpoint.source_task_id)
                        })
                })
                .ok_or_else(|| DomainError::InvalidState {
                    message: "线程历史恢复来源缺少先前已取消的 canonical Turn".into(),
                })?;
            let source_thread = state
                .thread_registry
                .iter()
                .find(|source| {
                    source.session_id == thread.session_id
                        && source.thread_id == checkpoint.source_thread_id
                        && source.thread_id != thread.thread_id
                        && source.handled_task_ids.contains(&checkpoint.source_task_id)
                })
                .ok_or_else(|| DomainError::InvalidState {
                    message: "线程历史恢复来源与任务或线程不匹配".into(),
                })?;
            projected.extend(project_thread_message_history(
                state,
                source_thread,
                Some(source_turn.turn_seq),
            )?);
        }
        // 一个模型 Step 的输出（reasoning + 一批 function_call）作为一条 assistant 消息投影，
        // 与实时循环写入 thread 的形态一致；否则回合边界重建出的历史与上一轮实际发送的
        // 前缀字节不同，提示词缓存在整段历史处失效。调用列表取模型输出的完整列表，
        // 不随 ToolCall 条目逐个出现而增长，这样每次重建命中同一个历史槽位。
        let recorded_call_ids = turn
            .items
            .iter()
            .filter(|item| item.source_thread_id == *thread_id)
            .filter_map(|item| item.tool.as_ref().map(|tool| tool.call_id.clone()))
            .collect::<std::collections::HashSet<_>>();
        let mut batched_calls = Vec::<(String, String)>::new();
        let mut batched_call_ids = std::collections::HashSet::<String>::new();
        let start = projected.len();
        for item in turn.items.iter().filter(|item| {
            item.source_thread_id == *thread_id
                // 恢复主线任务可能使用独立执行线程；其最终答复仍属于主线。
                || (thread.role_id == ORCHESTRATOR_ROLE_ID
                    && item.kind == CanonicalTurnItemKind::AssistantText
                    && item.visibility.renderable
                    && item.metadata.get("assistantOutputKind").and_then(Value::as_str) == Some("final")
                    && item.worker.as_ref().is_some_and(|worker| {
                        worker.task_id.is_some() && worker.worker_id.is_none() && worker.role_id.is_none()
                    }))
                || (thread.role_id != ORCHESTRATOR_ROLE_ID
                    && item.kind == CanonicalTurnItemKind::UserMessage
                    && item.metadata.get("route").and_then(Value::as_str) == Some("continue")
                    && item.metadata.get("continuedThreadIds").and_then(Value::as_array)
                        .is_some_and(|threads| threads.iter().any(|id| id.as_str() == Some(thread_id.as_str()))))
        }) {
            match item.kind {
                CanonicalTurnItemKind::UserMessage => {
                    if item
                        .content
                        .as_deref()
                        .is_some_and(|content| !content.trim().is_empty())
                        || item
                            .metadata
                            .get("images")
                            .and_then(Value::as_array)
                            .is_some_and(|images| !images.is_empty())
                    {
                        projected.push(ThreadChatMessage {
                            input_request_id: item
                                .metadata
                                .get("requestId")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            role: "user".to_string(),
                            content: item.model_input_content(),
                            images: Vec::new(),
                            tool_calls: Vec::new(),
                            tool_call_id: None,
                            provider_context: Vec::new(),
                        });
                    }
                }
                CanonicalTurnItemKind::AssistantText => {
                    let batch_tool_calls = item
                        .metadata
                        .get("toolCalls")
                        .and_then(|value| {
                            serde_json::from_value::<Vec<ThreadChatToolCall>>(value.clone()).ok()
                        })
                        .unwrap_or_default();
                    if item
                        .content
                        .as_deref()
                        .is_some_and(|content| !content.trim().is_empty())
                        || !batch_tool_calls.is_empty()
                    {
                        let provider_context = item
                            .metadata
                            .get("providerContext")
                            .and_then(|value| {
                                serde_json::from_value::<Vec<ThreadModelProviderContext>>(
                                    value.clone(),
                                )
                                .ok()
                            })
                            .unwrap_or_default();
                        let tool_calls = batch_tool_calls;
                        for call in &tool_calls {
                            if batched_call_ids.insert(call.id.clone()) {
                                batched_calls.push((call.id.clone(), call.function.name.clone()));
                            }
                        }
                        projected.push(ThreadChatMessage {
                            input_request_id: None,
                            role: "assistant".to_string(),
                            content: item.content.clone(),
                            images: Vec::new(),
                            tool_calls,
                            tool_call_id: None,
                            provider_context,
                        });
                    }
                }
                CanonicalTurnItemKind::ToolCall => {
                    let Some(tool) = item.tool.as_ref() else {
                        continue;
                    };
                    if !batched_call_ids.contains(&tool.call_id) {
                        projected.push(ThreadChatMessage {
                            input_request_id: None,
                            role: "assistant".to_string(),
                            content: None,
                            images: Vec::new(),
                            tool_calls: vec![ThreadChatToolCall {
                                id: tool.call_id.clone(),
                                kind: "function".to_string(),
                                function: ThreadChatToolFunction {
                                    name: tool.name.clone(),
                                    arguments: tool
                                        .arguments
                                        .as_ref()
                                        .map(value_text)
                                        .unwrap_or_else(|| "{}".to_string()),
                                },
                            }],
                            tool_call_id: None,
                            provider_context: Vec::new(),
                        });
                    }
                    // 等待授权的调用尚未执行（批准后会先改写为 running）；它记录的进度
                    // 不是工具结果，投影为“中断、未执行”，恢复后可以重新申请授权。
                    let question_result = item
                        .user_question()?
                        .and_then(|question| question.result_payload());
                    let awaiting_approval = tool.result.as_ref().is_some_and(|result| {
                        matches!(
                            result.get("status").and_then(Value::as_str),
                            Some("awaiting_approval" | "awaiting_user_input")
                        )
                    });
                    let result = question_result.as_ref().or(tool.result.as_ref())
                        .filter(|_| question_result.is_some() || !awaiting_approval)
                        .map(value_text)
                        .or_else(|| {
                            tool.error.as_ref().map(|error| {
                                serde_json::json!({
                                    "tool": tool.name,
                                    "status": "failed",
                                    "error": error,
                                })
                                .to_string()
                            })
                        })
                        .unwrap_or_else(|| {
                            if awaiting_approval {
                                return serde_json::json!({
                                    "tool": tool.name,
                                    "status": "interrupted",
                                    "execution": "not_started",
                                    "reason": "task_interrupted_before_tool_execution_started",
                                    "message": "本次工具调用在等待授权时中断，尚未执行；如仍有必要，可以重新调用并申请授权。",
                                })
                                .to_string();
                            }
                            let interrupted = matches!(
                                turn.status,
                                CanonicalTurnStatus::Cancelled
                                    | CanonicalTurnStatus::Interrupted
                                    | CanonicalTurnStatus::Superseded
                            ) || matches!(
                                item.status,
                                CanonicalTurnItemStatus::Cancelled
                                    | CanonicalTurnItemStatus::Failed
                            );
                            serde_json::json!({
                                "tool": tool.name,
                                "status": if interrupted { "interrupted" } else { "pending" },
                                "execution": if interrupted { "unknown" } else { "not_started" },
                                "reason": if interrupted {
                                    "task_interrupted_before_tool_result_persisted"
                                } else {
                                    "canonical_projection_missing_tool_result"
                                },
                            })
                            .to_string()
                        });
                    projected.push(ThreadChatMessage {
                        input_request_id: None,
                        role: "tool".to_string(),
                        content: Some(result),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(tool.call_id.clone()),
                        provider_context: Vec::new(),
                    });
                }
                CanonicalTurnItemKind::AssistantThinking
                | CanonicalTurnItemKind::TaskStatus
                | CanonicalTurnItemKind::SystemNotice => {}
            }
        }
        // 回合已经结束，但模型输出的某些调用从未开始执行：补“未执行”结果，避免历史里留下
        // 没有结果的悬空调用。回合仍在运行时由实时循环写入结果，这里不预先占位。
        if turn.status.is_terminal() {
            for (call_id, tool_name) in &batched_calls {
                if recorded_call_ids.contains(call_id) {
                    continue;
                }
                projected.push(ThreadChatMessage {
                    input_request_id: None,
                    role: "tool".to_string(),
                    content: Some(
                        serde_json::json!({
                            "tool": tool_name,
                            "status": "interrupted",
                            "execution": "not_started",
                            "reason": "task_interrupted_before_tool_execution_started",
                        })
                        .to_string(),
                    ),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call_id.clone()),
                    provider_context: Vec::new(),
                });
            }
        }
        if thread.role_id == ORCHESTRATOR_ROLE_ID
            && turn.status == CanonicalTurnStatus::Cancelled
            && projected[start..]
                .iter()
                .any(|message| message.role == "user")
        {
            projected.push(ThreadChatMessage {
                input_request_id: None,
                role: "assistant".into(),
                content: Some("（上面这一轮回复已被用户中断，可能不完整。）".into()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            });
        }
    }
    Ok(projected)
}

fn thread_context_snapshot_is_current(
    state: &SessionStoreState,
    snapshot: &ThreadContextSnapshot,
) -> bool {
    let Some(thread) = state.thread_registry.iter().find(|thread| {
        thread.thread_id == snapshot.thread_id && thread.session_id == snapshot.session_id
    }) else {
        return false;
    };
    thread.observed_context_window_tokens == snapshot.context_window_tokens
        && state
            .thread_context_checkpoints
            .iter()
            .find(|checkpoint| checkpoint.thread_id == snapshot.thread_id)
            == snapshot.checkpoint.as_ref()
        && project_thread_message_history(state, thread, None)
            .is_ok_and(|history| history == snapshot.messages)
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
    if let Some(value) = item
        .timeline_entry_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        metadata.insert("timelineEntryId".to_string(), Value::String(value.clone()));
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
    let has_images = item
        .metadata
        .get("images")
        .and_then(Value::as_array)
        .is_some_and(|images| !images.is_empty());
    if kind == CanonicalTurnItemKind::AssistantText {
        return has_content || !status.is_terminal();
    }
    // 命令身份是用户消息的可见内容，即使没有任何命令参数。
    let has_command = kind == CanonicalTurnItemKind::UserMessage
        && (item
            .metadata
            .get(crate::SESSION_COMMAND_METADATA_KEY)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
            || item.metadata.get("goalMode").and_then(Value::as_bool) == Some(true)
            || item
                .metadata
                .get("skillName")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty()));
    has_content
        || has_images
        || has_command
        || item.tool_call_id.is_some()
        || item.worker_id.is_some()
        || item.task_id.is_some()
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
        // 新 item 从版本 1 开始。版本号属于 canonical 事实，不应等到第一次更新
        // 后才出现，否则流式事件的版本 1 会与首次完整事实更新发生冲突。
        item_version: Some(1),
        // accepted_at 来自全局单调序列，在并行请求下可能领先系统时钟数毫秒；
        // canonical item 的更新时间必须始终不早于创建时间，否则 daemon 事件日志
        // 会把合法的 accepted 事实判定为时间倒流。
        updated_at: UtcMillis(UtcMillis::now().0.max(turn.accepted_at.0)),
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
    let mut metadata = turn
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
    if let Some(request_id) = crate::models::active_execution_turn_request_id(turn) {
        metadata.insert("requestId".to_string(), Value::String(request_id));
    }
    // Turn 级协议元数据由首个 user item 携带，canonical Turn 也保留同一份
    // 轻量身份，便于重启后按 requestId/fingerprint 恢复而无需重新解析请求。
    if let Some(user_item) = turn.items.iter().find(|item| item.kind == "user_message") {
        for key in [
            "executionProfile",
            "requestFingerprint",
            "traceId",
            "attemptId",
            "userMessageId",
            "placeholderMessageId",
            "route",
        ] {
            if let Some(value) = user_item.metadata.get(key) {
                metadata.insert(key.to_string(), value.clone());
            }
        }
    }
    let mut canonical_turn = CanonicalTurn {
        model_steps: Vec::new(),
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

pub(super) fn prepare_canonical_turn_in_state(
    state: &SessionStoreState,
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
) -> DomainResult<CanonicalTurn> {
    let mut incoming = current_turn_to_canonical_turn(session_id, turn)?;
    apply_goal_response_duration_scope(state, &mut incoming);
    incoming.normalize();
    let mut item_ids = HashSet::new();
    let mut item_seqs = HashSet::new();
    for item in &incoming.items {
        if !item_ids.insert(item.item_id.as_str()) || !item_seqs.insert(item.item_seq) {
            return Err(DomainError::InvalidState {
                message: format!(
                    "canonical turn {} 包含重复 item id 或 itemSeq",
                    incoming.turn_id
                ),
            });
        }
    }
    if let Some(existing) = state
        .canonical_turns
        .iter()
        .find(|existing| existing.session_id == *session_id && existing.turn_id == incoming.turn_id)
    {
        incoming.model_steps = existing.model_steps.clone();
        if let Some(binding) = existing
            .metadata
            .get(crate::agent_turn::AGENT_EXECUTION_METADATA_KEY)
        {
            incoming.metadata.insert(
                crate::agent_turn::AGENT_EXECUTION_METADATA_KEY.into(),
                binding.clone(),
            );
            if let Some(skill) = existing
                .metadata
                .get(crate::agent_turn::AGENT_ACTIVE_SKILL_METADATA_KEY)
            {
                incoming.metadata.insert(
                    crate::agent_turn::AGENT_ACTIVE_SKILL_METADATA_KEY.into(),
                    skill.clone(),
                );
            }
            if let Some(goal) = existing.metadata.get("goalId") {
                incoming.metadata.insert("goalId".into(), goal.clone());
            }
        }
        incoming.validate_update_from(existing)?;
        if let Some(removed) = existing.items.iter().find(|existing_item| {
            !incoming
                .items
                .iter()
                .any(|incoming_item| incoming_item.item_id == existing_item.item_id)
        }) {
            return Err(DomainError::InvalidState {
                message: format!(
                    "canonical turn {} 不能删除 item {}",
                    incoming.turn_id, removed.item_id
                ),
            });
        }
        for incoming_item in &mut incoming.items {
            if let Some(existing_item) = existing
                .items
                .iter()
                .find(|existing_item| existing_item.item_id == incoming_item.item_id)
            {
                for key in [
                    magi_tool_runtime::BACKGROUND_JOB_METADATA_KEY,
                    crate::user_question::USER_QUESTION_METADATA_KEY,
                    crate::tool_approval::TOOL_APPROVAL_METADATA_KEY,
                    "approvalSource",
                ] {
                    if let Some(value) = existing_item.metadata.get(key) {
                        incoming_item.metadata.insert(key.into(), value.clone());
                    }
                }
                if let Some(result) = incoming_item
                    .tool_approval()?
                    .and_then(|approval| approval.denied_result())
                {
                    incoming_item.tool.as_mut().expect("approval tool").result = Some(result);
                }
                if let Some(approval) = incoming_item.tool_approval()?
                    && approval.execution.is_some()
                    && incoming_item
                        .tool
                        .as_ref()
                        .and_then(|tool| tool.result.as_ref())
                        .and_then(|result| result.get("status"))
                        .and_then(Value::as_str)
                        == Some("awaiting_approval")
                {
                    incoming_item.tool.as_mut().expect("approval tool").result = existing_item
                        .tool
                        .as_ref()
                        .and_then(|tool| tool.result.clone());
                }
                if let Some(result) = incoming_item
                    .user_question()?
                    .and_then(|question| question.result_payload())
                {
                    incoming_item.tool.as_mut().expect("question tool").result = Some(result);
                }
                incoming_item.validate_update_from(existing_item)?;
                let generated_updated_at = incoming_item.updated_at;
                incoming_item.updated_at = existing_item.updated_at;
                incoming_item.item_version = existing_item.item_version;
                if incoming_item != existing_item {
                    incoming_item.updated_at = UtcMillis(
                        generated_updated_at
                            .0
                            .max(existing_item.updated_at.0.saturating_add(1)),
                    );
                    incoming_item.item_version =
                        Some(existing_item.item_version.unwrap_or(0).saturating_add(1));
                }
            }
        }
    }
    Ok(incoming)
}

pub(super) fn apply_canonical_turn_in_state(
    state: &mut SessionStoreState,
    incoming: CanonicalTurn,
) {
    // 流式 item 写回不触碰用量投影；只有 Step 变化才应用该 Turn 的水位。
    let previous = state
        .canonical_turns
        .iter()
        .find(|old| old.session_id == incoming.session_id && old.turn_id == incoming.turn_id);
    let usage_changed = previous.is_none_or(|old| old.model_steps != incoming.model_steps);
    if usage_changed {
        let previous = previous.cloned();
        super::goals::apply_goal_attempt_change(&mut state.goals, previous.as_ref(), &incoming);
        super::goals::apply_goal_token_watermarks(
            &mut state.goals,
            &incoming.session_id,
            &incoming.model_steps,
        );
    }
    if let Some(existing) = state.canonical_turns.iter_mut().find(|existing| {
        existing.session_id == incoming.session_id && existing.turn_id == incoming.turn_id
    }) {
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
}

pub(super) struct CanonicalCommitPlan<T> {
    pub(super) mutations: Vec<super::CanonicalTurnMutation>,
    pub(super) value: T,
    pub(super) acceptance: Option<(SessionAcceptanceRecord, Option<Task>)>,
}

impl SessionStore {
    /// 在不占用 session state 写锁的情况下完成一笔 canonical 事务。
    ///
    /// prepare 只读取并校验内存状态，事件写入完成后才重新取得 state 写锁提交
    /// canonical projection。canonical session commit lock 保证 prepare 到 apply 期间不会有
    /// 另一笔 canonical 事务改变同一份事件 projection；普通 session 读取可以在 fsync
    /// 期间继续进行。
    pub(super) fn commit_canonical_transaction<T, R>(
        &self,
        session_id: &SessionId,
        prepare: impl FnOnce(&SessionStoreState) -> DomainResult<CanonicalCommitPlan<T>>,
        apply: impl FnOnce(&mut SessionStoreState, T) -> R,
    ) -> DomainResult<R> {
        let _canonical_barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical commit barrier poisoned");
        let canonical_session_lock = self.canonical_commit_lock_for(session_id);
        let _canonical_guard = canonical_session_lock
            .lock()
            .expect("canonical session commit lock poisoned");
        let plan = {
            let state = self.state.read().expect("session state read lock poisoned");
            prepare(&state)?
        };

        if plan.acceptance.is_some() && plan.mutations.is_empty() {
            return Err(DomainError::InvalidState {
                message: "accepted canonical transaction 不能没有 canonical mutation".to_string(),
            });
        }
        let inputs = {
            let state = self.state.read().expect("session state read lock poisoned");
            super::inputs::attached_inputs(&state.inputs, &plan.mutations)?
        };
        if !plan.mutations.is_empty() || !inputs.is_empty() {
            self.persist_session_transaction(
                session_id,
                &plan.mutations,
                &inputs,
                plan.acceptance
                    .as_ref()
                    .map(|(acceptance, task)| (acceptance, task.as_ref())),
            )?;
        }

        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        for mutation in &plan.mutations {
            let current = state
                .canonical_turns
                .iter()
                .find(|turn| {
                    turn.session_id == mutation.next.session_id
                        && turn.turn_id == mutation.next.turn_id
                })
                .cloned();
            if current != mutation.previous {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "canonical mutation 提交期间状态发生并发变化: {}",
                        mutation.next.turn_id
                    ),
                });
            }
        }
        for mutation in &plan.mutations {
            apply_canonical_turn_in_state(&mut state, mutation.next.clone());
        }
        let inputs_changed = !inputs.is_empty();
        for mutation in inputs {
            super::inputs::apply_input(&mut state.inputs, mutation.next);
        }
        let result = apply(&mut state, plan.value);
        drop(state);
        if inputs_changed {
            self.notify_input_commit(session_id);
        }
        Ok(result)
    }

    fn canonical_turn_commit_plan(
        state: &SessionStoreState,
        session_id: &SessionId,
        turn: &ActiveExecutionTurn,
        acceptance: Option<(SessionAcceptanceRecord, Option<Task>)>,
    ) -> DomainResult<CanonicalCommitPlan<CanonicalTurn>> {
        let incoming = prepare_canonical_turn_in_state(state, session_id, turn)?;
        let previous = state
            .canonical_turns
            .iter()
            .find(|existing| {
                existing.session_id == *session_id && existing.turn_id == incoming.turn_id
            })
            .cloned();
        let mutations = if previous.as_ref() == Some(&incoming) {
            Vec::new()
        } else {
            vec![super::CanonicalTurnMutation {
                previous,
                next: incoming.clone(),
            }]
        };
        Ok(CanonicalCommitPlan {
            mutations,
            value: incoming,
            acceptance,
        })
    }

    fn canonical_prepared_turn_commit_plan(
        state: &SessionStoreState,
        session_id: &SessionId,
        incoming: CanonicalTurn,
        acceptance: Option<(SessionAcceptanceRecord, Option<Task>)>,
    ) -> CanonicalCommitPlan<CanonicalTurn> {
        let previous = state
            .canonical_turns
            .iter()
            .find(|existing| {
                existing.session_id == *session_id && existing.turn_id == incoming.turn_id
            })
            .cloned();
        let mutations = if previous.as_ref() == Some(&incoming) {
            Vec::new()
        } else {
            vec![super::CanonicalTurnMutation {
                previous,
                next: incoming.clone(),
            }]
        };
        CanonicalCommitPlan {
            mutations,
            value: incoming,
            acceptance,
        }
    }

    fn canonical_replacement_commit_plan(
        state: &SessionStoreState,
        replaced_turn_index: usize,
        mut incoming: CanonicalTurn,
        superseded_at: UtcMillis,
        acceptance: Option<(SessionAcceptanceRecord, Option<Task>)>,
    ) -> DomainResult<CanonicalCommitPlan<CanonicalTurn>> {
        apply_goal_response_duration_scope(state, &mut incoming);
        incoming.normalize();
        let previous = state.canonical_turns[replaced_turn_index].clone();
        let mut superseded = previous.clone();
        supersede_canonical_turn(&mut superseded, &incoming.turn_id, superseded_at);
        superseded.validate_update_from(&previous)?;
        Ok(CanonicalCommitPlan {
            mutations: vec![
                super::CanonicalTurnMutation {
                    previous: Some(previous),
                    next: superseded.clone(),
                },
                super::CanonicalTurnMutation {
                    previous: None,
                    next: incoming,
                },
            ],
            value: superseded,
            acceptance,
        })
    }
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

pub(super) fn goal_owns_turn(
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
        GoalStatus::Active | GoalStatus::Paused => false,
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
                (GoalStatus::Paused, _) | (GoalStatus::Blocked, _) => None,
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

fn user_message_item(turn: &ActiveExecutionTurn) -> Option<&ActiveExecutionTurnItem> {
    turn.items.iter().find(|item| item.kind == "user_message")
}

fn validate_turn_replacement_target(
    state: &SessionStoreState,
    session_id: &SessionId,
    replace_turn_id: &str,
) -> DomainResult<usize> {
    let current_turn = state
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
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
    let interrupted_by_user = user_message_item(&current_turn)
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

impl SessionRuntimeSidecarState {
    pub fn project(
        &self,
        canonical_turns: &[CanonicalTurn],
    ) -> DomainResult<SessionRuntimeSidecar> {
        if let Some(chain) = &self.active_execution_chain
            && (chain.session_id != self.session_id
                || chain.current_turn_id != self.current_turn_id)
        {
            return Err(DomainError::InvalidState {
                message: format!("session {} 的执行链 Turn 归属不一致", self.session_id),
            });
        }
        let canonical_turn = self
            .current_turn_id
            .as_deref()
            .map(|turn_id| {
                canonical_turns
                    .iter()
                    .find(|turn| turn.session_id == self.session_id && turn.turn_id == turn_id)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: format!("sidecar turn {turn_id} 缺少对应 canonical turn"),
                    })
            })
            .transpose()?;
        // sidecar 的时间只描述执行元数据；活动时间还须包含独立提交的 item/Step。
        let updated_at = canonical_turn.map_or(self.updated_at, |turn| {
            std::iter::once(turn.accepted_at)
                .chain(turn.completed_at)
                .chain(turn.items.iter().map(|item| item.updated_at))
                .chain(turn.model_steps.iter().flat_map(|step| {
                    std::iter::once(step.started_at)
                        .chain(step.settled_at)
                        .chain(step.attempts.iter().flat_map(|attempt| {
                            std::iter::once(attempt.started_at).chain(attempt.settled_at)
                        }))
                }))
                .fold(self.updated_at, UtcMillis::max)
        });
        Ok(SessionRuntimeSidecar {
            session_id: self.session_id.clone(),
            ownership: self.ownership.clone(),
            recovery_id: self.recovery_id.clone(),
            current_turn: canonical_turn.map(active_turn_projection),
            active_execution_chain: self.active_execution_chain.clone(),
            status: self.status.clone(),
            updated_at,
        })
    }
}

impl SessionStoreState {
    pub(crate) fn project_sidecar(
        &self,
        sidecar: &SessionRuntimeSidecarState,
    ) -> SessionRuntimeSidecar {
        sidecar
            .project(&self.canonical_turns)
            .expect("validated sidecar canonical identity")
    }

    pub(crate) fn runtime_sidecar(&self, session: &SessionId) -> Option<SessionRuntimeSidecar> {
        self.execution_sidecar_store
            .runtime_sidecars
            .iter()
            .find(|sidecar| &sidecar.session_id == session)
            .map(|sidecar| self.project_sidecar(sidecar))
    }
}

pub(super) fn validate_sidecars_from_canonical(state: &SessionStoreState) -> DomainResult<()> {
    for sidecar in &state.execution_sidecar_store.runtime_sidecars {
        if !state
            .sessions
            .iter()
            .any(|session| session.session_id == sidecar.session_id)
        {
            return Err(DomainError::InvalidState {
                message: format!("sidecar 引用了不存在的 session {}", sidecar.session_id),
            });
        }
        sidecar.project(&state.canonical_turns)?;
    }
    Ok(())
}

fn canonical_value_to_active(value: Option<&Value>) -> Option<String> {
    value.map(|value| match value {
        Value::String(value) => value.clone(),
        value => value.to_string(),
    })
}

fn canonical_item_to_active(item: &CanonicalTurnItem) -> ActiveExecutionTurnItem {
    let kind = match item.kind {
        CanonicalTurnItemKind::UserMessage => "user_message",
        CanonicalTurnItemKind::AssistantText => match item
            .metadata
            .get("assistantOutputKind")
            .and_then(Value::as_str)
        {
            Some("final") => "assistant_final",
            Some("error") => "assistant_error",
            _ => "assistant_stream",
        },
        CanonicalTurnItemKind::AssistantThinking => "assistant_thinking",
        CanonicalTurnItemKind::ToolCall => {
            if item
                .tool
                .as_ref()
                .is_some_and(|tool| tool.result.is_some() || tool.error.is_some())
            {
                "tool_call_result"
            } else {
                "tool_call_started"
            }
        }
        CanonicalTurnItemKind::TaskStatus => "task_status",
        CanonicalTurnItemKind::SystemNotice => "assistant_phase",
    };
    let worker = item.worker.as_ref();
    let tool = item.tool.as_ref();
    ActiveExecutionTurnItem {
        item_id: item.item_id.clone(),
        item_seq: item.item_seq,
        kind: kind.to_string(),
        status: canonical_turn_item_status_name(item.status).to_string(),
        source: worker
            .and_then(|worker| worker.role_id.clone())
            .unwrap_or_else(|| ORCHESTRATOR_ROLE_ID.to_string()),
        title: item.title.clone(),
        content: item.content.clone(),
        task_id: worker.and_then(|worker| worker.task_id.clone()),
        worker_id: worker.and_then(|worker| worker.worker_id.clone()),
        role_id: worker.and_then(|worker| worker.role_id.clone()),
        tool_call_id: tool.map(|tool| tool.call_id.clone()),
        tool_name: tool.map(|tool| tool.name.clone()),
        tool_status: tool.map(|_| canonical_turn_item_status_name(item.status).to_string()),
        tool_arguments: tool.and_then(|tool| canonical_value_to_active(tool.arguments.as_ref())),
        tool_result: tool.and_then(|tool| canonical_value_to_active(tool.result.as_ref())),
        tool_error: tool.and_then(|tool| tool.error.clone()),
        request_id: item
            .metadata
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string),
        user_message_id: item
            .metadata
            .get("userMessageId")
            .and_then(Value::as_str)
            .map(str::to_string),
        placeholder_message_id: item
            .metadata
            .get("placeholderMessageId")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata: item.metadata.clone(),
        timeline_entry_id: item
            .metadata
            .get("timelineEntryId")
            .and_then(Value::as_str)
            .map(str::to_string),
        source_thread_id: item.source_thread_id.clone(),
    }
}

pub(super) fn upsert_item_in_turn(
    turn: &mut ActiveExecutionTurn,
    mut item: ActiveExecutionTurnItem,
) -> DomainResult<()> {
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
    Ok(())
}

pub(super) fn active_turn_projection(canonical: &CanonicalTurn) -> ActiveExecutionTurn {
    let mut turn = ActiveExecutionTurn {
        turn_id: canonical.turn_id.clone(),
        turn_seq: canonical.turn_seq,
        accepted_at: canonical.accepted_at,
        completed_at: canonical.completed_at,
        status: if canonical.status == CanonicalTurnStatus::Pending {
            "accepted"
        } else {
            canonical_turn_status_name(canonical.status)
        }
        .to_string(),
        user_message: canonical
            .items
            .iter()
            .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            .and_then(|item| item.content.clone()),
        items: canonical
            .items
            .iter()
            .map(canonical_item_to_active)
            .collect(),
    };
    turn.normalize();
    turn
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
        let projected = sidecar
            .project(&state.canonical_turns)
            .expect("validated canonical sidecar");
        let Some(turn) = projected.current_turn.as_ref() else {
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

/// 校验执行面写回仍然属于发起它的 Turn。
///
/// `current_turn` 是 session 级别的可变指针。旧 runner 在用户停止后仍可能有
/// 延迟到达的流式 delta、工具结果或终态回调；如果只按 session 查找，就会把这些
/// 事件写进后续新 Turn，甚至把已取消的 Turn 重新改成 completed。所有生产执行面
/// 写回都必须带 expected_turn_id，并在同一把 state 锁内完成这项校验。
fn validate_expected_current_turn(
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
    expected_turn_id: Option<&str>,
) -> DomainResult<()> {
    validate_expected_current_turn_owner(session_id, turn, expected_turn_id)?;
    if expected_turn_id.is_some_and(|_| current_turn_status_is_terminal(&turn.status)) {
        return Err(DomainError::CurrentTurnConflict {
            session_id: session_id.to_string(),
            active_turn_id: turn.turn_id.clone(),
        });
    }
    Ok(())
}

fn validate_expected_current_turn_owner(
    session_id: &SessionId,
    turn: &ActiveExecutionTurn,
    expected_turn_id: Option<&str>,
) -> DomainResult<()> {
    if expected_turn_id.is_some_and(|expected| turn.turn_id != expected) {
        return Err(DomainError::CurrentTurnConflict {
            session_id: session_id.to_string(),
            active_turn_id: turn.turn_id.clone(),
        });
    }
    Ok(())
}

fn reject_duplicate_timeline_entry(timeline: &[TimelineEntry], entry_id: &str) -> DomainResult<()> {
    if timeline.iter().any(|entry| entry.entry_id == entry_id) {
        return Err(DomainError::InvalidState {
            message: format!("timeline entry {} already exists", entry_id),
        });
    }
    Ok(())
}

fn upsert_runtime_sidecar_in_state(
    state: &mut SessionStoreState,
    sidecar: impl Into<SessionRuntimeSidecarState>,
) {
    let sidecar = sidecar.into();
    if let Some(workspace_id) = sidecar.ownership.workspace_id.as_ref()
        && let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.session_id == sidecar.session_id)
        && session.workspace_id.as_deref() != Some(workspace_id.as_str())
    {
        session.workspace_id = Some(workspace_id.to_string());
        session.updated_at = UtcMillis::now();
    }
    state
        .execution_sidecar_store
        .upsert_runtime_sidecar(sidecar);
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

fn validate_execution_chain_in_state<'a>(
    state: &'a SessionStoreState,
    session_id: &SessionId,
    expected: &ActiveExecutionChain,
) -> DomainResult<&'a SessionRuntimeSidecarState> {
    let sidecar = state
        .execution_sidecar_store
        .runtime_sidecars
        .iter()
        .find(|sidecar| &sidecar.session_id == session_id)
        .ok_or(DomainError::NotFound {
            entity: "session_runtime_sidecar",
        })?;
    let chain =
        sidecar
            .active_execution_chain
            .as_ref()
            .ok_or_else(|| DomainError::InvalidState {
                message: "当前会话没有执行链".into(),
            })?;
    if expected.session_id != *session_id
        || chain.execution_chain_ref != expected.execution_chain_ref
        || chain.mission_id != expected.mission_id
        || chain.root_task_id != expected.root_task_id
        || chain.current_turn_id != expected.current_turn_id
    {
        return Err(DomainError::InvalidState {
            message: "执行链或 Turn 归属已变化".into(),
        });
    }
    Ok(sidecar)
}

mod turn_acceptance;
mod threads;
mod recovery;
mod turn_status;

impl SessionStore {
}
