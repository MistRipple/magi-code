use crate::{BuiltinToolAccessMode, BuiltinToolName, ToolExecutionContext};

pub(crate) fn execute_tool_catalog(input: &str, _context: &ToolExecutionContext) -> String {
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

    serde_json::json!({
        "tool": "tool_catalog",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "summary": format!(
            "内置工具目录: public={} internal={} schema_warnings={}",
            public_count, internal_count, schema_warning_count
        ),
        "total": BuiltinToolName::ALL.len(),
        "public_count": public_count,
        "internal_count": internal_count,
        "schema_warning_count": schema_warning_count,
        "tools": tools,
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
        let output = execute_tool_catalog("{}", &ToolExecutionContext::default());
        let payload: serde_json::Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["schema_warning_count"], 0);
        assert_eq!(
            payload["total"].as_u64().expect("total"),
            BuiltinToolName::ALL.len() as u64
        );
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
}
