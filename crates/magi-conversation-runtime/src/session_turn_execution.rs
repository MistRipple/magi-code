//! 任务系统 — session turn execution
//!
//! 错误返回值改为 `Result<_, String>`，调用方在 magi-api 边界用
//! `.map_err(|msg| ApiError::model_invocation_failed("执行 session turn 失败", msg))`
//! 等方式桥接到 `ApiError` 枚举。

use crate::context_authority::{
    ContextAuthority, ContextCompactionTerminal, ContextPrepareRequest, current_session_file_facts,
    estimate_chat_messages_tokens, estimate_tool_definition_tokens,
};
#[cfg(test)]
use crate::context_authority::{ContextCompactionProgress, ContextCompactionRecord};
use crate::model_context_window::{
    conservative_context_limit_recovery_window, resolve_model_context_window_with_override,
};
use crate::{
    ConversationRegistry, GoalModeLifecycleState, SessionTurnInputBoundary, UserSignal,
    conversation_loop::{
        append_thread_messages_checkpoint, chat_message_to_thread_chat_message,
        insert_interrupted_tool_result_messages, thread_chat_message_to_chat_message,
    },
    goal_mode_required_tool_chain, goal_mode_requires_terminalization,
    goal_mode_tool_batch_violation,
    model_config::{resolve_orchestrator_model_config, resolve_vision_execution_config},
    model_error::{
        MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS, MODEL_PRE_OUTPUT_RECOVERY_MAX_ATTEMPTS,
        MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS, ModelFailureDiagnostic,
        classify_model_invocation_error, model_empty_response_recovery_prompt,
        model_stream_interruption_recovery_prompt, public_model_image_invocation_error_message,
    },
    prompt_utils::{
        PromptFragmentKind, current_turn_context_priority_prompt, dynamic_skill_prompt_message,
        normalize_model_stream_preview_content, normalize_model_visible_content,
        skill_prompt_message, system_prompt_fragment_message, workspace_context_system_prompt,
    },
    session_images::{SessionTurnImage, session_turn_image_sources},
    session_writeback::{
        ContextCompactionWritebackContext, SessionStatePersistCallback,
        SessionTurnStreamPublishGate, append_session_tool_call_items_batch_with_context,
        append_session_turn_error_item, append_session_turn_item, apply_model_response_round,
        new_context_compaction_item_id, persist_session_state_checkpoint,
        publish_current_session_turn_item_event, publish_model_retry_runtime_event,
        publish_session_turn_item_event, publish_session_turn_item_stream_event, session_turn_item,
        session_turn_stream_update, upsert_context_compaction_completed_notice,
        upsert_context_compaction_progress_notice, upsert_session_turn_item,
    },
    strict_goal_mode_tool_definitions_for_round,
    tool_call_validation::{
        ToolCallFailureDiagnostic, ToolCallValidationIssue, ToolCallValidationTracker,
        invalid_tool_result_message, validate_tool_call_batch,
    },
    tool_execution_ledger::ToolExecutionLedger,
    tool_surface_state::{
        BrowserToolSurfaceContext, activate_skill_tool_definitions,
        refresh_live_browser_tool_definitions, refresh_live_mcp_tool_definitions,
    },
    usage_recording::{
        ContextUsageRuntimeTracker, ContextUsageRuntimeTrackerInput, ModelUsageBinding,
        account_active_goal_usage, publish_model_usage_record, resolved_model_for_usage_binding,
        resolved_provider_for_usage_binding, session_turn_model_usage_binding,
        vision_model_usage_binding,
    },
};
use magi_bridge_client::{
    ChatMessage, ChatToolChoice, ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest,
    ModelProviderContext, ModelResponseStatus, ModelStreamingDelta,
};
use magi_core::{AccessProfile, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{CanonicalTurnItemKind, SessionStore, ThreadChatMessage};
use magi_settings_store::SettingsStore;
use magi_snapshot::SnapshotManager;
use magi_tool_runtime::ToolRegistry;
use magi_usage_authority::UsageCallStatus;
use std::{collections::BTreeSet, fmt, path::PathBuf, sync::Arc};

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
    ModelResponseInvalid,
    ModelImageInvocationFailed,
    ToolCallProtocolFailed,
    ContextCompactionFailed,
    RuntimeInvalidState,
}

impl SessionTurnFailureReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::ModelInvocationFailed => "model_invocation_failed",
            Self::ModelStreamInterrupted => "model_stream_interrupted",
            Self::ModelEmptyResponse => "model_empty_response",
            Self::ModelEmptyResponseAfterTools => "model_empty_response_after_tools",
            Self::ModelResponseInvalid => "model_response_invalid",
            Self::ModelImageInvocationFailed => "model_image_invocation_failed",
            Self::ToolCallProtocolFailed => "tool_arguments_invalid",
            Self::ContextCompactionFailed => "context_compaction_failed",
            Self::RuntimeInvalidState => "session_turn_runtime_invalid_state",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTurnExecutionError {
    pub reason: SessionTurnFailureReason,
    pub diagnostic_code: String,
    pub public_message: String,
    pub model_failure: Option<Box<ModelFailureDiagnostic>>,
    pub(crate) tool_call_failure: Option<Box<ToolCallFailureDiagnostic>>,
}

impl SessionTurnExecutionError {
    fn new(reason: SessionTurnFailureReason, public_message: impl Into<String>) -> Self {
        Self {
            reason,
            diagnostic_code: reason.code().to_string(),
            public_message: public_message.into(),
            model_failure: None,
            tool_call_failure: None,
        }
    }

    pub(crate) fn from_model_failure(
        reason: SessionTurnFailureReason,
        model_failure: ModelFailureDiagnostic,
    ) -> Self {
        Self {
            reason,
            diagnostic_code: model_failure.code.clone(),
            public_message: model_failure.summary.clone(),
            model_failure: Some(Box::new(model_failure)),
            tool_call_failure: None,
        }
    }

    fn from_tool_call_failure(tool_call_failure: ToolCallFailureDiagnostic) -> Self {
        Self {
            reason: SessionTurnFailureReason::ToolCallProtocolFailed,
            diagnostic_code: tool_call_failure.code.clone(),
            public_message: tool_call_failure.summary.clone(),
            model_failure: None,
            tool_call_failure: Some(Box::new(tool_call_failure)),
        }
    }

    fn runtime_invalid_state() -> Self {
        Self::new(
            SessionTurnFailureReason::RuntimeInvalidState,
            "对话运行状态异常，请重新发送。",
        )
    }

    fn context_compaction_failed() -> Self {
        Self::new(
            SessionTurnFailureReason::ContextCompactionFailed,
            "上下文压缩失败，本轮已停止。请检查辅助模型配置或网络后重试。",
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

fn canonical_session_turn_history(
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
) -> Vec<ThreadChatMessage> {
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
    accepted_at
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
                    if !item.visibility.renderable {
                        return None;
                    }
                    let is_orchestrator_item = &item.source_thread_id == orchestrator_thread_id;
                    let is_root_final_item = item.kind == CanonicalTurnItemKind::AssistantText
                        && item
                            .metadata
                            .get("assistantOutputKind")
                            .and_then(|value| value.as_str())
                            == Some("final")
                        && item.worker.as_ref().is_some_and(|worker| {
                            worker.task_id.is_some()
                                && worker.worker_id.is_none()
                                && worker.role_id.is_none()
                        });
                    // 普通 session turn 的最终回复由 coordinator task 负责落盘，
                    // source_thread_id 可能不是 orchestrator thread；它仍是主线事实，
                    // 必须进入后续回合上下文。带 worker/role 的 sidechain final 继续排除。
                    if !is_orchestrator_item && !is_root_final_item {
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
                        provider_context: Vec::new(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn started_tool_call_ids_for_session_thread(
    session_store: &SessionStore,
    session_id: &SessionId,
    thread_id: &magi_core::ThreadId,
) -> BTreeSet<String> {
    session_store
        .canonical_turns_for_session(session_id)
        .into_iter()
        .flat_map(|turn| turn.items)
        .filter(|item| item.kind == CanonicalTurnItemKind::ToolCall)
        .filter(|item| item.source_thread_id == *thread_id)
        .filter_map(|item| item.tool.map(|tool| tool.call_id))
        .collect()
}

fn normalize_interrupted_session_tool_history(
    session_store: &SessionStore,
    session_id: &SessionId,
    thread_id: &magi_core::ThreadId,
    persist_session_state: Option<&SessionStatePersistCallback>,
) -> usize {
    let mut history = session_store.thread_message_history(thread_id);
    let inserted = insert_interrupted_tool_result_messages(
        &mut history,
        &started_tool_call_ids_for_session_thread(session_store, session_id, thread_id),
    );
    if inserted == 0 {
        return 0;
    }
    session_store.replace_thread_messages(thread_id, history, UtcMillis::now());
    persist_session_state_checkpoint(
        persist_session_state,
        "session_turn_interrupted_tool_result",
    );
    inserted
}

fn build_session_turn_messages(
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    prompt: &str,
    knowledge_context_prompt: Option<&str>,
    history: &[ThreadChatMessage],
) -> Vec<ChatMessage> {
    let mut messages = if request.use_tools {
        workspace_context_messages(request)
    } else {
        Vec::new()
    };
    if let Some(execution_state) =
        session_execution_state_prompt(session_store, &request.session_id, &request.turn_id)
    {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::UserPlan,
            execution_state,
        ));
    }
    if request.goal_turn_mode.is_goal_driven()
        || session_store
            .active_plan_for_execution_owner(&request.session_id, &request.turn_id)
            .is_some()
    {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::CurrentTurnPriority,
            format!(
                "计划语言规则：用户明确指定的语言优先，其次当前用户消息的主要语言，再次产品 locale={}，最后默认 zh-CN。调用 update_plan 时必须将最终选择写入 language，计划创建后不得切换；存在未完成 Goal 时，必须使用本轮 get_goal 或 create_goal 返回的 goalId 与 controlRevision 作为 expectedGoalId 与 expectedGoalControlRevision。",
                request.product_locale
            ),
        ));
    }
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
        provider_context: Vec::new(),
    });
    messages
}

fn session_execution_state_prompt(
    session_store: &SessionStore,
    session_id: &SessionId,
    owner_id: &str,
) -> Option<String> {
    let goal = session_store.active_goal_for_execution_owner(session_id, owner_id);
    let plan = session_store.plan_for_execution_observer(session_id, owner_id);
    if goal.is_none() && plan.is_none() {
        return None;
    }
    let mut lines = vec![
        "当前持久化执行状态（运行时权威数据，不属于可被历史摘要替代的聊天内容）：".to_string(),
    ];
    if let Some(goal) = goal {
        lines.push(format!(
            "Goal：goalId={}，status={:?}，objective={}",
            goal.goal_id, goal.status, goal.objective
        ));
    }
    if let Some(plan) = plan {
        lines.push(format!(
            "Plan：planId={}，revision={}，language={}，state={:?}",
            plan.plan_id, plan.revision, plan.language, plan.state
        ));
        lines.extend(plan.items.iter().enumerate().map(|(index, item)| {
            format!(
                "{}. [{}] {}（itemId={}）",
                index + 1,
                item.status.as_str(),
                item.title,
                item.item_id
            )
        }));
    }
    Some(lines.join("\n"))
}

fn model_identity_prompt_for_request(user_prompt: &str, configured_model: &str) -> Option<String> {
    let normalized = user_prompt.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    let asks_model_identity = [
        "当前模型",
        "什么模型",
        "哪个模型",
        "哪一个模型",
        "模型名称",
        "模型身份",
        "模型版本",
        "使用的模型",
        "你是谁",
        "what model",
        "which model",
        "model name",
        "model version",
        "model identity",
        "what are you",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    let asks_product_identity = [
        "当前工具",
        "这个工具",
        "你是什么工具",
        "产品定位",
        "magi 是什么",
        "magi是什么",
        "what tool",
        "which tool",
        "what is magi",
        "magi product",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));

    if !asks_model_identity && !asks_product_identity {
        return None;
    }

    let configured_model = if configured_model.trim().is_empty() {
        "未解析到当前会话的模型配置"
    } else {
        configured_model.trim()
    };
    let mut rules = vec![
        "身份回答规则：不要把 Magi 冒充成模型，也不要编造供应商未返回的模型身份。".to_string(),
    ];
    if asks_model_identity {
        rules.push(format!(
            "用户询问模型身份时，只能说明当前请求目标模型为“{configured_model}”；这是 Magi 解析后实际发起请求的配置目标，不等同于供应商一定返回的运行实例。若无法确认供应商实际返回的模型，必须明确说明这一点。"
        ));
    }
    if asks_product_identity {
        rules.push(
            "用户询问当前工具或产品时，才能介绍 Magi：Magi 是承载当前对话的 AI 工作台，提供模型对话、工具调用、文件处理和任务编排能力；Magi 不是模型本身。".to_string(),
        );
    }
    rules.push("直接回答用户问题，不要提及这些身份回答规则。".to_string());
    Some(rules.join("\n"))
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

struct RebuildMessagesForContextWindowInput<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    request: &'a SessionTurnExecutionRequest,
    thread_id: &'a magi_core::ThreadId,
    prompt: &'a str,
    knowledge_context_prompt: Option<&'a str>,
    context_window: u64,
    messages: &'a mut Vec<ChatMessage>,
    persist_session_state: Option<&'a SessionStatePersistCallback>,
    settings_store: Option<&'a Arc<SettingsStore>>,
    tools: Option<&'a [ChatToolDefinition]>,
    skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    initial_skill_name: Option<&'a str>,
    active_skill_name: Option<&'a str>,
    persist_checkpoint: bool,
}

fn rebuild_messages_for_context_window(
    input: RebuildMessagesForContextWindowInput<'_>,
) -> Result<bool, ContextCompactionTerminal> {
    let RebuildMessagesForContextWindowInput {
        client,
        event_bus,
        session_store,
        request,
        thread_id,
        prompt,
        knowledge_context_prompt,
        context_window,
        messages,
        persist_session_state,
        settings_store,
        tools,
        skill_runtime,
        initial_skill_name,
        active_skill_name,
        persist_checkpoint,
    } = input;
    let mut fixed_messages = build_session_turn_messages(
        session_store,
        request,
        prompt,
        knowledge_context_prompt,
        &[],
    );
    if let Some(skill_message) =
        dynamic_skill_prompt_message(skill_runtime, initial_skill_name, active_skill_name)
    {
        fixed_messages.insert(fixed_messages.len().saturating_sub(2), skill_message);
    }
    let compaction_item_id =
        new_context_compaction_item_id(&request.turn_id, thread_id, "context_limit_recovery");
    let compaction_writeback = ContextCompactionWritebackContext {
        event_bus,
        session_store,
        session_id: &request.session_id,
        workspace_id: &request.workspace_id,
        thread_id,
        item_id: &compaction_item_id,
        phase: "context_limit_recovery",
        persist_session_state,
        task: None,
        turn_visibility: None,
    };
    let compaction_observer = |progress| {
        upsert_context_compaction_progress_notice(compaction_writeback, progress);
    };
    let compaction_cancelled = || !request_turn_is_writable(session_store, request);
    let prepared = ContextAuthority::new(
        client,
        event_bus,
        session_store,
        &request.session_id,
        &request.workspace_id,
        thread_id,
        settings_store,
    )
    .with_compaction_runtime(&compaction_observer, &compaction_cancelled)
    .prepare(ContextPrepareRequest {
        fallback_history: Vec::new(),
        phase: "context_limit_recovery",
        context_window_override: Some(context_window),
        additional_token_estimate: estimate_chat_messages_tokens(&fixed_messages)
            .saturating_add(estimate_tool_definition_tokens(tools)),
        persist_checkpoint,
        model_identity: None,
        force_compaction: true,
    });
    if let Some(terminal) = prepared.terminal {
        return Err(terminal);
    }
    let compacted = prepared.compaction.is_some();
    if let Some(compaction) = prepared.compaction.as_ref() {
        upsert_context_compaction_completed_notice(compaction_writeback, compaction);
    }
    let mut history = prepared.messages;
    if history
        .last()
        .is_some_and(|message| message.role == "user" && message.content.as_deref() == Some(prompt))
    {
        history.pop();
    }
    *messages = build_session_turn_messages(
        session_store,
        request,
        prompt,
        knowledge_context_prompt,
        &history,
    );
    if let Some(skill_message) =
        dynamic_skill_prompt_message(skill_runtime, initial_skill_name, active_skill_name)
    {
        messages.insert(messages.len().saturating_sub(2), skill_message);
    }
    Ok(compacted)
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
    pub live_settings_store: Option<Arc<SettingsStore>>,
}

pub fn run_session_turn_execution(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    let plan_store = runtime.plan_store;
    let session_store = runtime.session_store;
    let session_id = runtime.request.session_id.clone();
    let turn_id = runtime.request.turn_id.clone();
    let workspace_id = runtime.request.workspace_id.clone();
    let event_bus = runtime.event_bus;
    let result = run_session_turn_execution_inner(runtime);
    if result.is_err() {
        let owns_active_plan = session_store
            .active_plan_for_execution_owner(&session_id, &turn_id)
            .is_some();
        let stopped_goal = session_store
            .active_goal_for_execution_owner(&session_id, &turn_id)
            .map(|goal| {
                session_store.stop_goal_for_runtime_failure(
                    &session_id,
                    &goal.goal_id,
                    None,
                    &turn_id,
                    "session_turn_failed",
                )
            })
            .transpose();
        let plan_result = match stopped_goal {
            Ok(Some(_)) => Ok(session_store.plan(&session_id)),
            Ok(None) if owns_active_plan => plan_store.pause(),
            Ok(None) => Ok(None),
            Err(error) => Err(magi_plan::PlanUpdateError::Store(error.to_string())),
        };
        match plan_result {
            Ok(Some(plan)) => magi_plan::publish_plan_event(
                event_bus,
                magi_plan::plan_event_type(&plan),
                &plan,
                workspace_id.as_ref(),
                None,
                None,
            ),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                session_id = %session_id,
                %error,
                "对话轮次失败后停止 Goal/Plan 失败"
            ),
        }
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
        live_settings_store: _live_settings_store,
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

    normalize_interrupted_session_tool_history(
        session_store,
        &request.session_id,
        &orchestrator_thread_id,
        persist_session_state,
    );
    let fallback_history = canonical_session_turn_history(session_store, &request);
    let selected_model = settings_store
        .and_then(|store| resolve_orchestrator_model_config(store, Some(&request.session_id)).ok())
        .and_then(|config| config.to_usage_llm_config())
        .map(|config| config.model)
        .unwrap_or_default();
    let current_turn_contains_images = !request.images.is_empty();
    let vision_execution_config = resolve_vision_execution_config(
        settings_store.map(Arc::as_ref),
        &selected_model,
        current_turn_contains_images,
    )
    .map_err(|error| {
        SessionTurnExecutionError::new(SessionTurnFailureReason::ModelImageInvocationFailed, error)
    })?;
    let vision_client = vision_execution_config
        .as_ref()
        .and_then(|config| config.to_http_vision_client());
    let client: &dyn ModelBridgeClient = vision_client
        .as_ref()
        .map(|client| client as &dyn ModelBridgeClient)
        .unwrap_or(client);
    let resolved_context_model = vision_execution_config
        .as_ref()
        .and_then(|config| config.require_model().ok())
        .unwrap_or(&selected_model)
        .to_string();
    let usage_binding = if vision_execution_config.is_some() {
        vision_model_usage_binding()
    } else {
        session_turn_model_usage_binding(request.use_tools)
    };
    let mut effective_context_window = resolve_model_context_window_with_override(
        settings_store.map(Arc::as_ref),
        &resolved_context_model,
        vision_execution_config
            .as_ref()
            .and_then(|config| config.context_window_tokens()),
    );
    let fixed_messages = build_session_turn_messages(
        session_store,
        &request,
        &prompt,
        knowledge_context_prompt.as_deref(),
        &[],
    );
    let compaction_item_id =
        new_context_compaction_item_id(&request.turn_id, &orchestrator_thread_id, "pre_turn");
    let compaction_writeback = ContextCompactionWritebackContext {
        event_bus,
        session_store,
        session_id: &request.session_id,
        workspace_id: &request.workspace_id,
        thread_id: &orchestrator_thread_id,
        item_id: &compaction_item_id,
        phase: "pre_turn",
        persist_session_state,
        task: None,
        turn_visibility: None,
    };
    let compaction_observer = |progress| {
        upsert_context_compaction_progress_notice(compaction_writeback, progress);
    };
    let compaction_cancelled = || !request_turn_is_writable(session_store, &request);
    let prepared_history = ContextAuthority::new(
        client,
        event_bus,
        session_store,
        &request.session_id,
        &request.workspace_id,
        &orchestrator_thread_id,
        settings_store,
    )
    .with_compaction_runtime(&compaction_observer, &compaction_cancelled)
    .prepare(ContextPrepareRequest {
        fallback_history,
        phase: "pre_turn",
        context_window_override: Some(effective_context_window),
        additional_token_estimate: estimate_chat_messages_tokens(&fixed_messages)
            .saturating_add(estimate_tool_definition_tokens(tools.as_deref())),
        persist_checkpoint: vision_execution_config.is_none(),
        model_identity: Some(magi_usage_authority::ModelIdentitySnapshot::new(
            vision_execution_config
                .as_ref()
                .map(|_| "vision")
                .unwrap_or("configured"),
            resolved_context_model.clone(),
            0,
        )),
        force_compaction: false,
    });
    if let Some(terminal) = prepared_history.terminal {
        return match terminal {
            ContextCompactionTerminal::Cancelled => Ok(SessionTurnExecutionOutput::interrupted()),
            ContextCompactionTerminal::Failed => {
                Err(SessionTurnExecutionError::context_compaction_failed())
            }
        };
    }
    if let Some(compaction) = prepared_history.compaction.as_ref() {
        upsert_context_compaction_completed_notice(compaction_writeback, compaction);
    }
    let mut proactive_context_compaction_completed = prepared_history.compaction.is_some();
    let mut messages = build_session_turn_messages(
        session_store,
        &request,
        &prompt,
        knowledge_context_prompt.as_deref(),
        &prepared_history.messages,
    );
    if let Some(identity_prompt) =
        model_identity_prompt_for_request(&request.prompt, &resolved_context_model)
    {
        messages.insert(
            0,
            system_prompt_fragment_message(
                PromptFragmentKind::CurrentTurnPriority,
                identity_prompt,
            ),
        );
    }
    if let Some(current_user_message) = messages.last() {
        let mut persisted_user_message = chat_message_to_thread_chat_message(current_user_message);
        // 原图由 canonical turn 负责审计与 UI 展示。thread 历史只保留文本语义，
        // 避免后续纯文本回合把历史图片再次发送给主模型。
        persisted_user_message.images.clear();
        session_store.append_thread_messages(
            &orchestrator_thread_id,
            vec![persisted_user_message],
            UtcMillis::now(),
        );
        persist_session_state_checkpoint(persist_session_state, "session_turn_thread_user");
    }
    let mut final_content: Option<String> = None;
    let mut final_item_id: Option<String> = None;
    let mut final_model_round: Option<usize> = None;
    let mut main_timeline_entry_id: Option<String> = None;
    let mut had_tool_calls = false;
    let initial_skill_name = skill_name.clone();
    let mut active_skill_name = skill_name;
    let mut active_tools = tools.unwrap_or_default();
    let mut tool_execution_ledger = ToolExecutionLedger::from_thread_history(
        &request.prompt,
        &session_store.thread_message_history(&orchestrator_thread_id),
        tool_registry,
    )
    .with_current_file_facts(
        &current_session_file_facts(session_store, &request.session_id),
        request
            .workspace_root_path
            .as_deref()
            .map(std::path::Path::new),
    );
    let mut completed_required_tool_names: Vec<String> = Vec::new();
    let mut goal_creation_required = request.goal_turn_mode.is_goal_driven()
        && request.goal_turn_mode.allows_goal_creation()
        && session_store
            .current_unfinished_goal(&request.session_id)
            .is_none();
    let mut required_tool_chain = session_required_tool_chain(
        &request,
        goal_creation_required,
        goal_mode_requires_terminalization(session_store, &request.session_id),
    );
    let mut context_budget_recheck_required = false;
    let mut empty_response_recovery_attempts = 0usize;
    let mut pre_output_invocation_recovery_attempts = 0usize;
    let mut stream_interruption_recovery_attempts = 0usize;
    let mut context_limit_recovery_attempted = false;
    let mut tool_call_validation_tracker = ToolCallValidationTracker::default();
    let mut last_response_observation: Option<String> = None;
    let mut round = 0usize;
    loop {
        let mut browser_capability_revision = None;
        if request.use_tools
            && let Some(registry) = tool_registry
        {
            let browser_surface = refresh_live_browser_tool_definitions(
                active_tools,
                registry,
                BrowserToolSurfaceContext::new(
                    skill_runtime,
                    active_skill_name.as_deref(),
                    request.access_profile,
                    None,
                    &[],
                    Some(&request.session_id),
                ),
            );
            active_tools = browser_surface.definitions;
            browser_capability_revision = browser_surface.capability_revision;
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
        let strict_goal_mode_round = request.goal_turn_mode.is_goal_driven();
        if strict_goal_mode_round {
            if session_store
                .current_unfinished_goal(&request.session_id)
                .is_some()
                || completed_required_tool_names
                    .iter()
                    .any(|tool_name| tool_name == "create_goal")
            {
                goal_creation_required = false;
            }
            required_tool_chain = session_required_tool_chain(
                &request,
                goal_creation_required,
                goal_mode_requires_terminalization(session_store, &request.session_id),
            );
            completed_required_tool_names.retain(|tool_name| {
                required_tool_chain
                    .iter()
                    .any(|required| required == tool_name)
            });
        }
        if strict_goal_mode_round
            && (!request.goal_turn_mode.allows_goal_creation()
                || !required_tool_chain
                    .iter()
                    .any(|tool_name| tool_name == "create_goal")
                || completed_required_tool_names
                    .iter()
                    .any(|tool_name| tool_name == "create_goal"))
        {
            active_tools.retain(|definition| definition.function.name != "create_goal");
        }
        let round_tool_definitions = if strict_goal_mode_round {
            strict_goal_mode_tool_definitions_for_round(
                &active_tools,
                &required_tool_chain,
                &completed_required_tool_names,
            )
        } else {
            active_tools.clone()
        };
        let round_tools = (request.use_tools && !round_tool_definitions.is_empty())
            .then_some(round_tool_definitions);
        if strict_goal_mode_round {
            if let Some(next_required_tool) = required_tool_chain.iter().find(|tool_name| {
                !completed_required_tool_names
                    .iter()
                    .any(|completed| completed == *tool_name)
            }) && !round_tools.as_ref().is_some_and(|tools| {
                tools
                    .iter()
                    .any(|definition| definition.function.name == *next_required_tool)
            }) {
                return Err(SessionTurnExecutionError::new(
                    SessionTurnFailureReason::RuntimeInvalidState,
                    format!(
                        "严格目标模式要求调用工具 {next_required_tool}，但当前工具面未提供该工具；拒绝绕过 Goal/Plan 生命周期。"
                    ),
                ));
            }
        }
        if context_budget_recheck_required && !proactive_context_compaction_completed {
            let rebuild_result =
                rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
                    client,
                    event_bus,
                    session_store,
                    request: &request,
                    thread_id: &orchestrator_thread_id,
                    prompt: &prompt,
                    knowledge_context_prompt: knowledge_context_prompt.as_deref(),
                    context_window: effective_context_window,
                    messages: &mut messages,
                    persist_session_state,
                    settings_store,
                    tools: round_tools.as_deref(),
                    skill_runtime,
                    initial_skill_name: initial_skill_name.as_deref(),
                    active_skill_name: active_skill_name.as_deref(),
                    persist_checkpoint: vision_execution_config.is_none(),
                });
            match rebuild_result {
                Ok(compacted) => {
                    proactive_context_compaction_completed |= compacted;
                }
                Err(ContextCompactionTerminal::Cancelled) => {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                Err(ContextCompactionTerminal::Failed) => {
                    return Err(SessionTurnExecutionError::context_compaction_failed());
                }
            }
        }
        context_budget_recheck_required = false;
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
                browser_capability_revision,
                messages: &mut messages,
                completed_required_tool_names: &completed_required_tool_names,
                required_tool_chain: &required_tool_chain,
                pre_output_invocation_recovery_attempts,
                stream_interruption_recovery_attempts,
                round,
                orchestrator_thread_id: &orchestrator_thread_id,
                orchestrator_mission_id: &orchestrator_mission_id,
                persist_session_state,
                tool_execution_ledger: &mut tool_execution_ledger,
            },
            tool_registry,
            skill_runtime,
            skill_dispatch_runtime,
            active_skill_name.as_deref(),
        ) {
            Ok(output) => output,
            Err(SessionTurnRoundError::StreamInterruptedRecovered) => {
                stream_interruption_recovery_attempts += 1;
                round = round.saturating_add(1);
                continue;
            }
            Err(SessionTurnRoundError::PreOutputInvocationRecovered) => {
                pre_output_invocation_recovery_attempts += 1;
                round = round.saturating_add(1);
                continue;
            }
            Err(SessionTurnRoundError::InvalidResponse(model_failure)) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let execution_error = SessionTurnExecutionError::from_model_failure(
                    SessionTurnFailureReason::ModelResponseInvalid,
                    *model_failure,
                );
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
                        model_failure: execution_error.model_failure.as_deref(),
                        tool_call_failure: None,
                        streaming_entry_id: main_timeline_entry_id.as_deref(),
                        source_thread_id: orchestrator_thread_id.clone(),
                        persist_session_state,
                    },
                );
                return Err(execution_error);
            }
            Err(SessionTurnRoundError::Failed {
                error,
                context_overflow,
                non_stream_fallback_attempted,
            }) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let classification = classify_model_invocation_error(&error);
                if classification.code == "model_empty_response"
                    && empty_response_recovery_attempts < MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
                {
                    empty_response_recovery_attempts += 1;
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: Some(
                            model_empty_response_recovery_prompt(had_tool_calls).to_string(),
                        ),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        provider_context: Vec::new(),
                    });
                    tracing::warn!(
                        session_id = %request.session_id,
                        round,
                        attempt = empty_response_recovery_attempts,
                        max_attempts = MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS,
                        after_tool_calls = had_tool_calls,
                        "模型桥接空响应，追加用户可见答复约束后继续会话"
                    );
                    round = round.saturating_add(1);
                    continue;
                }
                if context_overflow.is_some() && !context_limit_recovery_attempted {
                    context_limit_recovery_attempted = true;
                    let reported_context_limit =
                        context_overflow.and_then(|overflow| overflow.context_limit_tokens);
                    effective_context_window = reported_context_limit.unwrap_or_else(|| {
                        conservative_context_limit_recovery_window(effective_context_window)
                    });
                    let compacted = match rebuild_messages_for_context_window(
                        RebuildMessagesForContextWindowInput {
                            client,
                            event_bus,
                            session_store,
                            request: &request,
                            thread_id: &orchestrator_thread_id,
                            prompt: &prompt,
                            knowledge_context_prompt: knowledge_context_prompt.as_deref(),
                            context_window: effective_context_window,
                            messages: &mut messages,
                            persist_session_state,
                            settings_store,
                            tools: Some(active_tools.as_slice()),
                            skill_runtime,
                            initial_skill_name: initial_skill_name.as_deref(),
                            active_skill_name: active_skill_name.as_deref(),
                            persist_checkpoint: vision_execution_config.is_none(),
                        },
                    ) {
                        Ok(compacted) => compacted,
                        Err(ContextCompactionTerminal::Cancelled) => {
                            return Ok(SessionTurnExecutionOutput::interrupted());
                        }
                        Err(ContextCompactionTerminal::Failed) => {
                            return Err(SessionTurnExecutionError::context_compaction_failed());
                        }
                    };
                    if compacted {
                        proactive_context_compaction_completed = true;
                        continue;
                    }
                }
                let retry_attempts = pre_output_invocation_recovery_attempts
                    + stream_interruption_recovery_attempts
                    + empty_response_recovery_attempts
                    + usize::from(non_stream_fallback_attempted);
                let execution_error = if classification.code == "model_empty_response" {
                    session_turn_empty_response_error(
                        &request,
                        had_tool_calls,
                        retry_attempts,
                        Some(&error),
                    )
                } else {
                    session_turn_model_error(&request, &error, retry_attempts)
                };
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
                        model_failure: execution_error.model_failure.as_deref(),
                        tool_call_failure: execution_error.tool_call_failure.as_ref().map(
                            |failure| {
                                serde_json::to_value(failure)
                                    .expect("tool call failure diagnostic must serialize")
                            },
                        ),
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
        if let Some(observation) = streamed_content.response_observation.as_ref() {
            last_response_observation = Some(observation.clone());
        }
        let response_provider_context = streamed_content.provider_context.clone();
        let repeated_tool_call_failure = if streamed_content.invalid_tool_calls.is_empty() {
            None
        } else {
            let attempts = tool_call_validation_tracker.record_round();
            if attempts >= 2 {
                streamed_content.invalid_tool_calls.first().map(|issue| {
                    ToolCallFailureDiagnostic::repeated(issue, attempts.saturating_sub(1))
                })
            } else {
                for issue in &streamed_content.invalid_tool_calls {
                    tracing::warn!(
                        session_id = %request.session_id,
                        round,
                        tool = %issue.tool_name,
                        reason_code = %issue.reason_code,
                        "模型提交了无效工具调用，已拒绝执行并请求模型修正"
                    );
                }
                None
            }
        };
        if let Some(tool_call_failure) = repeated_tool_call_failure {
            let execution_error =
                SessionTurnExecutionError::from_tool_call_failure(tool_call_failure);
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
                    model_failure: None,
                    tool_call_failure: execution_error.tool_call_failure.as_ref().map(|failure| {
                        serde_json::to_value(failure)
                            .expect("tool call failure diagnostic must serialize")
                    }),
                    streaming_entry_id: main_timeline_entry_id.as_deref(),
                    source_thread_id: orchestrator_thread_id.clone(),
                    persist_session_state,
                },
            );
            return Err(execution_error);
        }
        if main_timeline_entry_id.is_none() {
            main_timeline_entry_id = streamed_content.timeline_entry_id.clone();
        }
        had_tool_calls |= streamed_content.encountered_tool_calls;
        context_budget_recheck_required |= streamed_content.encountered_tool_calls;
        record_completed_required_tools(
            &mut completed_required_tool_names,
            &required_tool_chain,
            &streamed_content.tool_call_names,
        );
        if strict_goal_mode_round {
            if session_store
                .current_unfinished_goal(&request.session_id)
                .is_some()
                || completed_required_tool_names
                    .iter()
                    .any(|tool_name| tool_name == "create_goal")
            {
                goal_creation_required = false;
            }
            required_tool_chain = session_required_tool_chain(
                &request,
                goal_creation_required,
                goal_mode_requires_terminalization(session_store, &request.session_id),
            );
            completed_required_tool_names.retain(|tool_name| {
                required_tool_chain
                    .iter()
                    .any(|required| required == tool_name)
            });
        }

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
            if let Some(skill_message) = skill_prompt_message(runtime, skill_id) {
                messages.push(skill_message);
            }
        }

        if let Some(content) = streamed_content.final_content {
            if !required_tool_chain_is_complete(
                &required_tool_chain,
                &completed_required_tool_names,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: response_provider_context.clone(),
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(required_tool_chain_recovery_prompt(
                        &required_tool_chain,
                        &completed_required_tool_names,
                    )),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
                let steers = conversation_registry
                    .drain_session_turn_steers(&request.session_id, &request.turn_id);
                append_session_turn_steers_to_messages(&mut messages, steers);
                round = round.saturating_add(1);
                continue;
            }
            if session_store
                .active_plan_for_execution_owner(&request.session_id, &request.turn_id)
                .is_some()
                && plan_store.requires_execution_follow_up()
            {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(content),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: response_provider_context.clone(),
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: plan_store.render_execution_follow_up_prompt(),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
                let steers = conversation_registry
                    .drain_session_turn_steers(&request.session_id, &request.turn_id);
                append_session_turn_steers_to_messages(&mut messages, steers);
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
                        provider_context: response_provider_context.clone(),
                    });
                    append_session_turn_steers_to_messages(&mut messages, steers);
                    round = round.saturating_add(1);
                    continue;
                }
                SessionTurnInputBoundary::Closed => {}
            }
            final_item_id = streamed_content.final_item_id;
            final_model_round = Some(round);
            final_content = Some(content);
            break;
        }
        if streamed_content.content_recovery_needed {
            if !response_provider_context.is_empty() {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: response_provider_context,
                });
            }
            if !required_tool_chain_is_complete(
                &required_tool_chain,
                &completed_required_tool_names,
            ) {
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(required_tool_chain_recovery_prompt(
                        &required_tool_chain,
                        &completed_required_tool_names,
                    )),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
            } else if empty_response_recovery_attempts < MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
            {
                empty_response_recovery_attempts += 1;
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(model_empty_response_recovery_prompt(had_tool_calls).to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
            } else {
                break;
            }
            round = round.saturating_add(1);
            continue;
        }
        let steers =
            conversation_registry.drain_session_turn_steers(&request.session_id, &request.turn_id);
        append_session_turn_steers_to_messages(&mut messages, steers);
        round = round.saturating_add(1);
    }

    let final_content = if let Some(content) = final_content {
        content
    } else {
        if !request_turn_is_writable(session_store, &request) {
            return Ok(SessionTurnExecutionOutput::interrupted());
        }
        let failure = session_turn_empty_response_error(
            &request,
            had_tool_calls,
            empty_response_recovery_attempts,
            last_response_observation.as_deref(),
        );
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
                model_failure: failure.model_failure.as_deref(),
                tool_call_failure: failure.tool_call_failure.as_ref().map(|failure| {
                    serde_json::to_value(failure)
                        .expect("tool call failure diagnostic must serialize")
                }),
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
            model_round: final_model_round,
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
            provider_context: Vec::new(),
        });
        appended = true;
    }
    appended
}

fn session_turn_model_error(
    request: &SessionTurnExecutionRequest,
    error: &str,
    retry_attempts: usize,
) -> SessionTurnExecutionError {
    if !request.images.is_empty() {
        let summary = public_model_image_invocation_error_message(error);
        return SessionTurnExecutionError::from_model_failure(
            SessionTurnFailureReason::ModelImageInvocationFailed,
            ModelFailureDiagnostic::image_failure(error, summary, retry_attempts),
        );
    }
    let classification = classify_model_invocation_error(error);
    let reason = if classification.code == "model_stream_interrupted" {
        SessionTurnFailureReason::ModelStreamInterrupted
    } else {
        SessionTurnFailureReason::ModelInvocationFailed
    };
    let stage = if classification.code == "model_stream_interrupted" {
        "response_stream"
    } else if classification.code == "model_empty_response" {
        "response_validation"
    } else {
        "request_dispatch"
    };
    SessionTurnExecutionError::from_model_failure(
        reason,
        ModelFailureDiagnostic::from_invocation(classification, error, stage, retry_attempts),
    )
}

fn session_turn_empty_response_error(
    request: &SessionTurnExecutionRequest,
    after_tool_calls: bool,
    retry_attempts: usize,
    response_observation: Option<&str>,
) -> SessionTurnExecutionError {
    if !request.images.is_empty() {
        let summary = public_model_image_invocation_error_message("empty stream response");
        return SessionTurnExecutionError::from_model_failure(
            SessionTurnFailureReason::ModelImageInvocationFailed,
            ModelFailureDiagnostic::image_failure(
                "模型请求成功结束，但未返回可见的图片分析结果。",
                summary,
                retry_attempts,
            ),
        );
    }
    let reason = if after_tool_calls {
        SessionTurnFailureReason::ModelEmptyResponseAfterTools
    } else {
        SessionTurnFailureReason::ModelEmptyResponse
    };
    SessionTurnExecutionError::from_model_failure(
        reason,
        ModelFailureDiagnostic::empty_response(
            after_tool_calls,
            retry_attempts,
            response_observation,
        ),
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
    browser_capability_revision: Option<u64>,
    messages: &'a mut Vec<ChatMessage>,
    completed_required_tool_names: &'a [String],
    required_tool_chain: &'a [String],
    pre_output_invocation_recovery_attempts: usize,
    stream_interruption_recovery_attempts: usize,
    round: usize,
    /// session 主线 thread：该 turn 内所有 session_turn_item 的 source_thread_id。
    orchestrator_thread_id: &'a magi_core::ThreadId,
    orchestrator_mission_id: &'a magi_core::MissionId,
    persist_session_state: Option<&'a SessionStatePersistCallback>,
    tool_execution_ledger: &'a mut ToolExecutionLedger,
}

struct SessionTurnRoundOutput {
    final_content: Option<String>,
    final_item_id: Option<String>,
    timeline_entry_id: Option<String>,
    encountered_tool_calls: bool,
    tool_call_names: Vec<String>,
    activated_skill_id: Option<String>,
    content_recovery_needed: bool,
    invalid_tool_calls: Vec<ToolCallValidationIssue>,
    response_observation: Option<String>,
    provider_context: Vec<ModelProviderContext>,
    interrupted: bool,
}

/// 单轮流式调用的失败语义。仅在未交付可见内容时才能重放请求；已交付片段时必须续写。
#[derive(Debug)]
enum SessionTurnRoundError {
    Failed {
        error: String,
        context_overflow: Option<magi_bridge_client::ContextOverflowInfo>,
        non_stream_fallback_attempted: bool,
    },
    InvalidResponse(Box<ModelFailureDiagnostic>),
    PreOutputInvocationRecovered,
    StreamInterruptedRecovered,
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

fn session_required_tool_chain(
    request: &SessionTurnExecutionRequest,
    goal_creation_required: bool,
    goal_terminalization_required: bool,
) -> Vec<String> {
    let mut required = if request.goal_turn_mode.is_goal_driven() {
        goal_mode_required_tool_chain(
            GoalModeLifecycleState {
                goal_creation_required,
                goal_terminalization_required,
            },
            &request.required_tool_chain,
        )
    } else {
        request.required_tool_chain.clone()
    };
    if let Some(forced_tool_name) = request
        .forced_tool_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        && !required.iter().any(|name| name == forced_tool_name)
    {
        if request.goal_turn_mode.is_goal_driven() {
            // 目标模式的生命周期顺序由运行时拥有；只允许在生命周期之后追加业务工具。
            if !matches!(
                forced_tool_name,
                "get_goal" | "create_goal" | "update_plan" | "update_goal"
            ) {
                required.push(forced_tool_name.to_string());
            }
        } else {
            required.insert(0, forced_tool_name.to_string());
        }
    }
    required
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

fn stream_session_turn_round(
    runtime: SessionTurnRoundRuntime<'_>,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    skill_dispatch_runtime: Option<&magi_skill_runtime::SkillDispatchRuntime>,
    skill_name: Option<&str>,
) -> Result<SessionTurnRoundOutput, SessionTurnRoundError> {
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
        browser_capability_revision,
        messages,
        completed_required_tool_names,
        required_tool_chain,
        pre_output_invocation_recovery_attempts,
        stream_interruption_recovery_attempts,
        round,
        orchestrator_thread_id,
        orchestrator_mission_id,
        persist_session_state,
        tool_execution_ledger,
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
    let call_id = format!("session-turn-{round}-{}", UtcMillis::now().0);
    let resolved_model =
        resolved_model_for_usage_binding(settings_store, usage_binding, &request.session_id)
            .unwrap_or_default();
    let resolved_provider =
        resolved_provider_for_usage_binding(settings_store, usage_binding, &request.session_id);
    let prefill_tokens = estimate_chat_messages_tokens(messages)
        .saturating_add(estimate_tool_definition_tokens(tools.as_deref()));
    let context_usage_tracker = usage_binding.tracks_active_context().then(|| {
        ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
            event_bus,
            settings_store: settings_store.map(Arc::as_ref),
            session_id: &request.session_id,
            workspace_id: &request.workspace_id,
            turn_id: Some(&request.turn_id),
            call_id: &call_id,
            resolved_model: &resolved_model,
            prefill_tokens: prefill_tokens as u64,
            thread_id: Some(orchestrator_thread_id),
            model_provider: resolved_provider,
            binding_revision: usage_binding.binding_revision(),
            checkpoint_generation: session_store
                .thread_context_checkpoint(orchestrator_thread_id)
                .map(|checkpoint| checkpoint.generation)
                .unwrap_or_default(),
        })
    });
    let on_delta = |delta: &ModelStreamingDelta| {
        if !request_turn_is_writable(session_store, request) {
            writeback_aborted.set(true);
            return;
        }
        if let Some(tracker) = context_usage_tracker.as_ref() {
            tracker.observe_accumulated_output(&delta.content, &delta.thinking);
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
            apply_model_response_round(&mut item, round);
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
        apply_model_response_round(&mut item, round);
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
        required_tool_chain,
        tools.as_ref(),
        round,
        completed_required_tool_names,
    );
    let round_goal_id = session_store
        .active_goal_for_execution_owner(&request.session_id, &request.turn_id)
        .map(|goal| goal.goal_id);
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
    let invocation_request = ModelInvocationRequest {
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt: prompt.to_string(),
        messages: Some(messages.clone()),
        tools: tools.clone(),
        tool_choice,
    };
    let non_stream_fallback_template = invocation_request.clone();
    let response = match client.invoke_streaming_with_cancellation(
        invocation_request,
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
                    content_recovery_needed: false,
                    invalid_tool_calls: Vec::new(),
                    response_observation: None,
                    provider_context: Vec::new(),
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
                    call_id: call_id.clone(),
                    usage: None,
                    status: UsageCallStatus::Failed,
                    assignment_id: None,
                    error_code: Some(classification.code.to_string()),
                },
            );
            let partial_visible_content = streamed_visible_content.borrow().trim().to_string();
            let partial_thinking = streamed_thinking.borrow().trim().to_string();
            if partial_visible_content.is_empty()
                && partial_thinking.is_empty()
                && classification.retryable_before_output
                && pre_output_invocation_recovery_attempts < MODEL_PRE_OUTPUT_RECOVERY_MAX_ATTEMPTS
            {
                tracing::warn!(
                    session_id = %request.session_id,
                    round,
                    attempt = pre_output_invocation_recovery_attempts + 1,
                    max_attempts = MODEL_PRE_OUTPUT_RECOVERY_MAX_ATTEMPTS,
                    error_code = classification.code,
                    "模型未交付内容即发生暂态调用故障，重新执行同一轮请求"
                );
                return Err(SessionTurnRoundError::PreOutputInvocationRecovered);
            }
            if classification.code != "model_stream_interrupted" {
                return Err(SessionTurnRoundError::Failed {
                    error: raw_error,
                    context_overflow: error.context_overflow(),
                    non_stream_fallback_attempted: false,
                });
            }

            if !partial_thinking.is_empty() {
                let mut thinking_item = session_turn_item(
                    "assistant_thinking",
                    "completed",
                    Some("模型思考".to_string()),
                    Some(partial_thinking),
                    Some(thinking_item_id.clone()),
                    orchestrator_thread_id.clone(),
                );
                apply_request_aliases(&mut thinking_item, request);
                apply_model_response_round(&mut thinking_item, round);
                apply_goal_turn_intermediate_visibility(&mut thinking_item, request);
                if let Some(published) =
                    upsert_session_turn_item(session_store, &request.session_id, thinking_item)
                {
                    persist_session_state_checkpoint(
                        persist_session_state,
                        "session_turn_stream_interrupted_thinking",
                    );
                    publish_session_turn_item_event(
                        event_bus,
                        &request.session_id,
                        &request.workspace_id,
                        &published,
                    );
                }
            }
            if !partial_visible_content.is_empty() {
                let mut stream_item = session_turn_item(
                    "assistant_stream",
                    "completed",
                    Some("生成回复".to_string()),
                    Some(partial_visible_content.clone()),
                    Some(stream_item_id.clone()),
                    orchestrator_thread_id.clone(),
                );
                apply_request_aliases(&mut stream_item, request);
                apply_model_response_round(&mut stream_item, round);
                apply_goal_turn_intermediate_visibility(&mut stream_item, request);
                if let Some(published) =
                    upsert_session_turn_item(session_store, &request.session_id, stream_item)
                {
                    persist_session_state_checkpoint(
                        persist_session_state,
                        "session_turn_stream_interrupted_content",
                    );
                    publish_session_turn_item_event(
                        event_bus,
                        &request.session_id,
                        &request.workspace_id,
                        &published,
                    );
                }
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(partial_visible_content.clone()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
            }
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: Some(
                    model_stream_interruption_recovery_prompt(!partial_visible_content.is_empty())
                        .to_string(),
                ),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            });
            if stream_interruption_recovery_attempts
                < MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS
            {
                tracing::warn!(
                    session_id = %request.session_id,
                    round,
                    attempt = stream_interruption_recovery_attempts + 1,
                    max_attempts = MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS,
                    preserved_visible_chars = partial_visible_content.len(),
                    ?error,
                    "模型流中断，保留片段后继续会话"
                );
                return Err(SessionTurnRoundError::StreamInterruptedRecovered);
            }

            let mut fallback_request = non_stream_fallback_template;
            fallback_request.messages = Some(messages.clone());
            tracing::warn!(
                session_id = %request.session_id,
                round,
                preserved_visible_chars = partial_visible_content.len(),
                "流式恢复已耗尽，降级为非流式完成请求"
            );
            match client.invoke_with_cancellation(fallback_request, &|| {
                !request_turn_is_writable(session_store, request)
            }) {
                Ok(response) => response,
                Err(fallback_error) => {
                    let fallback_raw_error = fallback_error.to_string();
                    let fallback_classification =
                        classify_model_invocation_error(&fallback_raw_error);
                    publish_model_usage_record(
                        event_bus,
                        session_store,
                        settings_store,
                        crate::usage_recording::ModelUsageRecordInput {
                            session_id: &request.session_id,
                            workspace_id: &request.workspace_id,
                            binding: usage_binding,
                            call_id: format!("{call_id}-non-stream-fallback"),
                            usage: None,
                            status: UsageCallStatus::Failed,
                            assignment_id: None,
                            error_code: Some(fallback_classification.code.to_string()),
                        },
                    );
                    tracing::error!(
                        session_id = %request.session_id,
                        round,
                        ?fallback_error,
                        "非流式降级请求失败"
                    );
                    return Err(SessionTurnRoundError::Failed {
                        error: fallback_raw_error,
                        context_overflow: fallback_error.context_overflow(),
                        non_stream_fallback_attempted: true,
                    });
                }
            }
        }
    };
    let parsed = response;
    let response_observation = Some(format!(
        "模型轮次={}，status={:?}，finish_reason={}，正文字符数={}，thinking字符数={}，工具调用数={}",
        round + 1,
        parsed.status,
        parsed.finish_reason.as_deref().unwrap_or("<missing>"),
        parsed.content.as_deref().map(str::len).unwrap_or(0),
        parsed.thinking.as_deref().map(str::len).unwrap_or(0),
        parsed.tool_calls.len(),
    ));
    let tool_validation =
        validate_tool_call_batch(&parsed.tool_calls, tools.as_deref().unwrap_or_default());
    let has_actionable_output = parsed
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty())
        || !tool_validation.valid_calls.is_empty();
    let response_contract_failure = match parsed.status {
        ModelResponseStatus::Incomplete => Some(ModelFailureDiagnostic::incomplete_response(
            parsed.finish_reason.as_deref(),
            parsed.content.as_deref().map(str::len).unwrap_or(0),
        )),
        ModelResponseStatus::RequiresToolExecution if parsed.tool_calls.is_empty() => Some(
            ModelFailureDiagnostic::missing_tool_call(parsed.finish_reason.as_deref()),
        ),
        ModelResponseStatus::Completed | ModelResponseStatus::RequiresToolExecution => None,
    };
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
            status: if has_actionable_output && response_contract_failure.is_none() {
                UsageCallStatus::Success
            } else {
                UsageCallStatus::Failed
            },
            assignment_id: None,
            error_code: if let Some(failure) = response_contract_failure.as_ref() {
                Some(failure.code.clone())
            } else if !tool_validation.invalid_calls.is_empty()
                && tool_validation.valid_calls.is_empty()
            {
                tool_validation
                    .invalid_calls
                    .first()
                    .map(|invalid| invalid.issue.code.clone())
            } else {
                (!has_actionable_output).then(|| "model_empty_response".to_string())
            },
        },
    );
    account_active_goal_usage(
        session_store,
        &request.session_id,
        round_goal_id.as_ref(),
        parsed.usage.as_ref(),
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
            content_recovery_needed: false,
            invalid_tool_calls: Vec::new(),
            response_observation: response_observation.clone(),
            provider_context: Vec::new(),
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
                content_recovery_needed: false,
                invalid_tool_calls: Vec::new(),
                response_observation: response_observation.clone(),
                provider_context: Vec::new(),
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
        apply_model_response_round(&mut thinking_item, round);
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
                content_recovery_needed: false,
                invalid_tool_calls: Vec::new(),
                response_observation: response_observation.clone(),
                provider_context: Vec::new(),
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
        apply_model_response_round(&mut stream_item, round);
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

    let assistant_response_message = ChatMessage {
        role: "assistant".to_string(),
        content: parsed
            .content
            .clone()
            .or_else(|| completed_stream_content.clone()),
        images: Vec::new(),
        tool_calls: parsed.tool_calls.clone(),
        tool_call_id: None,
        provider_context: parsed.provider_context.clone(),
    };
    if assistant_response_message
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty())
        || !assistant_response_message.tool_calls.is_empty()
        || !assistant_response_message.provider_context.is_empty()
    {
        append_thread_messages_checkpoint(
            session_store,
            orchestrator_thread_id,
            vec![chat_message_to_thread_chat_message(
                &assistant_response_message,
            )],
            persist_session_state,
            "session_turn_thread_assistant_response",
        );
    }

    if let Some(failure) = response_contract_failure {
        return Err(SessionTurnRoundError::InvalidResponse(Box::new(failure)));
    }

    if !parsed.tool_calls.is_empty() {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                activated_skill_id: None,
                content_recovery_needed: false,
                invalid_tool_calls: Vec::new(),
                response_observation: response_observation.clone(),
                provider_context: Vec::new(),
                interrupted: true,
            });
        }
        messages.push(assistant_response_message);
        let tool_result_history_start = messages.len();
        let invalid_tool_calls = tool_validation.invalid_calls;
        for invalid in &invalid_tool_calls {
            messages.push(invalid_tool_result_message(invalid));
        }
        // Goal 模式遇到混合批次时，invalid call 不能与 valid call 部分执行；否则模型
        // 即使绕过了当前严格工具面，批次里的浏览器/文件副作用仍可能已经发生。
        let valid_tool_calls =
            if request.goal_turn_mode.is_goal_driven() && !invalid_tool_calls.is_empty() {
                Vec::new()
            } else {
                tool_validation.valid_calls
            };
        if request.goal_turn_mode.is_goal_driven()
            && !valid_tool_calls.is_empty()
            && let Some(failure) = goal_mode_tool_batch_violation(
                required_tool_chain,
                completed_required_tool_names,
                &valid_tool_calls,
            )
        {
            return Err(SessionTurnRoundError::Failed {
                error: failure,
                context_overflow: None,
                non_stream_fallback_attempted: false,
            });
        }
        let snapshot_session = snapshot_manager.and_then(|mgr| {
            request
                .workspace_root_path
                .as_deref()
                .map(PathBuf::from)
                .and_then(|root| mgr.get_session_for_workspace(request.session_id.as_str(), &root))
        });
        let execution_group_id = format!("turn:{}", request.turn_id);
        let tool_batch = (!valid_tool_calls.is_empty()).then(|| {
            append_session_tool_call_items_batch_with_context(
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
                    browser_capability_revision,
                    browser_execution_id: Some(&request.turn_id),
                    snapshot_session,
                    execution_group_id: Some(execution_group_id),
                    source_thread_id: orchestrator_thread_id,
                    persist_session_state,
                    tool_execution_ledger,
                },
                &valid_tool_calls,
                messages,
                || request_turn_is_writable(session_store, request),
            )
        });
        append_thread_messages_checkpoint(
            session_store,
            orchestrator_thread_id,
            messages[tool_result_history_start..]
                .iter()
                .map(chat_message_to_thread_chat_message)
                .collect(),
            persist_session_state,
            "session_turn_thread_tool_results",
        );
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput {
                final_content: None,
                final_item_id: None,
                timeline_entry_id: timeline_entry_id.clone(),
                encountered_tool_calls: false,
                tool_call_names: Vec::new(),
                activated_skill_id: None,
                content_recovery_needed: false,
                invalid_tool_calls: Vec::new(),
                response_observation: response_observation.clone(),
                provider_context: Vec::new(),
                interrupted: true,
            });
        }
        return Ok(SessionTurnRoundOutput {
            final_content: None,
            final_item_id: None,
            timeline_entry_id: timeline_entry_id.clone(),
            encountered_tool_calls: !valid_tool_calls.is_empty(),
            tool_call_names: tool_batch
                .as_ref()
                .map(|batch| batch.succeeded_tool_names.clone())
                .unwrap_or_default(),
            activated_skill_id: tool_batch.and_then(|batch| batch.activated_skill_id),
            content_recovery_needed: false,
            invalid_tool_calls: invalid_tool_calls
                .into_iter()
                .map(|invalid| invalid.issue)
                .collect(),
            response_observation: response_observation.clone(),
            provider_context: Vec::new(),
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
    let content_recovery_needed = final_content.is_none();

    Ok(SessionTurnRoundOutput {
        final_content,
        final_item_id,
        timeline_entry_id,
        encountered_tool_calls: false,
        tool_call_names: Vec::new(),
        activated_skill_id: None,
        content_recovery_needed,
        invalid_tool_calls: Vec::new(),
        response_observation,
        provider_context: parsed.provider_context.clone(),
        interrupted: false,
    })
}

fn forced_tool_choice_for_round(
    request: &SessionTurnExecutionRequest,
    required_tool_chain: &[String],
    tools: Option<&Vec<ChatToolDefinition>>,
    round: usize,
    completed_required_tool_names: &[String],
) -> Option<ChatToolChoice> {
    if !request.use_tools {
        return None;
    }
    let forced_tool_name = if request.goal_turn_mode.is_goal_driven() {
        required_tool_chain
            .iter()
            .cloned()
            .into_iter()
            .find(|tool_name| {
                !completed_required_tool_names
                    .iter()
                    .any(|completed| completed == tool_name)
            })?
    } else {
        (round == 0)
            .then_some(request.forced_tool_name.as_deref())
            .flatten()?
            .trim()
            .to_string()
    };
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
    model_round: Option<usize>,
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
        model_round,
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
    if let Some(model_round) = model_round {
        apply_model_response_round(&mut final_item, model_round);
    }
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
        BridgeClientError, BridgeErrorLayer, ModelResponse, ModelRetryRuntimeEvent,
        ModelRetryRuntimePhase,
    };
    use magi_core::{SessionLifecycleStatus, TaskId};
    use magi_session_store::{
        ActiveExecutionTurn, CanonicalToolCall, CanonicalTurn, CanonicalTurnItem,
        CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus,
        CanonicalTurnVisibility, CanonicalWorkerRef, ExecutionThread, ExecutionThreadStatus,
        ORCHESTRATOR_ROLE_ID, SessionRecord, SessionStoreState, ThreadChatMessage,
        ThreadChatToolCall, ThreadChatToolFunction, TimelineEntry, TimelineEntryKind,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        time::Duration,
    };

    fn model_response(payload: serde_json::Value) -> ModelResponse {
        ModelResponse::from_chat_payload(
            serde_json::from_value(payload).expect("测试模型响应必须符合统一响应结构"),
        )
    }

    fn ts(value: u64) -> UtcMillis {
        UtcMillis(value)
    }

    fn spawn_vision_http_stub() -> (String, mpsc::Receiver<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("识图模型测试服务必须能监听");
        let address = listener.local_addr().expect("识图模型测试地址必须存在");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("识图模型测试服务必须收到请求");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("识图模型测试读取超时必须可设置");
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).expect("识图模型测试请求必须可读取");
                assert!(read > 0, "识图模型测试请求不得提前结束");
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8(buffer[..header_end].to_vec())
                .expect("识图模型测试请求头必须是 UTF-8");
            assert!(
                headers.starts_with("POST /v1/chat/completions HTTP/1.1"),
                "识图模型必须使用已配置的 OpenAI Chat 协议"
            );
            let content_length = headers
                .split("\r\n")
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .expect("识图模型测试请求必须包含 Content-Length");
            while buffer.len() < header_end + content_length {
                let read = stream
                    .read(&mut chunk)
                    .expect("识图模型测试请求体必须可读取");
                assert!(read > 0, "识图模型测试请求体不得提前结束");
                buffer.extend_from_slice(&chunk[..read]);
            }
            let payload = serde_json::from_slice::<serde_json::Value>(
                &buffer[header_end..header_end + content_length],
            )
            .expect("识图模型测试请求体必须是 JSON");
            sender.send(payload).expect("识图模型测试请求必须可回传");
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"识图模型完成\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            stream
                .write_all(response.as_bytes())
                .expect("识图模型测试响应必须可写入");
            stream.flush().expect("识图模型测试响应必须可刷新");
        });
        (format!("http://{address}/v1"), receiver)
    }

    struct StreamingTextModelBridgeClient {
        delta_content: String,
        payload: String,
    }

    struct CountingEmptyModelBridgeClient {
        calls: AtomicUsize,
    }

    struct CountingTextModelBridgeClient {
        calls: AtomicUsize,
        requests: Mutex<Vec<ModelInvocationRequest>>,
    }

    struct EmptyStreamThenRecoveredSessionModelBridgeClient {
        streaming_calls: AtomicUsize,
    }

    struct RetryEventModelBridgeClient;

    struct RepeatedInvalidShellModelBridgeClient {
        calls: AtomicUsize,
        requests: Mutex<Vec<ModelInvocationRequest>>,
    }

    struct SemanticContextCompactionModelBridgeClient {
        requests: Mutex<Vec<ModelInvocationRequest>>,
    }

    struct FailingContextCompactionModelBridgeClient {
        calls: AtomicUsize,
    }

    struct CancellingContextCompactionModelBridgeClient {
        store: Arc<SessionStore>,
        session_id: SessionId,
    }

    struct CancellingModelBridgeClient {
        store: Arc<SessionStore>,
        session_id: SessionId,
    }

    impl ModelBridgeClient for CancellingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("session turn should use cancellable streaming invocation")
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("session turn should use cancellable streaming invocation")
        }

        fn invoke_streaming_with_cancellation(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
            _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
            is_cancelled: &dyn Fn() -> bool,
        ) -> Result<ModelResponse, BridgeClientError> {
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
        ) -> Result<ModelResponse, BridgeClientError> {
            Ok(model_response(
                serde_json::json!({ "content": "重连后完成" }),
            ))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
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
        ) -> Result<ModelResponse, BridgeClientError> {
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
        ) -> Result<ModelResponse, BridgeClientError> {
            Ok(model_response(
                serde_json::from_str(&self.payload).expect("测试模型响应必须是 JSON"),
            ))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: self.delta_content.clone(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for CountingEmptyModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(model_response(serde_json::json!({
                "content": null,
                "finish_reason": "stop"
            })))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for CountingTextModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .expect("主模型请求记录锁必须可用")
                .push(request);
            Ok(model_response(serde_json::json!({
                "content": "主模型完成",
                "finish_reason": "stop"
            })))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: "主模型完成".to_string(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for RepeatedInvalidShellModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            let call_number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.requests
                .lock()
                .expect("requests mutex poisoned")
                .push(request);
            let arguments = if call_number == 1 {
                "{}"
            } else {
                r#"{"command":" "}"#
            };
            Ok(model_response(serde_json::json!({
                "content": null,
                "finish_reason": "tool_calls",
                "tool_calls": [{
                    "id": format!("call-invalid-shell-{call_number}"),
                    "type": "function",
                    "function": {
                        "name": "shell_exec",
                        "arguments": arguments
                    }
                }]
            })))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for SemanticContextCompactionModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.requests
                .lock()
                .expect("context compaction requests mutex poisoned")
                .push(request);
            Ok(ModelResponse::completed(
                "## 关键事实\n- file_read 已成功读取 facts.txt，内容哈希保持有效。\n## 未完成与下一步\n- 继续当前任务。",
            ))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for FailingContextCompactionModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(503),
                message: "compaction unavailable".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("上下文压缩不应调用流式接口")
        }
    }

    impl ModelBridgeClient for CancellingContextCompactionModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.store
                .cancel_current_turn(&self.session_id)
                .expect("turn cancellation should succeed");
            Ok(ModelResponse::completed("不应安装的压缩摘要"))
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("上下文压缩不应调用流式接口")
        }
    }

    impl ModelBridgeClient for EmptyStreamThenRecoveredSessionModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            Ok(model_response(serde_json::json!({
                "content": "主线在暂态空流后完成。",
                "finish_reason": "stop"
            })))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            let attempt = self.streaming_calls.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::RemoteBusiness,
                    code: Some(-32007),
                    message: "provider response invalid: empty stream response".to_string(),
                });
            }
            assert!(
                request
                    .messages
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|message| message.content.as_deref()
                        == Some(model_empty_response_recovery_prompt(false)))
            );
            on_delta(&ModelStreamingDelta {
                content: "主线在暂态空流后完成。".to_string(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    #[test]
    fn model_identity_prompt_distinguishes_model_from_magi() {
        let prompt = model_identity_prompt_for_request("当前模型是什么？", "grok-4");

        let prompt = prompt.expect("模型身份问题应生成身份规则");
        assert!(prompt.contains("grok-4"));
        assert!(prompt.contains("Magi 解析后实际发起请求的配置目标"));
        assert!(prompt.contains("不要把 Magi 冒充成模型"));
        assert!(!prompt.contains("Magi 是承载当前对话的 AI 工作台"));
    }

    #[test]
    fn model_identity_prompt_describes_magi_only_for_tool_questions() {
        let prompt = model_identity_prompt_for_request("当前工具是什么？", "grok-4");

        let prompt = prompt.expect("工具身份问题应生成身份规则");
        assert!(prompt.contains("Magi 是承载当前对话的 AI 工作台"));
        assert!(!prompt.contains("当前请求目标模型为“grok-4”"));
    }

    #[test]
    fn model_identity_prompt_is_not_added_to_regular_questions() {
        assert!(model_identity_prompt_for_request("读取 README.md", "grok-4").is_none());
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
            live_settings_store: None,
        })
        .expect("cancelled model invocation should resolve as interrupted turn");
        assert!(output.interrupted);
    }

    #[test]
    fn vision_model_only_takes_over_the_current_image_turn() {
        let (vision_base_url, vision_request_receiver) = spawn_vision_http_stub();
        let session_id = SessionId::new("session-vision-takeover");
        let turn_id = "turn-vision-takeover".to_string();
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "vision takeover")
            .expect("识图接管测试会话必须可创建");
        let (_, thread_id) = store.ensure_session_mission(&session_id, ts(900), || {
            magi_core::MissionId::new("mission-vision-takeover")
        });
        store.append_thread_messages(
            &thread_id,
            vec![
                ThreadChatMessage {
                    role: "user".to_string(),
                    content: Some("历史约束：保留表格结构".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                },
                ThreadChatMessage {
                    role: "assistant".to_string(),
                    content: Some("已确认历史约束".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                },
            ],
            ts(950),
        );
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: turn_id.clone(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("结合图片继续处理".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("结合图片继续处理".to_string()),
                        Some("user-vision-takeover".to_string()),
                        thread_id.clone(),
                    )],
                },
            )
            .expect("识图接管测试当前轮次必须可写入");
        let settings = Arc::new(SettingsStore::new());
        settings
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://main.example.com/v1",
                    "apiKey": "main-key",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat"
                }),
            )
            .expect("主模型连接配置必须可写入");
        settings
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({"model": "deepseek-v4-flash", "reasoningEffort": "medium"}),
            )
            .expect("主模型默认配置必须可写入");
        settings
            .set_section(
                crate::model_config::VISION_MODEL_SECTION,
                serde_json::json!({
                    "baseUrl": vision_base_url,
                    "apiKey": "vision-key",
                    "model": "vision-model",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                    "contextWindowTokens": 128000,
                    "textModelRules": []
                }),
            )
            .expect("识图模型配置必须可写入");
        let registry = ConversationRegistry::new();
        registry
            .begin_session_turn_input(session_id.clone(), turn_id.clone())
            .expect("识图接管测试输入边界必须可创建");
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        let main_client = CountingTextModelBridgeClient {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        };
        let tools = vec![ChatToolDefinition {
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunctionDefinition {
                name: "inspect_table".to_string(),
                description: "检查表格".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            origin: magi_bridge_client::ChatToolOrigin::Builtin,
        }];
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id,
            workspace_id: None,
            prompt: "结合图片继续处理".to_string(),
            images: vec![
                SessionTurnImage::from_data_url("table.png", "data:image/png;base64,AAA")
                    .expect("识图接管测试图片必须有效"),
            ],
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: Some("request-vision-takeover".to_string()),
            user_message_id: Some("user-vision-takeover".to_string()),
            placeholder_message_id: Some("assistant-vision-takeover".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        let output = run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &main_client,
            event_bus: &InMemoryEventBus::new(32),
            session_store: store.as_ref(),
            conversation_registry: &registry,
            plan_store: &plan_store,
            settings_store: Some(&settings),
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request,
            prompt: "结合图片继续处理".to_string(),
            knowledge_context_prompt: None,
            tools: Some(tools),
            persist_session_state: None,
            live_settings_store: Some(settings.clone()),
        })
        .expect("识图模型必须完成整个会话轮次");

        assert_eq!(output.final_content, "识图模型完成");
        assert_eq!(main_client.calls.load(Ordering::SeqCst), 0);
        let payload = vision_request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("识图模型必须且只需收到一次完整请求");
        assert_eq!(payload["model"], "vision-model");
        let serialized = payload.to_string();
        assert!(serialized.contains("历史约束：保留表格结构"));
        assert!(serialized.contains("已确认历史约束"));
        assert!(serialized.contains("结合图片继续处理"));
        assert!(serialized.contains("data:image/png;base64,AAA"));
        assert!(serialized.contains("inspect_table"));
        assert!(
            vision_request_receiver.try_recv().is_err(),
            "识图接管不得先识图再调用第二次模型"
        );

        let persisted_image_turn = store.thread_message_history(&thread_id);
        assert!(
            persisted_image_turn
                .iter()
                .filter(|message| message.content.as_deref() == Some("结合图片继续处理"))
                .all(|message| message.images.is_empty()),
            "图片只属于当前回合输入，不能进入后续回合的模型历史"
        );

        let follow_up_turn_id = "turn-after-vision-takeover".to_string();
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: follow_up_turn_id.clone(),
                    turn_seq: 2,
                    accepted_at: ts(2_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续说明刚才的结论".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("继续说明刚才的结论".to_string()),
                        Some("user-after-vision-takeover".to_string()),
                        thread_id.clone(),
                    )],
                },
            )
            .expect("识图后的纯文本轮次必须可写入");
        registry
            .begin_session_turn_input(session_id.clone(), follow_up_turn_id.clone())
            .expect("识图后的纯文本输入边界必须可创建");
        let follow_up_request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: follow_up_turn_id,
            workspace_id: None,
            prompt: "继续说明刚才的结论".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: false,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: Some("request-after-vision-takeover".to_string()),
            user_message_id: Some("user-after-vision-takeover".to_string()),
            placeholder_message_id: Some("assistant-after-vision-takeover".to_string()),
            forced_tool_name: None,
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };
        let follow_up_output = run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &main_client,
            event_bus: &InMemoryEventBus::new(32),
            session_store: store.as_ref(),
            conversation_registry: &registry,
            plan_store: &plan_store,
            settings_store: Some(&settings),
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request: follow_up_request,
            prompt: "继续说明刚才的结论".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
            live_settings_store: Some(settings.clone()),
        })
        .expect("识图后的纯文本轮次必须恢复主模型执行");

        assert_eq!(follow_up_output.final_content, "主模型完成");
        assert_eq!(main_client.calls.load(Ordering::SeqCst), 1);
        let main_requests = main_client
            .requests
            .lock()
            .expect("主模型请求记录锁必须可用");
        assert_eq!(main_requests.len(), 1);
        assert!(
            main_requests[0]
                .messages
                .as_ref()
                .expect("主模型后续轮次必须携带完整文本上下文")
                .iter()
                .all(|message| message.images.is_empty()),
            "后续纯文本轮次不得向主模型重放历史图片"
        );
    }

    struct FailingModelBridgeClient {
        message: String,
    }

    impl ModelBridgeClient for FailingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
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
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    struct StreamingThenFailingModelBridgeClient {
        delta_content: String,
        message: String,
    }

    struct InterruptedThenRecoveredSessionModelBridgeClient {
        streaming_calls: AtomicUsize,
        non_stream_calls: AtomicUsize,
        fallback_messages: Mutex<Vec<ChatMessage>>,
    }

    struct SteeringModelBridgeClient {
        registry: Arc<ConversationRegistry>,
        session_id: SessionId,
        turn_id: String,
        calls: AtomicUsize,
        requests: std::sync::Mutex<Vec<ModelInvocationRequest>>,
    }

    struct PlanFollowUpModelBridgeClient {
        plan_store: magi_plan::PlanStore,
        calls: AtomicUsize,
        requests: std::sync::Mutex<Vec<ModelInvocationRequest>>,
    }

    impl ModelBridgeClient for SteeringModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
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
            let mut response = model_response(serde_json::json!({
                "content": content,
                "finish_reason": "stop"
            }));
            if call == 0 {
                response.provider_context = vec![ModelProviderContext {
                    provider: "anthropic".to_string(),
                    kind: "thinking".to_string(),
                    data: serde_json::json!({
                        "type": "thinking",
                        "thinking": "先响应第一阶段",
                        "signature": "session-signed-thinking"
                    }),
                }];
            }
            Ok(response)
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
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

    impl ModelBridgeClient for PlanFollowUpModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .expect("request log lock")
                .push(request);
            if call == 39 {
                let current = self.plan_store.snapshot().expect("plan should exist");
                self.plan_store
                    .update(magi_plan::UpdatePlanInput {
                        plan_id: Some(current.plan_id.to_string()),
                        expected_revision: Some(current.revision),
                        expected_goal_id: None,
                        expected_goal_control_revision: None,
                        language: current.language,
                        explanation: None,
                        plan: current
                            .items
                            .into_iter()
                            .map(|item| magi_plan::UpdatePlanItemInput {
                                item_id: Some(item.item_id.to_string()),
                                step: item.title,
                                status: magi_core::PlanItemStatus::Completed,
                            })
                            .collect(),
                    })
                    .expect("final model round should complete plan");
            }
            Ok(model_response(serde_json::json!({
                "content": if call < 39 { "继续推进当前计划" } else { "全部计划完成" },
                "finish_reason": "stop"
            })))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            let call = self.calls.load(Ordering::SeqCst);
            on_delta(&ModelStreamingDelta {
                content: if call < 39 {
                    "继续推进当前计划".to_string()
                } else {
                    "全部计划完成".to_string()
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
                        orchestrator_thread_id.clone(),
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
            plan_store: &crate::test_plan_store("test-plan"),
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
            live_settings_store: None,
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
            message.role == "assistant"
                && message.content.as_deref() == Some("第一段回复")
                && message
                    .provider_context
                    .first()
                    .is_some_and(|context| context.data["signature"] == "session-signed-thinking")
        }));
        assert!(second_messages.iter().any(|message| {
            message.role == "user" && message.content.as_deref() == Some("优先收口，不要继续扩展")
        }));
        assert!(
            store
                .thread_message_history(&orchestrator_thread_id)
                .iter()
                .any(|message| {
                    message.role == "assistant"
                        && message.content.as_deref() == Some("第一段回复")
                        && message.provider_context.first().is_some_and(|context| {
                            context.data["signature"] == "session-signed-thinking"
                        })
                }),
            "主会话 thread 必须持久化提供方签名上下文"
        );
    }

    #[test]
    fn unfinished_plan_continues_beyond_legacy_follow_up_limit() {
        let session_id = SessionId::new("session-plan-follow-up");
        let turn_id = "turn-plan-follow-up".to_string();
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "plan follow up")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(900), || {
                magi_core::MissionId::new("mission-plan-follow-up")
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
                    user_message: Some("完成全部计划".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("完成全部计划".to_string()),
                        Some("user-plan-follow-up".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let registry = ConversationRegistry::new();
        registry
            .begin_session_turn_input(session_id.clone(), turn_id.clone())
            .expect("turn input should begin");
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("implement".to_string()),
                    step: "完成实现".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let client = PlanFollowUpModelBridgeClient {
            plan_store: plan_store.clone(),
            calls: AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        };
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
                prompt: "完成全部计划".to_string(),
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
            prompt: "完成全部计划".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
            live_settings_store: None,
        })
        .expect("plan turn should complete");

        assert_eq!(output.final_content, "全部计划完成");
        assert_eq!(client.calls.load(Ordering::SeqCst), 40);
        let requests = client.requests.lock().expect("request log lock");
        let last_messages = requests[39]
            .messages
            .as_ref()
            .expect("follow-up request should include messages");
        assert!(last_messages.iter().any(|message| {
            message.role == "user"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("当前执行计划仍未完成"))
        }));
        assert!(!plan_store.requires_execution_follow_up());
    }

    #[test]
    fn ordinary_turn_does_not_consume_waiting_goal_plan() {
        let session_id = SessionId::new("session-waiting-goal-plan-isolation");
        let goal_turn_id = "turn-goal-owner".to_string();
        let ordinary_turn_id = "turn-ordinary-diversion".to_string();
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "waiting goal plan isolation")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(900), || {
                magi_core::MissionId::new("mission-waiting-goal-plan-isolation")
            });
        let goal = store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id.clone(),
                goal_turn_id,
                "验证普通 Turn 不接管等待中的 Goal",
                AccessProfile::Restricted,
                None,
            )
            .expect("goal should create");
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        let plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("goal-step".to_string()),
                    step: "继续 Goal".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("goal plan should create");
        let paused = store
            .pause_goal_with_plan(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                Some(plan.revision),
            )
            .expect("goal should pause")
            .0;
        let paused_plan = store.plan(&session_id).expect("paused plan should exist");
        store
            .resume_goal_with_plan(
                &session_id,
                &goal.goal_id,
                paused.control_revision,
                Some(paused_plan.revision),
                None,
                None,
            )
            .expect("goal resume request should wait for an owner");
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: ordinary_turn_id.clone(),
                    turn_seq: 2,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("执行普通任务".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("执行普通任务".to_string()),
                        Some("user-ordinary-diversion".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("ordinary turn should be stored");
        let registry = ConversationRegistry::new();
        registry
            .begin_session_turn_input(session_id.clone(), ordinary_turn_id.clone())
            .expect("ordinary turn input should begin");
        let client = PlanFollowUpModelBridgeClient {
            plan_store: plan_store.clone(),
            calls: AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        };

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
                turn_id: ordinary_turn_id,
                workspace_id: None,
                prompt: "执行普通任务".to_string(),
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
            prompt: "执行普通任务".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
            live_settings_store: None,
        })
        .expect("ordinary turn should complete independently");

        assert_eq!(output.final_content, "继续推进当前计划");
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        assert!(plan_store.requires_execution_follow_up());
        let requests = client.requests.lock().expect("request log lock");
        let messages = requests[0]
            .messages
            .as_ref()
            .expect("ordinary request should include messages");
        assert!(!messages.iter().any(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("当前持久化执行状态") || content.contains("当前执行计划仍未完成")
            })
        }));
    }

    impl ModelBridgeClient for StreamingThenFailingModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
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
        ) -> Result<ModelResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: self.delta_content.clone(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for InterruptedThenRecoveredSessionModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.non_stream_calls.fetch_add(1, Ordering::SeqCst);
            *self
                .fallback_messages
                .lock()
                .expect("fallback messages mutex poisoned") = request.messages.unwrap_or_default();
            Ok(model_response(serde_json::json!({
                "content": "已通过非流式降级完成。",
                "finish_reason": "stop"
            })))
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            let attempt = self.streaming_calls.fetch_add(1, Ordering::SeqCst);
            on_delta(&ModelStreamingDelta {
                content: if attempt == 0 {
                    "已保留的半截回复".to_string()
                } else {
                    "，后续流仍然中断".to_string()
                },
                thinking: String::new(),
            });
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: "provider stream interrupted: missing terminal SSE event".to_string(),
            })
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
            origin: magi_bridge_client::ChatToolOrigin::Builtin,
        }];

        let choice = forced_tool_choice_for_round(
            &request,
            &request.required_tool_chain,
            Some(&tools),
            0,
            &[],
        )
        .expect("first round should force diagram_render");
        assert_eq!(choice.function.name, "diagram_render");
        assert!(
            forced_tool_choice_for_round(
                &request,
                &request.required_tool_chain,
                Some(&tools),
                1,
                &[]
            )
            .is_none()
        );

        let mut unavailable_request = request;
        unavailable_request.forced_tool_name = Some("missing_tool".to_string());
        assert!(
            forced_tool_choice_for_round(
                &unavailable_request,
                &unavailable_request.required_tool_chain,
                Some(&tools),
                0,
                &[],
            )
            .is_none()
        );
    }

    #[test]
    fn forced_tool_is_recovered_when_provider_only_supports_automatic_choice() {
        let request = SessionTurnExecutionRequest {
            session_id: SessionId::new("session-forced-tool-recovery"),
            turn_id: "turn-forced-tool-recovery".to_string(),
            workspace_id: None,
            prompt: "生成一张图片".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            use_tools: true,
            access_profile: AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            forced_tool_name: Some("image_generate".to_string()),
            required_tool_chain: Vec::new(),
            goal_turn_mode: SessionGoalTurnMode::None,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        assert_eq!(
            session_required_tool_chain(&request, false, false),
            vec!["image_generate".to_string()]
        );
    }

    #[test]
    fn goal_turn_required_tool_chain_is_lifecycle_ordered() {
        let request = SessionTurnExecutionRequest {
            session_id: SessionId::new("session-goal-lifecycle-order"),
            turn_id: "turn-goal-lifecycle-order".to_string(),
            workspace_id: None,
            prompt: "推进目标".to_string(),
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
                "create_goal".to_string(),
                "shell_exec".to_string(),
                "update_plan".to_string(),
            ],
            goal_turn_mode: SessionGoalTurnMode::Start,
            product_locale: "zh-CN".to_string(),
            workspace_root_path: None,
        };

        assert_eq!(
            session_required_tool_chain(&request, true, false),
            ["get_goal", "create_goal", "update_plan", "shell_exec"]
        );

        let continuation = SessionTurnExecutionRequest {
            goal_turn_mode: SessionGoalTurnMode::Continuation,
            ..request
        };
        assert_eq!(
            session_required_tool_chain(&continuation, false, false),
            ["get_goal", "update_plan", "shell_exec"]
        );
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
                origin: magi_bridge_client::ChatToolOrigin::Builtin,
            })
            .collect::<Vec<_>>();

        assert!(
            forced_tool_choice_for_round(
                &request,
                &request.required_tool_chain,
                Some(&tools),
                0,
                &[],
            )
            .is_none()
        );
        assert!(
            forced_tool_choice_for_round(
                &request,
                &request.required_tool_chain,
                Some(&tools),
                1,
                &["shell_exec".to_string()],
            )
            .is_none()
        );
        assert!(
            forced_tool_choice_for_round(
                &request,
                &request.required_tool_chain,
                Some(&tools),
                2,
                &["shell_exec".to_string(), "file_write".to_string()],
            )
            .is_none()
        );
    }

    #[test]
    fn runtime_invalid_state_pauses_active_plan() {
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
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("continue-current-step".to_string()),
                    step: "继续处理当前步骤".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should write");
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
            live_settings_store: None,
        });

        assert!(matches!(
            result,
            Err(SessionTurnExecutionError {
                reason: SessionTurnFailureReason::RuntimeInvalidState,
                ..
            })
        ));
        let plan = plan_store.snapshot().expect("plan should remain visible");
        assert_eq!(plan.state, magi_core::PlanState::Paused);
        assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
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
        let client = CountingEmptyModelBridgeClient {
            calls: AtomicUsize::new(0),
        };
        let event_bus = InMemoryEventBus::new(16);
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("continue-current-step".to_string()),
                    step: "继续处理当前步骤".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should write");
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
            live_settings_store: None,
        }) {
            Ok(_) => panic!("empty provider response should fail"),
            Err(error) => error,
        };

        assert_eq!(error.reason, SessionTurnFailureReason::ModelEmptyResponse);
        assert_eq!(
            error.public_message,
            "模型服务返回了空响应，未生成正文或可执行工具调用。"
        );
        assert_eq!(
            client.calls.load(Ordering::SeqCst),
            MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS + 1,
            "空响应必须先执行每轮带恢复提示的自动重试，再进入失败终态"
        );
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain visible");
        assert_eq!(turn.status, "failed");
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("empty response should append an assistant error");
        assert_eq!(
            error_item.content.as_deref(),
            Some(error.public_message.as_str())
        );
        assert_eq!(
            error_item.metadata["modelFailure"]["code"],
            "model_empty_response"
        );
        assert_eq!(
            error_item.metadata["modelFailure"]["retryAttempts"],
            MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
        );
        assert!(
            error_item.metadata["modelFailure"]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("仍未返回用户可见正文"))
        );
        let plan = plan_store.snapshot().expect("plan should remain visible");
        assert_eq!(plan.state, magi_core::PlanState::Paused);
        assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
    }

    #[test]
    fn repeated_invalid_shell_call_stops_without_creating_tool_cards() {
        let session_id = SessionId::new("session-invalid-shell-call");
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "invalid shell call")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(910), || {
                magi_core::MissionId::new("mission-invalid-shell-call")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-invalid-shell-call".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("检查项目".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("检查项目".to_string()),
                        Some("user-invalid-shell-call".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let client = RepeatedInvalidShellModelBridgeClient {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        };
        let event_bus = InMemoryEventBus::new(16);
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-invalid-shell-call".to_string(),
            workspace_id: None,
            prompt: "检查项目".to_string(),
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
            workspace_root_path: None,
        };
        let tools = vec![ChatToolDefinition {
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunctionDefinition {
                name: "shell_exec".to_string(),
                description: "执行 Shell 命令".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": []
                }),
            },
            origin: magi_bridge_client::ChatToolOrigin::Builtin,
        }];

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
            prompt: "检查项目".to_string(),
            knowledge_context_prompt: None,
            tools: Some(tools),
            persist_session_state: None,
            live_settings_store: None,
        }) {
            Ok(_) => panic!("repeated invalid shell call should fail the turn"),
            Err(error) => error,
        };

        assert_eq!(
            error.reason,
            SessionTurnFailureReason::ToolCallProtocolFailed
        );
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
        let requests = client.requests.lock().expect("requests mutex poisoned");
        let second_messages = requests[1]
            .messages
            .as_ref()
            .expect("messages should exist");
        assert!(second_messages.iter().any(|message| {
            message.role == "tool"
                && message.content.as_deref().is_some_and(|content| {
                    content.contains("tool-call-validation.v1") && content.contains("command")
                })
        }));
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain visible");
        assert_eq!(turn.status, "failed");
        assert!(
            turn.items.iter().all(|item| {
                item.kind != "tool_call_started" && item.kind != "tool_call_result"
            })
        );
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("tool call validation failure should be visible");
        assert_eq!(
            error_item.metadata["toolCallFailure"]["code"],
            "tool_arguments_invalid"
        );
        assert_eq!(
            error_item.metadata["toolCallFailure"]["toolName"],
            "shell_exec"
        );
        assert_eq!(
            error_item.metadata["toolCallFailure"]["argumentsPreview"],
            r#"{"command":" "}"#
        );
    }

    #[test]
    fn session_turn_retries_empty_stream_before_output() {
        let session_id = SessionId::new("session-empty-stream-recovery");
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "empty stream recovery")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(1_100), || {
                magi_core::MissionId::new("mission-empty-stream-recovery")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-empty-stream-recovery".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_200),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请回复一句话".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请回复一句话".to_string()),
                        Some("user-empty-stream-recovery".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("turn should be stored");
        let client = EmptyStreamThenRecoveredSessionModelBridgeClient {
            streaming_calls: AtomicUsize::new(0),
        };
        let event_bus = InMemoryEventBus::new(16);
        let plan_store = magi_plan::PlanStore::new(store.clone(), session_id.clone());
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-empty-stream-recovery".to_string(),
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

        let output = run_session_turn_execution(SessionTurnExecutionRuntime {
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
            live_settings_store: None,
        })
        .expect("empty stream before output should be retried");

        assert_eq!(output.final_content, "主线在暂态空流后完成。");
        assert_eq!(
            client.streaming_calls.load(Ordering::SeqCst),
            3,
            "主线机械重试仍为空流时，应追加用户可见答复约束后恢复"
        );
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("completed turn should remain visible");
        assert_eq!(turn.status, "completed");
        assert!(turn.items.iter().all(|item| item.kind != "assistant_error"));
    }

    #[test]
    fn partial_stream_failure_preserves_output_before_terminal_fallback_failure() {
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
            plan_store: &crate::test_plan_store("test-plan"),
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
            live_settings_store: None,
        }) {
            Ok(_) => panic!("incomplete stream should fail the turn"),
            Err(error) => error,
        };

        assert_eq!(
            error.reason,
            SessionTurnFailureReason::ModelStreamInterrupted
        );
        assert_eq!(error.public_message, "模型响应流在完成前中断。");
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain visible");
        assert_eq!(turn.status, "failed");
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("stream failure should append an assistant error");
        assert_eq!(
            error_item.content.as_deref(),
            Some(error.public_message.as_str())
        );
        assert_eq!(
            error_item.metadata["modelFailure"]["code"],
            "model_stream_interrupted"
        );
        assert!(
            error_item.metadata["modelFailure"]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("missing terminal SSE event"))
        );
        assert!(
            turn.items.iter().any(|item| {
                item.kind == "assistant_stream"
                    && item.status == "completed"
                    && item.content.as_deref() == Some("这是一段半截输出")
            }),
            "流中断时必须保留已经展示给用户的内容"
        );
    }

    #[test]
    fn session_turn_recovers_after_repeated_partial_stream_interruptions() {
        let session_id = SessionId::new("session-stream-recovery");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "stream recovery")
            .expect("session should be creatable");
        let (_mission_id, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, ts(1_500), || {
                magi_core::MissionId::new("mission-stream-recovery")
            });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-stream-recovery".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_500),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("请输出完整回复".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("请输出完整回复".to_string()),
                        Some("user-stream-recovery".to_string()),
                        orchestrator_thread_id,
                    )],
                },
            )
            .expect("current turn should be stored");
        let client = InterruptedThenRecoveredSessionModelBridgeClient {
            streaming_calls: AtomicUsize::new(0),
            non_stream_calls: AtomicUsize::new(0),
            fallback_messages: Mutex::new(Vec::new()),
        };
        let event_bus = InMemoryEventBus::new(32);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-stream-recovery".to_string(),
            workspace_id: None,
            prompt: "请输出完整回复".to_string(),
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

        let output = run_session_turn_execution(SessionTurnExecutionRuntime {
            client: &client,
            event_bus: &event_bus,
            session_store: &store,
            conversation_registry: &ConversationRegistry::new(),
            plan_store: &crate::test_plan_store("test-plan"),
            settings_store: None,
            safety_gate: None,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            skill_name: None,
            snapshot_manager: None,
            request,
            prompt: "请输出完整回复".to_string(),
            knowledge_context_prompt: None,
            tools: None,
            persist_session_state: None,
            live_settings_store: None,
        })
        .expect("流中断应由非流式降级完成");

        assert_eq!(output.final_content, "已通过非流式降级完成。");
        assert_eq!(
            client.streaming_calls.load(Ordering::SeqCst),
            MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS + 1
        );
        assert_eq!(client.non_stream_calls.load(Ordering::SeqCst), 1);
        let fallback_messages = client
            .fallback_messages
            .lock()
            .expect("fallback messages mutex poisoned");
        assert_eq!(
            fallback_messages
                .iter()
                .filter(|message| {
                    message.role == "user"
                        && message
                            .content
                            .as_deref()
                            .is_some_and(|content| content.contains("已保留此前可见内容"))
                })
                .count(),
            MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS + 1
        );
        let turn = store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("completed turn should remain visible");
        assert_eq!(turn.status, "completed");
        assert!(turn.items.iter().all(|item| item.kind != "assistant_error"));
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
            plan_store: &crate::test_plan_store("test-plan"),
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
            live_settings_store: None,
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
            provider_context: Vec::new(),
        }];
        let mut tool_execution_ledger = ToolExecutionLedger::default();

        let output = stream_session_turn_round(
            SessionTurnRoundRuntime {
                client: &client,
                event_bus: &event_bus,
                session_store: &store,
                plan_store: &crate::test_plan_store("test-plan"),
                settings_store: None,
                safety_gate: None,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &request.prompt,
                tools: None,
                browser_capability_revision: None,
                messages: &mut messages,
                completed_required_tool_names: &[],
                required_tool_chain: &[],
                pre_output_invocation_recovery_attempts: 0,
                stream_interruption_recovery_attempts: 0,
                snapshot_manager: None,
                round: 0,
                orchestrator_thread_id: &orchestrator_thread_id,
                orchestrator_mission_id: &mission_id,
                persist_session_state: None,
                tool_execution_ledger: &mut tool_execution_ledger,
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
        assert_eq!(
            assistant_items[0].metadata.get("modelRound"),
            Some(&serde_json::Value::from(0_u64)),
            "流式正文必须携带模型轮次，供前端将下一轮 thinking 与本轮正文分隔"
        );
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
            provider_context: Vec::new(),
        }];
        let mut tool_execution_ledger = ToolExecutionLedger::default();

        stream_session_turn_round(
            SessionTurnRoundRuntime {
                client: &RetryEventModelBridgeClient,
                event_bus: &event_bus,
                session_store: &store,
                plan_store: &crate::test_plan_store("test-plan"),
                settings_store: None,
                safety_gate: None,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &request.prompt,
                tools: None,
                browser_capability_revision: None,
                messages: &mut messages,
                completed_required_tool_names: &[],
                required_tool_chain: &[],
                pre_output_invocation_recovery_attempts: 0,
                stream_interruption_recovery_attempts: 0,
                snapshot_manager: None,
                round: 0,
                orchestrator_thread_id: &orchestrator_thread_id,
                orchestrator_mission_id: &mission_id,
                persist_session_state: None,
                tool_execution_ledger: &mut tool_execution_ledger,
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
                        worker: Some(CanonicalWorkerRef {
                            task_id: Some(TaskId::new("task-root-context-history")),
                            worker_id: None,
                            role_id: None,
                            title: Some("最终回复".to_string()),
                        }),
                        source_thread_id: magi_core::ThreadId::new(
                            "thread-coordinator-task-root-context-history",
                        ),
                        visibility: CanonicalTurnVisibility::default(),
                        metadata: HashMap::from([(
                            "assistantOutputKind".to_string(),
                            serde_json::json!("final"),
                        )]),
                    },
                ],
                metadata: HashMap::new(),
            }],
            notifications: Vec::new(),
            goals: Vec::new(),
            plans: Vec::new(),
            execution_sidecar_store: Default::default(),
            thread_context_checkpoints: vec![],
            thread_registry: vec![ExecutionThread {
                thread_id: magi_core::ThreadId::new("thread-test-orchestrator"),
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-context-history"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-orchestrator-test"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1200),
                observed_context_window_tokens: None,
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
        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);

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
    fn session_turn_normalizes_interrupted_tool_history_before_next_model_call() {
        let session_id = SessionId::new("session-interrupted-tool-history");
        let thread_id = magi_core::ThreadId::new("thread-interrupted-tool-history");
        let tool_call_id = "call-interrupted-shell".to_string();
        let tool_arguments = r#"{"command":"sleep 20; printf done"}"#.to_string();
        let store = SessionStore::from_state(SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "interrupted tool history".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: ts(900),
                updated_at: ts(1_100),
                message_count: None,
                workspace_id: None,
                last_completed_at: None,
                last_viewed_at: None,
            }],
            timeline: Vec::new(),
            canonical_turns: vec![CanonicalTurn {
                session_id: session_id.clone(),
                turn_id: "turn-interrupted-tool".to_string(),
                turn_seq: 1_000,
                accepted_at: ts(1_000),
                completed_at: Some(ts(1_100)),
                status: CanonicalTurnStatus::Cancelled,
                response_duration_ms: Some(100),
                usage: None,
                items: vec![CanonicalTurnItem {
                    session_id: session_id.clone(),
                    turn_id: "turn-interrupted-tool".to_string(),
                    turn_seq: 1_000,
                    item_id: "turn-item-interrupted-tool".to_string(),
                    item_seq: 2,
                    kind: CanonicalTurnItemKind::ToolCall,
                    created_at: ts(1_000),
                    status: CanonicalTurnItemStatus::Cancelled,
                    item_version: None,
                    updated_at: ts(1_100),
                    title: Some("shell".to_string()),
                    content: Some("正在调用工具：shell".to_string()),
                    blocks: Vec::new(),
                    tool: Some(CanonicalToolCall {
                        call_id: tool_call_id.clone(),
                        name: "shell".to_string(),
                        arguments: Some(serde_json::json!({
                            "command": "sleep 20; printf done"
                        })),
                        result: None,
                        error: None,
                    }),
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
            thread_context_checkpoints: vec![],
            thread_registry: vec![ExecutionThread {
                thread_id: thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-interrupted-tool-history"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-interrupted-tool-history"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1_100),
                observed_context_window_tokens: None,
                handled_task_ids: Vec::new(),
                message_history: vec![ThreadChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    images: Vec::new(),
                    tool_calls: vec![ThreadChatToolCall {
                        id: tool_call_id,
                        kind: "function".to_string(),
                        function: ThreadChatToolFunction {
                            name: "shell".to_string(),
                            arguments: tool_arguments,
                        },
                    }],
                    tool_call_id: None,
                    provider_context: Vec::new(),
                }],
            }],
        });

        assert_eq!(
            normalize_interrupted_session_tool_history(&store, &session_id, &thread_id, None,),
            1
        );
        let history = store.thread_message_history(&thread_id);
        assert_eq!(
            history
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["assistant", "tool"]
        );
        assert!(history[1].content.as_deref().is_some_and(|content| {
            content.contains(r#""status":"interrupted""#)
                && content.contains(r#""execution":"unknown""#)
        }));
        assert_eq!(
            normalize_interrupted_session_tool_history(&store, &session_id, &thread_id, None,),
            0,
            "重复进入下一轮不得再次插入中断结果"
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
            thread_context_checkpoints: vec![],
            thread_registry: vec![ExecutionThread {
                thread_id,
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-context-history-cancelled"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-context-history-cancelled"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1_100),
                observed_context_window_tokens: None,
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

        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);
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
            thread_context_checkpoints: vec![],
            thread_registry: vec![ExecutionThread {
                thread_id: thread_id.clone(),
                session_id: session_id.clone(),
                mission_id: magi_core::MissionId::new("mission-context-history-long"),
                role_id: ORCHESTRATOR_ROLE_ID.to_string(),
                worker_instance_id: magi_core::WorkerId::new("worker-context-history-long"),
                status: ExecutionThreadStatus::Idle,
                created_at: ts(900),
                last_used_at: ts(1_700),
                observed_context_window_tokens: None,
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

        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);

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

        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);

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

        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);
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
            &store,
            &request,
            &request.prompt,
            Some("[reference:knowledge:adr] 单一事实源\n只读投影来自事件事实。"),
            &canonical_session_turn_history(&store, &request),
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

        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);
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

        let history = canonical_session_turn_history(&store, &request);
        let messages =
            build_session_turn_messages(&store, &request, &request.prompt, None, &history);

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

        let goal_request = SessionTurnExecutionRequest {
            goal_turn_mode: SessionGoalTurnMode::Start,
            ..request
        };
        let goal_history = canonical_session_turn_history(&store, &goal_request);
        let goal_messages = build_session_turn_messages(
            &store,
            &goal_request,
            &goal_request.prompt,
            None,
            &goal_history,
        );
        assert!(
            goal_messages[0].content.as_deref().is_some_and(|content| {
                content.contains("计划语言规则") && content.contains("locale=zh-CN")
            }),
            "目标模式才应注入计划语言规则"
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
                model_round: Some(1),
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
        assert_eq!(
            post_tool_item.metadata.get("modelRound"),
            Some(&serde_json::Value::from(1_u64)),
            "最终正文必须保留所属模型轮次，供前端按轮次分隔 thinking"
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
            canonical_post_tool_item.metadata.get("modelRound"),
            Some(&serde_json::Value::from(1_u64)),
            "模型轮次必须进入 canonical 事件，不能只停留在运行时 sidecar"
        );
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
    fn context_compaction_notice_is_persisted_as_renderable_system_item() {
        let session_id = SessionId::new("session-context-compaction-notice");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "context compaction notice")
            .expect("session should be creatable");
        let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1_000), || {
            magi_core::MissionId::new("mission-context-compaction-notice")
        });
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-context-compaction-notice".to_string(),
                    turn_seq: 1,
                    accepted_at: ts(1_000),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续对话".to_string()),
                    items: vec![session_turn_item(
                        "user_message",
                        "completed",
                        None,
                        Some("继续对话".to_string()),
                        Some("user-context-compaction-notice".to_string()),
                        thread_id.clone(),
                    )],
                },
            )
            .expect("current turn should be stored");
        let event_bus = InMemoryEventBus::new(16);
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-context-compaction-notice".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context-compaction-notice")),
            prompt: "继续对话".to_string(),
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
        let item_id = new_context_compaction_item_id(&request.turn_id, &thread_id, "pre_turn");
        let writeback = ContextCompactionWritebackContext {
            event_bus: &event_bus,
            session_store: &store,
            session_id: &request.session_id,
            workspace_id: &request.workspace_id,
            thread_id: &thread_id,
            item_id: &item_id,
            phase: "pre_turn",
            persist_session_state: None,
            task: None,
            turn_visibility: None,
        };
        upsert_context_compaction_progress_notice(
            writeback,
            ContextCompactionProgress::Advanced {
                stage: "history_chunk",
                completed_chunks: 2,
                total_chunks: 4,
            },
        );
        upsert_context_compaction_completed_notice(
            writeback,
            &ContextCompactionRecord {
                reason: "context_window_pressure",
                original_message_count: 42,
                compacted_message_count: 9,
                original_token_estimate: 180_000,
                compacted_token_estimate: 36_000,
                compacted_at: ts(1_001),
            },
        );

        let turn = store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == request.turn_id)
            .expect("canonical turn should exist");
        let notice = turn
            .items
            .iter()
            .find(|item| item.kind == CanonicalTurnItemKind::SystemNotice)
            .expect("context compaction notice should exist");
        assert!(notice.visibility.renderable);
        assert_eq!(
            notice.metadata.get("noticeKind"),
            Some(&serde_json::json!("context_compaction"))
        );
        assert_eq!(
            notice.metadata.get("compactedTokenEstimate"),
            Some(&serde_json::json!(36_000))
        );
        assert_eq!(
            notice.metadata.get("compactionState"),
            Some(&serde_json::json!("completed"))
        );
        assert_eq!(
            turn.items
                .iter()
                .filter(|item| {
                    item.metadata.get("noticeKind")
                        == Some(&serde_json::json!("context_compaction"))
                })
                .count(),
            1,
            "压缩进度和终态必须更新同一个 canonical item"
        );
        assert!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| event.event_type == "session.turn.item")
        );
    }

    #[test]
    fn context_limit_recovery_rebuilds_after_tool_fact_with_dynamic_target() {
        let session_id = SessionId::new("session-context-limit-after-tool");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "context limit after tool")
            .expect("session should create");
        let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1), || {
            magi_core::MissionId::new("mission-context-limit-after-tool")
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let file_path = directory.path().join("facts.txt");
        std::fs::write(&file_path, "stable fact").expect("write file fact");
        let content_hash = magi_snapshot::path_content_hash(&file_path).expect("hash file fact");
        let mut history = vec![
            ThreadChatMessage {
                role: "user".to_string(),
                content: Some("读取 facts.txt 并继续完成任务".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![ThreadChatToolCall {
                    id: "call-context-limit-file-read".to_string(),
                    kind: "function".to_string(),
                    function: ThreadChatToolFunction {
                        name: "file_read".to_string(),
                        arguments: serde_json::json!({"path": file_path}).to_string(),
                    },
                }],
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "tool".to_string(),
                content: Some(
                    serde_json::json!({
                        "tool": "file_read",
                        "status": "succeeded",
                        "path": file_path,
                        "content_hash": content_hash,
                        "content": "stable fact"
                    })
                    .to_string(),
                ),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-context-limit-file-read".to_string()),
                provider_context: Vec::new(),
            },
        ];
        history.extend((0..24).map(|index| ThreadChatMessage {
            role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
            content: Some("x".repeat(1_000)),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        }));
        store.append_thread_messages(&thread_id, history, ts(2));
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-context-limit-after-tool".to_string(),
                    turn_seq: 3,
                    accepted_at: ts(3),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续".to_string()),
                    items: vec![],
                },
            )
            .expect("current turn should store");
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-context-limit-after-tool".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context-limit-after-tool")),
            prompt: "继续".to_string(),
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
            workspace_root_path: None,
        };
        let client = SemanticContextCompactionModelBridgeClient {
            requests: Mutex::new(Vec::new()),
        };
        let mut messages = Vec::new();
        let rebuilt = rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
            client: &client,
            event_bus: &InMemoryEventBus::new(16),
            session_store: &store,
            request: &request,
            thread_id: &thread_id,
            prompt: &request.prompt,
            knowledge_context_prompt: None,
            context_window: 4_000,
            messages: &mut messages,
            persist_session_state: None,
            settings_store: None,
            tools: None,
            skill_runtime: None,
            initial_skill_name: None,
            active_skill_name: None,
            persist_checkpoint: true,
        });

        assert!(
            rebuilt.expect("上下文压缩应成功"),
            "小窗口恢复必须生成更小的上下文检查点"
        );
        let compaction_requests = client
            .requests
            .lock()
            .expect("context compaction requests mutex poisoned");
        assert_eq!(
            compaction_requests.len(),
            1,
            "同一次恢复只能执行一次有界语义压缩"
        );
        assert!(
            compaction_requests
                .iter()
                .any(|request| request.prompt.contains("file_read"))
        );
        assert!(messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("file_read 已成功读取 facts.txt"))
        }));
        assert!(store.thread_context_checkpoint(&thread_id).is_some());
    }

    #[test]
    fn context_limit_recovery_returns_terminal_failure_instead_of_reusing_oversized_history() {
        let session_id = SessionId::new("session-context-compaction-failure");
        let store = SessionStore::new();
        store
            .create_session(session_id.clone(), "context compaction failure")
            .expect("session should create");
        let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1), || {
            magi_core::MissionId::new("mission-context-compaction-failure")
        });
        store.append_thread_messages(
            &thread_id,
            (0..32)
                .map(|index| ThreadChatMessage {
                    role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
                    content: Some("x".repeat(1_000)),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                })
                .collect(),
            ts(2),
        );
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-context-compaction-failure".to_string(),
                    turn_seq: 3,
                    accepted_at: ts(3),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续".to_string()),
                    items: vec![],
                },
            )
            .expect("current turn should store");
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-context-compaction-failure".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context-compaction-failure")),
            prompt: "继续".to_string(),
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
            workspace_root_path: None,
        };
        let client = FailingContextCompactionModelBridgeClient {
            calls: AtomicUsize::new(0),
        };
        let event_bus = InMemoryEventBus::new(16);
        let mut messages = Vec::new();

        let result = rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
            client: &client,
            event_bus: &event_bus,
            session_store: &store,
            request: &request,
            thread_id: &thread_id,
            prompt: &request.prompt,
            knowledge_context_prompt: None,
            context_window: 4_000,
            messages: &mut messages,
            persist_session_state: None,
            settings_store: None,
            tools: None,
            skill_runtime: None,
            initial_skill_name: None,
            active_skill_name: None,
            persist_checkpoint: true,
        });

        assert_eq!(result, Err(ContextCompactionTerminal::Failed));
        assert!(client.calls.load(Ordering::SeqCst) > 0);
        assert!(messages.is_empty(), "失败后不能重新使用超大原始上下文");
        assert!(store.thread_context_checkpoint(&thread_id).is_none());
        let turn = store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == request.turn_id)
            .expect("canonical turn should exist");
        let compaction_notice = turn
            .items
            .iter()
            .find(|item| {
                item.metadata.get("noticeKind") == Some(&serde_json::json!("context_compaction"))
            })
            .expect("failed compaction notice should exist");
        assert_eq!(compaction_notice.status, CanonicalTurnItemStatus::Failed);
        assert_eq!(
            compaction_notice.metadata.get("compactionState"),
            Some(&serde_json::json!("failed"))
        );
    }

    #[test]
    fn context_limit_recovery_cancellation_does_not_install_checkpoint() {
        let session_id = SessionId::new("session-context-compaction-cancelled");
        let store = Arc::new(SessionStore::new());
        store
            .create_session(session_id.clone(), "context compaction cancelled")
            .expect("session should create");
        let (_, thread_id) = store.ensure_session_mission(&session_id, ts(1), || {
            magi_core::MissionId::new("mission-context-compaction-cancelled")
        });
        store.append_thread_messages(
            &thread_id,
            (0..32)
                .map(|index| ThreadChatMessage {
                    role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
                    content: Some("x".repeat(1_000)),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                })
                .collect(),
            ts(2),
        );
        store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-context-compaction-cancelled".to_string(),
                    turn_seq: 3,
                    accepted_at: ts(3),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("继续".to_string()),
                    items: vec![],
                },
            )
            .expect("current turn should store");
        let request = SessionTurnExecutionRequest {
            session_id: session_id.clone(),
            turn_id: "turn-context-compaction-cancelled".to_string(),
            workspace_id: Some(WorkspaceId::new("workspace-context-compaction-cancelled")),
            prompt: "继续".to_string(),
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
            workspace_root_path: None,
        };
        let client = CancellingContextCompactionModelBridgeClient {
            store: Arc::clone(&store),
            session_id: session_id.clone(),
        };
        let event_bus = InMemoryEventBus::new(16);
        let mut messages = Vec::new();

        let result = rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
            client: &client,
            event_bus: &event_bus,
            session_store: &store,
            request: &request,
            thread_id: &thread_id,
            prompt: &request.prompt,
            knowledge_context_prompt: None,
            context_window: 4_000,
            messages: &mut messages,
            persist_session_state: None,
            settings_store: None,
            tools: None,
            skill_runtime: None,
            initial_skill_name: None,
            active_skill_name: None,
            persist_checkpoint: true,
        });

        assert_eq!(result, Err(ContextCompactionTerminal::Cancelled));
        assert!(messages.is_empty());
        assert!(store.thread_context_checkpoint(&thread_id).is_none());
        let turn = store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == request.turn_id)
            .expect("canonical turn should exist");
        let compaction_notice = turn
            .items
            .iter()
            .find(|item| {
                item.metadata.get("noticeKind") == Some(&serde_json::json!("context_compaction"))
            })
            .expect("cancelled compaction notice should exist");
        assert_eq!(compaction_notice.status, CanonicalTurnItemStatus::Cancelled);
        assert_eq!(
            compaction_notice.metadata.get("compactionState"),
            Some(&serde_json::json!("cancelled"))
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
                model_round: None,
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
