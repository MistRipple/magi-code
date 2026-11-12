use super::*;

impl LlmTaskDispatcher {
    pub fn new(
        event_bus: Arc<InMemoryEventBus>,
        pipeline: ExecutionPipeline,
        dependencies: LlmTaskDispatcherDependencies,
        mission_state_root: PathBuf,
    ) -> Self {
        let LlmTaskDispatcherDependencies {
            session_store,
            execution_registry,
            completion_sink,
            conversation_registry,
            agent_role_registry,
        } = dependencies;
        Self {
            event_bus,
            pipeline,
            session_store,
            execution_registry,
            completion_sink,
            completion_notifier: None,
            model_bridge_client: None,
            model_client_cache: Arc::new(Mutex::new(HashMap::new())),
            dispatch_quiesce: Arc::new(DispatchQuiesceState::default()),
            conversation_quiesce: Arc::new(DispatchQuiesceState::default()),
            knowledge_store: None,
            knowledge_persist_callback: None,
            session_state_persist_callback: None,
            settings_store: None,
            context_runtime: None,
            context_budget: None,
            workspace_registry: None,
            git_service: None,
            session_code_contexts: None,
            workspace_git_coordinator: None,
            agent_worktree_root: mission_state_root.join("worktrees"),
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            plugin_command_executor: None,
            plugin_session_message_admitter: None,
            extension_hooks: None,
            plugin_model_calls: Arc::default(),
            snapshot_manager: None,
            workspace_execution_preparer: None,
            conversation_registry,
            agent_role_registry,
            project_memory_registry: Arc::new(
                magi_project_memory::ProjectMemoryRegistry::with_home(mission_state_root),
            ),
        }
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    /// 绑定 Task/Turn 来源与完成交付到同一通知器；结果直接提交 TaskStore，
    /// 再唤醒 Turn 侧，不经内存结果队列或 Runner 轮询。
    pub fn with_completion_notifier(mut self, notifier: Arc<TaskCompletionNotifier>) -> Self {
        self.completion_sink = notifier.clone();
        self.completion_notifier = Some(notifier);
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

    pub fn with_session_state_persist_callback(
        mut self,
        callback: Arc<SessionStatePersistCallback>,
    ) -> Self {
        self.session_state_persist_callback = Some(callback);
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

    /// 注入 daemon 配置的 [`ContextBudget`]。dispatch summary 与 prompt 注入两条路径必须
    /// 使用同一份预算，避免"UI 看到 6 条候选、实际只投放 2 条"这类分裂。
    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.context_budget = Some(budget);
        self
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceStore>) -> Self {
        let mut runtime = self.execution_registry.agent_spawn_preflight_runtime();
        runtime.workspace_registry = Some(registry.clone());
        self.execution_registry = self
            .execution_registry
            .clone()
            .with_agent_spawn_preflight_runtime(runtime);
        self.workspace_registry = Some(registry);
        self
    }

    pub fn with_git_context_runtime(
        mut self,
        git_service: Arc<magi_git::GitService>,
        session_code_contexts: magi_git::SessionCodeContextRegistry,
    ) -> Self {
        self.git_service = Some(git_service);
        self.session_code_contexts = Some(session_code_contexts);
        self
    }

    pub fn with_workspace_git_coordinator(
        mut self,
        coordinator: magi_git::WorkspaceGitOperationCoordinator,
    ) -> Self {
        self.workspace_git_coordinator = Some(coordinator);
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

    pub(super) fn resolve_registered_skill_id(
        &self,
        requested: Option<&str>,
    ) -> Result<Option<String>, String> {
        let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let unavailable = || {
            format!(
                "skill_unavailable: 已选择的 Skill {requested} 当前不可用，请重新选择有效 Skill"
            )
        };
        let runtime = self.skill_runtime.as_ref().ok_or_else(unavailable)?;
        match runtime.registry().resolve_skill_id(requested) {
            magi_skill_runtime::SkillIdResolution::Found(skill_id) => Ok(Some(skill_id)),
            // 配置撤销不能降级成无 Skill 的全工具执行。
            magi_skill_runtime::SkillIdResolution::NotFound => Err(unavailable()),
            magi_skill_runtime::SkillIdResolution::Ambiguous(_) => Err(format!(
                "skill_ambiguous: 已选择的 Skill {requested} 名称不唯一，请选择完整 Skill ID"
            )),
        }
    }

    pub fn with_skill_dispatch_runtime(
        mut self,
        runtime: Arc<magi_skill_runtime::SkillDispatchRuntime>,
    ) -> Self {
        self.skill_dispatch_runtime = Some(runtime);
        self
    }

    /// 接纳插件业务结果，复用 API 的原生 Input 提交端口。
    pub fn with_plugin_session_message_admitter(
        mut self,
        admitter: Arc<crate::plugin_command::PluginSessionMessageAdmitter>,
    ) -> Self {
        self.plugin_session_message_admitter = Some(admitter);
        self
    }

    pub fn with_plugin_command_executor(
        mut self,
        executor: Arc<dyn crate::plugin_command::PluginCommandExecutor>,
    ) -> Self {
        self.plugin_command_executor = Some(executor);
        self
    }

    pub fn with_extension_hooks(
        mut self,
        hooks: Arc<dyn crate::extension_hooks::ExtensionHooks>,
    ) -> Self {
        self.extension_hooks = Some(hooks);
        self
    }

    pub fn with_snapshot_manager(mut self, manager: Arc<magi_snapshot::SnapshotManager>) -> Self {
        self.snapshot_manager = Some(manager);
        self
    }

    pub fn with_workspace_execution_preparer(
        mut self,
        prepare: Arc<WorkspaceExecutionPreparer>,
    ) -> Self {
        self.workspace_execution_preparer = Some(prepare);
        self
    }

    pub fn project_memory_registry(&self) -> Arc<magi_project_memory::ProjectMemoryRegistry> {
        self.project_memory_registry.clone()
    }
}
