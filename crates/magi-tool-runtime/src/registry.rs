use crate::{
    AgentRoleCatalogProvider, BuiltinTool, BuiltinToolAccessMode, BuiltinToolName, BuiltinToolSpec,
    ExternalMcpToolExecutor, ExternalToolCatalogProvider, ExternalToolCatalogSnapshot,
    ImageGenerationExecutor, ImageGenerationReadinessProvider, RuntimeCapabilityDependencyProvider,
    ToolExecutionContext, ToolExecutionContextQuery, ToolExecutionInput, ToolExecutionOutput,
    ToolExecutionPolicy, ToolExecutionSummary, ToolInvocationRecord, ToolRuntimeResources,
    builtin::{self, NormalizedBuiltinTool, infer_execution_status},
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

    pub fn external_tool_catalog_snapshot(&self) -> ExternalToolCatalogSnapshot {
        self.runtime_resources
            .external_tool_catalog_provider
            .as_ref()
            .map(|provider| provider())
            .unwrap_or_default()
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
        if !binding.read_only {
            match access_profile {
                AccessProfile::ReadOnly => {
                    return Some((
                        serde_json::json!({
                            "tool": model_tool_name,
                            "status": "rejected",
                            "error_code": "mcp_blocked_in_read_only",
                            "error": "只读访问模式不允许调用有外部副作用的 MCP 工具",
                            "access_profile": "read_only",
                        })
                        .to_string(),
                        ExecutionResultStatus::Rejected,
                    ));
                }
                AccessProfile::Restricted => {
                    return Some((
                        serde_json::json!({
                            "tool": model_tool_name,
                            "status": "needs_approval",
                            "error_code": "mcp_requires_full_access",
                            "error": "受限访问已拦截该 MCP 工具，请切换为完全访问权限后重试",
                            "access_profile": "restricted",
                        })
                        .to_string(),
                        ExecutionResultStatus::NeedsApproval,
                    ));
                }
                AccessProfile::FullAccess => {}
            }
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

    pub fn register_builtin(&mut self, tool: Arc<dyn BuiltinTool>) {
        self.builtin_tools.insert(tool.name().to_string(), tool);
    }

    pub fn register_default_builtins(&mut self) {
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
        custom_tools.sort_by(|(left, _), (right, _)| left.cmp(right));
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
        self.execute_with_policy_for_surface(input, context, policy, false)
    }

    pub fn cancel_active_shell_execs(&self, query: &ToolExecutionContextQuery) -> usize {
        builtin::cancel_active_shell_execs(query)
    }

    #[cfg(test)]
    pub(crate) fn execute_internal_builtin_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, true)
    }

    fn execute_with_policy_for_surface(
        &self,
        mut input: ToolExecutionInput,
        mut context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        allow_internal_builtin_surface: bool,
    ) -> ToolExecutionOutput {
        context.access_profile = policy.effective_access_profile();
        if input.tool_kind == ToolKind::Builtin
            && let Some(canonical_name) = BuiltinToolName::from_name(input.tool_name.trim())
        {
            input.tool_name = canonical_name.as_str().to_string();
            if !allow_internal_builtin_surface && !canonical_name.is_public_tool_surface() {
                let output = self.build_internal_builtin_surface_rejection(&input, canonical_name);
                self.record_invocation(&input, &context, &output);
                return output;
            }
        }
        if let Some(output) = self.enforce_execution_policy(&input, policy) {
            self.record_invocation(&input, &context, &output);
            return output;
        }
        let access_mode = self.resolve_access_mode(&input);
        if let Some(output) =
            self.enforce_access_profile_policy(&input, &context, policy, access_mode)
        {
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
                    let before_changes = capture_tool_workspace_snapshot(&input, &context);
                    let payload = tool.execute(&input.input, &context, &self.runtime_resources);
                    let payload =
                        append_workspace_changed_paths(payload, before_changes.as_ref(), &context);
                    drop(write_guard);
                    ToolExecutionOutput {
                        tool_call_id: input.tool_call_id.clone(),
                        status: infer_execution_status(&payload),
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
        let total_invocations = invocations.len();
        let successful_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Succeeded)
            .count();
        let blocked_invocations = invocations
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    ExecutionResultStatus::NeedsApproval | ExecutionResultStatus::Rejected
                )
            })
            .count();
        let failed_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Failed)
            .count();
        ToolExecutionSummary {
            total_invocations,
            successful_invocations,
            blocked_invocations,
            failed_invocations,
        }
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
