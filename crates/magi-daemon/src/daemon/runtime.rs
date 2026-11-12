use super::{
    browser_host::BrowserHostControllerLifecycle,
    config::{DaemonConfig, DaemonError},
    events::publish_ledger_status_event,
    maintenance::{RuntimeMaintenance, RuntimeMaintenanceConfig},
    persistence::{RuntimeSidecarPersistence, StateRepository},
    plugin_bridge::PluginServices,
};
use magi_api::{
    ApiError, ApiState, DaemonIdentity, RunnerManager, RuntimeStatePersistence,
    SessionProjectionPersistMode, TaskCheckpointPersist, build_router,
    build_runtime_capability_dependency_provider,
    mcp_config::{build_mcp_config_from_entry, mcp_server_entry_enabled, mcp_server_entry_id},
};
use magi_bridge_client::{
    BridgeBindingKind, BridgeClientError, BridgeDispatchRuntime, BridgeErrorLayer, BridgeResponse,
    HttpModelBridgeClient, HttpModelBridgeProtocol,
    ImageGenerationRequest as BridgeImageGenerationRequest, McpBridgeClient, McpServerClient,
    McpToolCallRequest, StdioMcpBridgeClient,
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, FileSummaryStore, ProjectRecentTurnStore, SharedContextPool,
};
use magi_conversation_runtime::{
    CanonicalTurnEventSink,
    execution_admission::ExecutionAdmissionController,
    model_config::{
        NormalizedModelConfig, canonicalize_model_settings, configured_role_engine_model_config,
        resolve_orchestrator_model_config,
    },
    session_turn_finalize::{
        current_turn_status_is_terminal, publish_task_status_turn_item_for_active_sessions,
    },
    task_completion_notifier::TaskCompletionNotifier,
    task_execution_dispatcher::{LlmTaskDispatcher, LlmTaskDispatcherDependencies},
    task_execution_registry::{AgentSpawnPreflightRuntime, TaskExecutionRegistry},
    usage_recording::{
        ModelUsageRecordInput, image_generation_model_usage_binding,
        publish_model_usage_record_with_config,
    },
};
#[cfg(test)]
use magi_core::ExecutionOwnership;
use magi_core::{DomainError, EventId, SessionId, TaskStatus, UtcMillis, public_runtime_excerpt};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_memory_store::MemoryStore;
use magi_orchestrator::task_store::TaskStore;
use magi_process::ManagedProcessGroup;
use magi_session_store::SessionStore;
use magi_settings_store::SettingsStore;
use magi_skill_runtime::SkillDispatchRuntime;
use magi_tool_runtime::{
    AgentRoleCatalogEntry, AgentRoleCatalogProvider, ExternalMcpServerCatalogEntry,
    ExternalMcpToolCatalogEntry, ExternalMcpToolExecutor, ExternalToolCatalogEntry,
    ExternalToolCatalogProvider, ExternalToolCatalogSnapshot, GeneratedImageData,
    ImageGenerationExecutor, ImageGenerationReadinessProvider, ToolRegistry,
    external_mcp_model_tool_name,
};
use magi_usage_authority::UsageCallStatus;
use magi_workspace::WorkspaceStore;
use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
    time::Instant,
};
use tracing::{info, warn};

#[cfg(test)]
struct StaticTestModelBridgeClient;

/// 测试夹具的 workspace 必须与 daemon 全局状态根保持物理分离。
///
/// 生产启动会拒绝两类状态根重叠；测试不能通过把 workspace 放进 state_root
/// 来绕过这条边界，否则所有重启恢复测试都会失去真实覆盖价值。
#[cfg(test)]
fn test_fixture_sibling_root(state_root: &Path, suffix: &str) -> PathBuf {
    let name = state_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("magi-daemon-test-state");
    state_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{name}-{suffix}"))
}

#[cfg(test)]
fn test_fixture_workspace_root(state_root: &Path) -> PathBuf {
    test_fixture_sibling_root(state_root, "workspace")
}

#[cfg(test)]
fn test_fixture_worktree_root(state_root: &Path) -> PathBuf {
    test_fixture_sibling_root(state_root, "worktrees")
}

fn build_external_tool_catalog_provider(
    settings_store: Arc<SettingsStore>,
    skill_runtime: Arc<magi_skill_runtime::SkillRuntime>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
) -> ExternalToolCatalogProvider {
    Arc::new(move || {
        let instruction_skills = skill_runtime.registry().list();
        let instruction_skill_count = instruction_skills.len();
        let mut skill_tools = Vec::new();
        for skill in instruction_skills {
            for binding in skill.custom_tool_bindings {
                let (access_profile_behavior, risk_level, approval_requirement) =
                    external_binding_policy_labels(binding.bridge_kind);
                let status = if binding.tool_name.trim().is_empty()
                    || binding.binding_id.trim().is_empty()
                    || binding.bridge_target.trim().is_empty()
                {
                    "invalid"
                } else {
                    "available"
                };
                skill_tools.push(ExternalToolCatalogEntry {
                    source: "skill".to_string(),
                    skill_id: Some(skill.skill_id.clone()),
                    binding_id: Some(binding.binding_id),
                    name: binding.tool_name,
                    description: binding.description,
                    bridge_kind: bridge_binding_kind_label(binding.bridge_kind).to_string(),
                    dispatch_action: bridge_dispatch_action_label(binding.dispatch_action)
                        .to_string(),
                    bridge_target: binding.bridge_target,
                    access_profile_behavior: access_profile_behavior.to_string(),
                    risk_level: risk_level.to_string(),
                    approval_requirement: approval_requirement.to_string(),
                    status: status.to_string(),
                });
            }
        }
        skill_tools.sort_by(|left, right| {
            left.skill_id
                .cmp(&right.skill_id)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.binding_id.cmp(&right.binding_id))
        });

        let mut mcp_servers = Vec::new();
        let mut mcp_tools = Vec::new();
        let settings_snapshot = settings_store.get_section("mcpServers");
        for entry in settings_snapshot
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(magi_api::mcp_config::normalize_mcp_server_snapshot_entry)
        {
            if let Some((server, tools)) =
                external_mcp_server_catalog_snapshot(&entry, &mcp_connections)
            {
                mcp_servers.push(server);
                mcp_tools.extend(tools);
            }
        }
        mcp_servers.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        mcp_tools.sort_by(|left, right| left.model_tool_name.cmp(&right.model_tool_name));

        ExternalToolCatalogSnapshot {
            instruction_skill_count,
            skill_tools,
            mcp_servers,
            mcp_tools,
        }
    })
}

fn build_external_mcp_tool_executor(
    settings_store: Arc<SettingsStore>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
) -> ExternalMcpToolExecutor {
    Arc::new(move |server_id, tool_name, arguments| {
        let entry = settings_store
            .get_section("mcpServers")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(magi_api::mcp_config::normalize_mcp_server_snapshot_entry)
            .find(|entry| mcp_entry_matches_target(entry, server_id));
        let Some(entry) = entry else {
            return (
                serde_json::json!({
                    "tool": external_mcp_model_tool_name(server_id, tool_name),
                    "status": "failed",
                    "error": "MCP 服务未配置或已移除",
                })
                .to_string(),
                magi_core::ExecutionResultStatus::Failed,
            );
        };
        let client = match mcp_client_for_settings_entry(&entry, &mcp_connections) {
            Ok(client) => client,
            Err(error) => {
                return (
                    serde_json::json!({
                        "tool": external_mcp_model_tool_name(server_id, tool_name),
                        "status": "failed",
                        "error": error,
                    })
                    .to_string(),
                    magi_core::ExecutionResultStatus::Failed,
                );
            }
        };
        match client.call_tool(McpToolCallRequest {
            server_name: server_id.to_string(),
            tool_name: tool_name.to_string(),
            input: arguments.to_string(),
        }) {
            Ok(response) => (
                response.payload,
                if response.ok {
                    magi_core::ExecutionResultStatus::Succeeded
                } else {
                    magi_core::ExecutionResultStatus::Failed
                },
            ),
            Err(error) => {
                if mcp_error_invalidates_connection(&error) {
                    mcp_connections
                        .write()
                        .expect("mcp connections write lock poisoned")
                        .remove(server_id);
                }
                (
                    serde_json::json!({
                        "tool": external_mcp_model_tool_name(server_id, tool_name),
                        "status": "failed",
                        "error": error.to_string(),
                    })
                    .to_string(),
                    magi_core::ExecutionResultStatus::Failed,
                )
            }
        }
    })
}

fn bridge_binding_kind_label(kind: magi_bridge_client::BridgeBindingKind) -> &'static str {
    match kind {
        magi_bridge_client::BridgeBindingKind::Model => "model",
        magi_bridge_client::BridgeBindingKind::Mcp => "mcp",
    }
}

fn bridge_dispatch_action_label(action: magi_bridge_client::BridgeDispatchAction) -> &'static str {
    match action {
        magi_bridge_client::BridgeDispatchAction::ModelPrompt => "model_prompt",
        magi_bridge_client::BridgeDispatchAction::McpToolCall => "mcp_tool_call",
    }
}

fn build_agent_role_catalog_provider(
    registry: Arc<magi_agent_role::AgentRoleRegistry>,
    settings_store: Arc<SettingsStore>,
    default_model_available: bool,
) -> AgentRoleCatalogProvider {
    Arc::new(move || {
        let mut roles = registry
            .all()
            .into_iter()
            .map(|role| {
                let spawnable = registry.is_spawnable_agent_role(&role.id);
                let status = if spawnable {
                    "spawnable"
                } else if role.coordinator_mode {
                    "coordinator_only"
                } else {
                    "unsupported"
                };
                let model_binding_status = agent_role_model_binding_status(
                    settings_store.as_ref(),
                    &role.id,
                    default_model_available,
                );
                let display_name = if role.display_name.trim().is_empty() {
                    role.id.clone()
                } else {
                    role.display_name.clone()
                };
                let description = if role.description.trim().is_empty() {
                    if role.coordinator_mode {
                        "主线任务编排角色".to_string()
                    } else {
                        "可派发的专业代理角色".to_string()
                    }
                } else {
                    role.description.clone()
                };
                AgentRoleCatalogEntry {
                    role_id: role.id.clone(),
                    display_name,
                    description,
                    spawnable,
                    coordinator_mode: role.coordinator_mode,
                    supported_kinds: role
                        .supported_task_kinds()
                        .into_iter()
                        .map(task_kind_label)
                        .collect(),
                    capability_ids: registry.capability_ids_for_role(&role.id),
                    parallelism_limit: role.parallelism_limit,
                    status: status.to_string(),
                    model_binding_status,
                }
            })
            .collect::<Vec<_>>();
        roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
        roles
    })
}

fn agent_role_model_binding_status(
    settings_store: &SettingsStore,
    role_id: &str,
    default_model_available: bool,
) -> String {
    match configured_role_engine_model_config(settings_store, role_id) {
        Ok(Some(_)) => "role_engine".to_string(),
        Err(error) => {
            tracing::debug!(role_id, %error, "agent role model binding is invalid");
            "invalid".to_string()
        }
        Ok(None) => {
            let orchestrator_ready = resolve_orchestrator_model_config(settings_store, None)
                .ok()
                .and_then(|config| config.to_http_model_client())
                .is_some();
            if orchestrator_ready || default_model_available {
                "inherited_orchestrator".to_string()
            } else {
                "unconfigured".to_string()
            }
        }
    }
}

fn task_kind_label(kind: magi_core::TaskKind) -> String {
    match kind {
        magi_core::TaskKind::LocalAgent => "local_agent",
        magi_core::TaskKind::RemoteAgent => "remote_agent",
        magi_core::TaskKind::MonitorMcp => "monitor_mcp",
        magi_core::TaskKind::InProcessTeammate => "in_process_teammate",
        magi_core::TaskKind::Dream => "dream",
    }
    .to_string()
}

fn external_binding_policy_labels(
    bridge_kind: magi_bridge_client::BridgeBindingKind,
) -> (&'static str, &'static str, &'static str) {
    match bridge_kind {
        magi_bridge_client::BridgeBindingKind::Mcp => {
            ("restricted_blocks_high_risk", "high", "required")
        }
        magi_bridge_client::BridgeBindingKind::Model => ("access_profile_inherited", "low", "none"),
    }
}

#[cfg(test)]
fn external_mcp_server_catalog_entry(
    entry: &serde_json::Value,
    mcp_connections: &Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
) -> Option<ExternalMcpServerCatalogEntry> {
    external_mcp_server_catalog_snapshot(entry, mcp_connections).map(|(server, _)| server)
}

fn external_mcp_server_catalog_snapshot(
    entry: &serde_json::Value,
    mcp_connections: &Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
) -> Option<(
    ExternalMcpServerCatalogEntry,
    Vec<ExternalMcpToolCatalogEntry>,
)> {
    let server_id = mcp_server_entry_id(entry)?.to_string();
    let name = read_json_string(entry, &["name"]).unwrap_or_else(|| server_id.clone());
    let enabled = entry
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let live_tools = enabled.then(|| {
        mcp_client_for_settings_entry(entry, mcp_connections)
            .and_then(|client| client.list_tools().map_err(|error| error.to_string()))
    });
    if live_tools.as_ref().is_some_and(Result::is_err) {
        mcp_connections
            .write()
            .expect("mcp connections write lock poisoned")
            .remove(&server_id);
    }
    let connected = enabled && live_tools.as_ref().is_some_and(Result::is_ok);
    let tool_count = live_tools
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(Vec::len);
    let health = if !enabled {
        "disabled"
    } else if connected {
        "connected"
    } else {
        "disconnected"
    };
    let tools = live_tools
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .into_iter()
        .flatten()
        .map(|tool| ExternalMcpToolCatalogEntry {
            server_id: server_id.clone(),
            server_name: name.clone(),
            model_tool_name: external_mcp_model_tool_name(&server_id, &tool.name),
            tool_name: tool.name.clone(),
            description: tool.description.clone().unwrap_or_default(),
            read_only: tool
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            input_schema: tool
                .input_schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} })),
        })
        .collect::<Vec<_>>();
    Some((
        ExternalMcpServerCatalogEntry {
            server_id,
            name,
            enabled,
            connected,
            health: health.to_string(),
            tool_count,
            error: if enabled && live_tools.as_ref().is_some_and(Result::is_err) {
                Some("mcp_connection_failed".to_string())
            } else if enabled {
                external_mcp_error_marker(entry)
            } else {
                None
            },
        },
        tools,
    ))
}

fn mcp_client_for_settings_entry(
    entry: &serde_json::Value,
    mcp_connections: &Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
) -> Result<Arc<McpServerClient>, String> {
    let server_id = mcp_server_entry_id(entry)
        .map(str::to_string)
        .ok_or_else(|| "MCP 服务缺少稳定 ID".to_string())?;
    if !mcp_server_entry_enabled(entry) {
        mcp_connections
            .write()
            .expect("mcp connections write lock poisoned")
            .remove(&server_id);
        return Err(format!("MCP server {server_id} is disabled"));
    }
    if let Some(client) = mcp_connections
        .read()
        .expect("mcp connections read lock poisoned")
        .get(&server_id)
        .cloned()
    {
        return Ok(client);
    }
    let config = build_mcp_config_from_entry(entry)
        .ok_or_else(|| format!("MCP server {server_id} config is incomplete"))?;
    let client = Arc::new(McpServerClient::new(config));
    let client = mcp_connections
        .write()
        .expect("mcp connections write lock poisoned")
        .entry(server_id)
        .or_insert(client)
        .clone();
    Ok(client)
}

fn external_mcp_error_marker(entry: &serde_json::Value) -> Option<String> {
    read_json_string(entry, &["error"]).map(|error| match error.as_str() {
        "mcp_connection_failed" | "mcp_invalid_config" => error,
        _ => "mcp_connection_failed".to_string(),
    })
}

#[derive(Clone)]
struct SettingsBackedMcpBridgeClient {
    settings_store: Arc<SettingsStore>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
    environment_client: Option<Arc<dyn McpBridgeClient>>,
}

impl SettingsBackedMcpBridgeClient {
    fn new(
        settings_store: Arc<SettingsStore>,
        mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
        environment_client: Option<Arc<dyn McpBridgeClient>>,
    ) -> Self {
        Self {
            settings_store,
            mcp_connections,
            environment_client,
        }
    }

    fn settings_entry_for_target(&self, target: &str) -> Option<serde_json::Value> {
        self.settings_store
            .get_section("mcpServers")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(magi_api::mcp_config::normalize_mcp_server_snapshot_entry)
            .find(|entry| mcp_entry_matches_target(entry, target))
    }
}

impl McpBridgeClient for SettingsBackedMcpBridgeClient {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError> {
        let target = request.server_name.trim();
        if target.is_empty() {
            return Err(mcp_config_unavailable_error(
                "MCP bridge target is empty".to_string(),
            ));
        }

        let Some(entry) = self.settings_entry_for_target(target) else {
            return self
                .environment_client
                .as_ref()
                .ok_or_else(|| {
                    mcp_config_unavailable_error(format!("MCP target is not configured: {target}"))
                })?
                .call_tool(request);
        };
        let server_id = mcp_server_entry_id(&entry)
            .map(str::to_string)
            .unwrap_or_else(|| target.to_string());
        let client = mcp_client_for_settings_entry(&entry, &self.mcp_connections)
            .map_err(mcp_config_unavailable_error)?;
        let response = client.call_tool(request);
        if response
            .as_ref()
            .is_err_and(mcp_error_invalidates_connection)
        {
            self.mcp_connections
                .write()
                .expect("mcp connections write lock poisoned")
                .remove(&server_id);
        }
        response
    }
}

fn mcp_entry_matches_target(entry: &serde_json::Value, target: &str) -> bool {
    ["id", "name"].iter().any(|field| {
        entry
            .get(*field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .is_some_and(|value| value == target)
    })
}

fn mcp_config_unavailable_error(message: String) -> BridgeClientError {
    warn!(reason = %message, "settings-backed MCP bridge target unavailable");
    BridgeClientError::MissingClient {
        bridge_kind: BridgeBindingKind::Mcp,
    }
}

fn mcp_error_invalidates_connection(error: &BridgeClientError) -> bool {
    matches!(
        error.layer(),
        Some(BridgeErrorLayer::Transport | BridgeErrorLayer::Protocol)
    )
}

fn read_json_string(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        value
            .get(*field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

#[derive(Clone)]
struct UnavailableBusinessModelBridgeClient {
    state_root: PathBuf,
}

impl UnavailableBusinessModelBridgeClient {
    fn new(state_root: PathBuf) -> Self {
        Self { state_root }
    }

    fn error(&self) -> magi_bridge_client::BridgeClientError {
        let _ = &self.state_root; // 保留 state_root 字段以便后续扩展定位提示。
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            code: Some(-32004),
            message:
                "业务模型桥未配置：请在设置面板「模型 · 主对话/编排模型」中填入 baseUrl / apiKey，\
                 并在会话输入区选择当前会话主模型，\
                 开发或测试场景也可通过环境变量 MAGI_OPENAI_COMPAT_BASE_URL / MAGI_OPENAI_COMPAT_API_KEY / MAGI_OPENAI_COMPAT_MODEL 提供配置。\
                 settings.json 的 auxiliary 段仅用于辅助模型（会话标题精修 / 知识抽取 / 会话记忆 / Prompt 增强），不参与业务派发。"
                    .to_string(),
        }
    }
}

impl magi_bridge_client::ModelBridgeClient for UnavailableBusinessModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::text_executor_capabilities()
    }

    fn invoke(
        &self,
        _request: magi_bridge_client::ModelInvocationRequest,
    ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError> {
        Err(self.error())
    }

    fn invoke_streaming(
        &self,
        _request: magi_bridge_client::ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError> {
        Err(self.error())
    }
}

struct OpenAiCompatEnvConfig {
    base_url: String,
    api_key: Option<String>,
    model: String,
}

fn orchestrator_settings_is_configured(value: &serde_json::Value) -> bool {
    value
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|sources| !sources.is_empty())
}

#[cfg(test)]
impl magi_bridge_client::ModelBridgeClient for StaticTestModelBridgeClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
    ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError> {
        if let Some(payload) = classifier_payload_for_prompt(&request.prompt) {
            return Ok(static_test_chat_response(payload));
        }
        if request.prompt.contains("代理运行规划器") {
            return Ok(magi_bridge_client::ModelResponse::completed(
                serde_json::json!({
                    "phases": [
                        {
                            "title": "P1",
                            "workPackages": [
                                {
                                    "title": "WP1",
                                    "actions": [
                                        { "title": "A1", "goal": "g1", "dependsOn": [], "writeScope": null }
                                    ]
                                }
                            ]
                        },
                        {
                            "title": "P2",
                            "workPackages": [
                                {
                                    "title": "WP2",
                                    "actions": [
                                        { "title": "A2", "goal": "g2", "dependsOn": [], "writeScope": null }
                                    ]
                                }
                            ]
                        },
                        {
                            "title": "P3",
                            "workPackages": [
                                {
                                    "title": "WP3",
                                    "actions": [
                                        { "title": "A3", "goal": "g3", "dependsOn": [], "writeScope": null }
                                    ]
                                }
                            ]
                        }
                    ]
                })
                .to_string(),
            ));
        }
        Ok(magi_bridge_client::ModelResponse::completed(
            request.prompt.trim(),
        ))
    }

    fn invoke_streaming(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError> {
        self.invoke(request)
    }
}

#[cfg(test)]
fn static_test_chat_response(payload: String) -> magi_bridge_client::ModelResponse {
    magi_bridge_client::ModelResponse::from_chat_payload(
        serde_json::from_str(&payload).expect("daemon 测试模型响应必须符合统一响应结构"),
    )
}

#[cfg(test)]
fn classifier_payload_for_prompt(prompt: &str) -> Option<String> {
    if !prompt.contains("Session Turn 编排分类器") {
        return None;
    }
    let has_recoverable_chain = prompt
        .lines()
        .any(|line| line.trim() == "hasRecoverableChain=true");
    let user_text = prompt
        .lines()
        .find_map(|line| line.trim().strip_prefix("userText="))
        .unwrap_or("");
    let route = if has_recoverable_chain && user_text.contains("继续") {
        "continue"
    } else if !prompt.contains("skillName=\"\"")
        || !prompt.contains("imageCount=0")
        || user_text.contains("复杂任务")
        || user_text.contains("分析并拆分")
    {
        "task"
    } else {
        "chat"
    };
    let task_tier = "execution_chain";
    let arguments = serde_json::json!({
        "route": route,
        "taskTitle": (route == "task").then_some("模型判定任务"),
        "executionGoal": (route == "task").then_some(user_text.trim_matches('"')),
        "taskTier": task_tier,
        "toolIntent": null,
        "confidence": (route == "task").then_some(0.95),
        "reasonCode": (route == "task").then_some("explicit_task_request"),
        "routeReason": (route == "task").then_some("测试 stub 判定任务路由"),
        "taskEvidence": (route == "task").then_some(vec!["test-stub-classifier"]),
    });
    Some(
        serde_json::json!({
            "content": null,
            "finish_reason": "tool_calls",
            "tool_calls": [{
                "id": "call-classify-session-turn",
                "type": "function",
                "function": {
                    "name": "classify_session_turn",
                    "arguments": arguments.to_string(),
                }
            }]
        })
        .to_string(),
    )
}

#[derive(Clone)]
pub(crate) struct DaemonRuntime {
    state_root: PathBuf,
    state_repository: StateRepository,
    local_port: u16,
    daemon_identity: DaemonIdentity,
    event_bus: Arc<InMemoryEventBus>,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    knowledge_store: Arc<KnowledgeStore>,
    governance: Arc<GovernanceService>,
    runtime_maintenance: RuntimeMaintenance,
    managed_process_group: ManagedProcessGroup,
    browser_host_controller_lifecycle: BrowserHostControllerLifecycle,
    /// 插件安装、授权和生命周期的唯一 daemon 所有者；UI 只通过 API 投影访问。
    plugins: PluginServices,
}

impl DaemonRuntime {
    pub(crate) fn restore(config: &DaemonConfig) -> Result<Self, DaemonError> {
        let state_repository = StateRepository::new(config.state_root.clone());

        // 先加载工作区注册表（需要工作区路径来定位会话文件）
        let workspace_durable_state = state_repository.load_workspace_durable_state()?;
        let workspace_roots: Vec<(String, PathBuf)> = workspace_durable_state
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.workspace_id.to_string(),
                    workspace.native_root_path(),
                )
            })
            .collect();
        let source_roots = workspace_durable_state
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .source_folder_roots()
                    .into_iter()
                    .map(|root| (workspace.workspace_id.to_string(), root))
            })
            .collect::<Vec<_>>();
        Self::validate_workspace_state_separation(&config.state_root, &source_roots)?;
        // 先恢复旧事务并建立旧 daemon 的写屏障，再单向迁移项目历史。
        state_repository.verify_state_layout(&workspace_roots)?;
        state_repository.migrate_project_storage()?;
        let workspace_durable_state = state_repository.load_workspace_durable_state()?;
        let workspace_store = Arc::new(WorkspaceStore::from_persisted_parts(
            workspace_durable_state,
            state_repository.load_workspace_recovery_sidecars()?,
        ));

        let (session_durable, session_sidecars) =
            state_repository.load_session_projections(&workspace_roots)?;
        let session_store = Arc::new(
            SessionStore::from_persisted_parts(session_durable, session_sidecars).map_err(
                |error| DaemonError::internal(format!("恢复 session canonical turn 失败: {error}")),
            )?,
        );
        session_store.install_session_event_writer(Arc::new(state_repository.clone()));
        session_store
            .reconcile_background_jobs_after_restart()
            .map_err(DaemonError::internal)?;
        session_store.set_unavailable_sessions(state_repository.unavailable_sessions());
        state_repository.validate_session_event_log_coverage(&session_store.durable_state())?;
        let knowledge_store = Arc::new(KnowledgeStore::from_state(
            state_repository.load_knowledge_state()?,
        ));
        let event_bus = Arc::new(InMemoryEventBus::new(2048));
        let runtime_persistence = RuntimeSidecarPersistence::new(
            state_repository.clone(),
            session_store.clone(),
            workspace_store.clone(),
        );

        Self::restore_ledger(&state_repository, &event_bus)?;
        let repaired_usage =
            magi_conversation_runtime::canonical_usage::recover_model_usage_projections(
                &event_bus,
                &session_store,
            )
            .map_err(|error| DaemonError::internal(format!("恢复模型用量投影失败: {error}")))?;
        if repaired_usage > 0 {
            event_bus
                .refresh_audit_usage_ledger_persistence()
                .map_err(|error| {
                    DaemonError::internal(format!("模型用量恢复投影落盘失败: {error}"))
                })?;
        }
        Self::persist_restored_runtime_state(
            &state_repository,
            &runtime_persistence,
            &workspace_store,
        )?;
        let runtime_maintenance = RuntimeMaintenance::new(
            RuntimeMaintenanceConfig::default(),
            event_bus.clone(),
            runtime_persistence,
            session_store.clone(),
            workspace_store.clone(),
        );
        runtime_maintenance.publish_runtime_status_event("system-runtime-maintenance-ready");

        let managed_process_group = ManagedProcessGroup::new();
        let plugins = PluginServices::open(&config.state_root, managed_process_group.clone())?;
        Ok(Self {
            state_root: config.state_root.clone(),
            state_repository,
            local_port: config.port,
            daemon_identity: config.identity(),
            event_bus,
            session_store,
            workspace_store,
            knowledge_store,
            governance: Arc::new(GovernanceService::default()),
            runtime_maintenance,
            managed_process_group,
            browser_host_controller_lifecycle: BrowserHostControllerLifecycle::new(),
            plugins,
        })
    }

    /// 校验全局状态根与项目 workspace 状态根的物理边界。
    ///
    /// 全局状态和 workspace 状态由不同的所有者管理：前者属于 daemon，后者
    /// 属于项目目录。二者一旦指向同一个 `.magi`，projection 扫描就无法区分
    /// personal 与 workspace session，恢复只能靠猜测。该配置直接拒绝，不能
    /// 通过迁移、补字段或自动合并掩盖边界错误。
    fn validate_workspace_state_separation(
        state_root: &Path,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        for (workspace_id, workspace_root) in workspace_roots {
            let overlaps = magi_runtime_state::state_roots_overlap(state_root, workspace_root)
                .map_err(|error| {
                    DaemonError::internal(format!("解析 daemon 与 workspace 状态边界失败: {error}"))
                })?;
            if overlaps {
                return Err(DaemonError::internal(format!(
                    "daemon 状态根与 workspace 状态根重合，拒绝启动: state_root={} workspace_id={} workspace_state_root={}",
                    state_root.display(),
                    workspace_id,
                    workspace_root.join(".magi").display()
                )));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn restore_with_test_fixture(config: &DaemonConfig) -> Result<Self, DaemonError> {
        let runtime = Self::restore(config)?;
        if !runtime.workspace_store.is_empty() {
            return Ok(runtime);
        }

        let session_id = SessionId::new("test-session-001");
        let workspace_id = magi_core::WorkspaceId::new("test-workspace-001");
        let workspace_root = test_fixture_workspace_root(&config.state_root);
        let worktree_root =
            test_fixture_worktree_root(&config.state_root).join("test-worktree-001");
        std::fs::create_dir_all(&workspace_root)?;

        runtime
            .session_store
            .create_session(session_id.clone(), "Runtime test session")
            .map_err(|error| DaemonError::internal(format!("创建测试会话失败: {error}")))?;
        runtime
            .workspace_store
            .register(
                workspace_id.clone(),
                magi_core::AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
            )
            .map_err(|error| DaemonError::internal(format!("注册测试工作区失败: {error}")))?;
        runtime
            .workspace_store
            .activate(&workspace_id)
            .map_err(|error| DaemonError::internal(format!("激活测试工作区失败: {error}")))?;

        let ownership = ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            execution_chain_ref: Some("test-execution-chain-001".to_string()),
            ..ExecutionOwnership::default()
        };
        runtime
            .workspace_store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ownership.clone(),
                magi_core::AbsolutePath::new(worktree_root.to_string_lossy().to_string()),
            )
            .map_err(|error| DaemonError::internal(format!("分配测试 worktree 失败: {error}")))?;
        let snapshot = runtime.workspace_store.append_execution_snapshot(
            workspace_id.clone(),
            ownership.clone(),
            "snapshot-bootstrap",
            "测试工作区初始快照",
        );
        let recovery = runtime.workspace_store.prepare_recovery_entry(
            workspace_id,
            ownership.clone(),
            snapshot.snapshot_id,
            "recovery-bootstrap",
            Some("测试恢复入口".to_string()),
        );
        runtime
            .session_store
            .bind_execution_ownership_for_test(session_id.clone(), ownership);
        runtime
            .session_store
            .set_recovery_id(&session_id, None, Some(recovery.recovery_id))
            .map_err(|error| DaemonError::internal(format!("绑定测试恢复入口失败: {error}")))?;

        let state_repository = runtime.state_repository.clone();
        let runtime_persistence = RuntimeSidecarPersistence::new(
            state_repository.clone(),
            runtime.session_store.clone(),
            runtime.workspace_store.clone(),
        );
        Self::persist_initial_runtime_state(
            &state_repository,
            &runtime_persistence,
            &runtime.workspace_store,
        )?;
        Ok(runtime)
    }

    pub(crate) fn start_background_tasks(&self) {
        let runtime_maintenance = self.runtime_maintenance.clone();
        tokio::spawn(async move {
            runtime_maintenance.run_loop().await;
        });

        let Some(active_workspace_id) = self.workspace_store.active_workspace_id() else {
            return;
        };
        let Some(active_workspace) = self
            .workspace_store
            .workspaces()
            .into_iter()
            .find(|workspace| workspace.workspace_id == active_workspace_id)
        else {
            return;
        };
        let multi_source = active_workspace.source_folders.len() > 1;
        for folder in active_workspace.source_folders {
            let scan_root = folder.native_path();
            if std::fs::canonicalize(&scan_root).is_ok_and(|canonical| canonical != scan_root) {
                warn!(workspace_id = %active_workspace_id, source_folder_id = %folder.source_folder_id, "源文件夹原登记路径已变化，跳过代码索引预热");
                continue;
            }
            let knowledge_store = if multi_source {
                match self
                    .knowledge_store
                    .for_source_folder(&folder.source_folder_id, &scan_root)
                {
                    Ok(store) => store,
                    Err(error) => {
                        warn!(workspace_id = %active_workspace_id, source_folder_id = %folder.source_folder_id, %error, "活动项目代码索引作用域无效");
                        continue;
                    }
                }
            } else {
                self.knowledge_store.as_ref().clone()
            };
            if !knowledge_store.begin_workspace_index_build(&active_workspace_id) {
                continue;
            }
            let workspace_id = active_workspace_id.clone();
            tokio::spawn(async move {
                let build_store = knowledge_store.clone();
                let build_workspace_id = workspace_id.clone();
                let result = tokio::task::spawn_blocking(move || {
                    build_store.build_workspace_index(&build_workspace_id, &scan_root)
                })
                .await;
                if let Err(error) = result {
                    warn!(workspace_id = %workspace_id, scope = ?knowledge_store.code_index_scope(), %error, "活动项目代码索引后台构建失败");
                }
                knowledge_store.finish_workspace_index_build(&workspace_id);
            });
        }
    }

    pub(crate) fn prepare_graceful_shutdown(
        &self,
        reason: impl Into<String>,
    ) -> Result<(), DaemonError> {
        // 关闭闸门先收口所有仍在运行的 canonical Turn，再终止工具/子进程并
        // 关闭 HTTP server。不能只依赖进程退出或下一次 restore 才修复孤儿 Turn：
        // Desktop shutdown 的完成证据必须在当前 daemon 生命周期内看到唯一终态。
        let settled_turn_count = self.interrupt_active_session_turns_for_shutdown();
        if settled_turn_count > 0 {
            info!(
                settled_turn_count,
                "daemon 关闭前已将活动 Session Turn 收口为 canonical terminal"
            );
        }
        self.runtime_maintenance.request_graceful_shutdown(reason);
        self.browser_host_controller_lifecycle.request_shutdown();
        let cancelled_execution_count = ToolRegistry::cancel_all_active_executions();
        if cancelled_execution_count > 0 {
            info!(
                cancelled_execution_count,
                "daemon 关闭前已取消工具执行并终止进程树"
            );
        }
        let cancelled_managed_process_count = self.managed_process_group.terminate_all();
        if cancelled_managed_process_count > 0 {
            info!(
                cancelled_managed_process_count,
                "daemon 关闭前已终止全部受管子进程树"
            );
        }
        self.runtime_maintenance.run_once()?;
        Ok(())
    }

    fn interrupt_active_session_turns_for_shutdown(&self) -> usize {
        let sink = CanonicalTurnEventSink::for_store(&self.session_store, None);
        let mut settled_count = 0usize;
        for sidecar in self.session_store.runtime_sidecars() {
            let Some(turn) = sidecar.current_turn.as_ref() else {
                continue;
            };
            if current_turn_status_is_terminal(&turn.status) {
                continue;
            }
            // 普通 conversation Turn 可能已经 durable accepted 但尚未建立
            // task execution chain；关闭闸门仍必须把它收口，不能因为缺少
            // 可恢复链而留下非终态 current_turn。已有 execution chain 的
            // Task/sidecar 保留 daemon-restart interruption 语义，没有链的
            // Turn 使用同一 canonical sink 的 cancel 语义。
            let settled = if sidecar.active_execution_chain.is_some() {
                sink.interrupt_turn_by_daemon_restart(&sidecar.session_id)
            } else {
                sink.cancel_turn(&sidecar.session_id)
            };
            match settled {
                Ok(Some(_)) => settled_count += 1,
                Ok(None) => {}
                Err(error) => warn!(
                    session_id = %sidecar.session_id,
                    ?error,
                    "daemon 关闭时收口活动 Session Turn 失败"
                ),
            }
        }
        settled_count
    }

    pub(crate) fn publish_started_event(&self, service_name: &str) {
        let _ = self.event_bus.publish(EventEnvelope::system(
            EventId::new("system-started"),
            "system.started",
            serde_json::json!({
                "service": service_name,
                "mode": "daemon"
            }),
        ));
    }

    fn build_api_state(&self, service_name: String) -> Result<ApiState, DaemonError> {
        self.build_api_state_with_options(service_name, &[], None)
    }

    fn build_api_state_with_options(
        &self,
        service_name: String,
        bridge_env: &[(&str, &str)],
        model_bridge_override: Option<Arc<dyn magi_bridge_client::ModelBridgeClient>>,
    ) -> Result<ApiState, DaemonError> {
        let mcp_connections = Arc::new(RwLock::new(HashMap::new()));
        // 创建带持久化路径的设置存储，并从磁盘恢复已有设置
        let settings_store = Arc::new(SettingsStore::with_persistence_path(
            self.state_root.join("settings.json"),
        ));
        settings_store
            .load_from_disk()
            .map_err(|error| DaemonError::internal(format!("加载设置文件失败: {error}")))?;
        magi_api::recover_role_delete_transaction(&self.state_root, &settings_store)
            .map_err(|error| DaemonError::internal(format!("恢复代理角色删除事务失败: {error}")))?;
        canonicalize_model_settings(&settings_store)
            .map_err(|error| DaemonError::internal(format!("清理模型设置失败: {error}")))?;
        self.plugins.bind_settings(&settings_store);
        let appearance_library = Arc::new(
            magi_appearance::AppearanceLibrary::open(self.state_root.join("appearance"))
                .map_err(|error| DaemonError::internal(format!("加载外观主题失败: {error}")))?,
        );
        Self::seed_orchestrator_settings_from_env_if_empty(&settings_store, bridge_env)
            .map_err(|error| DaemonError::internal(format!("保存环境模型设置失败: {error}")))?;
        let environment_model_client = if model_bridge_override.is_some() {
            None
        } else {
            Self::try_build_http_model_client(bridge_env)
        };
        let default_model_available =
            model_bridge_override.is_some() || environment_model_client.is_some();
        let agent_role_registry = Arc::new(
            magi_agent_role::AgentRoleRegistry::load_from_state_root(&self.state_root)
                .map_err(|error| DaemonError::internal(format!("加载代理角色失败: {error}")))?,
        );
        // 内置 Skill（如命名隧道配置向导）：失败只记日志，不影响 daemon 启动。
        if let Err(error) =
            magi_api::builtin_skills::install_builtin_skills(&settings_store, &self.state_root)
        {
            tracing::warn!(%error, "内置 Skill 安装失败");
        }
        let app_skill_runtime = Arc::new(
            magi_api::skill_loader::build_skill_runtime_from_settings(&settings_store),
        );
        let external_tool_catalog_provider = build_external_tool_catalog_provider(
            settings_store.clone(),
            app_skill_runtime.clone(),
            mcp_connections.clone(),
        );
        let external_mcp_tool_executor =
            build_external_mcp_tool_executor(settings_store.clone(), mcp_connections.clone());
        let agent_role_catalog_provider = build_agent_role_catalog_provider(
            agent_role_registry.clone(),
            settings_store.clone(),
            default_model_available,
        );
        let snapshot_manager = Arc::new(magi_api::managed_snapshot_manager(
            self.state_root.clone(),
            self.session_store.clone(),
        ));
        let memory_store = MemoryStore::new();
        let context_runtime = ContextRuntime::with_runtime_sources(
            (*self.knowledge_store).clone(),
            memory_store.clone(),
            (*self.session_store).clone(),
            SharedContextPool::default(),
            FileSummaryStore::default(),
            ProjectRecentTurnStore::default(),
        );
        let context_runtime_for_dispatcher = Arc::new(context_runtime);
        let runtime_capability_dependency_provider =
            build_runtime_capability_dependency_provider(true);
        let image_generation_executor: ImageGenerationExecutor = {
            let settings_store = Arc::clone(&settings_store);
            let event_bus = Arc::clone(&self.event_bus);
            let session_store = Arc::clone(&self.session_store);
            Arc::new(move |request, execution_context| {
                let config = settings_store.get_section("imageGeneration");
                let normalized = NormalizedModelConfig::from_settings_value(&config)
                    .map_err(|error| format!("图片生成模型配置无效: {error}"))?;
                let usage_model_config = normalized
                    .to_usage_llm_config()
                    .ok_or_else(|| "图片生成模型配置缺少用量身份字段".to_string())?;
                let client = normalized
                    .to_http_image_generation_client()
                    .map_err(|error| format!("图片生成模型配置无效: {error}"))?;
                let binding = image_generation_model_usage_binding();
                let provider_result = client.generate(BridgeImageGenerationRequest {
                    prompt: request.prompt,
                    size: request.size,
                    quality: request.quality,
                });
                let publish_usage =
                    |usage: Option<&serde_json::Value>,
                     status: UsageCallStatus,
                     error_code: Option<String>| {
                        let Some(session_id) = execution_context.session_id.as_ref() else {
                            warn!(call_id = %execution_context.call_id, "图片模型调用缺少会话标识，无法记录统计");
                            return;
                        };
                        publish_model_usage_record_with_config(
                            &event_bus,
                            &session_store,
                            Some(&settings_store),
                            ModelUsageRecordInput {
                                context_breakdown: None,
                                session_id,
                                workspace_id: &execution_context.workspace_id,
                                binding: &binding,
                                call_id: execution_context.call_id.clone(),
                                usage,
                                status,
                                assignment_id: None,
                                error_code,
                            },
                            usage_model_config.clone(),
                        );
                    };
                let generated = match provider_result {
                    Ok(generated) => {
                        publish_usage(generated.usage.as_ref(), UsageCallStatus::Success, None);
                        generated
                    }
                    Err(error) => {
                        publish_usage(
                            None,
                            UsageCallStatus::Failed,
                            Some("image_generation_provider_failed".to_string()),
                        );
                        return Err(error.to_string());
                    }
                };
                Ok(GeneratedImageData {
                    bytes: generated.bytes,
                    media_type: generated.media_type,
                    revised_prompt: generated.revised_prompt,
                })
            })
        };
        let image_generation_readiness_provider: ImageGenerationReadinessProvider = {
            let settings_store = Arc::clone(&settings_store);
            Arc::new(move || {
                let config = settings_store.get_section("imageGeneration");
                NormalizedModelConfig::from_settings_value(&config)
                    .and_then(|normalized| normalized.to_http_image_generation_client().map(|_| ()))
                    .is_ok()
            })
        };
        let runtime_persistence = Arc::new(
            RuntimeStatePersistence::new(
                self.state_root.clone(),
                self.state_root.join("workspaces.json"),
                self.state_root.join("knowledge.json"),
            )
            .with_shared_write_lock(self.state_repository.shared_write_lock()),
        );
        let git_service = Arc::new(magi_git::GitService::new());
        let session_code_contexts = magi_git::SessionCodeContextRegistry::default();
        let workspace_git_coordinator = magi_git::WorkspaceGitOperationCoordinator::default();
        let task_store_projection_path = self.state_repository.task_store_projection_path();
        let accepted_submission_repository = self.state_repository.clone();
        let accepted_submission_repository_for_checkpoint = accepted_submission_repository.clone();
        let task_checkpoint_persist: TaskCheckpointPersist = Arc::new(move |snapshot| {
            accepted_submission_repository_for_checkpoint
                .checkpoint_task_store_snapshot(snapshot)
                .map_err(|error| DomainError::Persistence {
                    message: format!("task checkpoint 持久化失败: {error}"),
                })?;
            Ok(())
        });
        let restored_from_projection =
            TaskStore::restore_from_projection_directory(&task_store_projection_path)?;
        let mut task_store_requires_checkpoint = false;
        let task_store = if let Some(restored) = restored_from_projection {
            let task_checkpoint_persist_for_store = task_checkpoint_persist.clone();
            restored.set_checkpoint_callback(Box::new(move |store| {
                task_checkpoint_persist_for_store(store)
            }));
            let (revoked_leases, failed_tasks) = restored
                .reconcile_volatile_runtime_after_restore()
                .map_err(|error| {
                    DaemonError::internal(format!("恢复任务运行态收敛失败: {error}"))
                })?;
            if revoked_leases > 0 || failed_tasks > 0 {
                warn!(
                    revoked_leases,
                    failed_tasks, "检测到 projection 中残留的易失执行态，已统一收口为可恢复状态"
                );
            }
            restored
        } else {
            TaskStore::new()
        };
        let task_checkpoint_persist_for_store = task_checkpoint_persist.clone();
        task_store.set_checkpoint_callback(Box::new(move |store| {
            task_checkpoint_persist_for_store(store)
        }));
        // Worker 结果直接提交 TaskStore durable 终态，再由同一完成通知唤醒 Turn 侧。
        let task_store = Arc::new(task_store);
        let git_tool_executor = magi_api::git_tool_runtime::build_git_tool_executor(
            magi_api::git_tool_runtime::GitToolRuntimeDependencies {
                task_store: Some(task_store.clone()),
                git_service: git_service.clone(),
                session_code_contexts: session_code_contexts.clone(),
                workspace_git_coordinator: workspace_git_coordinator.clone(),
                event_bus: self.event_bus.clone(),
                knowledge_store: self.knowledge_store.clone(),
                snapshot_manager: snapshot_manager.clone(),
                runtime_persistence: runtime_persistence.clone(),
                managed_worktree_root: self.state_root.join("worktrees"),
            },
        );
        // ToolRegistry 在 ApiState 之前装配；只捕获共享依赖，避免完整 ApiState 引用环。
        let browser_automation_dependencies =
            Arc::new(OnceLock::<magi_api::BrowserToolRuntimeDependencies>::new());

        let browser_tool_executor: magi_tool_runtime::BrowserToolExecutor = {
            let holder = Arc::clone(&browser_automation_dependencies);

            Arc::new(move |tool_call_id, tool_name, input, context| {
                holder.get().map_or_else(
                    || {
                        (
                            serde_json::json!({
                                "tool": tool_name,
                                "status": "failed",
                                "error_code": "browser_host_not_initialized",
                                "recoverable": false,
                                "requires_user_action": false,
                                "error": "浏览器运行时尚未初始化",
                            })
                            .to_string(),
                            magi_core::ExecutionResultStatus::Failed,
                        )
                    },
                    |dependencies| dependencies.execute(tool_call_id, tool_name, input, context),
                )
            })
        };
        let browser_capability_provider: magi_tool_runtime::BrowserCapabilityProvider = {
            let holder = Arc::clone(&browser_automation_dependencies);
            Arc::new(move || {
                holder.get().map_or_else(
                    || magi_browser_authority::BrowserCapabilitySnapshot {
                        revision: 0,
                        in_app_browser_enabled: false,
                        browser_use_enabled: false,
                        devtools_enabled: false,
                        lan_access_enabled: false,
                        host_status: magi_browser_authority::BrowserHostStatus::Stopped,
                        host_protocol_compatible: false,
                    },
                    |dependencies| dependencies.capabilities(),
                )
            })
        };
        let mut tool_registry = ToolRegistry::new(self.governance.clone(), self.event_bus.clone())
            .with_background_job_journal(
                self.session_store
                    .background_job_journal(self.state_root.join("job-output")),
            )
            .with_knowledge_store(self.knowledge_store.clone())
            .with_external_tool_catalog_provider(external_tool_catalog_provider)
            .with_external_mcp_tool_executor(external_mcp_tool_executor)
            .with_plugin_tools(
                self.plugins.tool_catalog_provider(),
                self.plugins
                    .tool_executor(tokio::runtime::Handle::current()),
            )
            .with_web_provider(self.plugins.web_executor(tokio::runtime::Handle::current()))
            .with_agent_role_catalog_provider(agent_role_catalog_provider)
            .with_image_generation_runtime(
                image_generation_executor,
                image_generation_readiness_provider,
            )
            .with_git_tool_executor(git_tool_executor)
            .with_browser_automation(browser_tool_executor, browser_capability_provider)
            .with_runtime_capability_dependency_provider(runtime_capability_dependency_provider);
        tool_registry.register_default_builtins();

        // 会话正文、任务执行与辅助调用使用正式模型客户端。
        //
        // 单一事实源（按优先级，由 task_execution_dispatcher::resolve_target_for_role
        // 的 RoleTarget::Orchestrator 分支串联）：
        //   1. 测试场景 model_bridge_override 注入的 stub
        //   2. settings.json 的 `orchestrator` 段（前端「主对话/编排模型」表单写入位置，
        //      携带 reasoningEffort / apiProtocol 全套字段，是业务模型的权威入口）
        //   3. 此处 daemon bootstrap 注入的环境变量配置 client（MAGI_OPENAI_COMPAT_*）
        //      —— 仅在 settings.json 未配置 orchestrator 段时生效，
        //      用于开发/测试不带 UI 也能跑通的场景。
        //
        // settings.json 的 `auxiliary` 段不参与业务派发，只服务于会话标题、知识抽取、
        // 会话记忆、Prompt 增强等辅助任务（通过 RoleTarget::Auxiliary 分支独立解析）。
        // 仅保留显式配置的真实环境 MCP；未配置时没有模拟服务或默认成功结果。
        let environment_mcp_client = StdioMcpBridgeClient::from_env().map(|client| {
            Arc::new(client.with_process_group(self.managed_process_group.clone()))
                as Arc<dyn McpBridgeClient>
        });

        let business_model_client: Arc<dyn magi_bridge_client::ModelBridgeClient> =
            match model_bridge_override.clone() {
                Some(client) => client,
                None => {
                    if let Some(client) = environment_model_client {
                        // 环境配置只在 bootstrap 解析一次。实际 HTTP client 同时
                        // 拥有缓存 IR、连接身份和传输合同，不经另一层重建/转发。
                        Arc::new(client)
                    } else {
                        warn!(
                            state_root = %self.state_root.display(),
                            orchestrator_configured = orchestrator_settings_is_configured(
                                &settings_store.get_section("orchestrator"),
                            ),
                            "业务模型的环境默认桥未配置；真实会话仍会按会话/设置中的主模型配置创建 HTTP client，只有解析不到有效主模型时才返回可操作配置错误。未配置时不会生成模拟模型结果"
                        );
                        Arc::new(UnavailableBusinessModelBridgeClient::new(
                            self.state_root.clone(),
                        ))
                    }
                }
            };
        let settings_backed_mcp_client = SettingsBackedMcpBridgeClient::new(
            settings_store.clone(),
            mcp_connections.clone(),
            environment_mcp_client,
        );
        let bridge_runtime = BridgeDispatchRuntime::new()
            .with_model_client(business_model_client.clone())
            .with_mcp_client(Arc::new(settings_backed_mcp_client));
        let skill_runtime = SkillDispatchRuntime::new(tool_registry.clone(), bridge_runtime);
        let tool_registry_for_dispatcher = tool_registry;
        let event_bus_for_task_store = self.event_bus.clone();
        let session_store_for_task_status = self.session_store.clone();
        // `agent_spawn` 创建前预检与 Runner 必须共享同一个执行准入控制器，否则
        // 预检看到的容量和真正派发时的容量可能不一致，导致“已创建后才排队/失败”。
        let execution_admission = Arc::new(ExecutionAdmissionController::default());
        let task_execution_registry = TaskExecutionRegistry::default()
            .with_execution_admission(Arc::clone(&execution_admission))
            .with_agent_spawn_preflight_runtime(AgentSpawnPreflightRuntime {
                settings_store: Some(settings_store.clone()),
                // UnavailableBusinessModelBridgeClient 不能作为有效预检客户端；
                // settings/orchestrator 的有效配置仍由 resolver 单独解析。
                default_model_client: default_model_available
                    .then(|| business_model_client.clone()),
                workspace_registry: Some(self.workspace_store.clone()),
                session_code_contexts: Some(session_code_contexts.clone()),
                git_service_configured: true,
                skill_runtime: Some(app_skill_runtime.clone()),
            });
        let completion_notifier = Arc::new(TaskCompletionNotifier::new(Arc::clone(&task_store)));
        let accepted_submissions = self.state_repository.load_accepted_submissions()?;
        let mut restored_accepted_task_count = 0;
        for record in accepted_submissions
            .iter()
            .filter(|record| !record.task_checkpointed)
        {
            let Some(task) = record.task.as_ref() else {
                continue;
            };
            if task_store.get_task(&task.task_id).is_some() {
                continue;
            }
            task_store
                .insert_task_without_checkpoint(task.clone())
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "恢复 accepted task {} 失败: {error}",
                        task.task_id
                    ))
                })?;
            restored_accepted_task_count += 1;
        }
        let reconciled_threads = reconcile_terminal_task_execution_threads(
            self.session_store.as_ref(),
            task_store.as_ref(),
        );
        if reconciled_threads > 0 {
            info!(
                reconciled_threads,
                "已将历史终态任务关联的活动线程统一收口为 Idle"
            );
        }
        // 独立代理可能属于已结束、已归档或已被下一轮替换的主线；不能只扫
        // 当前 sidecar。TaskStore 先撤销旧运行态，canonical 再按原任务结算，
        // 写盘失败阻止启动服务，不让旧 Agent 看起来仍在运行或接受新执行。
        let restored_tasks = task_store.all_tasks();
        for session in self.session_store.session_index() {
            self.session_store
                .reconcile_agent_turns_after_restart(&session, &restored_tasks)
                .map_err(|error| {
                    DaemonError::internal(format!("恢复独立 Agent Turn 失败: {error}"))
                })?;
        }
        let stale_task_count = self.reconcile_stale_session_task_chains(task_store.as_ref());
        task_store_requires_checkpoint |= stale_task_count > 0;
        if restored_accepted_task_count > 0 || task_store_requires_checkpoint {
            accepted_submission_repository
                .checkpoint_task_store(task_store.as_ref())
                .map_err(|error| {
                    DaemonError::internal(format!("恢复 accepted task 失败: {error}"))
                })?;
        }
        // LlmTaskDispatcher 的 context 摘要与 prompt 注入共用该 ContextBudget。max_memory ≥ 一批
        // session-memory 的 slice 数（=5），否则辅助模型提取的 5 条 slice 会被预算切断、只投放前两条进 prompt。
        let context_budget = ContextBudget {
            max_turns: 8,
            max_knowledge: 6,
            max_memory: 8,
            max_shared_items: 4,
            max_file_summaries: 4,
        };

        let session_checkpoint_persistence = RuntimeSidecarPersistence::new(
            self.state_repository.clone(),
            self.session_store.clone(),
            self.workspace_store.clone(),
        );
        let session_state_checkpoint_persist = Arc::new(move |checkpoint: &str| {
            session_checkpoint_persistence
                .persist_session_checkpoint()
                .map(|_| true)
                .map_err(|error| {
                    ApiError::internal_assembly(
                        "session turn 关键状态持久化失败",
                        format!("{checkpoint}: {error}"),
                    )
                })
        });

        let mut state = ApiState::new(
            service_name,
            self.event_bus.clone(),
            self.session_store.clone(),
            self.workspace_store.clone(),
            self.governance.clone(),
        )
        .with_daemon_identity(self.daemon_identity.clone())
        .with_managed_process_group(self.managed_process_group.clone())
        .with_knowledge_store(self.knowledge_store.clone())
        .with_settings_store(settings_store)
        .with_appearance_library(appearance_library)
        .with_snapshot_manager(snapshot_manager)
        .with_skill_runtime(app_skill_runtime.clone())
        .with_skill_dispatch_runtime(Arc::new(skill_runtime))
        .with_mcp_connections(mcp_connections)
        .with_tool_registry(tool_registry_for_dispatcher.clone())
        .with_git_context_runtime(
            git_service,
            session_code_contexts,
            workspace_git_coordinator,
        )
        .with_task_execution_registry(task_execution_registry.clone())
        .with_agent_role_registry(agent_role_registry)
        .with_tunnel_port(self.local_port)
        .with_plugin_manager(self.plugins.manager())
        .with_extension_status(self.plugins.extension_status())
        .with_plugin_route_executor(self.plugins.route_executor())
        .with_optional_plugin_toolchain(self.plugins.toolchain())
        .with_runtime_persistence(runtime_persistence)
        .with_workspace_history_restore({
            let repository = self.state_repository.clone();
            let sessions = self.session_store.clone();
            Arc::new(move |project| {
                sessions
                    .restore_workspace_history(project.workspace_id.as_str(), || {
                        let load = || -> Result<_, DaemonError> {
                            repository.migrate_project_storage()?;
                            repository.load_project_session_projections(project)
                        };
                        load().map_err(|error| DomainError::Persistence {
                            message: error.to_string(),
                        })
                    })
                    .map_err(|error| {
                        ApiError::internal_assembly(
                            "恢复项目历史失败，原文件保留；请修复后重试注册",
                            error,
                        )
                    })
            })
        })
        .with_canonical_event_next_sequence_provider({
            let repository = self.state_repository.clone();
            Arc::new(move |session_id| {
                repository
                    .canonical_event_next_sequence(session_id)
                    .map_err(|error| error.to_string())
            })
        })
        .with_session_state_checkpoint_persist(session_state_checkpoint_persist)
        .with_unavailable_session_discard({
            let repository = self.state_repository.clone();
            Arc::new(move |session_id| {
                repository
                    .discard_unavailable_session(session_id)
                    .map_err(|error| ApiError::internal_assembly("删除不可用会话失败", error))
            })
        })
        .with_session_projection_persist({
            let repository = self.state_repository.clone();
            Arc::new(move |durable, sidecars, mode| {
                let result = match mode {
                    SessionProjectionPersistMode::Full => {
                        repository.save_session_projection_state(durable, sidecars)
                    }
                    SessionProjectionPersistMode::Navigation { target_session_id } => repository
                        .save_session_navigation_state(
                            durable,
                            sidecars,
                            target_session_id.as_ref(),
                        ),
                    SessionProjectionPersistMode::Sessions { session_ids } => repository
                        .save_session_projection_state_for_sessions(
                            durable,
                            sidecars,
                            &session_ids,
                        ),
                };
                result.map_err(|error| {
                    ApiError::internal_assembly("session projection 持久化失败", error)
                })
            })
        })
        .with_execution_pipeline(task_store.clone(), memory_store);
        browser_automation_dependencies
            .set(state.browser_tool_runtime_dependencies())
            .map_err(|_| DaemonError::internal("重复装配 Browser 工具运行时依赖"))?;

        state
            .restore_regular_session_turn_queues()
            .map_err(|error| {
                DaemonError::internal(format!("恢复 session turn 排队状态失败: {error:?}"))
            })?;
        state = state
            .with_task_store(Arc::clone(&task_store))
            .with_task_completion_notifier(completion_notifier.clone());
        let restored_turn_coordinator_count = state.restore_turn_coordinator_from_session_store();
        if restored_turn_coordinator_count > 0 {
            tracing::info!(
                restored_turn_coordinator_count,
                "已从 canonical Turn 恢复 SessionTurnCoordinator 身份索引"
            );
        }
        if magi_api::task_turn_finalize::reconcile_terminal_session_task_turns(&state) > 0 {
            let _ = state.persist_session_projection();
        }
        let state_for_task_workers = state.clone();
        let state_for_knowledge_persist = state.clone();
        let knowledge_persist_callback = Arc::new(move || {
            if let Err(error) = state_for_knowledge_persist.persist_knowledge_state() {
                tracing::warn!(?error, "自动知识沉淀持久化失败");
            }
        });
        let session_state_persist_callback =
            magi_api::task_turn_finalize::session_state_persist_callback(&state);
        let llm_task_dispatcher = LlmTaskDispatcher::new(
            self.event_bus.clone(),
            state
                .execution_pipeline()
                .expect("execution pipeline should exist when daemon wires task runner")
                .clone(),
            LlmTaskDispatcherDependencies {
                session_store: state.session_store.clone(),
                execution_registry: state.task_execution_registry().clone(),
                completion_sink: completion_notifier.clone(),
                conversation_registry: state.conversation_registry.clone(),
                agent_role_registry: state.agent_role_registry.clone(),
            },
            self.state_root.clone(),
        );

        let extension_hooks_adapter = Arc::new(super::extension_bridge::ExtensionBusHooks::new(
            self.plugins.extension_bus(),
            tokio::runtime::Handle::current(),
        ));
        let extension_bus_hooks: Arc<
            dyn magi_conversation_runtime::extension_hooks::ExtensionHooks,
        > = extension_hooks_adapter.clone();
        let llm_task_dispatcher = Arc::new(
            llm_task_dispatcher
                .with_plugin_session_message_admitter(magi_api::plugin_session_message_admitter(
                    &state,
                ))
                .with_plugin_command_executor(
                    self.plugins
                        .command_executor(tokio::runtime::Handle::current()),
                )
                .with_model_bridge_client(business_model_client.clone())
                .with_completion_notifier(completion_notifier.clone())
                .with_knowledge_store(state.knowledge_store.clone())
                .with_knowledge_persist_callback(knowledge_persist_callback)
                .with_session_state_persist_callback(session_state_persist_callback)
                .with_settings_store(state.settings_store.clone())
                .with_context_runtime(context_runtime_for_dispatcher)
                .with_context_budget(context_budget)
                .with_workspace_registry(state.workspace_registry.clone())
                .with_git_context_runtime(
                    state.git_service.clone(),
                    state.session_code_contexts.clone(),
                )
                .with_workspace_git_coordinator(state.workspace_git_coordinator.clone())
                .with_tool_registry(tool_registry_for_dispatcher)
                .with_skill_runtime(app_skill_runtime)
                .with_snapshot_manager(state.snapshot_manager.clone())
                .with_workspace_execution_preparer(magi_api::workspace_execution_preparer(&state))
                .with_extension_hooks(extension_bus_hooks),
        );
        self.plugins.bind_native(&llm_task_dispatcher);
        self.plugins.bind_operations(&state);
        self.plugins.start_extensions(self.event_bus.clone());
        let session_turn_dispatcher = llm_task_dispatcher.clone();
        state = state
            .with_input_guard(extension_hooks_adapter)
            .with_session_turn_dispatcher(session_turn_dispatcher)
            .with_model_bridge_client(business_model_client);
        // Runner 只负责调度与资源循环。任务终态由 TaskStore durable status
        // callback 进入 TaskCompletionNotifier，再由 Turn finalizer 收口，避免
        // Runner 只负责调度，Task completion notification 是唯一终态入口。
        let runner_manager = RunnerManager::with_dispatcher_and_worker_catalog(
            Arc::clone(&task_store),
            state.session_store.clone(),
            Arc::new(move || state_for_task_workers.task_worker_catalog()),
            llm_task_dispatcher,
        )
        .with_agent_role_registry(state.agent_role_registry.clone())
        .with_execution_admission(Arc::clone(&execution_admission))
        .with_checkpoint_persist(task_checkpoint_persist);
        let runner_manager = Arc::new(runner_manager);
        state = state.with_shared_runner_manager(runner_manager.clone());
        state.start_plugin_jobs();
        // 回调在 mutation guard 外的普通线程等待原 restart 锁；整个投影/释放
        // 过程结束前不能重开该 scope，也不会持有 TaskStore mutation 锁执行 IO。
        task_store.set_status_change_callback_async(true);
        let status_runner_manager = Arc::downgrade(&runner_manager);
        let eb = event_bus_for_task_store;
        let session_store = session_store_for_task_status;
        let task_execution_registry_for_status = task_execution_registry;
        let completion_notifier_for_status = completion_notifier.clone();
        let task_store_for_status = Arc::downgrade(&task_store);
        task_store.set_status_change_callback(Box::new(
            move |task_id, old_status, new_status, task: magi_core::Task| {
                let Some(current_store) = task_store_for_status.upgrade() else {
                    return;
                };
                let Some(status_runner_manager) = status_runner_manager.upgrade() else {
                    return;
                };
                let _restart = status_runner_manager.lock_for_task_status_change(&task);
                if current_store.get_task(task_id).is_none_or(|current| {
                    current.status != new_status
                        || current.updated_at != task.updated_at
                        || current.retry_count != task.retry_count
                }) {
                    return;
                }
                // TaskStore 在 production 模式下已经把 callback 放到 mutation guard
                // 之外执行。终态事实提交后立即通知 Turn Coordinator，避免计划/事件等
                // 投影工作把 Turn 终态延迟到不可观测的后台窗口；后续任务侧投影仍在
                // 同一回调中继续完成。
                let callback_started_at = Instant::now();
                if matches!(
                    new_status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
                ) {
                    match terminal_task_context(
                        &session_store,
                        &task_execution_registry_for_status,
                        &task,
                    ) {
                        Ok(Some((session_id, turn_id))) => {
                            completion_notifier_for_status.bind_task_context(
                                task_id,
                                Some(session_id),
                                Some(turn_id),
                            );
                        }
                        Ok(None) => {}
                        Err(error) => {
                            tracing::error!(%task_id, %error, "解析 Task 终态的原执行来源失败")
                        }
                    }
                    completion_notifier_for_status
                        .notify_terminal_status(task_id, new_status, &task);
                }
                sync_task_plan_status(eb.as_ref(), session_store.as_ref(), &task, new_status);
                settle_task_execution_threads(session_store.as_ref(), task_id, new_status);
                publish_task_status_changed_event(
                    eb.as_ref(),
                    session_store.as_ref(),
                    task_id,
                    old_status,
                    new_status,
                    &task,
                );
                if let Err(error) = publish_task_status_turn_item_for_active_sessions(
                    &eb,
                    session_store.as_ref(),
                    None,
                    &task,
                    new_status,
                ) {
                    tracing::error!(
                        task_id = %task_id,
                        %error,
                        "任务状态事实写回会话 Turn 失败"
                    );
                }
                if matches!(
                    new_status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
                ) {
                    task_execution_registry_for_status.remove(task_id);
                }
                tracing::info!(
                    target: "magi.performance",
                    task_id = %task_id,
                    stage = "task_status_callback_completed",
                    elapsed_ms = callback_started_at.elapsed().as_millis() as u64,
                    "conversation response timing"
                );
            },
        ));

        // 完成通知器必须捕获已经装配 RunnerManager 的最终 ApiState。若在
        // with_shared_runner_manager 之前注册 observer，队列出队时会使用缺少
        // runner manager 的旧快照，把合法的 queued Turn 错误收口为失败。
        let completion_state = state.clone();
        completion_notifier.set_observer(move |notification| {
            magi_api::task_turn_finalize::handle_task_completion_notification(
                &completion_state,
                notification,
            );
        });

        // MCP 服务必须在工具注册表、审批注册表和会话存储全部装配后再恢复；
        // 它是独立监听入口，不挂到主 HTTP Router，但共享同一个 ApiState 事实源。
        state.start_mcp_service_if_enabled();

        // 把 SnapshotManager 桥接到 session-store 生命周期事件。生产路径必装；
        // 测试可用 ApiState::new 直接构造而不调用此函数，惰性初始化仍可用。
        state.install_snapshot_lifecycle_observer();
        let reconciled_browser_sessions = state
            .reconcile_browser_sessions_with_session_store()
            .map_err(|error| {
                DaemonError::internal(format!("清理孤儿 Browser Session 失败: {error:?}"))
            })?;
        if reconciled_browser_sessions > 0 {
            state
                .persist_browser_durable_state_for_api()
                .map_err(|error| {
                    DaemonError::internal(format!("持久化 Browser Session 清理结果失败: {error:?}"))
                })?;
            tracing::info!(
                count = reconciled_browser_sessions,
                "已清理不再属于现有 Magi 会话的 Browser Session"
            );
        }
        state.reclaim_orphan_session_isolations();
        magi_api::task_turn_finalize::schedule_restored_session_turn_queues(&state);
        magi_api::schedule_restored_session_task_dispatches(state.clone());
        super::browser_host::start_controller(&state, &self.browser_host_controller_lifecycle);

        Ok(state)
    }

    fn reconcile_stale_session_task_chains(&self, task_store: &TaskStore) -> usize {
        let orphan_task_count =
            fail_orphan_session_root_tasks(self.session_store.as_ref(), task_store);
        let interrupted_task_count = self.fail_interrupted_session_task_chains(task_store);
        let reconciled_task_count = orphan_task_count.saturating_add(interrupted_task_count);
        let stale_sidecars = self
            .session_store
            .runtime_sidecars()
            .into_iter()
            .filter(|sidecar| {
                let Some(chain) = sidecar.active_execution_chain.as_ref() else {
                    return false;
                };
                !task_store
                    .get_task(&chain.root_task_id)
                    .is_some_and(|root_task| {
                        root_task.root_task_id == chain.root_task_id
                            && root_task.mission_id == chain.mission_id
                    })
            })
            .collect::<Vec<_>>();

        if stale_sidecars.is_empty() {
            // 即便没有 stale chain，也要扫一遍 chain 缺失但 current_turn 非终态的 sidecar，
            // 防止 daemon 重启后这些孤立轮次让前端误判会话仍在执行。
            self.cancel_orphan_non_terminal_current_turns();
            if reconciled_task_count > 0 {
                self.flush_reconciled_runtime_sidecars(
                    "收敛重启遗留的 session task chain 后持久化 sidecar 失败",
                );
                warn!(
                    orphan_task_count,
                    interrupted_task_count, "已将 daemon 重启遗留的执行中任务收口为可恢复状态"
                );
            }
            return reconciled_task_count;
        }

        let mut stale_count = 0;
        for sidecar in stale_sidecars {
            // 在切断 chain 前，先把仍处于非终态的 current_turn 收敛为 cancelled，
            // 否则 reconcile_terminal_session_task_turns 会因 chain 已为 None 而跳过它，
            // 留下永远停在 "running" 的孤立 canonical turn。
            let session_id = sidecar.session_id.clone();
            if sidecar
                .current_turn
                .as_ref()
                .is_some_and(|turn| !current_turn_status_is_terminal(&turn.status))
                && let Err(error) = CanonicalTurnEventSink::for_store(&self.session_store, None)
                    .cancel_turn(&session_id)
            {
                warn!(
                    ?error,
                    %session_id,
                    "取消失效 session chain 的 current_turn 失败"
                );
                continue;
            }
            let chain = sidecar
                .active_execution_chain
                .as_ref()
                .expect("stale chain was selected");
            match self.session_store.archive_active_execution_chain(
                &session_id,
                &chain.root_task_id,
                chain.current_turn_id.as_deref(),
            ) {
                Ok(()) => stale_count += 1,
                Err(error) => warn!(?error, %session_id, "归档失效 session chain 失败"),
            }
        }

        // chain 已经清理完，再统一处理 chain 缺失但 current_turn 仍非终态的 sidecar。
        self.cancel_orphan_non_terminal_current_turns();

        self.flush_reconciled_runtime_sidecars("清理失效 session task chain 后持久化 sidecar 失败");
        warn!(
            stale_count,
            orphan_task_count,
            interrupted_task_count,
            "已清理指向缺失 root task 的 session task chain"
        );
        reconciled_task_count
    }

    fn fail_interrupted_session_task_chains(&self, task_store: &TaskStore) -> usize {
        let interrupted_chains =
            self.session_store
                .runtime_sidecars()
                .into_iter()
                .filter_map(|sidecar| {
                    let chain = sidecar.active_execution_chain?;
                    let root_task = task_store.get_task(&chain.root_task_id)?;
                    if matches!(root_task.status, TaskStatus::Completed | TaskStatus::Killed) {
                        return None;
                    }
                    // 准备阶段在 canonical 投影中为 accepted/running；root Task
                    // 仍为 Pending 才是尚未开始执行的准备阶段。
                    // 这类 Turn 尚未向 provider 发起不可安全重放的请求，保留 chain，
                    // 待 ApiState 装配完成后由 preparation worker 恢复。
                    let recoverable_preparation = root_task.status == TaskStatus::Pending
                        && sidecar.current_turn.as_ref().is_some_and(|turn| {
                            matches!(turn.status.as_str(), "accepted" | "running")
                        });
                    if recoverable_preparation {
                        return None;
                    }
                    let turn_is_active = sidecar
                        .current_turn
                        .as_ref()
                        .is_some_and(|turn| !current_turn_status_is_terminal(&turn.status));
                    let has_in_memory_task = task_store
                        .collect_subtree_ids(&chain.root_task_id)
                        .into_iter()
                        .any(|task_id| {
                            task_store.get_task(&task_id).is_some_and(|task| {
                                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                            })
                        });
                    (turn_is_active || has_in_memory_task)
                        .then_some((sidecar.session_id, chain.root_task_id))
                })
                .collect::<HashSet<_>>();

        let mut failed_count = 0usize;
        let mut interrupted_turn_count = 0usize;
        for (session_id, root_task_id) in interrupted_chains {
            for task_id in task_store.collect_subtree_ids(&root_task_id) {
                let Some(task) = task_store.get_task(&task_id) else {
                    continue;
                };
                if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                    let lease_id = task_store
                        .get_active_lease(&task_id)
                        .map(|lease| lease.lease_id);
                    match task_store.revoke_lease_and_set_task_terminal(
                        &task_id,
                        &root_task_id,
                        lease_id.as_ref(),
                        TaskStatus::Failed,
                        vec!["daemon 重启时检测到执行链中断，任务已终止".to_string()],
                    ) {
                        Ok(true) => failed_count += 1,
                        Ok(false) => warn!(
                            %task_id,
                            %root_task_id,
                            "忽略未生效的 daemon 重启任务终态收敛"
                        ),
                        Err(error) => warn!(
                            ?error,
                            %task_id,
                            %root_task_id,
                            "daemon 重启任务终态收敛失败"
                        ),
                    }
                }
            }
            match CanonicalTurnEventSink::for_store(&self.session_store, None)
                .interrupt_turn_by_daemon_restart(&session_id)
            {
                Ok(Some(_)) => interrupted_turn_count += 1,
                Ok(None) => {}
                Err(error) => warn!(
                    ?error,
                    %session_id,
                    %root_task_id,
                    "收敛 daemon 重启中断轮次失败"
                ),
            }
        }
        failed_count.saturating_add(interrupted_turn_count)
    }

    fn flush_reconciled_runtime_sidecars(&self, warning: &'static str) {
        let repository = self.state_repository.clone();
        let persistence = RuntimeSidecarPersistence::new(
            repository,
            self.session_store.clone(),
            self.workspace_store.clone(),
        );
        if let Err(error) = persistence.flush_runtime_sidecars() {
            warn!(?error, warning);
        }
    }

    /// daemon 重启后兜底清理：任何 sidecar 若失去 active_execution_chain 但
    /// current_turn 仍处于非终态，说明上次进程崩溃时这条轮次没被收敛掉。
    /// 这里统一标记为 cancelled，让 canonical 投影回归终态，避免前端订阅到
    /// "假在跑"的会话状态。
    fn cancel_orphan_non_terminal_current_turns(&self) {
        let orphan_sessions: Vec<SessionId> = self
            .session_store
            .runtime_sidecars()
            .into_iter()
            .filter_map(|sidecar| {
                if sidecar.active_execution_chain.is_some() {
                    return None;
                }
                let turn = sidecar.current_turn.as_ref()?;
                if current_turn_status_is_terminal(&turn.status) {
                    return None;
                }
                Some(sidecar.session_id.clone())
            })
            .collect();

        if orphan_sessions.is_empty() {
            return;
        }

        let orphan_count = orphan_sessions.len();
        for session_id in orphan_sessions {
            if let Err(error) = CanonicalTurnEventSink::for_store(&self.session_store, None)
                .cancel_turn(&session_id)
            {
                warn!(
                    ?error,
                    %session_id,
                    "取消孤立非终态 current_turn 失败"
                );
            }
        }
        warn!(
            orphan_count,
            "已清理 daemon 重启遗留的孤立非终态 current_turn"
        );
    }

    pub(crate) fn router(&self, service_name: String) -> Result<axum::Router, DaemonError> {
        Ok(build_router(self.build_api_state(service_name)?))
    }

    #[cfg(test)]
    pub(crate) fn router_with_state_for_tests(
        &self,
        service_name: String,
    ) -> (axum::Router, ApiState) {
        let state = self
            .build_api_state_with_options(
                service_name,
                &[],
                Some(Arc::new(StaticTestModelBridgeClient)),
            )
            .expect("测试 API 状态应成功构造");
        (build_router(state.clone()), state)
    }

    #[cfg(test)]
    pub(crate) fn router_with_bridge_env_for_tests(
        &self,
        service_name: String,
        bridge_env: &[(&str, &str)],
    ) -> (axum::Router, ApiState) {
        let state = self
            .build_api_state_with_options(service_name, bridge_env, None)
            .expect("测试 API 状态应成功构造");
        (build_router(state.clone()), state)
    }

    fn restore_ledger(
        state_repository: &StateRepository,
        event_bus: &Arc<InMemoryEventBus>,
    ) -> Result<(), DaemonError> {
        let audit_usage_ledger = state_repository.load_audit_usage_ledger()?;
        let ledger_path = state_repository.audit_usage_ledger_path();
        event_bus.restore_persisted_audit_usage_ledger(audit_usage_ledger);
        event_bus.set_audit_usage_ledger_persistence(ledger_path.clone());
        if !ledger_path.exists()
            && let Err(error) = event_bus.refresh_audit_usage_ledger_persistence()
        {
            warn!(error = %error, "审计/用量账本初始落盘失败，后续事件仍会继续运行");
        }
        publish_ledger_status_event(event_bus, "system-ledger-ready", "system.ledger.ready");
        Ok(())
    }

    /// 恢复只加载已经提交的状态；启动阶段不得因重建内存缓存而重写全部 session。
    fn persist_restored_runtime_state(
        state_repository: &StateRepository,
        runtime_persistence: &RuntimeSidecarPersistence,
        workspace_store: &Arc<WorkspaceStore>,
    ) -> Result<(), DaemonError> {
        state_repository.save_workspace_durable_state(&workspace_store.durable_state())?;
        runtime_persistence.flush_runtime_sidecars()?;
        Ok(())
    }

    /// 仅用于测试 fixture：fixture 在内存中创建了全新的 session，必须显式建立 projection。
    #[cfg(test)]
    fn persist_initial_runtime_state(
        state_repository: &StateRepository,
        runtime_persistence: &RuntimeSidecarPersistence,
        workspace_store: &Arc<WorkspaceStore>,
    ) -> Result<(), DaemonError> {
        runtime_persistence.persist_session_checkpoint()?;
        state_repository.save_workspace_durable_state(&workspace_store.durable_state())?;
        runtime_persistence.flush_runtime_sidecars()?;
        Ok(())
    }

    /// Build an `HttpModelBridgeClient` from configuration.
    ///
    /// 业务模型配置来源（仅这两类，按优先级）：
    /// 1. `bridge_env` overrides（测试场景注入）
    /// 2. 进程级 env（`MAGI_OPENAI_COMPAT_*`）
    ///
    /// 不读取 `settings.json` 的 `auxiliary` 段 —— aux 段是辅助模型专用配置，
    /// 业务模型与辅助模型混读同一份字段会造成"改 aux 设置静默切换业务模型"的
    /// 配置错位。业务模型未配置时返回 `None`，调用方应据此走 unavailable-client 提示。
    ///
    fn try_build_http_model_client(bridge_env: &[(&str, &str)]) -> Option<HttpModelBridgeClient> {
        let config = Self::openai_compat_env_config(bridge_env)?;
        Some(HttpModelBridgeClient::new_with_protocol(
            config.base_url,
            config.api_key,
            config.model,
            HttpModelBridgeProtocol::ChatCompletions,
            None,
        ))
    }

    fn seed_orchestrator_settings_from_env_if_empty(
        settings_store: &Arc<SettingsStore>,
        bridge_env: &[(&str, &str)],
    ) -> Result<(), std::io::Error> {
        if orchestrator_settings_is_configured(&settings_store.get_section("orchestrator")) {
            return Ok(());
        }
        let Some(config) = Self::openai_compat_env_config(bridge_env) else {
            return Ok(());
        };
        let section = serde_json::json!({"revision":0,"sources":[{
            "id":"environment", "name":"Environment", "baseUrl":config.base_url,
            "apiKey":config.api_key.unwrap_or_default(), "apiProtocol":"openai_chat",
            "models":[{"id":config.model}],
        }]});
        settings_store.set_section("orchestrator", section)?;
        settings_store.set_section(magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            serde_json::json!({"sourceId":"custom/environment","model":config.model,"reasoningEffort":null}))
    }

    fn openai_compat_env_config(bridge_env: &[(&str, &str)]) -> Option<OpenAiCompatEnvConfig> {
        let find_env = |key: &str| -> Option<String> {
            bridge_env
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    env::var(key)
                        .ok()
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                })
        };

        Some(OpenAiCompatEnvConfig {
            base_url: find_env("MAGI_OPENAI_COMPAT_BASE_URL")?,
            api_key: find_env("MAGI_OPENAI_COMPAT_API_KEY"),
            model: find_env("MAGI_OPENAI_COMPAT_MODEL").unwrap_or_else(|| "gpt-4".to_string()),
        })
    }
}

fn fail_orphan_session_root_tasks(session_store: &SessionStore, task_store: &TaskStore) -> usize {
    let active_root_task_ids = session_store
        .runtime_sidecars()
        .into_iter()
        .filter_map(|sidecar| {
            sidecar
                .active_execution_chain
                .map(|chain| chain.root_task_id)
        })
        .collect::<HashSet<_>>();
    let session_task_ids = session_store
        .session_index()
        .into_iter()
        .flat_map(|session_id| session_store.execution_task_ids_for_session(&session_id))
        .collect::<HashSet<_>>();
    let orphan_task_ids = task_store
        .all_tasks()
        .into_iter()
        .filter(|task| {
            task.parent_task_id.is_none()
                && task.root_task_id == task.task_id
                && matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                && session_task_ids.contains(&task.task_id)
                && !active_root_task_ids.contains(&task.task_id)
        })
        .map(|task| task.task_id)
        .collect::<Vec<_>>();

    let mut failed_count = 0usize;
    for task_id in orphan_task_ids {
        let lease_id = task_store
            .get_active_lease(&task_id)
            .map(|lease| lease.lease_id);
        match task_store.revoke_lease_and_set_task_terminal(
            &task_id,
            &task_id,
            lease_id.as_ref(),
            TaskStatus::Failed,
            vec!["daemon 重启时检测到任务未绑定 execution chain，已终止孤立执行状态".to_string()],
        ) {
            Ok(true) => failed_count += 1,
            Ok(false) => warn!(%task_id, "忽略未生效的孤立任务终态收敛"),
            Err(error) => warn!(?error, %task_id, "孤立任务终态收敛失败"),
        }
    }
    failed_count
}

fn settle_task_execution_threads(
    session_store: &SessionStore,
    task_id: &magi_core::TaskId,
    new_status: TaskStatus,
) {
    if matches!(
        new_status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    ) {
        session_store.mark_task_threads_idle(task_id, UtcMillis::now());
    }
}

fn terminal_task_context(
    session_store: &SessionStore,
    registry: &TaskExecutionRegistry,
    task: &magi_core::Task,
) -> Result<Option<(SessionId, String)>, String> {
    // 独立 Agent 的 canonical 绑定优先于进程内计划与 Session 当前指针。
    // 后两者可能仍指向原父轮；状态回调先到时也不能把 Agent 终态发送给主线。
    if let Some(activation) = task.agent_activation()
        && let Some(turn) = session_store
            .agent_turn_for_task(&activation.session_id, &task.task_id)
            .map_err(|error| error.to_string())?
    {
        return Ok(Some((activation.session_id.clone(), turn.turn_id)));
    }
    if let Some(plan) = registry.get(&task.task_id) {
        let magi_conversation_runtime::task_execution_registry::TaskExecutionPlan::Dispatch {
            session_id,
            turn_id,
            ..
        } = plan;
        return Ok(Some((session_id, turn_id)));
    }
    // kill、租约过期及排队任务可能没有执行计划。普通 Task 才从原执行链补齐关联。
    Ok(session_store
        .active_execution_sidecars_for_task(&task.task_id, &task.root_task_id)
        .into_iter()
        .find_map(|sidecar| {
            sidecar
                .current_turn
                .map(|turn| (sidecar.session_id, turn.turn_id))
        }))
}

fn reconcile_terminal_task_execution_threads(
    session_store: &SessionStore,
    task_store: &TaskStore,
) -> usize {
    let now = UtcMillis::now();
    task_store
        .all_tasks()
        .into_iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
            )
        })
        .map(|task| session_store.mark_task_threads_idle(&task.task_id, now))
        .sum()
}

fn publish_task_status_changed_event(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_id: &magi_core::TaskId,
    old_status: TaskStatus,
    new_status: TaskStatus,
    task: &magi_core::Task,
) {
    let scoped = task_status_event_scope(session_store, task);
    let session_id = scoped.as_ref().map(|(session_id, _)| session_id.clone());
    let workspace_id = scoped.and_then(|(_, workspace_id)| workspace_id);
    let failure_detail = (new_status == TaskStatus::Failed)
        .then(|| {
            task.output_refs
                .iter()
                .find(|value| !value.trim().is_empty())
                .map(|value| public_runtime_excerpt(value, 4096))
        })
        .flatten();
    let event = EventEnvelope::domain(
        EventId::unique(format!("event-task-status-changed-{}", task_id)),
        magi_event_bus::task_events::TASK_STATUS_CHANGED,
        serde_json::json!({
            "task_id": task_id.to_string(),
            "root_task_id": task.root_task_id.to_string(),
            "mission_id": task.mission_id.to_string(),
            "title": task.title.as_str(),
            "old_status": format!("{:?}", old_status),
            "new_status": format!("{:?}", new_status),
            "kind": format!("{:?}", task.kind),
            "failure_stage": (new_status == TaskStatus::Failed).then_some("task_execution"),
            "failure_detail": failure_detail,
            "session_id": session_id.as_ref().map(ToString::to_string),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        }),
    )
    .with_context(EventContext {
        workspace_id,
        session_id,
        mission_id: Some(task.mission_id.clone()),
        task_id: Some(task_id.clone()),
        ..EventContext::default()
    });
    let _ = event_bus.publish(event);
}

fn sync_task_plan_status(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task: &magi_core::Task,
    status: TaskStatus,
) {
    let Some((session_id, workspace_id)) = task_status_event_scope(session_store, task) else {
        tracing::warn!(
            task_id = %task.task_id,
            canonical_task_name = task.canonical_task_name().unwrap_or(""),
            "计划绑定任务状态变化时无法解析 session 作用域"
        );
        return;
    };
    let plan_store = magi_plan::PlanStore::from_store(session_store, session_id);
    let authority = if task.parent_task_id.is_none() {
        magi_plan::PlanTaskAuthority::PhaseOwner
    } else {
        magi_plan::PlanTaskAuthority::Supporting
    };
    match plan_store.sync_task_status_with_authority(&task.task_id, status, authority) {
        Ok(Some(plan)) => magi_plan::publish_plan_event(
            event_bus,
            magi_plan::plan_event_type(&plan),
            &plan,
            workspace_id.as_ref(),
            Some(&task.task_id),
            Some(&task.mission_id),
        ),
        Ok(None) => {}
        Err(error) => tracing::error!(
            task_id = %task.task_id,
            error = %error,
            "同步代理任务状态到用户计划失败"
        ),
    }
}

fn task_status_event_scope(
    session_store: &SessionStore,
    task: &magi_core::Task,
) -> Option<(SessionId, Option<magi_core::WorkspaceId>)> {
    if let Some(activation) = task.agent_activation()
        && let Some(session) = session_store.session(&activation.session_id)
    {
        return Some((
            session.session_id,
            session.workspace_id.map(magi_core::WorkspaceId::new),
        ));
    }
    session_store
        .active_execution_sidecars_for_task(&task.task_id, &task.root_task_id)
        .into_iter()
        .next()
        .map(|sidecar| {
            let workspace_id = sidecar
                .active_execution_chain
                .as_ref()
                .and_then(|chain| chain.workspace_id.clone())
                .or_else(|| sidecar.ownership.workspace_id.clone());
            (sidecar.session_id, workspace_id)
        })
}

#[cfg(test)]
mod tests;
