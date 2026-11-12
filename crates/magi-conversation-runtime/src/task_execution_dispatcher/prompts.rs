use super::*;

impl LlmTaskDispatcher {
    pub(super) fn task_fact_context_parts(&self, task: &magi_core::Task) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(scope) = task
            .workspace_scope
            .as_deref()
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
        {
            parts.push(format!("[task-workspace] {scope}"));
        }
        if crate::task_helpers::task_can_delegate(
            Some(task),
            Some(self.agent_role_registry.as_ref()),
        ) && let Some(TaskExecutionPlan::Dispatch { session_id, .. }) =
            self.execution_registry.get(&task.task_id)
        {
            let heads = crate::agent_activation::retained_agent_heads(
                self.pipeline.task_store.as_ref(), &self.session_store, &session_id, task,
            ).into_iter().map(|head| serde_json::json!({
                    "activation":head.agent_activation(), "task_id":head.task_id, "title":head.title,
                    "status":head.status, "role":head.executor_binding_target_role(),
                })).collect::<Vec<_>>();
            if !heads.is_empty() {
                parts.push(format!(
                    "[session-agents] 本会话保留代理身份的当前事实：\n{}",
                    serde_json::to_string(&heads).expect("agent heads serialize")
                ));
            }
        }
        if let Some(activation) = task.agent_activation() {
            parts.push(format!("[agent-activation] agent_id={} generation={} lifetime={:?} previous_task_id={:?}。这是新的执行实例；旧产物与 transcript 保留在原 Task/Thread，不能宣称旧分支已合入。",
                activation.agent_id, activation.generation, activation.lifetime, activation.previous_task_id));
            if let Some(previous) = activation
                .previous_task_id
                .as_ref()
                .and_then(|id| self.pipeline.task_store.as_ref().get_task(id))
            {
                parts.push(format!("[previous-agent-activation] title={} goal={}\n可通过 context_read 按需读取输出：{}\n证据：{}\n原始执行记录：task:{}:transcript（历史参考，不是本轮工具执行证据）",
                    previous.title, previous.goal,
                    (0..previous.output_refs.len()).map(|index| format!("task:{}:output:{index}", previous.task_id)).collect::<Vec<_>>().join(", "),
                    (0..previous.evidence_refs.len()).map(|index| format!("task:{}:evidence:{index}", previous.task_id)).collect::<Vec<_>>().join(", "), previous.task_id));
            }
        }
        if let Some(package) = task.agent_context_package() {
            parts.push(package.render_for_prompt());
        } else if !task.input_refs.is_empty() {
            parts.push(format!(
                "[task-input] {}",
                format_task_ref_list(&task.input_refs)
            ));
        }
        if let Some(prompt) = crate::context_reference::browser_annotation_references_prompt(
            task.browser_annotation_references(),
        ) {
            parts.push(prompt);
        }
        if let Some(prompt) =
            crate::context_reference::browser_node_selections_prompt(task.browser_node_selections())
        {
            parts.push(prompt);
        }

        let task_store = self.pipeline.task_store.as_ref();
        for dependency_id in &task.dependency_ids {
            if let Some(dependency) = task_store.get_task(dependency_id) {
                parts.push(format_dependency_task_context(&dependency));
            } else {
                parts.push(format!("[dependency] id={dependency_id} status=missing"));
            }
        }
        if parts.is_empty() && task.kind != TaskKind::LocalAgent {
            return parts;
        }
        let mut ordered_parts = vec![CURRENT_TASK_PRIORITY_NOTE.to_string()];
        if task.kind == TaskKind::LocalAgent {
            ordered_parts.push(
                "[validation-rule] 只验证本任务 dependency/input 指向的当前执行产出；不得把历史经验、知识库记录或其他会话目标当成本次交付对象。"
                    .to_string(),
            );
        }
        ordered_parts.extend(parts);
        ordered_parts
    }

    /// `main_thread` 为真时，模型请求本来就带着完整的会话线程历史：近期回合和文件读取摘要
    /// 只是历史的投影副本，每轮重复放在请求尾部会多发数千 token 且永远命不中缓存，因此不再装配。
    pub(super) fn assemble_prompt(
        &self,
        _settings_store: Option<&Arc<SettingsStore>>,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        main_thread: bool,
    ) -> (
        String,
        Vec<magi_bridge_client::ChatMessage>,
        Option<ExecutionContextSummary>,
    ) {
        let base_prompt = if task.goal.is_empty() {
            task.title.clone()
        } else if task_title_is_derived_from_goal(&task.title, &task.goal) {
            // 用户回合的任务标题只是目标的截断副本；把它再拼一遍会让模型看到的输入与
            // thread 历史里的原始用户消息不同，回合边界重建后缓存前缀在此处失效。
            task.goal.clone()
        } else {
            format!("{}\n\n{}", task.title, task.goal)
        };
        let mut context_messages = Vec::new();
        let task_facts = self.task_fact_context_parts(task);
        if !task_facts.is_empty() {
            context_messages.push(runtime_context_message(
                PromptFragmentKind::TaskContext,
                task_facts.join("\n"),
            ));
        }
        if let Some(snapshot) = task
            .agent_context_package()
            .and_then(|package| package.fork.as_ref())
        {
            context_messages.push(runtime_context_message(
                PromptFragmentKind::ContextReferences,
                format!(
                    "{}\n{}",
                    REFERENCE_CONTEXT_PRIORITY_NOTE,
                    snapshot.render_for_prompt()
                ),
            ));
        }
        let (Some(ctx_runtime), Some(ws_id)) = (&self.context_runtime, workspace_id.clone()) else {
            return (base_prompt, context_messages, None);
        };
        let knowledge_selection = ctx_runtime.select_knowledge_on_demand(KnowledgeContextRequest {
            consumer: KnowledgeConsumer::TaskExecution,
            workspace_id: Some(ws_id.clone()),
            source_folder_id: task
                .executor_binding
                .as_ref()
                .and_then(|binding| binding.project_directories.as_ref())
                .map(|directories| directories.default_source_folder_id.clone()),
            execution_root: (task.kind != TaskKind::LocalAgent)
                .then(|| {
                    self.prepare_workspace_execution(
                        task,
                        session_id,
                        workspace_id,
                        crate::workspace_execution::WorkspacePreparationMode::Mapping,
                    )
                    .ok()
                    .and_then(|resources| resources.directories)
                    .and_then(|map| {
                        map.root(None)
                            .ok()
                            .map(|root| root.execution_root_ref.clone())
                    })
                })
                .flatten(),
            query: base_prompt.clone(),
        });
        self.publish_knowledge_context_diagnostic(
            &knowledge_selection,
            session_id,
            Some(&ws_id),
            Some(&task.mission_id),
            Some(&task.task_id),
        );
        let knowledge_context_prompt = knowledge_selection.render_for_prompt();
        let mut context_budget = self
            .context_budget
            .clone()
            .unwrap_or_else(default_context_budget_for_unconfigured_dispatcher);
        context_budget.max_knowledge = 0;
        if task.kind == TaskKind::LocalAgent
            && !task_is_coordinator(Some(task), Some(self.agent_role_registry.as_ref()))
        {
            context_budget.max_turns = 0;
            context_budget.max_memory = 0;
            context_budget.max_shared_items = 0;
        }
        if main_thread {
            context_budget.max_turns = 0;
            context_budget.max_file_summaries = 0;
        }
        refresh_file_summary_projection(
            ctx_runtime,
            self.session_store.as_ref(),
            session_id,
            &ws_id,
        );
        let result = ctx_runtime.assemble_execution_context(&ExecutionContextAssemblyRequest {
            session_id: session_id.clone(),
            workspace_id: ws_id,
            project_key: None,
            clues: ExecutionContextClues {
                mission: Some(task.title.clone()),
                assignment: None,
                task: Some(task.goal.clone()),
            },
            budget: context_budget,
        });
        let mut context_summary = ExecutionContextSummary::from_context_assembly(&result);
        context_summary.used_knowledge = knowledge_selection.results.len();
        context_summary.knowledge_ids = knowledge_selection
            .results
            .iter()
            .map(|item| item.knowledge_id.clone())
            .collect();
        context_summary.knowledge_ids.sort();
        context_summary.knowledge_ids.dedup();
        if knowledge_selection.truncated
            && !context_summary
                .truncation_parts
                .iter()
                .any(|part| part == "knowledge")
        {
            context_summary
                .truncation_parts
                .push("knowledge".to_string());
            context_summary.truncation_parts.sort();
            context_summary.truncation_count = context_summary.truncation_parts.len();
        }

        let mut ctx_parts: Vec<String> = Vec::new();
        for item in &result.selected_recent_turns {
            ctx_parts.push(format!(
                "[reference:recent-turn:{}] {}",
                recent_turn_source_label(item.source),
                item.content
            ));
        }
        if let Some(knowledge_context_prompt) = knowledge_context_prompt {
            ctx_parts.push(knowledge_context_prompt);
        }
        for item in &result.selected_memory {
            ctx_parts.push(format!("[reference:memory] {}", item.content));
        }
        for item in &result.selected_shared_context {
            ctx_parts.push(format!(
                "[reference:context] {}: {}",
                item.title, item.content
            ));
        }
        for item in &result.selected_file_summaries {
            ctx_parts.push(format!(
                "[reference:file-summary] {}: {}",
                item.absolute_path, item.summary
            ));
        }
        if !ctx_parts.is_empty() {
            ctx_parts.insert(0, REFERENCE_CONTEXT_PRIORITY_NOTE.to_string());
            context_messages.push(runtime_context_message(
                PromptFragmentKind::ContextReferences,
                ctx_parts.join("\n"),
            ));
        }
        (base_prompt, context_messages, Some(context_summary))
    }

    pub(super) fn resolve_user_rules_prompt(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
    ) -> Option<String> {
        let store = settings_store?;
        user_rules_from_settings(&store.get_section("userRules"))
    }

    pub(super) fn resolve_safeguard_prompt(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
    ) -> Option<String> {
        Some(render_safeguard_prompt(
            self.build_safety_gate(settings_store).as_ref(),
        ))
    }

    /// S8：依据当前 settings 快照构造 SafetyGate。
    /// 调用者每次进入 LLM 轮次循环前都构造一次；引擎本身无状态，可在该轮次内共享。
    pub(crate) fn build_safety_gate(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
    ) -> Option<magi_safety_gate::SafetyGate> {
        safety_gate_from_settings(settings_store)
    }
}
