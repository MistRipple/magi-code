use crate::RunnerStartError;
pub use crate::session_turn_execution::{
    BUSINESS_MODEL_PROVIDER, SessionTurnExecutionOutput, SessionTurnExecutionRequest,
};
use crate::{
    builtin_tool_schema::builtin_tool_definition,
    errors::ApiError,
    model_config::NormalizedModelConfig,
    prompt_utils::prepend_session_instructions,
    session_turn_execution::{SessionTurnExecutionRuntime, run_session_turn_execution},
    session_turn_writeback::{
        build_completed_turn_timeline_snapshot, publish_current_session_turn_item_event,
    },
    settings_store::SettingsStore,
    shadow_execution::{
        ShadowTaskGraphSubmission, cleanup_shadow_task_tree, run_shadow_dispatch_submission,
    },
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, skill_apply_tool_definition},
    state::{ApiState, ShadowExecutionPipeline},
    usage_recording::{ModelUsageBinding, model_usage_binding_for_worker},
};
use magi_bridge_client::{ChatToolDefinition, ModelBridgeClient};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_core::{
    DomainError, EventId, ExecutionOwnership, LeaseId, SessionId, TaskExecutionTarget, TaskId,
    TaskStatus, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_knowledge_store::{KnowledgeKind, KnowledgeRecord, KnowledgeStore};
use magi_orchestrator::{
    ExecutionContextSummary, ExecutionWritebackPlans,
    task_runner::{EventBasedResultReceiver, TaskDispatcher, TaskOutcome, TaskResult, WorkerInfo},
};
use magi_session_store::{SessionStore, TimelineEntryKind, timeline_entry_visible_text};
use magi_tool_runtime::ToolRegistry;
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

#[derive(Clone, Debug)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub entry_id: String,
    pub timeline_message: String,
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub deep_task: bool,
    pub skill_name: Option<String>,
    pub target_role: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
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

pub fn finalize_background_session_task_turn_if_root_completed(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> bool {
    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return false;
    };
    if root_task.status != TaskStatus::Completed {
        return false;
    }

    let Some(sidecar) = state.session_store.runtime_sidecar(session_id) else {
        return false;
    };
    let Some(active_chain) = sidecar.active_execution_chain.as_ref() else {
        return false;
    };
    if active_chain.root_task_id != *root_task_id {
        return false;
    }
    let workspace_id = active_chain.workspace_id.clone();
    let Some(turn) = sidecar.current_turn.as_ref() else {
        return false;
    };
    let Some((response_text, streaming_entry_id)) = turn
        .items
        .iter()
        .filter(|item| item.kind == "assistant_final" && item.thread_visible)
        .filter_map(|item| {
            let content = item.content.as_ref()?.trim();
            if content.is_empty() {
                return None;
            }
            let entry_id = item
                .timeline_entry_id
                .as_deref()
                .filter(|entry_id| !entry_id.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    item.item_id
                        .starts_with("timeline-")
                        .then(|| item.item_id.clone())
                })
                .unwrap_or_else(|| format!("timeline-turn-snapshot-{}", root_task_id));
            Some((item.item_seq, content.to_string(), entry_id))
        })
        .max_by_key(|(item_seq, _, _)| *item_seq)
        .map(|(_, content, entry_id)| (content, entry_id))
    else {
        return false;
    };

    if state
        .session_store
        .update_current_turn_status(session_id, "completed")
        .is_err()
    {
        return false;
    }
    let timeline_message = build_completed_turn_timeline_snapshot(
        state.session_store.as_ref(),
        session_id,
        Some(&response_text),
        Some(&streaming_entry_id),
        state.task_store(),
    )
    .unwrap_or_else(|| response_text.clone());
    state.session_store.upsert_timeline_entry(
        session_id.clone(),
        &streaming_entry_id,
        TimelineEntryKind::AssistantMessage,
        timeline_message,
    );
    publish_current_session_turn_item_event(
        &state.event_bus,
        state.session_store.as_ref(),
        session_id,
        &workspace_id,
        &streaming_entry_id,
        state.task_store(),
    );
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-message-assistant-{}", UtcMillis::now().0)),
            "message.created",
            serde_json::json!({
                "session_id": session_id.to_string(),
                "role": "assistant",
                "content": response_text,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
    true
}

#[derive(Clone, Default)]
pub struct ShadowTaskExecutionRegistry {
    plans: Arc<RwLock<HashMap<TaskId, ShadowTaskExecutionPlan>>>,
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

#[derive(Clone)]
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
    /// 强制同步执行 dispatch，用于普通模式的同步 for 循环（设计 §1.3）。
    force_sync_dispatch: Arc<std::sync::atomic::AtomicBool>,
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
            force_sync_dispatch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn set_force_sync_dispatch(&self, force: bool) {
        self.force_sync_dispatch
            .store(force, std::sync::atomic::Ordering::Relaxed);
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
        worker_lane_id: Option<String>,
        worker_lane_seq: Option<usize>,
        worker_id: WorkerId,
        system_prompt: Option<String>,
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
            worker_lane_id.as_deref(),
            worker_lane_seq,
            Some(&worker_id),
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

    fn build_tool_definitions(&self) -> Vec<ChatToolDefinition> {
        let Some(ref registry) = self.tool_registry else {
            return Vec::new();
        };
        let mut definitions = registry
            .builtin_specs()
            .into_iter()
            .filter(|spec| spec.name != SKILL_APPLY_TOOL_NAME)
            .map(|spec| builtin_tool_definition(&spec.name))
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
        let task_context_entries = self
            .pipeline
            .execution_runtime
            .task_store()
            .context_entries_for_refs(&task.context_refs);
        let has_context = !result.selected_knowledge.is_empty()
            || !result.selected_memory.is_empty()
            || !result.selected_shared_context.is_empty()
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
        for item in &result.selected_knowledge {
            ctx_parts.push(format!("[knowledge] {}: {}", item.title, item.excerpt));
        }
        for item in &result.selected_memory {
            ctx_parts.push(format!("[memory] {}", item.content));
        }
        for item in &result.selected_shared_context {
            ctx_parts.push(format!("[context] {}: {}", item.title, item.content));
        }
        for entry in &task_context_entries {
            ctx_parts.push(format!(
                "[task-context] {}: {}",
                entry.context_ref, entry.content
            ));
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
    ) -> Result<SessionTurnExecutionOutput, ApiError> {
        let Some(client) = self.resolve_model_client() else {
            return Err(ApiError::internal_assembly(
                "执行 session turn 失败",
                "model bridge client 未配置",
            ));
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
            let tool_defs = self.build_tool_definitions();
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
            request,
            prompt,
            tools,
        })
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

        let tools = if use_tools {
            let tool_defs = self.build_tool_definitions();
            if tool_defs.is_empty() {
                None
            } else {
                Some(tool_defs)
            }
        } else {
            None
        };

        crate::task_llm_loop::run_task_llm_loop(crate::task_llm_loop::TaskLlmLoopRequest {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            settings_store: self.settings_store.as_ref(),
            tool_registry: self.tool_registry.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            task_store: self.pipeline.execution_runtime.task_store(),
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
            context_summary,
            system_prompt,
        })
    }

    /// Synchronous inner dispatch logic; invoked either directly or inside
    /// `tokio::task::spawn_blocking` so the LLM call does not starve the
    /// async runtime (design §1.3).
    fn dispatch_inner(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
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
                "shadow task dispatch missing execution plan"
            );
            self.push_result(
                &task.task_id,
                &lease.lease_id,
                TaskOutcome::Failed { error },
            );
            return Ok(());
        };

        match plan {
            ShadowTaskExecutionPlan::Dispatch {
                target: _,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn learning_record(id: &str, workspace_id: Option<&str>, content: &str) -> KnowledgeRecord {
        KnowledgeRecord {
            knowledge_id: id.to_string(),
            kind: KnowledgeKind::Learning,
            title: content.to_string(),
            content: content.to_string(),
            tags: Vec::new(),
            workspace_id: workspace_id.map(WorkspaceId::new),
            source_ref: None,
            updated_at: UtcMillis::now(),
        }
    }

    #[test]
    fn learning_duplicate_detection_is_workspace_scoped() {
        let content = "最佳实践：同一条经验可以在不同 workspace 分别沉淀";
        let existing = vec![learning_record(
            "learning-workspace-a",
            Some("workspace-a"),
            content,
        )];

        assert!(knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            Some(&WorkspaceId::new("workspace-a")),
            content,
        ));
        assert!(!knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            Some(&WorkspaceId::new("workspace-b")),
            content,
        ));
        assert!(!knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            None,
            content,
        ));
    }
}

impl TaskDispatcher for ShadowTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
    ) -> Result<(), String> {
        // 普通模式的同步 for 循环要求 dispatch 同步完成，直接走 inner。
        if self
            .force_sync_dispatch
            .load(std::sync::atomic::Ordering::Relaxed)
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

fn submit_shadow_task_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    state
        .session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(map_shadow_dispatch_store_error)?;
    let graph = run_shadow_dispatch_submission(state, &request)?;
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = state
            .session_store
            .accept_active_execution_chain_with_timeline_entry(
                request.session_id.clone(),
                request.entry_id.clone(),
                TimelineEntryKind::UserMessage,
                request.timeline_message.clone(),
                request.accepted_at,
                active_execution_chain,
            );
        if let Err(error) = accept_result {
            cleanup_rejected_shadow_dispatch(state, &graph);
            return Err(map_shadow_dispatch_store_error(error));
        }
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

fn cleanup_rejected_shadow_dispatch(state: &ApiState, graph: &ShadowTaskGraphSubmission) {
    if let Some(chain) = graph.active_execution_chain.as_ref() {
        let registry = state.shadow_task_execution_registry();
        for branch in &chain.branches {
            let _ = registry.remove(&branch.task_id);
        }
    }
    if let Some(task_store) = state.task_store() {
        cleanup_shadow_task_tree(task_store, &graph.root_task_id);
    }
}

fn map_shadow_dispatch_store_error(error: DomainError) -> ApiError {
    match error {
        DomainError::InvalidState { message } if message.contains("active current_turn") => {
            ApiError::conflict("执行 shadow dispatch 失败", &message)
        }
        other => ApiError::internal_assembly("执行 shadow dispatch 失败", other),
    }
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
    let manager = state.runner_manager().ok_or_else(|| {
        ApiError::internal_assembly("执行 shadow dispatch 失败", "runner_manager 未配置")
    })?;
    let task_store = state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("执行 shadow dispatch 失败", "task_store 未配置")
    })?;

    let root_task = task_store.get_task(&accepted.root_task_id).ok_or_else(|| {
        ApiError::internal_assembly("执行 shadow dispatch 失败", "root task 不存在")
    })?;
    let background_allowed = root_task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.background_allowed)
        .unwrap_or(false);

    if background_allowed {
        match manager.start(
            accepted.root_task_id.as_str(),
            Some(accepted.session_id.clone()),
        ) {
            Ok(_) | Err(RunnerStartError::AlreadyRunning) => {
                accepted.runner_started = true;
                Ok(())
            }
            Err(RunnerStartError::NotFound) => Err(ApiError::internal_assembly(
                "执行 shadow dispatch 失败",
                "root task 不存在",
            )),
        }
    } else {
        let execution = drive_shadow_task_graph(
            state,
            &accepted.root_task_id,
            &accepted.action_task_id,
            "执行 shadow dispatch 失败",
        )?;
        accepted.runner_started = execution.runner_started;
        Ok(())
    }
}

pub fn drive_shadow_task_graph(
    state: &ApiState,
    root_task_id: &TaskId,
    action_task_id: &TaskId,
    failure_title: &'static str,
) -> Result<ShadowGraphDriveResult, ApiError> {
    // 普通模式使用同步 for 循环，要求 dispatch 同步完成，否则结果来不及被收集。
    if let Some(dispatcher) = state.session_turn_dispatcher() {
        dispatcher.set_force_sync_dispatch(true);
    }

    let result = (|| {
        let manager = state
            .runner_manager()
            .ok_or_else(|| ApiError::internal_assembly(failure_title, "runner_manager 未配置"))?;
        let task_store = state
            .task_store()
            .ok_or_else(|| ApiError::internal_assembly(failure_title, "task_store 未配置"))?;

        let mut executed = false;
        for _ in 0..32 {
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
                format!("同步任务未在窗口内完成: {:?}", action_status),
            ));
        }

        Ok(ShadowGraphDriveResult {
            runner_started: executed,
        })
    })();

    if let Some(dispatcher) = state.session_turn_dispatcher() {
        dispatcher.set_force_sync_dispatch(false);
    }

    result
}
