//! 任务系统 — session turn execution
//!
//! 错误返回值改为 `Result<_, String>`，调用方在 magi-api 边界用
//! `.map_err(|msg| ApiError::model_invocation_failed("执行 session turn 失败", msg))`
//! 等方式桥接到 `ApiError` 枚举。

use crate::{
    ConversationRegistry, SessionTurnInputBoundary, UserSignal,
    conversation_loop::{
        compact_history_for_prompt, latest_session_usage_observation,
        thread_chat_message_to_chat_message,
    },
    model_error::{
        classify_model_invocation_error, provider_empty_assistant_response_error,
        public_model_image_invocation_error_message,
    },
    prompt_utils::{
        PromptFragmentKind, current_turn_context_priority_prompt,
        normalize_model_stream_preview_content, normalize_model_visible_content,
        system_prompt_fragment_message, workspace_context_system_prompt,
    },
    session_images::{SessionTurnImage, session_turn_image_sources},
    session_writeback::{
        SessionStatePersistCallback, SessionTurnStreamPublishGate,
        append_session_tool_call_items_batch_with_context, append_session_turn_error_item,
        append_session_turn_item, persist_session_state_checkpoint,
        publish_current_session_turn_item_event, publish_model_retry_runtime_event,
        publish_session_turn_item_event, publish_session_turn_item_stream_event, session_turn_item,
        session_turn_stream_update, upsert_session_turn_item,
    },
    tool_surface_state::{activate_skill_tool_definitions, refresh_live_mcp_tool_definitions},
    usage_recording::{
        ModelUsageBinding, account_active_goal_turn, publish_model_usage_record,
        session_turn_model_usage_binding,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolChoice, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest,
    ModelStreamingDelta,
};
use magi_core::{AccessProfile, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{CanonicalTurnItemKind, SessionStore, ThreadChatMessage};
use magi_settings_store::SettingsStore;
use magi_snapshot::SnapshotManager;
use magi_tool_runtime::ToolRegistry;
use magi_usage_authority::UsageCallStatus;
use std::{fmt, path::PathBuf, sync::Arc};

const BASE_TOOL_CALL_ROUNDS: usize = 16;
const MAX_TOOL_CALL_ROUNDS: usize = 32;
pub const BUSINESS_MODEL_PROVIDER: &str = "openai-compatible";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionGoalTurnMode {
    #[default]
    None,
    Start,
    Continuation,
}

impl SessionGoalTurnMode {
    pub fn is_goal_driven(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn allows_goal_creation(self) -> bool {
        !matches!(self, Self::Continuation)
    }
}

pub struct SessionTurnExecutionRequest {
    pub session_id: SessionId,
    pub turn_id: String,
    pub workspace_id: Option<WorkspaceId>,
    pub prompt: String,
    pub images: Vec<SessionTurnImage>,
    pub context_references: Vec<crate::context_reference::SessionContextReference>,
    pub use_tools: bool,
    pub access_profile: AccessProfile,
    pub skill_name: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
    pub forced_tool_name: Option<String>,
    pub required_tool_chain: Vec<String>,
    pub goal_turn_mode: SessionGoalTurnMode,
    pub product_locale: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTurnFailureReason {
    ModelInvocationFailed,
    ModelStreamInterrupted,
    ModelEmptyResponse,
    ModelEmptyResponseAfterTools,
    ModelImageInvocationFailed,
    RuntimeInvalidState,
}

impl SessionTurnFailureReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::ModelInvocationFailed => "model_invocation_failed",
            Self::ModelStreamInterrupted => "model_stream_interrupted",
            Self::ModelEmptyResponse => "model_empty_response",
            Self::ModelEmptyResponseAfterTools => "model_empty_response_after_tools",
            Self::ModelImageInvocationFailed => "model_image_invocation_failed",
            Self::RuntimeInvalidState => "session_turn_runtime_invalid_state",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTurnExecutionError {
    pub reason: SessionTurnFailureReason,
    pub diagnostic_code: String,
    pub public_message: String,
}

impl SessionTurnExecutionError {
    fn new(reason: SessionTurnFailureReason, public_message: impl Into<String>) -> Self {
        Self {
            reason,
            diagnostic_code: reason.code().to_string(),
            public_message: public_message.into(),
        }
    }

    fn with_diagnostic_code(
        reason: SessionTurnFailureReason,
        diagnostic_code: impl Into<String>,
        public_message: impl Into<String>,
    ) -> Self {
        Self {
            reason,
            diagnostic_code: diagnostic_code.into(),
            public_message: public_message.into(),
        }
    }

    fn runtime_invalid_state() -> Self {
        Self::new(
            SessionTurnFailureReason::RuntimeInvalidState,
            "对话运行状态异常，请重新发送。",
        )
    }
}

impl fmt::Display for SessionTurnExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.public_message)
    }
}

impl std::error::Error for SessionTurnExecutionError {}

fn apply_request_aliases(
    item: &mut magi_session_store::ActiveExecutionTurnItem,
    request: &SessionTurnExecutionRequest,
) {
    item.request_id = request.request_id.clone();
    item.user_message_id = request.user_message_id.clone();
    item.placeholder_message_id = request.placeholder_message_id.clone();
}

fn apply_goal_turn_intermediate_visibility(
    item: &mut magi_session_store::ActiveExecutionTurnItem,
    request: &SessionTurnExecutionRequest,
) {
    if request.goal_turn_mode.is_goal_driven() {
        item.metadata
            .insert("renderable".to_string(), serde_json::Value::Bool(false));
    }
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
    event_bus: Option<&InMemoryEventBus>,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    prompt: &str,
    knowledge_context_prompt: Option<&str>,
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
                    turn.turn_id != request.turn_id
                        && turn.accepted_at.0 < accepted_at.0
                        && turn.status != magi_session_store::CanonicalTurnStatus::Cancelled
                        && turn.status != magi_session_store::CanonicalTurnStatus::Superseded
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
                    Some(ThreadChatMessage {
                        role: role.to_string(),
                        content: Some(content),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let usage_observation = event_bus
        .and_then(|event_bus| latest_session_usage_observation(event_bus, &request.session_id));
    history = compact_history_for_prompt(history, usage_observation.as_ref());
    let mut messages = if request.use_tools {
        workspace_context_messages(request)
    } else {
        Vec::new()
    };
    messages.push(system_prompt_fragment_message(
        PromptFragmentKind::CurrentTurnPriority,
        format!(
            "计划语言规则：用户明确指定的语言优先，其次当前用户消息的主要语言，再次产品 locale={}，最后默认 zh-CN。调用 update_plan 时必须将最终选择写入 language，计划创建后不得切换。",
            request.product_locale
        ),
    ));
    if let Some(reference_prompt) =
        crate::context_reference::session_context_references_prompt(&request.context_references)
    {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::ContextReferences,
            reference_prompt,
        ));
    }
    if let Some(knowledge_context_prompt) = knowledge_context_prompt {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::KnowledgeContext,
            knowledge_context_prompt,
        ));
    }
    messages.extend(history.iter().map(thread_chat_message_to_chat_message));
    messages.push(system_prompt_fragment_message(
        PromptFragmentKind::CurrentTurnPriority,
        current_turn_context_priority_prompt(),
    ));
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.to_string()),
        images: session_turn_image_sources(&request.images),
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

    vec![system_prompt_fragment_message(
        PromptFragmentKind::WorkspaceContext,
        workspace_context_system_prompt(root_path),
    )]
}

pub struct SessionTurnExecutionRuntime<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub conversation_registry: &'a ConversationRegistry,
    pub plan_store: &'a magi_plan::PlanStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub skill_dispatch_runtime: Option<&'a magi_skill_runtime::SkillDispatchRuntime>,
    pub skill_name: Option<String>,
    pub snapshot_manager: Option<&'a Arc<SnapshotManager>>,
    pub request: SessionTurnExecutionRequest,
    pub prompt: String,
    pub knowledge_context_prompt: Option<String>,
    pub tools: Option<Vec<ChatToolDefinition>>,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
}

pub fn run_session_turn_execution(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    let plan_store = runtime.plan_store;
    let session_id = runtime.request.session_id.clone();
    let result = run_session_turn_execution_inner(runtime);
    if result.is_err()
        && let Err(todo_error) = plan_store.pause()
    {
        tracing::warn!(
            session_id = %session_id,
            error = %todo_error,
            "对话轮次失败后暂停 PlanStore 进行项失败"
        );
    }
    result
}

fn run_session_turn_execution_inner(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    let SessionTurnExecutionRuntime {
        client,
        event_bus,
        session_store,
        conversation_registry,
        plan_store,
        settings_store,
        safety_gate,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        snapshot_manager,
        request,
        prompt,
        knowledge_context_prompt,
        tools,
        persist_session_state,
    } = runtime;

    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }

    // session 一生一 mission：session turn 执行必须在已注册的 orchestrator thread 上。
    let orchestrator_thread = session_store
        .orchestrator_thread_for_session(&request.session_id)
        .ok_or_else(SessionTurnExecutionError::runtime_invalid_state)?;
    let orchestrator_thread_id = orchestrator_thread.thread_id;
    let orchestrator_mission_id = orchestrator_thread.mission_id;

    let mut messages = build_session_turn_messages(
        Some(event_bus),
        session_store,
        &request,
        &prompt,
        knowledge_context_prompt.as_deref(),
    );
    let mut final_content: Option<String> = None;
    let mut final_item_id: Option<String> = None;
    let mut main_timeline_entry_id: Option<String> = None;
    let mut had_tool_calls = false;
    let mut active_skill_name = skill_name;
    let mut active_tools = tools.unwrap_or_default();
    let mut completed_required_tool_names: Vec<String> = Vec::new();
    let usage_binding = session_turn_model_usage_binding(request.use_tools);

    let mut round_limit = tool_call_round_limit(&request.required_tool_chain);
    let mut round = 0usize;
    while round < round_limit {
        if request.use_tools
            && let Some(registry) = tool_registry
        {
            active_tools = refresh_live_mcp_tool_definitions(
                active_tools,
                registry,
                skill_runtime,
                active_skill_name.as_deref(),
                request.access_profile,
                None,
                &[],
            );
        }
        let round_tools = (!active_tools.is_empty()).then_some(active_tools.clone());
        let streamed_content = match stream_session_turn_round(
            SessionTurnRoundRuntime {
                client,
                event_bus,
                session_store,
                plan_store,
                settings_store,
                safety_gate,
                snapshot_manager,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &prompt,
                tools: round_tools,
                messages: &mut messages,
                completed_required_tool_names: &completed_required_tool_names,
                round,
                orchestrator_thread_id: &orchestrator_thread_id,
                orchestrator_mission_id: &orchestrator_mission_id,
                persist_session_state,
            },
            tool_registry,
            skill_runtime,
            skill_dispatch_runtime,
            active_skill_name.as_deref(),
        ) {
            Ok(output) => output,
            Err(error) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let execution_error = session_turn_model_error(&request, &error);
                append_session_turn_error_item(
                    event_bus,
                    session_store,
                    crate::session_writeback::SessionTurnErrorInput {
                        session_id: &request.session_id,
                        workspace_id: &request.workspace_id,
                        task_id: None,
                        request_id: request.request_id.as_deref(),
                        user_message_id: request.user_message_id.as_deref(),
                        placeholder_message_id: request.placeholder_message_id.as_deref(),
                        error_text: &execution_error.public_message,
                        streaming_entry_id: main_timeline_entry_id.as_deref(),
                        source_thread_id: orchestrator_thread_id.clone(),
                        persist_session_state,
                    },
                );
                return Err(execution_error);
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

        if let Some(skill_id) = streamed_content.activated_skill_id.as_deref()
            && active_skill_name.as_deref() != Some(skill_id)
            && let Some(runtime) = skill_runtime
        {
            let preserved_goal_tools = if request.goal_turn_mode.is_goal_driven() {
                ["get_goal", "create_goal", "update_goal", "update_plan"].as_slice()
            } else {
                [].as_slice()
            };
            active_tools = activate_skill_tool_definitions(
                active_tools,
                runtime,
                skill_id,
                request.access_profile,
                preserved_goal_tools,
            );
            active_skill_name = Some(skill_id.to_string());
            if let Some(skill) = runtime.registry().get(skill_id) {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(format!(
                        "--- Skill: {} ---\n{}\n{}",
                        skill.title,
                        crate::prompt_utils::SKILL_PROMPT_PRIORITY_NOTE,
                        skill.instruction
                    )),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
        }

        if let Some(content) = streamed_content.final_content {
            if !required_tool_chain_is_complete(
                &request.required_tool_chain,
                &completed_required_tool_names,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(required_tool_chain_recovery_prompt(
                        &request.required_tool_chain,
                        &completed_required_tool_names,
                    )),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                let steers = conversation_registry
                    .drain_session_turn_steers(&request.session_id, &request.turn_id);
                if append_session_turn_steers_to_messages(&mut messages, steers) {
                    round_limit = round_limit
                        .max(round.saturating_add(2))
                        .min(MAX_TOOL_CALL_ROUNDS);
                }
                round = round.saturating_add(1);
                continue;
            }
            match conversation_registry
                .take_session_turn_steers_or_close(&request.session_id, &request.turn_id)
            {
                SessionTurnInputBoundary::Pending(steers) => {
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(content),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                    append_session_turn_steers_to_messages(&mut messages, steers);
                    round_limit = round_limit
                        .max(round.saturating_add(2))
                        .min(MAX_TOOL_CALL_ROUNDS);
                    round = round.saturating_add(1);
                    continue;
                }
                SessionTurnInputBoundary::Closed => {}
            }
            final_item_id = streamed_content.final_item_id;
            final_content = Some(content);
            break;
        }
        let steers =
            conversation_registry.drain_session_turn_steers(&request.session_id, &request.turn_id);
        if append_session_turn_steers_to_messages(&mut messages, steers) {
            round_limit = round_limit
                .max(round.saturating_add(2))
                .min(MAX_TOOL_CALL_ROUNDS);
        }
        round = round.saturating_add(1);
    }

    let final_content = if let Some(content) = final_content {
        content
    } else {
        if !request_turn_is_writable(session_store, &request) {
            return Ok(SessionTurnExecutionOutput::interrupted());
        }
        let failure = session_turn_empty_response_error(&request, had_tool_calls);
        append_session_turn_error_item(
            event_bus,
            session_store,
            crate::session_writeback::SessionTurnErrorInput {
                session_id: &request.session_id,
                workspace_id: &request.workspace_id,
                task_id: None,
                request_id: request.request_id.as_deref(),
                user_message_id: request.user_message_id.as_deref(),
                placeholder_message_id: request.placeholder_message_id.as_deref(),
                error_text: &failure.public_message,
                streaming_entry_id: main_timeline_entry_id.as_deref(),
                source_thread_id: orchestrator_thread_id.clone(),
                persist_session_state,
            },
        );
        return Err(failure);
    };
    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }
    append_final_item(
        event_bus,
        session_store,
        &request,
        FinalItemInput {
            content: &final_content,
            item_id: final_item_id.as_deref(),
            timeline_entry_id: main_timeline_entry_id.as_deref(),
        },
        &orchestrator_thread_id,
        persist_session_state,
    );

    Ok(SessionTurnExecutionOutput::completed(final_content))
}

fn append_session_turn_steers_to_messages(
    messages: &mut Vec<ChatMessage>,
    steers: Vec<UserSignal>,
) -> bool {
    let mut appended = false;
    for signal in steers {
        let Some(text) = signal
            .text
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(text),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
        appended = true;
    }
    appended
}

fn session_turn_model_error(
    request: &SessionTurnExecutionRequest,
    error: &str,
) -> SessionTurnExecutionError {
    if !request.images.is_empty() {
        return SessionTurnExecutionError::new(
            SessionTurnFailureReason::ModelImageInvocationFailed,
            public_model_image_invocation_error_message(error),
        );
    }
    let classification = classify_model_invocation_error(error);
    let reason = if classification.code == "model_stream_interrupted" {
        SessionTurnFailureReason::ModelStreamInterrupted
    } else {
        SessionTurnFailureReason::ModelInvocationFailed
    };
    SessionTurnExecutionError::with_diagnostic_code(
        reason,
        classification.code,
        classification.public_message,
    )
}

fn session_turn_empty_response_error(
    request: &SessionTurnExecutionRequest,
    after_tool_calls: bool,
) -> SessionTurnExecutionError {
    if !request.images.is_empty() {
        return SessionTurnExecutionError::new(
            SessionTurnFailureReason::ModelImageInvocationFailed,
            public_model_image_invocation_error_message("empty stream response"),
        );
    }
    let reason = if after_tool_calls {
        SessionTurnFailureReason::ModelEmptyResponseAfterTools
    } else {
        SessionTurnFailureReason::ModelEmptyResponse
    };
    SessionTurnExecutionError::new(
        reason,
        provider_empty_assistant_response_error(after_tool_calls),
    )
}

struct SessionTurnRoundRuntime<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    plan_store: &'a magi_plan::PlanStore,
    settings_store: Option<&'a Arc<SettingsStore>>,
    safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
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
    orchestrator_mission_id: &'a magi_core::MissionId,
    persist_session_state: Option<&'a SessionStatePersistCallback>,
}

struct SessionTurnRoundOutput {
    final_content: Option<String>,
    final_item_id: Option<String>,
    timeline_entry_id: Option<String>,
    encountered_tool_calls: bool,
    tool_call_names: Vec<String>,
    activated_skill_id: Option<String>,
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
    skill_dispatch_runtime: Option<&magi_skill_runtime::SkillDispatchRuntime>,
    skill_name: Option<&str>,
) -> Result<SessionTurnRoundOutput, String> {
    let SessionTurnRoundRuntime {
        client,
        event_bus,
        session_store,
        plan_store,
        settings_store,
        safety_gate,
        snapshot_manager,
        request,
        usage_binding,
        prompt,
        tools,
        messages,
        completed_required_tool_names,
        round,
        orchestrator_thread_id,
        orchestrator_mission_id,
        persist_session_state,
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
    let stream_publish_gate = std::cell::RefCell::new(SessionTurnStreamPublishGate::default());
    let thinking_publish_gate = std::cell::RefCell::new(SessionTurnStreamPublishGate::default());
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
                && let Some(stream_update) = stream_update.as_ref()
            {
                publish_session_turn_item_stream_event(
                    event_bus,
                    &request.session_id,
                    &request.workspace_id,
                    &published,
                    stream_update,
                    &mut thinking_publish_gate.borrow_mut(),
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
            && let Some(stream_update) = stream_update.as_ref()
        {
            publish_session_turn_item_stream_event(
                event_bus,
                &request.session_id,
                &request.workspace_id,
                &published,
                stream_update,
                &mut stream_publish_gate.borrow_mut(),
            );
        }
    };

    let tool_choice = forced_tool_choice_for_round(
        request,
        tools.as_ref(),
        round,
        completed_required_tool_names,
    );
    let round_started_at = UtcMillis::now();
    let on_retry = |retry_event: &magi_bridge_client::ModelRetryRuntimeEvent| {
        publish_model_retry_runtime_event(
            event_bus,
            &request.session_id,
            &request.workspace_id,
            &stream_item_id,
            None,
            retry_event,
        );
    };
    let call_id = format!("session-turn-{round}-{}", UtcMillis::now().0);
    let response = match client.invoke_streaming_with_cancellation(
        ModelInvocationRequest {
            provider: BUSINESS_MODEL_PROVIDER.to_string(),
            prompt: prompt.to_string(),
            messages: Some(messages.clone()),
            tools: tools.clone(),
            tool_choice,
        },
        &on_delta,
        &on_retry,
        &|| !request_turn_is_writable(session_store, request),
    ) {
        Ok(response) => response,
        Err(error) => {
            if !request_turn_is_writable(session_store, request) {
                return Ok(SessionTurnRoundOutput {
                    final_content: None,
                    final_item_id: None,
                    timeline_entry_id: None,
                    encountered_tool_calls: false,
                    tool_call_names: Vec::new(),
                    activated_skill_id: None,
                    interrupted: true,
                });
            }
            let raw_error = error.to_string();
            let classification = classify_model_invocation_error(&raw_error);
            publish_model_usage_record(
                event_bus,
                session_store,
                settings_store,
                crate::usage_recording::ModelUsageRecordInput {
                    session_id: &request.session_id,
                    workspace_id: &request.workspace_id,
                    binding: usage_binding,
                    call_id,
                    usage: None,
                    status: UsageCallStatus::Failed,
                    assignment_id: None,
                    error_code: Some(classification.code.to_string()),
                },
            );
            return Err(raw_error);
        }
    };
    let parsed = response.parse_chat_payload();
    let has_actionable_output = parsed
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty())
        || !parsed.tool_calls.is_empty();
    publish_model_usage_record(
        event_bus,
        session_store,
        settings_store,
        crate::usage_recording::ModelUsageRecordInput {
            session_id: &request.session_id,
            workspace_id: &request.workspace_id,
            binding: usage_binding,
            call_id,
            usage: parsed.usage.as_ref(),
            status: if has_actionable_output {
                UsageCallStatus::Success
            } else {
                UsageCallStatus::Failed
            },
            assignment_id: None,
            error_code: (!has_actionable_output).then(|| "model_empty_response".to_string()),
        },
    );
    account_active_goal_turn(
        session_store,
        &request.session_id,
        parsed.usage.as_ref(),
        UtcMillis::now()
            .0
            .saturating_sub(round_started_at.0)
            .saturating_div(1000),
    );
    let timeline_entry_id = None;
    if writeback_aborted.get() || !request_turn_is_writable(session_store, request) {
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            final_item_id: None,
            timeline_entry_id: timeline_entry_id.clone(),
            encountered_tool_calls: false,
            tool_call_names: Vec::new(),
            activated_skill_id: None,
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
                activated_skill_id: None,
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
        apply_goal_turn_intermediate_visibility(&mut thinking_item, request);
        if let Some(published) =
            upsert_session_turn_item(session_store, &request.session_id, thinking_item)
        {
            persist_session_state_checkpoint(
                persist_session_state,
                "session_turn_thinking_completed",
            );
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
                activated_skill_id: None,
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
        apply_goal_turn_intermediate_visibility(&mut stream_item, request);
        if let Some(published) =
            upsert_session_turn_item(session_store, &request.session_id, stream_item)
        {
            persist_session_state_checkpoint(
                persist_session_state,
                "session_turn_stream_completed",
            );
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
                activated_skill_id: None,
                interrupted: true,
            });
        }
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            images: Vec::new(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });
        let snapshot_session = snapshot_manager.and_then(|mgr| {
            request
                .workspace_root_path
                .as_deref()
                .map(PathBuf::from)
                .and_then(|root| mgr.get_session_for_workspace(request.session_id.as_str(), &root))
        });
        let execution_group_id = session_store
            .execution_ownership(&request.session_id)
            .and_then(|ownership| ownership.mission_id)
            .map(|mid| mid.to_string())
            .unwrap_or_else(|| format!("session:{}", request.session_id));
        let tool_batch = append_session_tool_call_items_batch_with_context(
            crate::session_writeback::SessionToolCallBatchContext {
                session_store,
                event_bus,
                tool_registry,
                skill_runtime,
                skill_dispatch_runtime,
                skill_name,
                safety_gate,
                plan_store,
                mission_id: orchestrator_mission_id,
                session_id: &request.session_id,
                workspace_id: &request.workspace_id,
                workspace_root_path: request.workspace_root_path.as_deref().map(PathBuf::from),
                context_references: &request.context_references,
                access_profile: request.access_profile,
                snapshot_session,
                execution_group_id: Some(execution_group_id),
                source_thread_id: orchestrator_thread_id,
                persist_session_state,
            },
            &parsed.tool_calls,
            messages,
            || request_turn_is_writable(session_store, request),
        );
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                activated_skill_id: None,
                interrupted: true,
            });
        }
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            final_item_id: None,
            timeline_entry_id: timeline_entry_id.clone(),
            encountered_tool_calls: true,
            tool_call_names: tool_batch.succeeded_tool_names,
            activated_skill_id: tool_batch.activated_skill_id,
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
        activated_skill_id: None,
        interrupted: false,
    })
}

fn forced_tool_choice_for_round(
    request: &SessionTurnExecutionRequest,
    tools: Option<&Vec<ChatToolDefinition>>,
    round: usize,
    _completed_required_tool_names: &[String],
) -> Option<ChatToolChoice> {
    if !request.use_tools {
        return None;
    }
    let forced_tool_name = (round == 0)
        .then_some(request.forced_tool_name.as_deref())
        .flatten()?
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

struct FinalItemInput<'a> {
    content: &'a str,
    item_id: Option<&'a str>,
    timeline_entry_id: Option<&'a str>,
}

fn append_final_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    input: FinalItemInput<'_>,
    orchestrator_thread_id: &magi_core::ThreadId,
    persist_session_state: Option<&SessionStatePersistCallback>,
) {
    let FinalItemInput {
        content: final_content,
        item_id: final_item_id,
        timeline_entry_id,
    } = input;
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
    if request.goal_turn_mode.is_goal_driven()
        && session_store.active_goal(&request.session_id).is_some()
    {
        final_item
            .metadata
            .insert("renderable".to_string(), serde_json::Value::Bool(false));
    }
    let final_item_id = final_item.item_id.clone();
    if has_requested_final_item_id {
        if let Some(published) =
            upsert_session_turn_item(session_store, &request.session_id, final_item)
        {
            persist_session_state_checkpoint(persist_session_state, "session_turn_final_item");
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
        persist_session_state_checkpoint(persist_session_state, "session_turn_final_item");
        publish_session_turn_item_event(
            event_bus,
            &request.session_id,
            &request.workspace_id,
            &published,
        );
    }
    let _ = session_store.update_current_turn_status(&request.session_id, "completed");
    persist_session_state_checkpoint(persist_session_state, "session_turn_completed");
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
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, BridgeResponse, ModelRetryRuntimeEvent,
        ModelRetryRuntimePhase,
    };
    use magi_core::SessionLifecycleStatus;
    use magi_session_store::{
        ActiveExecutionTurn, CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind,
        CanonicalTurnItemStatus, CanonicalTurnStatus, CanonicalTurnVisibility, ExecutionThread,
        ExecutionThreadStatus, ORCHESTRATOR_ROLE_ID, SessionRecord, SessionStoreState,
        TimelineEntry, TimelineEntryKind,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ts(value: u64) -> UtcMillis {
        UtcMillis(value)
    }

    struct StreamingTextModelBridgeClient {
        delta_content: String,
        payload: String,
    }

    struct RetryEventModelBridgeClient;

    struct CancellingModelBridgeClient {
        store: Arc<SessionStore>,
        session_id: SessionId,
    }

    impl ModelBridgeClient for CancellingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            panic!("session turn should use cancellable streaming invocation")
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            panic!("session turn should use cancellable streaming invocation")
        }

        fn invoke_streaming_with_cancellation(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
            _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
            is_cancelled: &dyn Fn() -> bool,
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.store
                .cancel_current_turn(&self.session_id)
                .expect("turn cancellation should succeed");
            assert!(is_cancelled());
            Err(magi_bridge_client::model_invocation_cancelled_error())
        }
    }

    impl ModelBridgeClient for RetryEventModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({ "content": "重连后完成" }).to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: "重连后完成".to_string(),
                thinking: String::new(),
            });
            self.invoke(request)
        }

        fn invoke_streaming_with_retry_events(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
            on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        ) -> Result<BridgeResponse, BridgeClientError> {
            on_retry(&ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::Scheduled,
                attempt: 1,
                max_attempts: 5,
                delay_ms: Some(10_000),
            });
            on_retry(&ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::AttemptStarted,
                attempt: 1,
                max_attempts: 5,
                delay_ms: None,
            });
            let response = self.invoke_streaming(request, on_delta);
            on_retry(&ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::Settled,
                attempt: 1,
                max_attempts: 5,
                delay_ms: None,
            });
            response
        }
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
    fn session_turn_cancellation_interrupts_model_invocation() {
        let session_id = SessionId::new("session-model-cancellation");
        let turn_id = "turn-model-cancellation".to_string();
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "model cancellation")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(900), || {
                magi_core::MissionId::new("mission-model-cancellation")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: turn_id.clone(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("停止测试".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("停止测试".to_string()),
                        Some("user-model-cancellation".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let registry = ConversationRegistry::new();
        registry
            .begin_session_turn_input(session_id.clone(), turn_id.clone())
            .expect("turn input should begin");
        let client = CancellingModelBridgeClient {
            store: store.clone(),
            session_id: session_id.clone(),
        };
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        let output = run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &InMemoryEventBus::new(16),
            session_store: store.as_ref(),
            conversation_registry: &registry,
            plan_store: &plan_store,
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request: SessionTurnExecutionRequest {
                session_id,
                turn_id,
                workspace_id: None,
                prompt: "停止测试".to_string(),
                images: Vec::new(),
                context_references: Vec::new(),
                use_tools: false,
                access_profile: AccessProfile::Restricted,
                skill_name: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                forced_tool_name: None,
                required_tool_chain: Vec::new(),
                goal_turn_mode: SessionGoalTurnMode::None,
                product_locale: "zh-CN".to_string(),
                workspace_root_path: None,
            },
            prompt: "停止测试".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
        })
        .expect("cancelled model invocation should resolve as interrupted turn");
        assert!(output.interrupted);
    }

    struct FailingModelBridgeClient {
        message: String,
    }

    impl ModelBridgeClient for FailingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32007),
                message: self.message.clone(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    struct StreamingThenFailingModelBridgeClient {
        delta_content: String,
        message: String,
    }

    struct SteeringModelBridgeClient {
        registry: Arc<ConversationRegistry>,
        session_id: SessionId,
        turn_id: String,
        calls: AtomicUsize,
        requests: std::sync::Mutex<Vec<ModelInvocationRequest>>,
    }

    impl ModelBridgeClient for SteeringModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .expect("request log lock")
                .push(request);
            if call == 0 {
                self.registry
                    .try_steer_session_turn(
                        &self.session_id,
                        &self.turn_id,
                        UserSignal {
                            text: Some("优先收口，不要继续扩展".to_string()),
                            request_id: Some("request-steer-runtime".to_string()),
                            user_message_id: Some("user-steer-runtime".to_string()),
                            placeholder_message_id: None,
                            accepted_at: ts(1_100),
                        },
                    )
                    .expect("steer should be accepted while first model call is active");
            }
            let content = if call == 0 {
                "第一段回复"
            } else {
                "最终收口"
            };
            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "content": content,
                    "finish_reason": "stop"
                })
                .to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            let next_call = self.calls.load(Ordering::SeqCst);
            on_delta(&ModelStreamingDelta {
                content: if next_call == 0 {
                    "第一段回复".to_string()
                } else {
                    "最终收口".to_string()
                },
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    #[test]
    fn active_turn_steer_continues_same_turn_with_a_second_model_call() {
        let session_id = SessionId::new("session-runtime-steer");
        let turn_id = "turn-runtime-steer".to_string();
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "runtime steer")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(900), || {
                magi_core::MissionId::new("mission-runtime-steer")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: turn_id.clone(),
                    turn_seq: 1_000,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请给出完整方案".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请给出完整方案".to_string()),
                        Some("user-runtime-steer".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let registry = Arc::new(ConversationRegistry::new());
        registry
            .begin_session_turn_input(session_id.clone(), turn_id.clone())
            .expect("turn input should begin");
        let client = SteeringModelBridgeClient {
            registry: registry.clone(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            calls: AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        };
        let event_bus = InMemoryEventBus::new(32);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id,
            workspace_id: None,
            prompt: "请给出完整方案".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: Some("request-runtime-steer".to_string()),
            user_message_id: Some("user-runtime-steer".to_string()),
            placeholder_message_id: Some("assistant-runtime-steer".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let output = run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &event_bus,
            session_store: &store,
            conversation_registry: registry.as_ref(),
            plan_store: &crate::test_plan_store("test-todo-ledger"),
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request,
            prompt: "请给出完整方案".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
        })
        .expect("steered turn should complete");

        assert_eq!(output.final_content, "最终收口");
        let requests = client.requests.lock().expect("request log lock");
        assert_eq!(requests.len(), 2);
        let second_messages = requests[1]
            .messages
            .as_ref()
            .expect("second call should carry messages");
        assert!(second_messages.iter().any(|message| {
            message.role == "assistant" && message.content.as_deref() == Some("第一段回复")
        }));
        assert!(second_messages.iter().any(|message| {
            message.role == "user" && message.content.as_deref() == Some("优先收口，不要继续扩展")
        }));
    }

    impl ModelBridgeClient for StreamingThenFailingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32007),
                message: self.message.clone(),
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
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: Some("diagram_render".to_string()),
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
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
    fn required_tool_chain_uses_recovery_without_provider_specific_forcing() {
        let request = SessionTurnExecutionRequest {
            session_id: SessionId::new("session-required-tool-chain"),
            turn_id: "turn-required-tool-chain".to_string(),
            workspace_id: None,
            prompt: "依次调用 shell_exec、file_write、file_read".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
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
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
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

        assert!(forced_tool_choice_for_round(&request, Some(&tools), 0, &[]).is_none());
        assert!(
            forced_tool_choice_for_round(&request, Some(&tools), 1, &["shell_exec".to_string()])
                .is_none()
        );
        assert!(
            forced_tool_choice_for_round(
                &request,
                Some(&tools),
                2,
                &["shell_exec".to_string(), "file_write".to_string()],
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
    fn runtime_invalid_state_pauses_in_progress_todo() {
        let session_id = SessionId::new("session-runtime-invalid-state");
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "runtime invalid state")
            .expect("session should be creatable");
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-runtime-invalid-state".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续处理".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        plan_store
            .write(vec![magi_core::TodoItem::new(
                "继续处理当前步骤",
                "正在处理当前步骤",
                magi_core::TodoStatus::InProgress,
            )])
            .expect("todo should write");
        let client = StreamingTextModelBridgeClient {
            delta_content: "不会执行".to_string(),
            payload: serde_json::json!({
                "content": "不会执行",
                "finish_reason": "stop"
            })
            .to_string(),
        };

        let result = run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &InMemoryEventBus::new(16),
            session_store: store.as_ref(),
            conversation_registry: &ConversationRegistry::new(),
            plan_store: &plan_store,
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request: SessionTurnExecutionRequest {
                session_id: session_id.clone(),
                turn_id: "turn-runtime-invalid-state".to_string(),
                workspace_id: None,
                prompt: "继续处理".to_string(),
                images: Vec::new(),
                context_references: Vec::new(),
                use_tools: false,
                access_profile: AccessProfile::Restricted,
                skill_name: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                forced_tool_name: None,
                required_tool_chain: Vec::new(),
                goal_turn_mode: SessionGoalTurnMode::None,
                product_locale: "zh-CN".to_string(),
                workspace_root_path: None,
            },
            prompt: "继续处理".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
        });

        assert!(matches!(
            result,
            Err(SessionTurnExecutionError {
                reason: SessionTurnFailureReason::RuntimeInvalidState,
                ..
            })
        ));
        assert_eq!(
            plan_store.snapshot()[0].status,
            magi_core::TodoStatus::Pending
        );
    }

    #[test]
    fn empty_session_turn_response_uses_public_failure_message() {
        let session_id = SessionId::new("session-empty-response-layer");
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "empty response layer")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(910), || {
                magi_core::MissionId::new("mission-empty-response-layer")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-empty-response-layer".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请回复一句话".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请回复一句话".to_string()),
                        Some("user-empty-response-layer".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let client = StreamingTextModelBridgeClient {
            delta_content: String::new(),
            payload: serde_json::json!({
                "content": null,
                "finish_reason": "stop"
            })
            .to_string(),
        };
        let event_bus = InMemoryEventBus::new(16);
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        plan_store
            .write(vec![magi_core::TodoItem::new(
                "继续处理当前步骤",
                "正在处理当前步骤",
                magi_core::TodoStatus::InProgress,
            )])
            .expect("todo should write");
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-empty-response-layer".to_string(),
            workspace_id: None,
            prompt: "请回复一句话".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let error = match run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &event_bus,
            session_store: store.as_ref(),
            conversation_registry: &ConversationRegistry::new(),
            plan_store: &plan_store,
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request,
            prompt: "请回复一句话".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
        }) {
            Ok(_) => panic!("empty provider response should fail"),
            Err(error) => error,
        };

        assert_eq!(error.reason, SessionTurnFailureReason::ModelEmptyResponse);
        assert_eq!(
            error.public_message,
            "模型本轮未返回有效内容，可直接继续重试。"
        );
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain visible");
        assert_eq!(turn.status, "failed");
        assert!(turn.items.iter().any(|item| {
            item.kind == "assistant_error"
                && item.content.as_deref() == Some(error.public_message.as_str())
        }));
        assert_eq!(
            plan_store.snapshot()[0].status,
            magi_core::TodoStatus::Pending
        );
    }

    #[test]
    fn partial_stream_failure_marks_turn_failed_instead_of_completed() {
        let session_id = SessionId::new("session-partial-stream-failure");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "partial stream failure")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(915), || {
                magi_core::MissionId::new("mission-partial-stream-failure")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-partial-stream-failure".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请输出长回复".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请输出长回复".to_string()),
                        Some("user-partial-stream-failure".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let client = StreamingThenFailingModelBridgeClient {
            delta_content: "这是一段半截输出".to_string(),
            message:
                "provider response invalid: incomplete stream response: missing terminal SSE event"
                    .to_string(),
        };
        let event_bus = InMemoryEventBus::new(16);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-partial-stream-failure".to_string(),
            workspace_id: None,
            prompt: "请输出长回复".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let error = match run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &event_bus,
            session_store: &store,
            conversation_registry: &ConversationRegistry::new(),
            plan_store: &crate::test_plan_store("test-todo-ledger"),
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request,
            prompt: "请输出长回复".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
        }) {
            Ok(_) => panic!("incomplete stream should fail the turn"),
            Err(error) => error,
        };

        assert_eq!(
            error.reason,
            SessionTurnFailureReason::ModelStreamInterrupted
        );
        assert_eq!(error.public_message, "模型响应流中断，可直接继续重试。");
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain visible");
        assert_eq!(turn.status, "failed");
        assert!(turn.items.iter().any(|item| {
            item.kind == "assistant_error"
                && item.content.as_deref() == Some(error.public_message.as_str())
        }));
        assert!(
            !turn
                .items
                .iter()
                .any(|item| item.kind == "assistant_stream" && item.status == "completed"),
            "半截流失败后不能把 assistant_stream 结算为 completed"
        );
    }

    #[test]
    fn image_session_turn_streaming_error_uses_image_capability_message() {
        let session_id = SessionId::new("session-image-error-layer");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "image error layer")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(920), || {
                magi_core::MissionId::new("mission-image-error-layer")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-image-error-layer".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请识别这张图片".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请识别这张图片".to_string()),
                        Some("user-image-error-layer".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let client = FailingModelBridgeClient {
            message: "provider response invalid: empty stream response".to_string(),
        };
        let event_bus = InMemoryEventBus::new(16);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-image-error-layer".to_string(),
            workspace_id: None,
            prompt: "请识别这张图片".to_string(),
            images: vec![
                SessionTurnImage::from_data_url("smoke.png", "data:image/png;base64,iVBORw0KGgo=")
                    .expect("image should parse"),
            ],
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let error = match run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &event_bus,
            session_store: &store,
            conversation_registry: &ConversationRegistry::new(),
            plan_store: &crate::test_plan_store("test-todo-ledger"),
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request,
            prompt: "请识别这张图片".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
        }) {
            Ok(_) => panic!("image provider empty stream should fail"),
            Err(error) => error,
        };

        assert_eq!(
            error.public_message,
            crate::model_error::PUBLIC_MODEL_IMAGE_INVOCATION_FAILURE_MESSAGE
        );
        assert_eq!(
            error.reason,
            SessionTurnFailureReason::ModelImageInvocationFailed
        );
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain visible");
        assert_eq!(turn.status, "failed");
        assert!(turn.items.iter().any(|item| {
            item.kind == "assistant_error"
                && item.content.as_deref() == Some(error.public_message.as_str())
        }));
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
        let (mission_id, orchestrator_thread_id) =
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
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: Some("request-placeholder-reuse".to_string()),
            user_message_id: Some("user-placeholder-reuse".to_string()),
            placeholder_message_id: Some("assistant-placeholder-reuse".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };
        let usage_binding = session_turn_model_usage_binding(false);
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(request.prompt.clone()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];

        let output = stream_session_turn_round(
            SessionTurnRoundRuntime {
                client: &client,
                event_bus: &event_bus,
                session_store: &store,
                plan_store: &crate::test_plan_store("test-todo-ledger"),
                settings_store: None,
                safety_gate: None,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &request.prompt,
                tools: None,
                messages: &mut messages,
                completed_required_tool_names: &[],
                snapshot_manager: None,
                round: 0,
                orchestrator_thread_id: &orchestrator_thread_id,
                orchestrator_mission_id: &mission_id,
                persist_session_state: None,
            },
            None,
            None,
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
    fn session_turn_round_forwards_model_retry_runtime_events() {
        let session_id = SessionId::new("session-model-retry-runtime");
        let workspace_id = Some(WorkspaceId::new("workspace-model-retry-runtime"));
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "model retry runtime")
            .expect("session should be creatable");
        let (mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(900), || {
                magi_core::MissionId::new("mission-model-retry-runtime")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-model-retry-runtime".to_string(),
                    turn_seq: 1_000,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请继续".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请继续".to_string()),
                        Some("user-model-retry-runtime".to_string()),
                        orchestrator_thread_id.clone(),
                    )],
                },
            )
            .expect("current turn should be stored");
        let event_bus = InMemoryEventBus::new(16);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-model-retry-runtime".to_string(),
            workspace_id: workspace_id.clone(),
            prompt: "请继续".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: Some("assistant-model-retry-runtime".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };
        let usage_binding = session_turn_model_usage_binding(false);
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(request.prompt.clone()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];

        stream_session_turn_round(
            SessionTurnRoundRuntime {
                client: &RetryEventModelBridgeClient,
                event_bus: &event_bus,
                session_store: &store,
                plan_store: &crate::test_plan_store("test-todo-ledger"),
                settings_store: None,
                safety_gate: None,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &request.prompt,
                tools: None,
                messages: &mut messages,
                completed_required_tool_names: &[],
                snapshot_manager: None,
                round: 0,
                orchestrator_thread_id: &orchestrator_thread_id,
                orchestrator_mission_id: &mission_id,
                persist_session_state: None,
            },
            None,
            None,
            None,
            None,
        )
        .expect("round should complete after retry");

        let retry_events = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .filter(|event| event.event_type == "model.retry.runtime")
            .collect::<Vec<_>>();
        assert_eq!(retry_events.len(), 3);
        assert_eq!(retry_events[0].payload["phase"], "scheduled");
        assert_eq!(retry_events[1].payload["phase"], "attempt_started");
        assert_eq!(retry_events[2].payload["phase"], "settled");
        assert!(retry_events.iter().all(|event| {
            event.payload["message_id"] == "assistant-model-retry-runtime"
                && event.session_id.as_ref() == Some(&session_id)
                && event.workspace_id.as_ref() == workspace_id.as_ref()
        }));
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
                last_completed_at: None,
                last_viewed_at: None,
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
                        metadata: HashMap::from([(
                            "images".to_string(),
                            serde_json::json!([{
                                "name": "previous.png",
                                "dataUrl": "data:image/png;base64,AAA"
                            }]),
                        )]),
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
            goals: Vec::new(),
            plans: Vec::new(),
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
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };
        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);

        assert_eq!(
            messages
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "assistant", "system", "user"]
        );
        let contents = messages
            .iter()
            .map(|message| message.content.as_deref().unwrap_or(""))
            .collect::<Vec<_>>();
        assert_eq!(contents[0], "请用一句话回答：2+3 等于几？");
        assert!(
            messages[0].images.is_empty(),
            "历史图片只能作为会话记录展示，不能重复进入后续文本 turn 的模型上下文"
        );
        assert_eq!(contents[1], "2+3 等于 5。");
        assert!(contents[2].contains("本轮用户原始输入"));
        assert!(contents[2].contains("只能作为参考证据"));
        assert_eq!(
            contents[3],
            "请基于上一轮结果，用一句话回答：再加 4 等于几？"
        );
    }

    #[test]
    fn session_turn_messages_exclude_cancelled_turn_from_model_history() {
        let session_id = SessionId::new("session-context-history-cancelled");
        let thread_id = magi_core::ThreadId::new("thread-context-history-cancelled");
        let store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "cancelled context history".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: ts(900),
                updated_at: ts(2_000),
                message_count: None,
                workspace_id: None,
                last_completed_at: None,
                last_viewed_at: None,
            }],
            timeline: Vec::new(),
            canonical_turns: vec![CanonicalTurn {
                session_id: session_id.clone(),
                turn_id: "turn-cancelled".to_string(),
                turn_seq: 1_000,
                accepted_at: ts(1_000),
                completed_at: Some(ts(1_100)),
                status: CanonicalTurnStatus::Cancelled,
                response_duration_ms: Some(100),
                usage: None,
                items: vec![CanonicalTurnItem {
                    session_id: session_id.clone(),
                    turn_id: "turn-cancelled".to_string(),
                    turn_seq: 1_000,
                    item_id: "turn-cancelled-user".to_string(),
                    item_seq: 1,
                    kind: CanonicalTurnItemKind::UserMessage,
                    created_at: ts(1_000),
                    status: CanonicalTurnItemStatus::Cancelled,
                    item_version: None,
                    updated_at: ts(1_100),
                    title: None,
                    content: Some("执行 sleep 20，完成后回复未被停止".to_string()),
                    blocks: Vec::new(),
                    tool: None,
                    worker: None,
                    source_thread_id: thread_id.clone(),
                    visibility: CanonicalTurnVisibility::default(),
                    metadata: HashMap::new(),
                }],
                metadata: HashMap::new(),
            }],
            notifications: Vec::new(),
            goals: Vec::new(),
            plans: Vec::new(),
            execution_sidecar_store: Default::default(),
            thread_registry: vec![ExecutionThread {
                thread_id,
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-context-history-cancelled"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-context-history-cancelled"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1_100),
                handled_task_ids: Vec::new(),
                message_history: Vec::new(),
            }],
        });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-current".to_string(),
                    turn_seq: 2_000,
                    accepted_at: ts(2_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("只回复停止后恢复正常".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-current".to_string(),
            workspace_id: None,
            prompt: "只回复停止后恢复正常".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);
        let contents = messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>();

        assert!(
            contents
                .iter()
                .all(|content| !content.contains("sleep 20") && !content.contains("未被停止"))
        );
        assert_eq!(contents.last().copied(), Some("只回复停止后恢复正常"));
    }

    #[test]
    fn session_turn_messages_do_not_drop_early_history_after_six_rounds() {
        let session_id = SessionId::new("session-context-history-long");
        let thread_id = magi_core::ThreadId::new("thread-context-history-long");
        let canonical_turns = (0..7)
            .map(|index| {
                let turn_seq = 1_000 + index * 100;
                CanonicalTurn {
                    session_id: session_id.clone(),
                    turn_id: format!("turn-history-{index}"),
                    turn_seq,
                    accepted_at: ts(turn_seq),
                    completed_at: Some(ts(turn_seq + 10)),
                    status: CanonicalTurnStatus::Completed,
                    response_duration_ms: Some(10),
                    usage: None,
                    items: vec![
                        CanonicalTurnItem {
                            session_id: session_id.clone(),
                            turn_id: format!("turn-history-{index}"),
                            turn_seq,
                            item_id: format!("history-user-{index}"),
                            item_seq: 1,
                            kind: CanonicalTurnItemKind::UserMessage,
                            created_at: ts(turn_seq),
                            status: CanonicalTurnItemStatus::Completed,
                            item_version: None,
                            updated_at: ts(turn_seq),
                            title: None,
                            content: Some(if index == 0 {
                                "最早上下文标记：银杏-7429-海盐".to_string()
                            } else {
                                format!("第 {index} 轮用户消息")
                            }),
                            blocks: Vec::new(),
                            tool: None,
                            worker: None,
                            source_thread_id: thread_id.clone(),
                            visibility: CanonicalTurnVisibility::default(),
                            metadata: HashMap::new(),
                        },
                        CanonicalTurnItem {
                            session_id: session_id.clone(),
                            turn_id: format!("turn-history-{index}"),
                            turn_seq,
                            item_id: format!("history-assistant-{index}"),
                            item_seq: 2,
                            kind: CanonicalTurnItemKind::AssistantText,
                            created_at: ts(turn_seq + 10),
                            status: CanonicalTurnItemStatus::Completed,
                            item_version: None,
                            updated_at: ts(turn_seq + 10),
                            title: None,
                            content: Some(format!("第 {index} 轮助手回复")),
                            blocks: Vec::new(),
                            tool: None,
                            worker: None,
                            source_thread_id: thread_id.clone(),
                            visibility: CanonicalTurnVisibility::default(),
                            metadata: HashMap::new(),
                        },
                    ],
                    metadata: HashMap::new(),
                }
            })
            .collect::<Vec<_>>();
        let store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "long context history".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: ts(900),
                updated_at: ts(2_000),
                message_count: None,
                workspace_id: None,
                last_completed_at: None,
                last_viewed_at: None,
            }],
            timeline: Vec::new(),
            canonical_turns,
            notifications: Vec::new(),
            goals: Vec::new(),
            plans: Vec::new(),
            execution_sidecar_store: Default::default(),
            thread_registry: vec![ExecutionThread {
                thread_id: thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-context-history-long"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-context-history-long"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1_700),
                handled_task_ids: Vec::new(),
                message_history: Vec::new(),
            }],
        });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-session-2000".to_string(),
                    turn_seq: 2_000,
                    accepted_at: ts(2_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("最早的上下文标记是什么？".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-session-2000".to_string(),
            workspace_id: None,
            prompt: "最早的上下文标记是什么？".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);

        assert!(messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("银杏-7429-海盐"))
        }));
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
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: Some("/tmp/current-project".to_string()),
        };

        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);

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
        assert!(
            messages
                .iter()
                .any(|message| message.content.as_deref().is_some_and(|content| {
                    content.contains("上下文优先级") && content.contains("ProjectMemory")
                }))
        );
    }

    #[test]
    fn build_session_turn_messages_injects_structured_context_references() {
        let session_id = SessionId::new("session-context-references");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "context references")
            .expect("session should be created");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-context-references".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context-references")),
            prompt: "分析引用内容".to_string(),
            images: Vec::new(),
            context_references: vec![crate::context_reference::SessionContextReference {
                kind: crate::context_reference::SessionContextReferenceKind::Directory,
                path: PathBuf::from("/tmp/external-reference"),
                name: "external-reference".to_string(),
            }],
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: Some("/tmp/current-project".to_string()),
        };

        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);
        let reference_context = messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("/tmp/external-reference"))
            .expect("context reference prompt should be injected");
        assert!(reference_context.contains("只读上下文引用"));
        assert!(reference_context.contains("directory"));
        assert_eq!(
            messages
                .last()
                .and_then(|message| message.content.as_deref()),
            Some("分析引用内容")
        );
    }

    #[test]
    fn build_session_turn_messages_injects_current_turn_knowledge_as_system_fragment() {
        let session_id = SessionId::new("session-knowledge-context");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "knowledge context")
            .expect("session should be created");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-knowledge-context".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-knowledge-context")),
            prompt: "为什么采用单一事实源架构？".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: Some("/tmp/current-project".to_string()),
        };

        let messages = build_session_turn_messages(
            None,
            &store,
            &request,
            &request.prompt,
            Some("[reference:knowledge:adr] 单一事实源\n只读投影来自事件事实。"),
        );

        let knowledge = messages
            .iter()
            .find(|message| {
                message.role == "system"
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("kind=\"knowledge_context\""))
            })
            .expect("knowledge context should be injected as a system fragment");
        assert!(
            knowledge
                .content
                .as_deref()
                .is_some_and(|content| content.contains("只读投影来自事件事实"))
        );
        assert_eq!(
            messages
                .last()
                .and_then(|message| message.content.as_deref()),
            Some("为什么采用单一事实源架构？")
        );
    }

    #[test]
    fn build_session_turn_messages_attaches_current_user_images() {
        let session_id = SessionId::new("session-current-image");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "current image")
            .expect("session should be created");
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-current-image".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("识别图片".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-current-image".to_string(),
            workspace_id: None,
            prompt: "识别图片".to_string(),
            images: vec![
                SessionTurnImage::from_data_url("paste.png", "data:image/png;base64,AAA")
                    .expect("image should parse"),
            ],
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);
        let current_user_message = messages.last().expect("current user message");

        assert_eq!(current_user_message.role, "user");
        assert_eq!(current_user_message.content.as_deref(), Some("识别图片"));
        assert_eq!(current_user_message.images.len(), 1);
        assert_eq!(current_user_message.images[0].media_type, "image/png");
        assert_eq!(current_user_message.images[0].data, "AAA");
    }

    #[test]
    fn build_session_turn_messages_does_not_inject_workspace_context_without_tools() {
        let session_id = SessionId::new("session-workspace-chat");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "workspace chat")
            .expect("session should be created");
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-workspace-chat".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("解释一下当前状态".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should be stored");
        let request = SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-workspace-chat".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context")),
            prompt: "解释一下当前状态".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: Some("/tmp/current-project".to_string()),
        };

        let messages = build_session_turn_messages(None, &store, &request, &request.prompt, None);

        assert_eq!(
            messages
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "user"]
        );
        assert!(
            messages[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("上下文优先级"))
        );
        assert!(
            !messages[0]
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("/tmp/current-project")
        );
        assert_eq!(messages[1].content.as_deref(), Some("解释一下当前状态"));
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
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: Some("request-post-tool-final-item".to_string()),
            user_message_id: Some("user-post-tool-final-item".to_string()),
            placeholder_message_id: Some("placeholder-post-tool-final-item".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
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
            FinalItemInput {
                content: "最终答案来自工具后轮次。",
                item_id: Some("turn-item-assistant-stream-post-tool"),
                timeline_entry_id: Some("turn-item-assistant-stream-main"),
            },
            &orchestrator_thread_id,
            None,
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
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: Some("request-terminal-duration".to_string()),
            user_message_id: Some("user-terminal-duration".to_string()),
            placeholder_message_id: Some("placeholder-terminal-duration".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        append_final_item(
            &event_bus,
            &store,
            &request,
            FinalItemInput {
                content: "最终回复",
                item_id: None,
                timeline_entry_id: None,
            },
            &orchestrator_thread_id,
            None,
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
