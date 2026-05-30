use crate::{
    BuiltinToolAccessMode, BuiltinToolName, ExternalToolCatalogSnapshot, ToolExecutionContext,
    ToolRuntimeResources,
};

pub(crate) fn execute_tool_catalog(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
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

    let mut tools = Vec::new();
    let mut public_count = 0usize;
    let mut internal_count = 0usize;
    let mut schema_warning_count = 0usize;
    let runtime_health = RuntimeHealth::from_context(context, resources);
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
            .filter(|server| server.connected)
            .count()
    } else {
        0
    };
    let external_catalog_status =
        if include_external && resources.external_tool_catalog_provider.is_some() {
            "available"
        } else if include_external {
            "unavailable"
        } else {
            "disabled"
        };

    serde_json::json!({
        "tool": "tool_catalog",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "summary": format!(
            "工具目录: builtin_public={} builtin_internal={} skill_tools={} mcp_servers={} connected_mcp_servers={} schema_warnings={} runtime_warnings={}",
            public_count,
            internal_count,
            skill_tool_count,
            mcp_server_count,
            connected_mcp_server_count,
            schema_warning_count,
            runtime_warning_count
        ),
        "total": tools.len() + skill_tool_count,
        "builtin_total": BuiltinToolName::ALL.len(),
        "public_count": public_count,
        "internal_count": internal_count,
        "schema_warning_count": schema_warning_count,
        "runtime_warning_count": runtime_warning_count,
        "runtime_dependencies": runtime_health.dependencies_json(),
        "external_catalog_status": external_catalog_status,
        "skill_tool_count": skill_tool_count,
        "mcp_server_count": mcp_server_count,
        "connected_mcp_server_count": connected_mcp_server_count,
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
    })
    .to_string()
}

struct RuntimeHealth {
    knowledge_store_available: bool,
    workspace_id: Option<String>,
    workspace_code_index_ready: bool,
    workspace_code_index_file_count: Option<usize>,
    workspace_code_index_last_indexed: Option<u64>,
}

struct RuntimeToolStatus {
    status: &'static str,
    warnings: Vec<String>,
}

impl RuntimeHealth {
    fn from_context(context: &ToolExecutionContext, resources: &ToolRuntimeResources) -> Self {
        let knowledge_store_available = resources.knowledge_store.is_some();
        let workspace_id = context
            .workspace_id
            .as_ref()
            .map(|id| id.as_str().to_string());
        let (
            workspace_code_index_ready,
            workspace_code_index_file_count,
            workspace_code_index_last_indexed,
        ) = match (
            resources.knowledge_store.as_ref(),
            context.workspace_id.as_ref(),
        ) {
            (Some(store), Some(workspace_id)) => {
                let summary = store.code_index_summary_for_workspace(workspace_id);
                (
                    store.workspace_index_ready(workspace_id),
                    summary.as_ref().map(|summary| summary.files.len()),
                    summary.as_ref().map(|summary| summary.last_indexed),
                )
            }
            _ => (false, None, None),
        };

        Self {
            knowledge_store_available,
            workspace_id,
            workspace_code_index_ready,
            workspace_code_index_file_count,
            workspace_code_index_last_indexed,
        }
    }

    fn tool_status(&self, tool: BuiltinToolName) -> RuntimeToolStatus {
        match tool {
            BuiltinToolName::KnowledgeQuery => self.knowledge_tool_status("knowledge_query"),
            BuiltinToolName::SearchSemantic => self.code_index_tool_status("search_semantic"),
            BuiltinToolName::CodeSymbols => self.code_index_tool_status("code_symbols"),
            _ => RuntimeToolStatus {
                status: "ready",
                warnings: Vec::new(),
            },
        }
    }

    fn knowledge_tool_status(&self, tool_name: &str) -> RuntimeToolStatus {
        if !self.knowledge_store_available {
            return RuntimeToolStatus {
                status: "unavailable",
                warnings: vec![format!(
                    "{tool_name} 需要 KnowledgeStore，但当前运行时未注入"
                )],
            };
        }
        if self.workspace_id.is_none() {
            return RuntimeToolStatus {
                status: "missing_context",
                warnings: vec![format!("{tool_name} 需要 workspace 上下文")],
            };
        }
        RuntimeToolStatus {
            status: "ready",
            warnings: Vec::new(),
        }
    }

    fn code_index_tool_status(&self, tool_name: &str) -> RuntimeToolStatus {
        let base = self.knowledge_tool_status(tool_name);
        if base.status != "ready" {
            return base;
        }
        if !self.workspace_code_index_ready {
            return RuntimeToolStatus {
                status: "not_ready",
                warnings: vec![format!("{tool_name} 需要当前 workspace 的本地代码索引就绪")],
            };
        }
        RuntimeToolStatus {
            status: "ready",
            warnings: Vec::new(),
        }
    }

    fn dependencies_json(&self) -> serde_json::Value {
        serde_json::json!([
            {
                "name": "knowledge_store",
                "status": if self.knowledge_store_available { "available" } else { "unavailable" },
                "required_by": ["knowledge_query", "search_semantic", "code_symbols"],
            },
            {
                "name": "workspace_code_index",
                "status": self.workspace_code_index_status(),
                "workspace_id": self.workspace_id,
                "file_count": self.workspace_code_index_file_count,
                "last_indexed": self.workspace_code_index_last_indexed,
                "required_by": ["search_semantic", "code_symbols"],
            }
        ])
    }

    fn workspace_code_index_status(&self) -> &'static str {
        if !self.knowledge_store_available {
            "unavailable"
        } else if self.workspace_id.is_none() {
            "missing_context"
        } else if self.workspace_code_index_ready {
            "ready"
        } else {
            "not_ready"
        }
    }
}

fn access_mode_for_tool(tool: BuiltinToolName) -> BuiltinToolAccessMode {
    tool.default_access_mode()
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
        let search_semantic = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == "search_semantic")
            .expect("search_semantic should be listed");
        assert_eq!(search_semantic["runtime_status"], "unavailable");
        assert_eq!(
            payload["runtime_dependencies"][0]["name"],
            "knowledge_store"
        );
        assert_eq!(payload["runtime_dependencies"][0]["status"], "unavailable");
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
}
