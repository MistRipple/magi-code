use magi_agent_role::AgentRoleRegistry;
use magi_bridge_client::{ChatToolDefinition, ChatToolFunctionDefinition, ChatToolOrigin};
use magi_tool_runtime::{BuiltinToolName, ToolRegistry, is_internal_builtin_tool_surface};

pub(crate) fn public_builtin_tool_definition(name: &str) -> Option<ChatToolDefinition> {
    let tool_name = BuiltinToolName::from_name(name)?;
    if !tool_name.is_public_tool_surface() {
        return None;
    }

    Some(ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: name.to_string(),
            // 模型每一轮都会收到工具定义；使用首段短描述，避免把面向文档的
            // 长篇使用说明重复塞进 prompt。完整说明仍由 tool_catalog 保留。
            description: model_tool_description(tool_name),
            parameters: tool_name.parameters_schema(),
        },
        origin: ChatToolOrigin::Builtin,
    })
}

fn model_tool_description(tool: BuiltinToolName) -> String {
    if tool == BuiltinToolName::AgentSpawn {
        return "模型调用此工具创建一个子代理任务并立即返回 child_task_id；role 已知时 capabilities 可省略，由服务端按角色默认能力补齐；需要结果时使用 agent_wait。必须传入结构化 context_package。".to_string();
    }
    const MAX_MODEL_TOOL_DESCRIPTION_CHARS: usize = 800;
    let description = tool
        .description()
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .trim();
    let mut compact = description
        .chars()
        .take(MAX_MODEL_TOOL_DESCRIPTION_CHARS)
        .collect::<String>();
    if description.chars().count() > MAX_MODEL_TOOL_DESCRIPTION_CHARS {
        compact.push('…');
    }
    compact
}

fn apply_runtime_schema(
    definition: ChatToolDefinition,
    _agent_role_registry: &AgentRoleRegistry,
) -> ChatToolDefinition {
    // 角色能力是运行时注册表的事实源，不能把全局能力集合写入工具 Schema。
    // agent_spawn 缺省 capabilities 时由服务端按目标角色补齐；显式值仍由
    // preflight 严格校验，避免模型依据一个跨角色的 enum 生成必然会被拒绝的参数。
    definition
}

pub fn public_builtin_tool_definitions(
    registry: &ToolRegistry,
    agent_role_registry: &AgentRoleRegistry,
) -> Vec<ChatToolDefinition> {
    registry
        .public_builtin_specs()
        .into_iter()
        .filter_map(|spec| public_builtin_tool_definition(&spec.name))
        .map(|definition| apply_runtime_schema(definition, agent_role_registry))
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
        assert!(
            definition
                .function
                .description
                .starts_with("读取指定路径文件的内容")
        );
        assert!(definition.function.description.chars().count() < 800);
        assert_eq!(
            definition.function.parameters["required"],
            serde_json::json!(["path"])
        );
    }

    #[test]
    fn model_tool_descriptions_are_compact_and_keep_agent_spawn_contract() {
        let definition = public_builtin_tool_definition("agent_spawn").expect("public agent_spawn");
        assert!(
            definition
                .function
                .description
                .contains("capabilities 可省略")
        );
        assert!(definition.function.description.contains("context_package"));
        assert!(definition.function.description.chars().count() < 800);
    }

    #[test]
    fn agent_spawn_schema_keeps_capabilities_optional_and_role_scoped() {
        let definition = apply_runtime_schema(
            public_builtin_tool_definition("agent_spawn").expect("public agent_spawn"),
            &AgentRoleRegistry::load_default(),
        );

        assert!(
            definition.function.parameters["properties"]["capabilities"]
                .get("minItems")
                .is_some()
        );
        assert!(
            definition.function.parameters["properties"]["capabilities"]["items"]
                .get("enum")
                .is_none()
        );
        assert!(
            !definition.function.parameters["required"]
                .as_array()
                .expect("required")
                .iter()
                .any(|field| field == "capabilities")
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
        assert!(
            access_mode["description"]
                .as_str()
                .expect("description")
                .contains("临时文件")
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
            assert_eq!(definition.origin, ChatToolOrigin::Builtin);
            assert!(
                definition
                    .function
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "{name:?} 必须能直接映射为跨模型安全工具名"
            );
            assert!(
                format!("magi_builtin_{}", definition.function.name).len() <= 64,
                "{name:?} 的协议工具名不得触发不透明哈希"
            );
            assert_eq!(
                definition.function.description,
                model_tool_description(name)
            );
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
    fn builtin_tool_definition_rejects_non_canonical_alias() {
        assert!(public_builtin_tool_definition("file_view").is_none());
    }

    #[test]
    fn builtin_tool_definition_rejects_unknown_tool() {
        assert!(public_builtin_tool_definition("mystery_tool").is_none());
    }
}
