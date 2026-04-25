use magi_bridge_client::{ChatToolDefinition, ChatToolFunctionDefinition};
use magi_tool_runtime::BuiltinToolName;

pub(crate) fn builtin_tool_definition(name: &str) -> ChatToolDefinition {
    let tool_name = BuiltinToolName::from_str(name);
    let description = tool_name
        .map(|name| name.description().to_string())
        .unwrap_or_else(|| format!("Builtin tool: {name}"));
    let parameters = tool_name
        .map(|name| name.parameters_schema())
        .unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        });

    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: name.to_string(),
            description,
            parameters,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tool_definition_uses_runtime_tool_metadata() {
        let definition = builtin_tool_definition("file_read");

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
    fn builtin_tool_definition_covers_process_tools() {
        let launch = builtin_tool_definition("process_launch");
        assert_eq!(launch.function.name, "process_launch");
        assert_eq!(
            launch.function.parameters["required"],
            serde_json::json!(["command"])
        );
        assert!(launch.function.parameters["properties"]["command"].is_object());

        let read = builtin_tool_definition("process_read");
        assert_eq!(
            read.function.parameters["required"],
            serde_json::json!(["terminal_id"])
        );
        assert!(read.function.parameters["properties"]["max_bytes"].is_object());

        let list = builtin_tool_definition("process_list");
        assert_eq!(list.function.parameters["type"], "object");
        assert_eq!(
            list.function.parameters["properties"],
            serde_json::json!({})
        );
    }

    #[test]
    fn builtin_tool_definition_covers_all_registered_builtins() {
        for name in BuiltinToolName::ALL {
            let definition = builtin_tool_definition(name.as_str());

            assert_eq!(definition.function.name, name.as_str());
            assert_eq!(definition.function.description, name.description());
            assert_eq!(definition.function.parameters, name.parameters_schema());
            assert_eq!(definition.function.parameters["type"], "object", "{name:?}");
            assert!(
                definition.function.parameters.get("properties").is_some(),
                "{name:?} should expose a properties object"
            );
        }
    }

    #[test]
    fn builtin_tool_definition_keeps_alias_name_with_canonical_schema() {
        let definition = builtin_tool_definition("file_view");

        assert_eq!(definition.function.name, "file_view");
        assert_eq!(
            definition.function.description,
            BuiltinToolName::FileRead.description()
        );
    }

    #[test]
    fn builtin_tool_definition_falls_back_for_unknown_tool() {
        let definition = builtin_tool_definition("mystery_tool");

        assert_eq!(definition.function.name, "mystery_tool");
        assert_eq!(
            definition.function.description,
            "Builtin tool: mystery_tool"
        );
        assert_eq!(definition.function.parameters["type"], "object");
    }
}
