use crate::{
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{
        summarize_tool_result, tool_execution_status_label, turn_item_status_for_tool_result,
    },
};
use magi_bridge_client::{ChatMessage, ChatToolCall};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId, TaskId,
    ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, SessionRuntimeTurnItemSummaryEntry,
    SessionRuntimeTurnLaneSummaryEntry, SessionRuntimeTurnSummaryEntry,
};
use magi_governance::ToolKind;
use magi_session_store::{
    ActiveExecutionTurnItem, SessionRuntimeSidecar, SessionStore, TimelineEntryKind,
};
use magi_skill_runtime::SkillRuntime;
use magi_tool_runtime::{
    ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use serde::Serialize;

#[derive(Clone, Debug)]
pub(crate) struct PublishedSessionTurnItem {
    pub turn_id: String,
    pub turn_seq: u64,
    pub item: ActiveExecutionTurnItem,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionTurnIdentity {
    pub turn_id: String,
    pub turn_seq: u64,
}

fn published_session_turn_item_from_sidecar(
    sidecar: SessionRuntimeSidecar,
    item_id: &str,
) -> Option<PublishedSessionTurnItem> {
    let turn = sidecar.current_turn?;
    let item = turn
        .items
        .iter()
        .find(|candidate| candidate.item_id == item_id)?
        .clone();
    Some(PublishedSessionTurnItem {
        turn_id: turn.turn_id,
        turn_seq: turn.turn_seq,
        item,
    })
}

pub(crate) fn current_session_turn_identity(
    session_store: &SessionStore,
    session_id: &SessionId,
) -> Option<SessionTurnIdentity> {
    let sidecar = session_store.runtime_sidecar(session_id)?;
    let turn = sidecar.current_turn.as_ref()?;
    Some(SessionTurnIdentity {
        turn_id: turn.turn_id.clone(),
        turn_seq: turn.turn_seq,
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
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .append_current_turn_item(session_id, item)
        .ok()
        .flatten()?;
    published_session_turn_item_from_sidecar(sidecar, &item_id)
}

pub(crate) fn upsert_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) -> Option<PublishedSessionTurnItem> {
    let item_id = item.item_id.clone();
    let sidecar = session_store
        .upsert_current_turn_item(session_id, item)
        .ok()
        .flatten()?;
    published_session_turn_item_from_sidecar(sidecar, &item_id)
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

fn to_turn_item_summary(item: &ActiveExecutionTurnItem) -> SessionRuntimeTurnItemSummaryEntry {
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
        role_id: item.role_id.clone(),
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
) -> SessionRuntimeTurnLaneSummaryEntry {
    SessionRuntimeTurnLaneSummaryEntry {
        lane_id: lane.lane_id.clone(),
        lane_seq: lane.lane_seq,
        task_id: lane.task_id.to_string(),
        worker_id: lane.worker_id.to_string(),
        role_id: lane.role_id.clone(),
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
) -> Option<String> {
    let sidecar = session_store.runtime_sidecar(session_id)?;
    let turn = sidecar.current_turn.as_ref()?;
    let chain = sidecar.active_execution_chain.as_ref();
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

    let lane_status_by_id = turn
        .items
        .iter()
        .filter_map(|item| {
            item.lane_id
                .as_ref()
                .map(|lane_id| (lane_id.clone(), item.status.clone()))
        })
        .collect::<std::collections::HashMap<_, _>>();

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
        turn_items: turn.items.iter().map(to_turn_item_summary).collect(),
        worker_lanes: turn
            .worker_lanes
            .iter()
            .map(|lane| {
                let status = lane_status_by_id
                    .get(&lane.lane_id)
                    .map(String::as_str)
                    .unwrap_or(turn.status.as_str());
                to_turn_lane_summary(lane, status)
            })
            .collect(),
    };

    serde_json::to_string(&snapshot).ok()
}

pub(crate) fn append_session_tool_call_items(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&SkillRuntime>,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
    messages: &mut Vec<ChatMessage>,
) {
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

    let (tool_result, tool_status) = execute_session_turn_tool_call(
        event_bus,
        tool_registry,
        skill_runtime,
        tool_call,
        session_id,
        workspace_id,
    );
    let status_label = tool_execution_status_label(tool_status);
    let mut result_item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(&tool_result)),
        Some(format!("turn-item-tool-result-{}", tool_call.id)),
    );
    result_item.source = "tool".to_string();
    result_item.tool_call_id = Some(tool_call.id.clone());
    result_item.tool_name = Some(tool_call.function.name.clone());
    result_item.tool_status = Some(status_label.to_string());
    result_item.tool_arguments = Some(tool_call.function.arguments.clone());
    result_item.tool_result = Some(tool_result.clone());
    if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
        result_item.tool_error = Some(tool_result.clone());
    }
    if let Some(published) = append_session_turn_item(session_store, session_id, result_item) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }

    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(tool_result),
        tool_calls: Vec::new(),
        tool_call_id: Some(tool_call.id.clone()),
    });
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
    use magi_governance::GovernanceService;
    use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry};
    use std::sync::Arc;

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
}
