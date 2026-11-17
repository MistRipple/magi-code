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
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    if value["mode"] == "reference" {
        return None;
    }
    value
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

pub(crate) struct RefreshLiveExternalToolDefinitionsInput<'a> {
    pub definitions: Vec<ChatToolDefinition>,
    pub tool_registry: &'a ToolRegistry,
    pub skill_runtime: Option<&'a SkillRuntime>,
    pub active_skill_id: Option<&'a str>,
    pub access_profile: AccessProfile,
    pub allowed_tools: Option<&'a [String]>,
    pub denied_tools: &'a [String],
    pub loaded_tool_names: &'a std::collections::BTreeSet<String>,
}

/// 插件 schema 仍按显式发现加载；MCP 由 live server 完整提供。
pub(crate) fn extend_discovered_tool_names(
    loaded: &mut std::collections::BTreeSet<String>,
    result: &str,
) {
    collect_discovered_tool_names(loaded, result, true);
}

/// `require_schema` 为真时按实时回执要求携带 schema；历史投影已去掉 schema，只能依赖
/// `schema_included` 这一回执当时的事实。
fn collect_discovered_tool_names(
    loaded: &mut std::collections::BTreeSet<String>,
    result: &str,
    require_schema: bool,
) {
    let Ok(result) = serde_json::from_str::<serde_json::Value>(result) else {
        return;
    };
    if result["tool"] != "tool_catalog"
        || result["status"] != "succeeded"
        || result["execution"] == "skipped"
    {
        return;
    }
    for kind in ["mcp_tools", "plugin_tools"] {
        for tool in result[kind].as_array().into_iter().flatten() {
            if tool["schema_included"] == true
                && tool["model_available"] == true
                && (!require_schema || tool["input_schema"].is_object())
                && let Some(name) = tool["model_tool_name"].as_str()
            {
                loaded.insert(name.to_string());
            }
        }
    }
}

/// 从线程历史里已成功的 `tool_catalog` 回执恢复“已发现”的插件工具集合。
///
/// 发现集合决定 tools 定义，而 tools 位于请求前缀最前面；若每个回合都从空集合开始、
/// 等模型再次发现后才追加，每回合至少让整段历史缓存失效一次。历史回执本身就是权威
/// 事实，会随压缩一起消失（压缩本就是缓存断代边界），因此不需要另存一份发现状态。
/// 历史投影会去掉 `input_schema`，只保留 `schema_included`，所以这里不再要求 schema 对象；
/// 工具是否仍然可用由 `refresh_live_external_tool_definitions` 对当前目录与准入再次裁决。
pub(crate) fn discovered_tool_names_from_history(
    history: &[magi_session_store::ThreadChatMessage],
) -> std::collections::BTreeSet<String> {
    let mut catalog_calls = std::collections::HashSet::new();
    let mut loaded = std::collections::BTreeSet::new();
    for message in history {
        if message.role == "assistant" {
            for call in &message.tool_calls {
                if crate::task_helpers::canonical_tool_call_name(&call.function.name)
                    == "tool_catalog"
                {
                    catalog_calls.insert(call.id.as_str());
                }
            }
        } else if message.role == "tool"
            && message
                .tool_call_id
                .as_deref()
                .is_some_and(|id| catalog_calls.contains(id))
            && let Some(content) = message.content.as_deref()
        {
            collect_discovered_tool_names(&mut loaded, content, false);
        }
    }
    loaded
}

pub(crate) struct LiveExternalToolSurface {
    pub definitions: Vec<ChatToolDefinition>,
    pub mcp_instructions: Option<String>,
}

/// Schema 与 initialize instructions 来自同一个当前目录快照。
pub(crate) fn refresh_live_external_tool_definitions(
    input: RefreshLiveExternalToolDefinitionsInput<'_>,
) -> LiveExternalToolSurface {
    let RefreshLiveExternalToolDefinitionsInput {
        mut definitions,
        tool_registry,
        skill_runtime,
        active_skill_id,
        access_profile,
        allowed_tools,
        denied_tools,
        loaded_tool_names,
    } = input;
    definitions.retain(|definition| {
        !definition.function.name.starts_with("mcp__")
            && !definition.function.name.starts_with("plugin__")
    });
    let skill_allowed_tools = active_skill_allowed_tools(skill_runtime, active_skill_id);
    let catalog = tool_registry.external_tool_catalog_snapshot();
    let mut visible_servers = std::collections::BTreeSet::new();
    let mut mcp_tools = catalog.mcp_tools;
    mcp_tools.sort_by(|left, right| left.model_tool_name.cmp(&right.model_tool_name));
    if tool_registry.external_mcp_tool_executor_available() {
        for tool in mcp_tools {
            if magi_tool_runtime::external_tool_access_decision(tool.read_only, access_profile)
                == Some(ExecutionResultStatus::Rejected)
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
            visible_servers.insert(tool.server_id);
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
    for tool in tool_registry.plugin_tool_catalog() {
        if !loaded_tool_names.contains(&tool.model_tool_name)
            || magi_tool_runtime::external_tool_access_decision(tool.read_only, access_profile)
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
    let mut servers = catalog.mcp_servers;
    servers.sort_by(|left, right| left.server_id.cmp(&right.server_id));
    let instructions = servers
        .into_iter()
        .filter_map(|server| {
            if !visible_servers.contains(&server.server_id) {
                return None;
            }
            let instructions = server.instructions?;
            (!instructions.trim().is_empty())
                .then(|| format!("MCP server {}:\n{}", server.server_id, instructions.trim()))
        })
        .collect::<Vec<_>>();
    LiveExternalToolSurface {
        definitions,
        mcp_instructions: (!instructions.is_empty()).then(|| instructions.join("\n\n")),
    }
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
    fn plugin_discovery_loads_selected_schema_and_rechecks_live_policy() {
        let enabled = Arc::new(AtomicBool::new(true));
        let provider_enabled = Arc::clone(&enabled);
        let registry = ToolRegistry::new(
            Arc::new(magi_governance::GovernanceService::default()),
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        )
        .with_plugin_tools(
            Arc::new(move || {
                if !provider_enabled.load(Ordering::Acquire) {
                    return Vec::new();
                }
                ["analyze", "write"]
                    .into_iter()
                    .map(|name| magi_tool_runtime::PluginToolCatalogEntry {
                        plugin_id: "example.analysis".into(),
                        contribution_id: name.into(),
                        model_tool_name: format!("plugin__analysis__{name}"),
                        title: name.into(),
                        description: name.into(),
                        read_only: name == "analyze",
                        input_schema: serde_json::json!({"type":"object","properties":{
                            "evidence": {"type":"string", "description":"retained evidence ".repeat(2_000)}
                        }}),
                    })
                    .collect()
            }),
            Arc::new(|_, _, _, _| ("{}".into(), ExecutionResultStatus::Succeeded)),
        );
        let refresh = |loaded: &std::collections::BTreeSet<String>,
                       access_profile,
                       denied_tools: &[String]| {
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile,
                allowed_tools: None,
                denied_tools,
                loaded_tool_names: loaded,
            })
            .definitions
        };
        let mut loaded = std::collections::BTreeSet::new();
        assert!(refresh(&loaded, AccessProfile::Restricted, &[]).is_empty());
        let metadata = registry.tool_catalog_value(
            r#"{"query":"PLUGIN ANALYSIS ANALYZE","include_schema":true}"#,
            &Default::default(),
        );
        assert_eq!(metadata["plugin_tools"].as_array().unwrap().len(), 1);
        assert!(metadata["tools"].as_array().unwrap().is_empty());
        assert!(
            metadata.to_string().len()
                < crate::tool_result_utils::MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES
        );
        assert!(metadata["plugin_tools"][0].get("input_schema").is_none());
        extend_discovered_tool_names(&mut loaded, &metadata.to_string());
        assert!(loaded.is_empty(), "元数据查询不能加载整个插件目录");
        let selected = registry.tool_catalog_value(
            r#"{"query":"additional descriptive words","tool_names":["plugin__analysis__analyze"],"include_schema":true}"#,
            &Default::default(),
        );
        let raw = selected.to_string();
        assert!(raw.len() > crate::tool_result_utils::MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES);
        let visible =
            crate::history_projection::project_tool_result(&raw, ExecutionResultStatus::Succeeded);
        let visible: serde_json::Value = serde_json::from_str(&visible).unwrap();
        assert_eq!(
            visible["plugin_tools"][0]["model_tool_name"],
            "plugin__analysis__analyze"
        );
        assert_eq!(visible["plugin_tools"][0]["schema_included"], true);
        assert!(visible["plugin_tools"][0].get("input_schema").is_none());
        extend_discovered_tool_names(&mut loaded, &selected.to_string());
        let definitions = refresh(&loaded, AccessProfile::Restricted, &[]);
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].function.name, "plugin__analysis__analyze");
        assert_eq!(
            definitions[0].function.parameters,
            selected["plugin_tools"][0]["input_schema"]
        );
        assert!(
            refresh(
                &loaded,
                AccessProfile::Restricted,
                &["plugin__analysis__analyze".into()]
            )
            .is_empty()
        );
        loaded.insert("plugin__analysis__write".into());
        assert_eq!(refresh(&loaded, AccessProfile::ReadOnly, &[]).len(), 1);
        enabled.store(false, Ordering::Release);
        assert!(
            refresh(&loaded, AccessProfile::Restricted, &[]).is_empty(),
            "停用后已发现工具也须失效"
        );
    }

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
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::ReadOnly,
                allowed_tools: None,
                denied_tools: &[],
                loaded_tool_names: &std::collections::BTreeSet::from([
                    "mcp__repo__inspect".into(),
                    "mcp__repo__write".into(),
                ]),
            })
            .definitions;
        let names = definitions
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["mcp__repo__inspect"]);
    }

    #[test]
    fn live_mcp_surface_does_not_depend_on_discovery_state() {
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
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::FullAccess,
                allowed_tools: None,
                denied_tools: &[],
                loaded_tool_names: &std::collections::BTreeSet::from([
                    "mcp__repo__inspect".into(),
                    "mcp__repo__write".into(),
                ]),
            })
            .definitions;
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
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::Restricted,
                allowed_tools: None,
                denied_tools: &[],
                loaded_tool_names: &std::collections::BTreeSet::new(),
            })
            .definitions;
        assert_eq!(
            first_round.len(),
            1,
            "当前 live MCP schema 必须完整进入工具面"
        );

        let after_catalog =
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                definitions: first_round,
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::Restricted,
                allowed_tools: None,
                denied_tools: &[],
                loaded_tool_names: &std::collections::BTreeSet::from([
                    "mcp__repo__inspect".into(),
                    "mcp__repo__write".into(),
                ]),
            })
            .definitions;
        assert_eq!(
            after_catalog
                .iter()
                .map(|definition| definition.function.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__repo__inspect"]
        );
    }

    fn history_pair(
        call_id: &str,
        tool_name: &str,
        result: serde_json::Value,
    ) -> Vec<magi_session_store::ThreadChatMessage> {
        vec![
            magi_session_store::ThreadChatMessage {
                input_request_id: None,
                role: "assistant".into(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![magi_session_store::ThreadChatToolCall {
                    id: call_id.into(),
                    kind: "function".into(),
                    function: magi_session_store::ThreadChatToolFunction {
                        name: tool_name.into(),
                        arguments: "{}".into(),
                    },
                }],
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            magi_session_store::ThreadChatMessage {
                input_request_id: None,
                role: "tool".into(),
                content: Some(result.to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(call_id.into()),
                provider_context: Vec::new(),
            },
        ]
    }

    #[test]
    fn discovered_plugin_tools_are_restored_from_history_across_turns() {
        let plugin = |name: &str, schema_included: bool| {
            serde_json::json!({
                "model_tool_name": name,
                "model_available": true,
                "schema_included": schema_included,
            })
        };
        let catalog = |tools: Vec<serde_json::Value>| serde_json::json!({"tool": "tool_catalog", "status": "succeeded", "plugin_tools": tools});
        let mut history = history_pair(
            "call-a",
            "tool_catalog",
            // 历史投影已去掉 input_schema，只保留回执当时的 schema_included。
            catalog(vec![
                plugin("plugin__a__run", true),
                plugin("plugin__a__metadata_only", false),
            ]),
        );
        history.extend(history_pair(
            "call-b",
            "tool_catalog",
            serde_json::json!({
                "tool": "tool_catalog", "status": "succeeded", "execution": "skipped",
                "plugin_tools": [plugin("plugin__b__skipped", true)],
            }),
        ));
        history.extend(history_pair(
            "call-c",
            "tool_catalog",
            serde_json::json!({
                "tool": "tool_catalog", "status": "failed",
                "plugin_tools": [plugin("plugin__c__failed", true)],
            }),
        ));
        // 非 tool_catalog 的工具回执即使形状相似也不能加载 schema。
        history.extend(history_pair(
            "call-d",
            "file_read",
            catalog(vec![plugin("plugin__d__other_tool", true)]),
        ));

        let loaded = discovered_tool_names_from_history(&history);
        assert_eq!(
            loaded.into_iter().collect::<Vec<_>>(),
            vec!["plugin__a__run".to_string()]
        );
        // 相同历史恢复出相同集合：下一回合第一步的 tools 面与上一回合末尾一致。
        assert_eq!(
            discovered_tool_names_from_history(&history),
            discovered_tool_names_from_history(&history)
        );
        assert!(discovered_tool_names_from_history(&[]).is_empty());
    }

    #[test]
    fn mcp_surface_keeps_initialize_instructions_with_schema_snapshot() {
        let registry = ToolRegistry::new(
            std::sync::Arc::new(magi_governance::GovernanceService::default()),
            std::sync::Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
        )
        .with_external_tool_catalog_provider(std::sync::Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                mcp_servers: vec![magi_tool_runtime::ExternalMcpServerCatalogEntry {
                    server_id: "tap".into(),
                    name: "Tap".into(),
                    enabled: true,
                    connected: true,
                    health: "connected".into(),
                    tool_count: Some(2),
                    error: None,
                    instructions: Some("Always call ax_read before node actions.".into()),
                }],
                mcp_tools: ["ax_node_action", "ax_read"]
                    .into_iter()
                    .map(|name| magi_tool_runtime::ExternalMcpToolCatalogEntry {
                        server_id: "tap".into(),
                        server_name: "Tap".into(),
                        model_tool_name: format!("mcp__tap__{name}"),
                        tool_name: name.into(),
                        description: "Read UI".into(),
                        read_only: true,
                        input_schema: serde_json::json!({"type":"object"}),
                    })
                    .collect(),
                ..Default::default()
            }
        }))
        .with_external_mcp_tool_executor(std::sync::Arc::new(|_, _, _| {
            ("{}".into(), ExecutionResultStatus::Succeeded)
        }));
        let surface =
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                definitions: Vec::new(),
                tool_registry: &registry,
                skill_runtime: None,
                active_skill_id: None,
                access_profile: AccessProfile::Restricted,
                allowed_tools: None,
                denied_tools: &[],
                loaded_tool_names: &Default::default(),
            });
        assert_eq!(
            surface
                .definitions
                .iter()
                .map(|definition| definition.function.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__tap__ax_node_action", "mcp__tap__ax_read"]
        );
        assert!(
            surface
                .mcp_instructions
                .unwrap()
                .contains("Always call ax_read")
        );
    }
}
