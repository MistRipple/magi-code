use crate::{
    AgentRoleCatalogEntry, BuiltinToolAccessMode, BuiltinToolName, ExternalToolCatalogSnapshot,
    ToolExecutionContext, ToolRuntimeResources,
};

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
    let mut public_count = 0usize;
    let mut internal_count = 0usize;
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
    let runtime_health = RuntimeHealth::from_context(context, resources, &agent_roles);
    let mut runtime_warning_count = 0usize;

    for tool in BuiltinToolName::ALL {
        let is_public = tool.is_public_tool_surface();
        if is_public {
            public_count += 1;
        } else {
            internal_count += 1;
        }
        if !is_public && !include_internal {
            continue;
        }

        let schema = tool.parameters_schema();
        let schema_warnings = schema_warnings(&schema);
        schema_warning_count += schema_warnings.len();
        let runtime_status = runtime_health.tool_status(tool);
        runtime_warning_count += runtime_status.warnings.len();

        let mut item = serde_json::json!({
            "name": tool.as_str(),
            "category": tool.category(),
            "public": is_public,
            "runtime_internal": tool.is_runtime_internal_tool_call(),
            "access_mode": access_mode_for_tool(tool).as_str(),
            "policy_scope": policy_scope_label(tool),
            "input_sensitive_policy": tool.uses_input_sensitive_invocation_policy(),
            "policy_summary": policy_summary(tool),
            "risk_level": risk_level_label(tool),
            "approval_requirement": approval_requirement_label(tool),
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
            .unwrap_or_default()
    } else {
        ExternalToolCatalogSnapshot::default()
    };
    let skill_tool_count = external_catalog.skill_tools.len();
    let mcp_server_count = if include_mcp_servers {
        external_catalog.mcp_servers.len()
    } else {
        0
    };
    let connected_mcp_server_count = if include_mcp_servers {
        external_catalog
            .mcp_servers
            .iter()
            .filter(|server| server.enabled && server.connected)
            .count()
    } else {
        0
    };
    let enabled_mcp_server_count = if include_mcp_servers {
        external_catalog
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .count()
    } else {
        0
    };
    let agent_role_count = agent_roles.len();
    let spawnable_agent_role_count = agent_roles.iter().filter(|role| role.spawnable).count();
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
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "summary": tool_catalog_summary(
            public_count,
            skill_tool_count,
            connected_mcp_server_count,
            enabled_mcp_server_count,
            spawnable_agent_role_count,
            agent_role_count,
            schema_warning_count,
            runtime_warning_count,
        ),
        "total": tools.len() + skill_tool_count,
        "builtin_total": BuiltinToolName::ALL.len(),
        "public_count": public_count,
        "internal_count": internal_count,
        "schema_warning_count": schema_warning_count,
        "runtime_warning_count": runtime_warning_count,
        "runtime_dependencies": runtime_health.dependencies_json(context, resources),
        "external_catalog_status": external_catalog_status,
        "skill_tool_count": skill_tool_count,
        "mcp_server_count": mcp_server_count,
        "connected_mcp_server_count": connected_mcp_server_count,
        "agent_role_catalog_status": agent_role_catalog_status,
        "agent_role_count": agent_role_count,
        "spawnable_agent_role_count": spawnable_agent_role_count,
        "tools": tools,
        "skill_tools": if include_external {
            serde_json::to_value(external_catalog.skill_tools).unwrap_or_else(|_| serde_json::json!([]))
        } else {
            serde_json::json!([])
        },
        "mcp_servers": if include_external && include_mcp_servers {
            serde_json::to_value(external_catalog.mcp_servers).unwrap_or_else(|_| serde_json::json!([]))
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

fn tool_catalog_summary(
    public_count: usize,
    skill_tool_count: usize,
    connected_mcp_server_count: usize,
    enabled_mcp_server_count: usize,
    spawnable_agent_role_count: usize,
    agent_role_count: usize,
    schema_warning_count: usize,
    runtime_warning_count: usize,
) -> String {
    let mut summary = format!(
        "工具目录已更新：{public_count} 个内置工具、{skill_tool_count} 个 Skill 工具、MCP 启用服务 {connected_mcp_server_count}/{enabled_mcp_server_count} 可用、子代理 {spawnable_agent_role_count}/{agent_role_count} 可派发"
    );
    let warning_count = schema_warning_count + runtime_warning_count;
    if warning_count > 0 {
        summary.push_str(&format!("，{warning_count} 项能力需关注"));
    }
    summary
}

struct RuntimeHealth {
    knowledge_store_available: bool,
    workspace_id: Option<String>,
    workspace_code_index_ready: bool,
    workspace_code_index_file_count: Option<usize>,
    workspace_code_index_last_indexed: Option<u64>,
    workspace_code_index_cache_status: Option<String>,
    workspace_code_index_cache_error_code: Option<String>,
    agent_role_registry_available: bool,
    agent_role_count: usize,
    spawnable_agent_role_count: usize,
}

struct RuntimeToolStatus {
    status: &'static str,
    warnings: Vec<String>,
}

impl RuntimeHealth {
    fn from_context(
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
        agent_roles: &[AgentRoleCatalogEntry],
    ) -> Self {
        let knowledge_store_available = resources.knowledge_store.is_some();
        let workspace_id = context
            .workspace_id
            .as_ref()
            .map(|id| id.as_str().to_string());
        let (
            workspace_code_index_ready,
            workspace_code_index_file_count,
            workspace_code_index_last_indexed,
            workspace_code_index_cache_status,
            workspace_code_index_cache_error_code,
        ) = match (
            resources.knowledge_store.as_ref(),
            context.workspace_id.as_ref(),
        ) {
            (Some(store), Some(workspace_id)) => {
                let summary = store.code_index_summary_for_workspace(workspace_id);
                let stats = store.workspace_index_stats(workspace_id);
                (
                    stats
                        .as_ref()
                        .is_some_and(|stats| stats.is_ready && stats.total_documents > 0),
                    summary.as_ref().map(|summary| summary.files.len()),
                    summary.as_ref().map(|summary| summary.last_indexed),
                    stats
                        .as_ref()
                        .map(|stats| stats.index_cache_status.to_string()),
                    stats
                        .as_ref()
                        .and_then(|stats| stats.index_cache_error_code.map(str::to_string)),
                )
            }
            _ => (false, None, None, None, None),
        };

        Self {
            knowledge_store_available,
            workspace_id,
            workspace_code_index_ready,
            workspace_code_index_file_count,
            workspace_code_index_last_indexed,
            workspace_code_index_cache_status,
            workspace_code_index_cache_error_code,
            agent_role_registry_available: resources.agent_role_catalog_provider.is_some(),
            agent_role_count: agent_roles.len(),
            spawnable_agent_role_count: agent_roles.iter().filter(|role| role.spawnable).count(),
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
            _ => RuntimeToolStatus {
                status: "ready",
                warnings: Vec::new(),
            },
        }
    }

    fn knowledge_tool_status(&self) -> RuntimeToolStatus {
        if !self.knowledge_store_available {
            return RuntimeToolStatus {
                status: "unavailable",
                warnings: vec!["知识检索能力暂不可用".to_string()],
            };
        }
        if self.workspace_id.is_none() {
            return RuntimeToolStatus {
                status: "missing_context",
                warnings: vec!["需要先选择工作区".to_string()],
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
        let base = self.knowledge_tool_status();
        if base.status != "ready" {
            return base;
        }
        if !self.workspace_code_index_ready {
            return RuntimeToolStatus {
                status: "not_ready",
                warnings: vec!["当前工作区本地索引尚未就绪".to_string()],
            };
        }
        if self.workspace_code_index_cache_status.as_deref() == Some("degraded") {
            return RuntimeToolStatus {
                status: "degraded",
                warnings: vec!["本地索引缓存暂不可持久化，当前检索仍可用".to_string()],
            };
        }
        RuntimeToolStatus {
            status: "ready",
            warnings: Vec::new(),
        }
    }

    fn dependencies_json(
        &self,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
    ) -> serde_json::Value {
        let mut dependencies = vec![
            serde_json::json!({
                "name": "knowledge_store",
                "status": if self.knowledge_store_available { "available" } else { "unavailable" },
                "required_by": ["knowledge_query", "search_semantic", "code_symbols"],
            }),
            serde_json::json!({
                "name": "workspace_code_index",
                "status": self.workspace_code_index_status(),
                "workspace_id": self.workspace_id,
                "file_count": self.workspace_code_index_file_count,
                "last_indexed": self.workspace_code_index_last_indexed,
                "cache_status": self.workspace_code_index_cache_status,
                "cache_error_code": self.workspace_code_index_cache_error_code,
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
        dependencies.extend(external_capability_dependencies(resources));

        if let Some(provider) = &resources.runtime_capability_dependency_provider {
            dependencies.extend(provider(context).into_iter().map(|entry| {
                serde_json::to_value(entry)
                    .expect("runtime capability dependency entry should serialize")
            }));
        }

        serde_json::Value::Array(dependencies)
    }

    fn workspace_code_index_status(&self) -> &'static str {
        if !self.knowledge_store_available {
            "unavailable"
        } else if self.workspace_id.is_none() {
            "missing_context"
        } else if self.workspace_code_index_ready
            && self.workspace_code_index_cache_status.as_deref() == Some("degraded")
        {
            "degraded"
        } else if self.workspace_code_index_ready {
            "ready"
        } else {
            "not_ready"
        }
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

fn external_capability_dependencies(resources: &ToolRuntimeResources) -> Vec<serde_json::Value> {
    let Some(provider) = &resources.external_tool_catalog_provider else {
        return vec![
            serde_json::json!({
                "name": "skill_runtime",
                "status": "unavailable",
                "required_by": ["skill prompt context", "skill custom tools"],
                "configured_count": 0,
                "tool_count": 0,
            }),
            serde_json::json!({
                "name": "mcp_servers",
                "status": "unavailable",
                "required_by": ["mcp custom tools", "skill MCP bridge tools"],
                "configured_count": 0,
                "enabled_count": 0,
                "ready_count": 0,
                "tool_count": 0,
            }),
        ];
    };

    let external = provider();
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
    let mcp_tool_count = external
        .mcp_servers
        .iter()
        .filter(|server| server.enabled)
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
            "configured_count": skill_tool_count,
            "tool_count": skill_tool_count,
        }),
        serde_json::json!({
            "name": "mcp_servers",
            "status": mcp_status,
            "required_by": ["mcp custom tools", "skill MCP bridge tools"],
            "configured_count": configured_mcp_count,
            "enabled_count": enabled_mcp_count,
            "ready_count": ready_mcp_count,
            "tool_count": mcp_tool_count,
        }),
    ]
}

fn access_mode_for_tool(tool: BuiltinToolName) -> BuiltinToolAccessMode {
    tool.default_access_mode()
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
        BuiltinToolName::ShellExec => "按 action、access_mode 和命令写入迹象逐次判定风险与审批要求",
        BuiltinToolName::FileRemove => "普通单文件删除为中风险；递归、force 或高危目标删除需要审批",
        _ => "使用工具默认风险与审批策略",
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

    #[test]
    fn tool_catalog_reports_builtin_health_from_single_source() {
        let output = execute_tool_catalog(
            "{}",
            &ToolExecutionContext::default(),
            &ToolRuntimeResources::default(),
        );
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
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
            BuiltinToolName::ALL.len() as u64
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
        assert!(
            shell_exec["policy_summary"]
                .as_str()
                .expect("policy_summary")
                .contains("逐次判定"),
            "input-sensitive tools should explain that runtime policy is decided per invocation"
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
            runtime_capability_dependency_provider: Some(std::sync::Arc::new(|context| {
                vec![crate::RuntimeCapabilityDependencyEntry {
                    name: "file_snapshot".to_string(),
                    status: if context.session_id.is_some() {
                        "not_ready".to_string()
                    } else {
                        "missing_context".to_string()
                    },
                    required_by: vec!["changes/diff".to_string()],
                    workspace_id: context.workspace_id.as_ref().map(ToString::to_string),
                    session_id: context.session_id.as_ref().map(ToString::to_string),
                    file_count: None,
                    last_indexed: None,
                    cache_status: None,
                    cache_error_code: None,
                    role_count: None,
                    spawnable_role_count: None,
                    snapshot_active: Some(false),
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

        assert_eq!(dependency["status"], "not_ready");
        assert_eq!(dependency["workspace_id"], "workspace-tool-catalog");
        assert_eq!(dependency["session_id"], "session-tool-catalog");
        assert_eq!(dependency["snapshot_active"], false);
    }

    #[test]
    fn tool_catalog_includes_external_skill_and_mcp_health_when_provider_exists() {
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(std::sync::Arc::new(|| {
                ExternalToolCatalogSnapshot {
                    skill_tools: vec![crate::ExternalToolCatalogEntry {
                        source: "skill".to_string(),
                        skill_id: Some("code-review".to_string()),
                        binding_id: Some("review-mcp".to_string()),
                        name: "echo.describe".to_string(),
                        description: "回显描述".to_string(),
                        bridge_kind: "Mcp".to_string(),
                        dispatch_action: "McpToolCall".to_string(),
                        bridge_target: "loopback-mcp".to_string(),
                        access_profile_behavior: "restricted_requires_approval".to_string(),
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
                }
            })),
            ..ToolRuntimeResources::default()
        };
        let output = execute_tool_catalog("{}", &ToolExecutionContext::default(), &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["external_catalog_status"], "available");
        assert_eq!(payload["skill_tool_count"], 1);
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
        assert_eq!(skill_dependency["tool_count"], 1);
        assert_eq!(mcp_dependency["status"], "ready");
        assert_eq!(mcp_dependency["enabled_count"], 1);
        assert_eq!(mcp_dependency["ready_count"], 1);
        assert_eq!(mcp_dependency["tool_count"], 1);
    }

    #[test]
    fn tool_catalog_treats_disabled_mcp_servers_as_configured_not_active() {
        let resources = ToolRuntimeResources {
            external_tool_catalog_provider: Some(std::sync::Arc::new(|| {
                ExternalToolCatalogSnapshot {
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
                }
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
        assert_eq!(mcp_dependency["tool_count"], 0);
    }

    #[test]
    fn tool_catalog_reports_agent_role_registry_health_when_provider_exists() {
        let resources = ToolRuntimeResources {
            agent_role_catalog_provider: Some(std::sync::Arc::new(|| {
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
    fn tool_catalog_reports_ready_workspace_code_index() {
        let root = std::env::temp_dir().join(format!(
            "magi-tool-catalog-index-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).expect("create test workspace");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn catalog_index_probe() -> bool { true }\n",
        )
        .expect("write test source");

        let workspace_id = magi_core::WorkspaceId::new("workspace-tool-catalog-index");
        let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
        store.build_workspace_index(&workspace_id, &root);
        let resources = ToolRuntimeResources {
            knowledge_store: Some(store),
            ..ToolRuntimeResources::default()
        };
        let context = ToolExecutionContext {
            workspace_id: Some(workspace_id),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = execute_tool_catalog("{}", &context, &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let search_semantic = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "search_semantic")
            .expect("search_semantic should be listed");
        let code_symbols = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "code_symbols")
            .expect("code_symbols should be listed");

        assert_eq!(search_semantic["runtime_status"], "ready");
        assert_eq!(code_symbols["runtime_status"], "ready");
        assert_eq!(
            payload["runtime_dependencies"][1]["name"],
            "workspace_code_index"
        );
        assert_eq!(payload["runtime_dependencies"][1]["status"], "ready");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tool_catalog_reports_degraded_workspace_code_index_cache() {
        let root = std::env::temp_dir().join(format!(
            "magi-tool-catalog-index-degraded-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).expect("create test workspace");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn catalog_index_degraded_probe() -> bool { true }\n",
        )
        .expect("write test source");
        std::fs::write(root.join(".magi"), "occupied").expect("occupy cache root");

        let workspace_id = magi_core::WorkspaceId::new("workspace-tool-catalog-index-degraded");
        let store = std::sync::Arc::new(magi_knowledge_store::KnowledgeStore::new());
        store.build_workspace_index(&workspace_id, &root);
        let resources = ToolRuntimeResources {
            knowledge_store: Some(store),
            ..ToolRuntimeResources::default()
        };
        let context = ToolExecutionContext {
            workspace_id: Some(workspace_id),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = execute_tool_catalog("{}", &context, &resources);
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");
        let search_semantic = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "search_semantic")
            .expect("search_semantic should be listed");
        let workspace_code_index = payload["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies")
            .iter()
            .find(|dependency| dependency["name"] == "workspace_code_index")
            .expect("workspace_code_index dependency should be listed");

        assert_eq!(search_semantic["runtime_status"], "degraded");
        assert_eq!(workspace_code_index["status"], "degraded");
        assert_eq!(workspace_code_index["cache_status"], "degraded");
        assert_eq!(
            workspace_code_index["cache_error_code"],
            "index_cache_save_failed"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
