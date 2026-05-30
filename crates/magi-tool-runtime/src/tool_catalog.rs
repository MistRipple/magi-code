use crate::{
    BuiltinToolAccessMode, BuiltinToolName, ExternalToolCatalogSnapshot, ToolExecutionContext,
    ToolRuntimeResources,
};

pub(crate) fn execute_tool_catalog(
    input: &str,
    _context: &ToolExecutionContext,
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
            "工具目录: builtin_public={} builtin_internal={} skill_tools={} mcp_servers={} connected_mcp_servers={} schema_warnings={}",
            public_count,
            internal_count,
            skill_tool_count,
            mcp_server_count,
            connected_mcp_server_count,
            schema_warning_count
        ),
        "total": tools.len() + skill_tool_count,
        "builtin_total": BuiltinToolName::ALL.len(),
        "public_count": public_count,
        "internal_count": internal_count,
        "schema_warning_count": schema_warning_count,
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

fn access_mode_for_tool(tool: BuiltinToolName) -> BuiltinToolAccessMode {
    if matches!(
        tool,
        BuiltinToolName::ShellExec | BuiltinToolName::ProcessLaunch
    ) {
        BuiltinToolAccessMode::MaybeWrite
    } else if tool.is_write_operation() {
        BuiltinToolAccessMode::ExplicitWrite
    } else {
        BuiltinToolAccessMode::ReadOnly
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
        assert_eq!(payload["schema_warning_count"], 0);
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
}
