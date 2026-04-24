use crate::{
    errors::ApiError,
    shadow_execution::run_shadow_dispatch_submission,
    state::{ApiState, ShadowExecutionPipeline, build_mcp_config_from_entry},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition,
    HttpModelBridgeClient, McpBridgeClient, McpToolCallRequest, McpToolInfo, ModelBridgeClient,
    ModelInvocationRequest, ModelStreamEvent, SHADOW_MODEL_PROVIDER, StdioMcpBridgeClient,
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionOwnership, LeaseId, RecoveryResumeInput, RiskLevel,
    SessionId, TaskExecutionTarget, TaskId, TaskStatus, ToolCallId, UtcMillis, WorkerId,
    WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::{
    ExecutionContextSummary, ExecutionWritebackPlans,
    task_runner::{EventBasedResultReceiver, TaskDispatcher, TaskOutcome, TaskResult, WorkerInfo},
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionTurnItem, SessionStore,
};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use magi_usage_authority::{
    UsageAuthority, UsageCallStatus, UsageSourceRole, UsageTokenInput,
    build_execution_binding_identity, build_usage_call_identity,
    types::{
        LlmConfig as UsageLlmConfig, OpenAiProtocol as UsageOpenAiProtocol,
        ReasoningEffort as UsageReasoningEffort, UrlMode as UsageUrlMode,
    },
};
use magi_worker_runtime::WorkerStage;
use magi_workspace::RecoveryStatus;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

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

fn validate_continue_checkpoint_ready(
    state: &ApiState,
    checkpoint_id: &str,
) -> Result<(), ApiError> {
    let export = state
        .workspace_registry
        .recovery_sidecar_export(checkpoint_id)
        .ok_or_else(|| ApiError::recovery_not_found(checkpoint_id))?;
    match export.current_status {
        RecoveryStatus::Ready => Ok(()),
        RecoveryStatus::Prepared => Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 当前状态为 prepared，必须先进入 ready 才能继续会话",
            checkpoint_id
        ))),
        RecoveryStatus::Consumed => Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 已被消费，不能再次继续会话",
            checkpoint_id
        ))),
    }
}

fn map_continue_checkpoint_input_error(
    checkpoint_id: &str,
    error: magi_core::DomainError,
) -> ApiError {
    match error {
        magi_core::DomainError::NotFound { .. } => ApiError::recovery_not_found(checkpoint_id),
        magi_core::DomainError::InvalidState { message }
        | magi_core::DomainError::Validation { message } => ApiError::InvalidInput(message),
        magi_core::DomainError::AlreadyExists { entity } => ApiError::internal_assembly(
            "继续会话失败",
            format!("继续检查点输入构建遇到重复实体: {entity}"),
        ),
    }
}

fn validate_continue_checkpoint_matches_chain(
    chain: &ActiveExecutionChain,
    input: &RecoveryResumeInput,
) -> Result<(), ApiError> {
    if input.ownership.session_id.as_ref() != Some(&chain.session_id) {
        return Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 不属于当前会话 {}",
            input.recovery_id, chain.session_id
        )));
    }
    if input.ownership.mission_id.as_ref() != Some(&chain.mission_id) {
        return Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 不属于当前执行链 mission {}",
            input.recovery_id, chain.mission_id
        )));
    }
    if input.ownership.workspace_id != chain.workspace_id {
        return Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 的工作区与当前执行链不一致",
            input.recovery_id
        )));
    }
    if input.ownership.execution_chain_ref.as_deref() != Some(chain.execution_chain_ref.as_str()) {
        return Err(ApiError::InvalidInput(format!(
            "继续检查点 {} 的 execution_chain_ref 与当前执行链不一致",
            input.recovery_id
        )));
    }
    Ok(())
}

fn consume_continue_checkpoint_if_needed(
    state: &ApiState,
    session_id: &SessionId,
    chain: &mut ActiveExecutionChain,
    primary_branch: &ActiveExecutionBranch,
) -> Result<(), ApiError> {
    let Some(checkpoint_id) = chain.recovery_ref.clone() else {
        return Ok(());
    };
    validate_continue_checkpoint_ready(state, &checkpoint_id)?;
    let input = state
        .workspace_registry
        .build_recovery_resume_input(&checkpoint_id)
        .map_err(|error| map_continue_checkpoint_input_error(&checkpoint_id, error))?;
    validate_continue_checkpoint_matches_chain(chain, &input)?;

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
    _requested_worker_ids: &[WorkerId],
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

    let primary_branch = resumable_branches
        .iter()
        .find(|branch| branch.is_primary)
        .or_else(|| resumable_branches.first())
        .expect("resumable_branches checked as non-empty");
    consume_continue_checkpoint_if_needed(state, session_id, &mut chain, primary_branch)?;

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
    let runner_started = match manager.start(chain.root_task_id.as_str()) {
        Ok(_handle) => true,
        Err(crate::state::RunnerStartError::AlreadyRunning) => false,
        Err(crate::state::RunnerStartError::NotFound) => {
            return Err(ApiError::not_found(
                "根任务不存在",
                chain.root_task_id.as_str(),
            ));
        }
    };

    Ok(SessionContinueAccepted {
        session_id: session_id.clone(),
        mission_id: chain.mission_id,
        root_task_id: chain.root_task_id,
        action_task_id: primary_branch.task_id.clone(),
        execution_chain_ref: chain.execution_chain_ref,
        resumed_branch_count: resumable_branches.len(),
        runner_started,
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
    settings_store: Option<Arc<crate::settings_store::SettingsStore>>,
    usage_authority: Option<Arc<Mutex<UsageAuthority>>>,
    context_runtime: Option<Arc<ContextRuntime>>,
    tool_registry: Option<ToolRegistry>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
}

const MAX_TOOL_CALL_ROUNDS: usize = 8;
const MCP_TOOL_NAME_PREFIX: &str = "mcp__";

fn resolve_session_user_rules_prompt(
    store: Option<&crate::settings_store::SettingsStore>,
    session_id: &SessionId,
) -> Option<String> {
    let raw = store?.get_session_section(session_id, "userRules");
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

fn resolve_session_safeguard_prompt(
    store: Option<&crate::settings_store::SettingsStore>,
    session_id: &SessionId,
) -> Option<String> {
    let raw = crate::state::normalize_safeguard_config_value(
        store?.get_session_section(session_id, "safeguardConfig"),
    );
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

fn usage_u64(value: Option<&serde_json::Value>) -> u64 {
    value.and_then(|value| value.as_u64()).unwrap_or(0)
}

fn usage_value_to_token_input(usage: Option<&serde_json::Value>) -> UsageTokenInput {
    let Some(usage) = usage else {
        return UsageTokenInput {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            cache_write_tokens: None,
        };
    };
    let cache_read_tokens = usage
        .get("cacheReadTokens")
        .or_else(|| usage.get("cache_read_tokens"))
        .and_then(|value| value.as_u64())
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(|value| value.as_u64())
        });
    let cache_write_tokens = usage
        .get("cacheWriteTokens")
        .or_else(|| usage.get("cache_write_tokens"))
        .and_then(|value| value.as_u64());

    UsageTokenInput {
        input_tokens: usage_u64(
            usage
                .get("inputTokens")
                .or_else(|| usage.get("input_tokens"))
                .or_else(|| usage.get("prompt_tokens")),
        ),
        output_tokens: usage_u64(
            usage
                .get("outputTokens")
                .or_else(|| usage.get("output_tokens"))
                .or_else(|| usage.get("completion_tokens")),
        ),
        cache_read_tokens,
        cache_write_tokens,
    }
}

fn resolve_usage_model_config(
    store: Option<&crate::settings_store::SettingsStore>,
) -> UsageLlmConfig {
    let config = store
        .map(|store| store.get_section("orchestrator"))
        .unwrap_or_else(|| serde_json::json!({}));
    let provider = config
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("openai")
        .to_string();
    let model = config
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("gpt-4")
        .to_string();
    let base_url = config
        .get("baseUrl")
        .or_else(|| config.get("base_url"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let url_mode = match config
        .get("urlMode")
        .or_else(|| config.get("url_mode"))
        .and_then(|value| value.as_str())
        .unwrap_or("default")
    {
        "full" => UsageUrlMode::Full,
        "proxy" => UsageUrlMode::Proxy,
        _ => UsageUrlMode::Default,
    };
    let openai_protocol = match config
        .get("openaiProtocol")
        .or_else(|| config.get("openai_protocol"))
        .and_then(|value| value.as_str())
    {
        Some("responses") => Some(UsageOpenAiProtocol::Responses),
        Some("chat") => Some(UsageOpenAiProtocol::Chat),
        _ => None,
    };
    let reasoning_effort = match config
        .get("reasoningEffort")
        .or_else(|| config.get("reasoning_effort"))
        .and_then(|value| value.as_str())
    {
        Some("low") => Some(UsageReasoningEffort::Low),
        Some("medium") => Some(UsageReasoningEffort::Medium),
        Some("high") => Some(UsageReasoningEffort::High),
        Some("xhigh") => Some(UsageReasoningEffort::Xhigh),
        _ => None,
    };
    UsageLlmConfig {
        provider,
        model,
        base_url,
        api_key: None,
        url_mode,
        openai_protocol,
        reasoning_effort,
        enable_thinking: config
            .get("enableThinking")
            .and_then(|value| value.as_bool()),
    }
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
            settings_store: None,
            usage_authority: None,
            context_runtime: None,
            tool_registry: None,
            skill_runtime: None,
        }
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_settings_store(mut self, store: Arc<crate::settings_store::SettingsStore>) -> Self {
        self.settings_store = Some(store);
        self
    }

    pub fn with_usage_authority(mut self, authority: Arc<Mutex<UsageAuthority>>) -> Self {
        self.usage_authority = Some(authority);
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

    fn record_llm_usage(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        turn_id: Option<String>,
        assignment_id: Option<String>,
        template_id: &str,
        role: UsageSourceRole,
        usage: Option<&serde_json::Value>,
        status: UsageCallStatus,
    ) {
        let Some(authority) = &self.usage_authority else {
            return;
        };
        let workspace_id = workspace_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "default-workspace".to_string());
        let timestamp = UtcMillis::now().0;
        let call_id = format!("{session_id}:{template_id}:{timestamp}");
        let input = magi_usage_authority::UsageCallRecordInput {
            workspace_id,
            session_id: session_id.to_string(),
            turn_id,
            dispatch_wave_id: None,
            assignment_id,
            event_id: None,
            timestamp: Some(timestamp),
            execution_binding: build_execution_binding_identity(template_id, template_id, 0, role),
            model_config: resolve_usage_model_config(self.settings_store.as_deref()),
            call_identity: build_usage_call_identity(&call_id, None, role, None),
            usage: usage_value_to_token_input(usage),
            status,
            error_code: None,
        };
        if let Ok(mut authority) = authority.lock() {
            authority.append_call_record(input);
        }
    }

    fn append_turn_item(&self, session_id: &SessionId, item: ActiveExecutionTurnItem) {
        if let Err(error) = self
            .session_store
            .append_current_turn_item(session_id, item)
        {
            tracing::warn!(session_id = %session_id, ?error, "写入当前 turn item 失败");
        }
    }

    fn upsert_turn_item(&self, session_id: &SessionId, item: ActiveExecutionTurnItem) {
        if let Err(error) = self
            .session_store
            .upsert_current_turn_item(session_id, item)
        {
            tracing::warn!(session_id = %session_id, ?error, "更新当前 turn item 失败");
        }
    }

    fn update_turn_status(&self, session_id: &SessionId, status: &str) {
        if let Err(error) = self
            .session_store
            .update_current_turn_status(session_id, status)
        {
            tracing::warn!(session_id = %session_id, ?error, "更新当前 turn 状态失败");
        }
    }

    fn build_turn_item(
        &self,
        kind: &str,
        status: &str,
        source: &str,
        title: Option<String>,
        content: Option<String>,
        task_id: Option<&TaskId>,
        worker_id: Option<&WorkerId>,
        lane_id: Option<&str>,
        lane_seq: Option<usize>,
        thread_visible: bool,
        worker_visible: bool,
    ) -> ActiveExecutionTurnItem {
        let role_id = task_id
            .and_then(|id| self.pipeline.execution_runtime.task_store().get_task(id))
            .and_then(|task| task.executor_binding.map(|binding| binding.target_role))
            .map(|role| role.trim().to_string())
            .filter(|role| !role.is_empty());
        ActiveExecutionTurnItem {
            item_id: format!("turn-item-{}-{}", kind, UtcMillis::now().0),
            item_seq: 0,
            lane_id: lane_id.map(str::to_string),
            lane_seq,
            kind: kind.to_string(),
            status: status.to_string(),
            source: source.to_string(),
            title,
            content,
            task_id: task_id.cloned(),
            worker_id: worker_id.cloned(),
            role_id,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            thread_visible,
            worker_visible,
        }
    }

    fn build_session_turn_item(
        &self,
        kind: &str,
        status: &str,
        title: Option<String>,
        content: Option<String>,
        tool_call: Option<&ChatToolCall>,
        thread_visible: bool,
    ) -> ActiveExecutionTurnItem {
        ActiveExecutionTurnItem {
            item_id: format!("turn-item-{}-{}", kind, UtcMillis::now().0),
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
            tool_call_id: tool_call.map(|call| call.id.clone()),
            tool_name: tool_call.map(|call| call.function.name.clone()),
            tool_status: None,
            tool_arguments: tool_call.map(|call| call.function.arguments.clone()),
            tool_result: None,
            tool_error: None,
            thread_visible,
            worker_visible: false,
        }
    }

    fn execute_session_tool_call(
        &self,
        tool_call: &ChatToolCall,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> String {
        let Some(ref registry) = self.tool_registry else {
            return serde_json::json!({ "error": "tool registry not available" }).to_string();
        };

        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-session-tool-invoked-{}", UtcMillis::now().0)),
                "session.tool.invoked",
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

        if let Some(mcp_result) = execute_mcp_tool_call(
            self.settings_store.as_deref(),
            &tool_call.function.name,
            &tool_call.function.arguments,
        ) {
            return mcp_result;
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

        output.payload
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
            resolve_session_user_rules_prompt(self.settings_store.as_deref(), &request.session_id)
                .as_deref(),
            resolve_session_safeguard_prompt(self.settings_store.as_deref(), &request.session_id)
                .as_deref(),
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

        let phase_content = if request.use_tools {
            "正在理解请求并准备调用工具。".to_string()
        } else {
            "正在理解请求并生成回复。".to_string()
        };
        let phase_item = self.build_session_turn_item(
            "assistant_phase",
            "running",
            Some("理解请求".to_string()),
            Some(phase_content.clone()),
            None,
            true,
        );
        self.append_turn_item(&request.session_id, phase_item.clone());
        self.publish_session_turn_item_event(
            &request.session_id,
            &request.workspace_id,
            None,
            &phase_item,
        );

        let mut final_content = String::new();
        let mut final_thinking = String::new();
        for round in 0..MAX_TOOL_CALL_ROUNDS {
            let stream_item_id = format!(
                "turn-item-assistant-stream-{}-{}",
                round + 1,
                UtcMillis::now().0
            );
            let mut streamed_content = String::new();
            let mut streamed_thinking = String::new();
            let mut last_persisted_visible_len = 0usize;
            let mut last_persisted_thinking_len = 0usize;
            let mut last_flush_at = Instant::now();
            let mut stream_item_created = false;

            let response = match client.invoke_stream(
                ModelInvocationRequest {
                    provider: SHADOW_MODEL_PROVIDER.to_string(),
                    prompt: prompt.clone(),
                    messages: Some(messages.clone()),
                    tools: tools.clone(),
                    tool_choice: None,
                },
                &mut |event| match event {
                        ModelStreamEvent::ContentDelta { delta } => {
                            if delta.is_empty() {
                                return;
                            }
                            streamed_content.push_str(&delta);
                            let visible_content =
                                normalize_shadow_model_visible_content(&streamed_content);
                            let should_flush = !stream_item_created
                                || delta.contains('\n')
                                || visible_content
                                    .len()
                                    .saturating_sub(last_persisted_visible_len)
                                    >= 32
                                || last_flush_at.elapsed() >= Duration::from_millis(120);
                            if should_flush {
                                let item_content = build_stream_message_content(
                                    &visible_content,
                                    &streamed_thinking,
                                    &stream_item_id,
                                );
                                let mut item = self.build_session_turn_item(
                                    "assistant_stream",
                                    "running",
                                    Some("生成回复".to_string()),
                                    Some(item_content.clone()),
                                    None,
                                    true,
                                );
                                item.item_id = stream_item_id.clone();
                                self.upsert_turn_item(&request.session_id, item.clone());
                                self.publish_session_turn_item_event(
                                    &request.session_id,
                                    &request.workspace_id,
                                    Some(round + 1),
                                    &item,
                                );
                                last_persisted_visible_len = visible_content.len();
                                last_persisted_thinking_len = streamed_thinking.len();
                                stream_item_created = true;
                                last_flush_at = Instant::now();
                            }
                        }
                        ModelStreamEvent::ThinkingDelta { delta } => {
                            if delta.is_empty() {
                                return;
                            }
                            streamed_thinking.push_str(&delta);
                            let visible_content =
                                normalize_shadow_model_visible_content(&streamed_content);
                            let should_flush = !stream_item_created
                                || delta.contains('\n')
                                || streamed_thinking
                                    .len()
                                    .saturating_sub(last_persisted_thinking_len)
                                    >= 32
                                || last_flush_at.elapsed() >= Duration::from_millis(120);
                            if should_flush {
                                let item_content = build_stream_message_content(
                                    &visible_content,
                                    &streamed_thinking,
                                    &stream_item_id,
                                );
                                let mut item = self.build_session_turn_item(
                                    "assistant_stream",
                                    "running",
                                    Some("思考过程".to_string()),
                                    Some(item_content.clone()),
                                    None,
                                    true,
                                );
                                item.item_id = stream_item_id.clone();
                                self.upsert_turn_item(&request.session_id, item.clone());
                                self.publish_session_turn_item_event(
                                    &request.session_id,
                                    &request.workspace_id,
                                    Some(round + 1),
                                    &item,
                                );
                                last_persisted_visible_len = visible_content.len();
                                last_persisted_thinking_len = streamed_thinking.len();
                                stream_item_created = true;
                                last_flush_at = Instant::now();
                            }
                        }
                    },
            ) {
                Ok(response) => response,
                Err(error) => {
                    let failure_content = format!("生成失败：{error}");
                    let failure_item = self.build_session_turn_item(
                        "assistant_final",
                        "failed",
                        Some("生成失败".to_string()),
                        Some(failure_content),
                        None,
                        true,
                    );
                    self.append_turn_item(&request.session_id, failure_item.clone());
                    self.publish_session_turn_item_event(
                        &request.session_id,
                        &request.workspace_id,
                        Some(round + 1),
                        &failure_item,
                    );
                    return Err(ApiError::internal_assembly("执行 session turn 失败", error));
                }
            };

            let parsed = response.parse_chat_payload();
            self.record_llm_usage(
                &request.session_id,
                &request.workspace_id,
                None,
                None,
                "orchestrator",
                UsageSourceRole::Orchestrator,
                parsed.usage.as_ref(),
                UsageCallStatus::Success,
            );
            if let Some(content) = parsed.content.clone() {
                final_content = normalize_shadow_model_visible_content(&content);
                streamed_content = content;
            }
            if !streamed_thinking.trim().is_empty() {
                final_thinking = streamed_thinking.clone();
            }

            let visible_content = normalize_shadow_model_visible_content(&streamed_content);
            if !visible_content.is_empty() || !streamed_thinking.trim().is_empty() {
                let item_content = build_stream_message_content(
                    &visible_content,
                    &streamed_thinking,
                    &stream_item_id,
                );
                let mut item = self.build_session_turn_item(
                    "assistant_stream",
                    "completed",
                    Some("生成回复".to_string()),
                    Some(item_content.clone()),
                    None,
                    true,
                );
                item.item_id = stream_item_id.clone();
                self.upsert_turn_item(&request.session_id, item.clone());
                self.publish_session_turn_item_event(
                    &request.session_id,
                    &request.workspace_id,
                    Some(round + 1),
                    &item,
                );
            }

            if parsed.tool_calls.is_empty() {
                break;
            }
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: parsed.content.clone(),
                tool_calls: parsed.tool_calls.clone(),
                tool_call_id: None,
            });
            for tool_call in &parsed.tool_calls {
                let mut started = self.build_session_turn_item(
                    "tool_call_started",
                    "running",
                    Some(tool_call.function.name.clone()),
                    Some(format!("开始调用工具 {}", tool_call.function.name)),
                    Some(tool_call),
                    true,
                );
                started.tool_status = Some("running".to_string());
                self.append_turn_item(&request.session_id, started.clone());
                self.publish_session_turn_item_event(
                    &request.session_id,
                    &request.workspace_id,
                    Some(round + 1),
                    &started,
                );

                let result = self.execute_session_tool_call(
                    tool_call,
                    &request.session_id,
                    &request.workspace_id,
                );
                let status = infer_tool_call_status(&result);
                let mut completed = self.build_session_turn_item(
                    "tool_call_result",
                    status,
                    Some(tool_call.function.name.clone()),
                    Some(format!(
                        "{}：{}",
                        tool_call.function.name,
                        summarize_tool_result(&result)
                    )),
                    Some(tool_call),
                    true,
                );
                completed.tool_status = Some(status.to_string());
                completed.tool_result = Some(summarize_tool_result(&result));
                completed.tool_error = (status == "error").then(|| summarize_tool_result(&result));
                self.append_turn_item(&request.session_id, completed.clone());
                self.publish_session_turn_item_event(
                    &request.session_id,
                    &request.workspace_id,
                    Some(round + 1),
                    &completed,
                );

                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(result),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool_call.id.clone()),
                });
            }
        }

        if final_content.trim().is_empty() {
            let failure_item = self.build_session_turn_item(
                "assistant_final",
                "failed",
                Some("生成失败".to_string()),
                Some("模型没有返回可展示内容。".to_string()),
                None,
                true,
            );
            self.append_turn_item(&request.session_id, failure_item.clone());
            self.publish_session_turn_item_event(
                &request.session_id,
                &request.workspace_id,
                None,
                &failure_item,
            );
            self.update_turn_status(&request.session_id, "failed");
            return Err(ApiError::internal_assembly(
                "执行 session turn 失败",
                "模型没有返回可展示内容",
            ));
        }

        let final_message_content =
            build_stream_message_content(&final_content, &final_thinking, "assistant-final");
        let final_item = self.build_session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some(final_message_content.clone()),
            None,
            true,
        );
        self.append_turn_item(&request.session_id, final_item.clone());
        self.publish_session_turn_item_event(
            &request.session_id,
            &request.workspace_id,
            None,
            &final_item,
        );
        self.update_turn_status(&request.session_id, "completed");

        Ok(SessionTurnExecutionOutput {
            final_content: final_message_content,
        })
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

    fn publish_llm_delta_event(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        round: usize,
        item_id: &str,
        item_kind: &str,
        status: &str,
        source: &str,
        worker_id: Option<&WorkerId>,
        role_id: Option<&str>,
        lane_id: Option<&str>,
        lane_seq: Option<usize>,
        title: Option<&str>,
        thread_visible: bool,
        worker_visible: bool,
        content: &str,
    ) {
        let content_len = content.chars().count();
        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-task-llm-delta-{}", UtcMillis::now().0)),
                "task.llm.delta",
                serde_json::json!({
                    "task_id": task.task_id.to_string(),
                    "mission_id": task.mission_id.to_string(),
                    "session_id": session_id.to_string(),
                    "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                    "round": round,
                    "item_id": item_id,
                    "item_kind": item_kind,
                    "status": status,
                    "source": source,
                    "worker_id": worker_id.map(ToString::to_string),
                    "role_id": role_id,
                    "lane_id": lane_id,
                    "lane_seq": lane_seq,
                    "title": title,
                    "thread_visible": thread_visible,
                    "worker_visible": worker_visible,
                    "content_length": content_len,
                    "content": content,
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
    }

    fn publish_session_turn_item_event(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        round: Option<usize>,
        item: &ActiveExecutionTurnItem,
    ) {
        let content_len = item
            .content
            .as_deref()
            .map(|value| value.chars().count())
            .unwrap_or(0);
        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-session-turn-item-{}-{}",
                    item.item_id.as_str(),
                    UtcMillis::now().0
                )),
                "session.turn.item.updated",
                serde_json::json!({
                    "session_id": session_id.to_string(),
                    "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                    "round": round,
                    "item_id": item.item_id.as_str(),
                    "item_seq": item.item_seq,
                    "item_kind": item.kind.as_str(),
                    "kind": item.kind.as_str(),
                    "status": item.status.as_str(),
                    "source": item.source.as_str(),
                    "title": item.title.as_deref(),
                    "lane_id": item.lane_id.as_deref(),
                    "lane_seq": item.lane_seq,
                    "task_id": item.task_id.as_ref().map(ToString::to_string),
                    "worker_id": item.worker_id.as_ref().map(ToString::to_string),
                    "role_id": item.role_id.as_deref(),
                    "tool_call_id": item.tool_call_id.as_deref(),
                    "tool_name": item.tool_name.as_deref(),
                    "tool_status": item.tool_status.as_deref(),
                    "tool_arguments": item.tool_arguments.as_deref(),
                    "tool_result": item.tool_result.as_deref(),
                    "tool_error": item.tool_error.as_deref(),
                    "thread_visible": item.thread_visible,
                    "worker_visible": item.worker_visible,
                    "content_length": content_len,
                    "content": item.content.as_deref(),
                }),
            )
            .with_context(EventContext {
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_stream_turn_item(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        round: usize,
        stream_item_id: &str,
        stream_item_kind: &str,
        stream_item_title: Option<String>,
        worker_id: &WorkerId,
        lane_id: Option<&str>,
        lane_seq: Option<usize>,
        is_primary: bool,
        streamed_content: &str,
        status: &str,
        thread_visible: bool,
        last_persisted_content: &mut String,
        stream_item_created: &mut bool,
        force_publish: bool,
    ) {
        if streamed_content.is_empty() {
            return;
        }
        let mut item = self.build_turn_item(
            stream_item_kind,
            status,
            if is_primary {
                "orchestrator"
            } else {
                worker_id.as_str()
            },
            stream_item_title,
            Some(streamed_content.to_string()),
            Some(&task.task_id),
            Some(worker_id),
            lane_id,
            lane_seq,
            thread_visible,
            !is_primary,
        );
        item.item_id = stream_item_id.to_string();
        let item_source = item.source.clone();
        let item_role_id = item.role_id.clone();
        let item_title = item.title.clone();
        let item_worker_id = item.worker_id.clone();
        self.upsert_turn_item(session_id, item);
        if force_publish || streamed_content != last_persisted_content.as_str() {
            self.publish_llm_delta_event(
                task,
                session_id,
                workspace_id,
                round,
                stream_item_id,
                stream_item_kind,
                status,
                &item_source,
                item_worker_id.as_ref(),
                item_role_id.as_deref(),
                lane_id,
                lane_seq,
                item_title.as_deref(),
                thread_visible,
                !is_primary,
                streamed_content,
            );
            *last_persisted_content = streamed_content.to_string();
        }
        *stream_item_created = true;
    }

    fn push_result(&self, task_id: &TaskId, lease_id: &LeaseId, outcome: TaskOutcome) {
        self.result_receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: lease_id.clone(),
            outcome,
        });
    }

    fn dispatch_result_still_authoritative(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
    ) -> bool {
        let task_store = self.pipeline.execution_runtime.task_store();
        let current_task = task_store.get_task(&task.task_id);
        let current_root = task_store.get_task(&task.root_task_id);
        let blocked_or_cancelled = current_task.as_ref().is_some_and(|entry| {
            matches!(entry.status, TaskStatus::Blocked | TaskStatus::Cancelled)
        }) || current_root.as_ref().is_some_and(|entry| {
            matches!(entry.status, TaskStatus::Blocked | TaskStatus::Cancelled)
        });
        if blocked_or_cancelled {
            return false;
        }

        task_store
            .get_active_lease(&task.task_id)
            .is_some_and(|lease| lease.lease_id == *lease_id)
    }

    fn execute_dispatch_plan(
        &self,
        task: &magi_core::Task,
        task_id: &TaskId,
        lease_id: &LeaseId,
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
    ) -> Result<(), String> {
        if target.task_id != task.task_id
            || target.mission_id != task.mission_id
            || target.root_task_id != task.root_task_id
        {
            return Err(format!("执行计划目标与任务不一致: {}", task.task_id));
        }
        if target
            .requested_worker_id
            .as_ref()
            .is_none_or(|requested_worker_id| requested_worker_id != &worker_id)
        {
            return Err(format!("执行计划缺少一致的 worker 绑定: {}", task.task_id));
        }
        if ownership
            .worker_id
            .as_ref()
            .is_some_and(|ownership_worker_id| ownership_worker_id != &worker_id)
        {
            return Err(format!(
                "执行计划 worker 与 ownership 不一致: {}",
                task.task_id
            ));
        }
        let worker_runtime = self.pipeline.execution_runtime.worker_runtime();
        self.update_turn_status(&session_id, "running");
        if is_primary {
            self.append_turn_item(
                &session_id,
                self.build_turn_item(
                    "assistant_phase",
                    "running",
                    "orchestrator",
                    Some("开始执行".to_string()),
                    Some(format!("正在执行主线任务：{}", task.title)),
                    Some(task_id),
                    Some(&worker_id),
                    lane_id.as_deref(),
                    lane_seq,
                    true,
                    false,
                ),
            );
        } else {
            self.append_turn_item(
                &session_id,
                self.build_turn_item(
                    "worker_phase",
                    "running",
                    worker_id.as_str(),
                    Some(task.title.clone()),
                    Some(format!("开始执行分支：{}", task.title)),
                    Some(task_id),
                    Some(&worker_id),
                    lane_id.as_deref(),
                    lane_seq,
                    false,
                    true,
                ),
            );
        }
        if let Some(intent) = self.pipeline.execution_runtime.build_execution_intent(
            &target,
            worker_id.clone(),
            Some(session_id.clone()),
            workspace_id.clone(),
            None,
        ) {
            let binding_lifecycle = Some(intent.execution_profile.binding_lifecycle);
            let execution_intent_ref = Some(format!("worker-intent-{}", intent.task_id));
            worker_runtime.register_execution_intent(intent);
            let _ = worker_runtime.resume_from_execution_target(&target);
            worker_runtime.record_branch_checkpoint(
                task_id,
                &worker_id,
                WorkerStage::Execute,
                Some(lease_id.to_string()),
                execution_intent_ref,
                binding_lifecycle,
                Some(magi_worker_runtime::WorkerExecutionCheckpointCursor {
                    checkpoint_stage: WorkerStage::Execute,
                    next_step_index: 0,
                    checkpoint_at: UtcMillis::now(),
                    resume_mode: magi_worker_runtime::WorkerCheckpointResumeMode::StageRestart,
                    resume_token: None,
                }),
            );
        }
        let (outcome, context_summary) = self.invoke_llm_with_tools(
            task,
            &session_id,
            &workspace_id,
            use_tools,
            skill_name,
            lane_id.as_deref(),
            lane_seq,
            is_primary,
            &worker_id,
        );
        if !self.dispatch_result_still_authoritative(task, lease_id) {
            tracing::info!(
                task_id = %task.task_id,
                lease_id = %lease_id,
                "dispatch outcome arrived after interrupt or lease revocation; skip finish/writeback/result"
            );
            return Ok(());
        }
        match &outcome {
            TaskOutcome::Completed { output_refs } => {
                let summary = if output_refs.is_empty() {
                    format!("任务 {} 已完成", task.title)
                } else {
                    output_refs.join("\n")
                };
                let _ = worker_runtime.finish(&worker_id, summary);
                self.append_turn_item(
                    &session_id,
                    self.build_turn_item(
                        if is_primary {
                            "assistant_final"
                        } else {
                            "worker_completed"
                        },
                        "completed",
                        if is_primary {
                            "orchestrator"
                        } else {
                            worker_id.as_str()
                        },
                        Some(task.title.clone()),
                        Some(
                            output_refs
                                .last()
                                .cloned()
                                .unwrap_or_else(|| format!("任务 {} 已完成", task.title)),
                        ),
                        Some(task_id),
                        Some(&worker_id),
                        lane_id.as_deref(),
                        lane_seq,
                        true,
                        !is_primary,
                    ),
                );
                if is_primary {
                    self.update_turn_status(&session_id, "completed");
                }
            }
            TaskOutcome::Failed { error } => {
                let _ = worker_runtime.fail(&worker_id, error.clone());
                self.append_turn_item(
                    &session_id,
                    self.build_turn_item(
                        if is_primary {
                            "assistant_phase"
                        } else {
                            "worker_phase"
                        },
                        "failed",
                        if is_primary {
                            "orchestrator"
                        } else {
                            worker_id.as_str()
                        },
                        Some(task.title.clone()),
                        Some(error.clone()),
                        Some(task_id),
                        Some(&worker_id),
                        lane_id.as_deref(),
                        lane_seq,
                        is_primary,
                        !is_primary,
                    ),
                );
                if is_primary {
                    self.update_turn_status(&session_id, "failed");
                }
            }
            TaskOutcome::NeedsRepair { reason } => {
                let _ = worker_runtime.start_repair(&worker_id);
                let _ = worker_runtime.record_repair_note(&worker_id, reason.clone());
                self.append_turn_item(
                    &session_id,
                    self.build_turn_item(
                        if is_primary {
                            "assistant_phase"
                        } else {
                            "worker_phase"
                        },
                        "repairing",
                        if is_primary {
                            "orchestrator"
                        } else {
                            worker_id.as_str()
                        },
                        Some(task.title.clone()),
                        Some(reason.clone()),
                        Some(task_id),
                        Some(&worker_id),
                        lane_id.as_deref(),
                        lane_seq,
                        is_primary,
                        !is_primary,
                    ),
                );
            }
            TaskOutcome::NeedsVerification { output_refs } => {
                let _ = worker_runtime.start_verification(&worker_id);
                let summary = if output_refs.is_empty() {
                    format!("任务 {} 进入验证阶段", task.title)
                } else {
                    output_refs.join("\n")
                };
                let _ = worker_runtime.record_verification(
                    &worker_id,
                    magi_core::VerificationStatus::Pending,
                    summary,
                );
                self.append_turn_item(
                    &session_id,
                    self.build_turn_item(
                        if is_primary {
                            "assistant_phase"
                        } else {
                            "worker_phase"
                        },
                        "verifying",
                        if is_primary {
                            "orchestrator"
                        } else {
                            worker_id.as_str()
                        },
                        Some(task.title.clone()),
                        Some(
                            output_refs
                                .last()
                                .cloned()
                                .unwrap_or_else(|| format!("任务 {} 进入验证阶段", task.title)),
                        ),
                        Some(task_id),
                        Some(&worker_id),
                        lane_id.as_deref(),
                        lane_seq,
                        is_primary,
                        !is_primary,
                    ),
                );
            }
        }
        if matches!(&outcome, TaskOutcome::Completed { .. }) {
            self.session_store
                .bind_execution_ownership(session_id.clone(), ownership);
            writebacks.apply(&self.pipeline.memory_store);
            self.publish_execution_overview(task, &session_id, &workspace_id, context_summary);
        }
        self.push_result(task_id, lease_id, outcome);
        Ok(())
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
        let mut tool_definitions = self
            .tool_registry
            .as_ref()
            .map(|registry| {
                registry
                    .builtin_specs()
                    .into_iter()
                    .filter(|spec| {
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
                            description: builtin_tool_description(&spec.name),
                            parameters: builtin_tool_parameters(&spec.name),
                        },
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        tool_definitions.extend(available_mcp_tool_definitions(
            self.settings_store.as_deref(),
        ));
        tool_definitions.sort_by(|left, right| left.function.name.cmp(&right.function.name));
        tool_definitions.dedup_by(|left, right| left.function.name == right.function.name);
        tool_definitions
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
        let user_rules_prefix =
            resolve_session_user_rules_prompt(self.settings_store.as_deref(), session_id);
        let safeguard_prefix =
            resolve_session_safeguard_prompt(self.settings_store.as_deref(), session_id);

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

        if let Some(mcp_result) = execute_mcp_tool_call(
            self.settings_store.as_deref(),
            &tool_call.function.name,
            &tool_call.function.arguments,
        ) {
            return mcp_result;
        }

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
        };

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

    fn resolve_model_client(&self) -> Option<Arc<dyn ModelBridgeClient>> {
        if let Some(ref store) = self.settings_store {
            let config = store.get_section("orchestrator");
            let base_url = config
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            if let Some(base_url) = base_url {
                let api_key = config
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let model = config
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "gpt-4".to_string());
                let reasoning_effort = config
                    .get("reasoningEffort")
                    .or_else(|| config.get("reasoning_effort"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let enable_thinking = config
                    .get("enableThinking")
                    .or_else(|| config.get("enable_thinking"))
                    .and_then(|value| value.as_bool());
                return Some(Arc::new(
                    HttpModelBridgeClient::new(base_url.to_string(), api_key, model)
                        .with_generation_options(reasoning_effort, enable_thinking),
                ));
            }
        }
        self.model_bridge_client.clone()
    }

    fn invoke_llm_with_tools(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        use_tools: bool,
        skill_name: Option<String>,
        lane_id: Option<&str>,
        lane_seq: Option<usize>,
        is_primary: bool,
        worker_id: &WorkerId,
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
        self.append_turn_item(
            session_id,
            self.build_turn_item(
                "assistant_phase",
                "running",
                if is_primary {
                    "orchestrator"
                } else {
                    worker_id.as_str()
                },
                Some(if is_primary {
                    "分析任务".to_string()
                } else {
                    task.title.clone()
                }),
                Some(if use_tools {
                    "正在分析请求并准备调用工具。".to_string()
                } else {
                    "正在分析请求并生成结果。".to_string()
                }),
                Some(&task.task_id),
                Some(worker_id),
                lane_id,
                lane_seq,
                is_primary,
                !is_primary,
            ),
        );

        for round in 0..MAX_TOOL_CALL_ROUNDS {
            let request = ModelInvocationRequest {
                provider: SHADOW_MODEL_PROVIDER.to_string(),
                prompt: prompt.clone(),
                messages: Some(messages.clone()),
                tools: tools.clone(),
                tool_choice: None,
            };

            let stream_item_kind = if is_primary {
                "assistant_stream"
            } else {
                "worker_stream"
            };
            let stream_item_id = format!(
                "turn-item-{stream_item_kind}-{}-{}",
                round + 1,
                UtcMillis::now().0
            );
            let stream_item_title = Some(if is_primary {
                "生成回复".to_string()
            } else {
                task.title.clone()
            });
            let mut streamed_content = String::new();
            let mut streamed_thinking = String::new();
            let mut last_persisted_content = String::new();
            let mut last_persisted_visible_len = 0usize;
            let mut last_persisted_thinking_len = 0usize;
            let mut stream_item_created = false;
            let mut last_flush_at = Instant::now();

            let response = match client.invoke_stream(request, &mut |event| match event {
                ModelStreamEvent::ContentDelta { delta } => {
                    if delta.is_empty() {
                        return;
                    }
                    streamed_content.push_str(&delta);
                    let visible_streamed_content =
                        normalize_shadow_model_visible_content(&streamed_content);
                    let should_flush = !stream_item_created
                        || delta.contains('\n')
                        || visible_streamed_content
                            .len()
                            .saturating_sub(last_persisted_visible_len)
                            >= 32
                        || last_flush_at.elapsed() >= Duration::from_millis(120);
                    if should_flush {
                        let stream_payload = build_stream_message_content(
                            &visible_streamed_content,
                            &streamed_thinking,
                            &stream_item_id,
                        );
                        self.sync_stream_turn_item(
                            task,
                            session_id,
                            workspace_id,
                            round + 1,
                            &stream_item_id,
                            stream_item_kind,
                            stream_item_title.clone(),
                            worker_id,
                            lane_id,
                            lane_seq,
                            is_primary,
                            &stream_payload,
                            "running",
                            true,
                            &mut last_persisted_content,
                            &mut stream_item_created,
                            true,
                        );
                        last_persisted_visible_len = visible_streamed_content.len();
                        last_persisted_thinking_len = streamed_thinking.len();
                        last_flush_at = Instant::now();
                    }
                }
                ModelStreamEvent::ThinkingDelta { delta } => {
                    if delta.is_empty() {
                        return;
                    }
                    streamed_thinking.push_str(&delta);
                    let visible_streamed_content =
                        normalize_shadow_model_visible_content(&streamed_content);
                    let should_flush = !stream_item_created
                        || delta.contains('\n')
                        || streamed_thinking
                            .len()
                            .saturating_sub(last_persisted_thinking_len)
                            >= 32
                        || last_flush_at.elapsed() >= Duration::from_millis(120);
                    if should_flush {
                        let stream_payload = build_stream_message_content(
                            &visible_streamed_content,
                            &streamed_thinking,
                            &stream_item_id,
                        );
                        self.sync_stream_turn_item(
                            task,
                            session_id,
                            workspace_id,
                            round + 1,
                            &stream_item_id,
                            stream_item_kind,
                            Some("思考过程".to_string()),
                            worker_id,
                            lane_id,
                            lane_seq,
                            is_primary,
                            &stream_payload,
                            "running",
                            true,
                            &mut last_persisted_content,
                            &mut stream_item_created,
                            true,
                        );
                        last_persisted_visible_len = visible_streamed_content.len();
                        last_persisted_thinking_len = streamed_thinking.len();
                        last_flush_at = Instant::now();
                    }
                }
            }) {
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
            };

            let parsed = response.parse_chat_payload();
            let usage_role = if is_primary {
                UsageSourceRole::Orchestrator
            } else {
                UsageSourceRole::Worker
            };
            let usage_template = if is_primary {
                "orchestrator".to_string()
            } else {
                worker_id.to_string()
            };
            self.record_llm_usage(
                session_id,
                workspace_id,
                Some(task.task_id.to_string()),
                (!is_primary).then(|| task.task_id.to_string()),
                &usage_template,
                usage_role,
                parsed.usage.as_ref(),
                UsageCallStatus::Success,
            );

            if let Some(ref content) = parsed.content {
                final_content = normalize_shadow_model_visible_content(content);
                if streamed_content != *content {
                    streamed_content = content.clone();
                }
            }

            if parsed.tool_calls.is_empty() {
                let visible_streamed_content =
                    normalize_shadow_model_visible_content(&streamed_content);
                if !visible_streamed_content.is_empty() || !streamed_thinking.trim().is_empty() {
                    let stream_payload = build_stream_message_content(
                        &visible_streamed_content,
                        &streamed_thinking,
                        &stream_item_id,
                    );
                    self.sync_stream_turn_item(
                        task,
                        session_id,
                        workspace_id,
                        round + 1,
                        &stream_item_id,
                        stream_item_kind,
                        stream_item_title.clone(),
                        worker_id,
                        lane_id,
                        lane_seq,
                        is_primary,
                        &stream_payload,
                        "completed",
                        false,
                        &mut last_persisted_content,
                        &mut stream_item_created,
                        true,
                    );
                }
                let _ = self.event_bus.publish(
                    EventEnvelope::domain(
                        EventId::new(format!("event-task-llm-completed-{}", UtcMillis::now().0)),
                        "task.llm.completed",
                        serde_json::json!({
                            "task_id": task.task_id.to_string(),
                            "mission_id": task.mission_id.to_string(),
                            "session_id": session_id.to_string(),
                            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                            "response_length": final_content.len(),
                            "rounds": round + 1,
                        }),
                    )
                    .with_context(task_context.clone()),
                );
                break;
            }

            let visible_streamed_content =
                normalize_shadow_model_visible_content(&streamed_content);
            if !visible_streamed_content.is_empty() || !streamed_thinking.trim().is_empty() {
                let should_force_publish = !stream_item_created;
                let stream_payload = build_stream_message_content(
                    &visible_streamed_content,
                    &streamed_thinking,
                    &stream_item_id,
                );
                self.sync_stream_turn_item(
                    task,
                    session_id,
                    workspace_id,
                    round + 1,
                    &stream_item_id,
                    stream_item_kind,
                    stream_item_title.clone(),
                    worker_id,
                    lane_id,
                    lane_seq,
                    is_primary,
                    &stream_payload,
                    "completed",
                    true,
                    &mut last_persisted_content,
                    &mut stream_item_created,
                    should_force_publish,
                );
            }

            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: parsed.content.clone(),
                tool_calls: parsed.tool_calls.clone(),
                tool_call_id: None,
            });

            for tc in &parsed.tool_calls {
                self.append_turn_item(
                    session_id,
                    ActiveExecutionTurnItem {
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.function.name.clone()),
                        tool_status: Some("running".to_string()),
                        tool_arguments: Some(tc.function.arguments.clone()),
                        ..self.build_turn_item(
                            if is_primary {
                                "tool_call_started"
                            } else {
                                "worker_tool_call_started"
                            },
                            "running",
                            if is_primary {
                                "orchestrator"
                            } else {
                                worker_id.as_str()
                            },
                            Some(tc.function.name.clone()),
                            Some(format!("开始调用工具 {}", tc.function.name)),
                            Some(&task.task_id),
                            Some(worker_id),
                            lane_id,
                            lane_seq,
                            is_primary,
                            !is_primary,
                        )
                    },
                );
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
                self.append_turn_item(
                    session_id,
                    ActiveExecutionTurnItem {
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.function.name.clone()),
                        tool_status: Some(status.to_string()),
                        tool_arguments: Some(tc.function.arguments.clone()),
                        tool_result: Some(summarize_tool_result(&result)),
                        tool_error: (status == "error").then(|| summarize_tool_result(&result)),
                        ..self.build_turn_item(
                            if is_primary {
                                "tool_call_result"
                            } else {
                                "worker_tool_call_result"
                            },
                            status,
                            if is_primary {
                                "orchestrator"
                            } else {
                                worker_id.as_str()
                            },
                            Some(tc.function.name.clone()),
                            Some(format!(
                                "{}：{}",
                                tc.function.name,
                                summarize_tool_result(&result)
                            )),
                            Some(&task.task_id),
                            Some(worker_id),
                            lane_id,
                            lane_seq,
                            is_primary,
                            !is_primary,
                        )
                    },
                );
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

impl TaskDispatcher for ShadowTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
    ) -> Result<(), String> {
        let Some(plan) = self.execution_registry.remove(&task.task_id) else {
            return Err(format!("任务缺少执行计划: {}", task.task_id));
        };

        match plan {
            ShadowTaskExecutionPlan::Dispatch {
                target,
                worker_id,
                lane_id,
                lane_seq,
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
                    target,
                    worker_id,
                    lane_id,
                    lane_seq,
                    is_primary,
                    session_id,
                    workspace_id,
                    ownership,
                    writebacks,
                    use_tools,
                    skill_name,
                )?;
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
                if shadow_dispatch_interrupted(task_store, root_task_id, action_task_id) {
                    return Ok(ShadowGraphDriveResult {
                        runner_started: executed,
                    });
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
    if action_status == TaskStatus::Blocked
        && shadow_dispatch_interrupted(task_store, root_task_id, action_task_id)
    {
        return Ok(ShadowGraphDriveResult {
            runner_started: executed,
        });
    }
    if action_status != TaskStatus::Completed && action_status != TaskStatus::Failed {
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

fn shadow_dispatch_interrupted(
    task_store: &magi_orchestrator::task_store::TaskStore,
    root_task_id: &TaskId,
    action_task_id: &TaskId,
) -> bool {
    let root_blocked = task_store
        .get_task(root_task_id)
        .map(|task| task.status == TaskStatus::Blocked)
        .unwrap_or(false);
    let action_blocked = task_store
        .get_task(action_task_id)
        .map(|task| task.status == TaskStatus::Blocked)
        .unwrap_or(false);
    root_blocked || action_blocked
}

fn configured_mcp_servers(
    store: Option<&crate::settings_store::SettingsStore>,
) -> Vec<serde_json::Value> {
    store
        .and_then(|store| store.get_section("mcpServers").as_array().cloned())
        .unwrap_or_default()
}

fn mcp_server_id(entry: &serde_json::Value) -> Option<String> {
    entry
        .get("id")
        .and_then(|value| value.as_str())
        .or_else(|| entry.get("serverId").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn encode_mcp_tool_name(server_id: &str, tool_name: &str) -> String {
    format!("{MCP_TOOL_NAME_PREFIX}{server_id}__{tool_name}")
}

fn decode_mcp_tool_name(encoded_name: &str) -> Option<(String, String)> {
    let mut parts = encoded_name.splitn(3, "__");
    let prefix = parts.next()?;
    if prefix != "mcp" {
        return None;
    }
    let server_id = parts.next()?.trim().to_string();
    let tool_name = parts.next()?.trim().to_string();
    if server_id.is_empty() || tool_name.is_empty() {
        return None;
    }
    Some((server_id, tool_name))
}

fn normalize_mcp_tool_parameters(input_schema: Option<serde_json::Value>) -> serde_json::Value {
    match input_schema {
        Some(serde_json::Value::Object(map)) if !map.is_empty() => serde_json::Value::Object(map),
        _ => json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn mcp_tool_definition(server_id: &str, tool: McpToolInfo) -> ChatToolDefinition {
    let tool_name = tool.name;
    let description = tool
        .description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("MCP tool {tool_name} from server {server_id}"));
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: encode_mcp_tool_name(server_id, &tool_name),
            description,
            parameters: normalize_mcp_tool_parameters(tool.input_schema),
        },
    }
}

fn available_mcp_tool_definitions(
    store: Option<&crate::settings_store::SettingsStore>,
) -> Vec<ChatToolDefinition> {
    let mut tool_definitions = Vec::new();
    for entry in configured_mcp_servers(store) {
        let enabled = entry
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let Some(server_id) = mcp_server_id(&entry) else {
            continue;
        };
        let Some(config) = build_mcp_config_from_entry(&entry) else {
            continue;
        };
        let client = StdioMcpBridgeClient::new(config);
        let Ok(tools) = client.list_tools() else {
            continue;
        };
        tool_definitions.extend(
            tools
                .into_iter()
                .map(|tool| mcp_tool_definition(&server_id, tool)),
        );
    }
    tool_definitions
}

fn execute_mcp_tool_call(
    store: Option<&crate::settings_store::SettingsStore>,
    encoded_tool_name: &str,
    input: &str,
) -> Option<String> {
    let (server_id, tool_name) = decode_mcp_tool_name(encoded_tool_name)?;
    let entry = configured_mcp_servers(store)
        .into_iter()
        .find(|candidate| mcp_server_id(candidate).as_deref() == Some(server_id.as_str()))?;
    let config = build_mcp_config_from_entry(&entry)?;
    let client = StdioMcpBridgeClient::new(config);
    match client.call_tool(McpToolCallRequest {
        server_name: server_id,
        tool_name,
        input: input.to_string(),
    }) {
        Ok(response) if response.ok => Some(response.payload),
        Ok(response) => Some(json!({ "error": response.payload }).to_string()),
        Err(error) => Some(json!({ "error": format!("MCP 调用失败: {error:?}") }).to_string()),
    }
}

fn builtin_tool_description(name: &str) -> String {
    match name {
        "file_read" => "Read the contents of a file at a given path".to_string(),
        "search_text" => "Search for text patterns in files within a directory".to_string(),
        "shell_exec" => "Execute a bounded shell command and return stdout/stderr. Use process_launch for long-running background processes.".to_string(),
        "process_launch" => "Start a long-running shell process in the background and return a terminal_id.".to_string(),
        "process_read" => "Read buffered stdout/stderr from a background process.".to_string(),
        "process_write" => "Write input to a background process.".to_string(),
        "process_kill" => "Stop a background process by terminal_id.".to_string(),
        "process_list" => "List background processes for the current session/workspace.".to_string(),
        "process_inspect" => "Inspect running processes by PID or name".to_string(),
        "diff_preview" => "Generate a unified diff between two text inputs".to_string(),
        _ => format!("Builtin tool: {name}"),
    }
}

fn builtin_tool_parameters(name: &str) -> serde_json::Value {
    match name {
        "file_read" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file to read" }
            },
            "required": ["path"]
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
                "cwd": { "type": "string", "description": "Working directory" },
                "access_mode": { "type": "string", "enum": ["read_only", "maybe_write", "explicit_write"], "description": "Use read_only for inspection commands; maybe_write/explicit_write hold write protection for the working directory." },
                "timeout_ms": { "type": "integer", "description": "Maximum execution time in milliseconds for bounded commands. Long-running processes should use process_launch instead." },
                "background": { "type": "boolean", "description": "When true, launch as a background process and return terminal_id immediately." }
            },
            "required": ["command"]
        }),
        "process_launch" => serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Long-running shell command to start in the background" },
                "cwd": { "type": "string", "description": "Working directory" },
                "access_mode": { "type": "string", "enum": ["read_only", "maybe_write", "explicit_write"], "description": "Use read_only for non-mutating watchers; maybe_write/explicit_write briefly checks launch write scope." }
            },
            "required": ["command"]
        }),
        "process_read" => serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "integer", "description": "Background process terminal id" },
                "max_bytes": { "type": "integer", "description": "Maximum output bytes to return" }
            },
            "required": ["terminal_id"]
        }),
        "process_write" => serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "integer", "description": "Background process terminal id" },
                "input": { "type": "string", "description": "Text to write to stdin" }
            },
            "required": ["terminal_id", "input"]
        }),
        "process_kill" => serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "integer", "description": "Background process terminal id" }
            },
            "required": ["terminal_id"]
        }),
        "process_list" => serde_json::json!({
            "type": "object",
            "properties": {}
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

fn normalize_shadow_model_visible_content(content: &str) -> String {
    let mut value = content.trim();
    while let Some(rest) = value.strip_prefix("shadow-model::") {
        value = rest.trim();
    }
    let chunks = value
        .split("\n\n")
        .map(str::trim)
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>();
    if chunks.len() == 2 {
        let first = chunks[0];
        let second = chunks[1];
        if first == second {
            return second.to_string();
        }
        if first
            .strip_prefix("执行:")
            .or_else(|| first.strip_prefix("执行："))
            .or_else(|| first.strip_prefix("继续:"))
            .or_else(|| first.strip_prefix("继续："))
            .map(str::trim)
            == Some(second)
        {
            return second.to_string();
        }
    }
    value.to_string()
}

fn build_stream_message_content(
    visible_content: &str,
    thinking_content: &str,
    item_id: &str,
) -> String {
    let visible_content = visible_content.trim();
    let thinking_content = thinking_content.trim();
    if thinking_content.is_empty() {
        return visible_content.to_string();
    }

    let mut blocks = Vec::new();
    blocks.push(json!({
        "type": "thinking",
        "blockId": format!("{item_id}-thinking"),
        "content": thinking_content,
    }));
    if !visible_content.is_empty() {
        blocks.push(json!({
            "type": "text",
            "content": visible_content,
        }));
    }

    json!({ "blocks": blocks }).to_string()
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
    use crate::settings_store::SettingsStore;
    use serde_json::json;

    fn mock_mcp_server_entry() -> serde_json::Value {
        json!({
            "id": "mock-mcp-e2e",
            "serverId": "mock-mcp-e2e",
            "name": "mock-mcp-e2e",
            "enabled": true,
            "command": "sh",
            "args": [
                "-c",
                r#"while IFS= read -r line; do
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
  case "$method" in
    initialize)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock-mcp","version":"0.1.0"}}}\n' "$id"
      ;;
    notifications/initialized)
      ;;
    tools/list)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"mock.echo","description":"A mock echo tool","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}\n' "$id"
      ;;
    tools/call)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"mock tool result"}]}}\n' "$id"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
      ;;
  esac
done"#
            ]
        })
    }

    #[test]
    fn normalizes_shadow_model_visible_echo_content() {
        assert_eq!(
            normalize_shadow_model_visible_content("shadow-model::执行: 整理结果\n\n整理结果"),
            "整理结果"
        );
        assert_eq!(
            normalize_shadow_model_visible_content(
                "shadow-model::shadow-model::汇总执行结果\n\n汇总执行结果"
            ),
            "汇总执行结果"
        );
    }

    #[test]
    fn session_user_rules_prompt_only_reads_current_session() {
        let store = SettingsStore::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        store.set_session_section(
            &session_a,
            "userRules",
            serde_json::json!({ "userRules": "【A】" }),
        );
        store.set_session_section(
            &session_b,
            "userRules",
            serde_json::json!({ "userRules": "【B】" }),
        );

        assert_eq!(
            resolve_session_user_rules_prompt(Some(&store), &session_a).as_deref(),
            Some("【A】")
        );
        assert_eq!(
            resolve_session_user_rules_prompt(Some(&store), &session_b).as_deref(),
            Some("【B】")
        );
    }

    #[test]
    fn session_safeguard_prompt_uses_session_specific_rules_without_leaking() {
        let store = SettingsStore::new();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        store.set_session_section(
            &session_a,
            "safeguardConfig",
            serde_json::json!({
                "rules": [
                    {
                        "pattern": "custom-danger-a",
                        "enabled": true,
                        "category": "custom"
                    }
                ]
            }),
        );

        let prompt_a = resolve_session_safeguard_prompt(Some(&store), &session_a)
            .expect("session a should have safeguard prompt");
        let prompt_b = resolve_session_safeguard_prompt(Some(&store), &session_b)
            .expect("session b should still have builtin safeguard prompt");

        assert!(prompt_a.contains("custom-danger-a"));
        assert!(prompt_a.contains("git push --force"));
        assert!(!prompt_b.contains("custom-danger-a"));
        assert!(prompt_b.contains("git push --force"));
    }

    #[test]
    fn available_mcp_tool_definitions_exposes_connected_server_tools() {
        let store = SettingsStore::new();
        store.set_section("mcpServers", json!([mock_mcp_server_entry()]));

        let tool_definitions = available_mcp_tool_definitions(Some(&store));
        assert_eq!(tool_definitions.len(), 1);
        assert_eq!(
            tool_definitions[0].function.name,
            "mcp__mock-mcp-e2e__mock.echo"
        );
        assert_eq!(tool_definitions[0].function.description, "A mock echo tool");
        assert_eq!(
            tool_definitions[0].function.parameters["required"][0],
            "text"
        );
    }

    #[test]
    fn execute_mcp_tool_call_dispatches_to_configured_server() {
        let store = SettingsStore::new();
        store.set_section("mcpServers", json!([mock_mcp_server_entry()]));

        let result = execute_mcp_tool_call(
            Some(&store),
            "mcp__mock-mcp-e2e__mock.echo",
            r#"{"text":"hello"}"#,
        )
        .expect("mcp tool should resolve");

        assert_eq!(result, "mock tool result");
    }
}
