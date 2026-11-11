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
        if let Some(chain) = &self.active_execution_chain {
            if chain.session_id != self.session_id || chain.current_turn_id != self.current_turn_id
            {
                return Err(DomainError::InvalidState {
                    message: format!("session {} 的执行链 Turn 归属不一致", self.session_id),
                });
            }
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

impl SessionStore {
    fn ownership_from_active_execution_chain(chain: &ActiveExecutionChain) -> ExecutionOwnership {
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

    /// 注册新 thread；重复的 `thread_id` 表示恢复/装配状态冲突，必须显式失败。
    pub fn register_thread(&self, thread: ExecutionThread) -> DomainResult<()> {
        let session_id = thread.session_id.clone();
        {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            if state
                .thread_registry
                .iter()
                .any(|existing| existing.thread_id == thread.thread_id)
            {
                return Err(DomainError::InvalidState {
                    message: format!("thread {} 已注册，拒绝重复注册", thread.thread_id),
                });
            }
            state.thread_registry.push(thread);
        }
        self.mark_sidecar_dirty_for_session(&session_id, SessionSidecarFlushReason::RegisterThread);
        Ok(())
    }

    /// 只删除本次恢复创建的 worker thread。
    ///
    /// 返回 `None` 表示该 thread 已经被同一回滚流程删除；如果 thread 仍存在但
    /// 任一归属字段不匹配，则返回错误，禁止把其他执行链的历史 thread 当成恢复产物删除。
    pub fn remove_recovery_thread_if_owned(
        &self,
        session_id: &SessionId,
        thread_id: &ThreadId,
        mission_id: &MissionId,
        task_id: &TaskId,
        worker_id: &WorkerId,
    ) -> DomainResult<Option<ExecutionThread>> {
        let removed = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(index) = state
                .thread_registry
                .iter()
                .position(|thread| &thread.thread_id == thread_id)
            else {
                return Ok(None);
            };
            let thread = &state.thread_registry[index];
            if thread.session_id != *session_id
                || thread.mission_id != *mission_id
                || thread.worker_instance_id != *worker_id
                || thread.role_id == ORCHESTRATOR_ROLE_ID
                || !thread
                    .handled_task_ids
                    .iter()
                    .any(|handled| handled == task_id)
            {
                return Err(DomainError::InvalidState {
                    message: format!("thread {} 不属于本次恢复创建的任务 {}", thread_id, task_id),
                });
            }
            state.thread_registry.remove(index)
        };
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RemoveThread);
        Ok(Some(removed))
    }

    /// 仅在 thread 仍保持指定快照时删除它，并同步删除其上下文检查点。
    ///
    /// materialize 失败时只能回收本次装配创建的 thread；快照校验把删除边界
    /// 固定在本次装配写入的状态，避免误删已经被 runner 接管的 thread。
    pub fn remove_thread_if_current(
        &self,
        session_id: &SessionId,
        thread_id: &ThreadId,
        expected: &ExecutionThread,
    ) -> DomainResult<Option<ExecutionThread>> {
        let removed = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(index) = state
                .thread_registry
                .iter()
                .position(|thread| &thread.thread_id == thread_id)
            else {
                return Ok(None);
            };
            let thread = &state.thread_registry[index];
            if &thread.session_id != session_id || thread != expected {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "thread {} 已被其他执行修改，拒绝回收 materialize thread",
                        thread_id
                    ),
                });
            }
            state
                .thread_context_checkpoints
                .retain(|checkpoint| &checkpoint.thread_id != thread_id);
            state.thread_registry.remove(index)
        };
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RemoveThread);
        Ok(Some(removed))
    }

    /// 回滚一次尚未启动 runner 的主线 thread 激活。
    ///
    /// 只有当前 thread 仍等于本次 materialize 写入后的完整快照时才允许恢复，
    /// 避免覆盖已经被后续执行使用的 thread 状态。
    pub fn restore_thread_after_materialization(
        &self,
        session_id: &SessionId,
        thread_id: &ThreadId,
        expected_current: &ExecutionThread,
        original: ExecutionThread,
    ) -> DomainResult<()> {
        {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(thread) = state
                .thread_registry
                .iter_mut()
                .find(|thread| &thread.thread_id == thread_id)
            else {
                return Err(DomainError::NotFound { entity: "thread" });
            };
            if thread.session_id != *session_id || thread != expected_current {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "thread {} 已被其他执行修改，拒绝回滚 materialize 状态",
                        thread_id
                    ),
                });
            }
            *thread = original;
        }
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RestoreThread);
        Ok(())
    }

    /// 激活 thread 并返回变更后的快照；thread 不存在时显式失败。
    pub fn activate_thread_checked(
        &self,
        thread_id: &ThreadId,
        task_id: &TaskId,
        now: UtcMillis,
    ) -> DomainResult<ExecutionThread> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let thread = state
                .thread_registry
                .iter_mut()
                .find(|thread| &thread.thread_id == thread_id)
                .ok_or(DomainError::NotFound { entity: "thread" })?;
            thread.status = ExecutionThreadStatus::Active;
            thread.last_used_at = now;
            if !thread.handled_task_ids.iter().any(|id| id == task_id) {
                thread.handled_task_ids.push(task_id.clone());
            }
            thread.clone()
        };
        self.mark_sidecar_dirty_for_session(
            &updated.session_id,
            SessionSidecarFlushReason::RegisterThread,
        );
        Ok(updated)
    }

    /// 将处理指定 task 且仍为 `Active` 的 thread 原子收口为 `Idle`。
    pub fn mark_task_threads_idle(&self, task_id: &TaskId, now: UtcMillis) -> usize {
        let (settled, session_ids) = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let mut settled = 0;
            let mut session_ids = Vec::new();
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
                session_ids.push(thread.session_id.clone());
            }
            session_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            session_ids.dedup();
            (settled, session_ids)
        };
        if settled > 0 {
            for session_id in session_ids {
                self.mark_sidecar_dirty_for_session(
                    &session_id,
                    SessionSidecarFlushReason::SettleThread,
                );
            }
        }
        settled
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
        let (mission_id, thread_id, _) =
            self.ensure_session_mission_with_created(session_id, now, mission_id_factory);
        (mission_id, thread_id)
    }

    /// 与 `ensure_session_mission` 相同，但返回本次调用是否创建了主线 thread，
    /// 供跨 store materialize 事务记录准确的回滚归属。
    pub fn ensure_session_mission_with_created(
        &self,
        session_id: &SessionId,
        now: UtcMillis,
        mission_id_factory: impl FnOnce() -> MissionId,
    ) -> (MissionId, ThreadId, bool) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state.thread_registry.iter().find(|thread| {
            &thread.session_id == session_id && thread.role_id == ORCHESTRATOR_ROLE_ID
        }) {
            return (thread.mission_id.clone(), thread.thread_id.clone(), false);
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
        });
        drop(state);
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RegisterThread);
        (mission_id, thread_id, true)
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

    /// 为已核验来源的恢复线程读取 canonical 投影，不读取缓存 transcript。
    pub fn canonical_thread_message_history(
        &self,
        thread: &ExecutionThread,
    ) -> DomainResult<Vec<ThreadChatMessage>> {
        let state = self.state.read().expect("session state read lock poisoned");
        project_thread_message_history(&state, thread, None)
    }

    /// 在同一锁内读取 canonical 历史和检查点，摘要视图只从这个快照派生。
    pub fn thread_context_snapshot(
        &self,
        thread_id: &ThreadId,
    ) -> DomainResult<ThreadContextSnapshot> {
        let state = self.state.read().expect("session state read lock poisoned");
        let thread = state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .ok_or(DomainError::NotFound { entity: "thread" })?;
        Ok(ThreadContextSnapshot {
            thread_id: thread.thread_id.clone(),
            session_id: thread.session_id.clone(),
            messages: project_thread_message_history(&state, thread, None)?,
            checkpoint: state
                .thread_context_checkpoints
                .iter()
                .find(|checkpoint| &checkpoint.thread_id == thread_id)
                .cloned(),
            context_window_tokens: thread.observed_context_window_tokens,
        })
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
        let session_id = state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .map(|thread| thread.session_id.clone());
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.observed_context_window_tokens = Some(context_window_tokens);
            thread.last_used_at = now;
        }
        drop(state);
        if let Some(session_id) = session_id {
            self.mark_sidecar_dirty_for_session(
                &session_id,
                SessionSidecarFlushReason::RegisterThread,
            );
        }
    }

    /// 安装上下文检查点并显式报告目标 thread 是否存在。
    pub fn install_thread_context_checkpoint_checked(
        &self,
        thread_id: &ThreadId,
        checkpoint: ThreadContextCheckpoint,
        now: UtcMillis,
    ) -> DomainResult<()> {
        let snapshot = self.thread_context_snapshot(thread_id)?;
        if &checkpoint.thread_id != thread_id
            || !self.install_thread_context_checkpoint_if_current(&snapshot, checkpoint, now)
        {
            return Err(DomainError::InvalidState {
                message: "上下文检查点来源、范围、指纹或代际与 canonical 快照不匹配".into(),
            });
        }
        Ok(())
    }

    /// 恢复摘要沿用来源快照，只有源、目标仍然有效且共享同一历史前缀时才能提交。
    pub fn inherit_thread_context_checkpoint(
        &self,
        source: &ThreadContextSnapshot,
        destination: &ThreadContextSnapshot,
        now: UtcMillis,
    ) -> DomainResult<()> {
        if source.session_id != destination.session_id || source.thread_id == destination.thread_id
        {
            return Err(DomainError::InvalidState {
                message: "恢复摘要必须属于同一会话的不同线程".into(),
            });
        }
        let Some(checkpoint) = source.checkpoint().filter(|checkpoint| {
            checkpoint.matches_history(source.messages())
                && checkpoint.matches_history(destination.messages())
        }) else {
            return Ok(());
        };
        let inherited = ThreadContextCheckpoint {
            thread_id: destination.thread_id.clone(),
            ..checkpoint.clone()
        };
        if !self.install_thread_context_checkpoint(destination, inherited, Some(source), now) {
            return Err(DomainError::InvalidState {
                message: "恢复摘要的来源或目标快照已变化，拒绝安装".into(),
            });
        }
        Ok(())
    }

    /// 安装时核对完整 canonical 历史、检查点身份和窗口；同长度改写也会拒绝。
    pub fn install_thread_context_checkpoint_if_current(
        &self,
        snapshot: &ThreadContextSnapshot,
        checkpoint: ThreadContextCheckpoint,
        now: UtcMillis,
    ) -> bool {
        self.install_thread_context_checkpoint(snapshot, checkpoint, None, now)
    }

    fn install_thread_context_checkpoint(
        &self,
        snapshot: &ThreadContextSnapshot,
        checkpoint: ThreadContextCheckpoint,
        source: Option<&ThreadContextSnapshot>,
        now: UtcMillis,
    ) -> bool {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !thread_context_snapshot_is_current(&state, snapshot)
            || source.is_some_and(|source| !thread_context_snapshot_is_current(&state, source))
            || checkpoint.thread_id != snapshot.thread_id
            || !checkpoint.matches_history(&snapshot.messages)
            || checkpoint.generation
                <= snapshot
                    .checkpoint
                    .as_ref()
                    .map(|value| value.generation)
                    .unwrap_or_default()
        {
            return false;
        }
        if let Some(existing) = state
            .thread_context_checkpoints
            .iter_mut()
            .find(|value| value.thread_id == snapshot.thread_id)
        {
            *existing = checkpoint;
        } else {
            state.thread_context_checkpoints.push(checkpoint);
        }
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| thread.thread_id == snapshot.thread_id)
        {
            thread.last_used_at = now;
        }
        drop(state);
        self.mark_sidecar_dirty_for_session(
            &snapshot.session_id,
            SessionSidecarFlushReason::RegisterThread,
        );
        true
    }

    /// 只清除读取时的检查点，不能删除并发安装的新摘要。
    pub fn clear_thread_context_checkpoint_if_current(
        &self,
        snapshot: &mut ThreadContextSnapshot,
    ) -> bool {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !thread_context_snapshot_is_current(&state, snapshot) {
            return false;
        }
        state
            .thread_context_checkpoints
            .retain(|value| value.thread_id != snapshot.thread_id);
        let changed = snapshot.checkpoint.take().is_some();
        drop(state);
        if changed {
            self.mark_sidecar_dirty_for_session(
                &snapshot.session_id,
                SessionSidecarFlushReason::RegisterThread,
            );
        }
        true
    }

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

    fn accept_active_execution_chain_with_timeline_entry_inner(
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

    fn update_recovery_id(
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

    fn update_current_turn_status_for_turn_with_change_at(
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
                    .extend(super::agent_turns::interrupted_descendant_turns(
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
                    && (!super::user_questions::pending_user_questions_in_state(
                        self, state, session_id,
                    )?
                    .is_empty()
                        || !super::tool_approvals::pending_in_state(self, state, session_id)?
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

    fn cancel_current_turn_with_source(
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
                        super::goals::pause_goal_and_bound_plan_in_state(
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
