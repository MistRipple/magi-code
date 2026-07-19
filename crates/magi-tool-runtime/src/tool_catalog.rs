use crate::{
    AgentRoleCatalogEntry, BuiltinToolAccessMode, BuiltinToolName, ExternalToolCatalogEntry,
    ExternalToolCatalogSnapshot, ToolExecutionContext, ToolRuntimeResources,
};
use std::collections::BTreeSet;

pub(crate) fn execute_tool_catalog(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    build_tool_catalog_value(input, context, resources).to_string()
}

pub(crate) fn build_tool_catalog_value(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> serde_json::Value {
    let request = serde_json::from_str::<serde_json::Value>(input).ok();
    let include_internal = request
        .as_ref()
        .and_then(|value| value.get("include_internal"))
        .or_else(|| {
            request
                .as_ref()
                .and_then(|value| value.get("includeInternal"))
        })
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let include_schema = request
        .as_ref()
        .and_then(|value| value.get("include_schema"))
        .or_else(|| {
            request
                .as_ref()
                .and_then(|value| value.get("includeSchema"))
        })
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let include_external = request
        .as_ref()
        .and_then(|value| value.get("include_external"))
        .or_else(|| {
            request
                .as_ref()
                .and_then(|value| value.get("includeExternal"))
        })
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let include_mcp_servers = request
        .as_ref()
        .and_then(|value| value.get("include_mcp_servers"))
        .or_else(|| {
            request
                .as_ref()
                .and_then(|value| value.get("includeMcpServers"))
        })
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let include_agent_roles = request
        .as_ref()
        .and_then(|value| value.get("include_agent_roles"))
        .or_else(|| {
            request
                .as_ref()
                .and_then(|value| value.get("includeAgentRoles"))
        })
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let mut tools = Vec::new();
    let mut builtin_categories = BTreeSet::new();
    let mut public_categories = BTreeSet::new();
    let mut internal_categories = BTreeSet::new();
    let mut schema_warning_count = 0usize;
    let agent_roles = resources
        .agent_role_catalog_provider
        .as_ref()
        .map(|provider| {
            let mut roles = provider();
            roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
            roles
        })
        .unwrap_or_default();
    let runtime_health = RuntimeHealth::from_resources(resources, &agent_roles);
    let mut runtime_warning_count = 0usize;
    let access_profile = context.access_profile;

    for tool in BuiltinToolName::ALL {
        let is_public = tool.is_public_tool_surface();
        builtin_categories.insert(tool.category());
        if is_public {
            public_categories.insert(tool.category());
        } else {
            internal_categories.insert(tool.category());
        }
        if !is_public && !include_internal {
            continue;
        }

        let schema = tool.parameters_schema();
        let schema_warnings = schema_warnings(&schema);
        schema_warning_count += schema_warnings.len();
        let runtime_status = runtime_health.tool_status(tool);
        runtime_warning_count += runtime_status.warnings.len();
        let policy_view = tool_catalog_policy_view(tool, access_profile);

        let mut item = serde_json::json!({
            "name": tool.as_str(),
            "category": tool.category(),
            "public": is_public,
            "runtime_internal": tool.is_runtime_internal_tool_call(),
            "access_mode": access_mode_for_tool(tool).as_str(),
            "policy_scope": policy_view.policy_scope,
            "input_sensitive_policy": policy_view.input_sensitive_policy,
            "policy_summary": policy_view.policy_summary,
            "risk_level": risk_level_label(tool),
            "approval_requirement": approval_requirement_label(tool),
            "effective_approval_policy": policy_view.effective_approval_policy,
            "access_profile_behavior": policy_view.access_profile_behavior,
            "schema_status": if schema_warnings.is_empty() { "ok" } else { "warning" },
            "schema_warnings": schema_warnings,
            "runtime_status": runtime_status.status,
            "runtime_warnings": runtime_status.warnings,
        });
        if include_schema {
            item["parameters_schema"] = schema;
        }
        tools.push(item);
    }

    let external_catalog = if include_external {
        resources
            .external_tool_catalog_provider
            .as_ref()
            .map(|provider| provider())
    } else {
        None
    };
    let external_dependency_source = if include_external {
        external_catalog
            .as_ref()
            .map(ExternalCapabilityDependencySource::Available)
            .unwrap_or(ExternalCapabilityDependencySource::Unavailable)
    } else {
        ExternalCapabilityDependencySource::Disabled
    };
    let runtime_dependencies =
        runtime_health.dependencies_json(resources, external_dependency_source);
    let (instruction_skill_count, raw_skill_tools, external_mcp_servers, external_mcp_tools) =
        external_catalog
            .map(|catalog| {
                (
                    catalog.instruction_skill_count,
                    catalog.skill_tools,
                    catalog.mcp_servers,
                    catalog.mcp_tools,
                )
            })
            .unwrap_or_default();
    let skill_tools =
        effective_external_skill_tools_for_access_profile(raw_skill_tools, access_profile);
    let skill_tool_count = skill_tools.len();
    let mcp_server_count = if include_mcp_servers {
        external_mcp_servers.len()
    } else {
        0
    };
    let connected_mcp_server_count = if include_mcp_servers {
        external_mcp_servers
            .iter()
            .filter(|server| server.enabled && server.connected)
            .count()
    } else {
        0
    };
    let enabled_mcp_server_count = if include_mcp_servers {
        external_mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .count()
    } else {
        0
    };
    let agent_role_count = agent_roles.len();
    let spawnable_agent_role_count = agent_roles.iter().filter(|role| role.spawnable).count();
    let builtin_total = builtin_categories.len();
    let public_count = public_categories.len();
    let internal_count = internal_categories.len();
    let external_catalog_status =
        if include_external && resources.external_tool_catalog_provider.is_some() {
            "available"
        } else if include_external {
            "unavailable"
        } else {
            "disabled"
        };
    let agent_role_catalog_status = if resources.agent_role_catalog_provider.is_some() {
        "available"
    } else {
        "unavailable"
    };

    serde_json::json!({
        "tool": "tool_catalog",
        "status": "succeeded",
        "catalog_access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "current_access_profile": access_profile.as_str(),
        "approval_policy_summary": approval_policy_summary(access_profile),
        "summary": tool_catalog_summary(ToolCatalogSummaryInput {
            public_count,
            instruction_skill_count,
            skill_tool_count,
            connected_mcp_server_count,
            enabled_mcp_server_count,
            spawnable_agent_role_count,
            agent_role_count,
            schema_warning_count,
            runtime_warning_count,
        }),
        "total": tools.len() + skill_tool_count + external_mcp_tools.len(),
        "builtin_total": builtin_total,
        "public_count": public_count,
        "internal_count": internal_count,
        "schema_warning_count": schema_warning_count,
        "runtime_warning_count": runtime_warning_count,
        "runtime_dependencies": runtime_dependencies,
        "external_catalog_status": external_catalog_status,
        "instruction_skill_count": instruction_skill_count,
        "skill_tool_count": skill_tool_count,
        "mcp_server_count": mcp_server_count,
        "connected_mcp_server_count": connected_mcp_server_count,
        "mcp_tool_count": external_mcp_tools.len(),
        "agent_role_catalog_status": agent_role_catalog_status,
        "agent_role_count": agent_role_count,
        "spawnable_agent_role_count": spawnable_agent_role_count,
        "tools": tools,
        "skill_tools": if include_external {
            serde_json::to_value(skill_tools).unwrap_or_else(|_| serde_json::json!([]))
        } else {
            serde_json::json!([])
        },
        "mcp_servers": if include_external && include_mcp_servers {
            serde_json::to_value(external_mcp_servers).unwrap_or_else(|_| serde_json::json!([]))
        } else {
            serde_json::json!([])
        },
        "mcp_tools": if include_external && include_mcp_servers {
            serde_json::to_value(external_mcp_tools).unwrap_or_else(|_| serde_json::json!([]))
        } else {
            serde_json::json!([])
        },
        "agent_roles": if include_agent_roles {
            serde_json::to_value(agent_roles).unwrap_or_else(|_| serde_json::json!([]))
        } else {
            serde_json::json!([])
        },
    })
}

struct ToolCatalogSummaryInput {
    public_count: usize,
    instruction_skill_count: usize,
    skill_tool_count: usize,
    connected_mcp_server_count: usize,
    enabled_mcp_server_count: usize,
    spawnable_agent_role_count: usize,
    agent_role_count: usize,
    schema_warning_count: usize,
    runtime_warning_count: usize,
}

fn tool_catalog_summary(input: ToolCatalogSummaryInput) -> String {
    let ToolCatalogSummaryInput {
        public_count,
        instruction_skill_count,
        skill_tool_count,
        connected_mcp_server_count,
        enabled_mcp_server_count,
        spawnable_agent_role_count,
        agent_role_count,
        schema_warning_count,
        runtime_warning_count,
    } = input;
    let mut summary = format!(
        "工具目录已更新：{public_count} 类内置能力、{instruction_skill_count} 个 Skill、{skill_tool_count} 个自定义工具、MCP 启用服务 {connected_mcp_server_count}/{enabled_mcp_server_count} 可用、子代理 {spawnable_agent_role_count}/{agent_role_count} 可派发"
    );
    let warning_count = schema_warning_count + runtime_warning_count;
    if warning_count > 0 {
        summary.push_str(&format!("，{warning_count} 项能力需关注"));
    }
    summary
}

struct RuntimeHealth {
    knowledge_store_available: bool,
    agent_role_registry_available: bool,
    agent_role_count: usize,
    spawnable_agent_role_count: usize,
    image_generation_available: bool,
    image_generation_configured: bool,
}

struct RuntimeToolStatus {
    status: &'static str,
    warnings: Vec<String>,
}

#[derive(Clone, Copy)]
enum ExternalCapabilityDependencySource<'a> {
    Available(&'a ExternalToolCatalogSnapshot),
    Unavailable,
    Disabled,
}

impl RuntimeHealth {
    fn from_resources(
        resources: &ToolRuntimeResources,
        agent_roles: &[AgentRoleCatalogEntry],
    ) -> Self {
        let knowledge_store_available = resources.knowledge_store.is_some();

        Self {
            knowledge_store_available,
            agent_role_registry_available: resources.agent_role_catalog_provider.is_some(),
            agent_role_count: agent_roles.len(),
            spawnable_agent_role_count: agent_roles.iter().filter(|role| role.spawnable).count(),
            image_generation_available: resources.image_generation_executor.is_some()
                && resources.image_generation_readiness_provider.is_some(),
            image_generation_configured: resources
                .image_generation_readiness_provider
                .as_ref()
                .is_some_and(|provider| provider()),
        }
    }

    fn tool_status(&self, tool: BuiltinToolName) -> RuntimeToolStatus {
        match tool {
            BuiltinToolName::KnowledgeQuery => self.knowledge_tool_status(),
            BuiltinToolName::SearchSemantic => self.code_index_tool_status(),
            BuiltinToolName::CodeSymbols => self.code_index_tool_status(),
            BuiltinToolName::AgentSpawn | BuiltinToolName::AgentWait => {
                self.agent_role_tool_status()
            }
            BuiltinToolName::ImageGenerate => self.image_generation_tool_status(),
            _ => RuntimeToolStatus {
                status: "ready",
                warnings: Vec::new(),
            },
        }
    }

    fn image_generation_tool_status(&self) -> RuntimeToolStatus {
        if !self.image_generation_available {
            return RuntimeToolStatus {
                status: "unavailable",
                warnings: vec!["图片生成运行时暂不可用".to_string()],
            };
        }
        if !self.image_generation_configured {
            return RuntimeToolStatus {
                status: "not_ready",
                warnings: vec!["图片生成模型尚未配置".to_string()],
            };
        }
        RuntimeToolStatus {
            status: "ready",
            warnings: Vec::new(),
        }
    }

    fn knowledge_tool_status(&self) -> RuntimeToolStatus {
        if !self.knowledge_store_available {
            return RuntimeToolStatus {
                status: "unavailable",
                warnings: vec!["知识检索能力暂不可用".to_string()],
            };
        }
        RuntimeToolStatus {
            status: "ready",
            warnings: Vec::new(),
        }
    }

    fn agent_role_tool_status(&self) -> RuntimeToolStatus {
        if !self.agent_role_registry_available {
            return RuntimeToolStatus {
                status: "unavailable",
                warnings: vec!["子代理能力暂不可用".to_string()],
            };
        }
        if self.spawnable_agent_role_count == 0 {
            return RuntimeToolStatus {
                status: "not_ready",
                warnings: vec!["当前没有可派发的子代理角色".to_string()],
            };
        }
        RuntimeToolStatus {
            status: "ready",
            warnings: Vec::new(),
        }
    }

    fn code_index_tool_status(&self) -> RuntimeToolStatus {
        self.knowledge_tool_status()
    }

    fn dependencies_json(
        &self,
        resources: &ToolRuntimeResources,
        external_source: ExternalCapabilityDependencySource<'_>,
    ) -> serde_json::Value {
        let mut dependencies = vec![
            serde_json::json!({
                "name": "knowledge_store",
                "status": if self.knowledge_store_available { "available" } else { "unavailable" },
                "required_by": ["knowledge_query", "search_semantic", "code_symbols"],
            }),
            serde_json::json!({
                "name": "workspace_code_index",
                "status": if self.knowledge_store_available { "ready" } else { "unavailable" },
                "required_by": ["search_semantic", "code_symbols"],
            }),
            serde_json::json!({
                "name": "agent_role_registry",
                "status": self.agent_role_registry_status(),
                "role_count": self.agent_role_count,
                "spawnable_role_count": self.spawnable_agent_role_count,
                "required_by": ["agent_spawn", "agent_wait"],
            }),
        ];
        dependencies.extend(external_capability_dependencies(external_source));
        dependencies.push(serde_json::json!({
            "name": "image_generation_model",
            "status": self.image_generation_tool_status().status,
            "required_by": ["image_generate"],
        }));

        if let Some(provider) = &resources.runtime_capability_dependency_provider {
            dependencies.extend(provider().into_iter().map(|entry| {
                serde_json::to_value(entry)
                    .expect("runtime capability dependency entry should serialize")
            }));
        }

        serde_json::Value::Array(dependencies)
    }

    fn agent_role_registry_status(&self) -> &'static str {
        if !self.agent_role_registry_available {
            "unavailable"
        } else if self.spawnable_agent_role_count == 0 {
            "not_ready"
        } else {
            "ready"
        }
    }
}

fn external_capability_dependencies(
    source: ExternalCapabilityDependencySource<'_>,
) -> Vec<serde_json::Value> {
    match source {
        ExternalCapabilityDependencySource::Disabled => {
            external_capability_dependencies_unloaded("disabled")
        }
        ExternalCapabilityDependencySource::Unavailable => {
            external_capability_dependencies_unloaded("unavailable")
        }
        ExternalCapabilityDependencySource::Available(external) => {
            external_capability_dependencies_from_snapshot(external)
        }
    }
}

fn external_capability_dependencies_unloaded(status: &'static str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "skill_runtime",
            "status": status,
            "required_by": ["skill prompt context", "skill custom tools"],
            "configured_count": 0,
            "tool_count": 0,
        }),
        serde_json::json!({
            "name": "mcp_servers",
            "status": status,
            "required_by": ["mcp custom tools", "skill MCP bridge tools"],
            "configured_count": 0,
            "enabled_count": 0,
            "ready_count": 0,
            "enabled_tool_count": 0,
            "ready_tool_count": 0,
            "tool_count": 0,
        }),
    ]
}

fn external_capability_dependencies_from_snapshot(
    external: &ExternalToolCatalogSnapshot,
) -> Vec<serde_json::Value> {
    let skill_tool_count = external.skill_tools.len();
    let invalid_skill_tool_count = external
        .skill_tools
        .iter()
        .filter(|tool| tool.status != "available")
        .count();
    let skill_status = if invalid_skill_tool_count > 0 {
        "not_ready"
    } else {
        "ready"
    };

    let configured_mcp_count = external.mcp_servers.len();
    let enabled_mcp_count = external
        .mcp_servers
        .iter()
        .filter(|server| server.enabled)
        .count();
    let ready_mcp_count = external
        .mcp_servers
        .iter()
        .filter(|server| server.enabled && server.connected)
        .count();
    let enabled_mcp_tool_count = external
        .mcp_servers
        .iter()
        .filter(|server| server.enabled)
        .filter_map(|server| server.tool_count)
        .sum::<usize>();
    let ready_mcp_tool_count = external
        .mcp_servers
        .iter()
        .filter(|server| server.enabled && server.connected)
        .filter_map(|server| server.tool_count)
        .sum::<usize>();
    let mcp_status = if enabled_mcp_count == 0 || ready_mcp_count == enabled_mcp_count {
        "ready"
    } else {
        "not_ready"
    };

    vec![
        serde_json::json!({
            "name": "skill_runtime",
            "status": skill_status,
            "required_by": ["skill prompt context", "skill custom tools"],
            "configured_count": external.instruction_skill_count,
            "tool_count": skill_tool_count,
        }),
        serde_json::json!({
            "name": "mcp_servers",
            "status": mcp_status,
            "required_by": ["mcp custom tools", "skill MCP bridge tools"],
            "configured_count": configured_mcp_count,
            "enabled_count": enabled_mcp_count,
            "ready_count": ready_mcp_count,
            "enabled_tool_count": enabled_mcp_tool_count,
            "ready_tool_count": ready_mcp_tool_count,
            "tool_count": ready_mcp_tool_count,
        }),
    ]
}

fn effective_external_skill_tools_for_access_profile(
    skill_tools: Vec<ExternalToolCatalogEntry>,
    access_profile: magi_core::AccessProfile,
) -> Vec<ExternalToolCatalogEntry> {
    skill_tools
        .into_iter()
        .map(|mut tool| {
            if !external_skill_tool_is_mcp(&tool) {
                return tool;
            }
            match access_profile {
                magi_core::AccessProfile::ReadOnly => {
                    if tool.status == "available" {
                        tool.status = "unavailable_in_read_only".to_string();
                    }
                    tool.access_profile_behavior = "unavailable_in_read_only".to_string();
                    tool.approval_requirement = "not_applicable".to_string();
                }
                magi_core::AccessProfile::Restricted => {
                    tool.access_profile_behavior = "restricted_blocks_high_risk".to_string();
                    tool.approval_requirement = "required".to_string();
                }
                magi_core::AccessProfile::FullAccess => {
                    tool.access_profile_behavior =
                        "full_access_skips_regular_risk_blocks".to_string();
                    tool.approval_requirement = "none".to_string();
                }
            }
            tool
        })
        .collect()
}

fn external_skill_tool_is_mcp(tool: &ExternalToolCatalogEntry) -> bool {
    tool.bridge_kind.trim().eq_ignore_ascii_case("mcp")
}

fn access_mode_for_tool(tool: BuiltinToolName) -> BuiltinToolAccessMode {
    tool.default_access_mode()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolCatalogPolicyView {
    policy_scope: &'static str,
    input_sensitive_policy: bool,
    policy_summary: &'static str,
    effective_approval_policy: &'static str,
    access_profile_behavior: &'static str,
}

fn tool_catalog_policy_view(
    tool: BuiltinToolName,
    access_profile: magi_core::AccessProfile,
) -> ToolCatalogPolicyView {
    ToolCatalogPolicyView {
        policy_scope: policy_scope_label(tool),
        input_sensitive_policy: tool.uses_input_sensitive_invocation_policy(),
        policy_summary: policy_summary(tool),
        effective_approval_policy: effective_approval_policy_label(tool, access_profile),
        access_profile_behavior: access_profile_behavior_label(tool, access_profile),
    }
}

fn policy_scope_label(tool: BuiltinToolName) -> &'static str {
    if tool.uses_input_sensitive_invocation_policy() {
        "input_sensitive"
    } else {
        "fixed"
    }
}

fn policy_summary(tool: BuiltinToolName) -> &'static str {
    match tool {
        BuiltinToolName::ShellExec => "按 action、access_mode 和命令写入迹象逐次判定风险与受限拦截",
        BuiltinToolName::FileRemove => "有效删除目标在受限访问模式下会被拦截；参数缺失时由工具校验",
        _ => "使用工具默认风险策略",
    }
}

fn risk_level_label(tool: BuiltinToolName) -> &'static str {
    match tool.default_risk_level() {
        magi_core::RiskLevel::Low => "low",
        magi_core::RiskLevel::Medium => "medium",
        magi_core::RiskLevel::High => "high",
    }
}

fn approval_requirement_label(tool: BuiltinToolName) -> &'static str {
    match tool.default_approval_requirement() {
        magi_core::ApprovalRequirement::None => "none",
        magi_core::ApprovalRequirement::Required => "required",
    }
}

fn approval_policy_summary(access_profile: magi_core::AccessProfile) -> &'static str {
    match access_profile {
        magi_core::AccessProfile::ReadOnly => {
            "当前为只读分析模式：读、搜索、诊断类工具可用；写入和外部副作用工具不可用"
        }
        magi_core::AccessProfile::Restricted => {
            "当前为受限执行模式：常规工作区操作可直接执行，高风险或输入敏感动作会被拦截"
        }
        magi_core::AccessProfile::FullAccess => {
            "当前为完全授权模式：常规风险拦截会跳过；产品级硬阻断、任务约束和角色约束仍然生效"
        }
    }
}

fn effective_approval_policy_label(
    tool: BuiltinToolName,
    access_profile: magi_core::AccessProfile,
) -> &'static str {
    match access_profile {
        magi_core::AccessProfile::ReadOnly if tool.is_access_profile_write_operation() => {
            "not_applicable"
        }
        magi_core::AccessProfile::ReadOnly => "none",
        magi_core::AccessProfile::Restricted if tool.uses_input_sensitive_invocation_policy() => {
            "input_sensitive"
        }
        magi_core::AccessProfile::Restricted => approval_requirement_label(tool),
        magi_core::AccessProfile::FullAccess
            if tool.uses_input_sensitive_invocation_policy()
                || tool.default_approval_requirement()
                    == magi_core::ApprovalRequirement::Required =>
        {
            "regular_risk_block_skipped"
        }
        magi_core::AccessProfile::FullAccess => "none",
    }
}

fn access_profile_behavior_label(
    tool: BuiltinToolName,
    access_profile: magi_core::AccessProfile,
) -> &'static str {
    match access_profile {
        magi_core::AccessProfile::ReadOnly if tool.is_access_profile_write_operation() => {
            "unavailable_in_read_only"
        }
        magi_core::AccessProfile::ReadOnly => "read_only_allowed",
        magi_core::AccessProfile::Restricted if tool.uses_input_sensitive_invocation_policy() => {
            "restricted_input_sensitive"
        }
        magi_core::AccessProfile::Restricted
            if tool.default_approval_requirement() == magi_core::ApprovalRequirement::Required =>
        {
            "restricted_blocks_high_risk"
        }
        magi_core::AccessProfile::Restricted => "restricted_allowed",
        magi_core::AccessProfile::FullAccess
            if tool.uses_input_sensitive_invocation_policy()
                || tool.default_approval_requirement()
                    == magi_core::ApprovalRequirement::Required =>
        {
            "full_access_skips_regular_risk_blocks"
        }
        magi_core::AccessProfile::FullAccess => "full_access_allowed",
    }
}

fn schema_warnings(schema: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        warnings.push("schema.type 必须是 object".to_string());
    }
    if !schema
        .get("properties")
        .is_some_and(serde_json::Value::is_object)
    {
        warnings.push("schema.properties 必须是 object".to_string());
    }
    if !schema
        .get("required")
        .is_some_and(serde_json::Value::is_array)
    {
        warnings.push("schema.required 必须是 array".to_string());
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn tool_catalog_reports_builtin_health_from_single_source() {
        let output = execute_tool_catalog(
            "{}",
            &ToolExecutionContext::default(),
            &ToolRuntimeResources::default(),
        );
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
        assert!(payload.get("access_mode").is_none());
        assert_eq!(payload["catalog_access_mode"], "read_only");
        assert_eq!(payload["current_access_profile"], "restricted");
        assert!(
            payload["approval_policy_summary"]
                .as_str()
                .expect("approval_policy_summary")
                .contains("受限执行"),
            "default tool catalog should explain restricted execution semantics"
        );
        let summary = payload["summary"].as_str().expect("summary should be text");
        assert!(
            summary.starts_with("工具目录已更新："),
            "tool catalog summary should use product language"
        );
        for forbidden in [
            "builtin_public",
            "builtin_internal",
            "skill_tools",
            "mcp_servers",
            "schema_warnings",
            "runtime_warnings",
        ] {
            assert!(
                !summary.contains(forbidden),
                "tool catalog summary should not expose internal field {forbidden}"
            );
        }
        assert_eq!(payload["schema_warning_count"], 0);
        assert!(
            payload["runtime_warning_count"]
                .as_u64()
                .expect("runtime_warning_count")
                > 0,
            "catalog should expose runtime dependency warnings when resources are missing"
        );
        assert_eq!(
            payload["builtin_total"].as_u64().expect("builtin_total"),
            BuiltinToolName::ALL
                .iter()
                .map(BuiltinToolName::category)
                .collect::<BTreeSet<_>>()
                .len() as u64
        );
        assert_eq!(payload["external_catalog_status"], "unavailable");
        let names = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"apply_patch"));
        assert!(names.contains(&"view_image"));
        assert!(names.contains(&"tool_catalog"));
        assert!(!names.contains(&"process_launch"));
        let shell_exec = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "shell_exec")
            .expect("shell_exec should be listed");
        assert_eq!(shell_exec["policy_scope"], "input_sensitive");
        assert_eq!(shell_exec["input_sensitive_policy"], true);
        assert_eq!(shell_exec["effective_approval_policy"], "input_sensitive");
        assert_eq!(
            shell_exec["access_profile_behavior"],
            "restricted_input_sensitive"
        );
        assert!(
            shell_exec["policy_summary"]
                .as_str()
                .expect("policy_summary")
                .contains("逐次判定"),
            "input-sensitive tools should explain that runtime policy is decided per invocation"
        );
        let file_remove = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "file_remove")
            .expect("file_remove should be listed");
        assert_eq!(file_remove["policy_scope"], "input_sensitive");
        assert_eq!(file_remove["input_sensitive_policy"], true);
        assert_eq!(file_remove["effective_approval_policy"], "input_sensitive");
        assert_eq!(
            file_remove["access_profile_behavior"],
            "restricted_input_sensitive"
        );
        let file_read = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "file_read")
            .expect("file_read should be listed");
        assert_eq!(file_read["policy_scope"], "fixed");
        assert_eq!(file_read["input_sensitive_policy"], false);
        let search_semantic = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "search_semantic")
            .expect("search_semantic should be listed");
        assert_eq!(search_semantic["runtime_status"], "unavailable");
        assert_eq!(
            search_semantic["runtime_warnings"],
            serde_json::json!(["知识检索能力暂不可用"])
        );
        let agent_spawn = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "agent_spawn")
            .expect("agent_spawn should be listed");
        assert_eq!(agent_spawn["runtime_status"], "unavailable");
        assert_eq!(
            agent_spawn["runtime_warnings"],
            serde_json::json!(["子代理能力暂不可用"])
        );
        let warning_text = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .flat_map(|tool| {
                tool["runtime_warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in ["KnowledgeStore", "AgentRoleRegistry", "未注入", "workspace"] {
            assert!(
                !warning_text.contains(forbidden),
                "runtime warnings should use product language, found {forbidden}"
            );
        }
        assert_eq!(
            payload["runtime_dependencies"][0]["name"],
            "knowledge_store"
        );
        assert_eq!(payload["runtime_dependencies"][0]["status"], "unavailable");
        assert_eq!(
            payload["runtime_dependencies"][2]["name"],
            "agent_role_registry"
        );
        assert_eq!(payload["runtime_dependencies"][2]["status"], "unavailable");
    }

    #[test]
    fn image_generation_tool_health_tracks_live_model_configuration() {
        let executor: crate::ImageGenerationExecutor =
            Arc::new(|_, _| panic!("catalog health must not invoke image generation"));
        let not_configured = ToolRuntimeResources {
            image_generation_executor: Some(Arc::clone(&executor)),
            image_generation_readiness_provider: Some(Arc::new(|| false)),
            ..ToolRuntimeResources::default()
        };
        let configured = ToolRuntimeResources {
            image_generation_executor: Some(executor),
            image_generation_readiness_provider: Some(Arc::new(|| true)),
            ..ToolRuntimeResources::default()
        };

        for (resources, expected) in [
            (&ToolRuntimeResources::default(), "unavailable"),
            (&not_configured, "not_ready"),
            (&configured, "ready"),
        ] {
            let payload =
                build_tool_catalog_value("{}", &ToolExecutionContext::default(), resources);
            let image_tool = payload["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .find(|tool| tool["name"] == "image_generate")
                .expect("image_generate should be listed");
            assert_eq!(image_tool["runtime_status"], expected);
            let dependency = payload["runtime_dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .find(|dependency| dependency["name"] == "image_generation_model")
                .expect("image generation dependency should be listed");
            assert_eq!(dependency["status"], expected);
        }
    }

    #[test]
    fn tool_catalog_reports_effective_access_profile_policy() {
        let full_access_context = ToolExecutionContext {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionContext::default()
        };
        let full_access_output =
            execute_tool_catalog("{}", &full_access_context, &ToolRuntimeResources::default());
        let full_access_payload: serde_json::Value =
            serde_json::from_str(&full_access_output).expect("json output");
        assert_eq!(full_access_payload["current_access_profile"], "full_access");
        assert!(
            full_access_payload["approval_policy_summary"]
                .as_str()
                .expect("approval_policy_summary")
                .contains("常规风险拦截会跳过")
        );
        let shell_exec = full_access_payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "shell_exec")
            .expect("shell_exec should be listed");
        assert_eq!(
            shell_exec["effective_approval_policy"],
            "regular_risk_block_skipped"
        );
        assert_eq!(
            shell_exec["access_profile_behavior"],
            "full_access_skips_regular_risk_blocks"
        );

        let read_only_context = ToolExecutionContext {
            access_profile: magi_core::AccessProfile::ReadOnly,
            ..ToolExecutionContext::default()
        };
        let read_only_output =
            execute_tool_catalog("{}", &read_only_context, &ToolRuntimeResources::default());
        let read_only_payload: serde_json::Value =
            serde_json::from_str(&read_only_output).expect("json output");
        assert_eq!(read_only_payload["current_access_profile"], "read_only");
        let file_write = read_only_payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "file_write")
            .expect("file_write should be listed");
        assert_eq!(file_write["effective_approval_policy"], "not_applicable");
        assert_eq!(
            file_write["access_profile_behavior"],
            "unavailable_in_read_only"
        );
    }

    #[test]
    fn tool_catalog_policy_view_matches_builtin_policy_axes_for_all_tools() {
        for access_profile in [
            magi_core::AccessProfile::ReadOnly,
            magi_core::AccessProfile::Restricted,
            magi_core::AccessProfile::FullAccess,
        ] {
            let context = ToolExecutionContext {
                access_profile,
                ..ToolExecutionContext::default()
            };
            let payload: serde_json::Value = serde_json::from_str(&execute_tool_catalog(
                r#"{"include_internal":true}"#,
                &context,
                &ToolRuntimeResources::default(),
            ))
            .expect("catalog output should be json");
            let tools = payload["tools"].as_array().expect("tools");

            for builtin in BuiltinToolName::ALL {
                let tool = tools
                    .iter()
                    .find(|tool| tool["name"] == builtin.as_str())
                    .unwrap_or_else(|| panic!("{} should be listed", builtin.as_str()));
                let expected_input_sensitive = builtin.uses_input_sensitive_invocation_policy();
                assert_eq!(
                    tool["policy_scope"],
                    if expected_input_sensitive {
                        "input_sensitive"
                    } else {
                        "fixed"
                    },
                    "{} policy_scope should match input-sensitive axis",
                    builtin.as_str()
                );
                assert_eq!(
                    tool["input_sensitive_policy"],
                    expected_input_sensitive,
                    "{} input_sensitive_policy should match builtin policy",
                    builtin.as_str()
                );

                let (expected_approval, expected_behavior) = match access_profile {
                    magi_core::AccessProfile::ReadOnly
                        if builtin.is_access_profile_write_operation() =>
                    {
                        ("not_applicable", "unavailable_in_read_only")
                    }
                    magi_core::AccessProfile::ReadOnly => ("none", "read_only_allowed"),
                    magi_core::AccessProfile::Restricted if expected_input_sensitive => {
                        ("input_sensitive", "restricted_input_sensitive")
                    }
                    magi_core::AccessProfile::Restricted
                        if builtin.default_approval_requirement()
                            == magi_core::ApprovalRequirement::Required =>
                    {
                        ("required", "restricted_blocks_high_risk")
                    }
                    magi_core::AccessProfile::Restricted => ("none", "restricted_allowed"),
                    magi_core::AccessProfile::FullAccess
                        if expected_input_sensitive
                            || builtin.default_approval_requirement()
                                == magi_core::ApprovalRequirement::Required =>
                    {
                        (
                            "regular_risk_block_skipped",
                            "full_access_skips_regular_risk_blocks",
                        )
                    }
                    magi_core::AccessProfile::FullAccess => ("none", "full_access_allowed"),
                };
                assert_eq!(
                    tool["effective_approval_policy"],
                    expected_approval,
                    "{} effective_approval_policy should match access profile",
                    builtin.as_str()
                );
                assert_eq!(
                    tool["access_profile_behavior"],
                    expected_behavior,
                    "{} access_profile_behavior should match access profile",
                    builtin.as_str()
                );
            }
        }
    }

    #[test]
    fn tool_catalog_can_include_internal_and_schema() {
        let output = execute_tool_catalog(
            r#"{"include_internal":true,"include_schema":true}"#,
            &ToolExecutionContext::default(),
            &ToolRuntimeResources::default(),
        );
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let tools = payload["tools"].as_array().expect("tools");
        let process_launch = tools
            .iter()
            .find(|tool| tool["name"] == "process_launch")
            .expect("internal tool should be included");

        assert_eq!(process_launch["public"], false);
        assert_eq!(process_launch["parameters_schema"]["type"], "object");
    }

    #[test]
    fn tool_catalog_includes_runtime_capability_dependency_provider_entries() {
        let resources = ToolRuntimeResources {
            runtime_capability_dependency_provider: Some(std::sync::Arc::new(|| {
                vec![crate::RuntimeCapabilityDependencyEntry {
                    name: "file_snapshot".to_string(),
                    status: "ready".to_string(),
                    required_by: vec!["changes/diff".to_string()],
                    role_count: None,
                    spawnable_role_count: None,
                    configured_count: None,
                    enabled_count: None,
                    ready_count: None,
                    tool_count: None,
                }]
            })),
            ..ToolRuntimeResources::default()
        };
        let context = ToolExecutionContext {
            session_id: Some(magi_core::SessionId::new("session-tool-catalog")),
            workspace_id: Some(magi_core::WorkspaceId::new("workspace-tool-catalog")),
            ..ToolExecutionContext::default()
        };

        let output = execute_tool_catalog("{}", &context, &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "file_snapshot")
            .expect("provider dependency should be included");

        assert_eq!(dependency["status"], "ready");
        assert!(dependency.get("workspace_id").is_none());
        assert!(dependency.get("session_id").is_none());
    }

    #[test]
    fn tool_catalog_includes_external_skill_and_mcp_health_when_provider_exists() {
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
                instruction_skill_count: 2,
                skill_tools: vec![crate::ExternalToolCatalogEntry {
                    source: "skill".to_string(),
                    skill_id: Some("code-review".to_string()),
                    binding_id: Some("review-mcp".to_string()),
                    name: "echo.describe".to_string(),
                    description: "回显描述".to_string(),
                    bridge_kind: "Mcp".to_string(),
                    dispatch_action: "McpToolCall".to_string(),
                    bridge_target: "loopback-mcp".to_string(),
                    access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                    risk_level: "high".to_string(),
                    approval_requirement: "required".to_string(),
                    status: "available".to_string(),
                }],
                mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                    server_id: "loopback-mcp".to_string(),
                    name: "loopback-mcp".to_string(),
                    enabled: true,
                    connected: true,
                    health: "connected".to_string(),
                    tool_count: Some(1),
                    error: None,
                }],
                mcp_tools: Vec::new(),
            })),
            ..ToolRuntimeResources::default()
        };
        let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["external_catalog_status"], "available");
        assert_eq!(payload["instruction_skill_count"], 2);
        assert_eq!(payload["skill_tool_count"], 1);
        assert!(
            payload["summary"]
                .as_str()
                .expect("summary should be text")
                .contains("2 个 Skill、1 个自定义工具")
        );
        assert_eq!(payload["mcp_server_count"], 1);
        assert_eq!(payload["connected_mcp_server_count"], 1);
        assert_eq!(payload["skill_tools"][0]["name"], "echo.describe");
        assert_eq!(payload["mcp_servers"][0]["health"], "connected");
        let skill_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "skill_runtime")
            .expect("skill runtime dependency should be listed");
        let mcp_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "mcp_servers")
            .expect("mcp dependency should be listed");
        assert_eq!(skill_dependency["status"], "ready");
        assert_eq!(skill_dependency["configured_count"], 2);
        assert_eq!(skill_dependency["tool_count"], 1);
        assert_eq!(mcp_dependency["status"], "ready");
        assert_eq!(mcp_dependency["enabled_count"], 1);
        assert_eq!(mcp_dependency["ready_count"], 1);
        assert_eq!(mcp_dependency["enabled_tool_count"], 1);
        assert_eq!(mcp_dependency["ready_tool_count"], 1);
        assert_eq!(mcp_dependency["tool_count"], 1);
    }

    #[test]
    fn tool_catalog_does_not_call_external_provider_when_external_disabled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(Arc::new(move || {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                ExternalToolCatalogSnapshot {
                    instruction_skill_count: 1,
                    skill_tools: vec![crate::ExternalToolCatalogEntry {
                        source: "skill".to_string(),
                        skill_id: Some("slow-skill".to_string()),
                        binding_id: Some("slow-mcp".to_string()),
                        name: "slow.describe".to_string(),
                        description: "不应被加载的外部工具".to_string(),
                        bridge_kind: "Mcp".to_string(),
                        dispatch_action: "McpToolCall".to_string(),
                        bridge_target: "slow-mcp".to_string(),
                        access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                        risk_level: "high".to_string(),
                        approval_requirement: "required".to_string(),
                        status: "available".to_string(),
                    }],
                    mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                        server_id: "slow-mcp".to_string(),
                        name: "slow-mcp".to_string(),
                        enabled: true,
                        connected: true,
                        health: "connected".to_string(),
                        tool_count: Some(1),
                        error: None,
                    }],
                    mcp_tools: Vec::new(),
                }
            })),
            ..ToolRuntimeResources::default()
        };

        let output = execute_tool_catalog(
            r#"{"includeExternal":false,"includeMcpServers":false}"#,
            &ToolExecutionContext::default(),
            &resources,
        );
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "includeExternal=false must not hydrate external tool providers"
        );
        assert_eq!(payload["external_catalog_status"], "disabled");
        assert_eq!(payload["skill_tool_count"], 0);
        assert_eq!(payload["mcp_server_count"], 0);
        let skill_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "skill_runtime")
            .expect("skill runtime dependency should be listed");
        let mcp_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "mcp_servers")
            .expect("mcp dependency should be listed");
        assert_eq!(skill_dependency["status"], "disabled");
        assert_eq!(mcp_dependency["status"], "disabled");
    }

    #[test]
    fn tool_catalog_reuses_single_external_snapshot_for_dependencies() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(Arc::new(move || {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                ExternalToolCatalogSnapshot {
                    instruction_skill_count: 1,
                    skill_tools: vec![crate::ExternalToolCatalogEntry {
                        source: "skill".to_string(),
                        skill_id: Some("code-review".to_string()),
                        binding_id: Some("review-mcp".to_string()),
                        name: "echo.describe".to_string(),
                        description: "回显描述".to_string(),
                        bridge_kind: "Mcp".to_string(),
                        dispatch_action: "McpToolCall".to_string(),
                        bridge_target: "loopback-mcp".to_string(),
                        access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                        risk_level: "high".to_string(),
                        approval_requirement: "required".to_string(),
                        status: "available".to_string(),
                    }],
                    mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                        server_id: "loopback-mcp".to_string(),
                        name: "loopback-mcp".to_string(),
                        enabled: true,
                        connected: true,
                        health: "connected".to_string(),
                        tool_count: Some(1),
                        error: None,
                    }],
                    mcp_tools: Vec::new(),
                }
            })),
            ..ToolRuntimeResources::default()
        };

        let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "external provider should be called once and reused for dependency health"
        );
        assert_eq!(payload["external_catalog_status"], "available");
        assert_eq!(payload["skill_tool_count"], 1);
        assert_eq!(payload["mcp_server_count"], 1);
        let skill_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "skill_runtime")
            .expect("skill runtime dependency should be listed");
        let mcp_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "mcp_servers")
            .expect("mcp dependency should be listed");
        assert_eq!(skill_dependency["tool_count"], 1);
        assert_eq!(mcp_dependency["ready_tool_count"], 1);
    }

    #[test]
    fn tool_catalog_reports_effective_external_mcp_skill_policy_by_access_profile() {
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
                instruction_skill_count: 1,
                skill_tools: vec![crate::ExternalToolCatalogEntry {
                    source: "skill".to_string(),
                    skill_id: Some("code-review".to_string()),
                    binding_id: Some("review-mcp".to_string()),
                    name: "echo.describe".to_string(),
                    description: "回显描述".to_string(),
                    bridge_kind: "Mcp".to_string(),
                    dispatch_action: "McpToolCall".to_string(),
                    bridge_target: "loopback-mcp".to_string(),
                    access_profile_behavior: "restricted_blocks_high_risk".to_string(),
                    risk_level: "high".to_string(),
                    approval_requirement: "required".to_string(),
                    status: "available".to_string(),
                }],
                mcp_servers: Vec::new(),
                mcp_tools: Vec::new(),
            })),
            ..ToolRuntimeResources::default()
        };

        let read_only_context = ToolExecutionContext {
            access_profile: magi_core::AccessProfile::ReadOnly,
            ..ToolExecutionContext::default()
        };
        let read_only_payload: serde_json::Value =
            serde_json::from_str(&execute_tool_catalog("{}", &read_only_context, &resources))
                .expect("json output");
        assert_eq!(
            read_only_payload["skill_tools"][0]["status"],
            "unavailable_in_read_only"
        );
        assert_eq!(
            read_only_payload["skill_tools"][0]["access_profile_behavior"],
            "unavailable_in_read_only"
        );
        assert_eq!(
            read_only_payload["skill_tools"][0]["approval_requirement"],
            "not_applicable"
        );

        let full_access_context = ToolExecutionContext {
            access_profile: magi_core::AccessProfile::FullAccess,
            ..ToolExecutionContext::default()
        };
        let full_access_payload: serde_json::Value = serde_json::from_str(&execute_tool_catalog(
            "{}",
            &full_access_context,
            &resources,
        ))
        .expect("json output");
        assert_eq!(full_access_payload["skill_tools"][0]["status"], "available");
        assert_eq!(
            full_access_payload["skill_tools"][0]["access_profile_behavior"],
            "full_access_skips_regular_risk_blocks"
        );
        assert_eq!(
            full_access_payload["skill_tools"][0]["approval_requirement"],
            "none"
        );
    }

    #[test]
    fn tool_catalog_treats_disabled_mcp_servers_as_configured_not_active() {
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
                instruction_skill_count: 0,
                skill_tools: Vec::new(),
                mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                    server_id: "disabled-mcp".to_string(),
                    name: "disabled-mcp".to_string(),
                    enabled: false,
                    connected: true,
                    health: "disabled".to_string(),
                    tool_count: Some(3),
                    error: Some("mcp_connection_failed".to_string()),
                }],
                mcp_tools: Vec::new(),
            })),
            ..ToolRuntimeResources::default()
        };
        let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let mcp_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "mcp_servers")
            .expect("mcp dependency should be listed");

        assert_eq!(payload["mcp_server_count"], 1);
        assert_eq!(payload["connected_mcp_server_count"], 0);
        assert_eq!(mcp_dependency["status"], "ready");
        assert_eq!(mcp_dependency["configured_count"], 1);
        assert_eq!(mcp_dependency["enabled_count"], 0);
        assert_eq!(mcp_dependency["ready_count"], 0);
        assert_eq!(mcp_dependency["enabled_tool_count"], 0);
        assert_eq!(mcp_dependency["ready_tool_count"], 0);
        assert_eq!(mcp_dependency["tool_count"], 0);
    }

    #[test]
    fn tool_catalog_counts_only_connected_mcp_tools_as_ready() {
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(Arc::new(|| ExternalToolCatalogSnapshot {
                instruction_skill_count: 0,
                skill_tools: Vec::new(),
                mcp_servers: vec![crate::ExternalMcpServerCatalogEntry {
                    server_id: "disconnected-mcp".to_string(),
                    name: "disconnected-mcp".to_string(),
                    enabled: true,
                    connected: false,
                    health: "disconnected".to_string(),
                    tool_count: Some(7),
                    error: Some("mcp_connection_failed".to_string()),
                }],
                mcp_tools: Vec::new(),
            })),
            ..ToolRuntimeResources::default()
        };
        let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let mcp_dependency = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "mcp_servers")
            .expect("mcp dependency should be listed");

        assert_eq!(mcp_dependency["status"], "not_ready");
        assert_eq!(mcp_dependency["enabled_count"], 1);
        assert_eq!(mcp_dependency["ready_count"], 0);
        assert_eq!(mcp_dependency["enabled_tool_count"], 7);
        assert_eq!(mcp_dependency["ready_tool_count"], 0);
        assert_eq!(
            mcp_dependency["tool_count"], 0,
            "tool_count must mean currently usable MCP tools, not configured tools on disconnected servers"
        );
    }

    #[test]
    fn tool_catalog_reports_agent_role_registry_health_when_provider_exists() {
        let resources = ToolRuntimeResources {
            agent_role_catalog_provider: Some(Arc::new(|| {
                vec![
                    crate::AgentRoleCatalogEntry {
                        role_id: "coordinator".to_string(),
                        spawnable: false,
                        coordinator_mode: true,
                        supported_kinds: vec!["local_agent".to_string()],
                        parallelism_limit: None,
                        status: "coordinator_only".to_string(),
                    },
                    crate::AgentRoleCatalogEntry {
                        role_id: "executor".to_string(),
                        spawnable: true,
                        coordinator_mode: false,
                        supported_kinds: vec!["local_agent".to_string()],
                        parallelism_limit: Some(2),
                        status: "spawnable".to_string(),
                    },
                ]
            })),
            ..ToolRuntimeResources::default()
        };

        let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let agent_spawn = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "agent_spawn")
            .expect("agent_spawn should be listed");
        let agent_wait = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "agent_wait")
            .expect("agent_wait should be listed");

        assert_eq!(payload["agent_role_catalog_status"], "available");
        assert_eq!(payload["agent_role_count"], 2);
        assert_eq!(payload["spawnable_agent_role_count"], 1);
        assert_eq!(agent_spawn["runtime_status"], "ready");
        assert_eq!(agent_wait["runtime_status"], "ready");
        assert_eq!(payload["runtime_dependencies"][2]["status"], "ready");
        assert_eq!(payload["agent_roles"][0]["role_id"], "coordinator");
        assert_eq!(payload["agent_roles"][1]["role_id"], "executor");
        assert_eq!(payload["agent_roles"][1]["parallelism_limit"], 2);
    }

    #[test]
    fn tool_catalog_reports_initialized_knowledge_capabilities_without_workspace_context() {
        let resources = ToolRuntimeResources {
            knowledge_store: Some(std::sync::Arc::new(
                magi_knowledge_store::KnowledgeStore::new(),
            )),
            ..ToolRuntimeResources::default()
        };

        let payload = build_tool_catalog_value("{}", &ToolExecutionContext::default(), &resources);
        let tools = payload["tools"].as_array().expect("tools should be listed");
        for name in ["knowledge_query", "search_semantic", "code_symbols"] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} should be listed"));
            assert_eq!(tool["runtime_status"], "ready");
            assert_eq!(tool["runtime_warnings"], serde_json::json!([]));
        }

        let code_index = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies should be listed")
            .iter()
            .find(|dependency| dependency["name"] == "workspace_code_index")
            .expect("workspace code index dependency should be listed");
        assert_eq!(code_index["status"], "ready");
        assert!(code_index.get("workspace_id").is_none());
        assert!(code_index.get("file_count").is_none());
        assert!(code_index.get("last_indexed").is_none());
        assert!(code_index.get("cache_status").is_none());
    }

    #[test]
    fn tool_catalog_product_health_is_independent_of_workspace_index_state() {
        let resources = ToolRuntimeResources {
            knowledge_store: Some(std::sync::Arc::new(
                magi_knowledge_store::KnowledgeStore::new(),
            )),
            ..ToolRuntimeResources::default()
        };
        let context = ToolExecutionContext {
            workspace_id: Some(magi_core::WorkspaceId::new("empty-workspace")),
            working_directory: Some(std::env::temp_dir().join("missing-magi-workspace")),
            ..ToolExecutionContext::default()
        };

        let payload = build_tool_catalog_value("{}", &context, &resources);
        let search_semantic = payload["tools"]
            .as_array()
            .expect("tools should be listed")
            .iter()
            .find(|tool| tool["name"] == "search_semantic")
            .expect("search_semantic should be listed");
        let code_index = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies should be listed")
            .iter()
            .find(|dependency| dependency["name"] == "workspace_code_index")
            .expect("workspace code index dependency should be listed");

        assert_eq!(search_semantic["runtime_status"], "ready");
        assert_eq!(code_index["status"], "ready");
        assert!(code_index.get("workspace_id").is_none());
        assert!(code_index.get("file_count").is_none());
    }
}
