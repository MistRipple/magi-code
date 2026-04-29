use crate::{
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{
        summarize_tool_result, tool_execution_status_label, turn_item_status_for_tool_result,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId, TaskId, TaskStatus,
    ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, SessionRuntimeTurnItemSummaryEntry,
    SessionRuntimeTurnLaneSummaryEntry, SessionRuntimeTurnSummaryEntry,
};
use magi_governance::ToolKind;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    ActiveExecutionChain, ActiveExecutionTurn, ActiveExecutionTurnItem, SessionRuntimeSidecar,
    SessionStore, TimelineEntryKind,
};
use magi_skill_runtime::SkillRuntime;
use magi_tool_runtime::{
    ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use serde::Serialize;
use std::thread;

#[derive(Clone, Debug)]
pub(crate) struct PublishedSessionTurnItem {
    pub turn_id: String,
    pub turn_seq: u64,
    pub item: ActiveExecutionTurnItem,
    pub current_turn: SessionRuntimeTurnSummaryEntry,
    pub turn_items: Vec<SessionRuntimeTurnItemSummaryEntry>,
    pub worker_lanes: Vec<SessionRuntimeTurnLaneSummaryEntry>,
}

fn published_session_turn_item_from_sidecar(
    sidecar: SessionRuntimeSidecar,
    item_id: &str,
    task_store: Option<&TaskStore>,
) -> Option<PublishedSessionTurnItem> {
    let turn = sidecar.current_turn.as_ref()?;
    let item = turn
        .items
        .iter()
        .find(|candidate| candidate.item_id == item_id)?
        .clone();
    let chain = chain_for_turn_summary(&sidecar, turn);
    let response_duration_ms = turn
        .completed_at
        .map(|completed_at| completed_at.0.saturating_sub(turn.accepted_at.0));
    let lane_status_by_id = turn_lane_status_by_id(turn, task_store);
    Some(PublishedSessionTurnItem {
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
        item,
        current_turn: SessionRuntimeTurnSummaryEntry {
            turn_id: turn.turn_id.clone(),
            turn_seq: turn.turn_seq,
            accepted_at: Some(turn.accepted_at),
            completed_at: turn.completed_at,
            response_duration_ms,
            status: turn.status.clone(),
            user_message: turn.user_message.clone(),
            mission_id: chain.map(|chain| chain.mission_id.to_string()),
            root_task_id: chain.map(|chain| chain.root_task_id.to_string()),
            execution_chain_ref: chain.map(|chain| chain.execution_chain_ref.clone()),
        },
        turn_items: turn
            .items
            .iter()
            .map(|item| to_turn_item_summary(item, task_store))
            .collect(),
        worker_lanes: turn
            .worker_lanes
            .iter()
            .map(|lane| {
                let status = lane_status_by_id
                    .get(&lane.lane_id)
                    .map(String::as_str)
                    .unwrap_or(turn.status.as_str());
                to_turn_lane_summary(lane, status, task_store)
            })
            .collect(),
    })
}

fn chain_for_turn_summary<'a>(
    sidecar: &'a SessionRuntimeSidecar,
    turn: &ActiveExecutionTurn,
) -> Option<&'a ActiveExecutionChain> {
    if turn_has_execution_chain_items(turn) {
        sidecar.active_execution_chain.as_ref()
    } else {
        None
    }
}

fn turn_has_execution_chain_items(turn: &ActiveExecutionTurn) -> bool {
    !turn.worker_lanes.is_empty()
        || turn.items.iter().any(|item| {
            item.task_id.is_some() || item.worker_id.is_some() || item.lane_id.is_some()
        })
}

pub(crate) fn session_turn_item(
    kind: &str,
    status: &str,
    title: Option<String>,
    content: Option<String>,
    item_id: Option<String>,
) -> ActiveExecutionTurnItem {
    ActiveExecutionTurnItem {
        item_id: item_id.unwrap_or_else(|| format!("turn-item-{}-{}", kind, UtcMillis::now().0)),
        item_seq: 0,
        lane_id: None,
        lane_seq: None,
        kind: kind.to_string(),
        status: status.to_string(),
        source: "orchestrator".to_string(),
        title,
        content,
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
        timeline_entry_id: None,
        thread_visible: true,
        worker_visible: false,
    }
}

pub(crate) fn append_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) -> Option<PublishedSessionTurnItem> {
    append_session_turn_item_with_task_store(session_store, session_id, item, None)
}

pub(crate) fn append_session_turn_item_with_task_store(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Option<PublishedSessionTurnItem> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .append_current_turn_item(session_id, item)
        .ok()
        .flatten()?;
    published_session_turn_item_from_sidecar(sidecar, &item_id, task_store)
}

pub(crate) fn upsert_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) -> Option<PublishedSessionTurnItem> {
    upsert_session_turn_item_with_task_store(session_store, session_id, item, None)
}

pub(crate) fn upsert_session_turn_item_with_task_store(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> Option<PublishedSessionTurnItem> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .upsert_current_turn_item(session_id, item)
        .ok()
        .flatten()?;
    published_session_turn_item_from_sidecar(sidecar, &item_id, task_store)
}

pub(crate) fn publish_session_turn_item_event(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    published: &PublishedSessionTurnItem,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-session-turn-item-{}", UtcMillis::now().0)),
            "session.turn.item",
            serde_json::json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "turn_id": published.turn_id,
                "turn_seq": published.turn_seq,
                "item": published.item,
                "current_turn": published.current_turn,
                "turn_items": published.turn_items,
                "worker_lanes": published.worker_lanes,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

pub(crate) fn append_session_turn_error_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    task_id: Option<&TaskId>,
    request_id: Option<&str>,
    user_message_id: Option<&str>,
    placeholder_message_id: Option<&str>,
    error_text: &str,
    streaming_entry_id: Option<&str>,
) {
    let mut error_item = session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error_text.to_string()),
        Some(format!("turn-item-assistant-error-{}", UtcMillis::now().0)),
    );
    if let Some(task_id) = task_id {
        error_item.task_id = Some(task_id.clone());
    }
    error_item.request_id = request_id.map(str::to_string);
    error_item.user_message_id = user_message_id.map(str::to_string);
    error_item.placeholder_message_id = placeholder_message_id.map(str::to_string);
    if let Some(published) = append_session_turn_item(session_store, session_id, error_item) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    let _ = session_store.update_current_turn_status(session_id, "failed");
    let timeline_message = build_completed_turn_timeline_snapshot(
        session_store,
        session_id,
        Some(error_text),
        streaming_entry_id,
        None,
    )
    .unwrap_or_else(|| error_text.to_string());
    let fallback_entry_id = session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| {
            sidecar.current_turn.as_ref().map(|turn| {
                format!(
                    "timeline-turn-snapshot-error-{}-{}",
                    session_id, turn.turn_id
                )
            })
        })
        .unwrap_or_else(|| format!("timeline-turn-snapshot-error-{}", session_id));
    let entry_id = streaming_entry_id.unwrap_or(fallback_entry_id.as_str());
    session_store.upsert_timeline_entry(
        session_id.clone(),
        entry_id,
        TimelineEntryKind::AssistantMessage,
        timeline_message,
    );
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct CompletedTurnTimelineSnapshot {
    session_id: String,
    mission_id: Option<String>,
    root_task_id: Option<String>,
    execution_chain_ref: Option<String>,
    final_text: Option<String>,
    streaming_entry_id: Option<String>,
    is_historical_turn_snapshot: bool,
    current_turn: SessionRuntimeTurnSummaryEntry,
    turn_items: Vec<SessionRuntimeTurnItemSummaryEntry>,
    worker_lanes: Vec<SessionRuntimeTurnLaneSummaryEntry>,
}

fn task_role_id(task_store: Option<&TaskStore>, task_id: &TaskId) -> Option<String> {
    task_store
        .and_then(|store| store.get_task(task_id))
        .and_then(|task| task.executor_binding)
        .map(|binding| binding.target_role)
        .filter(|role_id| !role_id.trim().is_empty())
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Draft => "draft",
        TaskStatus::Ready => "ready",
        TaskStatus::Running => "running",
        TaskStatus::AwaitingApproval => "awaiting_approval",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Verifying => "verifying",
        TaskStatus::Repairing => "repairing",
        TaskStatus::Skipped => "skipped",
    }
}

fn turn_lane_status_by_id(
    turn: &magi_session_store::ActiveExecutionTurn,
    task_store: Option<&TaskStore>,
) -> std::collections::HashMap<String, String> {
    turn.worker_lanes
        .iter()
        .map(|lane| {
            let status = task_store
                .and_then(|store| store.get_task(&lane.task_id))
                .map(|task| task_status_label(&task.status).to_string())
                .or_else(|| {
                    turn.items.iter().rev().find_map(|item| {
                        (item.lane_id.as_ref() == Some(&lane.lane_id)).then(|| item.status.clone())
                    })
                })
                .unwrap_or_else(|| turn.status.clone());
            (lane.lane_id.clone(), status)
        })
        .collect()
}

fn to_turn_item_summary(
    item: &ActiveExecutionTurnItem,
    task_store: Option<&TaskStore>,
) -> SessionRuntimeTurnItemSummaryEntry {
    let role_id = item.role_id.clone().or_else(|| {
        item.task_id
            .as_ref()
            .and_then(|task_id| task_role_id(task_store, task_id))
    });
    SessionRuntimeTurnItemSummaryEntry {
        item_id: item.item_id.clone(),
        item_seq: item.item_seq,
        lane_id: item.lane_id.clone(),
        lane_seq: item.lane_seq,
        kind: item.kind.clone(),
        status: item.status.clone(),
        source: item.source.clone(),
        title: item.title.clone(),
        content: item.content.clone(),
        task_id: item.task_id.as_ref().map(ToString::to_string),
        worker_id: item.worker_id.as_ref().map(ToString::to_string),
        role_id,
        tool_call_id: item.tool_call_id.clone(),
        tool_name: item.tool_name.clone(),
        tool_status: item.tool_status.clone(),
        tool_arguments: item.tool_arguments.clone(),
        tool_result: item.tool_result.clone(),
        tool_error: item.tool_error.clone(),
        request_id: item.request_id.clone(),
        user_message_id: item.user_message_id.clone(),
        placeholder_message_id: item.placeholder_message_id.clone(),
        timeline_entry_id: item.timeline_entry_id.clone(),
        thread_visible: item.thread_visible,
        worker_visible: item.worker_visible,
    }
}

fn to_turn_lane_summary(
    lane: &magi_session_store::ActiveExecutionTurnLane,
    status: &str,
    task_store: Option<&TaskStore>,
) -> SessionRuntimeTurnLaneSummaryEntry {
    let role_id = lane
        .role_id
        .clone()
        .or_else(|| task_role_id(task_store, &lane.task_id));
    SessionRuntimeTurnLaneSummaryEntry {
        lane_id: lane.lane_id.clone(),
        lane_seq: lane.lane_seq,
        task_id: lane.task_id.to_string(),
        worker_id: lane.worker_id.to_string(),
        role_id,
        title: lane.title.clone(),
        status: status.to_string(),
        is_primary: lane.is_primary,
    }
}

pub(crate) fn build_completed_turn_timeline_snapshot(
    session_store: &SessionStore,
    session_id: &SessionId,
    fallback_final_text: Option<&str>,
    streaming_entry_id: Option<&str>,
    task_store: Option<&TaskStore>,
) -> Option<String> {
    let sidecar = session_store.runtime_sidecar(session_id)?;
    let turn = sidecar.current_turn.as_ref()?;
    let chain = chain_for_turn_summary(&sidecar, turn);
    let completed_at = turn.completed_at.unwrap_or_else(UtcMillis::now);
    let response_duration_ms = Some(completed_at.0.saturating_sub(turn.accepted_at.0));
    let final_text = fallback_final_text
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| {
            turn.items
                .iter()
                .rev()
                .find(|item| item.kind == "assistant_final")
                .and_then(|item| item.content.clone())
                .filter(|text| !text.trim().is_empty())
                .or_else(|| {
                    turn.items
                        .iter()
                        .rev()
                        .find(|item| item.kind == "assistant_error")
                        .and_then(|item| item.content.clone())
                        .filter(|text| !text.trim().is_empty())
                        .or_else(|| {
                            turn.items
                                .iter()
                                .rev()
                                .find(|item| item.kind == "assistant_stream")
                                .and_then(|item| item.content.clone())
                                .filter(|text| !text.trim().is_empty())
                        })
                })
        });

    let lane_status_by_id = turn_lane_status_by_id(turn, task_store);

    let snapshot = CompletedTurnTimelineSnapshot {
        session_id: session_id.to_string(),
        mission_id: chain.map(|chain| chain.mission_id.to_string()),
        root_task_id: chain.map(|chain| chain.root_task_id.to_string()),
        execution_chain_ref: chain.map(|chain| chain.execution_chain_ref.clone()),
        final_text,
        streaming_entry_id: streaming_entry_id.map(str::to_string),
        is_historical_turn_snapshot: true,
        current_turn: SessionRuntimeTurnSummaryEntry {
            turn_id: turn.turn_id.clone(),
            turn_seq: turn.turn_seq,
            accepted_at: Some(turn.accepted_at),
            completed_at: Some(completed_at),
            response_duration_ms,
            status: turn.status.clone(),
            user_message: turn.user_message.clone(),
            mission_id: chain.map(|chain| chain.mission_id.to_string()),
            root_task_id: chain.map(|chain| chain.root_task_id.to_string()),
            execution_chain_ref: chain.map(|chain| chain.execution_chain_ref.clone()),
        },
        turn_items: turn
            .items
            .iter()
            .map(|item| to_turn_item_summary(item, task_store))
            .collect(),
        worker_lanes: turn
            .worker_lanes
            .iter()
            .map(|lane| {
                let status = lane_status_by_id
                    .get(&lane.lane_id)
                    .map(String::as_str)
                    .unwrap_or(turn.status.as_str());
                to_turn_lane_summary(lane, status, task_store)
            })
            .collect(),
    };

    serde_json::to_string(&snapshot).ok()
}

pub(crate) fn append_session_tool_call_items_batch(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_calls: &[ChatToolCall],
    messages: &mut Vec<ChatMessage>,
    write_allowed: impl Fn() -> bool,
) -> bool {
    for tool_call in tool_calls {
        if !write_allowed() {
            return false;
        }
        let mut started_item = session_turn_item(
            "tool_call_started",
            "running",
            Some(tool_call.function.name.clone()),
            Some(format!("正在调用工具：{}", tool_call.function.name)),
            Some(format!("turn-item-tool-started-{}", tool_call.id)),
        );
        started_item.source = "tool".to_string();
        started_item.tool_call_id = Some(tool_call.id.clone());
        started_item.tool_name = Some(tool_call.function.name.clone());
        started_item.tool_arguments = Some(tool_call.function.arguments.clone());
        if let Some(published) = append_session_turn_item(session_store, session_id, started_item) {
            publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
        }
    }

    let tool_results = execute_session_turn_tool_call_batch(
        event_bus,
        tool_registry,
        skill_runtime,
        tool_calls,
        session_id,
        workspace_id,
    );
    for (tool_call, (tool_result, tool_status)) in tool_calls.iter().zip(tool_results.into_iter()) {
        if !write_allowed() {
            return false;
        }
        append_session_tool_call_result_item(
            session_store,
            event_bus,
            session_id,
            workspace_id,
            tool_call,
            &tool_result,
            tool_status,
        );
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(tool_result),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call.id.clone()),
        });
    }
    true
}

fn append_session_tool_call_result_item(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
) {
    let status_label = tool_execution_status_label(tool_status);
    let mut result_item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(format!("turn-item-tool-result-{}", tool_call.id)),
    );
    result_item.source = "tool".to_string();
    result_item.tool_call_id = Some(tool_call.id.clone());
    result_item.tool_name = Some(tool_call.function.name.clone());
    result_item.tool_status = Some(status_label.to_string());
    result_item.tool_arguments = Some(tool_call.function.arguments.clone());
    result_item.tool_result = Some(tool_result.to_string());
    if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
        result_item.tool_error = Some(tool_result.to_string());
    }
    if let Some(published) = append_session_turn_item(session_store, session_id, result_item) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn execute_session_turn_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    tool_calls: &[ChatToolCall],
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
) -> Vec<(String, ExecutionResultStatus)> {
    let parsed_arguments = tool_calls
        .iter()
        .map(|tool_call| {
            serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments).ok()
        })
        .collect::<Vec<_>>();
    let tool_inputs = tool_calls
        .iter()
        .zip(parsed_arguments.iter())
        .map(|(tool_call, arguments)| ToolConcurrencyInput {
            tool_name: tool_call.function.name.as_str(),
            arguments: arguments.as_ref(),
        })
        .collect::<Vec<_>>();
    let mut results = vec![None; tool_calls.len()];

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    results[tool_index] = Some(execute_session_turn_tool_call(
                        event_bus,
                        tool_registry,
                        skill_runtime,
                        &tool_calls[tool_index],
                        session_id,
                        workspace_id,
                    ));
                }
            }
            ToolBatchKind::Concurrent => {
                thread::scope(|scope| {
                    let handles = batch
                        .tool_indices
                        .iter()
                        .copied()
                        .map(|tool_index| {
                            let tool_call = &tool_calls[tool_index];
                            (
                                tool_index,
                                scope.spawn(move || {
                                    execute_session_turn_tool_call(
                                        event_bus,
                                        tool_registry,
                                        skill_runtime,
                                        tool_call,
                                        session_id,
                                        workspace_id,
                                    )
                                }),
                            )
                        })
                        .collect::<Vec<_>>();

                    for (tool_index, handle) in handles {
                        let result = handle.join().unwrap_or_else(|_| {
                            (
                                serde_json::json!({
                                    "tool": tool_calls[tool_index].function.name,
                                    "status": "failed",
                                    "error": "工具执行线程异常"
                                })
                                .to_string(),
                                ExecutionResultStatus::Failed,
                            )
                        });
                        results[tool_index] = Some(result);
                    }
                });
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(tool_index, result)| {
            result.unwrap_or_else(|| {
                (
                    serde_json::json!({
                        "tool": tool_calls[tool_index].function.name,
                        "status": "failed",
                        "error": "工具执行结果缺失"
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                )
            })
        })
        .collect()
}

fn execute_session_turn_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    tool_call: &ChatToolCall,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
) -> (String, ExecutionResultStatus) {
    let Some(registry) = tool_registry else {
        return (
            serde_json::json!({ "error": "tool registry not available" }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-session-turn-tool-{}", UtcMillis::now().0)),
            "session.turn.tool.invoked",
            serde_json::json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "tool_name": tool_call.function.name,
                "tool_call_id": tool_call.id,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );

    if tool_call.function.name == SKILL_APPLY_TOOL_NAME {
        return execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
    }

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_call.function.name.clone(),
            tool_kind: ToolKind::Builtin,
            input: tool_call.function.arguments.clone(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            worker_id: None,
            task_id: None,
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
        },
        &ToolExecutionPolicy::default(),
    );
    (output.payload, output.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;
    use magi_core::{ExecutorBinding, MissionId, Task, TaskKind, WorkerId};
    use magi_governance::GovernanceService;
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
        ActiveExecutionTurnLane,
    };
    use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry};
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    struct ConcurrentToolProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay: Duration,
    }

    impl ConcurrentToolProbe {
        fn new(delay: Duration) -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                delay,
            }
        }

        fn max_active(&self) -> usize {
            self.max_active.load(Ordering::SeqCst)
        }

        fn record_active_call(&self) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(self.delay);
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct ProbeBuiltinTool {
        name: &'static str,
        probe: Arc<ConcurrentToolProbe>,
    }

    impl ProbeBuiltinTool {
        fn new(name: &'static str, probe: Arc<ConcurrentToolProbe>) -> Self {
            Self { name, probe }
        }
    }

    impl BuiltinTool for ProbeBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(&self, input: &str, _context: &ToolExecutionContext) -> String {
            self.probe.record_active_call();
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "stdout": format!("{} done", self.name),
                "input": input,
            })
            .to_string()
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    #[test]
    fn session_turn_item_uses_expected_defaults() {
        let item = session_turn_item(
            "assistant_phase",
            "running",
            Some("理解请求".to_string()),
            Some("准备中".to_string()),
            None,
        );

        assert!(item.item_id.starts_with("turn-item-assistant_phase-"));
        assert_eq!(item.kind, "assistant_phase");
        assert_eq!(item.status, "running");
        assert_eq!(item.source, "orchestrator");
        assert!(item.thread_visible);
        assert!(!item.worker_visible);
    }

    #[test]
    fn execute_session_turn_tool_call_requires_registry_before_runtime_tools() {
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: SKILL_APPLY_TOOL_NAME.to_string(),
                arguments: serde_json::json!({ "skill_name": "code-review" }).to_string(),
            },
        };

        let (_, status) = execute_session_turn_tool_call(
            &event_bus,
            None,
            None,
            &call,
            &SessionId::new("session-1"),
            &None,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        assert!(event_bus.snapshot().recent_events.is_empty());
    }

    #[test]
    fn execute_session_turn_tool_call_uses_skill_runtime_after_registry_check() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性风险。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        let skill_runtime = SkillRuntime::new(registry);
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let call = ChatToolCall {
            id: "tool-call-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: SKILL_APPLY_TOOL_NAME.to_string(),
                arguments: serde_json::json!({ "skill_name": "code-review" }).to_string(),
            },
        };

        let (payload, status) = execute_session_turn_tool_call(
            &event_bus,
            Some(&tool_registry),
            Some(&skill_runtime),
            &call,
            &SessionId::new("session-1"),
            &None,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["skill_name"], "code-review");
        assert_eq!(event_bus.snapshot().recent_events.len(), 1);
    }

    #[test]
    fn session_turn_shell_tool_batch_executes_concurrently_and_preserves_order() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("session-turn-shell-batch");
        let workspace_id = Some(WorkspaceId::new("workspace-turn-shell-batch"));
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "shell batch session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-shell-batch".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("并发执行 shell".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let probe = Arc::new(ConcurrentToolProbe::new(Duration::from_millis(180)));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        tool_registry.register_builtin(Arc::new(ProbeBuiltinTool::new(
            "shell_exec",
            Arc::clone(&probe),
        )));
        tool_registry
            .register_builtin(Arc::new(ProbeBuiltinTool::new("shell", Arc::clone(&probe))));

        let tool_calls = vec![
            ChatToolCall {
                id: "tool-call-shell-a".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "shell_exec".to_string(),
                    arguments: serde_json::json!({
                        "command": "printf a",
                        "access_mode": "read_only"
                    })
                    .to_string(),
                },
            },
            ChatToolCall {
                id: "tool-call-shell-b".to_string(),
                kind: "function".to_string(),
                function: ChatToolFunction {
                    name: "shell".to_string(),
                    arguments: serde_json::json!({
                        "command": "printf b",
                        "access_mode": "read_only"
                    })
                    .to_string(),
                },
            },
        ];
        let mut messages = Vec::new();

        append_session_tool_call_items_batch(
            &session_store,
            &event_bus,
            Some(&tool_registry),
            None,
            &session_id,
            &workspace_id,
            &tool_calls,
            &mut messages,
            || true,
        );

        assert!(
            probe.max_active() > 1,
            "session turn 中的多个 shell 工具调用必须并发执行"
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tool_call_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("tool-call-shell-a"), Some("tool-call-shell-b")]
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["tool", "tool"]
        );

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        assert_eq!(
            turn.items
                .iter()
                .map(|item| (item.kind.as_str(), item.tool_call_id.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("tool_call_started", Some("tool-call-shell-a")),
                ("tool_call_started", Some("tool-call-shell-b")),
                ("tool_call_result", Some("tool-call-shell-a")),
                ("tool_call_result", Some("tool-call-shell-b")),
            ]
        );

        let snapshot_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| event.event_type == "session.turn.item")
            .expect("session.turn.item event should be published");
        assert!(
            snapshot_event.payload.get("current_turn").is_some(),
            "实时 turn item 事件必须携带完整 current_turn 快照"
        );
        assert_eq!(
            snapshot_event
                .payload
                .get("turn_items")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(4),
            "实时 turn item 事件必须携带当前 turn 的全部 item"
        );
        assert!(
            snapshot_event.payload.get("worker_lanes").is_some(),
            "实时 turn item 事件必须携带 worker_lanes，供前端走 canonical projection"
        );
    }

    #[test]
    fn live_turn_item_worker_lane_status_uses_task_store_instead_of_spawned_item() {
        let session_store = SessionStore::new();
        let task_store = TaskStore::new();
        let session_id = SessionId::new("session-worker-lane-authority");
        let task_id = TaskId::new("task-worker-lane-authority");
        let worker_id = WorkerId::new("worker-worker-lane-authority");
        let lane_id = "lane-task-worker-lane-authority".to_string();
        session_store
            .create_session(session_id.clone(), "worker lane authority")
            .expect("session should be creatable");

        let now = UtcMillis::now();
        task_store.insert_task(Task {
            task_id: task_id.clone(),
            mission_id: MissionId::new("mission-worker-lane-authority"),
            root_task_id: task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "权威任务状态".to_string(),
            goal: "验证 worker lane 状态来源".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(ExecutorBinding {
                target_role: "reviewer".to_string(),
                capability_requirements: Vec::new(),
                parallelism_group: None,
                exclusive_scope: None,
                worker_selector: None,
            }),
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: now,
            updated_at: now,
        });

        let mut spawned_item = session_turn_item(
            "worker_spawned",
            "pending",
            Some("权威任务状态".to_string()),
            Some("已创建执行分支。".to_string()),
            Some("turn-item-worker-spawned-authority".to_string()),
        );
        spawned_item.item_seq = 1;
        spawned_item.lane_id = Some(lane_id.clone());
        spawned_item.lane_seq = Some(1);
        spawned_item.task_id = Some(task_id.clone());
        spawned_item.worker_id = Some(worker_id.clone());
        spawned_item.role_id = Some("reviewer".to_string());
        spawned_item.thread_visible = false;
        spawned_item.worker_visible = true;

        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-worker-lane-authority".to_string(),
                    turn_seq: 1,
                    accepted_at: now,
                    status: "running".to_string(),
                    user_message: Some("验证 worker lane 状态来源".to_string()),
                    items: vec![spawned_item],
                    worker_lanes: vec![ActiveExecutionTurnLane {
                        lane_id: lane_id.clone(),
                        lane_seq: 1,
                        task_id: task_id.clone(),
                        worker_id,
                        role_id: None,
                        title: "权威任务状态".to_string(),
                        is_primary: true,
                    }],
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let published = append_session_turn_item_with_task_store(
            &session_store,
            &session_id,
            session_turn_item(
                "assistant_phase",
                "running",
                Some("理解请求".to_string()),
                Some("准备中".to_string()),
                Some("turn-item-phase-authority".to_string()),
            ),
            Some(&task_store),
        )
        .expect("published turn item should be available");

        let lane = published
            .worker_lanes
            .iter()
            .find(|lane| lane.lane_id == lane_id)
            .expect("published payload should include worker lane");
        assert_eq!(
            lane.status, "running",
            "worker_spawned 的 pending 只是生命周期事件，不能覆盖 TaskStore 中的执行状态"
        );
        assert_eq!(
            lane.role_id.as_deref(),
            Some("reviewer"),
            "worker lane role_id 应继续从 task executor binding 回填"
        );
    }

    #[test]
    fn completed_plain_turn_snapshot_does_not_inherit_previous_execution_chain() {
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-plain-after-task");
        let mission_id = MissionId::new("mission-previous-task");
        let root_task_id = TaskId::new("task-previous-root");
        let now = UtcMillis::now();
        session_store
            .create_session(session_id.clone(), "plain after task")
            .expect("session should be creatable");
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id,
                    root_task_id,
                    execution_chain_ref: "chain-previous-task".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: Vec::new(),
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-previous-task".to_string(),
                        trimmed_text: Some("之前的任务".to_string()),
                        deep_task: true,
                        skill_name: None,
                    },
                    current_turn: None,
                },
            )
            .expect("chain should be stored");

        let final_item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("OK-普通流式".to_string()),
            Some("turn-item-plain-final".to_string()),
        );
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-session-plain".to_string(),
                    turn_seq: 2,
                    accepted_at: now,
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("普通流式验证".to_string()),
                    items: vec![final_item],
                    worker_lanes: Vec::new(),
                },
            )
            .expect("plain turn should be stored");
        session_store
            .update_current_turn_status(&session_id, "completed")
            .expect("plain turn should complete");

        let snapshot = build_completed_turn_timeline_snapshot(
            &session_store,
            &session_id,
            Some("OK-普通流式"),
            Some("turn-item-plain-final"),
            None,
        )
        .expect("snapshot should be built");
        let parsed: serde_json::Value =
            serde_json::from_str(&snapshot).expect("snapshot should be valid json");

        assert_eq!(
            parsed["mission_id"],
            serde_json::Value::Null,
            "普通 turn 快照不能继承上一轮任务 mission_id"
        );
        assert_eq!(
            parsed["root_task_id"],
            serde_json::Value::Null,
            "普通 turn 快照不能继承上一轮任务 root_task_id"
        );
        assert_eq!(
            parsed["execution_chain_ref"],
            serde_json::Value::Null,
            "普通 turn 快照不能继承上一轮任务 execution_chain_ref"
        );
        assert_eq!(
            parsed["current_turn"]["mission_id"],
            serde_json::Value::Null,
            "current_turn 同样必须保持普通对话归属"
        );
    }
}
