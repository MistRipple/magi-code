use crate::{
    errors::ApiError,
    prompt_utils::{normalize_model_stream_preview_content, normalize_model_visible_content},
    session_turn_writeback::{
        append_session_tool_call_items_batch, append_session_turn_error_item,
        append_session_turn_item, build_completed_turn_timeline_snapshot,
        publish_current_session_turn_item_event, publish_session_turn_item_event,
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
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{SessionStore, TimelineEntryKind, timeline_entry_visible_text};
use magi_tool_runtime::ToolRegistry;
use magi_usage_authority::UsageCallStatus;
use std::sync::Arc;

const MAX_TOOL_CALL_ROUNDS: usize = 8;
const MAX_SESSION_CONTEXT_MESSAGES: usize = 12;
pub const BUSINESS_MODEL_PROVIDER: &str = "openai-compatible";

pub struct SessionTurnExecutionRequest {
    pub session_id: SessionId,
    pub turn_id: String,
    pub workspace_id: Option<WorkspaceId>,
    pub prompt: String,
    pub use_tools: bool,
    pub skill_name: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
}

pub struct SessionTurnExecutionOutput {
    pub final_content: String,
    pub interrupted: bool,
}

impl SessionTurnExecutionOutput {
    fn completed(final_content: String) -> Self {
        Self {
            final_content,
            interrupted: false,
        }
    }

    fn interrupted() -> Self {
        Self {
            final_content: String::new(),
            interrupted: true,
        }
    }
}

fn apply_request_aliases(
    item: &mut magi_session_store::ActiveExecutionTurnItem,
    request: &SessionTurnExecutionRequest,
) {
    item.request_id = request.request_id.clone();
    item.user_message_id = request.user_message_id.clone();
    item.placeholder_message_id = request.placeholder_message_id.clone();
}

fn current_turn_status_is_writable(status: &str) -> bool {
    !matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "cancelled"
            | "canceled"
    )
}

fn request_turn_is_writable(
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
) -> bool {
    session_store
        .runtime_sidecar(&request.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| {
            turn.turn_id == request.turn_id && current_turn_status_is_writable(&turn.status)
        })
}

fn build_session_turn_messages(
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    prompt: &str,
) -> Vec<ChatMessage> {
    let current_turn = session_store
        .runtime_sidecar(&request.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .filter(|turn| turn.turn_id == request.turn_id);
    let accepted_at = current_turn.as_ref().map(|turn| turn.accepted_at);
    let current_user_timeline_entry_id = current_turn.as_ref().and_then(|turn| {
        turn.items
            .iter()
            .find(|item| item.kind == "user_message")
            .and_then(|item| item.timeline_entry_id.clone())
    });
    let mut history = accepted_at
        .map(|accepted_at| {
            session_store
                .timeline_for_session(&request.session_id)
                .into_iter()
                .filter(|entry| {
                    if let Some(current_user_timeline_entry_id) =
                        current_user_timeline_entry_id.as_deref()
                    {
                        entry.entry_id != current_user_timeline_entry_id
                            && entry.occurred_at.0 <= accepted_at.0
                    } else {
                        entry.occurred_at.0 < accepted_at.0
                    }
                })
                .filter_map(|entry| {
                    let role = match entry.kind {
                        TimelineEntryKind::UserMessage => "user",
                        TimelineEntryKind::AssistantMessage => "assistant",
                        _ => return None,
                    };
                    let content = timeline_entry_visible_text(&entry.message)?;
                    Some(ChatMessage {
                        role: role.to_string(),
                        content: Some(content),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if history.len() > MAX_SESSION_CONTEXT_MESSAGES {
        history = history.split_off(history.len() - MAX_SESSION_CONTEXT_MESSAGES);
    }
    history.push(ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.to_string()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    history
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

    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }

    append_phase_item(event_bus, session_store, &request);
    let mut messages = build_session_turn_messages(session_store, &request, &prompt);
    let mut final_content: Option<String> = None;
    let mut last_streaming_entry_id: Option<String> = None;
    let mut had_tool_calls = false;
    let usage_binding = session_turn_model_usage_binding(request.use_tools);

    for round in 0..MAX_TOOL_CALL_ROUNDS {
        let streamed_content = match stream_session_turn_round(
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
        ) {
            Ok(output) => output,
            Err(error) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let error_text = session_turn_failure_text(&error);
                append_session_turn_error_item(
                    event_bus,
                    session_store,
                    &request.session_id,
                    &request.workspace_id,
                    None,
                    request.request_id.as_deref(),
                    request.user_message_id.as_deref(),
                    request.placeholder_message_id.as_deref(),
                    &error_text,
                    last_streaming_entry_id.as_deref(),
                );
                return Err(error);
            }
        };
        if streamed_content.interrupted || !request_turn_is_writable(session_store, &request) {
            return Ok(SessionTurnExecutionOutput::interrupted());
        }
        last_streaming_entry_id = streamed_content.streaming_entry_id.clone();
        had_tool_calls |= streamed_content.encountered_tool_calls;

        if let Some(content) = streamed_content.final_content {
            final_content = Some(content);
            break;
        }
    }

    let final_content = if let Some(content) = final_content {
        content
    } else {
        if !request_turn_is_writable(session_store, &request) {
            return Ok(SessionTurnExecutionOutput::interrupted());
        }
        let failure_reason = if had_tool_calls {
            "模型在工具调用后未返回最终回复"
        } else {
            "模型未返回可显示回复"
        };
        append_session_turn_error_item(
            event_bus,
            session_store,
            &request.session_id,
            &request.workspace_id,
            None,
            request.request_id.as_deref(),
            request.user_message_id.as_deref(),
            request.placeholder_message_id.as_deref(),
            failure_reason,
            last_streaming_entry_id.as_deref(),
        );
        return Err(ApiError::model_invocation_failed(
            "执行 session turn 失败",
            failure_reason,
        ));
    };
    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }
    append_final_item(
        event_bus,
        session_store,
        &request,
        &final_content,
        last_streaming_entry_id.as_deref(),
    );

    Ok(SessionTurnExecutionOutput::completed(final_content))
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
    streaming_entry_id: Option<String>,
    encountered_tool_calls: bool,
    interrupted: bool,
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
    let streamed_visible_content = std::cell::RefCell::new(String::new());
    let last_content_len = std::cell::Cell::new(0usize);
    let last_thinking_len = std::cell::Cell::new(0usize);
    let writeback_aborted = std::cell::Cell::new(false);
    let on_delta = |delta: &ModelStreamingDelta| {
        if !request_turn_is_writable(session_store, request) {
            writeback_aborted.set(true);
            return;
        }
        let accumulated_thinking = delta.thinking.as_str();
        if accumulated_thinking.len() > last_thinking_len.get() {
            last_thinking_len.set(accumulated_thinking.len());
            {
                let mut thinking = streamed_thinking.borrow_mut();
                thinking.clear();
                thinking.push_str(accumulated_thinking);
            }
            let mut item = session_turn_item(
                "assistant_thinking",
                "running",
                Some("模型思考".to_string()),
                Some(accumulated_thinking.to_string()),
                Some(thinking_item_id.clone()),
            );
            apply_request_aliases(&mut item, request);
            if let Some(published) =
                upsert_session_turn_item(session_store, &request.session_id, item)
            {
                publish_session_turn_item_event(
                    event_bus,
                    &request.session_id,
                    &request.workspace_id,
                    &published,
                );
            }
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
        let visible_content = normalize_model_stream_preview_content(accumulated);
        {
            let current_visible = streamed_visible_content.borrow();
            if *current_visible == visible_content {
                return;
            }
        }
        {
            let mut current_visible = streamed_visible_content.borrow_mut();
            current_visible.clear();
            current_visible.push_str(&visible_content);
        }
        if visible_content.trim().is_empty() {
            return;
        }
        let mut item = session_turn_item(
            "assistant_stream",
            "running",
            Some("生成回复".to_string()),
            Some(visible_content.clone()),
            Some(stream_item_id.clone()),
        );
        apply_request_aliases(&mut item, request);
        if let Some(published) = upsert_session_turn_item(session_store, &request.session_id, item)
        {
            publish_session_turn_item_event(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &published,
            );
        }
        session_store.upsert_timeline_entry(
            request.session_id.clone(),
            &stream_item_id,
            TimelineEntryKind::AssistantMessage,
            visible_content,
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
        .map_err(|error| ApiError::model_invocation_failed("执行 session turn 失败", error))?;
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
    if writeback_aborted.get() || !request_turn_is_writable(session_store, request) {
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            streaming_entry_id: Some(stream_item_id),
            encountered_tool_calls: false,
            interrupted: true,
        });
    }
    let streamed_content = streamed_content.into_inner();
    let streamed_thinking = streamed_thinking.into_inner();
    let streamed_visible_content = streamed_visible_content.into_inner();
    let final_thinking = parsed
        .thinking
        .as_ref()
        .filter(|thinking| !thinking.trim().is_empty())
        .cloned()
        .or_else(|| (!streamed_thinking.trim().is_empty()).then_some(streamed_thinking));
    if let Some(thinking) = final_thinking {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                streaming_entry_id: Some(stream_item_id),
                encountered_tool_calls: false,
                interrupted: true,
            });
        }
        let mut thinking_item = session_turn_item(
            "assistant_thinking",
            "completed",
            Some("模型思考".to_string()),
            Some(thinking),
            Some(thinking_item_id.clone()),
        );
        apply_request_aliases(&mut thinking_item, request);
        if let Some(published) =
            upsert_session_turn_item(session_store, &request.session_id, thinking_item)
        {
            publish_session_turn_item_event(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &published,
            );
        }
    }
    if !streamed_visible_content.trim().is_empty() {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                streaming_entry_id: Some(stream_item_id),
                encountered_tool_calls: false,
                interrupted: true,
            });
        }
        let mut stream_item = session_turn_item(
            "assistant_stream",
            "completed",
            Some("生成回复".to_string()),
            Some(streamed_visible_content.clone()),
            Some(stream_item_id.clone()),
        );
        apply_request_aliases(&mut stream_item, request);
        if let Some(published) =
            upsert_session_turn_item(session_store, &request.session_id, stream_item)
        {
            publish_session_turn_item_event(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &published,
            );
        }
    }

    if request.use_tools && !parsed.tool_calls.is_empty() {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                streaming_entry_id: Some(stream_item_id),
                encountered_tool_calls: false,
                interrupted: true,
            });
        }
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });
        append_session_tool_call_items_batch(
            session_store,
            event_bus,
            tool_registry,
            skill_runtime,
            &request.session_id,
            &request.workspace_id,
            &parsed.tool_calls,
            messages,
            || request_turn_is_writable(session_store, request),
        );
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                streaming_entry_id: Some(stream_item_id),
                encountered_tool_calls: false,
                interrupted: true,
            });
        }
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            streaming_entry_id: Some(stream_item_id),
            encountered_tool_calls: true,
            interrupted: false,
        });
    }

    let final_content = parsed
        .content
        .filter(|content| !content.trim().is_empty())
        .or_else(|| (!streamed_content.trim().is_empty()).then_some(streamed_content))
        .map(normalize_model_visible_content)
        .filter(|content| !content.trim().is_empty());

    Ok(SessionTurnRoundOutput {
        final_content,
        streaming_entry_id: Some(stream_item_id),
        encountered_tool_calls: false,
        interrupted: false,
    })
}

fn append_phase_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
) {
    let mut phase_item = session_turn_item(
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
    apply_request_aliases(&mut phase_item, request);
    if let Some(published) =
        append_session_turn_item(session_store, &request.session_id, phase_item)
    {
        publish_session_turn_item_event(
            event_bus,
            &request.session_id,
            &request.workspace_id,
            &published,
        );
    }
}

fn session_turn_failure_text(error: &ApiError) -> String {
    match error {
        ApiError::InvalidInput(message)
        | ApiError::SessionNotFound(message)
        | ApiError::RecoveryNotFound(message)
        | ApiError::NotFound(message)
        | ApiError::EventPublishFailed(message)
        | ApiError::ModelInvocationFailed(message)
        | ApiError::InternalAssemblyError(message)
        | ApiError::Conflict(message) => message.clone(),
    }
}

fn append_final_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    final_content: &str,
    streaming_entry_id: Option<&str>,
) {
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        streaming_entry_id.map(str::to_string),
    );
    apply_request_aliases(&mut final_item, request);
    let final_item_id = final_item.item_id.clone();
    if streaming_entry_id.is_some() {
        if let Some(published) =
            upsert_session_turn_item(session_store, &request.session_id, final_item)
        {
            publish_session_turn_item_event(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &published,
            );
        }
    } else if let Some(published) =
        append_session_turn_item(session_store, &request.session_id, final_item)
    {
        publish_session_turn_item_event(
            event_bus,
            &request.session_id,
            &request.workspace_id,
            &published,
        );
    }
    let _ = session_store.update_current_turn_status(&request.session_id, "completed");
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        &request.session_id,
        &request.workspace_id,
        &final_item_id,
        None,
    );
    let timeline_message = build_completed_turn_timeline_snapshot(
        session_store,
        &request.session_id,
        Some(final_content),
        streaming_entry_id,
        None,
    )
    .unwrap_or_else(|| final_content.to_string());
    let fallback_entry_id = session_store
        .runtime_sidecar(&request.session_id)
        .and_then(|sidecar| {
            sidecar.current_turn.as_ref().map(|turn| {
                format!(
                    "timeline-turn-snapshot-{}-{}",
                    &request.session_id, turn.turn_id
                )
            })
        })
        .unwrap_or_else(|| format!("timeline-turn-snapshot-{}", &request.session_id));
    let entry_id = streaming_entry_id.unwrap_or(fallback_entry_id.as_str());
    session_store.upsert_timeline_entry(
        request.session_id.clone(),
        entry_id,
        TimelineEntryKind::AssistantMessage,
        timeline_message,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::SessionLifecycleStatus;
    use magi_session_store::{
        ActiveExecutionTurn, SessionRecord, SessionStoreState, TimelineEntry,
    };

    fn ts(value: u64) -> UtcMillis {
        UtcMillis(value)
    }

    #[test]
    fn session_turn_messages_include_persisted_history_before_current_turn() {
        let session_id = SessionId::new("session-context-history");
        let assistant_snapshot = serde_json::json!({
            "is_historical_turn_snapshot": true,
            "current_turn": {
                "turn_id": "turn-prev",
                "turn_seq": 1000,
                "accepted_at": 1000,
                "completed_at": 1200,
                "response_duration_ms": 200,
                "status": "completed",
                "user_message": "请用一句话回答：2+3 等于几？"
            },
            "turn_items": [{
                "item_id": "turn-item-prev-final",
                "item_seq": 2,
                "kind": "assistant_final",
                "status": "completed",
                "source": "orchestrator",
                "content": "2+3 等于 5。"
            }]
        })
        .to_string();
        let store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "context history".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: ts(900),
                updated_at: ts(2000),
                message_count: None,
                workspace_id: None,
            }],
            timeline: vec![
                TimelineEntry {
                    entry_id: "timeline-user-prev".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::UserMessage,
                    message: "请用一句话回答：2+3 等于几？".to_string(),
                    occurred_at: ts(1000),
                },
                TimelineEntry {
                    entry_id: "timeline-assistant-prev".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::AssistantMessage,
                    message: assistant_snapshot,
                    occurred_at: ts(1200),
                },
                TimelineEntry {
                    entry_id: "timeline-user-current".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::UserMessage,
                    message: "请基于上一轮结果，用一句话回答：再加 4 等于几？".to_string(),
                    occurred_at: ts(2000),
                },
            ],
            notifications: Vec::new(),
            execution_sidecar_store: Default::default(),
        });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-session-2000".to_string(),
                    turn_seq: 2000,
                    accepted_at: ts(2000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some(
                        "请基于上一轮结果，用一句话回答：再加 4 等于几？".to_string(),
                    ),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                },
            )
            .expect("current turn should be stored");

        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-session-2000".to_string(),
            workspace_id: None,
            prompt: "请基于上一轮结果，用一句话回答：再加 4 等于几？".to_string(),
            use_tools: false,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
        };
        let messages = build_session_turn_messages(&store, &request, &request.prompt);

        assert_eq!(
            messages
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "assistant", "user"]
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.content.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec![
                "请用一句话回答：2+3 等于几？",
                "2+3 等于 5。",
                "请基于上一轮结果，用一句话回答：再加 4 等于几？",
            ]
        );
    }

    #[test]
    fn append_final_item_publishes_terminal_duration_from_backend_turn() {
        let session_id = SessionId::new("session-terminal-duration");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "terminal duration")
            .expect("session should be creatable");
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-terminal-duration".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请回答".to_string()),
                    items: Vec::new(),
                    worker_lanes: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let event_bus = InMemoryEventBus::new(16);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-terminal-duration".to_string(),
            workspace_id: None,
            prompt: "请回答".to_string(),
            use_tools: false,
            skill_name: None,
            request_id: Some("request-terminal-duration".to_string()),
            user_message_id: Some("user-terminal-duration".to_string()),
            placeholder_message_id: Some("placeholder-terminal-duration".to_string()),
        };

        append_final_item(&event_bus, &store, &request, "最终回复", None);

        let terminal_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| event.event_type == "session.turn.item")
            .expect("terminal item event should be published");
        assert_eq!(
            terminal_event.payload["current_turn"]["status"],
            "completed"
        );
        assert!(
            terminal_event.payload["current_turn"]["response_duration_ms"]
                .as_u64()
                .is_some(),
            "terminal session.turn.item 必须携带后端完成耗时"
        );
        assert!(
            store
                .timeline_for_session(&session_id)
                .iter()
                .any(
                    |entry| matches!(entry.kind, TimelineEntryKind::AssistantMessage)
                        && entry.message.contains("\"response_duration_ms\"")
                ),
            "持久 snapshot 必须携带后端完成耗时"
        );
    }
}
