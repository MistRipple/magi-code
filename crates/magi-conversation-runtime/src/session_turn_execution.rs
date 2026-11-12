//! 任务系统 — session turn execution
//!
//! 错误返回值改为 `Result<_, String>`，调用方在 magi-api 边界用
//! `.map_err(|msg| ApiError::model_invocation_failed("执行 session turn 失败", msg))`
//! 等方式桥接到 `ApiError` 枚举。

use crate::context_authority::{
    ContextAuthority, ContextCompactionMode, ContextCompactionTerminal, ContextPrepareRequest,
    estimate_context_usage_breakdown,
};
#[cfg(test)]
use crate::context_authority::{ContextCompactionProgress, ContextCompactionRecord};
use crate::model_context_window::{
    conservative_context_limit_recovery_window, resolve_model_context_window_with_override,
};
use crate::{
    ConversationRegistry, SessionTurnInputBoundary, UserSignal,
    conversation_loop::thread_chat_message_to_chat_message,
    model_config::{resolve_orchestrator_model_config, resolve_vision_execution_config},
    model_error::{
        MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS,
        MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS, ModelFailureDiagnostic,
        classify_model_invocation_error, model_empty_response_recovery_prompt,
        model_stream_interruption_recovery_prompt, public_model_image_invocation_error_message,
    },
    prompt_utils::{
        PromptFragmentKind, current_access_profile_prompt, current_turn_context_priority_prompt,
        normalize_model_visible_content, system_prompt_fragment_message,
    },
    session_images::{SessionTurnImage, session_turn_image_sources},
    session_writeback::{
        CanonicalTurnEventSink, ContextCompactionWritebackContext, SessionStatePersistCallback,
        SessionTurnStreamPublishGate, append_session_turn_error_item,
        append_session_turn_error_item_without_terminal_commit,
        apply_model_response_round, new_context_compaction_item_id,
        persist_session_state_checkpoint, publish_current_session_turn_item_event,
        publish_model_retry_runtime_event, publish_session_turn_item_event,
        publish_session_turn_item_stream_event, session_turn_item, session_turn_stream_update,
        upsert_context_compaction_completed_notice, upsert_context_compaction_progress_notice,
        upsert_session_turn_item_for_turn,
    },
    tool_result_utils::{DeterministicToolFailure, emergency_model_round_failure},
    turn_stream_buffer::TurnStreamBuffer,
    usage_recording::{
        ContextUsageRuntimeTracker, ContextUsageRuntimeTrackerInput, ModelUsageBinding,
        publish_model_usage_record_for_turn,
        publish_model_usage_record_for_turn_with_context_window, resolved_model_for_usage_binding,
        resolved_provider_for_usage_binding, session_turn_model_usage_binding,
        vision_model_usage_binding,
    },
};
use magi_bridge_client::{
    ChatMessage, ModelBridgeClient, ModelInvocationRequest, ModelProviderContext,
    ModelResponseStatus, ModelStreamingDelta,
};
use magi_core::{AccessProfile, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{SessionStore, ThreadChatMessage};
use magi_settings_store::SettingsStore;
use magi_usage_authority::UsageCallStatus;
use std::{fmt, sync::Arc, time::Instant};

pub const BUSINESS_MODEL_PROVIDER: &str = "openai-compatible";

fn mark_turn_timing(
    stage: &'static str,
    request: &SessionTurnExecutionRequest,
    elapsed_ms: u128,
    provider_call_id: Option<&str>,
) {
    tracing::info!(
        target: "magi.performance",
        trace_id = request.request_id.as_deref().unwrap_or(&request.turn_id),
        request_id = request.request_id.as_deref().unwrap_or_default(),
        session_id = %request.session_id,
        turn_id = %request.turn_id,
        provider_call_id = provider_call_id.unwrap_or_default(),
        stage,
        elapsed_ms = elapsed_ms as u64,
        "conversation response timing"
    );
}

/// 用户显式发起的会话命令。命令与普通对话共用 Turn 接纳、队列、取消和终态链路，
/// 只替换执行器内的模型调用阶段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTurnCommand {
    /// `/compact`：立即压缩主线上下文，可附带对摘要重点的补充要求。
    CompactContext { instructions: Option<String> },
}

#[derive(Clone)]
pub struct SessionTurnExecutionRequest {
    pub session_id: SessionId,
    pub turn_id: String,
    pub workspace_id: Option<WorkspaceId>,
    /// 接纳时冻结的项目目录授权；普通对话执行和恢复均沿此事实核对。
    pub project_directories: Option<magi_core::ProjectDirectoryBinding>,
    pub prompt: String,
    pub images: Vec<SessionTurnImage>,
    pub context_references: Vec<crate::context_reference::SessionContextReference>,
    pub access_profile: AccessProfile,
    pub skill_name: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
    /// 非空时本轮执行该命令，不调用主模型生成回复。
    pub command: Option<SessionTurnCommand>,
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

/// Turn 终态提交策略。
///
/// 生产执行只使用 `Coordinator`。测试专用的 `Executor` 仅验证存储终态写回，
/// 不参与生产执行路径。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum TurnTerminalCommitPolicy {
    #[cfg(test)]
    Executor,
    #[default]
    Coordinator,
}

impl TurnTerminalCommitPolicy {
    const fn commits_terminal(self) -> bool {
        #[cfg(test)]
        if matches!(self, Self::Executor) {
            return true;
        }
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTurnFailureReason {
    ModelInvocationFailed,
    ModelStreamInterrupted,
    ModelEmptyResponse,
    ModelResponseInvalid,
    ModelImageInvocationFailed,
    ToolPolicyRejected,
    ContextCompactionFailed,
    RuntimeInvalidState,
}

impl SessionTurnFailureReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::ModelInvocationFailed => "model_invocation_failed",
            Self::ModelStreamInterrupted => "model_stream_interrupted",
            Self::ModelEmptyResponse => "model_empty_response",
            Self::ModelResponseInvalid => "model_response_invalid",
            Self::ModelImageInvocationFailed => "model_image_invocation_failed",
            Self::ToolPolicyRejected => "tool_policy_rejected",
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
}

impl SessionTurnExecutionError {
    fn new(reason: SessionTurnFailureReason, public_message: impl Into<String>) -> Self {
        Self {
            reason,
            diagnostic_code: reason.code().to_string(),
            public_message: public_message.into(),
            model_failure: None,
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
        }
    }

    /// 轮数上限沿用任务轮次的确定性失败语义与诊断码。
    fn from_round_limit_failure(failure: DeterministicToolFailure) -> Self {
        Self::new(
            SessionTurnFailureReason::ToolPolicyRejected,
            failure.summary,
        )
    }

    /// 供 API 调度器在执行线程未能启动时写入统一的 canonical error item。
    pub fn runtime_invalid_state_with_message(message: impl Into<String>) -> Self {
        Self::new(SessionTurnFailureReason::RuntimeInvalidState, message)
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
            "上下文压缩失败，本轮已停止。请检查压缩模型配置或网络后重试。",
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

fn current_turn_status_is_writable(status: &str) -> bool {
    !matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "interrupted"
            | "cancelled"
            | "canceled"
            | "killed"
            | "superseded"
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

#[cfg(test)]
fn projected_test_history(
    store: &SessionStore,
    request: &SessionTurnExecutionRequest,
) -> Vec<ThreadChatMessage> {
    let Some(thread) = store.orchestrator_thread_for_session(&request.session_id) else {
        return Vec::new();
    };
    store
        .thread_context_snapshot(&thread.thread_id)
        .unwrap()
        .model_history()
}

#[cfg(test)]
fn build_session_turn_messages(
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    prompt: &str,
    knowledge_context_prompt: Option<&str>,
    history: &[ThreadChatMessage],
) -> Vec<ChatMessage> {
    build_session_turn_messages_with_runtime(BuildSessionTurnMessagesWithRuntimeInput {
        session_store,
        request,
        prompt,
        knowledge_context_prompt,
        history,
        settings_store: None,
        safety_gate: None,
        skill_runtime: None,
        stable_contributions: &[],
    })
}

struct BuildSessionTurnMessagesWithRuntimeInput<'a> {
    session_store: &'a SessionStore,
    request: &'a SessionTurnExecutionRequest,
    prompt: &'a str,
    knowledge_context_prompt: Option<&'a str>,
    history: &'a [ThreadChatMessage],
    settings_store: Option<&'a Arc<SettingsStore>>,
    safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    /// 扩展 `prompt.contribute`（start 阶段）已接纳的稳定片段：任务开始时征询一次，之后冻结。
    stable_contributions: &'a [ChatMessage],
}

fn build_session_turn_messages_with_runtime(
    input: BuildSessionTurnMessagesWithRuntimeInput<'_>,
) -> Vec<ChatMessage> {
    let BuildSessionTurnMessagesWithRuntimeInput {
        session_store,
        request,
        prompt,
        knowledge_context_prompt,
        history,
        settings_store,
        safety_gate,
        skill_runtime,
        stable_contributions,
    } = input;
    let mut messages = Vec::new();
    let user_rules = settings_store.map(|store| {
        crate::prompt_utils::user_rules_from_settings(&store.get_section("userRules"))
    });
    let user_rules = user_rules.flatten();
    let safeguard = Some(crate::prompt_utils::render_safeguard_prompt(safety_gate));
    if let Some(developer) = crate::prompt_utils::compose_developer_instructions(
        None,
        user_rules.as_deref(),
        safeguard.as_deref(),
    ) {
        messages.push(crate::prompt_utils::developer_instructions_message(
            developer,
        ));
    }
    if let Some(skill_id) = request.skill_name.as_deref()
        && let Some(skill_message) = skill_runtime
            .and_then(|runtime| crate::prompt_utils::skill_prompt_message(runtime, skill_id))
    {
        messages.push(skill_message);
    }
    // 稳定贡献紧随静态规则，位于所有易变片段与历史之前，属于缓存前缀。
    messages.extend(stable_contributions.iter().cloned());
    messages.push(system_prompt_fragment_message(
        PromptFragmentKind::CurrentAccessProfile,
        current_access_profile_prompt(request.access_profile, "full"),
    ));
    messages.push(system_prompt_fragment_message(
        PromptFragmentKind::CurrentTurnPriority,
        current_turn_context_priority_prompt(),
    ));
    if let Some(execution_state) =
        session_execution_state_prompt(session_store, &request.session_id, &request.turn_id)
    {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::UserPlan,
            execution_state,
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
    if request.command.is_none()
        && !request.request_id.as_ref().is_some_and(|id| {
            messages
                .iter()
                .any(|message| message.input_request_id.as_ref() == Some(id))
        })
    {
        messages.push(ChatMessage {
            input_request_id: request.request_id.clone(),
            context_origin: Default::default(),
            role: "user".to_string(),
            content: Some(prompt.to_string()),
            images: session_turn_image_sources(&request.images),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        });
    }
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

struct RebuildMessagesForContextWindowInput<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    request: &'a SessionTurnExecutionRequest,
    thread_id: &'a magi_core::ThreadId,
    prompt: &'a str,
    knowledge_context_prompt: Option<&'a str>,
    context_window: u64,
    mode: ContextCompactionMode,
    identity_prompt: Option<&'a str>,
    safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    messages: &'a mut Vec<ChatMessage>,
    history_generation: &'a mut u64,
    request_context: &'a [ChatMessage],
    persist_session_state: Option<&'a SessionStatePersistCallback>,
    settings_store: Option<&'a Arc<SettingsStore>>,
    skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    persist_checkpoint: bool,
    stable_contributions: &'a [ChatMessage],
    extension_hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
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
        mode,
        identity_prompt,
        safety_gate,
        messages,
        history_generation,
        request_context,
        persist_session_state,
        settings_store,
        skill_runtime,
        persist_checkpoint,
        stable_contributions,
        extension_hooks,
    } = input;
    let phase = if mode == ContextCompactionMode::Automatic {
        "pre_model"
    } else {
        "context_limit_recovery"
    };
    let mut fixed_messages =
        build_session_turn_messages_with_runtime(BuildSessionTurnMessagesWithRuntimeInput {
            session_store,
            request,
            prompt,
            knowledge_context_prompt,
            history: &[],
            settings_store,
            safety_gate,
            skill_runtime,
            stable_contributions,
        });
    let pending_inputs = crate::context_authority::unprojected_input_messages(
        session_store,
        &request.session_id,
        Some(&request.turn_id),
        thread_id,
        messages,
    )?;
    fixed_messages.extend(pending_inputs.iter().cloned());
    let compaction_item_id = new_context_compaction_item_id(&request.turn_id, thread_id, phase);
    let compaction_writeback = ContextCompactionWritebackContext {
        event_bus,
        session_store,
        session_id: &request.session_id,
        workspace_id: &request.workspace_id,
        thread_id,
        item_id: &compaction_item_id,
        phase,
        persist_session_state,
        task: None,
        turn_visibility: None,
        expected_turn_id: Some(&request.turn_id),
    };
    let compaction_observer =
        |progress| upsert_context_compaction_progress_notice(compaction_writeback, progress);
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
    .with_expected_turn_id(Some(&request.turn_id))
    .with_extension_hooks(extension_hooks)
    .with_compaction_runtime(&compaction_observer, &compaction_cancelled)
    .prepare(ContextPrepareRequest {
        phase,
        context_window_tokens: context_window,
        additional_context: {
            let mut usage = estimate_context_usage_breakdown(&fixed_messages, None);
            usage.merge(&estimate_context_usage_breakdown(request_context, None));
            usage
        },
        persist_checkpoint,
        model_identity: None,
        mode,
    });
    if let Some(terminal) = prepared.terminal {
        return Err(terminal);
    }
    let compacted = prepared.compaction.is_some();
    if let Some(compaction) = prepared.compaction.as_ref() {
        upsert_context_compaction_completed_notice(compaction_writeback, compaction)
            .map_err(|_| ContextCompactionTerminal::Failed)?;
    }
    *history_generation = prepared.checkpoint_generation;
    let history = prepared.messages;
    *messages =
        build_session_turn_messages_with_runtime(BuildSessionTurnMessagesWithRuntimeInput {
            session_store,
            request,
            prompt,
            knowledge_context_prompt,
            history: &history,
            settings_store,
            safety_gate,
            skill_runtime,
            stable_contributions,
        });
    if let Some(identity_prompt) = identity_prompt {
        messages.insert(
            0,
            system_prompt_fragment_message(
                PromptFragmentKind::CurrentTurnPriority,
                identity_prompt,
            ),
        );
    }
    for input in pending_inputs {
        if !messages
            .iter()
            .any(|message| message.input_request_id == input.input_request_id)
        {
            messages.push(input);
        }
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
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub request: SessionTurnExecutionRequest,
    pub prompt: String,
    pub knowledge_context_prompt: Option<String>,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
    pub live_settings_store: Option<Arc<SettingsStore>>,
    /// 扩展钩子端口；纯会话不执行工具，但历史工具结果仍经过模型投影钩子。
    pub extension_hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
}

/// 纯会话入口的扩展钩子归属：没有 Task，用 Turn ID 充当 `taskId`，`round` 固定为 0
/// （会话的模型调用以 Turn 为单位，重试不改变归属）。
fn hook_scope(request: &SessionTurnExecutionRequest) -> crate::extension_hooks::ToolHookScope<'_> {
    crate::extension_hooks::ToolHookScope {
        session_id: request.session_id.as_str(),
        task_id: request.turn_id.as_str(),
        workspace_id: request.workspace_id.as_ref().map(|id| id.as_str()),
        worker_id: None,
        round: 0,
    }
}

/// 一次模型请求的扩展装配：`context.assemble` 改写已装配历史，`prompt.contribute`（round 阶段）
/// 的易变片段进入尾部。同一次装配内多次构造视图（估算、请求、流式恢复）共用一次征询结果。
struct ConversationAssembly<'a> {
    hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
    request: &'a SessionTurnExecutionRequest,
    context_window_tokens: u64,
    history: crate::extension_hooks::RequestHistoryProjection,
    tail: Vec<ChatMessage>,
    projection: std::cell::RefCell<magi_session_store::HistoryProjection>,
}

impl<'a> ConversationAssembly<'a> {
    fn new(
        hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
        request: &'a SessionTurnExecutionRequest,
        context_window_tokens: u64,
    ) -> Self {
        let tail = hooks
            .map(|hooks| {
                crate::extension_hooks::contribute_prompt(
                    hooks,
                    &hook_scope(request),
                    crate::extension_hooks::ContributePhase::Round,
                )
            })
            .unwrap_or_default();
        Self {
            hooks,
            request,
            context_window_tokens,
            history: Default::default(),
            tail,
            projection: Default::default(),
        }
    }

    fn view(
        &self,
        messages: &[ChatMessage],
        controls: &crate::request_controls::RequestControls,
    ) -> Vec<ChatMessage> {
        let (history, projection) = self.history.view(
            self.hooks,
            &hook_scope(self.request),
            messages,
            self.context_window_tokens,
        );
        *self.projection.borrow_mut() = projection;
        controls.view(&history, self.tail.iter().cloned())
    }
}

#[cfg(test)]
pub(crate) fn run_session_turn_execution_for_test(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    run_session_turn_execution_with_policy(runtime, TurnTerminalCommitPolicy::Executor)
}

/// Conversation profile 的执行入口。所有失败/完成状态由 Coordinator 提交。
pub(crate) fn run_session_turn_execution(
    runtime: SessionTurnExecutionRuntime<'_>,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    run_session_turn_execution_with_policy(runtime, TurnTerminalCommitPolicy::Coordinator)
}

fn run_session_turn_execution_with_policy(
    runtime: SessionTurnExecutionRuntime<'_>,
    terminal_policy: TurnTerminalCommitPolicy,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    let plan_store = runtime.plan_store;
    let session_store = runtime.session_store;
    let session_id = runtime.request.session_id.clone();
    let turn_id = runtime.request.turn_id.clone();
    let workspace_id = runtime.request.workspace_id.clone();
    let event_bus = runtime.event_bus;
    let result = run_session_turn_execution_inner(runtime, terminal_policy);
    if result.is_err() {
        let owns_active_plan = session_store
            .active_plan_for_execution_owner(&session_id, &turn_id)
            .is_some();
        let stopped_goal = session_store
            .active_goal_for_execution_owner(&session_id, &turn_id)
            .map(|goal| {
                // 偶发故障先自动重试，连续失败到上限才受阻（计划随之暂停）。
                session_store.observe_goal_runtime_failure(
                    &session_id,
                    &goal.goal_id,
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
    terminal_policy: TurnTerminalCommitPolicy,
) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
    let SessionTurnExecutionRuntime {
        client,
        event_bus,
        session_store,
        conversation_registry,
        plan_store: _,
        settings_store,
        safety_gate,
        skill_runtime,
        request,
        prompt,
        knowledge_context_prompt,
        persist_session_state,
        live_settings_store: _live_settings_store,
        extension_hooks,
    } = runtime;

    let execution_started = Instant::now();
    mark_turn_timing("runner_started", &request, 0, None);

    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }

    // session 一生一 mission：session turn 执行必须在已注册的 orchestrator thread 上。
    let orchestrator_thread = session_store
        .orchestrator_thread_for_session(&request.session_id)
        .ok_or_else(SessionTurnExecutionError::runtime_invalid_state)?;
    let orchestrator_thread_id = orchestrator_thread.thread_id;

    let selected_model_config = settings_store
        .and_then(|store| resolve_orchestrator_model_config(store, Some(&request.session_id)).ok());
    let selected_model = selected_model_config
        .as_ref()
        .and_then(|config| config.to_usage_llm_config())
        .map(|config| config.model)
        .unwrap_or_default();
    let selected_main_source_id = selected_model_config
        .as_ref()
        .and_then(|config| config.source_id().map(str::to_string));
    let configured_main_context_window = selected_model_config
        .as_ref()
        .and_then(|config| config.context_window_tokens());

    let current_turn_contains_images = !request.images.is_empty();
    let vision_execution_config = resolve_vision_execution_config(
        settings_store.map(Arc::as_ref),
        selected_main_source_id.as_deref(),
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
        session_turn_model_usage_binding(false)
    };
    let context_settings = settings_store.map(Arc::as_ref);
    let configured_context_window = vision_execution_config
        .as_ref()
        .and_then(|config| config.context_window_tokens())
        .or(configured_main_context_window);
    let mut effective_context_window = if vision_execution_config.is_none()
        && let Some(source_id) = selected_main_source_id.as_deref()
    {
        crate::model_context_window::resolve_main_source_model_context_window(
            context_settings,
            source_id,
            &resolved_context_model,
            configured_context_window,
        )
    } else {
        resolve_model_context_window_with_override(
            context_settings,
            &resolved_context_model,
            configured_context_window,
        )
    };
    // 扩展 `prompt.contribute`（start 阶段）：本 Turn 只征询一次，稳定片段随后冻结进前缀；
    // 压缩重建与后续请求都复用这一份，不会因再次征询而改变前缀。
    let stable_contributions: Vec<ChatMessage> = extension_hooks
        .map(|hooks| {
            crate::extension_hooks::contribute_prompt(
                hooks,
                &hook_scope(&request),
                crate::extension_hooks::ContributePhase::Start,
            )
        })
        .unwrap_or_default();
    let fixed_messages =
        build_session_turn_messages_with_runtime(BuildSessionTurnMessagesWithRuntimeInput {
            session_store,
            request: &request,
            prompt: &prompt,
            knowledge_context_prompt: knowledge_context_prompt.as_deref(),
            history: &[],
            settings_store,
            safety_gate,
            skill_runtime,
            stable_contributions: &stable_contributions,
        });
    let (compaction_phase, compaction_mode) = match request.command.as_ref() {
        Some(SessionTurnCommand::CompactContext { instructions }) => (
            "manual",
            ContextCompactionMode::Manual {
                instructions: instructions.clone(),
            },
        ),
        None => ("pre_turn", ContextCompactionMode::Automatic),
    };
    let manual_compaction = matches!(compaction_mode, ContextCompactionMode::Manual { .. });
    let compaction_item_id =
        new_context_compaction_item_id(&request.turn_id, &orchestrator_thread_id, compaction_phase);
    let compaction_writeback = ContextCompactionWritebackContext {
        event_bus,
        session_store,
        session_id: &request.session_id,
        workspace_id: &request.workspace_id,
        thread_id: &orchestrator_thread_id,
        item_id: &compaction_item_id,
        phase: compaction_phase,
        persist_session_state,
        task: None,
        turn_visibility: None,
        expected_turn_id: Some(&request.turn_id),
    };
    let compaction_observer =
        |progress| upsert_context_compaction_progress_notice(compaction_writeback, progress);
    let compaction_cancelled = || !request_turn_is_writable(session_store, &request);
    mark_turn_timing(
        "context_prepare_started",
        &request,
        execution_started.elapsed().as_millis(),
        None,
    );
    let prepared_history = {
        ContextAuthority::new(
            client,
            event_bus,
            session_store,
            &request.session_id,
            &request.workspace_id,
            &orchestrator_thread_id,
            settings_store,
        )
        .with_expected_turn_id(Some(&request.turn_id))
        .with_extension_hooks(extension_hooks)
        .with_compaction_runtime(&compaction_observer, &compaction_cancelled)
        .prepare(ContextPrepareRequest {
            phase: compaction_phase,
            context_window_tokens: effective_context_window,
            additional_context: estimate_context_usage_breakdown(&fixed_messages, None),
            persist_checkpoint: vision_execution_config.is_none(),
            model_identity: Some(magi_usage_authority::ModelIdentitySnapshot::new(
                vision_execution_config
                    .as_ref()
                    .map(|_| "vision")
                    .unwrap_or("configured"),
                resolved_context_model.clone(),
                0,
            )),
            mode: compaction_mode,
        })
    };
    mark_turn_timing(
        "context_prepare_completed",
        &request,
        execution_started.elapsed().as_millis(),
        None,
    );
    if let Some(terminal) = prepared_history.terminal {
        return match terminal {
            ContextCompactionTerminal::Cancelled => Ok(SessionTurnExecutionOutput::interrupted()),
            ContextCompactionTerminal::Failed => {
                Err(SessionTurnExecutionError::context_compaction_failed())
            }
        };
    }
    if let Some(compaction) = prepared_history.compaction.as_ref() {
        upsert_context_compaction_completed_notice(compaction_writeback, compaction).map_err(
            |error| {
                SessionTurnExecutionError::new(
                    SessionTurnFailureReason::ContextCompactionFailed,
                    error,
                )
            },
        )?;
    }
    if manual_compaction {
        // `/compact` 只更新上下文检查点；压缩结果由压缩通知 item 展示，
        // 不调用主模型，也不把命令本身写入模型可见历史。
        return Ok(SessionTurnExecutionOutput::completed(String::new()));
    }
    let identity_prompt =
        model_identity_prompt_for_request(&request.prompt, &resolved_context_model);
    let mut history_generation = prepared_history.checkpoint_generation;
    let mut messages =
        build_session_turn_messages_with_runtime(BuildSessionTurnMessagesWithRuntimeInput {
            session_store,
            request: &request,
            prompt: &prompt,
            knowledge_context_prompt: knowledge_context_prompt.as_deref(),
            history: &prepared_history.messages,
            settings_store,
            safety_gate,
            skill_runtime,
            stable_contributions: &stable_contributions,
        });
    if let Some(identity_prompt) = identity_prompt.as_ref() {
        messages.insert(
            0,
            system_prompt_fragment_message(
                PromptFragmentKind::CurrentTurnPriority,
                identity_prompt,
            ),
        );
    }

    let mut request_controls = crate::request_controls::RequestControls::default();
    let mut final_content: Option<String> = None;
    let mut final_item_id: Option<String> = None;
    let mut final_model_round: Option<usize> = None;
    let mut main_timeline_entry_id: Option<String> = None;
    let mut empty_response_recovery_attempts = 0usize;
    // 暂态故障的重试只发生在 HTTP 调用层；这里仅累计它上报的重试次数，用于失败诊断。
    let provider_retry_attempts = std::cell::Cell::new(0usize);
    let mut stream_interruption_recovery_attempts = 0usize;
    let mut context_limit_recovery_attempted = false;
    let mut last_response_observation: Option<String> = None;
    let mut round = 0usize;
    loop {
        if round > 0 {
            match rebuild_messages_for_context_window(RebuildMessagesForContextWindowInput {
                client,
                event_bus,
                session_store,
                request: &request,
                thread_id: &orchestrator_thread_id,
                prompt: &prompt,
                knowledge_context_prompt: knowledge_context_prompt.as_deref(),
                context_window: effective_context_window,
                mode: ContextCompactionMode::Automatic,
                identity_prompt: identity_prompt.as_deref(),
                safety_gate,
                messages: &mut messages,
                history_generation: &mut history_generation,
                request_context: &request_controls.view(&[], []),
                persist_session_state,
                settings_store,
                skill_runtime,
                persist_checkpoint: vision_execution_config.is_none(),
                stable_contributions: &stable_contributions,
                extension_hooks,
            }) {
                Ok(_) => {}
                Err(ContextCompactionTerminal::Cancelled) => {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                Err(ContextCompactionTerminal::Failed) => {
                    return Err(SessionTurnExecutionError::context_compaction_failed());
                }
            }
        }
        // 轮数上限与任务轮次共用：达到上限时按终止失败收口，不再继续调用模型。
        let round_result = match emergency_model_round_failure(round) {
            Some(failure) => Err(SessionTurnRoundError::RoundLimitReached(failure)),
            None => stream_session_turn_round(SessionTurnRoundRuntime {
                client,
                event_bus,
                session_store,
                settings_store,
                request: &request,
                usage_binding: &usage_binding,
                prompt: &prompt,
                messages: &mut messages,
                request_controls: &mut request_controls,
                provider_retry_attempts: &provider_retry_attempts,
                stream_interruption_recovery_attempts,
                round,
                context_window_tokens: effective_context_window,
                history_generation,
                orchestrator_thread_id: &orchestrator_thread_id,
                persist_session_state,
                extension_hooks,
            }),
        };
        let streamed_content = match round_result {
            Ok(output) => output,
            Err(SessionTurnRoundError::StreamInterruptedRecovered) => {
                stream_interruption_recovery_attempts += 1;
                round = round.saturating_add(1);
                continue;
            }
            Err(SessionTurnRoundError::WritebackFailed(error)) => {
                tracing::error!(%error, "会话 Turn 运行事实写回失败");
                return Err(SessionTurnExecutionError::runtime_invalid_state());
            }
            Err(SessionTurnRoundError::RoundLimitReached(failure)) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let execution_error = SessionTurnExecutionError::from_round_limit_failure(failure);
                if let Err(writeback_error) = append_session_turn_error_for_policy(
                    terminal_policy,
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
                        streaming_entry_id: main_timeline_entry_id.as_deref(),
                        source_thread_id: orchestrator_thread_id.clone(),
                        persist_session_state,
                        expected_turn_id: Some(&request.turn_id),
                    },
                ) {
                    tracing::error!(?writeback_error, "会话 Turn 失败事实写回失败");
                    return Err(SessionTurnExecutionError::runtime_invalid_state());
                }
                return Err(execution_error);
            }
            Err(SessionTurnRoundError::InvalidResponse(model_failure)) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let execution_error = SessionTurnExecutionError::from_model_failure(
                    SessionTurnFailureReason::ModelResponseInvalid,
                    *model_failure,
                );
                if let Err(writeback_error) = append_session_turn_error_for_policy(
                    terminal_policy,
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
                        streaming_entry_id: main_timeline_entry_id.as_deref(),
                        source_thread_id: orchestrator_thread_id.clone(),
                        persist_session_state,
                        expected_turn_id: Some(&request.turn_id),
                    },
                ) {
                    tracing::error!(?writeback_error, "会话 Turn 失败事实写回失败");
                    return Err(SessionTurnExecutionError::runtime_invalid_state());
                }
                return Err(execution_error);
            }
            Err(SessionTurnRoundError::Failed {
                error,
                context_overflow,
                non_stream_recovery_attempted,
            }) => {
                if !request_turn_is_writable(session_store, &request) {
                    return Ok(SessionTurnExecutionOutput::interrupted());
                }
                let classification = classify_model_invocation_error(&error);
                if classification.code == "model_empty_response"
                    && empty_response_recovery_attempts < MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
                {
                    empty_response_recovery_attempts += 1;
                    request_controls
                        .recover(model_empty_response_recovery_prompt(false).to_string());
                    tracing::warn!(
                        session_id = %request.session_id,
                        round,
                        attempt = empty_response_recovery_attempts,
                        max_attempts = MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS,
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
                            mode: ContextCompactionMode::Recovery,
                            identity_prompt: identity_prompt.as_deref(),
                            safety_gate,
                            client,
                            event_bus,
                            session_store,
                            request: &request,
                            thread_id: &orchestrator_thread_id,
                            prompt: &prompt,
                            knowledge_context_prompt: knowledge_context_prompt.as_deref(),
                            context_window: effective_context_window,
                            messages: &mut messages,
                            history_generation: &mut history_generation,
                            request_context: &request_controls.view(&[], []),
                            persist_session_state,
                            settings_store,
                            skill_runtime,
                            persist_checkpoint: vision_execution_config.is_none(),
                            stable_contributions: &stable_contributions,
                            extension_hooks,
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
                        continue;
                    }
                }
                let retry_attempts = provider_retry_attempts.get()
                    + stream_interruption_recovery_attempts
                    + empty_response_recovery_attempts
                    + usize::from(non_stream_recovery_attempted);
                let execution_error = if classification.code == "model_empty_response" {
                    session_turn_empty_response_error(&request, retry_attempts, Some(&error))
                } else {
                    session_turn_model_error(&request, &error, retry_attempts)
                };
                if let Err(writeback_error) = append_session_turn_error_for_policy(
                    terminal_policy,
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
                        streaming_entry_id: main_timeline_entry_id.as_deref(),
                        source_thread_id: orchestrator_thread_id.clone(),
                        persist_session_state,
                        expected_turn_id: Some(&request.turn_id),
                    },
                ) {
                    tracing::error!(?writeback_error, "会话 Turn 失败事实写回失败");
                    return Err(SessionTurnExecutionError::runtime_invalid_state());
                }
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
        if main_timeline_entry_id.is_none() {
            main_timeline_entry_id = streamed_content.timeline_entry_id.clone();
        }

        if let Some(content) = streamed_content.final_content {
            request_controls.clear();
            match conversation_registry
                .turn_coordinator()
                .take_session_turn_steers_or_close(&request.session_id, &request.turn_id)
            {
                SessionTurnInputBoundary::Pending(steers) => {
                    messages.push(ChatMessage {
                        input_request_id: None,
                        context_origin: Default::default(),
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
        // 本轮没有可见正文：按空响应恢复补一次答复约束，耗尽后按空响应失败收口。
        if empty_response_recovery_attempts >= MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS {
            break;
        }
        if !response_provider_context.is_empty() {
            messages.push(ChatMessage {
                input_request_id: None,
                context_origin: Default::default(),
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: response_provider_context,
            });
        }
        empty_response_recovery_attempts += 1;
        request_controls.recover(model_empty_response_recovery_prompt(false).to_string());
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
            empty_response_recovery_attempts,
            last_response_observation.as_deref(),
        );
        if let Err(writeback_error) = append_session_turn_error_for_policy(
            terminal_policy,
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
                streaming_entry_id: main_timeline_entry_id.as_deref(),
                source_thread_id: orchestrator_thread_id.clone(),
                persist_session_state,
                expected_turn_id: Some(&request.turn_id),
            },
        ) {
            tracing::error!(?writeback_error, "会话 Turn 失败事实写回失败");
            return Err(SessionTurnExecutionError::runtime_invalid_state());
        }
        return Err(failure);
    };
    if !request_turn_is_writable(session_store, &request) {
        return Ok(SessionTurnExecutionOutput::interrupted());
    }
    append_final_item(
        terminal_policy,
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
    )
    .map_err(|error| {
        tracing::error!(%error, "会话 Turn 最终回复写回失败");
        SessionTurnExecutionError::runtime_invalid_state()
    })?;
    Ok(SessionTurnExecutionOutput::completed(final_content))
}

pub(crate) fn append_session_turn_steers_to_messages(
    messages: &mut Vec<ChatMessage>,
    steers: Vec<UserSignal>,
) {
    for signal in steers {
        if signal.request_id.as_ref().is_some_and(|id| {
            messages
                .iter()
                .any(|message| message.input_request_id.as_ref() == Some(id))
        }) {
            continue;
        }
        let text = signal
            .text
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        if text.is_none() && signal.images.is_empty() {
            continue;
        }
        messages.push(ChatMessage {
            input_request_id: signal.request_id,
            context_origin: Default::default(),
            role: "user".to_string(),
            content: text,
            images: signal.images,
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        });
    }
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
    SessionTurnExecutionError::from_model_failure(
        SessionTurnFailureReason::ModelEmptyResponse,
        ModelFailureDiagnostic::empty_response(false, retry_attempts, response_observation),
    )
}

struct SessionTurnRoundRuntime<'a> {
    client: &'a dyn ModelBridgeClient,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    settings_store: Option<&'a Arc<SettingsStore>>,
    request: &'a SessionTurnExecutionRequest,
    usage_binding: &'a ModelUsageBinding,
    prompt: &'a str,
    messages: &'a mut Vec<ChatMessage>,
    request_controls: &'a mut crate::request_controls::RequestControls,
    provider_retry_attempts: &'a std::cell::Cell<usize>,
    stream_interruption_recovery_attempts: usize,
    round: usize,
    context_window_tokens: u64,
    history_generation: u64,
    /// session 主线 thread：该 turn 内所有 session_turn_item 的 source_thread_id。
    orchestrator_thread_id: &'a magi_core::ThreadId,
    persist_session_state: Option<&'a SessionStatePersistCallback>,
    extension_hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
}

struct SessionTurnRoundOutput {
    final_content: Option<String>,
    final_item_id: Option<String>,
    timeline_entry_id: Option<String>,
    response_observation: Option<String>,
    provider_context: Vec<ModelProviderContext>,
    interrupted: bool,
}

impl SessionTurnRoundOutput {
    fn interrupted(response_observation: Option<String>) -> Self {
        Self {
            final_content: None,
            final_item_id: None,
            timeline_entry_id: None,
            response_observation,
            provider_context: Vec::new(),
            interrupted: true,
        }
    }
}

/// 单轮流式调用的失败语义。仅在未交付可见内容时才能重放请求；已交付片段时必须续写。
#[derive(Debug)]
enum SessionTurnRoundError {
    Failed {
        error: String,
        context_overflow: Option<magi_bridge_client::ContextOverflowInfo>,
        non_stream_recovery_attempted: bool,
    },
    InvalidResponse(Box<ModelFailureDiagnostic>),
    RoundLimitReached(DeterministicToolFailure),
    StreamInterruptedRecovered,
    WritebackFailed(String),
}

fn apply_provider_context_metadata(
    item: &mut magi_session_store::ActiveExecutionTurnItem,
    provider_context: &[ModelProviderContext],
) {
    if !provider_context.is_empty() {
        item.metadata.insert(
            "providerContext".to_string(),
            serde_json::to_value(provider_context).expect("模型提供方上下文必须能够序列化"),
        );
    }
}

fn stream_session_turn_round(
    runtime: SessionTurnRoundRuntime<'_>,
) -> Result<SessionTurnRoundOutput, SessionTurnRoundError> {
    let SessionTurnRoundRuntime {
        client,
        event_bus,
        session_store,
        settings_store,
        request,
        usage_binding,
        prompt,
        messages,
        request_controls,
        provider_retry_attempts,
        stream_interruption_recovery_attempts,
        round,
        context_window_tokens,
        history_generation,
        orchestrator_thread_id,
        persist_session_state,
        extension_hooks,
    } = runtime;
    // 本次模型请求的扩展装配：round 阶段贡献与历史改写只征询一次，估算/请求/恢复共用。
    let assembly = ConversationAssembly::new(extension_hooks, request, context_window_tokens);

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
    let writeback_error = std::cell::RefCell::new(None::<String>);
    let call_id = format!(
        "session-turn-{}-{round}-{}",
        request.turn_id,
        UtcMillis::now().0
    );
    let provider_started_at = Instant::now();
    let first_raw_delta_reported = std::cell::Cell::new(false);
    let first_delta_reported = std::cell::Cell::new(false);
    mark_turn_timing(
        "provider_request_started",
        request,
        provider_started_at.elapsed().as_millis(),
        Some(&call_id),
    );
    let resolved_model =
        resolved_model_for_usage_binding(settings_store, usage_binding, &request.session_id)
            .unwrap_or_default();
    let resolved_provider =
        resolved_provider_for_usage_binding(settings_store, usage_binding, &request.session_id);
    let context_breakdown =
        estimate_context_usage_breakdown(&assembly.view(messages, request_controls), None);
    let context_usage_tracker = (usage_binding.tracks_active_context()).then(|| {
        ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
            event_bus,
            settings_store: settings_store.map(Arc::as_ref),
            session_id: &request.session_id,
            workspace_id: &request.workspace_id,
            turn_id: Some(&request.turn_id),
            call_id: &call_id,
            resolved_model: &resolved_model,
            context_window_tokens,
            context_breakdown: context_breakdown.clone(),
            thread_id: Some(orchestrator_thread_id),
            model_provider: resolved_provider,
            binding_revision: usage_binding.binding_revision(),
            checkpoint_generation: history_generation,
        })
    });
    let content_buffer = std::cell::RefCell::new(TurnStreamBuffer::default());
    let thinking_buffer = std::cell::RefCell::new(TurnStreamBuffer::default());
    // 模型客户端只发这一帧新增的文字；这里累积成到目前为止的全文，再算对外发布的增量。
    let delta_content = std::cell::RefCell::new(String::new());
    let delta_thinking = std::cell::RefCell::new(String::new());
    let on_delta = |delta: &ModelStreamingDelta| {
        if (!delta.content.is_empty() || !delta.thinking.is_empty() || !delta.tool_calls.is_empty())
            && !first_raw_delta_reported.replace(true)
        {
            mark_turn_timing(
                "provider_first_raw_delta",
                request,
                provider_started_at.elapsed().as_millis(),
                Some(&call_id),
            );
        }
        if writeback_error.borrow().is_some() || !request_turn_is_writable(session_store, request) {
            writeback_aborted.set(true);
            return;
        }
        if (!delta.content.is_empty() || !delta.thinking.is_empty())
            && !first_delta_reported.replace(true)
        {
            mark_turn_timing(
                "provider_first_delta",
                request,
                provider_started_at.elapsed().as_millis(),
                Some(&call_id),
            );
        }
        let mut accumulated_content_buf = delta_content.borrow_mut();
        let mut accumulated_thinking_buf = delta_thinking.borrow_mut();
        delta.accumulate_into(&mut accumulated_content_buf, &mut accumulated_thinking_buf);
        if let Some(tracker) = context_usage_tracker.as_ref() {
            tracker.observe_accumulated_output(&accumulated_content_buf, &accumulated_thinking_buf);
        }
        let accumulated_thinking = accumulated_thinking_buf.as_str();
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
            if thinking_buffer
                .borrow_mut()
                .push_at(accumulated_thinking, UtcMillis::now())
                .is_none()
            {
                return;
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
            match upsert_session_turn_item_for_turn(
                session_store,
                &request.session_id,
                Some(&request.turn_id),
                item,
                None,
            ) {
                Ok(Some(published)) => {
                    if let Some(stream_update) = stream_update.as_ref() {
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
                Ok(None) => {
                    writeback_error.replace(Some("当前 Turn 已不存在，无法写入思考流".to_string()));
                    writeback_aborted.set(true);
                }
                Err(error) => {
                    writeback_error.replace(Some(error));
                    writeback_aborted.set(true);
                }
            }
        }

        let accumulated = accumulated_content_buf.as_str();
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
        let visible_content = normalize_model_visible_content(accumulated);
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
        if content_buffer
            .borrow_mut()
            .push_at(&visible_content, UtcMillis::now())
            .is_none()
        {
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
        match upsert_session_turn_item_for_turn(
            session_store,
            &request.session_id,
            Some(&request.turn_id),
            item,
            None,
        ) {
            Ok(Some(published)) => {
                if let Some(stream_update) = stream_update.as_ref() {
                    publish_session_turn_item_stream_event(
                        event_bus,
                        &request.session_id,
                        &request.workspace_id,
                        &published,
                        stream_update,
                        &mut stream_publish_gate.borrow_mut(),
                    );
                }
            }
            Ok(None) => {
                writeback_error.replace(Some("当前 Turn 已不存在，无法写入回复流".to_string()));
                writeback_aborted.set(true);
            }
            Err(error) => {
                writeback_error.replace(Some(error));
                writeback_aborted.set(true);
            }
        }
    };

    let on_retry = |retry_event: &magi_bridge_client::ModelRetryRuntimeEvent| {
        if retry_event.phase == magi_bridge_client::ModelRetryRuntimePhase::Scheduled {
            provider_retry_attempts.set(provider_retry_attempts.get() + 1);
        }
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
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt: prompt.to_string(),
        messages: Some(assembly.view(messages, request_controls)),
        tools: None,
        tool_choice: None,
    };
    let step_client = crate::model_step::StepModelClient::new(
        client,
        session_store,
        &request.session_id,
        Some(&request.turn_id),
        None,
        orchestrator_thread_id,
        &call_id,
        history_generation,
    )
    .with_model_binding(settings_store, usage_binding)
    .with_history_projection(assembly.projection.borrow().clone())
    .with_extension_hooks(extension_hooks);
    let client: &dyn ModelBridgeClient = &step_client;
    let non_stream_recovery_template = invocation_request.clone();
    let response = match client.invoke_streaming_with_cancellation(
        invocation_request,
        &on_delta,
        &on_retry,
        &|| !request_turn_is_writable(session_store, request),
    ) {
        Ok(response) => response,
        Err(error) => {
            let raw_error = error.to_string();
            let classification = classify_model_invocation_error(&raw_error);
            publish_model_usage_record_for_turn(
                event_bus,
                session_store,
                settings_store,
                Some(&request.turn_id),
                crate::usage_recording::ModelUsageRecordInput {
                    context_breakdown: None,
                    session_id: &request.session_id,
                    workspace_id: &request.workspace_id,
                    binding: usage_binding,
                    call_id: step_client
                        .last_step_id()
                        .unwrap_or_else(|| call_id.clone()),
                    usage: None,
                    status: UsageCallStatus::Failed,
                    assignment_id: None,
                    error_code: Some(classification.code.to_string()),
                },
            );
            if !request_turn_is_writable(session_store, request) {
                return Ok(SessionTurnRoundOutput::interrupted(None));
            }
            let partial_visible_content = streamed_visible_content.borrow().trim().to_string();
            let partial_thinking = streamed_thinking.borrow().trim().to_string();
            if classification.code != "model_stream_interrupted" {
                return Err(SessionTurnRoundError::Failed {
                    error: raw_error,
                    context_overflow: error.context_overflow(),
                    non_stream_recovery_attempted: false,
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
                let published = match upsert_session_turn_item_for_turn(
                    session_store,
                    &request.session_id,
                    Some(&request.turn_id),
                    thinking_item,
                    None,
                ) {
                    Ok(Some(published)) => published,
                    Ok(None) => {
                        return Err(SessionTurnRoundError::WritebackFailed(
                            "当前 Turn 已不存在，无法保存中断前思考".to_string(),
                        ));
                    }
                    Err(error) => return Err(SessionTurnRoundError::WritebackFailed(error)),
                };
                {
                    persist_session_state_checkpoint(
                        persist_session_state,
                        "session_turn_stream_interrupted_thinking",
                    )
                    .map_err(SessionTurnRoundError::WritebackFailed)?;
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
                let published = match upsert_session_turn_item_for_turn(
                    session_store,
                    &request.session_id,
                    Some(&request.turn_id),
                    stream_item,
                    None,
                ) {
                    Ok(Some(published)) => published,
                    Ok(None) => {
                        return Err(SessionTurnRoundError::WritebackFailed(
                            "当前 Turn 已不存在，无法保存中断前回复".to_string(),
                        ));
                    }
                    Err(error) => return Err(SessionTurnRoundError::WritebackFailed(error)),
                };
                {
                    persist_session_state_checkpoint(
                        persist_session_state,
                        "session_turn_stream_interrupted_content",
                    )
                    .map_err(SessionTurnRoundError::WritebackFailed)?;
                    publish_session_turn_item_event(
                        event_bus,
                        &request.session_id,
                        &request.workspace_id,
                        &published,
                    );
                }
                messages.push(ChatMessage {
                    input_request_id: None,
                    context_origin: Default::default(),
                    role: "assistant".to_string(),
                    content: Some(partial_visible_content.clone()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
            }
            request_controls.recover(
                model_stream_interruption_recovery_prompt(!partial_visible_content.is_empty())
                    .to_string(),
            );
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

            let mut recovery_request = non_stream_recovery_template;
            recovery_request.messages = Some(
                ConversationAssembly::new(extension_hooks, request, context_window_tokens)
                    .view(messages, request_controls),
            );
            tracing::warn!(
                session_id = %request.session_id,
                round,
                preserved_visible_chars = partial_visible_content.len(),
                "流式恢复已耗尽，降级为非流式完成请求"
            );
            match client.invoke_with_retry_events_and_cancellation(
                recovery_request,
                &on_retry,
                &|| !request_turn_is_writable(session_store, request),
            ) {
                Ok(response) => response,
                Err(recovery_error) => {
                    let recovery_raw_error = recovery_error.to_string();
                    let recovery_classification =
                        classify_model_invocation_error(&recovery_raw_error);
                    publish_model_usage_record_for_turn(
                        event_bus,
                        session_store,
                        settings_store,
                        Some(&request.turn_id),
                        crate::usage_recording::ModelUsageRecordInput {
                            context_breakdown: None,
                            session_id: &request.session_id,
                            workspace_id: &request.workspace_id,
                            binding: usage_binding,
                            call_id: step_client
                                .last_step_id()
                                .unwrap_or_else(|| call_id.clone()),
                            usage: None,
                            status: UsageCallStatus::Failed,
                            assignment_id: None,
                            error_code: Some(recovery_classification.code.to_string()),
                        },
                    );
                    tracing::error!(
                        session_id = %request.session_id,
                        round,
                        ?recovery_error,
                        "非流式降级请求失败"
                    );
                    return Err(SessionTurnRoundError::Failed {
                        error: recovery_raw_error,
                        context_overflow: recovery_error.context_overflow(),
                        non_stream_recovery_attempted: true,
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
    // 对话轮次不向模型提供工具；模型若仍返回工具调用，只按正文处理，不解析也不执行。
    let has_actionable_output = parsed
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty());
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
    publish_model_usage_record_for_turn_with_context_window(
        event_bus,
        session_store,
        settings_store,
        Some(&request.turn_id),
        context_window_tokens,
        crate::usage_recording::ModelUsageRecordInput {
            context_breakdown: Some(context_breakdown),
            session_id: &request.session_id,
            workspace_id: &request.workspace_id,
            binding: usage_binding,
            call_id: step_client.last_step_id().unwrap_or(call_id),
            usage: parsed.usage.as_ref(),
            status: if has_actionable_output && response_contract_failure.is_none() {
                UsageCallStatus::Success
            } else {
                UsageCallStatus::Failed
            },
            assignment_id: None,
            error_code: if let Some(failure) = response_contract_failure.as_ref() {
                Some(failure.code.clone())
            } else {
                (!has_actionable_output).then(|| "model_empty_response".to_string())
            },
        },
    );
    let timeline_entry_id = None;
    if let Some(error) = writeback_error.into_inner() {
        return Err(SessionTurnRoundError::WritebackFailed(error));
    }
    if writeback_aborted.get() || !request_turn_is_writable(session_store, request) {
        return Ok(SessionTurnRoundOutput::interrupted(
            response_observation.clone(),
        ));
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
            return Ok(SessionTurnRoundOutput::interrupted(
                response_observation.clone(),
            ));
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
        let published = match upsert_session_turn_item_for_turn(
            session_store,
            &request.session_id,
            Some(&request.turn_id),
            thinking_item,
            None,
        ) {
            Ok(Some(published)) => published,
            Ok(None) => {
                return Err(SessionTurnRoundError::WritebackFailed(
                    "当前 Turn 已不存在，无法保存完成思考".to_string(),
                ));
            }
            Err(error) => return Err(SessionTurnRoundError::WritebackFailed(error)),
        };
        {
            persist_session_state_checkpoint(
                persist_session_state,
                "session_turn_thinking_completed",
            )
            .map_err(SessionTurnRoundError::WritebackFailed)?;
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
        .map(normalize_model_visible_content)
        .filter(|content| !content.trim().is_empty());
    let completed_stream_content = if !streamed_visible_content.trim().is_empty() {
        Some(streamed_visible_content.clone())
    } else {
        parsed_visible_content.clone()
    };
    if let Some(completed_stream_content) = completed_stream_content.as_ref() {
        if !request_turn_is_writable(session_store, request) {
            return Ok(SessionTurnRoundOutput::interrupted(
                response_observation.clone(),
            ));
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
        apply_provider_context_metadata(&mut stream_item, &parsed.provider_context);
        let published = match upsert_session_turn_item_for_turn(
            session_store,
            &request.session_id,
            Some(&request.turn_id),
            stream_item,
            None,
        ) {
            Ok(Some(published)) => published,
            Ok(None) => {
                return Err(SessionTurnRoundError::WritebackFailed(
                    "当前 Turn 已不存在，无法保存完成回复".to_string(),
                ));
            }
            Err(error) => return Err(SessionTurnRoundError::WritebackFailed(error)),
        };
        {
            persist_session_state_checkpoint(
                persist_session_state,
                "session_turn_stream_completed",
            )
            .map_err(SessionTurnRoundError::WritebackFailed)?;
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
        input_request_id: None,
        context_origin: Default::default(),
        role: "assistant".to_string(),
        content: parsed
            .content
            .clone()
            .or_else(|| completed_stream_content.clone()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: parsed.provider_context.clone(),
    };
    if assistant_response_message
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty())
        || !assistant_response_message.provider_context.is_empty()
    {
        persist_session_state_checkpoint(
            persist_session_state,
            "session_turn_thread_assistant_response",
        )
        .map_err(SessionTurnRoundError::WritebackFailed)?;
    }

    if let Some(failure) = response_contract_failure {
        return Err(SessionTurnRoundError::InvalidResponse(Box::new(failure)));
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
        response_observation,
        provider_context: parsed.provider_context.clone(),
        interrupted: false,
    })
}

fn append_session_turn_error_for_policy(
    terminal_policy: TurnTerminalCommitPolicy,
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    input: crate::session_writeback::SessionTurnErrorInput<'_>,
) -> Result<(), String> {
    if terminal_policy.commits_terminal() {
        append_session_turn_error_item(event_bus, session_store, input)
    } else {
        append_session_turn_error_item_without_terminal_commit(event_bus, session_store, input)
    }
}

struct FinalItemInput<'a> {
    content: &'a str,
    item_id: Option<&'a str>,
    timeline_entry_id: Option<&'a str>,
    model_round: Option<usize>,
}

fn append_final_item(
    terminal_policy: TurnTerminalCommitPolicy,
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    request: &SessionTurnExecutionRequest,
    input: FinalItemInput<'_>,
    orchestrator_thread_id: &magi_core::ThreadId,
    persist_session_state: Option<&SessionStatePersistCallback>,
) -> Result<(), String> {
    let FinalItemInput {
        content: final_content,
        item_id: final_item_id,
        timeline_entry_id,
        model_round,
    } = input;
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
    let final_item_id = final_item.item_id.clone();
    let _published = upsert_session_turn_item_for_turn(
        session_store,
        &request.session_id,
        Some(&request.turn_id),
        final_item,
        None,
    )?
    .ok_or_else(|| {
        format!(
            "会话 {} 的当前 Turn 已不可写，无法保存最终回复",
            request.session_id
        )
    })?;
    if terminal_policy.commits_terminal() {
        CanonicalTurnEventSink::for_store(session_store, None)
            .set_status_domain(&request.session_id, Some(&request.turn_id), "completed")
            .map_err(|error| {
                format!(
                    "会话 {} 的 Turn completed 状态提交失败: {error}",
                    request.session_id
                )
            })?
            .ok_or_else(|| {
                format!(
                    "会话 {} 的当前 Turn 已不可写，无法提交 completed 状态",
                    request.session_id
                )
            })?;
    }
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        &request.session_id,
        &request.workspace_id,
        &final_item_id,
        None,
    )?;
    // 终态事件只代表执行生命周期结束，projection 快照随后合并写入一次；
    // 否则 UI 停止状态会被几十 MB 级 checkpoint 串行拖住。
    persist_session_state_checkpoint(
        persist_session_state,
        if terminal_policy.commits_terminal() {
            "session_turn_completed"
        } else {
            "session_turn_final_item"
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
