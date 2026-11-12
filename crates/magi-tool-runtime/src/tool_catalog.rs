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
    let flag = |name: &str, default: bool| {
        request
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    };
    let include_internal = flag("include_internal", false);
    let include_schema = flag("include_schema", false);
    let include_external = flag("include_external", true);
    let include_mcp_servers = flag("include_mcp_servers", true);
    let include_agent_roles = flag("include_agent_roles", true);
    let tool_names = request
        .as_ref()
        .and_then(|value| value.get("tool_names"))
        .and_then(serde_json::Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<BTreeSet<_>>()
        });
    let query = request
        .as_ref()
        .and_then(|value| value["query"].as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let matches = |name: &str, title: &str, description: &str| {
        tool_names
            .as_ref()
            .map(|names| names.contains(name))
            .unwrap_or_else(|| {
                query.split_whitespace().all(|word| {
                    [name, title, description]
                        .iter()
                        .any(|text| text.to_lowercase().contains(word))
                })
            })
    };

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
    // 产品目录保持全局；模型自检按项目身份与实际执行目录分别裁剪。
    let model_execution_without_workspace =
        context.task_id.is_some() && context.workspace_id.is_none();
    let model_execution_without_directory = context.task_id.is_some()
        && context.workspace_id.is_none()
        && context.working_directory.is_none();

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
        if !matches(tool.as_str(), tool.category(), tool.description()) {
            continue;
        }

        let schema = tool.parameters_schema();
        let schema_warnings = schema_warnings(&schema);
        schema_warning_count += schema_warnings.len();
        let runtime_status =
            if model_execution_without_workspace && tool.requires_workspace_context() {
                RuntimeToolStatus {
                    status: "unavailable",
                    warnings: vec!["当前任务未绑定工作区".to_string()],
                }
            } else if model_execution_without_directory && tool.requires_execution_directory() {
                RuntimeToolStatus {
                    status: "unavailable",
                    warnings: vec!["当前任务没有执行目录".into()],
                }
            } else {
                runtime_health.tool_status(tool)
            };
        runtime_warning_count += runtime_status.warnings.len();
        let policy_view = tool_catalog_policy_view(tool, access_profile);
        let model_call_scope = tool.model_call_scope().as_str();

        // Keep catalog visibility on the same caller-scope contract as the
        // execution preflight. SessionMainline is the trusted root even when
        // the conversation profile intentionally has no Task; delegation still
        // requires a concrete Task, so a taskless mainline never gets agent_*.
        let is_root_coordinator = context.caller_scope == crate::ToolCallerScope::SessionMainline;
        let can_delegate = matches!(
            context.caller_scope,
            crate::ToolCallerScope::SessionMainline | crate::ToolCallerScope::DelegatingTask
        ) && context.task_id.is_some();
        let scope_allowed = tool.model_call_scope().allows(
            context.task_id.is_some(),
            is_root_coordinator,
            can_delegate,
        );
        let policy_rejection = resources
            .execution_policy
            .as_ref()
            .and_then(|policy| policy.tool_name_rejection(tool.as_str()));
        let reason = if !scope_allowed {
            Some("model_call_scope_mismatch")
        } else if let Some(rejection) = policy_rejection {
            Some(rejection.reason_code())
        } else if access_profile == magi_core::AccessProfile::ReadOnly
            && tool.is_access_profile_write_operation()
        {
            Some("read_only_access_profile")
        } else if model_execution_without_workspace && tool.requires_workspace_context() {
            Some("workspace_required")
        } else if model_execution_without_directory && tool.requires_execution_directory() {
            Some("execution_directory_required")
        } else if runtime_status.status != "ready" {
            Some("runtime_unavailable")
        } else {
            None
        };
        let availability = match reason {
            Some(
                "model_call_scope_mismatch"
                | "tool_explicitly_denied"
                | "tool_not_in_allowed_scope"
                | "read_only_access_profile",
            ) => "denied",
            Some(_) => "unavailable",
            None if policy_view.effective_approval_policy == "required" => "needs_approval",
            None => "available",
        };
        let mut item = serde_json::json!({
            "name": tool.as_str(),
            "category": tool.category(),
            "public": is_public,
            "runtime_internal": tool.is_runtime_internal_tool_call(),
            // runtime_internal 描述的是实现入口，不等于模型不可调用。尤其是
            // agent_spawn / agent_wait：模型通过任务编排层调用，不能落到普通
            // BuiltinTool::execute。把可调用范围单独输出，避免模型把两者混为一谈。
            "model_call_scope": model_call_scope,
            "workspace_required": tool.requires_workspace_context(),
            "execution_directory_required": tool.requires_execution_directory(),
            "repository_required": tool.requires_repository(),
            "model_available": reason.is_none(),
            "availability": availability,
            "reason_code": reason,
            "schema_included": include_schema && tool_names.as_ref().is_none_or(|names| names.contains(tool.as_str())),
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
        if include_schema
            && tool_names
                .as_ref()
                .is_none_or(|names| names.contains(tool.as_str()))
        {
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
    let external_catalog_status = if include_external
        && (resources.external_tool_catalog_provider.is_some()
            || resources.plugin_tool_catalog_provider.is_some())
    {
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
    let plugin_tools = if include_external {
        resources
            .plugin_tool_catalog_provider
            .as_ref()
            .map(|provider| provider())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let external_tool_value =
        |tool: serde_json::Value, name: &str, read_only: bool, executor_available: bool| {
            external_tool_catalog_value(
                tool,
                name,
                read_only,
                context,
                resources,
                executor_available,
                include_schema
                    && tool_names
                        .as_ref()
                        .is_some_and(|names| names.contains(name)),
            )
        };
    let mcp_values = external_mcp_tools
        .iter()
        .filter(|tool| matches(&tool.model_tool_name, &tool.tool_name, &tool.description))
        .map(|tool| {
            external_tool_value(
                serde_json::json!(tool),
                &tool.model_tool_name,
                tool.read_only,
                resources.external_mcp_tool_executor.is_some(),
            )
        })
        .collect::<Vec<_>>();
    let plugin_values = plugin_tools
        .iter()
        .filter(|tool| matches(&tool.model_tool_name, &tool.title, &tool.description))
        .map(|tool| {
            external_tool_value(
                serde_json::json!(tool),
                &tool.model_tool_name,
                tool.read_only,
                resources.plugin_tool_executor.is_some(),
            )
        })
        .collect::<Vec<_>>();
    let mut summary = tool_catalog_summary(ToolCatalogSummaryInput {
        public_count,
        instruction_skill_count,
        skill_tool_count,
        connected_mcp_server_count,
        enabled_mcp_server_count,
        spawnable_agent_role_count,
        agent_role_count,
        schema_warning_count,
        runtime_warning_count,
    });

    if !plugin_tools.is_empty() {
        summary.push_str(&format!("，{} 个插件工具", plugin_tools.len()));
    }

    serde_json::json!({
        "tool": "tool_catalog",
        "status": "succeeded",
        "catalog_access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "current_access_profile": access_profile.as_str(),
        "workspace_bound": context.workspace_id.is_some(),
        "approval_policy_summary": approval_policy_summary(access_profile),
        "summary": summary,
        "total": tools.len() + skill_tool_count + external_mcp_tools.len() + plugin_tools.len(),
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
            mcp_values
        } else {
            Vec::new()
        },
        "plugin_tools": plugin_values,
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
            BuiltinToolName::KnowledgeQuery | BuiltinToolName::KnowledgeGraphQuery => {
                self.knowledge_tool_status()
            }
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
                "required_by": ["knowledge_query", "knowledge_graph_query", "search_semantic", "code_symbols"],
            }),
            serde_json::json!({
                "name": "workspace_code_index",
                "status": if self.knowledge_store_available { "ready" } else { "unavailable" },
                "required_by": ["knowledge_graph_query", "search_semantic", "code_symbols"],
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

fn external_tool_catalog_value(
    mut value: serde_json::Value,
    name: &str,
    read_only: bool,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
    executor_available: bool,
    include_schema: bool,
) -> serde_json::Value {
    let access_decision = crate::external_tool_access_decision(read_only, context.access_profile);
    let rejection = resources
        .execution_policy
        .as_ref()
        .and_then(|policy| policy.tool_name_rejection(name));
    let reason = rejection
        .map(|rejection| rejection.reason_code())
        .or_else(|| {
            (access_decision == Some(magi_core::ExecutionResultStatus::Rejected))
                .then_some("read_only_access_profile")
        })
        .or_else(|| (!executor_available).then_some("runtime_unavailable"));
    let availability = match reason {
        Some("runtime_unavailable") => "unavailable",
        Some(_) => "denied",
        None if access_decision == Some(magi_core::ExecutionResultStatus::NeedsApproval) => {
            "needs_approval"
        }
        None => "available",
    };
    value["model_available"] = serde_json::json!(reason.is_none());
    value["availability"] = serde_json::json!(availability);
    value["reason_code"] = serde_json::json!(reason);
    // schema 按精确工具名取得；目录元数据不携带全部外接 schema。
    if !include_schema {
        value
            .as_object_mut()
            .expect("serialized tool entry")
            .remove("input_schema");
    }
    value["schema_included"] = serde_json::json!(include_schema);
    value
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
mod tests;
