use crate::{
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{
        summarize_tool_result, tool_execution_status_label, turn_item_status_for_tool_result,
    },
};
use magi_bridge_client::{ChatMessage, ChatToolCall};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId, ToolCallId,
    UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_session_store::{ActiveExecutionTurnItem, SessionStore};
use magi_skill_runtime::SkillRuntime;
use magi_tool_runtime::{
    ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};

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
        thread_visible: true,
        worker_visible: false,
    }
}

pub(crate) fn append_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) {
    let _ = session_store.append_current_turn_item(session_id, item);
}

pub(crate) fn upsert_session_turn_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    item: ActiveExecutionTurnItem,
) {
    let _ = session_store.upsert_current_turn_item(session_id, item);
}

pub(crate) fn publish_session_turn_item_event(
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item: &ActiveExecutionTurnItem,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-session-turn-item-{}", UtcMillis::now().0)),
            "session.turn.item",
            serde_json::json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "item": item,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
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
    append_session_turn_item(session_store, session_id, started_item.clone());
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &started_item);

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
    append_session_turn_item(session_store, session_id, result_item.clone());
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &result_item);

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
