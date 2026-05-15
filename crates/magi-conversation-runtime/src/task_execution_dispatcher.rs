//! Task System v2 — LLM task dispatcher runtime.
//!
//! Owns the production task dispatch implementation for session turns and task LLM loops.

use crate::{
    ConversationRegistry, SKILL_APPLY_TOOL_NAME, StreamFanOut,
    model_config::NormalizedModelConfig,
    prompt_utils::prepend_session_instructions,
    public_builtin_tool_definitions,
    session_turn_execution::{
        SessionTurnExecutionOutput, SessionTurnExecutionRequest, SessionTurnExecutionRuntime,
        run_session_turn_execution,
    },
    session_turn_finalize::{
        compact_task_context_text, format_dependency_task_context, format_task_ref_list,
    },
    settings_store::SettingsStore,
    skill_apply_tool_definition,
    task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry},
    task_llm_loop::{self, TaskLlmLoopRequest},
    task_runner_bridge::{EventBasedResultReceiver, TaskDispatcher, TaskOutcome, TaskResult},
    usage_recording::{ModelUsageBinding, model_usage_binding_for_worker},
};
use magi_bridge_client::{ChatToolDefinition, ModelBridgeClient};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionOwnership, ExecutionResultStatus, LeaseId, RiskLevel,
    SessionId, TaskId, TaskKind, ToolCallId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_knowledge_store::{KnowledgeKind, KnowledgeRecord, KnowledgeStore};
use magi_memory_store::MemoryStore;
use magi_orchestrator::{
    ExecutionContextSummary, ExecutionWritebackPlans, OrchestratedExecutionRuntime,
    OrchestratorService, task_worker_catalog::WorkerInfo,
};
use magi_session_store::{SessionStore, TimelineEntryKind, timeline_entry_visible_text};
use magi_tool_runtime::{BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolRegistry};
use magi_workspace::WorkspaceStore;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct ExecutionPipeline {
    pub orchestrator: OrchestratorService,
    pub execution_runtime: OrchestratedExecutionRuntime,
    pub memory_store: MemoryStore,
}

#[derive(Clone)]
pub struct LlmTaskDispatcher {
    event_bus: Arc<InMemoryEventBus>,
    pipeline: ExecutionPipeline,
    session_store: Arc<SessionStore>,
    execution_registry: TaskExecutionRegistry,
    result_receiver: Arc<EventBasedResultReceiver>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    knowledge_store: Option<Arc<KnowledgeStore>>,
    knowledge_persist_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    settings_store: Option<Arc<SettingsStore>>,
    context_runtime: Option<Arc<ContextRuntime>>,
    workspace_registry: Option<Arc<WorkspaceStore>>,
    tool_registry: Option<ToolRegistry>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    snapshot_manager: Option<Arc<magi_snapshot::SnapshotManager>>,
    /// Task System v2：Conversation 注册中心，承载 Turn 状态机与单 Conversation 不并发不变式。
    conversation_registry: Option<Arc<ConversationRegistry>>,
    /// Task System v2：统一 StreamEvent 派生通道。
    stream_fanout: Option<Arc<StreamFanOut>>,
    /// Task System v2：AgentRole 注册表（来自 ApiState，注入到 task_llm_loop）。
    agent_role_registry: Option<Arc<magi_agent_role::AgentRoleRegistry>>,
    /// Task System v2 — L5：父子任务拓扑图。S7 协调器三件套（agent_spawn / send_message /
    /// task_stop）需要在 task_llm_loop 中读写。设计为构造期必填，避免运行期再做空检查。
    spawn_graph: Arc<std::sync::Mutex<magi_spawn_graph::SpawnGraph>>,
    /// Task System v2 — L13：session 维度的 TodoLedger 索引。S9 中模型通过
    /// `todo_write` 工具往这里写分解 + 进度；下一轮 Turn 起始时把快照注入 system prompt。
    todo_ledger_registry: Arc<magi_todo_ledger::TodoLedgerRegistry>,
    /// Task System v2 — L14：workspace 维度的 ProjectMemory 索引。S10 中模型通过
    /// `memory_write` 工具新增/删除项目记忆条目；每次 Turn 起始把 MEMORY.md 视图注入
    /// system prompt，跨 conversation 复用。
    project_memory_registry: Arc<magi_project_memory::ProjectMemoryRegistry>,
    /// Task System v2 — Tier 4 / L11：workspace 维度的 MissionCharter 索引。S11 中模型
    /// 通过 `mission_charter_write` 工具增量更新 mission 宪章；每次 Turn 起始把当前
    /// mission 的 charter 注入 system prompt，跨 conversation 锚定目标契约。
    mission_charter_registry: Arc<magi_mission_charter::MissionCharterRegistry>,
    /// Task System v2 — Tier 4 / L12：workspace 维度的 Plan 索引。S12 中模型通过
    /// `plan_write` 工具整体替换 mission.plan.steps；每次 Turn 起始把当前 plan
    /// 注入 system prompt，长链路推进时保留计划上下文。
    plan_registry: Arc<magi_plan::PlanRegistry>,
    /// Task System v2 — Tier 4 / L13：workspace 维度的 MissionWorkspace 索引。S13
    /// 中每个 Mission 拥有独占的 artifacts/logs/memory 目录骨架；Turn 起始时把目录
    /// 路径注入 system prompt，让 agent 把产物落在 mission 内而不是无主目录。
    mission_workspace_registry: Arc<magi_mission_workspace::MissionWorkspaceRegistry>,
    /// Task System v2 — Tier 4 / L18：workspace 维度的 KnowledgeGraph 索引。S14
    /// 中每个 Mission 累积"已知事实"（symbols / decisions / risks）；Turn 起始时把
    /// live facts 注入 system prompt，避免长 mission 中模型重新讨论已经达成的结论。
    knowledge_graph_registry: Arc<magi_knowledge_graph::KnowledgeGraphRegistry>,
    /// Task System v2 — Tier 4 / L19：workspace 维度的 ValidationRunner 索引。S15
    /// 中每个 Mission 在 Plan 节点上挂载验证记录（test_suite / type_check /
    /// integration_smoke / benchmark）；Coordinator 判定 Plan 节点完成的硬门槛
    /// 是：至少 1 条 Pass，且当前无 Fail。
    validation_runner_registry: Arc<magi_validation_runner::ValidationRunnerRegistry>,
    /// Task System v2 — Tier 4 / L20：workspace 维度的 Checkpoint 索引。S16 中每个
    /// Mission 维护一份 append-only 的检查点日志（process_restart / context_compaction
    /// / phase_transition / manual），让事后能定位到“恢复到 Tn”所需要的最小语义快照。
    checkpoint_registry: Arc<magi_checkpoint::CheckpointRegistry>,
    /// Task System v2 — Tier 4 / L21：workspace 维度的 HumanCheckpoint 索引。S17 中
    /// orchestrator 通过 human_checkpoint_request 申请人工审核点，mission 会进入
    /// awaiting_human 状态，operator 审批前 Coordinator 不再派发新工作。
    human_checkpoint_registry: Arc<magi_human_checkpoint::HumanCheckpointRegistry>,
    /// 强制同步执行 dispatch，用于普通模式的同步 for 循环（设计 §1.3）。
    force_sync_dispatch: Arc<std::sync::atomic::AtomicUsize>,
}

pub fn resolve_configured_model_client(
    settings_store: Option<&Arc<SettingsStore>>,
    fallback: Option<Arc<dyn ModelBridgeClient>>,
) -> Option<Arc<dyn ModelBridgeClient>> {
    if let Some(store) = settings_store {
        let config = store.get_section("auxiliary");
        let normalized = NormalizedModelConfig::from_settings_value(&config, "openai");
        if let Some(client) = normalized.to_http_model_client("gpt-4") {
            return Some(Arc::new(client));
        }
    }
    fallback
}

impl LlmTaskDispatcher {
    pub fn new(
        event_bus: Arc<InMemoryEventBus>,
        pipeline: ExecutionPipeline,
        session_store: Arc<SessionStore>,
        execution_registry: TaskExecutionRegistry,
        result_receiver: Arc<EventBasedResultReceiver>,
        spawn_graph: Arc<std::sync::Mutex<magi_spawn_graph::SpawnGraph>>,
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
            workspace_registry: None,
            tool_registry: None,
            skill_runtime: None,
            snapshot_manager: None,
            conversation_registry: None,
            stream_fanout: None,
            agent_role_registry: None,
            spawn_graph,
            todo_ledger_registry: Arc::new(magi_todo_ledger::TodoLedgerRegistry::new()),
            project_memory_registry: Arc::new(magi_project_memory::ProjectMemoryRegistry::new()),
            mission_charter_registry: Arc::new(magi_mission_charter::MissionCharterRegistry::new()),
            plan_registry: Arc::new(magi_plan::PlanRegistry::new()),
            mission_workspace_registry: Arc::new(
                magi_mission_workspace::MissionWorkspaceRegistry::new(),
            ),
            knowledge_graph_registry: Arc::new(magi_knowledge_graph::KnowledgeGraphRegistry::new()),
            validation_runner_registry: Arc::new(
                magi_validation_runner::ValidationRunnerRegistry::new(),
            ),
            checkpoint_registry: Arc::new(magi_checkpoint::CheckpointRegistry::new()),
            human_checkpoint_registry: Arc::new(
                magi_human_checkpoint::HumanCheckpointRegistry::new(),
            ),
            force_sync_dispatch: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn set_force_sync_dispatch(&self, force: bool) {
        if force {
            self.force_sync_dispatch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return;
        }
        let _ = self.force_sync_dispatch.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |current| Some(current.saturating_sub(1)),
        );
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

    pub fn with_settings_store(mut self, store: Arc<SettingsStore>) -> Self {
        self.settings_store = Some(store);
        self
    }

    pub fn with_context_runtime(mut self, runtime: Arc<ContextRuntime>) -> Self {
        self.context_runtime = Some(runtime);
        self
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceStore>) -> Self {
        self.workspace_registry = Some(registry);
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

    pub fn with_snapshot_manager(mut self, manager: Arc<magi_snapshot::SnapshotManager>) -> Self {
        self.snapshot_manager = Some(manager);
        self
    }

    pub fn with_conversation_registry(mut self, registry: Arc<ConversationRegistry>) -> Self {
        self.conversation_registry = Some(registry);
        self
    }

    pub fn with_stream_fanout(mut self, fanout: Arc<StreamFanOut>) -> Self {
        self.stream_fanout = Some(fanout);
        self
    }

    pub fn with_agent_role_registry(
        mut self,
        registry: Arc<magi_agent_role::AgentRoleRegistry>,
    ) -> Self {
        self.agent_role_registry = Some(registry);
        self
    }

    pub fn with_todo_ledger_registry(
        mut self,
        registry: Arc<magi_todo_ledger::TodoLedgerRegistry>,
    ) -> Self {
        self.todo_ledger_registry = registry;
        self
    }

    pub fn todo_ledger_registry(&self) -> Arc<magi_todo_ledger::TodoLedgerRegistry> {
        self.todo_ledger_registry.clone()
    }

    pub fn with_project_memory_registry(
        mut self,
        registry: Arc<magi_project_memory::ProjectMemoryRegistry>,
    ) -> Self {
        self.project_memory_registry = registry;
        self
    }

    pub fn project_memory_registry(&self) -> Arc<magi_project_memory::ProjectMemoryRegistry> {
        self.project_memory_registry.clone()
    }

    pub fn with_mission_charter_registry(
        mut self,
        registry: Arc<magi_mission_charter::MissionCharterRegistry>,
    ) -> Self {
        self.mission_charter_registry = registry;
        self
    }

    pub fn mission_charter_registry(&self) -> Arc<magi_mission_charter::MissionCharterRegistry> {
        self.mission_charter_registry.clone()
    }

    pub fn with_plan_registry(mut self, registry: Arc<magi_plan::PlanRegistry>) -> Self {
        self.plan_registry = registry;
        self
    }

    pub fn plan_registry(&self) -> Arc<magi_plan::PlanRegistry> {
        self.plan_registry.clone()
    }

    pub fn with_mission_workspace_registry(
        mut self,
        registry: Arc<magi_mission_workspace::MissionWorkspaceRegistry>,
    ) -> Self {
        self.mission_workspace_registry = registry;
        self
    }

    pub fn mission_workspace_registry(
        &self,
    ) -> Arc<magi_mission_workspace::MissionWorkspaceRegistry> {
        self.mission_workspace_registry.clone()
    }

    pub fn with_knowledge_graph_registry(
        mut self,
        registry: Arc<magi_knowledge_graph::KnowledgeGraphRegistry>,
    ) -> Self {
        self.knowledge_graph_registry = registry;
        self
    }

    pub fn knowledge_graph_registry(&self) -> Arc<magi_knowledge_graph::KnowledgeGraphRegistry> {
        self.knowledge_graph_registry.clone()
    }

    pub fn with_validation_runner_registry(
        mut self,
        registry: Arc<magi_validation_runner::ValidationRunnerRegistry>,
    ) -> Self {
        self.validation_runner_registry = registry;
        self
    }

    pub fn validation_runner_registry(
        &self,
    ) -> Arc<magi_validation_runner::ValidationRunnerRegistry> {
        self.validation_runner_registry.clone()
    }

    pub fn with_checkpoint_registry(
        mut self,
        registry: Arc<magi_checkpoint::CheckpointRegistry>,
    ) -> Self {
        self.checkpoint_registry = registry;
        self
    }

    pub fn checkpoint_registry(&self) -> Arc<magi_checkpoint::CheckpointRegistry> {
        self.checkpoint_registry.clone()
    }

    pub fn with_human_checkpoint_registry(
        mut self,
        registry: Arc<magi_human_checkpoint::HumanCheckpointRegistry>,
    ) -> Self {
        self.human_checkpoint_registry = registry;
        self
    }

    pub fn human_checkpoint_registry(&self) -> Arc<magi_human_checkpoint::HumanCheckpointRegistry> {
        self.human_checkpoint_registry.clone()
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

    /// S7-D：LocalBash 变体直接走 ShellExec，绕过 LLM 循环 / agent role / prompt 组装。
    /// 失败原因有两类：tool_registry 缺失（架构破坏，应 panic 一致行为）或
    /// shell 退出非零（作为 TaskOutcome::Failed 上报，留 payload 给主线核查）。
    fn execute_local_bash_variant(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        command: &str,
        working_dir: Option<&str>,
        worker_id: Option<&WorkerId>,
    ) -> TaskOutcome {
        let Some(registry) = self.tool_registry.as_ref() else {
            return TaskOutcome::Failed {
                error: format!(
                    "LocalBash task {} 无法执行：ToolRegistry 未配置",
                    task.task_id
                ),
            };
        };
        let mut payload = serde_json::json!({ "command": command });
        if let Some(dir) = working_dir {
            payload["working_dir"] = serde_json::Value::String(dir.to_string());
        }
        let input = ToolExecutionInput {
            tool_call_id: ToolCallId::new(format!("local-bash-{}", task.task_id)),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: payload.to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Medium,
        };
        let context = ToolExecutionContext {
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            working_directory: working_dir.map(PathBuf::from),
        };
        let output = registry.execute_with_context(input, context);
        match output.status {
            ExecutionResultStatus::Succeeded => TaskOutcome::Completed {
                output_refs: vec![output.payload],
            },
            other => TaskOutcome::Failed {
                error: format!(
                    "LocalBash task {} shell_exec 失败 (status={:?})：{}",
                    task.task_id, other, output.payload
                ),
            },
        }
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
        worker_lane_id: Option<String>,
        worker_lane_seq: Option<usize>,
        worker_id: WorkerId,
        thread_id: magi_core::ThreadId,
        system_prompt: Option<String>,
    ) {
        // 仅在有 writebacks 时（即主 action task）才生成 streaming entry_id。
        // sub-task 的 writebacks 为空，不需要在 timeline 中创建流式条目。
        let streaming_entry_id = if writebacks.is_empty() {
            None
        } else {
            Some(format!("timeline-streaming-{}", task.task_id))
        };
        // S7-D：LocalBash 变体直接走 ShellExec，绕过 LLM 循环。
        if let magi_core::TaskVariant::LocalBash {
            command,
            working_dir,
        } = &task.variant
        {
            let outcome = self.execute_local_bash_variant(
                task,
                &session_id,
                &workspace_id,
                command,
                working_dir.as_deref(),
                Some(&worker_id),
            );
            if matches!(&outcome, TaskOutcome::Completed { .. }) {
                self.session_store
                    .bind_execution_ownership(session_id.clone(), ownership);
                writebacks.apply(&self.pipeline.memory_store);
            }
            self.push_result(task_id, lease_id, outcome);
            return;
        }
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
            worker_lane_id.as_deref(),
            worker_lane_seq,
            Some(&worker_id),
            &thread_id,
            system_prompt,
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
            .filter_map(|entry| timeline_entry_visible_text(&entry.message))
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
            if knowledge_duplicate(
                &existing,
                KnowledgeKind::Learning,
                workspace_id.as_ref(),
                &learning.content,
            ) {
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

    fn build_tool_definitions(&self, task: Option<&magi_core::Task>) -> Vec<ChatToolDefinition> {
        let Some(ref registry) = self.tool_registry else {
            return Vec::new();
        };
        if task
            .and_then(|task| task.policy_snapshot.as_ref())
            .is_some_and(|policy| policy.command_mode.eq_ignore_ascii_case("no_tools"))
        {
            return Vec::new();
        }
        let registry = if let Some(policy) = task.and_then(|task| task.policy_snapshot.as_ref()) {
            registry.filtered_clone(&policy.allowed_tools, &policy.denied_tools)
        } else {
            registry.clone()
        };
        let mut definitions = public_builtin_tool_definitions(&registry)
            .into_iter()
            .filter(|definition| definition.function.name != SKILL_APPLY_TOOL_NAME)
            .collect::<Vec<_>>();
        if self.skill_runtime.is_some() {
            definitions.push(skill_apply_tool_definition());
        }
        definitions
    }

    fn resolve_workspace_root_path(&self, workspace_id: &Option<WorkspaceId>) -> Option<PathBuf> {
        let workspace_id = workspace_id.as_ref()?;
        self.workspace_registry
            .as_ref()?
            .workspaces()
            .into_iter()
            .find(|workspace| workspace.workspace_id == *workspace_id)
            .map(|workspace| PathBuf::from(workspace.root_path.as_str()))
    }

    fn task_fact_context_parts(&self, task: &magi_core::Task) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(scope) = task
            .workspace_scope
            .as_deref()
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
        {
            parts.push(format!("[task-workspace] {scope}"));
        }
        if !task.input_refs.is_empty() {
            parts.push(format!(
                "[task-input] {}",
                format_task_ref_list(&task.input_refs)
            ));
        }

        let task_store = self.pipeline.execution_runtime.task_store();
        for dependency_id in &task.dependency_ids {
            if let Some(dependency) = task_store.get_task(dependency_id) {
                parts.push(format_dependency_task_context(&dependency));
            } else {
                parts.push(format!("[dependency] id={dependency_id} status=missing"));
            }
        }
        if parts.is_empty() && task.kind != TaskKind::Validation {
            return parts;
        }
        parts.insert(
            0,
            "[current-task-rule] 当前任务标题、目标、input_refs、依赖任务输出和 task-context 是本次执行的主事实；knowledge/memory 只能补充，不能改写当前任务目标。目标中的路径、工具名、命令、标记字符串以及“必须/要求”条款必须逐项执行或明确说明无法执行的真实原因，不能替换成历史任务或泛化检查。"
                .to_string(),
        );
        if task.kind == TaskKind::Validation {
            parts.insert(
                1,
                "[validation-rule] 只验证本任务 dependency/input 指向的当前执行产出；不得把历史经验、知识库记录或其他会话目标当成本次交付对象。"
                    .to_string(),
            );
        }
        parts
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
        let task_fact_context_parts = self.task_fact_context_parts(task);

        let Some(ref ctx_runtime) = self.context_runtime else {
            if task_fact_context_parts.is_empty() {
                return (
                    prepend_session_instructions(
                        user_rules_prefix.as_deref(),
                        safeguard_prefix.as_deref(),
                        &base_prompt,
                    ),
                    None,
                );
            }
            let ctx_text = task_fact_context_parts.join("\n");
            return (
                prepend_session_instructions(
                    user_rules_prefix.as_deref(),
                    safeguard_prefix.as_deref(),
                    &format!("--- Context ---\n{ctx_text}\n--- Task ---\n{base_prompt}"),
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
        let task_context_entries = self
            .pipeline
            .execution_runtime
            .task_store()
            .context_entries_for_refs(&task.context_refs);
        let has_context = !result.selected_knowledge.is_empty()
            || !result.selected_memory.is_empty()
            || !result.selected_shared_context.is_empty()
            || !task_fact_context_parts.is_empty()
            || !task_context_entries.is_empty();

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
        ctx_parts.extend(task_fact_context_parts);
        for entry in &task_context_entries {
            ctx_parts.push(format!(
                "[task-context] {}: {}",
                entry.context_ref,
                compact_task_context_text(&entry.content)
            ));
        }
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
        // S8：单一事实源 —— 危险模式集合从 SafetyGate 派生，确保 prompt 文案与
        // 运行期 enforcement 共用同一份规则。
        let gate = self.build_safety_gate()?;
        let patterns = gate
            .rules()
            .iter()
            .filter(|rule| rule.enabled)
            .map(|rule| rule.pattern.trim())
            .filter(|pattern| !pattern.is_empty())
            .collect::<Vec<_>>();
        if patterns.is_empty() {
            return None;
        }
        Some(format!(
            "执行 shell / git / 文件写操作前，如果命中以下危险模式，必须先向用户确认，不得直接执行（违规调用会被 SafetyGate 在运行期直接拦截）：\n{}",
            patterns
                .iter()
                .map(|pattern| format!("- {}", pattern))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    /// S8：依据当前 settings 快照构造 SafetyGate。
    /// 调用者每次进入 LLM 轮次循环前都构造一次；引擎本身无状态，可在该轮次内共享。
    pub(crate) fn build_safety_gate(&self) -> Option<magi_safety_gate::SafetyGate> {
        let store = self.settings_store.as_ref()?;
        let raw = store.get_section("safeguardConfig");
        let rules = raw
            .get("rules")
            .map(magi_safety_gate::rules_from_settings_value)
            .unwrap_or_default();
        if rules.is_empty() {
            None
        } else {
            Some(magi_safety_gate::SafetyGate::new(rules))
        }
    }

    fn resolve_model_client(&self) -> Option<Arc<dyn ModelBridgeClient>> {
        resolve_configured_model_client(
            self.settings_store.as_ref(),
            self.model_bridge_client.clone(),
        )
    }

    fn apply_skill_prompt_injections(
        &self,
        mut prompt: String,
        skill_name: Option<&str>,
    ) -> String {
        let Some(skill_id) = skill_name else {
            return prompt;
        };
        let Some(ref skill_rt) = self.skill_runtime else {
            return prompt;
        };
        let plan = skill_rt.build_tool_runtime_plan(magi_skill_runtime::SkillSelection {
            skill_ids: vec![skill_id.to_string()],
            requested_tools: vec![],
        });
        for injection in plan.prompt_injections {
            prompt = format!("{}\n\n{}", injection.body, prompt);
        }
        prompt
    }

    pub fn execute_session_turn(
        &self,
        request: SessionTurnExecutionRequest,
    ) -> Result<SessionTurnExecutionOutput, String> {
        let Some(client) = self.resolve_model_client() else {
            return Err("model bridge client 未配置".to_string());
        };

        let prompt = self.apply_skill_prompt_injections(
            prepend_session_instructions(
                self.resolve_user_rules_prompt().as_deref(),
                self.resolve_safeguard_prompt().as_deref(),
                &request.prompt,
            ),
            request.skill_name.as_deref(),
        );

        let tools = if request.use_tools {
            let tool_defs = self.build_tool_definitions(None);
            (!tool_defs.is_empty()).then_some(tool_defs)
        } else {
            None
        };
        run_session_turn_execution(SessionTurnExecutionRuntime {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            settings_store: self.settings_store.as_ref(),
            tool_registry: self.tool_registry.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            snapshot_manager: self.snapshot_manager.as_ref(),
            request,
            prompt,
            tools,
        })
        .map_err(|msg| msg)
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
        worker_lane_id: Option<&str>,
        worker_lane_seq: Option<usize>,
        worker_id: Option<&WorkerId>,
        thread_id: &magi_core::ThreadId,
        system_prompt: Option<String>,
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

        let (prompt, context_summary) = self.assemble_prompt(task, session_id, workspace_id);
        let prompt = self.apply_skill_prompt_injections(prompt, skill_name.as_deref());
        let workspace_root_path = self.resolve_workspace_root_path(workspace_id);

        // P7：orchestrator_thread_id 为主线可见性锚点，分派到达时必然已 spawn；缺失即架构破坏。
        let orchestrator_thread_id = self
            .session_store
            .orchestrator_thread_for_session(session_id)
            .map(|thread| thread.thread_id)
            .unwrap_or_else(|| {
                panic!(
                    "session {session_id} missing orchestrator thread when dispatching task {}",
                    task.task_id
                )
            });

        let tools = if use_tools {
            let tool_defs = self.build_tool_definitions(Some(task));
            if tool_defs.is_empty() {
                None
            } else {
                Some(tool_defs)
            }
        } else {
            None
        };

        let conversation_registry = self.conversation_registry.as_ref().expect(
            "LlmTaskDispatcher 缺少 ConversationRegistry，无法走 Task System v2 Turn 状态机",
        );
        let stream_fanout = self
            .stream_fanout
            .as_ref()
            .expect("LlmTaskDispatcher 缺少 StreamFanOut，无法发布 v2 流派生事件");
        let agent_role_registry = self
            .agent_role_registry
            .as_ref()
            .expect("LlmTaskDispatcher 缺少 AgentRoleRegistry，无法解析 task→role");
        let safety_gate = self.build_safety_gate();
        let todo_ledger = self.todo_ledger_registry.get_or_create(session_id);
        let project_memory = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.project_memory_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "ProjectMemory: 打开失败，本次 Turn 不注入项目记忆");
                    None
                }
            }
        });
        let mission_charter = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.mission_charter_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "MissionCharter: 打开失败，本次 Turn 不注入 mission 宪章");
                    None
                }
            }
        });
        let plan = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.plan_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "Plan: 打开失败，本次 Turn 不注入 mission 计划");
                    None
                }
            }
        });
        let mission_workspace = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.mission_workspace_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "MissionWorkspace: 打开失败，本次 Turn 不注入工作目录视图");
                    None
                }
            }
        });
        let knowledge_graph = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.knowledge_graph_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "KnowledgeGraph: 打开失败，本次 Turn 不注入 mission KG");
                    None
                }
            }
        });
        let validation_runner = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.validation_runner_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "ValidationRunner: 打开失败，本次 Turn 不注入验证结果");
                    None
                }
            }
        });
        let checkpoint = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.checkpoint_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "Checkpoint: 打开失败，本次 Turn 不注入检查点日志");
                    None
                }
            }
        });
        let human_checkpoint = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.human_checkpoint_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "HumanCheckpoint: 打开失败，本次 Turn 不注入人工审核点");
                    None
                }
            }
        });
        task_llm_loop::run_task_llm_loop(TaskLlmLoopRequest {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            settings_store: self.settings_store.as_ref(),
            tool_registry: self.tool_registry.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            task_store: self.pipeline.execution_runtime.task_store(),
            conversation_registry: conversation_registry.as_ref(),
            stream_fanout: stream_fanout.as_ref(),
            agent_role_registry: agent_role_registry.as_ref(),
            spawn_graph: self.spawn_graph.as_ref(),
            safety_gate: safety_gate.as_ref(),
            todo_ledger: todo_ledger.as_ref(),
            project_memory: project_memory.as_deref(),
            mission_charter: mission_charter.as_deref(),
            plan: plan.as_deref(),
            mission_workspace: mission_workspace.as_deref(),
            knowledge_graph: knowledge_graph.as_deref(),
            validation_runner: validation_runner.as_deref(),
            checkpoint: checkpoint.as_deref(),
            human_checkpoint: human_checkpoint.as_deref(),
            task,
            task_id,
            lease_id,
            session_id,
            workspace_id,
            prompt,
            tools,
            usage_binding,
            streaming_entry_id,
            worker_lane_id,
            worker_lane_seq,
            worker_id,
            thread_id,
            orchestrator_thread_id: &orchestrator_thread_id,
            context_summary,
            system_prompt,
            workspace_root_path,
        })
    }

    /// Synchronous inner dispatch logic; invoked either directly or inside
    /// `tokio::task::spawn_blocking` so the LLM call does not starve the
    /// async runtime (design §1.3).
    fn dispatch_inner(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_orchestrator::task_store::TaskLease,
    ) -> Result<(), String> {
        let Some(plan) = self.execution_registry.remove(&task.task_id) else {
            let error = format!(
                "任务 {} 缺少结构化执行计划，已拒绝无计划执行路径",
                task.task_id
            );
            tracing::error!(
                task_id = %task.task_id,
                mission_id = %task.mission_id,
                worker_id = %worker.worker_id,
                "task dispatch missing execution plan"
            );
            self.push_result(
                &task.task_id,
                &lease.lease_id,
                TaskOutcome::Failed { error },
            );
            return Ok(());
        };

        match plan {
            TaskExecutionPlan::Dispatch {
                target: _,
                worker_id,
                lane_id,
                lane_seq,
                thread_id,
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
                    lane_id,
                    lane_seq,
                    worker_id,
                    thread_id,
                    worker.system_prompt_template.clone(),
                );
            }
        }

        Ok(())
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

fn knowledge_duplicate(
    existing: &[KnowledgeRecord],
    kind: KnowledgeKind,
    workspace_id: Option<&WorkspaceId>,
    content: &str,
) -> bool {
    let normalized = normalized_text(content);
    existing.iter().any(|record| {
        record.kind == kind && record.workspace_id.as_ref() == workspace_id && {
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

impl TaskDispatcher for LlmTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_orchestrator::task_store::TaskLease,
    ) -> Result<(), String> {
        // 普通模式的同步 for 循环要求 dispatch 同步完成，直接走 inner。
        if self
            .force_sync_dispatch
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
        {
            return self.dispatch_inner(task, worker, lease);
        }

        let dispatcher = self.clone();
        let task = task.clone();
        let worker = worker.clone();
        let lease = lease.clone();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.clone().spawn(async move {
                let result = handle
                    .spawn_blocking(move || {
                        if let Err(err) = dispatcher.dispatch_inner(&task, &worker, &lease) {
                            tracing::error!("dispatch_inner failed: {}", err);
                            dispatcher.push_result(
                                &task.task_id,
                                &lease.lease_id,
                                TaskOutcome::Failed {
                                    error: format!("dispatch failed: {}", err),
                                },
                            );
                        }
                    })
                    .await;
                if let Err(err) = result {
                    tracing::error!("dispatch spawn_blocking panicked: {:?}", err);
                }
            });
            Ok(())
        } else {
            // 不在 tokio 运行时中（例如同步测试环境），直接同步执行。
            self.dispatch_inner(&task, &worker, &lease)
        }
    }
}
