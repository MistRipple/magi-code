//! 任务系统 — usage recording helpers。
//!
//! 复用内部的 `model_config::NormalizedModelConfig` 与独立设置域。

use crate::model_config::{
    NormalizedModelConfig, configured_role_engine_model_config, resolve_orchestrator_model_config,
};
use crate::model_context_window::resolve_model_context_window;
use crate::prompt_lineage::{
    CacheInvalidationReason, PromptBranchScope, PromptFrame, PromptLineage, ProviderCacheScope,
};
use magi_bridge_client::{
    BridgeClientError, ModelBridgeClient, ModelInvocationRequest, ModelResponse,
};
use magi_core::{EventId, SessionId, ThreadId, UtcMillis, WorkspaceId, estimate_text_tokens};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, latest_usage_observation_for_session,
};
use magi_orchestrator::task_worker_catalog::WorkerInfo;
use magi_session_store::SessionStore;
use magi_settings_store::SettingsStore;
use magi_usage_authority::{
    ContextBudgetPolicy, ContextMeasurement, ContextPressureProjection, ContextPressureSnapshot,
    ContextUsageBreakdown, ExecutionBindingIdentity, LlmConfig, ModelIdentitySnapshot,
    UsageCallIdentity, UsageCallRecordInput, UsageCallStatus, UsagePhase, UsageSourceRole,
    UsageTokenInput, context_window_tokens_from_usage, prepare_llm_config_for_persistence,
    provider_context_tokens_from_usage,
};
use std::sync::Arc;

const CONTEXT_USAGE_EMIT_INTERVAL_MS: u64 = 300;
const CONTEXT_USAGE_EMIT_TOKEN_DELTA: u64 = 128;

/// 辅助调用的逻辑用途与用量阶段无关：标题、强化、建议和提取通常都处于
/// Integration，但不能因此竞争同一缓存路由分支。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuxiliaryModelPurpose {
    SessionTitle,
    PromptEnhance,
    KnowledgeExtraction,
    SessionMemoryExtraction,
}

impl AuxiliaryModelPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionTitle => "session_title",
            Self::PromptEnhance => "prompt_enhance",
            Self::KnowledgeExtraction => "knowledge_extraction",
            Self::SessionMemoryExtraction => "session_memory_extraction",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelUsageBinding {
    template_id: String,
    engine_id: String,
    binding_revision: u32,
    role: UsageSourceRole,
    phase: UsagePhase,
    model_config: Option<LlmConfig>,
}

impl ModelUsageBinding {
    pub(crate) fn with_task_model(
        mut self,
        task: &magi_core::Task,
        settings: Option<&Arc<SettingsStore>>,
    ) -> Result<Self, String> {
        if let Some(selection) = task
            .executor_binding
            .as_ref()
            .and_then(|binding| binding.model_selection.as_ref())
        {
            self.engine_id = selection
                .engine_id
                .clone()
                .unwrap_or_else(|| "orchestrator".into());
            self.binding_revision = selection.binding_revision;
            self.model_config = crate::model_config::frozen_agent_model_config(
                settings.ok_or("缺少子代理模型连接")?,
                selection,
            )?
            .to_usage_llm_config();
        }
        Ok(self)
    }

    pub fn tracks_active_context(&self) -> bool {
        self.role == UsageSourceRole::Orchestrator
    }

    pub fn binding_revision(&self) -> u32 {
        self.binding_revision
    }
}

pub struct ContextUsageRuntimeTrackerInput<'a> {
    pub event_bus: &'a InMemoryEventBus,
    pub settings_store: Option<&'a SettingsStore>,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub turn_id: Option<&'a str>,
    pub call_id: &'a str,
    pub resolved_model: &'a str,
    /// 本轮实际使用的模型窗口。调用方已经完成模型配置、会话覆盖和
    /// vision 接管的解析，运行时观测不得再次按模型名猜测窗口。
    pub context_window_tokens: u64,
    pub context_breakdown: ContextUsageBreakdown,
    pub thread_id: Option<&'a ThreadId>,
    pub model_provider: Option<String>,
    pub binding_revision: u32,
    pub checkpoint_generation: u64,
}

pub struct ContextUsageRuntimeTracker<'a> {
    input: ContextUsageRuntimeTrackerInput<'a>,
    last_emit_at: std::cell::Cell<u64>,
    last_emitted_tokens: std::cell::Cell<u64>,
}

impl<'a> ContextUsageRuntimeTracker<'a> {
    pub fn start(input: ContextUsageRuntimeTrackerInput<'a>) -> Self {
        publish_context_usage_update(
            input.event_bus,
            input.settings_store,
            input.session_id,
            input.workspace_id,
            input.turn_id,
            input.call_id,
            input.resolved_model,
            input.context_breakdown.total_tokens(),
            Some(input.context_window_tokens),
            input.thread_id,
            input.model_provider.as_deref(),
            input.binding_revision,
            input.checkpoint_generation,
            "prefill",
            "estimated",
            Some(&input.context_breakdown),
        );
        let prefill_tokens = input.context_breakdown.total_tokens();
        Self {
            input,
            last_emit_at: std::cell::Cell::new(0),
            last_emitted_tokens: std::cell::Cell::new(prefill_tokens),
        }
    }

    pub fn observe_accumulated_output(&self, content: &str, thinking: &str) {
        let estimated_output_tokens =
            estimate_text_tokens(content).saturating_add(estimate_text_tokens(thinking));
        let estimated_context_tokens = self
            .input
            .context_breakdown
            .total_tokens()
            .saturating_add(estimated_output_tokens as u64);
        let now = UtcMillis::now().0;
        let previous_tokens = self.last_emitted_tokens.get();
        if self.last_emit_at.get() != 0
            && now.saturating_sub(self.last_emit_at.get()) < CONTEXT_USAGE_EMIT_INTERVAL_MS
            && estimated_context_tokens.saturating_sub(previous_tokens)
                < CONTEXT_USAGE_EMIT_TOKEN_DELTA
        {
            return;
        }
        publish_context_usage_update(
            self.input.event_bus,
            self.input.settings_store,
            self.input.session_id,
            self.input.workspace_id,
            self.input.turn_id,
            self.input.call_id,
            self.input.resolved_model,
            estimated_context_tokens,
            Some(self.input.context_window_tokens),
            self.input.thread_id,
            self.input.model_provider.as_deref(),
            self.input.binding_revision,
            self.input.checkpoint_generation,
            "streaming",
            "estimated",
            Some(&{
                let mut breakdown = self.input.context_breakdown.clone();
                breakdown.add_conversation_tokens(estimated_output_tokens as u64);
                breakdown
            }),
        );
        self.last_emit_at.set(now);
        self.last_emitted_tokens.set(estimated_context_tokens);
    }
}

pub struct ModelUsageRecordInput<'a> {
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub binding: &'a ModelUsageBinding,
    pub call_id: String,
    pub usage: Option<&'a serde_json::Value>,
    pub context_breakdown: Option<ContextUsageBreakdown>,
    pub status: UsageCallStatus,
    pub assignment_id: Option<String>,
    pub error_code: Option<String>,
}

pub(crate) fn model_step_usage_context(
    settings: Option<&Arc<SettingsStore>>,
    binding: &ModelUsageBinding,
    session_id: &SessionId,
) -> Option<magi_app_server_protocol::ModelUsageContext> {
    let config = prepare_llm_config_for_persistence(usage_model_config_for_binding(
        settings, binding, session_id,
    )?);
    Some(
        serde_json::from_value(serde_json::json!({
            "templateId": binding.template_id, "engineId": binding.engine_id,
            "bindingRevision": binding.binding_revision, "role": binding.role,
            "phase": binding.phase, "modelConfig": config,
        }))
        .expect("trusted usage binding matches its schema"),
    )
}

pub struct AuxiliaryModelUsageContext<'a> {
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    /// 草稿态的辅助调用还没有会话；此时调用仍需执行，但不写入会话用量账本。
    pub session_id: Option<&'a SessionId>,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub call_id: String,
    pub phase: UsagePhase,
    pub purpose: AuxiliaryModelPurpose,
    /// 若辅助调用由某个执行 Turn 派生，固定其用量归属，避免异步回写落到后续 Turn。
    pub expected_turn_id: Option<&'a str>,
}

pub fn auxiliary_model_usage_binding(phase: UsagePhase) -> ModelUsageBinding {
    ModelUsageBinding {
        model_config: None,
        template_id: "auxiliary".to_string(),
        engine_id: "auxiliary".to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Auxiliary,
        phase,
    }
}

pub fn image_generation_model_usage_binding() -> ModelUsageBinding {
    ModelUsageBinding {
        model_config: None,
        template_id: "imageGeneration".to_string(),
        engine_id: "imageGeneration".to_string(),
        binding_revision: 0,
        role: UsageSourceRole::ImageGeneration,
        phase: UsagePhase::Execution,
    }
}

pub fn vision_model_usage_binding() -> ModelUsageBinding {
    ModelUsageBinding {
        model_config: None,
        template_id: "vision".to_string(),
        engine_id: "vision".to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Vision,
        phase: UsagePhase::Execution,
    }
}

/// 调用辅助模型并把这次调用写入统一用量账本。
pub fn invoke_auxiliary_model_with_usage(
    client: Arc<dyn ModelBridgeClient>,
    mut request: ModelInvocationRequest,
    context: AuxiliaryModelUsageContext<'_>,
) -> Result<ModelResponse, BridgeClientError> {
    // 复用实际 Bridge 的 Prompt IR 预处理；HTTP 连接在此完成工具 wire
    // name 编码，缓存摘要不能继续基于未编码的 canonical 工具面。
    let prompt_frame = PromptFrame::from_ir(&client.cache_prompt_ir(&request));
    let lineage = context.session_id.map(|session_id| {
        let branch = PromptBranchScope::Auxiliary {
            purpose: context.purpose.as_str().to_string(),
        };
        let scope = ProviderCacheScope::from_client(client.as_ref()).unwrap_or_else(|| {
            ProviderCacheScope {
                provider: Some(request.provider.clone()),
                ..Default::default()
            }
        });
        let epoch = prompt_frame.cache_epoch(&scope, 0, None);
        PromptLineage::for_branch(session_id, branch)
            .with_provider_scope(scope)
            .with_epoch(epoch, CacheInvalidationReason::Initial)
    });
    request.cache_affinity_key = lineage.as_ref().map(|lineage| lineage.lineage_id.clone());
    request.prompt_cache_key = lineage.as_ref().map(PromptLineage::cache_key);
    let mut binding = auxiliary_model_usage_binding(context.phase);
    let mut frozen_context = context.session_id.and_then(|session_id| {
        model_step_usage_context(context.settings_store, &binding, session_id)
    });
    if let Some(frozen) = frozen_context.as_mut() {
        frozen.workspace_id = context.workspace_id.as_ref().map(ToString::to_string);
        binding.model_config = serde_json::from_value(
            serde_json::to_value(&frozen.model_config).expect("safe config serializes"),
        )
        .ok();
    }
    let (response, receipts) = invoke_observed_model_with_usage(
        client.as_ref(),
        request,
        magi_bridge_client::ModelInvocationOptions {
            on_delta: None,
            on_retry: &|_| {},
            is_cancelled: &|| false,
            max_output_tokens: None,
        },
        context.event_bus,
        &|transport, attempt, call_status| {
            Some(crate::auxiliary_usage::AuxiliaryUsageFact {
                schema_version: 1,
                session_id: Some(context.session_id?.clone()),
                plugin: None,
                turn_id: context.expected_turn_id.map(str::to_string),
                call_id: context.call_id.clone(),
                attempt,
                context: frozen_context.as_ref()?.clone(),
                transport,
                call_status,
            })
        },
    );
    if context.session_id.is_some() && frozen_context.is_some() && !receipts.is_empty() {
        return response;
    }
    match response {
        Ok(response) => {
            if let Some(session_id) = context.session_id {
                publish_model_usage_record_internal(
                    context.event_bus,
                    context.session_store,
                    context.settings_store,
                    ModelUsageRecordInput {
                        context_breakdown: None,
                        session_id,
                        workspace_id: context.workspace_id,
                        binding: &binding,
                        call_id: context.call_id.clone(),
                        usage: response.usage.as_ref(),
                        status: UsageCallStatus::Success,
                        assignment_id: None,
                        error_code: None,
                    },
                    PublishModelUsageOptions {
                        explicit_model_config: None,
                        expected_turn_id: context.expected_turn_id,
                        context_window_override: None,
                        transport_records: Some(&receipts),
                    },
                );
            }
            Ok(response)
        }
        Err(error) => {
            if let Some(session_id) = context.session_id {
                publish_model_usage_record_internal(
                    context.event_bus,
                    context.session_store,
                    context.settings_store,
                    ModelUsageRecordInput {
                        context_breakdown: None,
                        session_id,
                        workspace_id: context.workspace_id,
                        binding: &binding,
                        call_id: context.call_id,
                        usage: None,
                        status: UsageCallStatus::Failed,
                        assignment_id: None,
                        error_code: Some("auxiliary_model_invocation_failed".to_string()),
                    },
                    PublishModelUsageOptions {
                        explicit_model_config: None,
                        expected_turn_id: context.expected_turn_id,
                        context_window_override: None,
                        transport_records: Some(&receipts),
                    },
                );
            }
            Err(error)
        }
    }
}

/// 辅助业务与插件业务共用同一传输记录、落盘和结算入口。
/// 观察回调落盘失败会中止 Bridge，不能在未记录请求时发送或重试。
pub(crate) fn invoke_observed_model_with_usage(
    client: &dyn ModelBridgeClient,
    request: ModelInvocationRequest,
    options: magi_bridge_client::ModelInvocationOptions<'_>,
    event_bus: &InMemoryEventBus,
    fact: &dyn Fn(
        magi_session_store::ModelTransportRecord,
        usize,
        Option<UsageCallStatus>,
    ) -> Option<crate::auxiliary_usage::AuxiliaryUsageFact>,
) -> (
    Result<ModelResponse, BridgeClientError>,
    Vec<magi_session_store::ModelTransportRecord>,
) {
    let receipts = std::cell::RefCell::new(magi_bridge_client::ModelTransportRecorder::default());
    let is_cancelled = options.is_cancelled;
    let response = client.invoke_observed(request, options, &|event| {
        let mut receipts = receipts.borrow_mut();
        let transport = receipts.apply(event)?.clone();
        if let Some(fact) = fact(transport, receipts.attempts.len(), None) {
            crate::auxiliary_usage::write_fact(event_bus, &fact)?;
        }
        Ok(())
    });
    let receipts = receipts.into_inner().attempts;
    for (index, transport) in receipts.iter().enumerate() {
        let last = index + 1 == receipts.len();
        let status = last.then_some(if is_cancelled() {
            UsageCallStatus::Cancelled
        } else if response.is_ok() {
            UsageCallStatus::Success
        } else {
            UsageCallStatus::Failed
        });
        if let Some(fact) = fact(transport.clone(), index + 1, status) {
            if last && let Err(error) = crate::auxiliary_usage::write_fact(event_bus, &fact) {
                return (Err(error), receipts);
            }
            crate::auxiliary_usage::publish_fact_usage(event_bus, &fact);
        }
    }
    (response, receipts)
}

/// 扩展模型调用沿同一角色绑定记录用量，不允许扩展自报模型身份。
pub(crate) fn extension_model_usage_binding(
    settings: Option<&Arc<SettingsStore>>,
    role: &str,
) -> Result<ModelUsageBinding, String> {
    let mut binding = session_turn_model_usage_binding(false);
    binding.template_id = role.to_owned();
    if let Some(config) = settings
        .map(|s| configured_role_engine_model_config(s, role))
        .transpose()?
        .flatten()
    {
        binding.engine_id = config.engine_id;
        binding.binding_revision = config.binding_revision;
        binding.model_config = config.config.to_usage_llm_config();
    }
    Ok(binding)
}

pub fn session_turn_model_usage_binding(use_tools: bool) -> ModelUsageBinding {
    ModelUsageBinding {
        model_config: None,
        template_id: "orchestrator".to_string(),
        engine_id: "orchestrator".to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Orchestrator,
        phase: if use_tools {
            UsagePhase::Execution
        } else {
            UsagePhase::Planning
        },
    }
}

pub fn model_usage_binding_for_worker(worker: &WorkerInfo, is_primary: bool) -> ModelUsageBinding {
    if is_primary {
        return ModelUsageBinding {
            model_config: None,
            template_id: "orchestrator".to_string(),
            engine_id: "orchestrator".to_string(),
            binding_revision: 0,
            role: UsageSourceRole::Orchestrator,
            phase: UsagePhase::Planning,
        };
    }
    ModelUsageBinding {
        model_config: None,
        template_id: worker.role.clone(),
        engine_id: worker.worker_id.to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Worker,
        phase: UsagePhase::Execution,
    }
}

pub fn model_usage_binding_for_worker_with_settings(
    worker: &WorkerInfo,
    is_primary: bool,
    settings_store: Option<&Arc<SettingsStore>>,
) -> ModelUsageBinding {
    let mut binding = model_usage_binding_for_worker(worker, is_primary);
    if is_primary {
        return binding;
    }
    if let Some(store) = settings_store
        && let Ok(Some(role_model)) = configured_role_engine_model_config(store, &worker.role)
    {
        binding.engine_id = role_model.engine_id;
        binding.binding_revision = role_model.binding_revision;
    }
    binding
}

pub fn publish_model_usage_record(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: Option<&Arc<SettingsStore>>,
    input: ModelUsageRecordInput<'_>,
) {
    publish_model_usage_record_internal(
        event_bus,
        session_store,
        settings_store,
        input,
        PublishModelUsageOptions {
            explicit_model_config: None,
            expected_turn_id: None,
            context_window_override: None,
            transport_records: None,
        },
    );
}

/// 为执行中的固定 Turn 记录模型用量。
///
/// 调用可能在当前 session 已切换到下一 Turn 后才返回，因此不能在写入时再读取
/// `current_turn` 推断归属；显式 Turn ID 只用于账本归属，不会把旧 Turn 写回当前状态。
pub fn publish_model_usage_record_for_turn(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: Option<&Arc<SettingsStore>>,
    expected_turn_id: Option<&str>,
    input: ModelUsageRecordInput<'_>,
) {
    publish_model_usage_record_internal(
        event_bus,
        session_store,
        settings_store,
        input,
        PublishModelUsageOptions {
            explicit_model_config: None,
            expected_turn_id,
            context_window_override: None,
            transport_records: None,
        },
    );
}

/// 为已绑定固定模型窗口的 Turn 记录用量。
///
/// authoritative pressure 事件必须继续使用该 Turn 实际发送请求时的窗口，不能在
/// provider 返回后重新按模型名或已变化的设置推导分母。
pub fn publish_model_usage_record_for_turn_with_context_window(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: Option<&Arc<SettingsStore>>,
    expected_turn_id: Option<&str>,
    context_window_tokens: u64,
    input: ModelUsageRecordInput<'_>,
) {
    publish_model_usage_record_internal(
        event_bus,
        session_store,
        settings_store,
        input,
        PublishModelUsageOptions {
            explicit_model_config: None,
            expected_turn_id,
            context_window_override: Some(context_window_tokens),
            transport_records: None,
        },
    );
}

pub fn publish_model_usage_record_with_config(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: Option<&Arc<SettingsStore>>,
    input: ModelUsageRecordInput<'_>,
    model_config: LlmConfig,
) {
    publish_model_usage_record_internal(
        event_bus,
        session_store,
        settings_store,
        input,
        PublishModelUsageOptions {
            explicit_model_config: Some(model_config),
            expected_turn_id: None,
            context_window_override: None,
            transport_records: None,
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub fn publish_context_usage_update(
    event_bus: &InMemoryEventBus,
    settings_store: Option<&SettingsStore>,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_id: Option<&str>,
    call_id: &str,
    resolved_model: &str,
    token_used: u64,
    context_window_override: Option<u64>,
    thread_id: Option<&ThreadId>,
    model_provider: Option<&str>,
    binding_revision: u32,
    checkpoint_generation: u64,
    phase: &str,
    accuracy: &str,
    context_breakdown: Option<&ContextUsageBreakdown>,
) {
    let context_window = context_window_override
        .unwrap_or_else(|| resolve_model_context_window(settings_store, resolved_model))
        .max(1);
    // 每次上下文压力更新都会走这里：读锁内按会话过滤扫描，不克隆整本账本。
    let previous_anchor = event_bus
        .with_audit_usage_ledger(|ledger| {
            latest_usage_observation_for_session(&ledger.usage_entries, session_id.as_str())
        })
        .filter(|observation| {
            observation
                .resolved_model
                .as_deref()
                .is_none_or(|model| model == resolved_model)
        })
        .filter(|observation| {
            observation
                .thread_id
                .as_deref()
                .is_none_or(|thread| thread_id.is_some_and(|current| current.as_str() == thread))
        })
        .filter(|observation| {
            observation
                .checkpoint_generation
                .is_none_or(|generation| generation == checkpoint_generation)
        })
        .filter(|observation| {
            observation
                .context_window_limit_tokens
                .is_none_or(|window| window == context_window)
        })
        .filter(|observation| observation.model_provider.as_deref() == model_provider)
        .filter(|observation| observation.binding_revision == Some(binding_revision))
        .and_then(|observation| observation.provider_context_tokens);
    let projected_tokens = if accuracy == "authoritative" {
        token_used
    } else {
        token_used.max(previous_anchor.unwrap_or_default())
    };
    let policy = ContextBudgetPolicy::for_window(context_window, None, 0);
    let remaining_tokens = context_window.saturating_sub(projected_tokens);
    let measurement = if accuracy == "authoritative" {
        ContextMeasurement::Provider
    } else {
        ContextMeasurement::Estimated
    };
    let snapshot = ContextPressureSnapshot::from_projected(ContextPressureProjection {
        session_id: session_id.to_string(),
        thread_id: thread_id.map(ToString::to_string),
        model: ModelIdentitySnapshot::new(
            model_provider.unwrap_or("configured"),
            resolved_model,
            binding_revision,
        ),
        policy,
        provider_context_tokens: if accuracy == "authoritative" {
            Some(token_used)
        } else {
            previous_anchor
        },
        projected_request_tokens: projected_tokens,
        measurement,
        anchor_call_id: Some(call_id.to_string()),
        checkpoint_generation,
        observed_at: UtcMillis::now().0,
    });
    let observed_at = UtcMillis::now();
    let payload = serde_json::json!({
        "session_id": session_id.to_string(),
        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        "thread_id": thread_id.map(ToString::to_string),
        "turn_id": turn_id,
        "call_id": call_id,
        "resolved_model": resolved_model,
        "model_provider": snapshot.model.provider,
        "binding_revision": snapshot.model.binding_revision,
        "phase": phase,
        "measurement": accuracy,
        "projected_request_tokens": snapshot.projected_request_tokens,
        "provider_context_tokens": snapshot.provider_context_tokens,
        "context_window_limit_tokens": snapshot.context_window_tokens,
        "remaining_tokens": remaining_tokens,
        "response_reserve_tokens": snapshot.response_reserve_tokens,
        "recovery_buffer_tokens": snapshot.recovery_buffer_tokens,
        "proactive_threshold_tokens": snapshot.proactive_threshold_tokens,
        "hard_request_limit_tokens": snapshot.hard_request_limit_tokens,
        "checkpoint_generation": snapshot.checkpoint_generation,
        "source_role": "orchestrator",
        "pressure_level": snapshot.pressure_level.as_str(),
        "usage_ratio": snapshot.projected_request_tokens as f64 / snapshot.context_window_tokens as f64,
        "observed_at": observed_at.0,
        "context_breakdown": context_breakdown,
    });
    let _ = event_bus.publish(
        EventEnvelope::usage(
            EventId::unique(format!(
                "session-context-usage-{call_id}-{phase}-{token_used}"
            )),
            "session.context.pressure.updated",
            payload,
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

struct PublishModelUsageOptions<'a> {
    explicit_model_config: Option<LlmConfig>,
    expected_turn_id: Option<&'a str>,
    context_window_override: Option<u64>,
    transport_records: Option<&'a [magi_session_store::ModelTransportRecord]>,
}

fn publish_model_usage_record_internal(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: Option<&Arc<SettingsStore>>,
    input: ModelUsageRecordInput<'_>,
    options: PublishModelUsageOptions<'_>,
) {
    let ModelUsageRecordInput {
        session_id,
        workspace_id,
        binding,
        call_id,
        usage,
        status,
        assignment_id,
        error_code,
        context_breakdown,
    } = input;
    let canonical_step = options
        .expected_turn_id
        .and_then(|turn_id| session_store.canonical_turn_for_session_turn_id(session_id, turn_id))
        .and_then(|turn| {
            turn.model_steps
                .into_iter()
                .find(|step| step.step_id == call_id)
        });
    let frozen_config = canonical_step
        .as_ref()
        .and_then(|step| step.binding.as_ref()?.usage_context.as_ref())
        .and_then(|context| {
            serde_json::from_value::<LlmConfig>(serde_json::to_value(&context.model_config).ok()?)
                .ok()
        });
    let usage = match usage_tokens_from_payload(usage) {
        Some(usage) => usage,
        None => UsageTokenInput {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: Some(0),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cache_read_included_in_input: false,
        },
    };
    let Some(model_config) = frozen_config
        .or(options.explicit_model_config)
        .or_else(|| usage_model_config_for_binding(settings_store, binding, session_id))
    else {
        tracing::warn!(
            template_id = binding.template_id,
            engine_id = binding.engine_id,
            "模型调用已返回用量，但缺少可审计的模型配置，跳过统计记录"
        );
        return;
    };
    let authoritative_context_tokens = provider_context_tokens_from_usage(&usage);
    let resolved_model = model_config.model.clone();
    let model_provider = model_config.provider.clone();
    let publishes_authoritative_context = binding.role == UsageSourceRole::Orchestrator
        && status == UsageCallStatus::Success
        && authoritative_context_tokens > 0;
    let workspace_id_value = workspace_id.as_ref().map(ToString::to_string);
    let usage_scope_key = workspace_id_value
        .clone()
        .unwrap_or_else(|| "personal".to_string());
    let attempts = if let Some(records) = options.transport_records {
        records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                (
                    format!("{call_id}:attempt:{}", index + 1),
                    Some(record.clone()),
                )
            })
            .collect::<Vec<_>>()
    } else {
        canonical_step
            .clone()
            .filter(|step| {
                step.attempts
                    .iter()
                    .any(|attempt| attempt.transport.is_some())
            })
            .map(|step| {
                step.attempts
                    .into_iter()
                    .map(|attempt| (attempt.attempt_id, attempt.transport))
                    .collect()
            })
            .unwrap_or_default()
    };
    let input = UsageCallRecordInput {
        workspace_id: workspace_id_value,
        owner: magi_usage_authority::UsageOwner::session(session_id.to_string()),
        turn_id: options
            .expected_turn_id
            .map(str::to_string)
            .or_else(|| current_turn_id(session_store, session_id)),
        dispatch_wave_id: None,
        assignment_id,
        event_id: Some(format!(
            "model-usage:{}:{}:{}",
            usage_scope_key, session_id, call_id
        )),
        timestamp: Some(UtcMillis::now().0),
        execution_binding: ExecutionBindingIdentity {
            template_id: binding.template_id.clone(),
            engine_id: binding.engine_id.clone(),
            binding_revision: binding.binding_revision,
            role: binding.role,
        },
        model_config: prepare_llm_config_for_persistence(model_config),
        call_identity: UsageCallIdentity {
            transport: None,
            call_id,
            parent_call_id: None,
            source: binding.role,
            phase: binding.phase,
        },
        usage,
        status,
        error_code,
    };
    let canonical_records = canonical_step
        .as_ref()
        .filter(|step| {
            !attempts.is_empty()
                && step
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.usage_context.is_some())
        })
        .map(|step| {
            crate::canonical_usage::model_step_usage_records(
                session_id,
                options.expected_turn_id.expect("canonical step has turn"),
                step,
            )
        });
    if let Some(records) = canonical_records {
        for record in records {
            publish_usage_call_input(event_bus, &record);
        }
    } else if attempts.is_empty() {
        publish_usage_call_input(event_bus, &input);
    } else {
        for (index, (attempt_id, transport)) in attempts.iter().enumerate() {
            let mut attempt = input.clone();
            attempt.call_identity.call_id = attempt_id.clone();
            attempt.call_identity.parent_call_id = Some(input.call_identity.call_id.clone());
            attempt.call_identity.transport = transport
                .as_ref()
                .map(magi_usage_authority::UsageTransportIdentity::from_receipt);
            attempt.event_id = Some(format!(
                "model-usage:{usage_scope_key}:{session_id}:{attempt_id}"
            ));
            let payload = transport
                .as_ref()
                .and_then(|record| record.usage.as_ref())
                .and_then(|usage| serde_json::to_value(usage).ok());
            attempt.usage =
                usage_tokens_from_payload(payload.as_ref()).unwrap_or(UsageTokenInput {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: Some(0),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cache_read_included_in_input: false,
                });
            let last = index + 1 == attempts.len();
            attempt.status = match transport
                .as_ref()
                .and_then(|record| record.outcome.as_ref())
            {
                Some(magi_session_store::ModelTransportOutcome::Cancelled) => {
                    UsageCallStatus::Cancelled
                }
                Some(magi_session_store::ModelTransportOutcome::Succeeded) if last => input.status,
                _ => UsageCallStatus::Failed,
            };
            if !last {
                attempt.error_code = Some(
                    transport
                        .as_ref()
                        .and_then(|record| record.http_status)
                        .filter(|status| *status >= 400)
                        .map(|status| format!("provider_http_{status}"))
                        .unwrap_or_else(|| "model_transport_retry".into()),
                );
            }
            publish_usage_call_input(event_bus, &attempt);
        }
    }
    if publishes_authoritative_context {
        // 完成统计沿原 Step 的线程和历史代际；迟到响应不能改绑到最新检查点。
        let thread_id = canonical_step.as_ref().map(|step| &step.thread_id);
        let checkpoint_generation = canonical_step
            .as_ref()
            .map_or(0, |step| step.checkpoint_generation);
        publish_context_usage_update(
            event_bus,
            settings_store.map(Arc::as_ref),
            session_id,
            workspace_id,
            input.turn_id.as_deref(),
            &input.call_identity.call_id,
            &resolved_model,
            authoritative_context_tokens,
            options.context_window_override,
            thread_id,
            Some(&model_provider),
            binding.binding_revision,
            checkpoint_generation,
            "completed",
            "authoritative",
            context_breakdown.as_ref(),
        );
    }
}

pub(crate) fn publish_usage_call_input(event_bus: &InMemoryEventBus, input: &UsageCallRecordInput) {
    let payload = match serde_json::to_value(input) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(?error, "序列化模型用量记录失败");
            return;
        }
    };
    let mut event = EventEnvelope::usage(
        EventId::new(
            input
                .event_id
                .clone()
                .unwrap_or_else(|| format!("model-usage-{}", UtcMillis::now().0)),
        ),
        "model.usage.recorded",
        payload,
    )
    .with_context(EventContext {
        workspace_id: input
            .workspace_id
            .as_ref()
            .map(|id| WorkspaceId::new(id.clone())),
        session_id: input.owner.session_id().map(SessionId::new),
        assignment_id: input
            .assignment_id
            .clone()
            .map(magi_core::AssignmentId::new),
        ..EventContext::default()
    });
    if let Some(at) = input.timestamp {
        event.occurred_at = UtcMillis(at);
    }
    event_bus.publish_usage_projection(event);
}

pub(crate) fn model_usage_tokens(usage: Option<&serde_json::Value>) -> Option<u64> {
    usage_tokens_from_payload(usage).map(|tokens| context_window_tokens_from_usage(&tokens))
}

pub(crate) fn current_turn_id(
    session_store: &SessionStore,
    session_id: &SessionId,
) -> Option<String> {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .map(|turn| turn.turn_id)
}

fn usage_model_config_for_binding(
    settings_store: Option<&Arc<SettingsStore>>,
    binding: &ModelUsageBinding,
    session_id: &SessionId,
) -> Option<LlmConfig> {
    if let Some(config) = binding.model_config.as_ref() {
        return Some(config.clone());
    }
    let store = settings_store?;
    match binding.role {
        UsageSourceRole::Worker => {
            if let Ok(Some(role_model)) =
                configured_role_engine_model_config(store, &binding.template_id)
            {
                return role_model.config.to_usage_llm_config();
            }
            let workers = store.get_section("workers");
            if let Some(config) = workers
                .get(&binding.engine_id)
                .or_else(|| workers.get(&binding.template_id))
                .and_then(|value| {
                    match NormalizedModelConfig::from_settings_with_catalog(store, value) {
                        Ok(config) => Some(config),
                        Err(error) => {
                            tracing::warn!(
                                role = %binding.template_id,
                                engine = %binding.engine_id,
                                error = %error,
                                "worker 模型配置无效，跳过用量身份归因"
                            );
                            None
                        }
                    }
                })
                .and_then(|config| config.to_usage_llm_config())
            {
                return Some(config);
            }
        }
        UsageSourceRole::Auxiliary => {
            let config = store.get_section("auxiliary");
            match NormalizedModelConfig::from_settings_with_catalog(store, &config) {
                Ok(config) => {
                    if let Some(config) = config.to_usage_llm_config() {
                        return Some(config);
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "auxiliary 模型配置无效，跳过用量身份归因");
                    return None;
                }
            }
        }
        UsageSourceRole::ImageGeneration => {
            let config = store.get_section("imageGeneration");
            match NormalizedModelConfig::from_settings_with_catalog(store, &config) {
                Ok(config) => return config.to_usage_llm_config(),
                Err(error) => {
                    tracing::warn!(error = %error, "图片模型配置无效，跳过用量身份归因");
                    return None;
                }
            }
        }
        UsageSourceRole::Vision => {
            let config = store.get_section(crate::model_config::VISION_MODEL_SECTION);
            match NormalizedModelConfig::from_settings_with_catalog(store, &config) {
                Ok(config) => return config.to_usage_llm_config(),
                Err(error) => {
                    tracing::warn!(error = %error, "vision 模型配置无效，跳过用量身份归因");
                    return None;
                }
            }
        }
        UsageSourceRole::Orchestrator => {}
    }
    match resolve_orchestrator_model_config(store, Some(session_id)) {
        Ok(config) => config.to_usage_llm_config(),
        Err(error) => {
            tracing::warn!(error = %error, "orchestrator 模型配置无效，跳过用量身份归因");
            None
        }
    }
}

pub(crate) fn resolved_model_for_usage_binding(
    settings_store: Option<&Arc<SettingsStore>>,
    binding: &ModelUsageBinding,
    session_id: &SessionId,
) -> Option<String> {
    usage_model_config_for_binding(settings_store, binding, session_id).map(|config| config.model)
}

pub(crate) fn resolved_provider_for_usage_binding(
    settings_store: Option<&Arc<SettingsStore>>,
    binding: &ModelUsageBinding,
    session_id: &SessionId,
) -> Option<String> {
    usage_model_config_for_binding(settings_store, binding, session_id)
        .map(|config| config.provider)
}

pub(crate) fn resolved_main_source_id_for_usage_binding(
    settings_store: Option<&Arc<SettingsStore>>,
    binding: &ModelUsageBinding,
    session_id: &SessionId,
) -> Option<String> {
    if !binding.tracks_active_context() {
        return None;
    }
    let provider = resolved_provider_for_usage_binding(settings_store, binding, session_id)?;
    (provider.starts_with("custom/") || provider.starts_with("plugin/")).then_some(provider)
}

fn usage_tokens_from_payload(usage: Option<&serde_json::Value>) -> Option<UsageTokenInput> {
    let usage = usage?;
    let input_tokens = usage_u64_field(
        usage,
        &[
            "prompt_tokens",
            "input_tokens",
            "promptTokens",
            "inputTokens",
        ],
    )
    .unwrap_or(0);
    let output_tokens = usage_u64_field(
        usage,
        &[
            "completion_tokens",
            "output_tokens",
            "completionTokens",
            "outputTokens",
        ],
    )
    .unwrap_or(0);
    let total_tokens = usage_u64_field(usage, &["total_tokens", "totalTokens"]).unwrap_or(0);
    let input_tokens = if input_tokens == 0 && output_tokens == 0 {
        total_tokens
    } else {
        input_tokens
    };
    let included_cache_read_tokens = usage_u64_pointer(
        usage,
        &[
            "/prompt_tokens_details/cached_tokens",
            "/input_tokens_details/cached_tokens",
            "/promptTokensDetails/cachedTokens",
            "/inputTokensDetails/cachedTokens",
        ],
    )
    .or_else(|| usage_u64_field(usage, &["prompt_cache_hit_tokens", "promptCacheHitTokens"]));
    let explicit_cache_read_tokens = usage_u64_field(
        usage,
        &[
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cache_read_tokens",
            "cacheReadTokens",
        ],
    );
    let cache_read_included_in_input = usage_bool_field(
        usage,
        &["cache_read_included_in_input", "cacheReadIncludedInInput"],
    )
    .unwrap_or(included_cache_read_tokens.is_some());
    let tokens = UsageTokenInput {
        input_tokens,
        output_tokens,
        total_tokens: (total_tokens > 0).then_some(total_tokens),
        cache_read_tokens: included_cache_read_tokens.or(explicit_cache_read_tokens),
        cache_write_tokens: usage_u64_field(
            usage,
            &[
                "cache_creation_input_tokens",
                "cacheCreationInputTokens",
                "cache_write_tokens",
                "cacheWriteTokens",
            ],
        ),
        cache_read_included_in_input,
    };
    // Anthropic 的 input_tokens 不含缓存，失败/取消响应可能只剩缓存用量。
    (input_tokens > 0
        || output_tokens > 0
        || tokens.cache_read_tokens.is_some()
        || tokens.cache_write_tokens.is_some())
    .then_some(tokens)
}

fn usage_u64_field(usage: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(|value| value.as_u64()))
}

fn usage_u64_pointer(usage: &serde_json::Value, pointers: &[&str]) -> Option<u64> {
    pointers
        .iter()
        .find_map(|pointer| usage.pointer(pointer).and_then(|value| value.as_u64()))
}

fn usage_bool_field(usage: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(|value| value.as_bool()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_step_usage_context_freezes_sanitized_connection_and_account_identity() {
        use super::*;
        let settings = Arc::new(SettingsStore::new());
        let session = SessionId::new("safe-usage-binding");
        settings
            .set_section(
                "orchestrator",
                crate::main_model_sources::test_sources(
                    "https://url-user:url-password@example.test/v1?api_key=url-token#fragment",
                    "private-key",
                    &["original-model", "later-model"],
                ),
            )
            .unwrap();
        settings
            .set_session_section(
                &session,
                "orchestrator",
                serde_json::json!({"sourceId":"custom/test","model":"original-model"}),
            )
            .unwrap();
        let frozen = model_step_usage_context(
            Some(&settings),
            &session_turn_model_usage_binding(true),
            &session,
        )
        .unwrap();
        magi_session_store::validate_model_usage_context(&frozen).unwrap();
        assert_eq!(frozen.model_config.base_url, "https://example.test/v1");
        assert!(frozen.model_config.account_fingerprint.is_some());
        let serialized = serde_json::to_string(&frozen).unwrap();
        for secret in [
            "url-user",
            "url-password",
            "url-token",
            "private-key",
            "fragment",
        ] {
            assert!(!serialized.contains(secret));
        }
        settings
            .set_session_section(
                &session,
                "orchestrator",
                serde_json::json!({"sourceId":"custom/test","model":"later-model"}),
            )
            .unwrap();
        assert_eq!(frozen.model_config.model, "original-model");
    }
    #[test]
    fn attempt_usage_uses_physical_identities_and_replay_does_not_double_charge() {
        use super::*;
        use magi_session_store::{
            ModelTransportOutcome, ModelTransportRecord, ModelTransportUsage,
        };
        let bus = InMemoryEventBus::new(16);
        let store = SessionStore::new();
        let session = SessionId::new("transport-ledger");
        let workspace = Some(WorkspaceId::new("workspace-transport"));
        let binding = auxiliary_model_usage_binding(UsagePhase::Integration);
        let receipts = vec![
            ModelTransportRecord {
                usage_observed: None,
                response_timing: None,
                observed_at: None,
                request_fingerprint: "a".repeat(64),
                http_status: Some(200),
                usage: Some(ModelTransportUsage {
                    input_tokens: 11,
                    output_tokens: 1,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cache_read_included_in_input: false,
                }),
                outcome: Some(ModelTransportOutcome::Failed),
            },
            ModelTransportRecord {
                usage_observed: None,
                response_timing: None,
                observed_at: None,
                request_fingerprint: "b".repeat(64),
                http_status: Some(200),
                usage: Some(ModelTransportUsage {
                    input_tokens: 20,
                    output_tokens: 2,
                    cache_read_tokens: Some(10),
                    cache_write_tokens: None,
                    cache_read_included_in_input: true,
                }),
                outcome: Some(ModelTransportOutcome::Succeeded),
            },
        ];
        for _ in 0..2 {
            publish_model_usage_record_internal(
                &bus,
                &store,
                None,
                ModelUsageRecordInput {
                    session_id: &session,
                    workspace_id: &workspace,
                    binding: &binding,
                    call_id: "logical-call".into(),
                    usage: None,
                    context_breakdown: None,
                    status: UsageCallStatus::Success,
                    assignment_id: None,
                    error_code: None,
                },
                PublishModelUsageOptions {
                    explicit_model_config: Some(LlmConfig {
                        provider: "test".into(),
                        model: "test".into(),
                        base_url: "http://localhost".into(),
                        api_key: None,
                        account_fingerprint: None,
                        reasoning_effort: None,
                    }),
                    expected_turn_id: Some("original-turn"),
                    context_window_override: None,
                    transport_records: Some(&receipts),
                },
            );
        }
        let mut authority = magi_usage_authority::UsageAuthority::new();
        let events = bus.snapshot().recent_events;
        assert_eq!(events.len(), 2);
        for event in events {
            let record: UsageCallRecordInput = serde_json::from_value(event.payload).unwrap();
            assert_eq!(
                record.call_identity.parent_call_id.as_deref(),
                Some("logical-call")
            );
            assert_eq!(record.turn_id.as_deref(), Some("original-turn"));
            assert_eq!(
                record.call_identity.transport.as_ref().unwrap().http_status,
                Some(200)
            );
            authority.append_call_record(record);
        }
        let snapshot = authority.get_session_snapshot("workspace-transport", session.as_str());
        assert_eq!(snapshot.totals.llm_call_count, 2);
        assert_eq!(snapshot.totals.raw_input_tokens, 31);
        assert_eq!(snapshot.totals.raw_output_tokens, 3);
        assert_eq!(snapshot.totals.total_tokens, 24);
        assert_eq!(snapshot.totals.cache_read_tokens, 10);
        assert_eq!(snapshot.totals.cache_observed_call_count, 1);
        assert_eq!(snapshot.totals.failure_count, 1);
    }
    use super::*;
    use magi_event_bus::InMemoryEventBus;
    use magi_session_store::SessionStore;
    use magi_usage_authority::UsageCallStatus;
    use serde_json::json;

    fn model_response(payload: serde_json::Value) -> ModelResponse {
        ModelResponse::from_chat_payload(
            serde_json::from_value(payload).expect("测试模型响应必须符合统一响应结构"),
        )
    }

    #[test]
    fn session_turn_model_usage_binding_respects_phase() {
        let planning = session_turn_model_usage_binding(false);
        let execution = session_turn_model_usage_binding(true);

        assert_eq!(planning.template_id, "orchestrator");
        assert_eq!(planning.engine_id, "orchestrator");
        assert!(matches!(planning.role, UsageSourceRole::Orchestrator));
        assert!(matches!(planning.phase, UsagePhase::Planning));
        assert!(matches!(execution.phase, UsagePhase::Execution));
    }

    #[test]
    fn auxiliary_cache_is_partitioned_by_real_purpose_and_drafts_have_no_identity() {
        struct Capture(std::sync::Mutex<Vec<Option<String>>>);
        impl ModelBridgeClient for Capture {
            fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
                magi_bridge_client::native_executor_capabilities()
            }
            fn invoke(
                &self,
                request: ModelInvocationRequest,
            ) -> Result<ModelResponse, BridgeClientError> {
                self.0.lock().unwrap().push(request.prompt_cache_key);
                Ok(ModelResponse::completed("ok"))
            }
            fn invoke_streaming(
                &self,
                request: ModelInvocationRequest,
                _: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
            ) -> Result<ModelResponse, BridgeClientError> {
                self.invoke(request)
            }
        }
        let client = Arc::new(Capture(std::sync::Mutex::new(Vec::new())));
        let store = SessionStore::new();
        let session = SessionId::new("purpose-cache-session");
        store.create_session(session.clone(), "cache").unwrap();
        let bus = InMemoryEventBus::new(32);
        let purposes = [
            AuxiliaryModelPurpose::SessionTitle,
            AuxiliaryModelPurpose::PromptEnhance,
            AuxiliaryModelPurpose::KnowledgeExtraction,
            AuxiliaryModelPurpose::SessionMemoryExtraction,
        ];
        for (index, purpose) in purposes
            .into_iter()
            .chain(purposes)
            .chain([purposes[0]])
            .enumerate()
        {
            invoke_auxiliary_model_with_usage(
                client.clone(),
                ModelInvocationRequest {
                    cache_affinity_key: None,
                    prompt_cache_key: Some("must-be-replaced".into()),
                    provider: "test".into(),
                    prompt: format!("input {index}"),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                AuxiliaryModelUsageContext {
                    event_bus: &bus,
                    session_store: &store,
                    settings_store: None,
                    session_id: (index < 8).then_some(&session),
                    workspace_id: &None,
                    call_id: format!("call-{index}"),
                    phase: UsagePhase::Integration,
                    purpose,
                    expected_turn_id: None,
                },
            )
            .unwrap();
        }
        let keys = client.0.lock().unwrap();
        assert_eq!(keys[..4], keys[4..8]);
        assert_eq!(
            keys[..5]
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4
        );
        assert!(keys[8].is_none());
    }

    #[test]
    fn deepseek_cache_hit_tokens_are_included_in_prompt_tokens() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "prompt_tokens":100, "completion_tokens":10, "prompt_cache_hit_tokens":60, "prompt_cache_miss_tokens":40
        }))).unwrap();
        assert_eq!(usage.cache_read_tokens, Some(60));
        assert!(usage.cache_read_included_in_input);
        assert_eq!(provider_context_tokens_from_usage(&usage), 100);
    }

    #[test]
    fn cache_only_usage_is_recorded_without_fabricating_uncached_input() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "input_tokens": 0, "output_tokens": 0,
            "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 200,
        })))
        .expect("cache-only usage");
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_read_tokens, Some(1000));
        assert_eq!(provider_context_tokens_from_usage(&usage), 1200);
        let zero = usage_tokens_from_payload(Some(&json!({
            "input_tokens": 0, "output_tokens": 0,
            "input_tokens_details": { "cached_tokens": 0 }
        })))
        .expect("explicit zero cache observation");
        assert_eq!(zero.cache_read_tokens, Some(0));
        assert!(usage_tokens_from_payload(Some(&json!({}))).is_none());
    }

    #[test]
    fn context_usage_runtime_tracker_publishes_prefill_and_streaming_estimates() {
        let event_bus = InMemoryEventBus::new(8);
        let settings_store = SettingsStore::new();
        let session_id = SessionId::new("session-context-runtime");
        let workspace_id = Some(WorkspaceId::new("workspace-context-runtime"));
        let tracker = ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
            event_bus: &event_bus,
            settings_store: Some(&settings_store),
            session_id: &session_id,
            workspace_id: &workspace_id,
            turn_id: Some("turn-context-runtime"),
            call_id: "call-context-runtime",
            resolved_model: "gpt-5.6-luna",
            context_window_tokens: 256_000,
            context_breakdown: ContextUsageBreakdown {
                conversation_tokens: 1_000,
                ..Default::default()
            },
            thread_id: None,
            model_provider: None,
            binding_revision: 0,
            checkpoint_generation: 0,
        });

        tracker.observe_accumulated_output("圆环实时增长", "正在思考");

        let events = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .filter(|event| event.event_type == "session.context.pressure.updated")
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload["phase"], json!("prefill"));
        assert_eq!(events[0].payload["measurement"], json!("estimated"));
        assert_eq!(events[0].payload["projected_request_tokens"], json!(1_000));
        assert_eq!(
            events[0].payload["context_window_limit_tokens"],
            json!(256_000)
        );
        assert_eq!(events[1].payload["phase"], json!("streaming"));
        assert_eq!(events[1].payload["measurement"], json!("estimated"));
        assert!(
            events[1].payload["projected_request_tokens"]
                .as_u64()
                .unwrap_or_default()
                > 1_000
        );
        assert_eq!(events[1].payload["call_id"], json!("call-context-runtime"));
        assert_eq!(events[1].payload["turn_id"], json!("turn-context-runtime"));
    }

    #[test]
    fn usage_tokens_from_payload_prefers_prompt_and_completion_fields() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "prompt_tokens": 3,
            "completion_tokens": 7,
            "prompt_tokens_details": { "cached_tokens": 2 },
            "cache_creation_input_tokens": 5
        })))
        .expect("usage tokens");

        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_read_tokens, Some(2));
        assert_eq!(usage.cache_write_tokens, Some(5));
        assert!(usage.cache_read_included_in_input);
    }

    #[test]
    fn usage_tokens_from_payload_reads_responses_input_details() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "input_tokens": 11,
            "output_tokens": 13,
            "total_tokens": 31,
            "input_tokens_details": { "cached_tokens": 4 }
        })))
        .expect("usage tokens");

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.total_tokens, Some(31));
        assert_eq!(usage.cache_read_tokens, Some(4));
        assert!(usage.cache_read_included_in_input);
    }

    #[test]
    fn usage_tokens_from_payload_preserves_zero_cache_reports() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "input_tokens": 11,
            "output_tokens": 13,
            "input_tokens_details": { "cached_tokens": 0 },
            "cache_creation_input_tokens": 0
        })))
        .expect("usage tokens");

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.cache_read_tokens, Some(0));
        assert_eq!(usage.cache_write_tokens, Some(0));
        assert!(usage.cache_read_included_in_input);
    }

    #[test]
    fn usage_tokens_from_payload_keeps_anthropic_cache_separate() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "input_tokens": 11,
            "output_tokens": 13,
            "cache_read_input_tokens": 4,
            "cache_creation_input_tokens": 6
        })))
        .expect("usage tokens");

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.cache_read_tokens, Some(4));
        assert_eq!(usage.cache_write_tokens, Some(6));
        assert!(!usage.cache_read_included_in_input);
    }

    #[test]
    fn usage_tokens_from_payload_honors_bridge_camel_case_inclusion_flag() {
        let usage = usage_tokens_from_payload(Some(&json!({
            "inputTokens": 11,
            "outputTokens": 13,
            "cacheReadTokens": 4,
            "cacheReadIncludedInInput": true
        })))
        .expect("usage tokens");

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.cache_read_tokens, Some(4));
        assert!(usage.cache_read_included_in_input);
    }

    #[test]
    fn publish_model_usage_record_emits_usage_event() {
        let event_bus = InMemoryEventBus::new(8);
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-1");
        settings_store
            .set_section(
                "orchestrator",
                crate::main_model_sources::test_sources(
                    "https://example.test",
                    "secret-success-call-key",
                    &["gpt-5.6-sol"],
                ),
            )
            .unwrap();
        settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({
                    "sourceId": "custom/test",
                    "model": "gpt-5.6-sol",
                    "reasoningEffort": "high"
                }),
            )
            .unwrap();
        let binding = session_turn_model_usage_binding(true);
        let workspace_id = Some(WorkspaceId::new("workspace-1"));

        let session_store = SessionStore::from_state_for_test(magi_session_store::SessionStoreState {
            canonical_turns: vec![serde_json::from_value(json!({
                "sessionId": session_id, "turnId": "usage-turn", "turnSeq": 1,
                "acceptedAt": 1, "status": "running", "items": [],
                "modelSteps": [{
                    "stepId": "call-1", "threadId": "original-execution-thread", "checkpointGeneration": 7,
                    "attempts": [{"attemptId": "call-1:attempt:1", "startedAt": 1, "settledAt": 2, "status": "response_received"}],
                    "requestFingerprint": "a".repeat(64), "provider": "test", "messageCount": 1,
                    "toolCount": 0, "inputRequestIds": [], "startedAt": 1, "settledAt": 2,
                    "status": "response_received", "responseFingerprint": "b".repeat(64)
                }]
            })).unwrap()],
            ..Default::default()
        });
        // 当前主线程没有检查点；完成事件仍应使用原 Step 的线程与代际。
        session_store.ensure_session_mission(&session_id, UtcMillis(3), || {
            magi_core::MissionId::new("usage-mission")
        });
        publish_model_usage_record_for_turn(
            &event_bus,
            &session_store,
            Some(&settings_store),
            Some("usage-turn"),
            ModelUsageRecordInput {
                context_breakdown: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                binding: &binding,
                call_id: "call-1".to_string(),
                usage: Some(&json!({"prompt_tokens": 3, "completion_tokens": 7})),
                status: UsageCallStatus::Success,
                assignment_id: Some("assignment-1".to_string()),
                error_code: None,
            },
        );

        let snapshot = event_bus.snapshot();
        assert_eq!(snapshot.recent_events.len(), 2);
        let usage_event = snapshot
            .recent_events
            .iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("usage event should exist");
        let context_event = snapshot
            .recent_events
            .iter()
            .find(|event| event.event_type == "session.context.pressure.updated")
            .expect("authoritative context event should exist");
        assert_eq!(usage_event.category, magi_event_bus::EventCategory::Usage);
        assert_eq!(usage_event.workspace_id.as_ref(), workspace_id.as_ref());
        assert_eq!(usage_event.payload["workspaceId"], json!("workspace-1"));
        assert_eq!(
            usage_event.payload["modelConfig"]["model"],
            json!("gpt-5.6-sol")
        );
        assert_eq!(
            usage_event.payload["modelConfig"]["reasoningEffort"],
            json!("high")
        );
        assert!(usage_event.payload["modelConfig"].get("apiKey").is_none());
        assert!(
            !usage_event
                .payload
                .to_string()
                .contains("secret-success-call-key")
        );
        assert_eq!(
            context_event.payload["thread_id"],
            json!("original-execution-thread")
        );
        assert_eq!(context_event.payload["checkpoint_generation"], json!(7));
        assert_eq!(context_event.payload["measurement"], json!("authoritative"));
        assert_eq!(context_event.payload["phase"], json!("completed"));
        assert_eq!(context_event.payload["projected_request_tokens"], json!(3));
        assert_eq!(
            context_event.payload["resolved_model"],
            json!("gpt-5.6-sol")
        );
    }

    #[test]
    fn fixed_turn_context_window_is_kept_for_authoritative_usage_snapshot() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-fixed-context-window");
        settings_store
            .set_section(
                "orchestrator",
                crate::main_model_sources::test_sources(
                    "https://example.test",
                    "secret",
                    &["gpt-fixed-window"],
                ),
            )
            .unwrap();
        settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({"sourceId": "custom/test", "model": "gpt-fixed-window"}),
            )
            .unwrap();
        let binding = session_turn_model_usage_binding(true);
        let workspace_id = None;

        let request_breakdown = ContextUsageBreakdown {
            conversation_tokens: 100,
            system_instruction_tokens: 25,
            ..Default::default()
        };
        let tracker = ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
            event_bus: &event_bus,
            settings_store: Some(&settings_store),
            session_id: &session_id,
            workspace_id: &workspace_id,
            turn_id: Some("turn-fixed-context-window"),
            call_id: "call-fixed-context-window",
            resolved_model: "gpt-fixed-window",
            context_window_tokens: 256_000,
            context_breakdown: request_breakdown.clone(),
            thread_id: None,
            model_provider: Some("configured".into()),
            binding_revision: 0,
            checkpoint_generation: 0,
        });
        tracker.observe_accumulated_output(&"output".repeat(100), "thinking");
        publish_model_usage_record_for_turn_with_context_window(
            &event_bus,
            &session_store,
            Some(&settings_store),
            Some("turn-fixed-context-window"),
            256_000,
            ModelUsageRecordInput {
                context_breakdown: Some(request_breakdown.clone()),
                session_id: &session_id,
                workspace_id: &workspace_id,
                binding: &binding,
                call_id: "call-fixed-context-window".to_string(),
                usage: Some(&json!({"prompt_tokens": 42, "completion_tokens": 7})),
                status: UsageCallStatus::Success,
                assignment_id: None,
                error_code: None,
            },
        );

        let event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| {
                event.event_type == "session.context.pressure.updated"
                    && event.payload["phase"] == "completed"
            })
            .expect("固定窗口必须写入 authoritative pressure 事件");
        assert_eq!(event.payload["context_window_limit_tokens"], json!(256_000));
        assert_eq!(event.payload["projected_request_tokens"], json!(42));
        assert_eq!(event.payload["context_breakdown"], json!(request_breakdown));
        let restored = event_bus
            .with_audit_usage_ledger(|ledger| {
                latest_usage_observation_for_session(&ledger.usage_entries, session_id.as_str())
            })
            .unwrap();
        assert_eq!(restored.context_breakdown, Some(request_breakdown));
    }

    struct SuccessfulAuxiliaryClient;

    impl ModelBridgeClient for SuccessfulAuxiliaryClient {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }

        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            Ok(model_response(json!({
                "content": "辅助模型已完成",
                "usage": {
                    "prompt_tokens": 17,
                    "completion_tokens": 9,
                    "total_tokens": 26
                }
            })))
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    #[test]
    fn invoke_auxiliary_model_with_usage_records_success_and_auxiliary_config() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        settings_store
            .set_section(
                "auxiliary",
                json!({
                    "baseUrl": "https://auxiliary.example.test/v1",
                    "apiProtocol": "openai_chat",
                    "apiKey": "secret-auxiliary-success-key",
                    "model": "auxiliary-success-model"
                }),
            )
            .unwrap();
        let session_id = SessionId::new("session-auxiliary-success");
        let workspace_id = Some(WorkspaceId::new("workspace-auxiliary-success"));

        let response = invoke_auxiliary_model_with_usage(
            Arc::new(SuccessfulAuxiliaryClient),
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "测试辅助模型用量记录".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            AuxiliaryModelUsageContext {
                event_bus: &event_bus,
                session_store: &session_store,
                settings_store: Some(&settings_store),
                session_id: Some(&session_id),
                workspace_id: &workspace_id,
                call_id: "auxiliary-success-call".to_string(),
                phase: UsagePhase::Integration,
                purpose: AuxiliaryModelPurpose::KnowledgeExtraction,
                expected_turn_id: None,
            },
        )
        .expect("辅助模型成功调用不应失败");

        assert_eq!(response.content.as_deref(), Some("辅助模型已完成"));
        let event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("辅助模型成功调用必须写入用量账本");
        assert_eq!(event.payload["status"], json!("success"));
        assert_eq!(
            event.payload["executionBinding"]["role"],
            json!("auxiliary")
        );
        assert_eq!(
            event.payload["modelConfig"]["model"],
            json!("auxiliary-success-model")
        );
        assert_eq!(event.payload["usage"]["inputTokens"], json!(17));
        assert_eq!(event.payload["usage"]["outputTokens"], json!(9));
        assert_eq!(event.payload["usage"]["totalTokens"], json!(26));
        assert!(
            !event
                .payload
                .to_string()
                .contains("secret-auxiliary-success-key")
        );
    }

    #[test]
    fn auxiliary_usage_inherits_orchestrator_identity_when_auxiliary_is_unconfigured() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-auxiliary-inherits-orchestrator");
        settings_store
            .set_section(
                "orchestrator",
                crate::main_model_sources::test_sources(
                    "https://orchestrator.example.test/v1",
                    "secret-orchestrator-key",
                    &["orchestrator-inherited-model"],
                ),
            )
            .unwrap();
        settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({"sourceId": "custom/test", "model": "orchestrator-inherited-model"}),
            )
            .unwrap();
        let binding = auxiliary_model_usage_binding(UsagePhase::Integration);
        let workspace_id = Some(WorkspaceId::new(
            "workspace-auxiliary-inherits-orchestrator",
        ));

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            ModelUsageRecordInput {
                context_breakdown: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                binding: &binding,
                call_id: "auxiliary-inherited-call".to_string(),
                usage: Some(&json!({"input_tokens": 5, "output_tokens": 3})),
                status: UsageCallStatus::Success,
                assignment_id: None,
                error_code: None,
            },
        );

        let event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("继承 orchestrator 的辅助调用必须写入用量账本");
        assert_eq!(
            event.payload["executionBinding"]["role"],
            json!("auxiliary")
        );
        assert_eq!(event.payload["callIdentity"]["phase"], json!("integration"));
        assert_eq!(
            event.payload["modelConfig"]["model"],
            json!("orchestrator-inherited-model")
        );
    }

    #[test]
    fn publish_model_usage_record_keeps_successful_call_without_token_usage() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-success-without-usage");
        settings_store
            .set_section(
                "auxiliary",
                json!({
                    "baseUrl": "https://example.test",
                    "apiProtocol": "openai_chat",
                    "model": "auxiliary-no-usage-model"
                }),
            )
            .unwrap();
        let binding = auxiliary_model_usage_binding(UsagePhase::Planning);
        let workspace_id = Some(WorkspaceId::new("workspace-success-without-usage"));

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            ModelUsageRecordInput {
                context_breakdown: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                binding: &binding,
                call_id: "call-success-without-usage".to_string(),
                usage: None,
                status: UsageCallStatus::Success,
                assignment_id: None,
                error_code: None,
            },
        );

        let event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("没有 token 的成功调用也必须保留调用统计");
        assert_eq!(event.payload["status"], json!("success"));
        assert_eq!(event.payload["usage"]["inputTokens"], json!(0));
        assert_eq!(event.payload["usage"]["outputTokens"], json!(0));
        assert_eq!(event.payload["usage"]["totalTokens"], json!(0));
    }

    #[test]
    fn image_generation_usage_records_calls_and_optional_provider_tokens() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-image-usage");
        settings_store
            .set_section(
                "imageGeneration",
                json!({
                    "baseUrl": "https://images.example.test/v1",
                    "apiProtocol": "openai_chat",
                    "apiKey": "secret-image-key",
                    "model": "gpt-image-test"
                }),
            )
            .unwrap();
        let binding = image_generation_model_usage_binding();
        let workspace_id = Some(WorkspaceId::new("workspace-image-usage"));
        let provider_usage = json!({
            "input_tokens": 18,
            "output_tokens": 32,
            "total_tokens": 50
        });

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            ModelUsageRecordInput {
                context_breakdown: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                binding: &binding,
                call_id: "image-call-1".to_string(),
                usage: Some(&provider_usage),
                status: UsageCallStatus::Success,
                assignment_id: None,
                error_code: None,
            },
        );

        let event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("图片模型调用必须进入统一模型用量账本");
        assert_eq!(
            event.payload["executionBinding"]["role"],
            json!("image_generation")
        );
        assert_eq!(
            event.payload["executionBinding"]["templateId"],
            json!("imageGeneration")
        );
        assert_eq!(
            event.payload["modelConfig"]["model"],
            json!("gpt-image-test")
        );
        assert_eq!(event.payload["usage"]["inputTokens"], json!(18));
        assert_eq!(event.payload["usage"]["outputTokens"], json!(32));
        assert_eq!(event.payload["usage"]["totalTokens"], json!(50));
    }

    #[test]
    fn publish_model_usage_record_keeps_failed_call_without_token_usage() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-failed-call");
        settings_store
            .set_section(
                "orchestrator",
                crate::main_model_sources::test_sources(
                    "https://example.test",
                    "secret-failed-call-key",
                    &["model-failed-call"],
                ),
            )
            .unwrap();
        settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({"sourceId": "custom/test", "model": "model-failed-call"}),
            )
            .unwrap();
        let binding = session_turn_model_usage_binding(true);
        let workspace_id = Some(WorkspaceId::new("workspace-failed-call"));

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            ModelUsageRecordInput {
                context_breakdown: None,
                session_id: &session_id,
                workspace_id: &workspace_id,
                binding: &binding,
                call_id: "call-failed".to_string(),
                usage: None,
                status: UsageCallStatus::Failed,
                assignment_id: None,
                error_code: Some("model_stream_interrupted".to_string()),
            },
        );

        let snapshot = event_bus.snapshot();
        assert_eq!(snapshot.recent_events.len(), 1);
        assert_eq!(snapshot.recent_events[0].payload["status"], json!("failed"));
        assert_eq!(
            snapshot.recent_events[0].payload["errorCode"],
            json!("model_stream_interrupted")
        );
        assert_eq!(snapshot.recent_events[0].payload["usage"]["inputTokens"], 0);
        assert_eq!(
            snapshot.recent_events[0].payload["usage"]["outputTokens"],
            0
        );
        assert!(
            snapshot.recent_events[0].payload["modelConfig"]
                .get("apiKey")
                .is_none()
        );
        assert!(
            snapshot.recent_events[0].payload["modelConfig"]["accountFingerprint"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            !snapshot.recent_events[0]
                .payload
                .to_string()
                .contains("secret-failed-call-key")
        );
    }

    #[test]
    fn publish_model_usage_record_emits_personal_session_usage_without_workspace() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        let session_id = SessionId::new("session-1");
        settings_store
            .set_section(
                "orchestrator",
                crate::main_model_sources::test_sources(
                    "https://example.test",
                    "",
                    &["gpt-session-test"],
                ),
            )
            .unwrap();
        settings_store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({
                    "sourceId": "custom/test",
                    "model": "gpt-session-test"
                }),
            )
            .unwrap();
        let binding = session_turn_model_usage_binding(true);

        publish_model_usage_record(
            &event_bus,
            &session_store,
            Some(&settings_store),
            ModelUsageRecordInput {
                context_breakdown: None,
                session_id: &session_id,
                workspace_id: &None,
                binding: &binding,
                call_id: "call-1".to_string(),
                usage: Some(&json!({"prompt_tokens": 3, "completion_tokens": 7})),
                status: UsageCallStatus::Success,
                assignment_id: Some("assignment-1".to_string()),
                error_code: None,
            },
        );

        let snapshot = event_bus.snapshot();
        let usage_event = snapshot
            .recent_events
            .iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("personal session usage should be recorded");
        assert!(usage_event.workspace_id.is_none());
        assert!(usage_event.payload.get("workspaceId").is_none());
        assert_eq!(usage_event.payload["sessionId"], json!(session_id.as_str()));
        assert!(
            snapshot
                .recent_events
                .iter()
                .any(|event| event.event_type == "session.context.pressure.updated"),
            "personal session should publish authoritative context usage"
        );
    }
}
