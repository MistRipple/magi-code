//! Task System v2 — session turn execution
//!
//! 错误返回值改为 `Result<_, String>`，调用方在 magi-api 边界用
//! `.map_err(|msg| ApiError::model_invocation_failed("执行 session turn 失败", msg))`
//! 等方式桥接到 `ApiError` 枚举。

use crate::{
    prompt_utils::{
        normalize_model_stream_preview_content, normalize_model_visible_content,
        workspace_context_system_prompt,
    },
    session_writeback::{
        append_session_tool_call_items_batch, append_session_turn_error_item,
        append_session_turn_item, publish_current_session_turn_item_event,
        publish_session_turn_item_event, publish_session_turn_item_event_with_stream_update,
        session_turn_item, session_turn_stream_update, upsert_session_turn_item,
    },
    settings_store::SettingsStore,
    usage_recording::{
        ModelUsageBinding, publish_model_usage_record, session_turn_model_usage_binding,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolChoice, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest,
    ModelStreamingDelta,
};
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{CanonicalTurnItemKind, SessionStore};
use magi_snapshot::SnapshotManager;
use magi_tool_runtime::ToolRegistry;
use magi_usage_authority::UsageCallStatus;
use std::{path::PathBuf, sync::Arc};

const BASE_TOOL_CALL_ROUNDS: usize = 16;
const MAX_TOOL_CALL_ROUNDS: usize = 32;
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
    pub forced_tool_name: Option<String>,
    pub required_tool_chain: Vec<String>,
    pub workspace_root_path: Option<String>,
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
    // 主线历史只取属于 orchestrator thread 的 item；非主线（task 详情）item
    // 不进入 LLM 上下文。session 一生一 mission，因此 thread 必存。
    let orchestrator_thread_id = session_store
        .orchestrator_thread_for_session(&request.session_id)
        .map(|thread| thread.thread_id);
    let mut history = accepted_at
        .zip(orchestrator_thread_id.as_ref())
        .map(|(accepted_at, orchestrator_thread_id)| {
            session_store
                .canonical_turns_for_session(&request.session_id)
                .into_iter()
                .filter(|turn| {
                    turn.turn_id != request.turn_id && turn.accepted_at.0 < accepted_at.0
                })
                .flat_map(|turn| turn.items.into_iter())
                .filter_map(|item| {
                    let role = match item.kind {
                        CanonicalTurnItemKind::UserMessage => "user",
                        CanonicalTurnItemKind::AssistantText => "assistant",
                        _ => return None,
                    };
                    if !item.visibility.renderable
                        || &item.source_thread_id != orchestrator_thread_id
                    {
                        return None;
                    }
                    let content = item.content?.trim().to_string();
                    if content.is_empty() {
                        return None;
                    }
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
    let mut messages = workspace_context_messages(request);
    messages.append(&mut history);
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.to_string()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    messages
}

fn workspace_context_messages(request: &SessionTurnExecutionRequest) -> Vec<ChatMessage> {
    let Some(root_path) = request
        .workspace_root_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };

    vec![ChatMessage {
        role: "system".to_string(),
        content: Some(workspace_context_system_prompt(root_path)),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }]
}

pub struct SessionTurnExecutionRuntime<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub snapshot_manager: Option<&'a Arc<SnapshotManager>>,
    pub request: SessionTurnExecutionRequest,
    pub prompt: String,
    pub tools: Option<Vec<ChatToolDefinition>>,
}

pub fn run_session_turn_execution(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, String> {
    let SessionTurnExecutionRuntime {
        client,
        event_bus,
        session_store,
        settings_store,
        tool_registry,
        skill_runtime,
        snapshot_manager,
        request,
        prompt,
        tools,
    } = runtime;

    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }

    // session 一生一 mission：session turn 执行必须在已注册的 orchestrator thread 上。
    let orchestrator_thread_id = session_store
        .orchestrator_thread_for_session(&request.session_id)
        .map(|thread| thread.thread_id)
        .ok_or_else(|| {
            "orchestrator thread 缺失：session 必须先经历 ensure_session_mission".to_string()
        })?;

    let mut messages = build_session_turn_messages(session_store, &request, &prompt);
    let mut final_content: Option<String> = None;
    let mut final_item_id: Option<String> = None;
    let mut main_timeline_entry_id: Option<String> = None;
    let mut had_tool_calls = false;
    let mut completed_required_tool_names: Vec<String> = Vec::new();
    let usage_binding = session_turn_model_usage_binding(request.use_tools);

    let tool_call_round_limit = tool_call_round_limit(&request.required_tool_chain);
    for round in 0..tool_call_round_limit {
        let streamed_content = match stream_session_turn_round(
            SessionTurnRoundRuntime {
                client,
                event_bus,
                session_store,
                settings_store,
                snapshot_manager,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &prompt,
                tools: tools.clone(),
                messages: &mut messages,
                completed_required_tool_names: &completed_required_tool_names,
                round,
                orchestrator_thread_id: &orchestrator_thread_id,
            },
            tool_registry,
            skill_runtime,
        ) {
            Ok(output) => output,
            Err(error) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                append_session_turn_error_item(
                    event_bus,
                    session_store,
                    &request.session_id,
                    &request.workspace_id,
                    None,
                    request.request_id.as_deref(),
                    request.user_message_id.as_deref(),
                    request.placeholder_message_id.as_deref(),
                    &error,
                    main_timeline_entry_id.as_deref(),
                    orchestrator_thread_id.clone(),
                );
                return Err(error);
            }
        };
        if streamed_content.interrupted || !request_turn_is_writable(session_store, &request) {
            return Ok(SessionTurnExecutionOutput::interrupted());
        }
        if main_timeline_entry_id.is_none() {
            main_timeline_entry_id = streamed_content.timeline_entry_id.clone();
        }
        had_tool_calls |= streamed_content.encountered_tool_calls;
        record_completed_required_tools(
            &mut completed_required_tool_names,
            &request.required_tool_chain,
            &streamed_content.tool_call_names,
        );

        if let Some(content) = streamed_content.final_content {
            if !required_tool_chain_is_complete(
                &request.required_tool_chain,
                &completed_required_tool_names,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(required_tool_chain_recovery_prompt(
                        &request.required_tool_chain,
                        &completed_required_tool_names,
                    )),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                continue;
            }
            final_item_id = streamed_content.final_item_id;
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
            main_timeline_entry_id.as_deref(),
            orchestrator_thread_id.clone(),
        );
        return Err(failure_reason.to_string());
    };
    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }
    append_final_item(
        event_bus,
        session_store,
        &request,
        &final_content,
        final_item_id.as_deref(),
        main_timeline_entry_id.as_deref(),
        &orchestrator_thread_id,
    );

    Ok(SessionTurnExecutionOutput::completed(final_content))
}

struct SessionTurnRoundRuntime<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    settings_store: Option<&'a Arc<SettingsStore>>,
    snapshot_manager: Option<&'a Arc<SnapshotManager>>,
    request: &'a SessionTurnExecutionRequest,
    usage_binding: &'a ModelUsageBinding,
    prompt: &'a str,
    tools: Option<Vec<ChatToolDefinition>>,
    messages: &'a mut Vec<ChatMessage>,
    completed_required_tool_names: &'a [String],
    round: usize,
    /// session 主线 thread：该 turn 内所有 session_turn_item 的 source_thread_id。
    orchestrator_thread_id: &'a magi_core::ThreadId,
}

struct SessionTurnRoundOutput {
    final_content: Option<String>,
    final_item_id: Option<String>,
    timeline_entry_id: Option<String>,
    encountered_tool_calls: bool,
    tool_call_names: Vec<String>,
    interrupted: bool,
}

fn record_completed_required_tools(
    completed: &mut Vec<String>,
    required_tool_chain: &[String],
    tool_call_names: &[String],
) {
    for tool_name in tool_call_names {
        if !required_tool_chain
            .iter()
            .any(|required| required == tool_name)
        {
            continue;
        }
        if !completed
            .iter()
            .any(|completed_name| completed_name == tool_name)
        {
            completed.push(tool_name.clone());
        }
    }
}

fn required_tool_chain_is_complete(required_tool_chain: &[String], completed: &[String]) -> bool {
    required_tool_chain.iter().all(|required| {
        completed
            .iter()
            .any(|completed_name| completed_name == required)
    })
}

fn required_tool_chain_recovery_prompt(
    required_tool_chain: &[String],
    completed: &[String],
) -> String {
    let missing = required_tool_chain
        .iter()
        .filter(|required| {
            !completed
                .iter()
                .any(|completed_name| completed_name == *required)
        })
        .cloned()
        .collect::<Vec<_>>();
    format!(
        "上一轮提前给出了文字回复，但用户明确要求调用的内置工具链尚未完成。已完成：{}。仍需继续调用：{}。请继续调用下一个缺失工具，不要总结。",
        if completed.is_empty() {
            "无".to_string()
        } else {
            completed.join(", ")
        },
        missing.join(", ")
    )
}

fn tool_call_round_limit(required_tool_chain: &[String]) -> usize {
    BASE_TOOL_CALL_ROUNDS
        .max(required_tool_chain.len().saturating_add(2))
        .min(MAX_TOOL_CALL_ROUNDS)
}

fn stream_session_turn_round(
    runtime: SessionTurnRoundRuntime<'_>,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
) -> Result<SessionTurnRoundOutput, String> {
    let SessionTurnRoundRuntime {
        client,
        event_bus,
        session_store,
        settings_store,
        snapshot_manager,
        request,
        usage_binding,
        prompt,
        tools,
        messages,
        completed_required_tool_names,
        round,
        orchestrator_thread_id,
    } = runtime;

    let stream_item_id = if round == 0 {
        request.placeholder_message_id.clone().unwrap_or_else(|| {
            format!(
                "turn-item-assistant-stream-{}-{}",
                UtcMillis::now().0,
                round
            )
        })
    } else {
        format!(
            "turn-item-assistant-stream-{}-{}",
            UtcMillis::now().0,
            round
        )
    };
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
            let stream_update = {
                let previous = streamed_thinking.borrow();
                session_turn_stream_update(&previous, accumulated_thinking)
            };
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
                orchestrator_thread_id.clone(),
            );
            apply_request_aliases(&mut item, request);
            if let Some(published) =
                upsert_session_turn_item(session_store, &request.session_id, item)
            {
                publish_session_turn_item_event_with_stream_update(
                    event_bus,
                    &request.session_id,
                    &request.workspace_id,
                    &published,
                    stream_update.as_ref(),
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
        let stream_update = {
            let current_visible = streamed_visible_content.borrow();
            let update = session_turn_stream_update(&current_visible, &visible_content);
            if update.is_none() {
                return;
            }
            update
        };
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
            orchestrator_thread_id.clone(),
        );
        apply_request_aliases(&mut item, request);
        if let Some(published) = upsert_session_turn_item(session_store, &request.session_id, item)
        {
            publish_session_turn_item_event_with_stream_update(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &published,
                stream_update.as_ref(),
            );
        }
    };

    let tool_choice = forced_tool_choice_for_round(
        request,
        tools.as_ref(),
        round,
        completed_required_tool_names,
    );
    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                provider: BUSINESS_MODEL_PROVIDER.to_string(),
                prompt: prompt.to_string(),
                messages: Some(messages.clone()),
                tools: tools.clone(),
                tool_choice,
            },
            &on_delta,
        )
        .map_err(|error| error.to_string())?;
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
    let timeline_entry_id = None;
    if writeback_aborted.get() || !request_turn_is_writable(session_store, request) {
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            final_item_id: None,
            timeline_entry_id: timeline_entry_id.clone(),
            encountered_tool_calls: false,
            tool_call_names: Vec::new(),
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
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                interrupted: true,
            });
        }
        let mut thinking_item = session_turn_item(
            "assistant_thinking",
            "completed",
            Some("模型思考".to_string()),
            Some(thinking),
            Some(thinking_item_id.clone()),
            orchestrator_thread_id.clone(),
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
    let parsed_visible_content = parsed
        .content
        .as_deref()
        .map(|content| normalize_model_visible_content(content.to_string()))
        .filter(|content| !content.trim().is_empty());
    let completed_stream_content = if !streamed_visible_content.trim().is_empty() {
        Some(streamed_visible_content.clone())
    } else {
        parsed_visible_content.clone()
    };
    if let Some(completed_stream_content) = completed_stream_content.as_ref() {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                interrupted: true,
            });
        }
        let mut stream_item = session_turn_item(
            "assistant_stream",
            "completed",
            Some("生成回复".to_string()),
            Some(completed_stream_content.clone()),
            Some(stream_item_id.clone()),
            orchestrator_thread_id.clone(),
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
    // 历史上这里还有一个 `else if round == 0 && placeholder_id == stream_item_id` 分支，
    // 用于把 accept 阶段预占的空 placeholder 显式 retire 成 completed。现在 sessions.rs
    // 不再预占 placeholder（只在首个 text delta 时按 max+1 自然分配 item_seq），无 item
    // 需要 retire——空回复直接在 canonical turn 里留白即可。

    if request.use_tools && !parsed.tool_calls.is_empty() {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                interrupted: true,
            });
        }
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });
        let snapshot_session =
            snapshot_manager.and_then(|mgr| mgr.get_session(request.session_id.as_str()));
        let execution_group_id = session_store
            .execution_ownership(&request.session_id)
            .and_then(|ownership| ownership.mission_id)
            .map(|mid| mid.to_string())
            .unwrap_or_else(|| format!("session:{}", request.session_id));
        append_session_tool_call_items_batch(
            session_store,
            event_bus,
            tool_registry,
            skill_runtime,
            &request.session_id,
            &request.workspace_id,
            request.workspace_root_path.as_deref().map(PathBuf::from),
            &parsed.tool_calls,
            messages,
            snapshot_session,
            Some(execution_group_id),
            orchestrator_thread_id,
            || request_turn_is_writable(session_store, request),
        );
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                interrupted: true,
            });
        }
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            final_item_id: None,
            timeline_entry_id: timeline_entry_id.clone(),
            encountered_tool_calls: true,
            tool_call_names: parsed
                .tool_calls
                .iter()
                .map(|call| call.function.name.clone())
                .collect(),
            interrupted: false,
        });
    }

    let final_content = parsed
        .content
        .clone()
        .filter(|content| !content.trim().is_empty())
        .or_else(|| (!streamed_content.trim().is_empty()).then_some(streamed_content))
        .map(normalize_model_visible_content)
        .filter(|content| !content.trim().is_empty());

    let final_item_id = final_content
        .as_ref()
        .and_then(|_| completed_stream_content.map(|_| stream_item_id));

    Ok(SessionTurnRoundOutput {
        final_content,
        final_item_id,
        timeline_entry_id,
        encountered_tool_calls: false,
        tool_call_names: Vec::new(),
        interrupted: false,
    })
}

fn forced_tool_choice_for_round(
    request: &SessionTurnExecutionRequest,
    tools: Option<&Vec<ChatToolDefinition>>,
    round: usize,
    completed_required_tool_names: &[String],
) -> Option<ChatToolChoice> {
    if !request.use_tools {
        return None;
    }
    let forced_tool_name = request
        .required_tool_chain
        .iter()
        .find(|tool_name| {
            !completed_required_tool_names
                .iter()
                .any(|completed| completed == *tool_name)
        })
        .map(String::as_str)
        .or_else(|| {
            (round == 0)
                .then(|| request.forced_tool_name.as_deref())
                .flatten()
        })?
        .trim();
    if forced_tool_name.is_empty() {
        return None;
    }
    let tool_is_available = tools
        .map(|definitions| {
            definitions
                .iter()
                .any(|definition| definition.function.name == forced_tool_name)
        })
        .unwrap_or(false);
    tool_is_available.then(|| ChatToolChoice::force_function(forced_tool_name))
}

fn append_final_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    final_content: &str,
    final_item_id: Option<&str>,
    timeline_entry_id: Option<&str>,
    orchestrator_thread_id: &magi_core::ThreadId,
) {
    let has_requested_final_item_id = final_item_id.is_some();
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        final_item_id.map(str::to_string),
        orchestrator_thread_id.clone(),
    );
    if let Some(timeline_entry_id) = timeline_entry_id {
        final_item.timeline_entry_id = Some(timeline_entry_id.to_string());
    }
    apply_request_aliases(&mut final_item, request);
    let final_item_id = final_item.item_id.clone();
    if has_requested_final_item_id {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{BridgeClientError, BridgeResponse};
    use magi_core::SessionLifecycleStatus;
    use magi_session_store::{
        ActiveExecutionTurn, CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind,
        CanonicalTurnItemStatus, CanonicalTurnStatus, CanonicalTurnVisibility, ExecutionThread,
        ExecutionThreadStatus, ORCHESTRATOR_ROLE_ID, SessionRecord, SessionStoreState,
        TimelineEntry, TimelineEntryKind,
    };
    use std::collections::HashMap;

    fn ts(value: u64) -> UtcMillis {
        UtcMillis(value)
    }

    struct StreamingTextModelBridgeClient {
        delta_content: String,
        payload: String,
    }

    impl ModelBridgeClient for StreamingTextModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: self.payload.clone(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: self.delta_content.clone(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    #[test]
    fn forced_tool_choice_only_applies_to_available_first_round_tool() {
        let request = SessionTurnExecutionRequest {
            session_id: SessionId::new("session-force-tool-choice"),
            turn_id: "turn-force-tool-choice".to_string(),
            workspace_id: None,
            prompt: "画一个流程图".to_string(),
            use_tools: true,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: Some("diagram_render".to_string()),
            required_tool_chain: Vec::new(),
            workspace_root_path: None,
        };
        let tools = vec![ChatToolDefinition {
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunctionDefinition {
                name: "diagram_render".to_string(),
                description: "render diagram".to_string(),
                parameters: serde_json::json!({ "type": "object" }),
            },
        }];

        let choice = forced_tool_choice_for_round(&request, Some(&tools), 0, &[])
            .expect("first round should force diagram_render");
        assert_eq!(choice.function.name, "diagram_render");
        assert!(forced_tool_choice_for_round(&request, Some(&tools), 1, &[]).is_none());

        let mut unavailable_request = request;
        unavailable_request.forced_tool_name = Some("missing_tool".to_string());
        assert!(forced_tool_choice_for_round(&unavailable_request, Some(&tools), 0, &[]).is_none());
    }

    #[test]
    fn required_tool_chain_forces_next_missing_tool_each_round() {
        let request = SessionTurnExecutionRequest {
            session_id: SessionId::new("session-required-tool-chain"),
            turn_id: "turn-required-tool-chain".to_string(),
            workspace_id: None,
            prompt: "依次调用 shell_exec、file_write、file_read".to_string(),
            use_tools: true,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: vec![
                "shell_exec".to_string(),
                "file_write".to_string(),
                "file_read".to_string(),
            ],
            workspace_root_path: None,
        };
        let tools = ["shell_exec", "file_write", "file_read"]
            .into_iter()
            .map(|name| ChatToolDefinition {
                kind: "function".to_string(),
                function: magi_bridge_client::ChatToolFunctionDefinition {
                    name: name.to_string(),
                    description: format!("{name} tool"),
                    parameters: serde_json::json!({ "type": "object" }),
                },
            })
            .collect::<Vec<_>>();

        let first = forced_tool_choice_for_round(&request, Some(&tools), 0, &[])
            .expect("first missing tool should be forced");
        assert_eq!(first.function.name, "shell_exec");
        let second =
            forced_tool_choice_for_round(&request, Some(&tools), 1, &["shell_exec".to_string()])
                .expect("second missing tool should be forced");
        assert_eq!(second.function.name, "file_write");
        let third = forced_tool_choice_for_round(
            &request,
            Some(&tools),
            2,
            &["shell_exec".to_string(), "file_write".to_string()],
        )
        .expect("third missing tool should be forced");
        assert_eq!(third.function.name, "file_read");
        assert!(
            forced_tool_choice_for_round(
                &request,
                Some(&tools),
                3,
                &[
                    "shell_exec".to_string(),
                    "file_write".to_string(),
                    "file_read".to_string()
                ],
            )
            .is_none()
        );
    }

    #[test]
    fn tool_call_round_limit_keeps_final_round_after_explicit_chain() {
        let required_tool_chain = [
            "file_mkdir",
            "file_write",
            "file_read",
            "file_patch",
            "search_text",
            "shell_exec",
            "diff_preview",
            "diagram_render",
            "file_remove",
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

        assert!(
            tool_call_round_limit(&required_tool_chain) >= required_tool_chain.len() + 2,
            "显式工具链需要为每个工具调用轮和最终回复轮预留空间"
        );
    }

    #[test]
    fn stream_session_turn_round_reuses_accepted_assistant_placeholder() {
        // 验证流式首段 assistant text 用 request.placeholder_message_id 作为 item_id。
        // 历史方案曾在 accept 阶段把 placeholder 以 item_seq=2 预占进 turn.items，
        // 现在不再预占——首个 text delta 走 upsert，按 max(item_seq)+1=2 自然创建。
        let session_id = SessionId::new("session-placeholder-reuse");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "placeholder reuse")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(900), || {
                magi_core::MissionId::new("mission-placeholder-reuse")
            });
        let mut user_item = session_turn_item(
            "user_message",
            "completed",
            None,
            Some("请只回复一句话".to_string()),
            Some("user-placeholder-reuse".to_string()),
            orchestrator_thread_id.clone(),
        );
        user_item.item_seq = 1;
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-placeholder-reuse".to_string(),
                    turn_seq: 1000,
                    accepted_at: ts(1000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请只回复一句话".to_string()),
                    items: vec![user_item],
                },
            )
            .expect("current turn should be stored");
        let event_bus = InMemoryEventBus::new(16);
        let client = StreamingTextModelBridgeClient {
            delta_content: "你好".to_string(),
            payload: serde_json::json!({ "content": "你好" }).to_string(),
        };
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-placeholder-reuse".to_string(),
            workspace_id: None,
            prompt: "请只回复一句话".to_string(),
            use_tools: false,
            skill_name: None,
            request_id: Some("request-placeholder-reuse".to_string()),
            user_message_id: Some("user-placeholder-reuse".to_string()),
            placeholder_message_id: Some("assistant-placeholder-reuse".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            workspace_root_path: None,
        };
        let usage_binding = session_turn_model_usage_binding(false);
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(request.prompt.clone()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];

        let output = stream_session_turn_round(
            SessionTurnRoundRuntime {
                client: &client,
                event_bus: &event_bus,
                session_store: &store,
                settings_store: None,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &request.prompt,
                tools: None,
                messages: &mut messages,
                completed_required_tool_names: &[],
                snapshot_manager: None,
                round: 0,
                orchestrator_thread_id: &orchestrator_thread_id,
            },
            None,
            None,
        )
        .expect("round should stream");

        assert_eq!(
            output.final_item_id.as_deref(),
            Some("assistant-placeholder-reuse"),
            "首段 assistant 文本必须以 request.placeholder_message_id 作为 item_id"
        );
        let canonical_turn = store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-placeholder-reuse")
            .expect("canonical turn should be stored");
        let assistant_items = canonical_turn
            .items
            .iter()
            .filter(|item| item.kind == CanonicalTurnItemKind::AssistantText)
            .collect::<Vec<_>>();
        assert_eq!(
            assistant_items.len(),
            1,
            "流式正文不能新增第二条 assistant item"
        );
        assert_eq!(assistant_items[0].item_id, "assistant-placeholder-reuse");
        // accept 阶段只写 user_message(seq=1)；流式正文是首个新 item，拿到 max+1=2。
        // 同 round 内的 thinking 即便后到（非增量 reasoning provider 在 post-streaming
        // 才补 item），由 projection 层按 kind 重排为 thinking → text，不依赖 item_seq。
        assert_eq!(
            assistant_items[0].item_seq, 2,
            "stream text 是首个 assistant item，应分到 item_seq=2（user=1, text=2）"
        );
        assert_eq!(
            assistant_items[0].status,
            CanonicalTurnItemStatus::Completed
        );
        assert_eq!(assistant_items[0].content.as_deref(), Some("你好"));
    }

    #[test]
    fn session_turn_messages_include_persisted_history_before_current_turn() {
        let session_id = SessionId::new("session-context-history");
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
                    message: "timeline snapshot 不应作为模型上下文事实源".to_string(),
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
            canonical_turns: vec![CanonicalTurn {
                session_id: session_id.clone(),
                turn_id: "turn-prev".to_string(),
                turn_seq: 1000,
                accepted_at: ts(1000),
                completed_at: Some(ts(1200)),
                status: CanonicalTurnStatus::Completed,
                response_duration_ms: Some(200),
                usage: None,
                items: vec![
                    CanonicalTurnItem {
                        session_id: session_id.clone(),
                        turn_id: "turn-prev".to_string(),
                        turn_seq: 1000,
                        item_id: "turn-item-prev-user".to_string(),
                        item_seq: 1,
                        kind: CanonicalTurnItemKind::UserMessage,
                        created_at: ts(1000),
                        status: CanonicalTurnItemStatus::Completed,
                        item_version: None,
                        updated_at: ts(1000),
                        title: None,
                        content: Some("请用一句话回答：2+3 等于几？".to_string()),
                        blocks: Vec::new(),
                        tool: None,
                        worker: None,
                        source_thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                        visibility: CanonicalTurnVisibility::default(),
                        metadata: HashMap::new(),
                    },
                    CanonicalTurnItem {
                        session_id: session_id.clone(),
                        turn_id: "turn-prev".to_string(),
                        turn_seq: 1000,
                        item_id: "turn-item-prev-final".to_string(),
                        item_seq: 2,
                        kind: CanonicalTurnItemKind::AssistantText,
                        created_at: ts(1200),
                        status: CanonicalTurnItemStatus::Completed,
                        item_version: None,
                        updated_at: ts(1200),
                        title: None,
                        content: Some("2+3 等于 5。".to_string()),
                        blocks: Vec::new(),
                        tool: None,
                        worker: None,
                        source_thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                        visibility: CanonicalTurnVisibility::default(),
                        metadata: HashMap::new(),
                    },
                ],
                metadata: HashMap::new(),
            }],
            notifications: Vec::new(),
            execution_sidecar_store: Default::default(),
            thread_registry: vec![ExecutionThread {
                thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-context-history"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-orchestrator-test"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1200),
                handled_task_ids: Vec::new(),
                message_history: Vec::new(),
            }],
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
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            workspace_root_path: None,
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
    fn build_session_turn_messages_injects_workspace_context() {
        let session_id = SessionId::new("session-workspace-context");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "workspace context")
            .expect("session should be created");
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-workspace-context".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("分析一下当前项目".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-workspace-context".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context")),
            prompt: "分析一下当前项目".to_string(),
            use_tools: true,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            workspace_root_path: Some("/tmp/current-project".to_string()),
        };

        let messages = build_session_turn_messages(&store, &request, &request.prompt);

        assert_eq!(messages[0].role, "system");
        let context = messages[0].content.as_deref().unwrap_or_default();
        assert!(context.contains("/tmp/current-project"));
        assert!(context.contains("不要要求用户手动粘贴项目结构"));
        assert_eq!(
            messages
                .last()
                .and_then(|message| message.content.as_deref()),
            Some("分析一下当前项目")
        );
    }

    #[test]
    fn append_final_item_keeps_post_tool_assistant_item_separate_from_main_timeline_entry() {
        let session_id = SessionId::new("session-post-tool-final-item");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "post tool final")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(1000), || {
                magi_core::MissionId::new("mission-post-tool-final-item")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-post-tool-final-item".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请调用工具后回答".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let event_bus = InMemoryEventBus::new(16);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-post-tool-final-item".to_string(),
            workspace_id: None,
            prompt: "请调用工具后回答".to_string(),
            use_tools: true,
            skill_name: None,
            request_id: Some("request-post-tool-final-item".to_string()),
            user_message_id: Some("user-post-tool-final-item".to_string()),
            placeholder_message_id: Some("placeholder-post-tool-final-item".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            workspace_root_path: None,
        };

        let mut pre_tool_stream = session_turn_item(
            "assistant_stream",
            "completed",
            Some("生成回复".to_string()),
            Some("我先检查工具结果。".to_string()),
            Some("turn-item-assistant-stream-main".to_string()),
            orchestrator_thread_id.clone(),
        );
        pre_tool_stream.timeline_entry_id = Some("turn-item-assistant-stream-main".to_string());
        append_session_turn_item(&store, &session_id, pre_tool_stream)
            .expect("pre-tool stream item should be stored");
        let post_tool_stream = session_turn_item(
            "assistant_stream",
            "completed",
            Some("生成回复".to_string()),
            Some("工具结果显示可以继续。".to_string()),
            Some("turn-item-assistant-stream-post-tool".to_string()),
            orchestrator_thread_id.clone(),
        );
        append_session_turn_item(&store, &session_id, post_tool_stream)
            .expect("post-tool stream item should be stored");

        append_final_item(
            &event_bus,
            &store,
            &request,
            "最终答案来自工具后轮次。",
            Some("turn-item-assistant-stream-post-tool"),
            Some("turn-item-assistant-stream-main"),
            &orchestrator_thread_id,
        );

        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain available");
        let pre_tool_item = turn
            .items
            .iter()
            .find(|item| item.item_id == "turn-item-assistant-stream-main")
            .expect("pre-tool item should remain stored");
        assert_eq!(pre_tool_item.kind, "assistant_stream");
        assert_eq!(pre_tool_item.content.as_deref(), Some("我先检查工具结果。"));
        let post_tool_item = turn
            .items
            .iter()
            .find(|item| item.item_id == "turn-item-assistant-stream-post-tool")
            .expect("post-tool item should become final item");
        assert_eq!(post_tool_item.kind, "assistant_final");
        assert_eq!(
            post_tool_item.timeline_entry_id.as_deref(),
            Some("turn-item-assistant-stream-main")
        );
        assert_eq!(
            post_tool_item.content.as_deref(),
            Some("最终答案来自工具后轮次。")
        );
        let canonical_turn = store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-post-tool-final-item")
            .expect("canonical turn should be stored");
        let canonical_post_tool_item = canonical_turn
            .items
            .iter()
            .find(|item| item.item_id == "turn-item-assistant-stream-post-tool")
            .expect("post-tool canonical assistant item should remain stored");
        assert_eq!(
            canonical_post_tool_item.kind,
            CanonicalTurnItemKind::AssistantText
        );
        assert_eq!(
            canonical_post_tool_item.status,
            CanonicalTurnItemStatus::Completed
        );
        assert_eq!(
            canonical_post_tool_item.content.as_deref(),
            Some("最终答案来自工具后轮次。")
        );
        assert!(
            store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| !entry.message.contains("最终答案来自工具后轮次。")),
            "完成态不能再反向写 completed snapshot 作为正文事实源"
        );
    }

    #[test]
    fn append_final_item_publishes_terminal_duration_from_backend_turn() {
        let session_id = SessionId::new("session-terminal-duration");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "terminal duration")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(1000), || {
                magi_core::MissionId::new("mission-terminal-duration")
            });
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
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            workspace_root_path: None,
        };

        append_final_item(
            &event_bus,
            &store,
            &request,
            "最终回复",
            None,
            None,
            &orchestrator_thread_id,
        );

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
                .canonical_turns_for_session(&session_id)
                .iter()
                .any(|turn| turn.turn_id == "turn-terminal-duration"
                    && turn.response_duration_ms.is_some()
                    && turn.status == CanonicalTurnStatus::Completed),
            "持久 canonical turn 必须携带后端完成耗时"
        );
    }
}
