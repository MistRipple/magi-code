use crate::{
    AgentRoleCatalogProvider, BrowserCapabilityProvider, BrowserToolExecutor, BuiltinTool,
    BuiltinToolAccessMode, BuiltinToolName, BuiltinToolSpec, ExternalMcpToolExecutor,
    ExternalToolCatalogProvider, ExternalToolCatalogSnapshot, GitToolExecutor,
    ImageGenerationExecutor, ImageGenerationReadinessProvider, PluginToolCatalogEntry,
    PluginToolCatalogProvider, PluginToolExecutor, RuntimeCapabilityDependencyProvider,
    ToolExecutionContext, ToolExecutionContextQuery, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionPolicy, ToolExecutionProgress, ToolExecutionSummary, ToolInvocationRecord,
    ToolRuntimeResources,
    builtin::{self, NormalizedBuiltinTool, execution_status_of},
    is_public_builtin_tool_surface,
    policy::WriteProtectionClaim,
    tool_catalog, tool_policy_decision_payload,
    workspace_changes::append_workspace_changed_paths,
    workspace_changes::capture_tool_workspace_snapshot,
};
use magi_core::{AccessProfile, EventId, ExecutionResultStatus, ToolCallId, UtcMillis};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::{
    DecisionPhase, GovernanceDecision, GovernanceOutcome, GovernanceService, ToolExecutionRequest,
    ToolKind,
};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct ToolRegistry {
    governance: Arc<GovernanceService>,
    event_bus: Arc<InMemoryEventBus>,
    builtin_tools: HashMap<String, Arc<dyn BuiltinTool>>,
    invocations: Arc<RwLock<Vec<ToolInvocationRecord>>>,
    pub(crate) active_write_claims: Arc<RwLock<HashMap<ToolCallId, WriteProtectionClaim>>>,
    runtime_resources: ToolRuntimeResources,
}

impl ToolRegistry {
    pub fn with_background_job_journal(
        mut self,
        journal: Arc<dyn crate::BackgroundJobJournal>,
    ) -> Self {
        self.runtime_resources.background_job_journal = Some(journal);
        self
    }
    pub fn new(governance: Arc<GovernanceService>, event_bus: Arc<InMemoryEventBus>) -> Self {
        Self {
            governance,
            event_bus,
            builtin_tools: HashMap::new(),
            invocations: Arc::new(RwLock::new(Vec::new())),
            active_write_claims: Arc::new(RwLock::new(HashMap::new())),
            runtime_resources: ToolRuntimeResources::default(),
        }
    }

    /// 注入 KnowledgeStore，让代码检索工具走真正的本地索引引擎。
    pub fn with_knowledge_store(
        mut self,
        knowledge_store: Arc<magi_knowledge_store::KnowledgeStore>,
    ) -> Self {
        self.runtime_resources.knowledge_store = Some(knowledge_store);
        self
    }

    pub fn with_external_tool_catalog_provider(
        mut self,
        provider: ExternalToolCatalogProvider,
    ) -> Self {
        self.runtime_resources.external_tool_catalog_provider = Some(provider);
        self
    }

    pub fn with_external_mcp_tool_executor(mut self, executor: ExternalMcpToolExecutor) -> Self {
        self.runtime_resources.external_mcp_tool_executor = Some(executor);
        self
    }

    /// 模型工具面只应发布有实际执行入口的 MCP 工具。目录 provider 可以在连接
    /// 尚未完成时提前返回 schema，因此不能把“有目录”误当成“可执行”。
    pub fn external_mcp_tool_executor_available(&self) -> bool {
        self.runtime_resources.external_mcp_tool_executor.is_some()
    }

    pub fn with_plugin_tools(
        mut self,
        provider: PluginToolCatalogProvider,
        executor: PluginToolExecutor,
    ) -> Self {
        self.runtime_resources.plugin_tool_catalog_provider = Some(provider);
        self.runtime_resources.plugin_tool_executor = Some(executor);
        self
    }

    pub fn plugin_tool_catalog(
        &self,
        workspace_id: Option<&magi_core::WorkspaceId>,
    ) -> Vec<PluginToolCatalogEntry> {
        self.runtime_resources
            .plugin_tool_catalog_provider
            .as_ref()
            .map(|provider| provider(workspace_id))
            .unwrap_or_default()
    }

    pub fn execute_plugin_tool(
        &self,
        model_tool_name: &str,
        arguments: &str,
        context: &ToolExecutionContext,
        access_profile: AccessProfile,
    ) -> Option<(String, ExecutionResultStatus)> {
        let entry = self
            .plugin_tool_catalog(context.workspace_id.as_ref())
            .into_iter()
            .find(|entry| entry.model_tool_name == model_tool_name)?;
        if let Some(result) = external_tool_access_rejection(
            model_tool_name,
            entry.read_only,
            access_profile,
            "plugin",
            "插件",
        ) {
            return Some(result);
        }
        self.runtime_resources
            .plugin_tool_executor
            .as_ref()
            .map(|executor| executor(&entry.model_tool_name, arguments, context))
    }

    pub fn external_tool_catalog_snapshot(&self) -> ExternalToolCatalogSnapshot {
        self.runtime_resources
            .external_tool_catalog_provider
            .as_ref()
            .map(|provider| provider())
            .unwrap_or_default()
    }

    /// 返回可由任务执行账本复用成功结果的工具。
    ///
    /// 内置工具的分类由 `BuiltinToolName` 统一维护；外部 MCP 工具只能以其
    /// 声明的 `read_only` 元数据加入，未知或可能写入的工具一律不复用。
    pub fn is_idempotent_read_tool(&self, tool_name: &str) -> bool {
        if BuiltinToolName::from_name(tool_name)
            .is_some_and(|tool| tool.is_idempotent_read_operation())
        {
            return true;
        }
        self.external_tool_catalog_snapshot()
            .mcp_tools
            .iter()
            .any(|tool| tool.model_tool_name == tool_name && tool.read_only)
    }

    pub fn execute_external_mcp_tool(
        &self,
        model_tool_name: &str,
        arguments: &str,
        access_profile: AccessProfile,
    ) -> Option<(String, ExecutionResultStatus)> {
        let binding = self
            .external_tool_catalog_snapshot()
            .mcp_tools
            .into_iter()
            .find(|tool| tool.model_tool_name == model_tool_name)?;
        if let Some(result) = external_tool_access_rejection(
            model_tool_name,
            binding.read_only,
            access_profile,
            "mcp",
            " MCP ",
        ) {
            return Some(result);
        }
        let executor = self.runtime_resources.external_mcp_tool_executor.as_ref()?;
        Some(executor(&binding.server_id, &binding.tool_name, arguments))
    }

    pub fn with_agent_role_catalog_provider(mut self, provider: AgentRoleCatalogProvider) -> Self {
        self.runtime_resources.agent_role_catalog_provider = Some(provider);
        self
    }

    pub fn with_runtime_capability_dependency_provider(
        mut self,
        provider: RuntimeCapabilityDependencyProvider,
    ) -> Self {
        self.runtime_resources
            .runtime_capability_dependency_provider = Some(provider);
        self
    }

    pub fn with_image_generation_runtime(
        mut self,
        executor: ImageGenerationExecutor,
        readiness_provider: ImageGenerationReadinessProvider,
    ) -> Self {
        self.runtime_resources.image_generation_executor = Some(executor);
        self.runtime_resources.image_generation_readiness_provider = Some(readiness_provider);
        self
    }

    pub fn with_git_tool_executor(mut self, executor: GitToolExecutor) -> Self {
        self.runtime_resources.git_tool_executor = Some(executor);
        self
    }

    pub fn with_browser_automation(
        mut self,
        executor: BrowserToolExecutor,
        capability_provider: BrowserCapabilityProvider,
    ) -> Self {
        self.runtime_resources.browser_tool_executor = Some(executor);
        self.runtime_resources.browser_capability_provider = Some(capability_provider);
        self
    }

    pub fn browser_tool_available(&self, tool: BuiltinToolName) -> bool {
        let Some(kind) = tool.browser_tool_kind() else {
            return true;
        };
        self.browser_capability_snapshot()
            .is_some_and(|snapshot| snapshot.allows_catalog_tool(kind))
    }

    pub fn browser_capability_snapshot(
        &self,
    ) -> Option<magi_browser_authority::BrowserCapabilitySnapshot> {
        let provider = self
            .runtime_resources
            .browser_capability_provider
            .as_ref()?;
        Some(provider())
    }

    pub fn register_builtin(&mut self, tool: Arc<dyn BuiltinTool>) {
        self.builtin_tools.insert(tool.name().to_string(), tool);
    }

    pub fn register_default_builtins(&mut self) {
        magi_process::initialize_user_process_environment();
        for name in BuiltinToolName::ALL {
            self.register_builtin(Arc::new(NormalizedBuiltinTool::new(
                name,
                name.default_risk_level(),
                name.default_approval_requirement(),
            )));
        }
    }

    pub fn builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        let mut specs = Vec::with_capacity(self.builtin_tools.len());
        for name in BuiltinToolName::ALL {
            if let Some(tool) = self.builtin_tools.get(name.as_str()) {
                specs.push(tool.spec());
            }
        }
        let mut custom_tools = self
            .builtin_tools
            .iter()
            .filter(|(name, _)| {
                !BuiltinToolName::ALL
                    .iter()
                    .any(|builtin| builtin.as_str() == name.as_str())
            })
            .collect::<Vec<_>>();
        custom_tools.sort_by_key(|(name, _)| *name);
        specs.extend(custom_tools.into_iter().map(|(_, tool)| tool.spec()));
        specs
    }

    pub fn public_builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        self.builtin_specs()
            .into_iter()
            .filter(|spec| is_public_builtin_tool_surface(&spec.name))
            .collect()
    }

    pub fn tool_catalog_value(
        &self,
        input: &str,
        context: &ToolExecutionContext,
    ) -> serde_json::Value {
        tool_catalog::build_tool_catalog_value(input, context, &self.runtime_resources)
    }

    pub fn builtin_access_mode(&self, tool_name: &str) -> Option<BuiltinToolAccessMode> {
        self.builtin_tools
            .get(tool_name)
            .and_then(|_| BuiltinToolName::from_name(tool_name))
            .map(|name| name.default_access_mode())
    }

    /// 根据允许/拒绝列表创建过滤后的工具注册表副本。
    pub fn filtered_clone(&self, allowed: &[String], denied: &[String]) -> Self {
        let mut filtered = self.clone();
        if !allowed.is_empty() {
            filtered
                .builtin_tools
                .retain(|name, _| allowed.contains(name));
        }
        if !denied.is_empty() {
            filtered
                .builtin_tools
                .retain(|name, _| !denied.contains(name));
        }
        filtered
    }

    pub fn execute(&self, input: ToolExecutionInput) -> ToolExecutionOutput {
        self.execute_with_context(input, ToolExecutionContext::default())
    }

    pub fn execute_with_context(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
    ) -> ToolExecutionOutput {
        self.execute_with_policy(input, context, &ToolExecutionPolicy::default())
    }

    pub fn execute_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, false, None, None)
    }

    pub fn execute_with_policy_and_progress(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        on_progress: &(dyn Fn(ToolExecutionProgress) + Sync),
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, false, Some(on_progress), None)
    }

    pub fn cancel_active_executions(&self, query: &ToolExecutionContextQuery) -> usize {
        builtin::cancellation::cancel_operations(Some(query))
            + builtin::cancel_active_processes(query)
    }

    pub fn has_active_executions(&self, query: &ToolExecutionContextQuery) -> bool {
        builtin::cancellation::has_active_operations(query) || builtin::has_active_processes(query)
    }

    pub fn cancel_all_active_executions() -> usize {
        builtin::cancellation::cancel_operations(None) + builtin::cancel_all_active_processes()
    }

    #[cfg(test)]
    pub(crate) fn execute_internal_builtin_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, true, None, None)
    }

    pub fn execute_with_cancellation(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, false, None, cancellation)
    }

    fn execute_with_policy_for_surface(
        &self,
        mut input: ToolExecutionInput,
        mut context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        allow_internal_builtin_surface: bool,
        on_progress: Option<&(dyn Fn(ToolExecutionProgress) + Sync)>,
        cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> ToolExecutionOutput {
        context.access_profile = policy.effective_access_profile();
        if cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
        {
            let output = ToolExecutionOutput {
                tool_call_id: input.tool_call_id.clone(),
                status: ExecutionResultStatus::Cancelled,
                governance: magi_governance::GovernanceDecision::allowed(magi_governance::DecisionPhase::ToolPolicy, input.risk_level, None),
                payload: serde_json::json!({ "tool": input.tool_name, "status": "cancelled", "error_code": "tool_cancelled", "error": "调用已取消，工具未执行" }).to_string(),
            };
            self.record_invocation(&input, &context, &output);
            return output;
        }
        if input.tool_kind == ToolKind::Builtin
            && let Some(canonical_name) = BuiltinToolName::from_name(input.tool_name.trim())
        {
            input.tool_name = canonical_name.as_str().to_string();
            if !allow_internal_builtin_surface && !canonical_name.is_public_tool_surface() {
                let output = self.build_internal_builtin_surface_rejection(&input, canonical_name);
                self.record_invocation(&input, &context, &output);
                return output;
            }
            if canonical_name.requires_workspace_context() && context.workspace_id.is_none() {
                let output = self.build_context_required_rejection(&input, canonical_name, true);
                self.record_invocation(&input, &context, &output);
                return output;
            }
        }
        if input.tool_kind == ToolKind::Builtin
            && BuiltinToolName::from_name(input.tool_name.trim())
                .is_some_and(|tool| tool.requires_execution_directory())
            && context.workspace_id.is_none()
            && context.working_directory.is_none()
        {
            let output = self.build_context_required_rejection(
                &input,
                BuiltinToolName::from_name(input.tool_name.trim()).expect("builtin checked"),
                false,
            );
            self.record_invocation(&input, &context, &output);
            return output;
        }
        if let Some(output) = self.enforce_execution_policy(&input, policy) {
            self.record_invocation(&input, &context, &output);
            return output;
        }
        context = match context.for_tool_arguments(&input.input) {
            Ok(context) => context,
            Err(error) => {
                let output = self.build_policy_rejection(
                    &input,
                    format!("project_directory_invalid: {error}"),
                    context.access_profile,
                );
                self.record_invocation(&input, &context, &output);
                return output;
            }
        };
        let projected_policy = match context.project_execution_policy(policy) {
            Ok(policy) => policy,
            Err(error) => {
                let output = self.build_policy_rejection(
                    &input,
                    format!("project_directory_invalid: {error}"),
                    context.access_profile,
                );
                self.record_invocation(&input, &context, &output);
                return output;
            }
        };
        let policy = &projected_policy;
        let access_mode = self.resolve_access_mode(&input);
        if let Some(output) = self.enforce_access_profile_policy(&input, &context, policy) {
            self.record_invocation(&input, &context, &output);
            return output;
        }

        let effective_access_profile = policy.effective_access_profile();
        let governance = if effective_access_profile == magi_core::AccessProfile::FullAccess {
            GovernanceDecision::allowed(
                DecisionPhase::ApprovalPolicy,
                input.risk_level,
                Some("完全授权模式跳过常规风险拦截".to_string()),
            )
        } else {
            self.governance
                .evaluate_tool_request(&ToolExecutionRequest {
                    tool_name: input.tool_name.clone(),
                    tool_kind: input.tool_kind.clone(),
                    risk_level: input.risk_level,
                    approval_requirement: input.approval_requirement,
                })
        };

        let output = if !governance.allowed {
            let status = if governance.requires_approval {
                ExecutionResultStatus::NeedsApproval
            } else {
                ExecutionResultStatus::Rejected
            };
            let reason = governance.reason.as_deref().unwrap_or("工具调用被阻断");
            ToolExecutionOutput {
                tool_call_id: input.tool_call_id.clone(),
                status,
                payload: tool_policy_decision_payload(
                    &input.tool_name,
                    status,
                    reason,
                    context.access_profile,
                ),
                governance,
            }
        } else {
            match self.builtin_tools.get(&input.tool_name) {
                Some(tool) => {
                    let write_guard = match self.acquire_write_guard(&input, &context, access_mode)
                    {
                        Ok(guard) => guard,
                        Err(output) => {
                            self.record_invocation(&input, &context, &output);
                            return output;
                        }
                    };
                    let before_changes = if BuiltinToolName::from_name(&input.tool_name)
                        .is_some_and(|tool| tool.is_background_process_observation(&input.input))
                    {
                        None
                    } else {
                        capture_tool_workspace_snapshot(&input, &context)
                    };
                    let mut resources = self.runtime_resources.clone();
                    resources.cancellation = cancellation.clone();
                    resources.execution_policy = Some(policy.clone());
                    // 读工具只做读取：与权限预检同一规则，读取不受默认的工作区范围约束。
                    let execution_policy =
                        crate::policy::execution_permission_policy(policy, &context);
                    resources.file_read_policy = crate::policy::path_access_policy(
                        &execution_policy,
                        input.tool_name.trim(),
                        magi_permissions::PathAccessKind::Read,
                        &policy.allowed_paths,
                    )
                    .into_owned();
                    let payload = match on_progress {
                        Some(on_progress) => tool.execute_with_progress(
                            &input.tool_call_id,
                            &input.input,
                            &context,
                            &resources,
                            on_progress,
                        ),
                        None => {
                            tool.execute(&input.tool_call_id, &input.input, &context, &resources)
                        }
                    };
                    let payload = append_workspace_changed_paths(
                        payload,
                        before_changes.as_deref(),
                        &context,
                    );
                    drop(write_guard);
                    ToolExecutionOutput {
                        tool_call_id: input.tool_call_id.clone(),
                        status: execution_status_of(&input.tool_name, &payload),
                        payload,
                        governance,
                    }
                }
                None => ToolExecutionOutput {
                    tool_call_id: input.tool_call_id.clone(),
                    status: ExecutionResultStatus::Failed,
                    payload: format!("未注册的工具: {}", input.tool_name),
                    governance,
                },
            }
        };

        self.record_invocation(&input, &context, &output);
        output
    }

    fn build_internal_builtin_surface_rejection(
        &self,
        input: &ToolExecutionInput,
        tool_name: BuiltinToolName,
    ) -> ToolExecutionOutput {
        let reason = format!(
            "{} 是 shell_exec 的内部运行时能力，不接受模型、worker 或外部调用直接执行；需要后台终端时请调用 shell_exec(background=true)",
            tool_name.as_str()
        );
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: serde_json::json!({
                "tool": tool_name.as_str(),
                "status": "rejected",
                "error": reason.clone(),
            })
            .to_string(),
            governance: GovernanceDecision {
                outcome: GovernanceOutcome::Rejected,
                allowed: false,
                requires_approval: false,
                phase: DecisionPhase::ToolPolicy,
                threshold: input.risk_level,
                reason: Some(reason),
            },
        }
    }

    fn build_context_required_rejection(
        &self,
        input: &ToolExecutionInput,
        tool_name: BuiltinToolName,
        project: bool,
    ) -> ToolExecutionOutput {
        let (code, requirement) = if project {
            ("workspace_required", "当前会话关联项目")
        } else {
            ("execution_directory_required", "daemon 提供会话执行目录")
        };
        let reason = format!("{} 需要{}；调用尚未执行", tool_name.as_str(), requirement);
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: serde_json::json!({
                "tool": tool_name.as_str(),
                "status": "rejected",
                "error_code": code,
                "error": reason.clone(),
            })
            .to_string(),
            governance: GovernanceDecision {
                outcome: GovernanceOutcome::Rejected,
                allowed: false,
                requires_approval: false,
                phase: DecisionPhase::ToolPolicy,
                threshold: input.risk_level,
                reason: Some(reason),
            },
        }
    }

    pub fn invocations(&self) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .clone()
    }

    pub fn summary(&self) -> ToolExecutionSummary {
        self.summary_for_query(&ToolExecutionContextQuery::default())
    }

    pub fn query_invocations(
        &self,
        query: &ToolExecutionContextQuery,
    ) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .iter()
            .filter(|record| !query.taskless_only || record.context.task_id.is_none())
            .filter(|record| {
                query
                    .worker_id
                    .as_ref()
                    .is_none_or(|id| record.context.worker_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .task_id
                    .as_ref()
                    .is_none_or(|id| record.context.task_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .session_id
                    .as_ref()
                    .is_none_or(|id| record.context.session_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .workspace_id
                    .as_ref()
                    .is_none_or(|id| record.context.workspace_id.as_ref() == Some(id))
            })
            .cloned()
            .collect()
    }

    pub fn governance_decision_for_tool_request(
        &self,
        request: &ToolExecutionRequest,
        access_profile: AccessProfile,
    ) -> GovernanceDecision {
        if access_profile == AccessProfile::FullAccess {
            return GovernanceDecision::allowed(
                DecisionPhase::ApprovalPolicy,
                request.risk_level,
                Some("完全授权模式跳过常规风险拦截".to_string()),
            );
        }
        self.governance.evaluate_tool_request(request)
    }

    pub fn record_external_invocation(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        output: &ToolExecutionOutput,
    ) {
        self.record_invocation(input, context, output);
    }

    pub fn summary_for_query(&self, query: &ToolExecutionContextQuery) -> ToolExecutionSummary {
        let invocations = self.query_invocations(query);

        self.summarize_invocations(&invocations)
    }

    fn summarize_invocations(&self, invocations: &[ToolInvocationRecord]) -> ToolExecutionSummary {
        ToolExecutionSummary::from_statuses(invocations.iter().map(|record| record.status))
    }

    fn record_invocation(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        output: &ToolExecutionOutput,
    ) {
        let record = ToolInvocationRecord {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            tool_kind: input.tool_kind.clone(),
            context: context.clone(),
            status: output.status,
            payload: output.payload.clone(),
            created_at: UtcMillis::now(),
        };
        self.invocations
            .write()
            .expect("tool invocation write lock poisoned")
            .push(record.clone());
        self.publish_with_category(
            "tool.invoked",
            EventCategory::Audit,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                task_id: record.context.task_id.clone(),
                ..EventContext::default()
            },
            EventId::new(format!("tool-call-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
        self.publish_with_category(
            "tool.usage.recorded",
            EventCategory::Usage,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                mission_id: None,
                assignment_id: None,
                task_id: record.context.task_id.clone(),
            },
            EventId::new(format!("tool-usage-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "risk_level": format!("{:?}", input.risk_level),
                "approval_requirement": format!("{:?}", input.approval_requirement),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
    }
}

fn external_tool_access_rejection(
    name: &str,
    read_only: bool,
    access_profile: AccessProfile,
    source: &str,
    label: &str,
) -> Option<(String, ExecutionResultStatus)> {
    let status = crate::external_tool_access_decision(read_only, access_profile)?;
    let approval = status == ExecutionResultStatus::NeedsApproval;
    let mut payload = serde_json::json!({
        "tool": name,
        "status": status.wire_label(),
        "error_code": format!("{source}_{}", if approval { "requires_full_access" } else { "blocked_in_read_only" }),
        "error": if approval {
            format!("该{label}工具需要你的确认，授权后将继续当前调用")
        } else {
            format!("只读访问模式不允许调用有外部副作用的{label}工具")
        },
        "access_profile": access_profile.as_str(),
    });
    if approval {
        payload["approval_resume_safe"] = serde_json::Value::Bool(true);
    }
    Some((payload.to_string(), status))
}

impl ToolRegistry {
    fn publish_with_category(
        &self,
        event_type: &str,
        category: EventCategory,
        context: EventContext,
        event_id: EventId,
        payload: serde_json::Value,
    ) {
        let envelope = match category {
            EventCategory::Domain => EventEnvelope::domain(event_id, event_type, payload),
            EventCategory::Audit => EventEnvelope::audit(event_id, event_type, payload),
            EventCategory::Usage => EventEnvelope::usage(event_id, event_type, payload),
            EventCategory::Projection => EventEnvelope::projection(event_id, event_type, payload),
            EventCategory::System => EventEnvelope::system(event_id, event_type, payload),
        };
        let _ = self.event_bus.publish(envelope.with_context(context));
    }
}
