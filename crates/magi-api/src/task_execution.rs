use crate::{
    dto::RecoveryResumeRequestDto,
    errors::ApiError,
    shadow_execution::{run_shadow_dispatch_submission, run_shadow_recovery_resume},
    state::{ApiState, ShadowExecutionPipeline},
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, ChatToolFunctionDefinition,
    HttpModelBridgeClient, ModelBridgeClient, ModelInvocationRequest,
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionOwnership, LeaseId, RiskLevel, SessionId,
    TaskExecutionTarget, TaskId, TaskStatus, ToolCallId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::{
    ExecutionWritebackPlans, MissionContextSummary, RecoveryExecutionResult,
    task_runner::{EventBasedResultReceiver, TaskDispatcher, TaskOutcome, TaskResult, WorkerInfo},
};
use magi_session_store::SessionStore;
use magi_tool_runtime::{ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry};
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
    RecoveryResume {
        input: magi_core::RecoveryResumeInput,
        worker_id: WorkerId,
        writebacks: ExecutionWritebackPlans,
    },
}

#[derive(Clone, Debug)]
pub enum ShadowTaskExecutionResult {
    RecoveryResume {
        result: RecoveryExecutionResult,
        memory_writeback_applied: bool,
    },
    Failed {
        error: String,
    },
}

pub struct ShadowGraphDriveResult {
    pub runner_started: bool,
    pub action_status: TaskStatus,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
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
pub struct RecoveryResumeSubmissionAccepted {
    pub result: RecoveryExecutionResult,
    pub resumed_at: UtcMillis,
    pub memory_writeback_applied: bool,
}

enum ShadowTaskSubmission {
    Dispatch(DispatchSubmissionRequest),
    RecoveryResume {
        request: RecoveryResumeRequestDto,
        resumed_at: UtcMillis,
    },
}

enum ShadowTaskSubmissionAcceptedKind {
    Dispatch(DispatchSubmissionAccepted),
    RecoveryResume(RecoveryResumeSubmissionAccepted),
}

#[derive(Clone, Default)]
pub struct ShadowTaskExecutionRegistry {
    plans: Arc<RwLock<HashMap<TaskId, ShadowTaskExecutionPlan>>>,
    results: Arc<RwLock<HashMap<TaskId, ShadowTaskExecutionResult>>>,
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

    pub fn store_result(&self, task_id: TaskId, result: ShadowTaskExecutionResult) {
        self.results
            .write()
            .expect("shadow task execution result registry write lock poisoned")
            .insert(task_id, result);
    }

    pub fn take_result(&self, task_id: &TaskId) -> Option<ShadowTaskExecutionResult> {
        self.results
            .write()
            .expect("shadow task execution result registry write lock poisoned")
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
    ) {
        let event = EventEnvelope::domain(
            EventId::new(format!("event-task-dispatched-{}", UtcMillis::now().0)),
            "task.dispatched",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": mission_id.to_string(),
                "worker_id": worker.worker_id.to_string(),
                "role": worker.role,
                "lease_id": lease_id.to_string(),
                "kind": format!("{:?}", kind),
            }),
        )
        .with_context(EventContext {
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
                .bind_execution_ownership(session_id, ownership);
            writebacks.apply(&self.pipeline.memory_store);
            self.publish_execution_overview(task, context_summary);
        }
        self.push_result(task_id, lease_id, outcome);
    }

    fn publish_execution_overview(
        &self,
        task: &magi_core::Task,
        context_summary: Option<MissionContextSummary>,
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
                "context": context_payload,
            }),
        )
        .with_context(EventContext {
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn execute_recovery_resume_plan(
        &self,
        task_id: &TaskId,
        lease_id: &LeaseId,
        input: magi_core::RecoveryResumeInput,
        worker_id: WorkerId,
        writebacks: ExecutionWritebackPlans,
    ) {
        let memory_writeback_applied = !writebacks.is_empty();
        let outcome = match self
            .pipeline
            .execute_recovery_with_writebacks(input, worker_id, writebacks)
        {
            Ok(result) => {
                self.execution_registry.store_result(
                    task_id.clone(),
                    ShadowTaskExecutionResult::RecoveryResume {
                        result,
                        memory_writeback_applied,
                    },
                );
                TaskOutcome::Completed {
                    output_refs: Vec::new(),
                }
            }
            Err(error) => {
                let error = format!("{error:?}");
                self.execution_registry.store_result(
                    task_id.clone(),
                    ShadowTaskExecutionResult::Failed {
                        error: error.clone(),
                    },
                );
                TaskOutcome::Failed { error }
            }
        };
        self.push_result(task_id, lease_id, outcome);
    }

    fn build_tool_definitions(&self) -> Vec<ChatToolDefinition> {
        let Some(ref registry) = self.tool_registry else {
            return Vec::new();
        };
        registry
            .builtin_specs()
            .into_iter()
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
    ) -> (String, Option<MissionContextSummary>) {
        let base_prompt = if task.goal.is_empty() {
            task.title.clone()
        } else {
            format!("{}\n\n{}", task.title, task.goal)
        };

        let Some(ref ctx_runtime) = self.context_runtime else {
            return (base_prompt, None);
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

        let context_summary = MissionContextSummary::from_context_assembly(&result);

        if !has_context {
            return (base_prompt, Some(context_summary));
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
            format!("--- Context ---\n{ctx_text}\n--- Task ---\n{base_prompt}"),
            Some(context_summary),
        )
    }

    fn execute_tool_call(&self, tool_call: &ChatToolCall, task: &magi_core::Task) -> String {
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
                    "tool_name": tool_call.function.name,
                    "tool_call_id": tool_call.id,
                }),
            )
            .with_context(EventContext {
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(task.task_id.clone()),
            session_id: self
                .session_store
                .current_session()
                .map(|s| s.session_id),
            workspace_id: None,
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
    ) -> (TaskOutcome, Option<MissionContextSummary>) {
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
            let tool_defs = self.build_tool_definitions();
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
                    "prompt_length": prompt.len(),
                }),
            )
            .with_context(task_context.clone()),
        );

        let mut final_content = String::new();
        let mut tool_call_records: Vec<serde_json::Value> = Vec::new();

        for round in 0..MAX_TOOL_CALL_ROUNDS {
            let request = ModelInvocationRequest {
                provider: "default".to_string(),
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
                let result = self.execute_tool_call(tc, task);
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

impl TaskDispatcher for ShadowTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
    ) -> Result<(), String> {
        self.publish_task_dispatched_event(
            &task.task_id,
            &task.mission_id,
            worker,
            &lease.lease_id,
            task.kind,
        );

        let Some(plan) = self.execution_registry.remove(&task.task_id) else {
            let session_id = self
                .session_store
                .current_session()
                .map(|s| s.session_id)
                .unwrap_or_else(|| SessionId::new("default"));
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
            ShadowTaskExecutionPlan::RecoveryResume {
                input,
                worker_id,
                writebacks,
            } => {
                self.execute_recovery_resume_plan(
                    &task.task_id,
                    &lease.lease_id,
                    input,
                    worker_id,
                    writebacks,
                );
            }
        }

        Ok(())
    }
}

fn submit_shadow_task_submission(
    state: &ApiState,
    submission: ShadowTaskSubmission,
) -> Result<ShadowTaskSubmissionAcceptedKind, ApiError> {
    match submission {
        ShadowTaskSubmission::Dispatch(request) => {
            let graph = run_shadow_dispatch_submission(state, &request)?;
            let execution = drive_shadow_task_graph(
                state,
                &graph.root_task_id,
                &graph.action_task_id,
                "执行 shadow dispatch 失败",
            )?;

            Ok(ShadowTaskSubmissionAcceptedKind::Dispatch(
                DispatchSubmissionAccepted {
                    session_id: request.session_id,
                    entry_id: request.entry_id,
                    accepted_at: request.accepted_at,
                    created_session: request.created_session,
                    root_task_id: graph.root_task_id,
                    action_task_id: graph.action_task_id,
                    runner_started: execution.runner_started,
                },
            ))
        }
        ShadowTaskSubmission::RecoveryResume {
            request,
            resumed_at,
        } => {
            let graph = run_shadow_recovery_resume(state, &request)?;
            let execution = drive_shadow_task_graph(
                state,
                &graph.root_task_id,
                &graph.action_task_id,
                "执行 shadow recovery 失败",
            )?;
            let (result, memory_writeback_applied) =
                take_recovery_resume_result(state, &graph.action_task_id, execution.action_status)?;

            Ok(ShadowTaskSubmissionAcceptedKind::RecoveryResume(
                RecoveryResumeSubmissionAccepted {
                    result,
                    resumed_at,
                    memory_writeback_applied,
                },
            ))
        }
    }
}

pub fn submit_shadow_dispatch_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    match submit_shadow_task_submission(state, ShadowTaskSubmission::Dispatch(request))? {
        ShadowTaskSubmissionAcceptedKind::Dispatch(accepted) => Ok(accepted),
        ShadowTaskSubmissionAcceptedKind::RecoveryResume(_) => Err(ApiError::internal_assembly(
            "执行 shadow dispatch 失败",
            "unexpected recovery completion for dispatch submission",
        )),
    }
}

pub fn submit_shadow_recovery_resume_submission(
    state: &ApiState,
    request: &RecoveryResumeRequestDto,
    resumed_at: UtcMillis,
) -> Result<RecoveryResumeSubmissionAccepted, ApiError> {
    match submit_shadow_task_submission(
        state,
        ShadowTaskSubmission::RecoveryResume {
            request: request.clone(),
            resumed_at,
        },
    )? {
        ShadowTaskSubmissionAcceptedKind::RecoveryResume(accepted) => Ok(accepted),
        ShadowTaskSubmissionAcceptedKind::Dispatch(_) => Err(ApiError::internal_assembly(
            "执行 shadow recovery 失败",
            "unexpected dispatch completion for recovery submission",
        )),
    }
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
        action_status,
    })
}

pub fn take_recovery_resume_result(
    state: &ApiState,
    action_task_id: &TaskId,
    action_status: TaskStatus,
) -> Result<(RecoveryExecutionResult, bool), ApiError> {
    let completion = state
        .shadow_task_execution_registry()
        .take_result(action_task_id)
        .ok_or_else(|| {
            ApiError::internal_assembly("执行 shadow recovery 失败", "missing recovery task result")
        })?;

    match completion {
        ShadowTaskExecutionResult::RecoveryResume {
            result,
            memory_writeback_applied,
        } if action_status == TaskStatus::Completed => Ok((result, memory_writeback_applied)),
        ShadowTaskExecutionResult::Failed { error } if action_status == TaskStatus::Failed => {
            Err(ApiError::internal_assembly("执行 shadow recovery 失败", error))
        }
        ShadowTaskExecutionResult::RecoveryResume { .. } => Err(ApiError::internal_assembly(
            "执行 shadow recovery 失败",
            format!(
                "recovery task completed with unexpected status: {:?}",
                action_status
            ),
        )),
        ShadowTaskExecutionResult::Failed { error } => Err(ApiError::internal_assembly(
            "执行 shadow recovery 失败",
            format!(
                "recovery task failed with unexpected status {:?}: {}",
                action_status, error
            ),
        )),
    }
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
