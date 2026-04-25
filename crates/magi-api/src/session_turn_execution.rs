use crate::{
    errors::ApiError,
    prompt_utils::normalize_model_visible_content,
    session_turn_writeback::{
        append_session_tool_call_items, append_session_turn_item, publish_session_turn_item_event,
        session_turn_item, upsert_session_turn_item,
    },
    settings_store::SettingsStore,
    usage_recording::{
        ModelUsageBinding, publish_model_usage_record, session_turn_model_usage_binding,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest, ModelStreamingDelta,
};
use magi_core::{EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_session_store::{SessionStore, TimelineEntryKind};
use magi_tool_runtime::ToolRegistry;
use magi_usage_authority::UsageCallStatus;
use std::sync::Arc;

const MAX_TOOL_CALL_ROUNDS: usize = 8;
pub const BUSINESS_MODEL_PROVIDER: &str = "openai-compatible";

pub struct SessionTurnExecutionRequest {
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub prompt: String,
    pub use_tools: bool,
    pub skill_name: Option<String>,
}

pub struct SessionTurnExecutionOutput {
    pub final_content: String,
}

pub(crate) struct SessionTurnExecutionRuntime<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub request: SessionTurnExecutionRequest,
    pub prompt: String,
    pub tools: Option<Vec<ChatToolDefinition>>,
}

pub(crate) fn run_session_turn_execution(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, ApiError> {
    let SessionTurnExecutionRuntime {
        client,
        event_bus,
        session_store,
        settings_store,
        tool_registry,
        skill_runtime,
        request,
        prompt,
        tools,
    } = runtime;

    append_phase_item(event_bus, session_store, &request);

    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }];
    let mut final_content: Option<String> = None;
    let usage_binding = session_turn_model_usage_binding(request.use_tools);

    for round in 0..MAX_TOOL_CALL_ROUNDS {
        let streamed_content = stream_session_turn_round(
            SessionTurnRoundRuntime {
                client,
                event_bus,
                session_store,
                settings_store,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &prompt,
                tools: tools.clone(),
                messages: &mut messages,
                round,
            },
            tool_registry,
            skill_runtime,
        )?;

        if let Some(content) = streamed_content.final_content {
            final_content = Some(content);
            break;
        }
    }

    let final_content = final_content.ok_or_else(|| {
        ApiError::internal_assembly("执行 session turn 失败", "模型未在工具调用后返回最终回复")
    })?;
    append_final_item(
        event_bus,
        session_store,
        &request.session_id,
        &request.workspace_id,
        &final_content,
    );

    Ok(SessionTurnExecutionOutput { final_content })
}

struct SessionTurnRoundRuntime<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    settings_store: Option<&'a Arc<SettingsStore>>,
    request: &'a SessionTurnExecutionRequest,
    usage_binding: &'a ModelUsageBinding,
    prompt: &'a str,
    tools: Option<Vec<ChatToolDefinition>>,
    messages: &'a mut Vec<ChatMessage>,
    round: usize,
}

struct SessionTurnRoundOutput {
    final_content: Option<String>,
}

fn stream_session_turn_round(
    runtime: SessionTurnRoundRuntime<'_>,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
) -> Result<SessionTurnRoundOutput, ApiError> {
    let SessionTurnRoundRuntime {
        client,
        event_bus,
        session_store,
        settings_store,
        request,
        usage_binding,
        prompt,
        tools,
        messages,
        round,
    } = runtime;

    let stream_item_id = format!(
        "turn-item-assistant-stream-{}-{}",
        UtcMillis::now().0,
        round
    );
    let thinking_item_id = format!(
        "turn-item-assistant-thinking-{}-{}",
        UtcMillis::now().0,
        round
    );
    let streamed_content = std::cell::RefCell::new(String::new());
    let streamed_thinking = std::cell::RefCell::new(String::new());
    let last_content_len = std::cell::Cell::new(0usize);
    let last_thinking_len = std::cell::Cell::new(0usize);
    let on_delta = |delta: &ModelStreamingDelta| {
        let accumulated_thinking = delta.thinking.as_str();
        if accumulated_thinking.len() > last_thinking_len.get() {
            last_thinking_len.set(accumulated_thinking.len());
            {
                let mut thinking = streamed_thinking.borrow_mut();
                thinking.clear();
                thinking.push_str(accumulated_thinking);
            }
            let item = session_turn_item(
                "assistant_thinking",
                "running",
                Some("模型思考".to_string()),
                Some(accumulated_thinking.to_string()),
                Some(thinking_item_id.clone()),
            );
            upsert_session_turn_item(session_store, &request.session_id, item.clone());
            publish_session_turn_item_event(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &item,
            );
        }

        let accumulated = delta.content.as_str();
        let previous = last_content_len.get();
        if accumulated.len() == previous {
            return;
        }
        last_content_len.set(accumulated.len());
        {
            let mut content = streamed_content.borrow_mut();
            content.clear();
            content.push_str(accumulated);
        }
        let item = session_turn_item(
            "assistant_stream",
            "running",
            Some("生成回复".to_string()),
            Some(accumulated.to_string()),
            Some(stream_item_id.clone()),
        );
        upsert_session_turn_item(session_store, &request.session_id, item);
        session_store.upsert_timeline_entry(
            request.session_id.clone(),
            &stream_item_id,
            TimelineEntryKind::AssistantMessage,
            accumulated,
        );
        let delta = &accumulated[previous..];
        if delta.is_empty() {
            return;
        }
        let _ = event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-session-delta-{}", UtcMillis::now().0)),
                "task.llm.delta",
                serde_json::json!({
                    "session_id": request.session_id.to_string(),
                    "entry_id": &stream_item_id,
                    "delta": delta,
                }),
            )
            .with_context(EventContext {
                workspace_id: request.workspace_id.clone(),
                session_id: Some(request.session_id.clone()),
                ..EventContext::default()
            }),
        );
    };

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                provider: BUSINESS_MODEL_PROVIDER.to_string(),
                prompt: prompt.to_string(),
                messages: Some(messages.clone()),
                tools: tools.clone(),
                tool_choice: None,
            },
            &on_delta,
        )
        .map_err(|error| ApiError::internal_assembly("执行 session turn 失败", error))?;
    let parsed = response.parse_chat_payload();
    publish_model_usage_record(
        event_bus,
        session_store,
        settings_store,
        &request.session_id,
        &request.workspace_id,
        usage_binding,
        format!("session-turn-{round}-{}", UtcMillis::now().0),
        parsed.usage.as_ref(),
        UsageCallStatus::Success,
        None,
        None,
    );
    let streamed_content = streamed_content.into_inner();
    let streamed_thinking = streamed_thinking.into_inner();
    let final_thinking = parsed
        .thinking
        .as_ref()
        .filter(|thinking| !thinking.trim().is_empty())
        .cloned()
        .or_else(|| (!streamed_thinking.trim().is_empty()).then_some(streamed_thinking));
    if let Some(thinking) = final_thinking {
        let thinking_item = session_turn_item(
            "assistant_thinking",
            "completed",
            Some("模型思考".to_string()),
            Some(thinking),
            Some(thinking_item_id.clone()),
        );
        upsert_session_turn_item(session_store, &request.session_id, thinking_item.clone());
        publish_session_turn_item_event(
            event_bus,
            &request.session_id,
            &request.workspace_id,
            &thinking_item,
        );
    }
    if !streamed_content.trim().is_empty() {
        let stream_item = session_turn_item(
            "assistant_stream",
            "completed",
            Some("生成回复".to_string()),
            Some(streamed_content.clone()),
            Some(stream_item_id.clone()),
        );
        upsert_session_turn_item(session_store, &request.session_id, stream_item.clone());
        publish_session_turn_item_event(
            event_bus,
            &request.session_id,
            &request.workspace_id,
            &stream_item,
        );
        let _ = event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-session-seal-{}", UtcMillis::now().0)),
                "task.llm.completed",
                serde_json::json!({
                    "session_id": request.session_id.to_string(),
                    "entry_id": &stream_item_id,
                    "response_length": streamed_content.len(),
                }),
            )
            .with_context(EventContext {
                workspace_id: request.workspace_id.clone(),
                session_id: Some(request.session_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    if request.use_tools && !parsed.tool_calls.is_empty() {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });
        for tool_call in parsed.tool_calls {
            append_session_tool_call_items(
                session_store,
                event_bus,
                tool_registry,
                skill_runtime,
                &request.session_id,
                &request.workspace_id,
                &tool_call,
                messages,
            );
        }
        return Ok(SessionTurnRoundOutput {
            final_content: None,
        });
    }

    let final_content = parsed
        .content
        .filter(|content| !content.trim().is_empty())
        .or_else(|| (!streamed_content.trim().is_empty()).then_some(streamed_content))
        .map(normalize_model_visible_content)
        .or_else(|| (!request.use_tools).then(|| "[LLM 未返回文本响应]".to_string()));

    Ok(SessionTurnRoundOutput { final_content })
}

fn append_phase_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
) {
    let phase_item = session_turn_item(
        "assistant_phase",
        "running",
        Some("理解请求".to_string()),
        Some(if request.use_tools {
            "正在理解请求并准备调用工具。".to_string()
        } else {
            "正在理解请求并生成回复。".to_string()
        }),
        None,
    );
    append_session_turn_item(session_store, &request.session_id, phase_item.clone());
    publish_session_turn_item_event(
        event_bus,
        &request.session_id,
        &request.workspace_id,
        &phase_item,
    );
}

fn append_final_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    final_content: &str,
) {
    let final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        None,
    );
    append_session_turn_item(session_store, session_id, final_item.clone());
    publish_session_turn_item_event(event_bus, session_id, workspace_id, &final_item);
    let _ = session_store.update_current_turn_status(session_id, "completed");
}
