use magi_bridge_client::{ChatToolDefinition, ChatToolFunctionDefinition};
use magi_tool_runtime::{BuiltinToolName, ToolRegistry, is_internal_builtin_tool_surface};

fn public_builtin_tool_definition(name: &str) -> Option<ChatToolDefinition> {
    let tool_name = BuiltinToolName::from_str(name)?;
    if !tool_name.is_public_tool_surface() {
        return None;
    }

    Some(ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: name.to_string(),
            description: tool_name.description().to_string(),
            parameters: tool_name.parameters_schema(),
        },
    })
}

pub fn public_builtin_tool_definitions(registry: &ToolRegistry) -> Vec<ChatToolDefinition> {
    registry
        .public_builtin_specs()
        .into_iter()
        .filter_map(|spec| public_builtin_tool_definition(&spec.name))
        .collect()
}

pub fn internal_builtin_tool_rejection_payload(name: &str) -> Option<String> {
    if !is_internal_builtin_tool_surface(name) {
        return None;
    }
    Some(
        serde_json::json!({
            "tool": name,
            "status": "failed",
            "error": format!(
                "{name} 是 shell 工具的内部执行能力，不接受模型直接调用；请使用 shell_exec，并在需要后台运行时设置 background=true"
            )
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_array_schemas_define_items(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("type").and_then(serde_json::Value::as_str) == Some("array") {
                    assert!(
                        object.get("items").is_some(),
                        "array schema at {path} must define items"
                    );
                }
                for (key, child) in object {
                    assert_array_schemas_define_items(child, &format!("{path}.{key}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    assert_array_schemas_define_items(child, &format!("{path}[{index}]"));
                }
            }
            _ => {}
        }
    }

    #[test]
    fn builtin_tool_definition_uses_runtime_tool_metadata() {
        let definition = public_builtin_tool_definition("file_read").expect("public file_read");

        assert_eq!(definition.kind, "function");
        assert_eq!(definition.function.name, "file_read");
        assert_eq!(
            definition.function.description,
            BuiltinToolName::FileRead.description()
        );
        assert_eq!(
            definition.function.parameters["required"],
            serde_json::json!(["path"])
        );
    }

    #[test]
    fn public_builtin_tool_definition_rejects_internal_process_tools() {
        for name in [
            "process_launch",
            "process_read",
            "process_write",
            "process_kill",
            "process_list",
        ] {
            assert!(
                public_builtin_tool_definition(name).is_none(),
                "{name} must not produce model-facing tool definitions"
            );
        }
        assert!(public_builtin_tool_definition("mermaid_diagram").is_none());
        for renderer_name in [
            "mermaid",
            "graphviz",
            "dot",
            "cytoscape",
            "svelte_flow",
            "svelte-flow",
        ] {
            assert!(
                public_builtin_tool_definition(renderer_name).is_none(),
                "{renderer_name} is a diagram renderer or diagram kind, not a model-facing tool"
            );
        }

        let diagram_definition = public_builtin_tool_definition("diagram_render")
            .expect("diagram_render should be public");
        assert_eq!(diagram_definition.function.name, "diagram_render");
        assert_eq!(
            diagram_definition.function.parameters["required"],
            serde_json::json!(["kind"])
        );
    }

    #[test]
    fn managed_process_tools_are_runtime_internal_shell_surface() {
        assert!(BuiltinToolName::ShellExec.is_public_tool_surface());
        assert!(!BuiltinToolName::ProcessLaunch.is_public_tool_surface());
        assert!(!BuiltinToolName::ProcessRead.is_public_tool_surface());
        assert!(!BuiltinToolName::ProcessWrite.is_public_tool_surface());
        assert!(!BuiltinToolName::ProcessKill.is_public_tool_surface());
        assert!(!BuiltinToolName::ProcessList.is_public_tool_surface());
        assert!(BuiltinToolName::ProcessInspect.is_public_tool_surface());
    }

    #[test]
    fn shell_exec_definition_exposes_access_mode_contract() {
        let definition = public_builtin_tool_definition("shell_exec").expect("public shell_exec");
        let access_mode = &definition.function.parameters["properties"]["access_mode"];

        assert_eq!(access_mode["type"], "string");
        assert_eq!(
            access_mode["enum"],
            serde_json::json!(["read_only", "maybe_write", "explicit_write"])
        );
        assert!(
            access_mode["description"]
                .as_str()
                .expect("description")
                .contains("read_only")
        );
    }

    #[test]
    fn apply_patch_definition_exposes_patch_envelope_contract() {
        let definition = public_builtin_tool_definition("apply_patch").expect("public apply_patch");

        assert_eq!(definition.kind, "function");
        assert_eq!(definition.function.name, "apply_patch");
        assert_eq!(
            definition.function.parameters["required"],
            serde_json::json!(["patch"])
        );
        assert!(
            definition.function.parameters["properties"]["patch"]["description"]
                .as_str()
                .expect("patch description")
                .contains("*** Begin Patch")
        );
    }

    #[test]
    fn view_image_definition_exposes_local_image_contract() {
        let definition = public_builtin_tool_definition("view_image").expect("public view_image");

        assert_eq!(definition.kind, "function");
        assert_eq!(definition.function.name, "view_image");
        assert_eq!(
            definition.function.parameters["required"],
            serde_json::json!(["path"])
        );
        assert!(
            definition.function.description.contains("多模态工具结果"),
            "view_image description should make multimodal behavior explicit"
        );
    }

    #[test]
    fn tool_catalog_definition_exposes_diagnostics_contract() {
        let definition =
            public_builtin_tool_definition("tool_catalog").expect("public tool_catalog");

        assert_eq!(definition.kind, "function");
        assert_eq!(definition.function.name, "tool_catalog");
        assert_eq!(
            definition.function.parameters["required"],
            serde_json::json!([])
        );
        assert!(
            definition.function.description.contains("健康状态"),
            "tool_catalog description should make diagnostics behavior explicit"
        );
    }

    #[test]
    fn internal_builtin_rejection_only_targets_known_internal_tools() {
        assert!(internal_builtin_tool_rejection_payload("process_launch").is_some());
        assert!(internal_builtin_tool_rejection_payload("shell_exec").is_none());
        assert!(internal_builtin_tool_rejection_payload("graphviz").is_none());
        assert!(internal_builtin_tool_rejection_payload("mystery_tool").is_none());
    }

    #[test]
    fn builtin_tool_definition_covers_all_registered_builtins() {
        for name in BuiltinToolName::ALL {
            let definition = public_builtin_tool_definition(name.as_str());
            if !name.is_public_tool_surface() {
                assert!(
                    definition.is_none(),
                    "{name:?} is an internal builtin and must not be model-facing"
                );
                continue;
            }
            let definition = definition.expect("public builtin definition");

            assert_eq!(definition.function.name, name.as_str());
            assert_eq!(definition.function.description, name.description());
            assert_eq!(definition.function.parameters, name.parameters_schema());
            assert_eq!(definition.function.parameters["type"], "object", "{name:?}");
            assert!(
                definition.function.parameters.get("properties").is_some(),
                "{name:?} should expose a properties object"
            );
            assert_array_schemas_define_items(
                &definition.function.parameters,
                &format!("{}.parameters", definition.function.name),
            );
        }
    }

    #[test]
    fn builtin_tool_definition_keeps_alias_name_with_canonical_schema() {
        let definition = public_builtin_tool_definition("file_view").expect("public alias");

        assert_eq!(definition.function.name, "file_view");
        assert_eq!(
            definition.function.description,
            BuiltinToolName::FileRead.description()
        );
    }

    #[test]
    fn builtin_tool_definition_rejects_unknown_tool() {
        assert!(public_builtin_tool_definition("mystery_tool").is_none());
    }
}
