use crate::{
    SKILL_APPLY_TOOL_NAME, build_skill_custom_tool_definitions, parse_skill_custom_tool_name,
};
use magi_bridge_client::{ChatToolDefinition, ChatToolFunctionDefinition, ChatToolOrigin};
use magi_core::{AccessProfile, ExecutionResultStatus};
use magi_skill_runtime::{SkillRuntime, SkillSelection};
use magi_tool_runtime::{BuiltinToolName, ToolRegistry};

pub(crate) fn activated_skill_id_from_tool_result(
    tool_name: &str,
    payload: &str,
    status: ExecutionResultStatus,
) -> Option<String> {
    if tool_name != SKILL_APPLY_TOOL_NAME || status != ExecutionResultStatus::Succeeded {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("skill_name")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn activate_skill_tool_definitions(
    mut definitions: Vec<ChatToolDefinition>,
    skill_runtime: &SkillRuntime,
    skill_id: &str,
    access_profile: AccessProfile,
    preserved_builtin_tools: &[&str],
) -> Vec<ChatToolDefinition> {
    let plan = skill_runtime.build_tool_runtime_plan(SkillSelection {
        skill_ids: vec![skill_id.to_string()],
        requested_tools: Vec::new(),
    });
    let standard_tool_allowlist = (!plan.tool_policy.source_skill_ids.is_empty())
        .then_some(plan.tool_policy.allowed_tool_names.as_slice());
    definitions.retain(|definition| {
        let name = definition.function.name.as_str();
        if name == SKILL_APPLY_TOOL_NAME || parse_skill_custom_tool_name(name).is_some() {
            return false;
        }
        if BuiltinToolName::from_name(name).is_some() {
            return preserved_builtin_tools.contains(&name)
                || standard_tool_allowlist.is_none_or(|allowed_tools| {
                    allowed_tools.iter().any(|allowed| allowed == name)
                });
        }
        if name.starts_with("mcp__") {
            return standard_tool_allowlist
                .is_none_or(|allowed_tools| allowed_tools.iter().any(|allowed| allowed == name));
        }
        true
    });
    definitions.extend(build_skill_custom_tool_definitions(
        skill_id,
        &plan,
        access_profile,
    ));
    definitions
}

pub(crate) fn refresh_live_mcp_tool_definitions(
    mut definitions: Vec<ChatToolDefinition>,
    tool_registry: &ToolRegistry,
    skill_runtime: Option<&SkillRuntime>,
    active_skill_id: Option<&str>,
    access_profile: AccessProfile,
    allowed_tools: Option<&[String]>,
    denied_tools: &[String],
) -> Vec<ChatToolDefinition> {
    definitions.retain(|definition| !definition.function.name.starts_with("mcp__"));
    let skill_allowed_tools = active_skill_id.and_then(|skill_id| {
        skill_runtime.and_then(|runtime| {
            let policy = runtime
                .build_tool_runtime_plan(SkillSelection {
                    skill_ids: vec![skill_id.to_string()],
                    requested_tools: Vec::new(),
                })
                .tool_policy;
            (!policy.source_skill_ids.is_empty()).then_some(policy.allowed_tool_names)
        })
    });
    for tool in tool_registry.external_tool_catalog_snapshot().mcp_tools {
        if (access_profile == AccessProfile::ReadOnly && !tool.read_only)
            || denied_tools
                .iter()
                .any(|denied| denied == &tool.model_tool_name)
            || allowed_tools
                .is_some_and(|allowed| !allowed.iter().any(|name| name == &tool.model_tool_name))
            || skill_allowed_tools
                .as_ref()
                .is_some_and(|allowed| !allowed.iter().any(|name| name == &tool.model_tool_name))
            || definitions
                .iter()
                .any(|definition| definition.function.name == tool.model_tool_name)
        {
            continue;
        }
        let mut parameters = tool.input_schema;
        if let Some(object) = parameters.as_object_mut()
            && object
                .get("properties")
                .is_none_or(serde_json::Value::is_null)
        {
            object.insert(
                "properties".to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        if !parameters.is_object() {
            parameters = serde_json::json!({ "type": "object", "properties": {} });
        }
        definitions.push(ChatToolDefinition {
            kind: "function".to_string(),
            function: ChatToolFunctionDefinition {
                name: tool.model_tool_name,
                description: if tool.description.trim().is_empty() {
                    format!("MCP tool {} from {}", tool.tool_name, tool.server_name)
                } else {
                    tool.description
                },
                parameters,
            },
            origin: ChatToolOrigin::ExternalMcp,
        });
    }
    definitions
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_skill_runtime::{SkillDefinition, SkillMetadata, SkillRegistry};

    #[test]
    fn successful_skill_activation_replaces_apply_surface_with_skill_tools() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查关键缺陷。".to_string(),
            metadata: SkillMetadata {
                category: "quality".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: true,
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        let runtime = SkillRuntime::new(registry);
        let definitions = vec![
            ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: SKILL_APPLY_TOOL_NAME.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: ChatToolOrigin::Builtin,
            },
            ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: "file_read".to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: ChatToolOrigin::Builtin,
            },
            ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: "file_write".to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: ChatToolOrigin::Builtin,
            },
        ];

        let activated = activate_skill_tool_definitions(
            definitions,
            &runtime,
            "code-review",
            AccessProfile::Restricted,
            &[],
        );
        let names = activated
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["file_read"]);
    }

    #[test]
    fn prompt_only_skill_activation_keeps_standard_tool_surface() {
        let registry = SkillRegistry::new();
        registry.register(SkillDefinition {
            skill_id: "prompt-only".to_string(),
            title: "Prompt Only".to_string(),
            instruction: "先取证，再回答。".to_string(),
            metadata: SkillMetadata {
                category: "workflow".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: false,
            allowed_tools: vec![],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        let runtime = SkillRuntime::new(registry);
        let definitions = vec![
            ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: SKILL_APPLY_TOOL_NAME.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: ChatToolOrigin::Builtin,
            },
            ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: "file_read".to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: ChatToolOrigin::Builtin,
            },
            ChatToolDefinition {
                kind: "function".to_string(),
                function: ChatToolFunctionDefinition {
                    name: "mcp__repo__inspect".to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: ChatToolOrigin::ExternalMcp,
            },
        ];

        let activated = activate_skill_tool_definitions(
            definitions,
            &runtime,
            "prompt-only",
            AccessProfile::Restricted,
            &[],
        );
        let names = activated
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["file_read", "mcp__repo__inspect"]);
    }

    #[test]
    fn read_only_surface_exposes_only_mcp_tools_with_read_only_annotation() {
        let registry = ToolRegistry::new(
            std::sync::Arc::new(magi_governance::GovernanceService::default()),
            std::sync::Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        )
        .with_external_tool_catalog_provider(std::sync::Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                mcp_tools: vec![
                    magi_tool_runtime::ExternalMcpToolCatalogEntry {
                        server_id: "repo".to_string(),
                        server_name: "Repository".to_string(),
                        model_tool_name: "mcp__repo__inspect".to_string(),
                        tool_name: "inspect".to_string(),
                        description: "Inspect repository".to_string(),
                        read_only: true,
                        input_schema: serde_json::json!({ "type": "object" }),
                    },
                    magi_tool_runtime::ExternalMcpToolCatalogEntry {
                        server_id: "repo".to_string(),
                        server_name: "Repository".to_string(),
                        model_tool_name: "mcp__repo__write".to_string(),
                        tool_name: "write".to_string(),
                        description: "Write repository".to_string(),
                        read_only: false,
                        input_schema: serde_json::json!({ "type": "object" }),
                    },
                ],
                ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
            }
        }));

        let definitions = refresh_live_mcp_tool_definitions(
            Vec::new(),
            &registry,
            None,
            None,
            AccessProfile::ReadOnly,
            None,
            &[],
        );
        let names = definitions
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["mcp__repo__inspect"]);
    }
}
