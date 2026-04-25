use crate::{
    errors::ApiError,
    settings_store::SettingsStore,
    shadow_execution::run_shadow_dispatch_submission,
    state::{ApiState, ShadowExecutionPipeline},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition,
    HttpModelBridgeClient, ModelBridgeClient, ModelInvocationRequest, SHADOW_MODEL_PROVIDER,
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionOwnership, ExecutionResultStatus, LeaseId,
    RecoveryResumeInput, RiskLevel, SessionId, TaskExecutionTarget, TaskId, TaskStatus, ToolCallId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_knowledge_store::{KnowledgeKind, KnowledgeRecord, KnowledgeStore};
use magi_orchestrator::{
    ExecutionContextSummary, ExecutionWritebackPlans,
    task_runner::{EventBasedResultReceiver, TaskDispatcher, TaskOutcome, TaskResult, WorkerInfo},
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionTurnItem, SessionStore,
    TimelineEntryKind,
};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use magi_usage_authority::{
    ExecutionBindingIdentity, LlmConfig, OpenAiProtocol, ReasoningEffort, UrlMode,
    UsageCallIdentity, UsageCallRecordInput, UsageCallStatus, UsagePhase, UsageSourceRole,
    UsageTokenInput,
};
use magi_workspace::RecoveryStatus;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug)]
pub enum ShadowTaskExecutionPlan {
    Dispatch {
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        lane_id: Option<String>,
        lane_seq: Option<usize>,
        is_primary: bool,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
    },
}

pub struct ShadowGraphDriveResult {
    pub runner_started: bool,
}

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

#[derive(Clone, Debug)]
struct ModelUsageBinding {
    template_id: String,
    engine_id: String,
    binding_revision: u32,
    role: UsageSourceRole,
    phase: UsagePhase,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub entry_id: String,
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub deep_task: bool,
    pub skill_name: Option<String>,
    pub target_role: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionAccepted {
    pub session_id: SessionId,
    pub entry_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub runner_started: bool,
}

#[derive(Clone, Debug)]
pub struct SessionContinueAccepted {
    pub session_id: SessionId,
    pub mission_id: magi_core::MissionId,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub execution_chain_ref: String,
    pub resumed_branch_count: usize,
    pub runner_started: bool,
}

#[derive(Clone, Default)]
pub struct ShadowTaskExecutionRegistry {
    plans: Arc<RwLock<HashMap<TaskId, ShadowTaskExecutionPlan>>>,
}

fn task_status_is_terminal(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Completed | TaskStatus::Cancelled)
}

pub(crate) fn task_status_is_continue_recoverable(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Blocked)
}

fn task_status_needs_terminal_branch_finalization(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Blocked
            | TaskStatus::Ready
            | TaskStatus::Running
            | TaskStatus::Verifying
            | TaskStatus::Repairing
    )
}

pub(crate) fn branch_stage_is_terminal(stage: &str) -> bool {
    matches!(
        stage.trim().to_ascii_lowercase().as_str(),
        "finish" | "finished"
    )
}

pub(crate) fn active_execution_branch_is_continue_recoverable(
    state: &ApiState,
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> bool {
    if branch_stage_is_terminal(&branch.stage) {
        return false;
    }
    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(task) = task_store.get_task(&branch.task_id) else {
        return false;
    };
    task.mission_id == chain.mission_id
        && task.root_task_id == chain.root_task_id
        && task_status_is_continue_recoverable(&task.status)
}

fn terminal_status_for_branch(
    state: &ApiState,
    branch: &ActiveExecutionBranch,
) -> Option<TaskStatus> {
    let reports = state
        .shadow_execution_pipeline()?
        .execution_runtime
        .worker_runtime()
        .reports();
    reports
        .iter()
        .rev()
        .find(|report| {
            report.worker_id == branch.worker_id
                && report.task_id == branch.task_id
                && report.stage == magi_worker_runtime::WorkerStage::Finish
        })
        .map(|report| match report.termination_reason {
            Some(magi_core::TerminationReason::Failed) => TaskStatus::Failed,
            Some(magi_core::TerminationReason::Cancelled) => TaskStatus::Cancelled,
            Some(magi_core::TerminationReason::Blocked) => TaskStatus::Blocked,
            Some(magi_core::TerminationReason::Completed) | None => TaskStatus::Completed,
        })
}

pub(crate) fn finalize_terminal_worker_branches(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<usize, ApiError> {
    let Some(chain) = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
    else {
        return Ok(0);
    };
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("收敛 worker 终态失败", "task_store 未配置"))?;
    let mut finalized_count = 0usize;
    for branch in chain
        .branches
        .iter()
        .filter(|branch| branch_stage_is_terminal(&branch.stage))
    {
        let Some(task) = task_store.get_task(&branch.task_id) else {
            continue;
        };
        if !task_status_needs_terminal_branch_finalization(&task.status) {
            continue;
        }
        let terminal_status =
            terminal_status_for_branch(state, branch).unwrap_or(TaskStatus::Completed);
        if matches!(terminal_status, TaskStatus::Blocked) {
            continue;
        }
        task_store
            .update_status(&branch.task_id, terminal_status)
            .map_err(|error| ApiError::internal_assembly("收敛 worker 终态失败", error))?;
        finalized_count += 1;
    }
    Ok(finalized_count)
}

fn rebuild_dispatch_plan_for_branch(
    chain: &ActiveExecutionChain,
    branch: &ActiveExecutionBranch,
) -> ShadowTaskExecutionPlan {
    let ownership = ExecutionOwnership {
        session_id: Some(chain.session_id.clone()),
        workspace_id: chain.workspace_id.clone(),
        mission_id: Some(chain.mission_id.clone()),
        task_id: Some(branch.task_id.clone()),
        worker_id: Some(branch.worker_id.clone()),
        execution_chain_ref: Some(chain.execution_chain_ref.clone()),
    };
    let writebacks = if branch.is_primary {
        ExecutionWritebackPlans::from_session_action_input(
            magi_orchestrator::DispatchMemoryExtractionInput {
                accepted_at: chain.dispatch_context.accepted_at,
                session_id: &chain.session_id,
                timeline_entry_id: chain.dispatch_context.entry_id.as_str(),
                text: chain.dispatch_context.trimmed_text.as_deref(),
                skill_name: chain.dispatch_context.skill_name.as_deref(),
                deep_task: chain.dispatch_context.deep_task,
            },
        )
    } else {
        ExecutionWritebackPlans::default()
    };
    ShadowTaskExecutionPlan::Dispatch {
        target: TaskExecutionTarget {
            mission_id: chain.mission_id.clone(),
            root_task_id: chain.root_task_id.clone(),
            task_id: branch.task_id.clone(),
            requested_worker_id: Some(branch.worker_id.clone()),
            recovery_id: chain.recovery_ref.clone(),
            execution_chain_ref: Some(chain.execution_chain_ref.clone()),
        },
        worker_id: branch.worker_id.clone(),
        lane_id: chain.current_turn.as_ref().and_then(|turn| {
            turn.worker_lanes
                .iter()
                .find(|lane| lane.task_id == branch.task_id)
                .map(|lane| lane.lane_id.clone())
        }),
        lane_seq: chain.current_turn.as_ref().and_then(|turn| {
            turn.worker_lanes
                .iter()
                .find(|lane| lane.task_id == branch.task_id)
                .map(|lane| lane.lane_seq)
        }),
        is_primary: branch.is_primary,
        session_id: chain.session_id.clone(),
        workspace_id: chain.workspace_id.clone(),
        ownership,
        writebacks,
        use_tools: branch.use_tools,
        skill_name: branch.skill_name.clone(),
    }
}

fn validate_recovery_status(state: &ApiState, recovery_id: &str) -> Result<(), ApiError> {
    let export = state
        .workspace_registry
        .recovery_sidecar_export(recovery_id)
        .ok_or_else(|| ApiError::recovery_not_found(recovery_id))?;
    match export.current_status {
        RecoveryStatus::Ready => Ok(()),
        RecoveryStatus::Prepared => Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 当前状态为 prepared，必须先进入 ready 才能继续会话",
            recovery_id
        ))),
        RecoveryStatus::Consumed => Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 已被消费，不能再次继续会话",
            recovery_id
        ))),
    }
}

fn map_recovery_input_error(recovery_id: &str, error: magi_core::DomainError) -> ApiError {
    match error {
        magi_core::DomainError::NotFound { .. } => ApiError::recovery_not_found(recovery_id),
        magi_core::DomainError::InvalidState { message }
        | magi_core::DomainError::Validation { message } => ApiError::InvalidInput(message),
        magi_core::DomainError::AlreadyExists { entity } => ApiError::internal_assembly(
            "继续会话失败",
            format!("recovery 输入构建遇到重复实体: {entity}"),
        ),
    }
}

fn validate_recovery_input_matches_chain(
    chain: &ActiveExecutionChain,
    input: &RecoveryResumeInput,
) -> Result<(), ApiError> {
    if input.ownership.session_id.as_ref() != Some(&chain.session_id) {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 不属于当前会话 {}",
            input.recovery_id, chain.session_id
        )));
    }
    if input.ownership.mission_id.as_ref() != Some(&chain.mission_id) {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 不属于当前执行链 mission {}",
            input.recovery_id, chain.mission_id
        )));
    }
    if input.ownership.workspace_id != chain.workspace_id {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 的工作区与当前执行链不一致",
            input.recovery_id
        )));
    }
    if input.ownership.execution_chain_ref.as_deref() != Some(chain.execution_chain_ref.as_str()) {
        return Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 的 execution_chain_ref 与当前执行链不一致",
            input.recovery_id
        )));
    }
    Ok(())
}

fn apply_chain_recovery_if_needed(
    state: &ApiState,
    session_id: &SessionId,
    chain: &mut ActiveExecutionChain,
    primary_branch: &ActiveExecutionBranch,
) -> Result<(), ApiError> {
    let Some(recovery_id) = chain.recovery_ref.clone() else {
        return Ok(());
    };
    validate_recovery_status(state, &recovery_id)?;
    let input = state
        .workspace_registry
        .build_recovery_resume_input(&recovery_id)
        .map_err(|error| map_recovery_input_error(&recovery_id, error))?;
    validate_recovery_input_matches_chain(chain, &input)?;

    state
        .session_store
        .apply_recovery_resume_input(session_id.clone(), input.clone())
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    let writebacks = ExecutionWritebackPlans::from_continue_checkpoint_input(&input);
    if !writebacks.is_empty() {
        let pipeline = state.shadow_execution_pipeline().ok_or_else(|| {
            ApiError::internal_assembly("继续会话失败", "shadow execution pipeline 未配置")
        })?;
        writebacks.apply(&pipeline.memory_store);
    }

    state
        .workspace_registry
        .consume_recovery_with_ownership(
            &input.recovery_id,
            ExecutionOwnership {
                session_id: Some(chain.session_id.clone()),
                workspace_id: chain.workspace_id.clone(),
                mission_id: Some(chain.mission_id.clone()),
                task_id: Some(primary_branch.task_id.clone()),
                worker_id: Some(primary_branch.worker_id.clone()),
                execution_chain_ref: Some(chain.execution_chain_ref.clone()),
            },
        )
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    state
        .session_store
        .attach_recovery_ref(session_id, None)
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
    chain.recovery_ref = None;
    Ok(())
}

pub fn continue_shadow_execution_chain(
    state: &ApiState,
    session_id: &SessionId,
    requested_worker_ids: &[WorkerId],
) -> Result<SessionContinueAccepted, ApiError> {
    if state.session_store.session(session_id).is_none() {
        return Err(ApiError::session_not_found(session_id.as_str()));
    }
    let sidecar = state
        .session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    let mut chain = sidecar
        .active_execution_chain
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行链".to_string()))?;
    if &chain.session_id != session_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "session sidecar 与 active execution chain 不一致: {} != {}",
                chain.session_id, session_id
            ),
        ));
    }
    if let Some(ownership_chain_ref) = sidecar.ownership.execution_chain_ref.as_deref()
        && ownership_chain_ref != chain.execution_chain_ref
    {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "session sidecar 的 execution_chain_ref 与 active chain 不一致: {} != {}",
                ownership_chain_ref, chain.execution_chain_ref
            ),
        ));
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "task_store 未配置"))?;
    let root_task = task_store
        .get_task(&chain.root_task_id)
        .ok_or_else(|| ApiError::not_found("根任务不存在", chain.root_task_id.as_str()))?;
    if root_task.mission_id != chain.mission_id {
        return Err(ApiError::internal_assembly(
            "继续会话失败",
            format!(
                "active chain 的 mission_id 与根任务不一致: {} != {}",
                chain.mission_id, root_task.mission_id
            ),
        ));
    }
    finalize_terminal_worker_branches(state, session_id)?;

    let resumable_branches = chain
        .branches
        .iter()
        .filter_map(|branch| {
            active_execution_branch_is_continue_recoverable(state, &chain, branch)
                .then(|| branch.clone())
        })
        .collect::<Vec<_>>();
    if resumable_branches.is_empty() {
        return Err(ApiError::InvalidInput(
            "当前会话没有可继续的 branch".to_string(),
        ));
    }
    if !requested_worker_ids.is_empty() {
        for worker_id in requested_worker_ids {
            if !chain
                .branches
                .iter()
                .any(|branch| &branch.worker_id == worker_id)
            {
                return Err(ApiError::InvalidInput(format!(
                    "请求继续的 worker 不属于当前执行链: {}",
                    worker_id
                )));
            }
        }
        let has_requested_resumable_worker = requested_worker_ids.iter().any(|worker_id| {
            resumable_branches
                .iter()
                .any(|branch| &branch.worker_id == worker_id)
        });
        if !has_requested_resumable_worker {
            return Err(ApiError::InvalidInput(
                "请求继续的 worker 当前不可继续".to_string(),
            ));
        }
    }

    let primary_branch = resumable_branches
        .iter()
        .find(|branch| {
            requested_worker_ids
                .iter()
                .any(|worker_id| worker_id == &branch.worker_id)
        })
        .or_else(|| resumable_branches.iter().find(|branch| branch.is_primary))
        .or_else(|| resumable_branches.first())
        .expect("resumable_branches checked as non-empty");
    apply_chain_recovery_if_needed(state, session_id, &mut chain, primary_branch)?;

    let mut root_status = root_task.status;
    if matches!(root_status, TaskStatus::Completed) {
        task_store
            .update_status(&chain.root_task_id, TaskStatus::Blocked)
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        root_status = TaskStatus::Blocked;
    } else if task_status_is_terminal(&root_status) {
        return Err(ApiError::InvalidInput(
            "当前会话执行链已结束，不能继续".to_string(),
        ));
    }

    for branch in &resumable_branches {
        state.shadow_task_execution_registry().insert(
            branch.task_id.clone(),
            rebuild_dispatch_plan_for_branch(&chain, branch),
        );
    }

    state
        .session_store
        .apply_resume_execution_target(
            session_id,
            &TaskExecutionTarget {
                mission_id: chain.mission_id.clone(),
                root_task_id: chain.root_task_id.clone(),
                task_id: primary_branch.task_id.clone(),
                requested_worker_id: Some(primary_branch.worker_id.clone()),
                recovery_id: chain.recovery_ref.clone(),
                execution_chain_ref: Some(chain.execution_chain_ref.clone()),
            },
        )
        .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;

    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("继续会话失败", "runner_manager 未配置"))?;
    match root_status {
        TaskStatus::Blocked => manager
            .resume_tree(chain.root_task_id.as_str())
            .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?,
        TaskStatus::Running => {}
        other => {
            return Err(ApiError::InvalidInput(format!(
                "当前执行链状态不支持继续: {other:?}"
            )));
        }
    }
    for branch in &resumable_branches {
        if branch.task_id != chain.root_task_id
            && task_store
                .get_task(&branch.task_id)
                .is_some_and(|task| task.status == TaskStatus::Blocked)
        {
            task_store
                .update_status(&branch.task_id, TaskStatus::Ready)
                .map_err(|error| ApiError::internal_assembly("继续会话失败", error))?;
        }
    }
    Ok(SessionContinueAccepted {
        session_id: session_id.clone(),
        mission_id: chain.mission_id,
        root_task_id: chain.root_task_id,
        action_task_id: primary_branch.task_id.clone(),
        execution_chain_ref: chain.execution_chain_ref,
        resumed_branch_count: resumable_branches.len(),
        runner_started: true,
    })
}

impl ShadowTaskExecutionRegistry {
    pub fn insert(&self, task_id: TaskId, plan: ShadowTaskExecutionPlan) {
        self.plans
            .write()
            .expect("shadow task execution registry write lock poisoned")
            .insert(task_id, plan);
    }

    pub fn remove(&self, task_id: &TaskId) -> Option<ShadowTaskExecutionPlan> {
        self.plans
            .write()
            .expect("shadow task execution registry write lock poisoned")
            .remove(task_id)
    }
}

pub struct ShadowTaskDispatcher {
    event_bus: Arc<InMemoryEventBus>,
    pipeline: ShadowExecutionPipeline,
    session_store: Arc<SessionStore>,
    execution_registry: ShadowTaskExecutionRegistry,
    result_receiver: Arc<EventBasedResultReceiver>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    knowledge_store: Option<Arc<KnowledgeStore>>,
    knowledge_persist_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    settings_store: Option<Arc<crate::settings_store::SettingsStore>>,
    context_runtime: Option<Arc<ContextRuntime>>,
    tool_registry: Option<ToolRegistry>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
}

const MAX_TOOL_CALL_ROUNDS: usize = 8;
const SKILL_APPLY_TOOL_NAME: &str = "skill_apply";
pub const BUSINESS_MODEL_PROVIDER: &str = "openai-compatible";

pub fn resolve_configured_model_client(
    settings_store: Option<&Arc<SettingsStore>>,
    fallback: Option<Arc<dyn ModelBridgeClient>>,
) -> Option<Arc<dyn ModelBridgeClient>> {
    if let Some(store) = settings_store {
        let config = store.get_section("orchestrator");
        let base_url = config
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(base_url) = base_url {
            let api_key = config
                .get("apiKey")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let model = config
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "gpt-4".to_string());
            return Some(Arc::new(HttpModelBridgeClient::new(
                base_url.to_string(),
                api_key,
                model,
            )));
        }
    }
    fallback
}

fn parse_usage_url_mode(value: Option<&str>) -> UrlMode {
    match value {
        Some("full") => UrlMode::Full,
        Some("proxy") => UrlMode::Proxy,
        _ => UrlMode::Default,
    }
}

fn parse_usage_openai_protocol(value: Option<&str>) -> Option<OpenAiProtocol> {
    match value {
        Some("chat") => Some(OpenAiProtocol::Chat),
        Some("responses") => Some(OpenAiProtocol::Responses),
        _ => None,
    }
}

fn parse_usage_reasoning_effort(value: Option<&str>) -> Option<ReasoningEffort> {
    match value {
        Some("low") => Some(ReasoningEffort::Low),
        Some("medium") => Some(ReasoningEffort::Medium),
        Some("high") => Some(ReasoningEffort::High),
        Some("xhigh") => Some(ReasoningEffort::Xhigh),
        _ => None,
    }
}

fn usage_model_config_from_settings(value: &serde_json::Value) -> Option<LlmConfig> {
    let base_url = value
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())?;
    let model = value
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())?;
    Some(LlmConfig {
        provider: value
            .get("provider")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("openai")
            .to_string(),
        model: model.to_string(),
        base_url: base_url.to_string(),
        api_key: value
            .get("apiKey")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned),
        url_mode: parse_usage_url_mode(value.get("urlMode").and_then(|v| v.as_str())),
        openai_protocol: parse_usage_openai_protocol(
            value.get("openaiProtocol").and_then(|v| v.as_str()),
        ),
        reasoning_effort: parse_usage_reasoning_effort(
            value.get("reasoningEffort").and_then(|v| v.as_str()),
        ),
        enable_thinking: value
            .get("enableThinking")
            .and_then(|v| v.as_bool())
            .or_else(|| value.get("thinking").and_then(|v| v.as_bool())),
    })
}

fn usage_tokens_from_payload(usage: Option<&serde_json::Value>) -> Option<UsageTokenInput> {
    let usage = usage?;
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("input_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| usage.get("output_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let input_tokens = if input_tokens == 0 && output_tokens == 0 {
        total_tokens
    } else {
        input_tokens
    };
    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }
    Some(UsageTokenInput {
        input_tokens,
        output_tokens,
        cache_read_tokens: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
            }),
        cache_write_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64()),
    })
}

impl ShadowTaskDispatcher {
    pub fn new(
        event_bus: Arc<InMemoryEventBus>,
        pipeline: ShadowExecutionPipeline,
        session_store: Arc<SessionStore>,
        execution_registry: ShadowTaskExecutionRegistry,
        result_receiver: Arc<EventBasedResultReceiver>,
    ) -> Self {
        Self {
            event_bus,
            pipeline,
            session_store,
            execution_registry,
            result_receiver,
            model_bridge_client: None,
            knowledge_store: None,
            knowledge_persist_callback: None,
            settings_store: None,
            context_runtime: None,
            tool_registry: None,
            skill_runtime: None,
        }
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_knowledge_store(mut self, store: Arc<KnowledgeStore>) -> Self {
        self.knowledge_store = Some(store);
        self
    }

    pub fn with_knowledge_persist_callback(
        mut self,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.knowledge_persist_callback = Some(callback);
        self
    }

    pub fn with_settings_store(mut self, store: Arc<crate::settings_store::SettingsStore>) -> Self {
        self.settings_store = Some(store);
        self
    }

    pub fn with_context_runtime(mut self, runtime: Arc<ContextRuntime>) -> Self {
        self.context_runtime = Some(runtime);
        self
    }

    pub fn with_tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn with_skill_runtime(mut self, runtime: Arc<magi_skill_runtime::SkillRuntime>) -> Self {
        self.skill_runtime = Some(runtime);
        self
    }

    fn publish_task_dispatched_event(
        &self,
        task_id: &TaskId,
        mission_id: &magi_core::MissionId,
        worker: &WorkerInfo,
        lease_id: &LeaseId,
        kind: magi_core::TaskKind,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
    ) {
        let event = EventEnvelope::domain(
            EventId::new(format!("event-task-dispatched-{}", UtcMillis::now().0)),
            "task.dispatched",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": mission_id.to_string(),
                "session_id": session_id.map(ToString::to_string),
                "workspace_id": workspace_id.map(ToString::to_string),
                "worker_id": worker.worker_id.to_string(),
                "role": worker.role,
                "lease_id": lease_id.to_string(),
                "kind": format!("{:?}", kind),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: session_id.cloned(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn push_result(&self, task_id: &TaskId, lease_id: &LeaseId, outcome: TaskOutcome) {
        self.result_receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: lease_id.clone(),
            outcome,
        });
    }

    fn execute_dispatch_plan(
        &self,
        task: &magi_core::Task,
        task_id: &TaskId,
        lease_id: &LeaseId,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
        usage_binding: ModelUsageBinding,
    ) {
        // 仅在有 writebacks 时（即主 action task）才生成 streaming entry_id。
        // sub-task 的 writebacks 为空，不需要在 timeline 中创建流式条目。
        let streaming_entry_id = if writebacks.is_empty() {
            None
        } else {
            Some(format!("timeline-streaming-{}", task.task_id))
        };
        let (outcome, context_summary) = self.invoke_llm_with_tools(
            task,
            task_id,
            lease_id,
            &session_id,
            &workspace_id,
            use_tools,
            skill_name,
            &usage_binding,
            streaming_entry_id.as_deref(),
        );
        if matches!(&outcome, TaskOutcome::Completed { .. }) {
            self.session_store
                .bind_execution_ownership(session_id.clone(), ownership);
            let should_extract_knowledge = !writebacks.is_empty();
            writebacks.apply(&self.pipeline.memory_store);
            if should_extract_knowledge {
                self.extract_and_persist_knowledge(&session_id, &workspace_id, &outcome);
            }
            self.publish_execution_overview(task, &session_id, &workspace_id, context_summary);
        }
        self.push_result(task_id, lease_id, outcome);
    }

    fn extract_and_persist_knowledge(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        outcome: &TaskOutcome,
    ) {
        let Some(store) = self.knowledge_store.as_ref() else {
            return;
        };
        let TaskOutcome::Completed { output_refs } = outcome else {
            return;
        };

        let timeline_text = self
            .session_store
            .timeline_for_session(session_id)
            .into_iter()
            .rev()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
                )
            })
            .take(12)
            .map(|entry| entry.message)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n\n");
        let output_text = output_refs.join("\n\n");
        let extraction_text = format!("{timeline_text}\n\n{output_text}");
        let learnings = extract_learning_candidates(&extraction_text);
        if learnings.is_empty() {
            return;
        }

        let existing = store.list();
        let mut inserted = 0usize;
        for (index, learning) in learnings.into_iter().enumerate() {
            if knowledge_duplicate(&existing, KnowledgeKind::Learning, &learning.content) {
                continue;
            }
            let now = UtcMillis::now();
            store.upsert(KnowledgeRecord {
                knowledge_id: format!("learning-auto-{}-{index}", now.0),
                kind: KnowledgeKind::Learning,
                title: title_from_learning_content(&learning.content),
                content: learning.content,
                tags: learning.tags,
                workspace_id: workspace_id.clone(),
                source_ref: Some(
                    learning
                        .context
                        .unwrap_or_else(|| format!("session:{}", session_id.as_str())),
                ),
                updated_at: now,
            });
            inserted += 1;
        }
        if inserted > 0 {
            if let Some(callback) = self.knowledge_persist_callback.as_ref() {
                callback();
            }
        }
    }

    fn publish_execution_overview(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        context_summary: Option<ExecutionContextSummary>,
    ) {
        let context_payload = context_summary
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let event = EventEnvelope::audit(
            EventId::new(format!("event-mission-overview-{}", UtcMillis::now().0)),
            "mission.execution.overview",
            serde_json::json!({
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "context": context_payload,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn build_tool_definitions(&self, include_orchestration_tools: bool) -> Vec<ChatToolDefinition> {
        let Some(ref registry) = self.tool_registry else {
            return Vec::new();
        };
        let mut definitions = registry
            .builtin_specs()
            .into_iter()
            .filter(|spec| {
                if spec.name == SKILL_APPLY_TOOL_NAME {
                    return false;
                }
                if include_orchestration_tools {
                    return true;
                }
                !BuiltinToolName::from_str(&spec.name)
                    .map(|tool_name| tool_name.is_orchestration())
                    .unwrap_or(false)
            })
            .map(|spec| ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: spec.name.clone(),
                    description: chat_tool_description(&spec.name),
                    parameters: chat_tool_parameters(&spec.name),
                },
            })
            .collect::<Vec<_>>();
        if self.skill_runtime.is_some() {
            definitions.push(skill_apply_tool_definition());
        }
        definitions
    }

    fn assemble_prompt(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> (String, Option<ExecutionContextSummary>) {
        let base_prompt = if task.goal.is_empty() {
            task.title.clone()
        } else {
            format!("{}\n\n{}", task.title, task.goal)
        };
        let user_rules_prefix = self.resolve_user_rules_prompt();
        let safeguard_prefix = self.resolve_safeguard_prompt();

        let Some(ref ctx_runtime) = self.context_runtime else {
            return (
                prepend_session_instructions(
                    user_rules_prefix.as_deref(),
                    safeguard_prefix.as_deref(),
                    &base_prompt,
                ),
                None,
            );
        };

        let ws_id = workspace_id
            .clone()
            .unwrap_or_else(|| WorkspaceId::new("default"));
        let result = ctx_runtime.assemble_execution_context(&ExecutionContextAssemblyRequest {
            session_id: session_id.clone(),
            workspace_id: ws_id,
            project_key: None,
            clues: ExecutionContextClues {
                mission: Some(task.title.clone()),
                assignment: None,
                task: Some(task.goal.clone()),
            },
            budget: ContextBudget {
                max_turns: 3,
                max_knowledge: 3,
                max_memory: 2,
                max_shared_items: 1,
                max_file_summaries: 2,
            },
        });
        let has_context = !result.selected_knowledge.is_empty()
            || !result.selected_memory.is_empty()
            || !result.selected_shared_context.is_empty();

        let context_summary = ExecutionContextSummary::from_context_assembly(&result);

        if !has_context {
            return (
                prepend_session_instructions(
                    user_rules_prefix.as_deref(),
                    safeguard_prefix.as_deref(),
                    &base_prompt,
                ),
                Some(context_summary),
            );
        }
        let mut ctx_parts: Vec<String> = Vec::new();
        for item in &result.selected_knowledge {
            ctx_parts.push(format!("[knowledge] {}: {}", item.title, item.excerpt));
        }
        for item in &result.selected_memory {
            ctx_parts.push(format!("[memory] {}", item.content));
        }
        for item in &result.selected_shared_context {
            ctx_parts.push(format!("[context] {}: {}", item.title, item.content));
        }
        let ctx_text = ctx_parts.join("\n");
        (
            prepend_session_instructions(
                user_rules_prefix.as_deref(),
                safeguard_prefix.as_deref(),
                &format!("--- Context ---\n{ctx_text}\n--- Task ---\n{base_prompt}"),
            ),
            Some(context_summary),
        )
    }

    fn resolve_user_rules_prompt(&self) -> Option<String> {
        let store = self.settings_store.as_ref()?;
        let raw = store.get_section("userRules");
        match raw {
            serde_json::Value::String(value) => {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            serde_json::Value::Object(map) => {
                let candidate = map
                    .get("userRules")
                    .and_then(|value| value.as_str())
                    .or_else(|| map.get("content").and_then(|value| value.as_str()))
                    .or_else(|| map.get("prompt").and_then(|value| value.as_str()))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                (!candidate.is_empty()).then_some(candidate)
            }
            _ => None,
        }
    }

    fn resolve_safeguard_prompt(&self) -> Option<String> {
        let store = self.settings_store.as_ref()?;
        let raw = store.get_section("safeguardConfig");
        let rules = raw
            .get("rules")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let patterns = rules
            .iter()
            .filter(|rule| {
                rule.get("enabled")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true)
            })
            .filter_map(|rule| rule.get("pattern").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .collect::<Vec<_>>();
        if patterns.is_empty() {
            return None;
        }
        Some(format!(
            "执行 shell / git / 文件写操作前，如果命中以下危险模式，必须先向用户确认，不得直接执行：\n{}",
            patterns
                .iter()
                .map(|pattern| format!("- {}", pattern))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    fn execute_tool_call(
        &self,
        tool_call: &ChatToolCall,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> String {
        let Some(ref registry) = self.tool_registry else {
            return serde_json::json!({ "error": "tool registry not available" }).to_string();
        };

        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-task-tool-invoked-{}", UtcMillis::now().0)),
                "task.tool.invoked",
                serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "mission_id": task.mission_id.to_string(),
                    "session_id": session_id.to_string(),
                    "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                    "tool_name": tool_call.function.name,
                    "tool_call_id": tool_call.id,
                }),
            )
            .with_context(EventContext {
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
        };

        if tool_call.function.name == SKILL_APPLY_TOOL_NAME {
            let (payload, _) = self.execute_skill_apply_tool_call(&tool_call.function.arguments);
            return payload;
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
            context,
            &ToolExecutionPolicy::default(),
        );

        output.payload
    }

    fn execute_skill_apply_tool_call(&self, arguments: &str) -> (String, ExecutionResultStatus) {
        execute_skill_apply_from_runtime(arguments, self.skill_runtime.as_deref())
    }

    fn resolve_model_client(&self) -> Option<Arc<dyn ModelBridgeClient>> {
        resolve_configured_model_client(
            self.settings_store.as_ref(),
            self.model_bridge_client.clone(),
        )
    }

    fn usage_model_config_for_binding(&self, binding: &ModelUsageBinding) -> Option<LlmConfig> {
        let store = self.settings_store.as_ref()?;
        if matches!(binding.role, UsageSourceRole::Worker) {
            let workers = store.get_section("workers");
            if let Some(config) = workers
                .get(&binding.engine_id)
                .or_else(|| workers.get(&binding.template_id))
                .and_then(usage_model_config_from_settings)
            {
                return Some(config);
            }
        }
        usage_model_config_from_settings(&store.get_section("orchestrator"))
    }

    fn current_turn_id(&self, session_id: &SessionId) -> Option<String> {
        self.session_store
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .map(|turn| turn.turn_id)
    }

    fn publish_model_usage_record(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        binding: &ModelUsageBinding,
        call_id: String,
        usage: Option<&serde_json::Value>,
        status: UsageCallStatus,
        assignment_id: Option<String>,
        error_code: Option<String>,
    ) {
        let Some(usage) = usage_tokens_from_payload(usage) else {
            return;
        };
        let Some(model_config) = self.usage_model_config_for_binding(binding) else {
            tracing::warn!(
                template_id = binding.template_id,
                engine_id = binding.engine_id,
                "模型调用已返回用量，但缺少可审计的模型配置，跳过统计记录"
            );
            return;
        };
        let workspace_id_value = workspace_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "default-workspace".to_string());
        let input = UsageCallRecordInput {
            workspace_id: workspace_id_value.clone(),
            session_id: session_id.to_string(),
            turn_id: self.current_turn_id(session_id),
            dispatch_wave_id: None,
            assignment_id,
            event_id: Some(format!(
                "model-usage:{}:{}:{}",
                workspace_id_value, session_id, call_id
            )),
            timestamp: Some(UtcMillis::now().0),
            execution_binding: ExecutionBindingIdentity {
                template_id: binding.template_id.clone(),
                engine_id: binding.engine_id.clone(),
                binding_revision: binding.binding_revision,
                role: binding.role,
            },
            model_config,
            call_identity: UsageCallIdentity {
                call_id,
                parent_call_id: None,
                source: binding.role,
                phase: binding.phase,
            },
            usage,
            status,
            error_code,
        };
        let payload = match serde_json::to_value(&input) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(?error, "序列化模型用量记录失败");
                return;
            }
        };
        let _ = self.event_bus.publish(
            EventEnvelope::usage(
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
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                assignment_id: input
                    .assignment_id
                    .clone()
                    .map(magi_core::AssignmentId::new),
                ..EventContext::default()
            }),
        );
    }

    fn session_turn_item(
        kind: &str,
        status: &str,
        title: Option<String>,
        content: Option<String>,
        item_id: Option<String>,
    ) -> ActiveExecutionTurnItem {
        ActiveExecutionTurnItem {
            item_id: item_id
                .unwrap_or_else(|| format!("turn-item-{}-{}", kind, UtcMillis::now().0)),
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

    fn append_session_turn_item(&self, session_id: &SessionId, item: ActiveExecutionTurnItem) {
        let _ = self
            .session_store
            .append_current_turn_item(session_id, item);
    }

    fn upsert_session_turn_item(&self, session_id: &SessionId, item: ActiveExecutionTurnItem) {
        let _ = self
            .session_store
            .upsert_current_turn_item(session_id, item);
    }

    fn publish_session_turn_item_event(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        item: &ActiveExecutionTurnItem,
    ) {
        let _ = self.event_bus.publish(
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

    fn append_session_tool_call_items(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        tool_call: &ChatToolCall,
        messages: &mut Vec<ChatMessage>,
    ) {
        let mut started_item = Self::session_turn_item(
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
        self.append_session_turn_item(session_id, started_item.clone());
        self.publish_session_turn_item_event(session_id, workspace_id, &started_item);

        let (tool_result, tool_status) =
            self.execute_session_turn_tool_call(tool_call, session_id, workspace_id);
        let status_label = tool_execution_status_label(tool_status);
        let mut result_item = Self::session_turn_item(
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
        self.append_session_turn_item(session_id, result_item.clone());
        self.publish_session_turn_item_event(session_id, workspace_id, &result_item);

        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(tool_result),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call.id.clone()),
        });
    }

    fn execute_session_turn_tool_call(
        &self,
        tool_call: &ChatToolCall,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> (String, ExecutionResultStatus) {
        let Some(ref registry) = self.tool_registry else {
            return (
                serde_json::json!({ "error": "tool registry not available" }).to_string(),
                ExecutionResultStatus::Failed,
            );
        };

        let _ = self.event_bus.publish(
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
            return self.execute_skill_apply_tool_call(&tool_call.function.arguments);
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

    pub fn execute_session_turn(
        &self,
        request: SessionTurnExecutionRequest,
    ) -> Result<SessionTurnExecutionOutput, ApiError> {
        let Some(client) = self.resolve_model_client() else {
            return Err(ApiError::internal_assembly(
                "执行 session turn 失败",
                "model bridge client 未配置",
            ));
        };

        let mut prompt = prepend_session_instructions(
            self.resolve_user_rules_prompt().as_deref(),
            self.resolve_safeguard_prompt().as_deref(),
            &request.prompt,
        );
        if let Some(skill_id) = request.skill_name.clone()
            && let Some(ref skill_rt) = self.skill_runtime
        {
            let plan = skill_rt.build_tool_runtime_plan(magi_skill_runtime::SkillSelection {
                skill_ids: vec![skill_id],
                requested_tools: vec![],
            });
            for injection in plan.prompt_injections {
                prompt = format!("{}\n\n{}", injection.body, prompt);
            }
        }

        let phase_item = Self::session_turn_item(
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
        self.append_session_turn_item(&request.session_id, phase_item.clone());
        self.publish_session_turn_item_event(
            &request.session_id,
            &request.workspace_id,
            &phase_item,
        );

        let tools = if request.use_tools {
            let tool_defs = self.build_tool_definitions(false);
            (!tool_defs.is_empty()).then_some(tool_defs)
        } else {
            None
        };
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(prompt.clone()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];
        let mut final_content: Option<String> = None;
        let usage_binding = ModelUsageBinding {
            template_id: "orchestrator".to_string(),
            engine_id: "orchestrator".to_string(),
            binding_revision: 0,
            role: UsageSourceRole::Orchestrator,
            phase: if request.use_tools {
                UsagePhase::Execution
            } else {
                UsagePhase::Planning
            },
        };

        for round in 0..MAX_TOOL_CALL_ROUNDS {
            let stream_item_id = format!(
                "turn-item-assistant-stream-{}-{}",
                UtcMillis::now().0,
                round
            );
            let streamed_content = std::cell::RefCell::new(String::new());
            let last_len = std::cell::Cell::new(0usize);
            let on_delta = |accumulated: &str| {
                let previous = last_len.get();
                if accumulated.len() == previous {
                    return;
                }
                last_len.set(accumulated.len());
                {
                    let mut content = streamed_content.borrow_mut();
                    content.clear();
                    content.push_str(accumulated);
                }
                let item = Self::session_turn_item(
                    "assistant_stream",
                    "running",
                    Some("生成回复".to_string()),
                    Some(accumulated.to_string()),
                    Some(stream_item_id.clone()),
                );
                self.upsert_session_turn_item(&request.session_id, item.clone());
                self.publish_session_turn_item_event(
                    &request.session_id,
                    &request.workspace_id,
                    &item,
                );
            };

            let response = client
                .invoke_streaming(
                    ModelInvocationRequest {
                        provider: BUSINESS_MODEL_PROVIDER.to_string(),
                        prompt: prompt.clone(),
                        messages: Some(messages.clone()),
                        tools: tools.clone(),
                        tool_choice: None,
                    },
                    &on_delta,
                )
                .map_err(|error| ApiError::internal_assembly("执行 session turn 失败", error))?;
            let parsed = response.parse_chat_payload();
            self.publish_model_usage_record(
                &request.session_id,
                &request.workspace_id,
                &usage_binding,
                format!("session-turn-{round}-{}", UtcMillis::now().0),
                parsed.usage.as_ref(),
                UsageCallStatus::Success,
                None,
                None,
            );
            let streamed_content = streamed_content.into_inner();
            if !streamed_content.trim().is_empty() {
                let stream_item = Self::session_turn_item(
                    "assistant_stream",
                    "completed",
                    Some("生成回复".to_string()),
                    Some(streamed_content.clone()),
                    Some(stream_item_id),
                );
                self.upsert_session_turn_item(&request.session_id, stream_item.clone());
                self.publish_session_turn_item_event(
                    &request.session_id,
                    &request.workspace_id,
                    &stream_item,
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
                    self.append_session_tool_call_items(
                        &request.session_id,
                        &request.workspace_id,
                        &tool_call,
                        &mut messages,
                    );
                }
                continue;
            }

            final_content = parsed
                .content
                .filter(|content| !content.trim().is_empty())
                .or_else(|| (!streamed_content.trim().is_empty()).then_some(streamed_content))
                .map(normalize_model_visible_content)
                .or_else(|| (!request.use_tools).then(|| "[LLM 未返回文本响应]".to_string()));
            break;
        }

        let final_content = final_content.ok_or_else(|| {
            ApiError::internal_assembly("执行 session turn 失败", "模型未在工具调用后返回最终回复")
        })?;
        let final_item = Self::session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some(final_content.clone()),
            None,
        );
        self.append_session_turn_item(&request.session_id, final_item.clone());
        self.publish_session_turn_item_event(
            &request.session_id,
            &request.workspace_id,
            &final_item,
        );
        let _ = self
            .session_store
            .update_current_turn_status(&request.session_id, "completed");

        Ok(SessionTurnExecutionOutput { final_content })
    }

    fn invoke_llm_with_tools(
        &self,
        task: &magi_core::Task,
        task_id: &TaskId,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        use_tools: bool,
        skill_name: Option<String>,
        usage_binding: &ModelUsageBinding,
        streaming_entry_id: Option<&str>,
    ) -> (TaskOutcome, Option<ExecutionContextSummary>) {
        let Some(client) = self.resolve_model_client() else {
            tracing::error!(task_id = %task.task_id, "invoke_llm_with_tools: no model bridge client configured");
            return (
                TaskOutcome::Failed {
                    error: format!(
                        "no model bridge client configured for task {}",
                        task.task_id
                    ),
                },
                None,
            );
        };

        let (mut prompt, context_summary) = self.assemble_prompt(task, session_id, workspace_id);

        if let Some(skill_id) = skill_name {
            if let Some(ref skill_rt) = self.skill_runtime {
                let plan = skill_rt.build_tool_runtime_plan(magi_skill_runtime::SkillSelection {
                    skill_ids: vec![skill_id],
                    requested_tools: vec![],
                });
                for injection in plan.prompt_injections {
                    prompt = format!("{}\n\n{}", injection.body, prompt);
                }
            }
        }

        let tools = if use_tools {
            let tool_defs = self.build_tool_definitions(false);
            if tool_defs.is_empty() {
                None
            } else {
                Some(tool_defs)
            }
        } else {
            None
        };

        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(prompt.clone()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];

        let task_context = EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        };

        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-task-llm-started-{}", UtcMillis::now().0)),
                "task.llm.started",
                serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "mission_id": task.mission_id.to_string(),
                    "session_id": session_id.to_string(),
                    "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                    "prompt_length": prompt.len(),
                }),
            )
            .with_context(task_context.clone()),
        );

        let mut final_content = String::new();
        let mut tool_call_records: Vec<serde_json::Value> = Vec::new();

        for round in 0..MAX_TOOL_CALL_ROUNDS {
            let request = ModelInvocationRequest {
                provider: SHADOW_MODEL_PROVIDER.to_string(),
                prompt: prompt.clone(),
                messages: Some(messages.clone()),
                tools: tools.clone(),
                tool_choice: None,
            };

            // 仅当有 streaming_entry_id 时才创建流式 timeline entry 并使用流式调用
            let response = if let Some(entry_id) = streaming_entry_id {
                let session_store_ref = &self.session_store;
                let event_bus_ref = &self.event_bus;
                let session_id_ref = session_id;
                let task_context_ref = &task_context;
                let task_id_str = task.task_id.to_string();
                let mission_id_str = task.mission_id.to_string();
                // 跟踪上次已发送的累积文本长度，用于计算增量
                let last_sent_len = std::cell::Cell::new(0usize);

                let on_delta = |accumulated_text: &str| {
                    session_store_ref.upsert_timeline_entry(
                        session_id_ref.clone(),
                        entry_id,
                        magi_session_store::TimelineEntryKind::AssistantMessage,
                        accumulated_text,
                    );

                    // 计算增量：只发送自上次以来新增的文本片段
                    let prev_len = last_sent_len.get();
                    let delta = &accumulated_text[prev_len..];
                    if delta.is_empty() {
                        return;
                    }
                    last_sent_len.set(accumulated_text.len());

                    let _ = event_bus_ref.publish(
                        EventEnvelope::domain(
                            EventId::new(format!("event-task-llm-delta-{}", UtcMillis::now().0)),
                            "task.llm.delta",
                            serde_json::json!({
                                "task_id": task_id_str,
                                "mission_id": mission_id_str,
                                "session_id": session_id_ref.to_string(),
                                "entry_id": entry_id,
                                "delta": delta,
                            }),
                        )
                        .with_context(task_context_ref.clone()),
                    );
                };

                match client.invoke_streaming(request, &on_delta) {
                    Ok(resp) => resp,
                    Err(error) => {
                        tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
                        return (
                            TaskOutcome::Failed {
                                error: format!("LLM invocation failed (round {round}): {error:?}"),
                            },
                            context_summary,
                        );
                    }
                }
            } else {
                // 非流式调用（无需 timeline 更新）
                match client.invoke(request) {
                    Ok(resp) => resp,
                    Err(error) => {
                        tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM invocation failed");
                        return (
                            TaskOutcome::Failed {
                                error: format!("LLM invocation failed (round {round}): {error:?}"),
                            },
                            context_summary,
                        );
                    }
                }
            };

            let parsed = response.parse_chat_payload();
            self.publish_model_usage_record(
                session_id,
                workspace_id,
                usage_binding,
                format!("task-{}-{}-{round}", task_id, lease_id),
                parsed.usage.as_ref(),
                UsageCallStatus::Success,
                Some(lease_id.to_string()),
                None,
            );

            if let Some(ref content) = parsed.content {
                final_content = content.clone();
            }

            if parsed.tool_calls.is_empty() {
                let _ = self.event_bus.publish(
                    EventEnvelope::domain(
                        EventId::new(format!("event-task-llm-completed-{}", UtcMillis::now().0)),
                        "task.llm.completed",
                        serde_json::json!({
                            "task_id": task.task_id.to_string(),
                            "mission_id": task.mission_id.to_string(),
                            "session_id": session_id.to_string(),
                            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                            "entry_id": streaming_entry_id,
                            "response_length": final_content.len(),
                            "rounds": round + 1,
                        }),
                    )
                    .with_context(task_context.clone()),
                );
                break;
            }

            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: parsed.content.clone(),
                tool_calls: parsed.tool_calls.clone(),
                tool_call_id: None,
            });

            for tc in &parsed.tool_calls {
                let result = self.execute_tool_call(tc, task, session_id, workspace_id);
                let status = infer_tool_call_status(&result);
                tool_call_records.push(serde_json::json!({
                    "type": "tool_call",
                    "content": format!("{}: {}", tc.function.name, summarize_tool_result(&result)),
                    "toolCall": {
                        "id": tc.id,
                        "name": tc.function.name,
                        "arguments": serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::String(tc.function.arguments.clone())),
                        "status": status,
                        "result": result,
                    }
                }));
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(result),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tc.id.clone()),
                });
            }
        }

        if final_content.is_empty() {
            final_content = "[LLM 未返回文本响应]".to_string();
        }
        final_content = normalize_model_visible_content(final_content);
        if !self.task_lease_is_current(task_id, lease_id) {
            return (
                TaskOutcome::Failed {
                    error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
                },
                context_summary,
            );
        }
        if streaming_entry_id.is_some()
            || self.task_is_thread_visible_turn_owner(session_id, task_id)
        {
            let mut final_item = Self::session_turn_item(
                "assistant_final",
                "completed",
                Some("最终回复".to_string()),
                Some(final_content.clone()),
                None,
            );
            final_item.task_id = Some(task.task_id.clone());
            self.append_session_turn_item(session_id, final_item.clone());
            self.publish_session_turn_item_event(session_id, workspace_id, &final_item);
            let _ = self
                .session_store
                .update_current_turn_status(session_id, "completed");
        }

        let output_content = if tool_call_records.is_empty() {
            final_content
        } else {
            let mut blocks = tool_call_records;
            blocks.push(serde_json::json!({
                "type": "text",
                "content": final_content,
            }));
            serde_json::json!({ "blocks": blocks }).to_string()
        };

        (
            TaskOutcome::Completed {
                output_refs: vec![output_content],
            },
            context_summary,
        )
    }

    fn task_lease_is_current(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        self.pipeline
            .execution_runtime
            .task_store()
            .get_active_lease(task_id)
            .is_some_and(|lease| lease.lease_id == *lease_id)
    }

    fn task_is_thread_visible_turn_owner(&self, session_id: &SessionId, task_id: &TaskId) -> bool {
        self.session_store
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .is_some_and(|turn| {
                turn.items.iter().any(|item| {
                    item.task_id.as_ref() == Some(task_id)
                        && item.thread_visible
                        && item.kind == "assistant_phase"
                })
            })
    }
}

struct LearningCandidate {
    content: String,
    context: Option<String>,
    tags: Vec<String>,
}

fn extract_learning_candidates(text: &str) -> Vec<LearningCandidate> {
    let markers = [
        "经验",
        "教训",
        "结论",
        "注意",
        "建议",
        "最佳实践",
        "踩坑",
        "坑点",
        "要点",
        "important",
        "note",
        "lesson",
        "tip",
        "best practice",
    ];
    let mut candidates = Vec::new();
    for raw in text.lines() {
        let line = raw
            .trim()
            .trim_start_matches(['-', '*', '•', '1', '2', '3', '4', '5', '.', ' '])
            .trim();
        if line.chars().count() < 12 || line.chars().count() > 600 {
            continue;
        }
        let lower = line.to_lowercase();
        if !markers
            .iter()
            .any(|marker| lower.contains(&marker.to_lowercase()))
        {
            continue;
        }
        if candidates.iter().any(|candidate: &LearningCandidate| {
            normalized_text(&candidate.content) == normalized_text(line)
        }) {
            continue;
        }
        candidates.push(LearningCandidate {
            content: line.to_string(),
            context: None,
            tags: vec!["auto".to_string(), "learning".to_string()],
        });
        if candidates.len() >= 5 {
            break;
        }
    }
    candidates
}

fn normalized_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn knowledge_duplicate(existing: &[KnowledgeRecord], kind: KnowledgeKind, content: &str) -> bool {
    let normalized = normalized_text(content);
    existing.iter().any(|record| {
        record.kind == kind && {
            let record_text = normalized_text(&record.content);
            record_text == normalized
                || record_text.contains(&normalized)
                || normalized.contains(&record_text)
        }
    })
}

fn title_from_learning_content(content: &str) -> String {
    let mut title = content.chars().take(80).collect::<String>();
    if content.chars().count() > 80 {
        title.push('…');
    }
    title
}

fn prepend_session_instructions(
    user_rules: Option<&str>,
    safeguard_rules: Option<&str>,
    prompt: &str,
) -> String {
    let mut sections = Vec::new();
    if let Some(rules) = user_rules.map(str::trim).filter(|rules| !rules.is_empty()) {
        sections.push(format!("--- 用户规则 ---\n{rules}"));
    }
    if let Some(rules) = safeguard_rules
        .map(str::trim)
        .filter(|rules| !rules.is_empty())
    {
        sections.push(format!("--- 安全防护 ---\n{rules}"));
    }
    if sections.is_empty() {
        return prompt.to_string();
    }
    format!("{}\n\n{}", sections.join("\n\n"), prompt)
}

fn normalize_model_visible_content(content: String) -> String {
    content
        .strip_prefix("shadow-model::")
        .unwrap_or(content.as_str())
        .trim()
        .to_string()
}

fn tool_execution_status_label(status: ExecutionResultStatus) -> &'static str {
    match status {
        ExecutionResultStatus::Succeeded => "succeeded",
        ExecutionResultStatus::Failed => "failed",
        ExecutionResultStatus::Rejected => "rejected",
        ExecutionResultStatus::NeedsApproval => "needs_approval",
        ExecutionResultStatus::Cancelled => "cancelled",
    }
}

fn turn_item_status_for_tool_result(status: ExecutionResultStatus) -> &'static str {
    match status {
        ExecutionResultStatus::Succeeded => "completed",
        ExecutionResultStatus::NeedsApproval => "blocked",
        ExecutionResultStatus::Failed
        | ExecutionResultStatus::Rejected
        | ExecutionResultStatus::Cancelled => "failed",
    }
}

fn model_usage_binding_for_worker(worker: &WorkerInfo, is_primary: bool) -> ModelUsageBinding {
    if is_primary {
        return ModelUsageBinding {
            template_id: "orchestrator".to_string(),
            engine_id: "orchestrator".to_string(),
            binding_revision: 0,
            role: UsageSourceRole::Orchestrator,
            phase: UsagePhase::Planning,
        };
    }
    ModelUsageBinding {
        template_id: worker.role.clone(),
        engine_id: worker.worker_id.to_string(),
        binding_revision: 0,
        role: UsageSourceRole::Worker,
        phase: UsagePhase::Execution,
    }
}

impl TaskDispatcher for ShadowTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
    ) -> Result<(), String> {
        let Some(plan) = self.execution_registry.remove(&task.task_id) else {
            let session_id = self
                .session_store
                .current_session()
                .map(|s| s.session_id)
                .unwrap_or_else(|| SessionId::new("default"));
            self.publish_task_dispatched_event(
                &task.task_id,
                &task.mission_id,
                worker,
                &lease.lease_id,
                task.kind,
                Some(&session_id),
                None,
            );
            let (outcome, _) = self.invoke_llm_with_tools(
                task,
                &task.task_id,
                &lease.lease_id,
                &session_id,
                &None,
                false,
                None,
                &model_usage_binding_for_worker(worker, false),
                None,
            );
            self.push_result(&task.task_id, &lease.lease_id, outcome);
            return Ok(());
        };

        match plan {
            ShadowTaskExecutionPlan::Dispatch {
                target: _,
                worker_id: _,
                lane_id: _,
                lane_seq: _,
                is_primary,
                session_id,
                workspace_id,
                ownership,
                writebacks,
                use_tools,
                skill_name,
            } => {
                self.publish_task_dispatched_event(
                    &task.task_id,
                    &task.mission_id,
                    worker,
                    &lease.lease_id,
                    task.kind,
                    Some(&session_id),
                    workspace_id.as_ref(),
                );
                self.execute_dispatch_plan(
                    task,
                    &task.task_id,
                    &lease.lease_id,
                    session_id,
                    workspace_id,
                    ownership,
                    writebacks,
                    use_tools,
                    skill_name,
                    model_usage_binding_for_worker(worker, is_primary),
                );
            }
        }

        Ok(())
    }
}

fn submit_shadow_task_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    let graph = run_shadow_dispatch_submission(state, &request)?;
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        state
            .session_store
            .upsert_active_execution_chain(request.session_id.clone(), active_execution_chain)
            .map_err(|error| ApiError::internal_assembly("执行 shadow dispatch 失败", error))?;
    }

    Ok(DispatchSubmissionAccepted {
        session_id: request.session_id,
        entry_id: request.entry_id,
        accepted_at: request.accepted_at,
        created_session: request.created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
        runner_started: false,
    })
}

pub fn submit_shadow_dispatch_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    submit_shadow_task_submission(state, request)
}

pub fn drive_shadow_dispatch_submission(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let execution = drive_shadow_task_graph(
        state,
        &accepted.root_task_id,
        &accepted.action_task_id,
        "执行 shadow dispatch 失败",
    )?;
    accepted.runner_started = execution.runner_started;
    Ok(())
}

pub fn drive_shadow_task_graph(
    state: &ApiState,
    root_task_id: &TaskId,
    action_task_id: &TaskId,
    failure_title: &'static str,
) -> Result<ShadowGraphDriveResult, ApiError> {
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly(failure_title, "runner_manager 未配置"))?;
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly(failure_title, "task_store 未配置"))?;

    let mut executed = false;
    for _ in 0..8 {
        executed = true;
        let outcome = manager
            .run_single_cycle(root_task_id.as_str())
            .map_err(|error| ApiError::internal_assembly(failure_title, error))?;
        match outcome {
            magi_orchestrator::task_runner::RunCycleOutcome::Continue => continue,
            magi_orchestrator::task_runner::RunCycleOutcome::AllComplete => break,
            magi_orchestrator::task_runner::RunCycleOutcome::Blocked(task_ids) => {
                if task_store
                    .get_task(action_task_id)
                    .is_some_and(|task| task.status == TaskStatus::Blocked)
                {
                    break;
                }
                return Err(ApiError::internal_assembly(
                    failure_title,
                    format!("task runner blocked: {:?}", task_ids),
                ));
            }
            magi_orchestrator::task_runner::RunCycleOutcome::Error(error) => {
                return Err(ApiError::internal_assembly(failure_title, error));
            }
        }
    }

    let action_status = task_store
        .get_task(action_task_id)
        .ok_or_else(|| ApiError::internal_assembly(failure_title, "action task 不存在"))?
        .status;
    if action_status != TaskStatus::Completed
        && action_status != TaskStatus::Failed
        && action_status != TaskStatus::Blocked
    {
        return Err(ApiError::internal_assembly(
            failure_title,
            format!(
                "task runner did not complete action task: {:?}",
                action_status
            ),
        ));
    }

    Ok(ShadowGraphDriveResult {
        runner_started: executed,
    })
}

fn chat_tool_description(name: &str) -> String {
    match name {
        "file_read" => "Read the contents of a file at a given path".to_string(),
        "file_write" => "Create or overwrite a file with the given content".to_string(),
        "file_patch" => "Apply targeted text replacements to a file (find-and-replace)".to_string(),
        "file_remove" => "Delete a file or directory".to_string(),
        "file_mkdir" => "Create a directory (including parent directories)".to_string(),
        "file_copy" => "Copy a file or directory to a new location".to_string(),
        "file_move" => "Move or rename a file or directory".to_string(),
        "search_text" => "Search for text patterns in files within a directory".to_string(),
        "search_semantic" => {
            "Semantic code search: find code by natural language description".to_string()
        }
        "shell_exec" => "Execute a shell command and return stdout/stderr".to_string(),
        "process_inspect" => "Inspect running processes by PID or name".to_string(),
        "diff_preview" => "Generate a unified diff between two text inputs".to_string(),
        "web_search" => "Search the web using DuckDuckGo and return results".to_string(),
        "web_fetch" => "Fetch content from a URL and convert HTML to markdown".to_string(),
        "mermaid_diagram" => "Generate a Mermaid diagram from code".to_string(),
        "knowledge_query" => {
            "Query project knowledge base: search README, docs, and code documentation".to_string()
        }
        "skill_apply" => "Load and apply a named skill for specialized task execution".to_string(),
        _ => format!("Builtin tool: {name}"),
    }
}

fn skill_apply_tool_definition() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: SKILL_APPLY_TOOL_NAME.to_string(),
            description: chat_tool_description(SKILL_APPLY_TOOL_NAME),
            parameters: chat_tool_parameters(SKILL_APPLY_TOOL_NAME),
        },
    }
}

fn skill_apply_failed(
    error: impl Into<String>,
    skill_name: Option<&str>,
    available_skills: Vec<String>,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": SKILL_APPLY_TOOL_NAME,
            "status": "failed",
            "error": error.into(),
            "skill_name": skill_name,
            "available_skills": available_skills,
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

fn execute_skill_apply_from_runtime(
    arguments: &str,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
) -> (String, ExecutionResultStatus) {
    let Some(skill_runtime) = skill_runtime else {
        return skill_apply_failed("SkillRuntime 未配置，无法应用 skill", None, Vec::new());
    };
    let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) => value,
        Err(error) => {
            return skill_apply_failed(
                format!("skill_apply 参数不是合法 JSON: {error}"),
                None,
                Vec::new(),
            );
        }
    };
    let skill_name = match parsed
        .get("skill_name")
        .or_else(|| parsed.get("name"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => {
            return skill_apply_failed("缺少 skill_name 字段", None, Vec::new());
        }
    };
    let context = parsed
        .get("context")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let registry = skill_runtime.registry();
    let available_skills = registry
        .list()
        .into_iter()
        .map(|skill| skill.skill_id)
        .collect::<Vec<_>>();
    let Some(skill) = registry.get(skill_name) else {
        return skill_apply_failed(
            format!("未找到已注册 skill: {skill_name}"),
            Some(skill_name),
            available_skills,
        );
    };
    let skill_id = skill.skill_id.clone();
    let title = skill.title.clone();
    (
        serde_json::json!({
            "tool": SKILL_APPLY_TOOL_NAME,
            "status": "succeeded",
            "skill_name": skill.skill_id,
            "title": skill.title,
            "instruction": skill.instruction,
            "allowed_tools": skill.allowed_tools,
            "metadata": {
                "category": skill.metadata.category,
                "tags": skill.metadata.tags,
            },
            "context": context,
            "summary": format!("已加载已注册 skill: {skill_id} ({title})")
        })
        .to_string(),
        ExecutionResultStatus::Succeeded,
    )
}

fn chat_tool_parameters(name: &str) -> serde_json::Value {
    match name {
        "file_read" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file to read" }
            },
            "required": ["path"]
        }),
        "file_write" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file to write" },
                "content": { "type": "string", "description": "Content to write to the file" },
                "overwrite": { "type": "boolean", "description": "Whether to overwrite existing file (default: true)" },
                "create_dirs": { "type": "boolean", "description": "Whether to create parent directories (default: true)" }
            },
            "required": ["path", "content"]
        }),
        "file_patch" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file to patch" },
                "old_string": { "type": "string", "description": "Text to find (must match exactly once)" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "patches": {
                    "type": "array",
                    "description": "Array of patches to apply (alternative to old_string/new_string)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path"]
        }),
        "file_remove" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file or directory to delete" },
                "recursive": { "type": "boolean", "description": "Whether to recursively delete directories (default: false)" }
            },
            "required": ["path"]
        }),
        "file_mkdir" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path of the directory to create" }
            },
            "required": ["path"]
        }),
        "file_copy" => serde_json::json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "Absolute path of the source file or directory" },
                "destination": { "type": "string", "description": "Absolute path of the destination" },
                "overwrite": { "type": "boolean", "description": "Whether to overwrite if destination exists (default: false)" }
            },
            "required": ["source", "destination"]
        }),
        "file_move" => serde_json::json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "Absolute path of the source file or directory" },
                "destination": { "type": "string", "description": "Absolute path of the destination" },
                "overwrite": { "type": "boolean", "description": "Whether to overwrite if destination exists (default: false)" }
            },
            "required": ["source", "destination"]
        }),
        "search_text" => serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string", "description": "Root directory to search in" },
                "query": { "type": "string", "description": "Text pattern to search for" },
                "limit": { "type": "integer", "description": "Maximum number of results" }
            },
            "required": ["root", "query"]
        }),
        "shell_exec" => serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "cwd": { "type": "string", "description": "Working directory" }
            },
            "required": ["command"]
        }),
        "process_inspect" => serde_json::json!({
            "type": "object",
            "properties": {
                "pid": { "type": "string", "description": "Process ID or name to inspect" }
            },
            "required": ["pid"]
        }),
        "diff_preview" => serde_json::json!({
            "type": "object",
            "properties": {
                "before": { "type": "string", "description": "Original text" },
                "after": { "type": "string", "description": "Modified text" }
            },
            "required": ["before", "after"]
        }),
        "web_search" => serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query keywords" }
            },
            "required": ["query"]
        }),
        "web_fetch" => serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to fetch content from" }
            },
            "required": ["url"]
        }),
        "mermaid_diagram" => serde_json::json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "Mermaid diagram code" },
                "title": { "type": "string", "description": "Optional diagram title" },
                "theme": { "type": "string", "description": "Diagram theme (default: default)" }
            },
            "required": ["code"]
        }),
        "knowledge_query" => serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Natural language query to search project documentation" },
                "category": { "type": "string", "description": "Knowledge category: all, readme, docs, code (default: all)" }
            },
            "required": ["query"]
        }),
        "search_semantic" => serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Natural language description of the code to find" },
                "root": { "type": "string", "description": "Root directory to search in" },
                "limit": { "type": "integer", "description": "Maximum number of results (default: 10)" }
            },
            "required": ["query"]
        }),
        "skill_apply" => serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": { "type": "string", "description": "Name of the skill to apply" },
                "context": { "type": "string", "description": "Additional context for the skill execution" }
            },
            "required": ["skill_name"]
        }),
        _ => serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn infer_tool_call_status(result: &str) -> &'static str {
    let parsed = serde_json::from_str::<serde_json::Value>(result).ok();
    match parsed
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
    {
        Some("error") | Some("failed") => "error",
        _ if parsed.as_ref().and_then(|v| v.get("error")).is_some() => "error",
        _ => "success",
    }
}

fn summarize_tool_result(result: &str) -> String {
    if result.len() <= 120 {
        return result.to_string();
    }
    let mut end = 120;
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &result[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry, SkillRuntime};

    fn make_skill_runtime() -> SkillRuntime {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "从产品稳定性角度检查关键缺陷。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        SkillRuntime::new(registry)
    }

    #[test]
    fn skill_apply_uses_registered_skill_runtime() {
        let runtime = make_skill_runtime();
        let (payload, status) = execute_skill_apply_from_runtime(
            &serde_json::json!({
                "skill_name": "code-review",
                "context": "检查主链路"
            })
            .to_string(),
            Some(&runtime),
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], SKILL_APPLY_TOOL_NAME);
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["skill_name"], "code-review");
        assert_eq!(parsed["title"], "代码审查");
        assert_eq!(parsed["context"], "检查主链路");
        assert!(
            parsed["instruction"]
                .as_str()
                .unwrap()
                .contains("产品稳定性")
        );
    }

    #[test]
    fn skill_apply_reports_missing_registered_skill_without_filesystem_scan_fields() {
        let runtime = SkillRuntime::new(SkillRegistry::new());
        let (payload, status) = execute_skill_apply_from_runtime(
            &serde_json::json!({ "skill_name": "auto-review" }).to_string(),
            Some(&runtime),
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], SKILL_APPLY_TOOL_NAME);
        assert_eq!(parsed["status"], "failed");
        assert_eq!(parsed["skill_name"], "auto-review");
        assert!(parsed["error"].as_str().unwrap().contains("auto-review"));
        assert!(parsed.get("search_paths").is_none());
        assert!(parsed.get("hint").is_none());
    }
}
