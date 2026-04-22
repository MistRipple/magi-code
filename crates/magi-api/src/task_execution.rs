use crate::{
    errors::ApiError,
    shadow_execution::run_shadow_dispatch_submission,
    state::{ApiState, ShadowExecutionPipeline},
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition,
    HttpModelBridgeClient, ModelBridgeClient, ModelInvocationRequest, SHADOW_MODEL_PROVIDER,
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
use magi_session_store::{ActiveExecutionBranch, ActiveExecutionChain, SessionStore};
use magi_tool_runtime::{BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry};
use magi_workspace::RecoveryStatus;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug)]
pub enum ShadowTaskExecutionPlan {
    Dispatch {
        target: TaskExecutionTarget,
        worker_id: WorkerId,
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
            "恢复入口 {} 当前状态为 prepared，必须先进入 ready 才能继续会话",
            recovery_id
        ))),
        RecoveryStatus::Consumed => Err(ApiError::InvalidInput(format!(
            "恢复入口 {} 已被消费，不能再次继续会话",
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

    let writebacks = ExecutionWritebackPlans::from_recovery_resume_input(&input);
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
    let mut chain = sidecar.active_execution_chain.ok_or_else(|| {
        ApiError::InvalidInput("当前会话没有可继续的执行链".to_string())
    })?;
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

    let resumable_branches = chain
        .branches
        .iter()
        .filter_map(|branch| {
            let task = task_store.get_task(&branch.task_id)?;
            if task.mission_id != chain.mission_id || task.root_task_id != chain.root_task_id {
                return None;
            }
            if task_status_is_terminal(&task.status) {
                return None;
            }
            Some(branch.clone())
        })
        .collect::<Vec<_>>();
    if resumable_branches.is_empty() {
        return Err(ApiError::InvalidInput("当前会话没有可继续的 branch".to_string()));
    }

    let primary_branch = resumable_branches
        .iter()
        .find(|branch| branch.is_primary)
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
        return Err(ApiError::InvalidInput("当前会话执行链已结束，不能继续".to_string()));
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
            return Err(ApiError::not_found("根任务不存在", chain.root_task_id.as_str()));
        }
    };

    Ok(SessionContinueAccepted {
        session_id: session_id.clone(),
        mission_id: chain.mission_id,
        root_task_id: chain.root_task_id,
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
    context_runtime: Option<Arc<ContextRuntime>>,
    tool_registry: Option<ToolRegistry>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
}

const MAX_TOOL_CALL_ROUNDS: usize = 8;

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
    ) {
        let (outcome, context_summary) =
            self.invoke_llm_with_tools(task, &session_id, &workspace_id, use_tools, skill_name);
        if matches!(&outcome, TaskOutcome::Completed { .. }) {
            self.session_store
                .bind_execution_ownership(session_id.clone(), ownership);
            writebacks.apply(&self.pipeline.memory_store);
            self.publish_execution_overview(task, &session_id, &workspace_id, context_summary);
        }
        self.push_result(task_id, lease_id, outcome);
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
            EventId::new(format!(
                "event-mission-overview-{}",
                UtcMillis::now().0
            )),
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
            .collect()
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
        let result = ctx_runtime.assemble_execution_context(
            &ExecutionContextAssemblyRequest {
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
            },
        );
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
            .filter(|rule| rule.get("enabled").and_then(|value| value.as_bool()).unwrap_or(true))
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
                EventId::new(format!(
                    "event-task-tool-invoked-{}",
                    UtcMillis::now().0
                )),
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
                return Some(Arc::new(HttpModelBridgeClient::new(
                    base_url.to_string(),
                    api_key,
                    model,
                )));
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
            if tool_defs.is_empty() { None } else { Some(tool_defs) }
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
            };

            let response = match client.invoke(request) {
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

            if let Some(ref content) = parsed.content {
                final_content = content.clone();
            }

            if parsed.tool_calls.is_empty() {
                let _ = self.event_bus.publish(
                    EventEnvelope::domain(
                        EventId::new(format!(
                            "event-task-llm-completed-{}",
                            UtcMillis::now().0
                        )),
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
    if let Some(rules) = safeguard_rules.map(str::trim).filter(|rules| !rules.is_empty()) {
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
            let (outcome, _) = self.invoke_llm_with_tools(task, &session_id, &None, false, None);
            self.push_result(&task.task_id, &lease.lease_id, outcome);
            return Ok(());
        };

        match plan {
            ShadowTaskExecutionPlan::Dispatch {
                target: _,
                worker_id: _,
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
    if action_status != TaskStatus::Completed && action_status != TaskStatus::Failed {
        return Err(ApiError::internal_assembly(
            failure_title,
            format!("task runner did not complete action task: {:?}", action_status),
        ));
    }

    Ok(ShadowGraphDriveResult {
        runner_started: executed,
    })
}

fn builtin_tool_description(name: &str) -> String {
    match name {
        "file_read" => "Read the contents of a file at a given path".to_string(),
        "search_text" => "Search for text patterns in files within a directory".to_string(),
        "shell_exec" => "Execute a shell command and return stdout/stderr".to_string(),
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
        _ => serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn infer_tool_call_status(result: &str) -> &'static str {
    let parsed = serde_json::from_str::<serde_json::Value>(result).ok();
    match parsed.as_ref().and_then(|v| v.get("status")).and_then(|v| v.as_str()) {
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
