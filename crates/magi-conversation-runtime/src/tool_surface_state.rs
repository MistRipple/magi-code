use crate::{SKILL_APPLY_TOOL_NAME, builtin_tool_schema::public_builtin_tool_definition};
use magi_bridge_client::{ChatToolDefinition, ChatToolFunctionDefinition, ChatToolOrigin};
use magi_browser_authority::BrowserCapabilitySnapshot;
use magi_core::{AccessProfile, ExecutionResultStatus};
use magi_skill_runtime::SkillRuntime;
use magi_tool_runtime::{BuiltinToolName, ToolRegistry};

/// 初始与按需刷新后的工具面共用 Task 的显式限制；Skill/宿主不能重新加入已禁止的工具。
pub(crate) fn constrain_task_tool_definitions(
    mut definitions: Vec<ChatToolDefinition>,
    task_policy: Option<&magi_core::TaskPolicy>,
) -> Vec<ChatToolDefinition> {
    if let Some(policy) = task_policy {
        if policy.command_mode.eq_ignore_ascii_case("no_tools") {
            return Vec::new();
        }
        let mut scope = magi_tool_runtime::ToolExecutionPolicy::default();
        scope.apply_task_policy(policy);
        definitions.retain(|definition| {
            scope
                .tool_name_rejection(&definition.function.name)
                .is_none()
        });
    }
    crate::task_helpers::enforce_agent_coordination_surface(definitions)
}

fn active_skill_allowed_tools(
    runtime: Option<&SkillRuntime>,
    active_id: Option<&str>,
) -> Option<Vec<String>> {
    let policy =
        crate::active_skill_tool_execution_policy(AccessProfile::ReadOnly, runtime, active_id);
    policy.allowed_tool_names
}

pub(crate) struct BrowserToolSurfaceSnapshot {
    pub definitions: Vec<ChatToolDefinition>,
    pub capability: Option<BrowserCapabilitySnapshot>,
}

pub(crate) struct BrowserToolSurfaceContext<'a> {
    skill_runtime: Option<&'a SkillRuntime>,
    active_skill_id: Option<&'a str>,
    allowed_tools: Option<&'a [String]>,
    denied_tools: &'a [String],
}

impl<'a> BrowserToolSurfaceContext<'a> {
    pub(crate) fn new(
        skill_runtime: Option<&'a SkillRuntime>,
        active_skill_id: Option<&'a str>,
        allowed_tools: Option<&'a [String]>,
        denied_tools: &'a [String],
    ) -> Self {
        Self {
            skill_runtime,
            active_skill_id,
            allowed_tools,
            denied_tools,
        }
    }
}

pub(crate) fn build_browser_tool_surface(
    mut definitions: Vec<ChatToolDefinition>,
    tool_registry: &ToolRegistry,
    context: BrowserToolSurfaceContext<'_>,
    capability: Option<BrowserCapabilitySnapshot>,
) -> BrowserToolSurfaceSnapshot {
    definitions.retain(|definition| {
        BuiltinToolName::from_name(&definition.function.name)
            .is_none_or(|tool| tool.browser_tool_kind().is_none())
    });

    let Some(capability) = capability else {
        return BrowserToolSurfaceSnapshot {
            definitions,
            capability: None,
        };
    };
    let skill_allowed_tools =
        active_skill_allowed_tools(context.skill_runtime, context.active_skill_id);
    let registered_public_tools = tool_registry
        .public_builtin_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::BTreeSet<_>>();

    for tool in BuiltinToolName::ALL {
        let Some(kind) = tool.browser_tool_kind() else {
            continue;
        };
        let name = tool.as_str();
        if !capability.allows_catalog_tool(kind)
            || !registered_public_tools.contains(name)
            || context.denied_tools.iter().any(|denied| denied == name)
            || context
                .allowed_tools
                .is_some_and(|allowed| !allowed.iter().any(|item| item == name))
            || skill_allowed_tools
                .as_ref()
                .is_some_and(|allowed| !allowed.iter().any(|item| item == name))
            || definitions
                .iter()
                .any(|definition| definition.function.name == name)
        {
            continue;
        }
        if let Some(definition) = public_builtin_tool_definition(name) {
            definitions.push(definition);
        }
    }

    BrowserToolSurfaceSnapshot {
        definitions,
        capability: Some(capability),
    }
}

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

/// 原生循环和工作流都先提交 Skill 限制，再发布工具成功或继续下一步。
pub(crate) fn persist_task_skill_activation(
    registry: &crate::task_execution_registry::TaskExecutionRegistry,
    sessions: &magi_session_store::SessionStore,
    task_id: &magi_core::TaskId,
    session_id: &magi_core::SessionId,
    turn_id: Option<&str>,
    skill_id: String,
    persist: Option<&crate::session_writeback::SessionStatePersistCallback>,
) -> Result<(), String> {
    let turn_id = turn_id.ok_or_else(|| "Skill 激活缺少执行 Turn".to_string())?;
    registry.update_active_skill(task_id, sessions, session_id, turn_id, skill_id)?;
    // 主线限制位于 sidecar，不能放进普通工具进度的异步合并窗口。
    crate::session_writeback::persist_session_state_checkpoint(persist, "task_skill_activated")
}

pub(crate) struct RefreshLiveMcpToolDefinitionsInput<'a> {
    pub definitions: Vec<ChatToolDefinition>,
    pub tool_registry: &'a ToolRegistry,
    pub skill_runtime: Option<&'a SkillRuntime>,
    pub active_skill_id: Option<&'a str>,
    pub access_profile: AccessProfile,
    pub allowed_tools: Option<&'a [String]>,
    pub denied_tools: &'a [String],
    pub include_external: bool,
    pub workspace_id: Option<&'a magi_core::WorkspaceId>,
}

pub(crate) fn refresh_live_mcp_tool_definitions_with_mode(
    input: RefreshLiveMcpToolDefinitionsInput<'_>,
) -> Vec<ChatToolDefinition> {
    let RefreshLiveMcpToolDefinitionsInput {
        mut definitions,
        tool_registry,
        skill_runtime,
        active_skill_id,
        access_profile,
        allowed_tools,
        denied_tools,
        include_external,
        workspace_id,
    } = input;
    definitions.retain(|definition| {
        !definition.function.name.starts_with("mcp__")
            && !definition.function.name.starts_with("plugin__")
    });
    let skill_allowed_tools = active_skill_allowed_tools(skill_runtime, active_skill_id);
    if include_external {
        for tool in tool_registry.external_tool_catalog_snapshot().mcp_tools {
            if magi_tool_runtime::external_tool_access_decision(tool.read_only, access_profile)
                == Some(ExecutionResultStatus::Rejected)
                || !tool_registry.external_mcp_tool_executor_available()
                || denied_tools
                    .iter()
                    .any(|denied| denied == &tool.model_tool_name)
                || allowed_tools.is_some_and(|allowed| {
                    !allowed.iter().any(|name| name == &tool.model_tool_name)
                })
                || skill_allowed_tools.as_ref().is_some_and(|allowed| {
                    !allowed.iter().any(|name| name == &tool.model_tool_name)
                })
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
    }
    for tool in tool_registry.plugin_tool_catalog(workspace_id) {
        if magi_tool_runtime::external_tool_access_decision(tool.read_only, access_profile)
            == Some(ExecutionResultStatus::Rejected)
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
        let parameters = if tool.input_schema.is_object() {
            tool.input_schema
        } else {
            serde_json::json!({ "type": "object", "properties": {} })
        };
        definitions.push(ChatToolDefinition {
            kind: "function".to_string(),
            function: ChatToolFunctionDefinition {
                name: tool.model_tool_name,
                description: if tool.description.trim().is_empty() {
                    tool.title
                } else {
                    tool.description
                },
                parameters,
            },
            origin: ChatToolOrigin::Plugin,
        });
    }
    definitions
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_browser_authority::{BrowserCapabilitySnapshot, BrowserHostStatus};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    #[test]
    fn browser_tool_surface_uses_one_round_scoped_capability_snapshot() {
        let revision = Arc::new(AtomicU64::new(7));
        let enabled = Arc::new(AtomicBool::new(true));
        let provider_revision = Arc::clone(&revision);
        let provider_enabled = Arc::clone(&enabled);
        let mut registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        )
        .with_browser_automation(
            Arc::new(|_, tool, _, _| {
                (
                    serde_json::json!({ "tool": tool, "status": "succeeded" }).to_string(),
                    ExecutionResultStatus::Succeeded,
                )
            }),
            Arc::new(move || BrowserCapabilitySnapshot {
                revision: provider_revision.load(Ordering::Acquire),
                in_app_browser_enabled: true,
                browser_use_enabled: provider_enabled.load(Ordering::Acquire),
                devtools_enabled: true,
                lan_access_enabled: false,
                host_status: BrowserHostStatus::Ready,
                host_protocol_compatible: true,
            }),
        );
        registry.register_default_builtins();

        let first = build_browser_tool_surface(
            Vec::new(),
            &registry,
            BrowserToolSurfaceContext::new(None, None, None, &[]),
            registry.browser_capability_snapshot(),
        );
        assert_eq!(
            first.capability.as_ref().map(|snapshot| snapshot.revision),
            Some(7)
        );
        let browser_tool_count = BuiltinToolName::ALL
            .iter()
            .filter(|tool| {
                tool.browser_tool_kind()
                    .is_some_and(magi_browser_authority::BrowserToolKind::is_supported)
            })
            .count();
        assert_eq!(
            first
                .definitions
                .iter()
                .filter(|definition| {
                    BuiltinToolName::from_name(&definition.function.name).is_some_and(|tool| {
                        tool.browser_tool_kind()
                            .is_some_and(magi_browser_authority::BrowserToolKind::is_supported)
                    })
                })
                .count(),
            browser_tool_count
        );

        revision.store(8, Ordering::Release);
        enabled.store(false, Ordering::Release);
        let disabled = build_browser_tool_surface(
            first.definitions,
            &registry,
            BrowserToolSurfaceContext::new(None, None, None, &[]),
            registry.browser_capability_snapshot(),
        );
        assert_eq!(
            disabled
                .capability
                .as_ref()
                .map(|snapshot| snapshot.revision),
            Some(8)
        );
        assert!(disabled.definitions.is_empty());

        revision.store(9, Ordering::Release);
        enabled.store(true, Ordering::Release);
        let reenabled = build_browser_tool_surface(
            disabled.definitions,
            &registry,
            BrowserToolSurfaceContext::new(None, None, None, &[]),
            registry.browser_capability_snapshot(),
        );
        let names = reenabled
            .definitions
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();
        let expected_names = BuiltinToolName::ALL
            .iter()
            .filter(|tool| {
                tool.browser_tool_kind()
                    .is_some_and(magi_browser_authority::BrowserToolKind::is_supported)
            })
            .map(BuiltinToolName::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            reenabled
                .capability
                .as_ref()
                .map(|snapshot| snapshot.revision),
            Some(9)
        );
        assert_eq!(names, expected_names);
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
        }))
        .with_external_mcp_tool_executor(std::sync::Arc::new(|_, _, _| {
            ("{}".into(), ExecutionResultStatus::Succeeded)
        }));

        let definitions =
            refresh_live_mcp_tool_definitions_with_mode(RefreshLiveMcpToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::ReadOnly,
                allowed_tools: None,
                denied_tools: &[],
                include_external: true,
                workspace_id: None,
            });
        let names = definitions
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["mcp__repo__inspect"]);
    }

    #[test]
    fn deferred_mcp_surface_omits_external_schema_until_requested() {
        let registry = ToolRegistry::new(
            std::sync::Arc::new(magi_governance::GovernanceService::default()),
            std::sync::Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        )
        .with_external_tool_catalog_provider(std::sync::Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                    server_id: "repo".to_string(),
                    server_name: "Repository".to_string(),
                    model_tool_name: "mcp__repo__inspect".to_string(),
                    tool_name: "inspect".to_string(),
                    description: "Inspect repository".to_string(),
                    read_only: true,
                    input_schema: serde_json::json!({ "type": "object" }),
                }],
                ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
            }
        }));

        let unavailable =
            refresh_live_mcp_tool_definitions_with_mode(RefreshLiveMcpToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::FullAccess,
                allowed_tools: None,
                denied_tools: &[],
                include_external: true,
                workspace_id: None,
            });
        assert!(
            unavailable.is_empty(),
            "目录没有执行器时不能发布模型 schema"
        );
        let catalog = registry.tool_catalog_value("{}", &Default::default());
        assert_eq!(
            catalog["mcp_tools"][0]["reason_code"],
            "runtime_unavailable"
        );
        let registry = registry.with_external_mcp_tool_executor(std::sync::Arc::new(|_, _, _| {
            ("{}".into(), ExecutionResultStatus::Succeeded)
        }));

        let first_round =
            refresh_live_mcp_tool_definitions_with_mode(RefreshLiveMcpToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::Restricted,
                allowed_tools: None,
                denied_tools: &[],
                include_external: false,
                workspace_id: None,
            });
        assert!(first_round.is_empty(), "首轮不应注入完整 MCP schema");

        let after_catalog =
            refresh_live_mcp_tool_definitions_with_mode(RefreshLiveMcpToolDefinitionsInput {
                definitions: first_round,
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::Restricted,
                allowed_tools: None,
                denied_tools: &[],
                include_external: true,
                workspace_id: None,
            });
        assert_eq!(
            after_catalog
                .iter()
                .map(|definition| definition.function.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__repo__inspect"]
        );
    }
}
