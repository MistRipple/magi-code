use super::{
    bootstrap::bootstrap_state,
    config::{DaemonConfig, DaemonError},
    events::publish_ledger_status_event,
    maintenance::{RuntimeMaintenance, RuntimeMaintenanceConfig},
    mission_recovery,
    persistence::{RuntimeSidecarPersistence, StateRepository},
};
use magi_api::{
    ApiError, ApiState, DirectHttpModelProbeConfig, RunnerManager, RuntimeStatePersistence,
    SettingsStore, build_router, build_runtime_capability_dependency_provider,
    mcp_config::{build_mcp_config_from_entry, mcp_server_entry_enabled, mcp_server_entry_id},
};
use magi_bridge_client::{
    BridgeBindingKind, BridgeClientError, BridgeDispatchRuntime, BridgeResponse, BridgeServerKind,
    BridgeTransport, HttpModelBridgeClient, HttpModelBridgeProtocol, JsonRpcMcpBridgeClient,
    JsonRpcModelBridgeClient, JsonRpcStdioTransport, McpBridgeClient, McpToolCallRequest,
    StdioMcpBridgeClient,
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, FileSummaryStore, ProjectRecentTurnStore, SharedContextPool,
};
use magi_conversation_runtime::{
    session_turn_finalize::{
        current_turn_status_is_terminal, publish_task_status_turn_item_for_active_sessions,
    },
    task_execution_dispatcher::LlmTaskDispatcher,
    task_execution_registry::TaskExecutionPlan,
    task_runner_bridge::{
        EventBasedResultReceiver, TaskDispatchGateDecision, TaskOutcome, TaskResult,
    },
};
use magi_core::{
    EventId, ExecutionOwnership, LeaseId, SessionId, TaskStatus, TaskTier, UtcMillis,
    WorkspaceRootPath,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_lifecycle_notice::{LifecycleNoticeRegistry, run_subscriber as run_lifecycle_subscriber};
use magi_memory_store::MemoryStore;
use magi_mission_metrics::MissionMetricsRegistry;
use magi_orchestrator::{ExecutionContextConfig, OrchestratorService, task_store::TaskStore};
use magi_session_store::{SessionExecutionSidecarStatus, SessionRuntimeSidecar, SessionStore};
use magi_skill_runtime::SkillDispatchRuntime;
use magi_snapshot::SnapshotManager;
use magi_tool_runtime::{
    AgentRoleCatalogEntry, AgentRoleCatalogProvider, ExternalMcpServerCatalogEntry,
    ExternalToolCatalogEntry, ExternalToolCatalogProvider, ExternalToolCatalogSnapshot,
    ToolRegistry,
};
use magi_worker_runtime::WorkerRuntime;
use magi_workspace::WorkspaceStore;
use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tracing::warn;

#[cfg(test)]
struct StaticTestModelBridgeClient;

fn build_external_tool_catalog_provider(
    settings_store: Arc<SettingsStore>,
    skill_runtime: Arc<magi_skill_runtime::SkillRuntime>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
) -> ExternalToolCatalogProvider {
    Arc::new(move || {
        let mut skill_tools = Vec::new();
        for skill in skill_runtime.registry().list() {
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

        let settings_snapshot = settings_store.public_snapshot();
        let mcp_servers = settings_snapshot
            .get("mcpServers")
            .and_then(serde_json::Value::as_array)
            .map(|servers| {
                servers
                    .iter()
                    .filter_map(|entry| external_mcp_server_catalog_entry(entry, &mcp_connections))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        ExternalToolCatalogSnapshot {
            skill_tools,
            mcp_servers,
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
) -> AgentRoleCatalogProvider {
    Arc::new(move || {
        let mut roles = registry
            .all()
            .map(|role| {
                let spawnable = registry.is_spawnable_agent_role(&role.id);
                let status = if spawnable {
                    "spawnable"
                } else if role.coordinator_mode {
                    "coordinator_only"
                } else {
                    "unsupported"
                };
                AgentRoleCatalogEntry {
                    role_id: role.id.clone(),
                    spawnable,
                    coordinator_mode: role.coordinator_mode,
                    supported_kinds: role
                        .supported_task_kinds()
                        .into_iter()
                        .map(task_kind_label)
                        .collect(),
                    parallelism_limit: role.parallelism_limit,
                    status: status.to_string(),
                }
            })
            .collect::<Vec<_>>();
        roles.sort_by(|left, right| left.role_id.cmp(&right.role_id));
        roles
    })
}

fn task_kind_label(kind: magi_core::TaskKind) -> String {
    match kind {
        magi_core::TaskKind::LocalAgent => "local_agent",
        magi_core::TaskKind::LocalWorkflow => "local_workflow",
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
            ("restricted_requires_approval", "high", "required")
        }
        magi_bridge_client::BridgeBindingKind::Model => ("access_profile_inherited", "low", "none"),
    }
}

fn external_mcp_server_catalog_entry(
    entry: &serde_json::Value,
    mcp_connections: &Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
) -> Option<ExternalMcpServerCatalogEntry> {
    let server_id = read_json_string(entry, &["id", "serverId", "name"])?;
    let name =
        read_json_string(entry, &["name", "serverName"]).unwrap_or_else(|| server_id.clone());
    let enabled = entry
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let connected_client = {
        let pool = mcp_connections
            .read()
            .expect("mcp connections read lock poisoned");
        pool.get(&server_id).cloned()
    };
    let connected = enabled && connected_client.is_some();
    let tool_count = if enabled {
        entry
            .get("toolCount")
            .or_else(|| entry.get("tool_count"))
            .and_then(serde_json::Value::as_u64)
            .map(|count| count as usize)
    } else {
        None
    };
    let health = if !enabled {
        "disabled"
    } else if connected {
        "connected"
    } else {
        "disconnected"
    };
    Some(ExternalMcpServerCatalogEntry {
        server_id,
        name,
        enabled,
        connected,
        health: health.to_string(),
        tool_count,
        error: if enabled {
            external_mcp_error_marker(entry)
        } else {
            None
        },
    })
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
    mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
    default_client: Arc<dyn McpBridgeClient>,
}

impl SettingsBackedMcpBridgeClient {
    fn new(
        settings_store: Arc<SettingsStore>,
        mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
        default_client: Arc<dyn McpBridgeClient>,
    ) -> Self {
        Self {
            settings_store,
            mcp_connections,
            default_client,
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

    fn client_for_entry(
        &self,
        server_id: &str,
        entry: &serde_json::Value,
    ) -> Result<Arc<StdioMcpBridgeClient>, BridgeClientError> {
        if !mcp_server_entry_enabled(entry) {
            self.remove_connection(server_id);
            return Err(mcp_config_unavailable_error(format!(
                "MCP server {server_id} is disabled"
            )));
        }
        if let Some(client) = self.connected_client(server_id) {
            return Ok(client);
        }

        let config = build_mcp_config_from_entry(entry).ok_or_else(|| {
            mcp_config_unavailable_error(format!("MCP server {server_id} config is incomplete"))
        })?;
        let client = Arc::new(StdioMcpBridgeClient::new(config));
        {
            let mut pool = self
                .mcp_connections
                .write()
                .expect("mcp connections write lock poisoned");
            pool.insert(server_id.to_string(), client.clone());
        }
        Ok(client)
    }

    fn connected_client(&self, server_id: &str) -> Option<Arc<StdioMcpBridgeClient>> {
        let pool = self
            .mcp_connections
            .read()
            .expect("mcp connections read lock poisoned");
        pool.get(server_id).cloned()
    }

    fn remove_connection(&self, server_id: &str) {
        let mut pool = self
            .mcp_connections
            .write()
            .expect("mcp connections write lock poisoned");
        pool.remove(server_id);
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
            return self.default_client.call_tool(request);
        };
        let server_id = mcp_server_entry_id(&entry)
            .map(str::to_string)
            .unwrap_or_else(|| target.to_string());
        let client = self.client_for_entry(&server_id, &entry)?;
        client.call_tool(request)
    }
}

fn mcp_entry_matches_target(entry: &serde_json::Value, target: &str) -> bool {
    ["id", "serverId", "name", "serverName"]
        .iter()
        .any(|field| {
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
                "业务模型桥未配置：请在设置面板「模型 · 主对话/编排模型」中填入 baseUrl / apiKey / model，\
                 或退回到环境变量 MAGI_OPENAI_COMPAT_BASE_URL / MAGI_OPENAI_COMPAT_API_KEY / MAGI_OPENAI_COMPAT_MODEL 作为兜底。\
                 settings.json 的 auxiliary 段仅用于辅助模型（会话标题精修 / 知识抽取 / 会话记忆 / Prompt 增强），不参与业务派发。"
                    .to_string(),
        }
    }
}

impl magi_bridge_client::ModelBridgeClient for UnavailableBusinessModelBridgeClient {
    fn invoke(
        &self,
        _request: magi_bridge_client::ModelInvocationRequest,
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        Err(self.error())
    }

    fn invoke_streaming(
        &self,
        _request: magi_bridge_client::ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        Err(self.error())
    }
}

#[derive(Clone)]
struct SettingsBackedBusinessModelBridgeClient {
    state_root: PathBuf,
    bridge_env: Vec<(String, String)>,
}

struct OpenAiCompatEnvConfig {
    base_url: String,
    api_key: Option<String>,
    model: String,
}

fn orchestrator_settings_is_configured(value: &serde_json::Value) -> bool {
    let field_is_present = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
    };
    field_is_present("baseUrl") && field_is_present("model")
}

impl SettingsBackedBusinessModelBridgeClient {
    fn new(state_root: PathBuf, bridge_env: &[(&str, &str)]) -> Self {
        Self {
            state_root,
            bridge_env: bridge_env
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn build_client(&self) -> Result<HttpModelBridgeClient, magi_bridge_client::BridgeClientError> {
        let bridge_env = self
            .bridge_env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        DaemonRuntime::try_build_http_model_client(&bridge_env)
            .map(|(client, _)| client)
            .ok_or_else(|| {
                UnavailableBusinessModelBridgeClient::new(self.state_root.clone()).error()
            })
    }
}

impl magi_bridge_client::ModelBridgeClient for SettingsBackedBusinessModelBridgeClient {
    fn invoke(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        self.build_client()?.invoke(request)
    }

    fn invoke_streaming(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
        on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        self.build_client()?.invoke_streaming(request, on_delta)
    }
}

#[cfg(test)]
impl magi_bridge_client::ModelBridgeClient for StaticTestModelBridgeClient {
    fn invoke(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        if let Some(payload) = classifier_payload_for_prompt(&request.prompt) {
            return Ok(magi_bridge_client::BridgeResponse { ok: true, payload });
        }
        if request.prompt.contains("任务投影规划器") {
            return Ok(magi_bridge_client::BridgeResponse {
                ok: true,
                payload: serde_json::json!({
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
            });
        }
        Ok(magi_bridge_client::BridgeResponse {
            ok: true,
            payload: format!("loopback-model::{}", request.prompt.trim()),
        })
    }

    fn invoke_streaming(
        &self,
        request: magi_bridge_client::ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError> {
        self.invoke(request)
    }
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
    let task_tier = if route == "task"
        && (user_text.contains("复杂任务")
            || user_text.contains("深度任务")
            || user_text.contains("长期任务")
            || user_text.contains("跨多轮")
            || user_text.contains("多阶段")
            || user_text.contains("可恢复")
            || user_text.contains("人审")
            || user_text.contains("审计"))
    {
        "long_mission"
    } else {
        "execution_chain"
    };
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
    local_port: u16,
    event_bus: Arc<InMemoryEventBus>,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    knowledge_store: Arc<KnowledgeStore>,
    governance: Arc<GovernanceService>,
    worker_runtime: WorkerRuntime,
    runtime_maintenance: RuntimeMaintenance,
}

impl DaemonRuntime {
    pub(crate) fn restore(config: &DaemonConfig) -> Result<Self, DaemonError> {
        let state_repository = StateRepository::new(config.state_root.clone());

        // 先加载工作区注册表（需要工作区路径来定位会话文件）
        let workspace_store = Arc::new(WorkspaceStore::from_persisted_parts(
            state_repository.load_workspace_durable_state()?,
            state_repository.load_workspace_recovery_sidecars()?,
        ));

        // 收集所有工作区的 (workspace_id, root_path)
        let workspace_roots: Vec<(String, std::path::PathBuf)> = workspace_store
            .workspaces()
            .into_iter()
            .map(|w| {
                (
                    w.workspace_id.to_string(),
                    std::path::PathBuf::from(w.root_path.as_str()),
                )
            })
            .collect();

        // 从各工作区 .magi/sessions.json 加载会话（含迁移旧全局 sessions.json）
        let session_durable = state_repository.load_sessions_from_workspaces(&workspace_roots)?;
        let session_store = Arc::new(SessionStore::from_persisted_parts(
            session_durable,
            state_repository.load_session_sidecars()?,
        ));
        let knowledge_store = Arc::new(KnowledgeStore::from_state(
            state_repository.load_knowledge_state()?,
        ));
        let event_bus = Arc::new(InMemoryEventBus::new(2048));
        let worker_runtime = WorkerRuntime::new(event_bus.clone());
        worker_runtime.restore_durable_snapshot(state_repository.load_worker_runtime_snapshot()?);
        let runtime_persistence = RuntimeSidecarPersistence::new(
            state_repository.clone(),
            session_store.clone(),
            workspace_store.clone(),
            worker_runtime.clone(),
        );

        Self::restore_ledger(&state_repository, &event_bus)?;
        Self::bootstrap_runtime_state(
            config,
            &state_repository,
            &runtime_persistence,
            &session_store,
            &workspace_store,
        )?;
        // 任务系统 §1.4 Phase B：bootstrap 完成后扫描所有 workspace 的可恢复
        // Mission，把 Checkpoint 里的 recovery_ref 回灌到对应 session sidecar，并发布
        // `mission.resumed.from_recovery` 事件。单个 mission 失败不影响其它恢复。
        mission_recovery::recover_missions_at_bootstrap(
            &config.state_root,
            &session_store,
            &workspace_store,
            &event_bus,
        );

        // 引导阶段：代码索引与 workspace 绑定。进程内检索引擎不持久化，
        // daemon 每次启动都必须为所有已注册 workspace 重建，而不是只恢复 active workspace。
        let mut synced_code_index = false;
        for workspace in workspace_store.workspaces() {
            let scan_root = PathBuf::from(workspace.root_path.as_str());
            knowledge_store.build_workspace_index(&workspace.workspace_id, &scan_root);
            synced_code_index = true;
        }
        if synced_code_index {
            let _ = state_repository.save_knowledge_state(&knowledge_store.export_state());
        }

        let runtime_maintenance = RuntimeMaintenance::new(
            RuntimeMaintenanceConfig::default(),
            event_bus.clone(),
            runtime_persistence,
            session_store.clone(),
            workspace_store.clone(),
        );
        runtime_maintenance.publish_runtime_status_event("system-runtime-maintenance-ready");

        Ok(Self {
            state_root: config.state_root.clone(),
            local_port: config.port,
            event_bus,
            session_store,
            workspace_store,
            knowledge_store,
            governance: Arc::new(GovernanceService::default()),
            worker_runtime,
            runtime_maintenance,
        })
    }

    pub(crate) fn start_background_tasks(&self) {
        let runtime_maintenance = self.runtime_maintenance.clone();
        tokio::spawn(async move {
            runtime_maintenance.run_loop().await;
        });
    }

    pub(crate) fn publish_started_event(&self, service_name: &str) {
        let _ = self.event_bus.publish(EventEnvelope::system(
            EventId::new("system-started"),
            "system.started",
            serde_json::json!({
                "service": service_name,
                "mode": "local-loopback"
            }),
        ));
    }

    fn build_api_state(&self, service_name: String) -> ApiState {
        self.build_api_state_with_options(service_name, &[], None)
    }

    fn build_api_state_with_options(
        &self,
        service_name: String,
        bridge_env: &[(&str, &str)],
        model_bridge_override: Option<Arc<dyn magi_bridge_client::ModelBridgeClient>>,
    ) -> ApiState {
        let orchestrator = OrchestratorService::new(self.event_bus.clone());
        let mcp_connections = Arc::new(RwLock::new(HashMap::new()));
        let model_transport =
            Self::bridge_loopback_transport_with_env("model_bridge_loopback", bridge_env);
        let mcp_transport =
            Self::bridge_loopback_transport_with_env("mcp_bridge_loopback", bridge_env);

        // 创建带持久化路径的设置存储，并从磁盘恢复已有设置
        let settings_store = Arc::new(SettingsStore::with_persistence_path(
            self.state_root.join("settings.json"),
        ));
        if let Err(error) = settings_store.load_from_disk() {
            warn!(error = %error, "设置文件加载失败，使用空默认值");
        }
        Self::seed_orchestrator_settings_from_env_if_empty(&settings_store, bridge_env);
        let agent_role_registry = Arc::new(magi_agent_role::AgentRoleRegistry::load_default());
        let app_skill_runtime = Arc::new(
            magi_api::skill_loader::build_skill_runtime_from_settings(&settings_store),
        );
        let external_tool_catalog_provider = build_external_tool_catalog_provider(
            settings_store.clone(),
            app_skill_runtime.clone(),
            mcp_connections.clone(),
        );
        let agent_role_catalog_provider =
            build_agent_role_catalog_provider(agent_role_registry.clone());
        let snapshot_manager = Arc::new(SnapshotManager::new());
        let memory_store = MemoryStore::new();
        let context_runtime = ContextRuntime::with_runtime_sources(
            (*self.knowledge_store).clone(),
            memory_store.clone(),
            (*self.session_store).clone(),
            SharedContextPool::default(),
            FileSummaryStore::default(),
            ProjectRecentTurnStore::default(),
        );
        let context_runtime_for_dispatcher = Arc::new(context_runtime.clone());
        let runtime_capability_dependency_provider = build_runtime_capability_dependency_provider(
            snapshot_manager.clone(),
            self.workspace_store.clone(),
            true,
        );
        let mut tool_registry = ToolRegistry::new(self.governance.clone(), self.event_bus.clone())
            .with_knowledge_store(self.knowledge_store.clone())
            .with_external_tool_catalog_provider(external_tool_catalog_provider)
            .with_agent_role_catalog_provider(agent_role_catalog_provider)
            .with_runtime_capability_dependency_provider(runtime_capability_dependency_provider);
        tool_registry.register_default_builtins();

        // 业务模型桥用于会话正文生成和任务执行；任务规划/分类另走本地 loopback-model。
        //
        // 单一事实源（按优先级，由 task_execution_dispatcher::resolve_target_for_role
        // 的 RoleTarget::Orchestrator 分支串联）：
        //   1. 测试场景 model_bridge_override 注入的 stub
        //   2. settings.json 的 `orchestrator` 段（前端「主对话/编排模型」表单写入位置，
        //      携带 reasoningEffort / urlMode 全套字段，是业务模型的权威入口）
        //   3. 此处 daemon bootstrap 注入的 env 兜底 client（MAGI_OPENAI_COMPAT_*）
        //      —— 仅在 settings.json 未配置 orchestrator 段时生效，
        //      用于开发/测试不带 UI 也能跑通的场景。
        //
        // settings.json 的 `auxiliary` 段不参与业务派发，只服务于会话标题、知识抽取、
        // 会话记忆、Prompt 增强等辅助任务（通过 RoleTarget::Auxiliary 分支独立解析）。
        let direct_http_probe_result = if model_bridge_override.is_some() {
            None
        } else {
            Self::try_build_http_model_client(bridge_env)
        };
        let direct_http_probe_config = direct_http_probe_result
            .as_ref()
            .map(|(_, config)| config.clone());

        // Use StdioMcpBridgeClient for direct MCP server connections when
        // MAGI_MCP_SERVER_COMMAND is configured, falling back to the
        // JSON-RPC subprocess loopback.
        let direct_mcp_client = StdioMcpBridgeClient::from_env();

        let business_model_client: Arc<dyn magi_bridge_client::ModelBridgeClient> =
            match model_bridge_override.clone() {
                Some(client) => client,
                None => {
                    if direct_http_probe_result.is_some() {
                        Arc::new(SettingsBackedBusinessModelBridgeClient::new(
                            self.state_root.clone(),
                            bridge_env,
                        ))
                    } else {
                        if !orchestrator_settings_is_configured(
                            &settings_store.get_section("orchestrator"),
                        ) {
                            warn!(
                                state_root = %self.state_root.display(),
                                "业务模型桥未配置，已退化为本地 loopback（仅用于开发/测试，生产请配置主模型或 MAGI_OPENAI_COMPAT_BASE_URL）"
                            );
                        }
                        Arc::new(JsonRpcModelBridgeClient::new(model_transport.clone()))
                    }
                }
            };
        let default_mcp_client: Arc<dyn McpBridgeClient> =
            if let Some(mcp_client) = direct_mcp_client {
                Arc::new(mcp_client)
            } else {
                Arc::new(JsonRpcMcpBridgeClient::new(mcp_transport.clone()))
            };
        let settings_backed_mcp_client = SettingsBackedMcpBridgeClient::new(
            settings_store.clone(),
            mcp_connections.clone(),
            default_mcp_client,
        );
        let bridge_runtime = BridgeDispatchRuntime::new()
            .with_model_client(business_model_client.clone())
            .with_mcp_client(Arc::new(settings_backed_mcp_client));
        let skill_runtime = SkillDispatchRuntime::new(tool_registry.clone(), bridge_runtime);
        let worker_runtime = self.worker_runtime.clone();
        let tool_registry_for_dispatcher = tool_registry.clone();
        let task_store_checkpoint_path = self.state_root.join("task-store.json");
        let event_bus_for_task_store = self.event_bus.clone();
        let session_store_for_task_status = self.session_store.clone();
        let runner_result_receiver = Arc::new(EventBasedResultReceiver::new());
        let task_store = match TaskStore::restore_from_file(&task_store_checkpoint_path) {
            Ok(Some(restored)) => {
                let eb = event_bus_for_task_store.clone();
                let session_store = session_store_for_task_status.clone();
                let receiver = runner_result_receiver.clone();
                restored.set_status_change_callback(Box::new(
                    move |task_id, old_status, new_status, task: magi_core::Task| {
                        publish_task_status_changed_event(
                            eb.as_ref(),
                            session_store.as_ref(),
                            task_id,
                            old_status,
                            new_status,
                            &task,
                        );
                        publish_task_status_turn_item_for_active_sessions(
                            &eb,
                            session_store.as_ref(),
                            None,
                            &task,
                            new_status,
                        );
                        push_terminal_task_result(&receiver, task_id, new_status);
                    },
                ));
                let (revoked_leases, failed_tasks) = restored
                    .reconcile_volatile_runtime_after_restore(&worker_runtime.durable_snapshot());
                if revoked_leases > 0 || failed_tasks > 0 {
                    warn!(
                        revoked_leases,
                        failed_tasks,
                        "检测到 checkpoint 中残留的易失执行态，已统一收口为可恢复状态"
                    );
                }
                Arc::new(restored)
            }
            _ => {
                let receiver = runner_result_receiver.clone();
                let session_store = session_store_for_task_status.clone();
                Arc::new(TaskStore::with_status_change_callback(Box::new(
                    move |task_id, old_status, new_status, task: magi_core::Task| {
                        publish_task_status_changed_event(
                            event_bus_for_task_store.as_ref(),
                            session_store.as_ref(),
                            task_id,
                            old_status,
                            new_status,
                            &task,
                        );
                        publish_task_status_turn_item_for_active_sessions(
                            &event_bus_for_task_store,
                            session_store.as_ref(),
                            None,
                            &task,
                            new_status,
                        );
                        push_terminal_task_result(&receiver, task_id, new_status);
                    },
                )))
            }
        };
        if self.reconcile_stale_session_task_chains(task_store.as_ref()) > 0
            && let Err(error) = task_store.checkpoint_to_file(&task_store_checkpoint_path)
        {
            warn!(?error, "收敛重启遗留任务状态后持久化 task-store 失败");
        }
        let (rebuilt_spawn_graph, spawn_graph_report) =
            magi_spawn_graph::SpawnGraph::rebuild_from_tasks(task_store.all_tasks());
        if spawn_graph_report.skipped_edges > 0 {
            warn!(
                candidate_edges = spawn_graph_report.candidate_edges,
                restored_edges = spawn_graph_report.restored_edges,
                skipped_edges = spawn_graph_report.skipped_edges,
                "从 task-store 重建 SpawnGraph 时跳过了不合法父子边"
            );
        } else if spawn_graph_report.restored_edges > 0 {
            tracing::debug!(
                restored_edges = spawn_graph_report.restored_edges,
                closed_edges = spawn_graph_report.closed_edges,
                "已从 task-store 重建 SpawnGraph"
            );
        }
        let spawn_graph = Arc::new(std::sync::Mutex::new(rebuilt_spawn_graph));
        let task_store_checkpoint_path_for_callback = task_store_checkpoint_path.clone();
        task_store.set_checkpoint_callback(Box::new(move |store| {
            if let Err(error) = store.checkpoint_to_file(&task_store_checkpoint_path_for_callback) {
                warn!(?error, "任务状态 checkpoint 持久化失败");
            }
        }));
        // 单一事实源：dispatch summary（execution_runtime）与 prompt 注入（LlmTaskDispatcher）
        // 使用同一份 ContextBudget。max_memory ≥ 一批 session-memory 的 slice 数（=5），
        // 否则辅助模型提取的 5 条 slice 会被预算切断、只投放前两条进 prompt。
        let context_budget = ContextBudget {
            max_turns: 8,
            max_knowledge: 6,
            max_memory: 8,
            max_shared_items: 4,
            max_file_summaries: 4,
        };
        let execution_runtime = orchestrator
            .execution_runtime(worker_runtime.clone(), tool_registry, skill_runtime.clone())
            .with_task_store(Arc::clone(&task_store))
            .with_context_runtime(
                context_runtime,
                ExecutionContextConfig {
                    budget: context_budget.clone(),
                    project_key: None,
                },
            );

        let session_checkpoint_persistence = RuntimeSidecarPersistence::new(
            StateRepository::new(self.state_root.clone()),
            self.session_store.clone(),
            self.workspace_store.clone(),
            self.worker_runtime.clone(),
        );
        let session_state_checkpoint_persist = Arc::new(move |checkpoint: &str| {
            session_checkpoint_persistence
                .flush_session_sidecars()
                .map(|_| ())
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
        .with_knowledge_store(self.knowledge_store.clone())
        .with_settings_store(settings_store.clone())
        .with_snapshot_manager(snapshot_manager)
        .with_skill_runtime(app_skill_runtime.clone())
        .with_skill_dispatch_runtime(Arc::new(skill_runtime.clone()))
        .with_mcp_connections(mcp_connections)
        .with_tool_registry(tool_registry_for_dispatcher.clone())
        .with_spawn_graph(spawn_graph)
        .with_agent_role_registry(agent_role_registry)
        .with_tunnel_port(self.local_port)
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            self.state_root.join("sessions.json"),
            self.state_root.join("workspaces.json"),
            self.state_root.join("knowledge.json"),
        )))
        .with_session_state_checkpoint_persist(session_state_checkpoint_persist)
        .with_bridge_probe_transport(BridgeServerKind::Model, model_transport)
        .with_bridge_probe_transport(BridgeServerKind::Mcp, mcp_transport)
        .with_execution_pipeline(orchestrator, execution_runtime, memory_store);

        state = state.with_task_store(Arc::clone(&task_store));
        if magi_api::task_turn_finalize::reconcile_terminal_session_task_turns(&state) > 0 {
            let _ = state.persist_session_durable_state();
        }
        let state_for_task_workers = state.clone();
        let state_for_runner_terminal = state.clone();
        let state_for_knowledge_persist = state.clone();
        let state_for_session_turn_persist = state.clone();
        let knowledge_persist_callback = Arc::new(move || {
            if let Err(error) = state_for_knowledge_persist.persist_knowledge_state() {
                tracing::warn!(?error, "自动知识沉淀持久化失败");
            }
        });
        let session_state_persist_callback = Arc::new(move |checkpoint: &str| {
            if let Err(error) =
                state_for_session_turn_persist.persist_session_state_checkpoint(checkpoint)
            {
                tracing::warn!(checkpoint, ?error, "session turn 关键状态持久化失败");
            }
        });
        let llm_task_dispatcher = LlmTaskDispatcher::new(
            self.event_bus.clone(),
            state
                .execution_pipeline()
                .expect("execution pipeline should exist when daemon wires task runner")
                .clone(),
            state.session_store.clone(),
            state.task_execution_registry().clone(),
            runner_result_receiver.clone(),
            state.spawn_graph.clone(),
        );
        let lifecycle_notice_registry = Arc::new(LifecycleNoticeRegistry::new());
        tokio::spawn(run_lifecycle_subscriber(
            lifecycle_notice_registry.clone(),
            self.event_bus.clone(),
        ));
        let mission_metrics_registry = Arc::new(MissionMetricsRegistry::new());
        let llm_task_dispatcher = Arc::new(
            llm_task_dispatcher
                .with_model_bridge_client(business_model_client.clone())
                .with_knowledge_store(state.knowledge_store.clone())
                .with_knowledge_persist_callback(knowledge_persist_callback)
                .with_session_state_persist_callback(session_state_persist_callback)
                .with_settings_store(state.settings_store.clone())
                .with_context_runtime(context_runtime_for_dispatcher)
                .with_context_budget(context_budget.clone())
                .with_workspace_registry(state.workspace_registry.clone())
                .with_tool_registry(tool_registry_for_dispatcher)
                .with_skill_runtime(app_skill_runtime)
                .with_snapshot_manager(state.snapshot_manager.clone())
                .with_conversation_registry(state.conversation_registry.clone())
                .with_agent_role_registry(state.agent_role_registry.clone())
                .with_lifecycle_notices(lifecycle_notice_registry)
                .with_mission_metrics_registry(mission_metrics_registry),
        );
        let session_turn_dispatcher = llm_task_dispatcher.clone();
        let human_checkpoint_registry_for_dispatch_gate =
            llm_task_dispatcher.human_checkpoint_registry();
        let workspace_registry_for_dispatch_gate = state.workspace_registry.clone();
        let task_execution_registry_for_dispatch_gate = state.task_execution_registry().clone();
        let runner_manager = RunnerManager::with_dispatcher_and_worker_catalog(
            Arc::clone(&task_store),
            Arc::new(move || state_for_task_workers.task_worker_catalog()),
            llm_task_dispatcher,
            runner_result_receiver,
        )
        .with_agent_role_registry(state.agent_role_registry.clone())
        .with_dispatch_gate(Arc::new(move |task| {
            if !task
                .policy_snapshot
                .as_ref()
                .is_some_and(|policy| policy.task_tier == TaskTier::LongMission)
            {
                return Ok(TaskDispatchGateDecision::Allow);
            }
            let workspace_id = match task_execution_registry_for_dispatch_gate.get(&task.task_id) {
                Some(TaskExecutionPlan::Dispatch {
                    workspace_id: Some(workspace_id),
                    ..
                }) => workspace_id,
                Some(TaskExecutionPlan::Dispatch {
                    workspace_id: None, ..
                }) => {
                    return Err(format!(
                        "long mission task {} 缺少 workspace_id，无法检查 HumanCheckpoint",
                        task.task_id
                    ));
                }
                None => {
                    return Err(format!(
                        "long mission task {} 缺少执行计划，无法检查 HumanCheckpoint",
                        task.task_id
                    ));
                }
            };
            let workspace_root = workspace_registry_for_dispatch_gate
                .workspaces()
                .into_iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .map(|workspace| WorkspaceRootPath::new(workspace.root_path.as_str()))
                .ok_or_else(|| {
                    format!(
                        "workspace {} 不存在，无法检查 HumanCheckpoint",
                        workspace_id
                    )
                })?;
            let store = human_checkpoint_registry_for_dispatch_gate
                .get_or_open(&workspace_root)
                .map_err(|error| format!("打开 HumanCheckpointStore 失败: {error}"))?;
            if store
                .has_pending(&task.mission_id)
                .map_err(|error| format!("读取 HumanCheckpoint pending 状态失败: {error}"))?
            {
                return Ok(TaskDispatchGateDecision::Blocked(format!(
                    "mission {} 存在 pending HumanCheckpoint，operator resolve 前禁止派发新任务",
                    task.mission_id
                )));
            }
            Ok(TaskDispatchGateDecision::Allow)
        }))
        .with_checkpoint_path(task_store_checkpoint_path)
        .with_terminal_observer(move |root_task_id, session_id, status| {
            let Some(session_id) = session_id else {
                return;
            };
            if magi_api::task_turn_finalize::finalize_background_session_task_turn_if_root_terminal(
                &state_for_runner_terminal,
                &session_id,
                &root_task_id,
                &status,
            ) {
                let _ = state_for_runner_terminal.persist_session_durable_state();
            }
        });
        state = state
            .with_runner_manager(runner_manager)
            .with_session_turn_dispatcher(session_turn_dispatcher)
            .with_model_bridge_client(business_model_client);

        if let Some(probe_config) = direct_http_probe_config {
            state = state.with_direct_http_model_probe(probe_config);
        }

        // 把 SnapshotManager 桥接到 session-store 生命周期事件。生产路径必装；
        // 测试可用 ApiState::new 直接构造而不调用此函数，惰性 fallback 仍兜底。
        state.install_snapshot_lifecycle_observer();

        state
    }

    fn reconcile_stale_session_task_chains(&self, task_store: &TaskStore) -> usize {
        let interrupted_task_count = self.fail_interrupted_session_task_chains(task_store);
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
            if interrupted_task_count > 0 {
                self.flush_reconciled_runtime_sidecars(
                    "收敛重启遗留的 session task chain 后持久化 sidecar 失败",
                );
                warn!(
                    interrupted_task_count,
                    "已将 daemon 重启遗留的执行中任务收口为可恢复状态"
                );
            }
            return interrupted_task_count;
        }

        let stale_count = stale_sidecars.len();
        for mut sidecar in stale_sidecars {
            // 在切断 chain 前，先把仍处于非终态的 current_turn 收敛为 cancelled，
            // 否则 reconcile_terminal_session_task_turns 会因 chain 已为 None 而跳过它，
            // 留下永远停在 "running" 的孤立 canonical turn。
            let session_id = sidecar.session_id.clone();
            if sidecar
                .current_turn
                .as_ref()
                .is_some_and(|turn| !current_turn_status_is_terminal(&turn.status))
                && let Err(error) = self.session_store.cancel_current_turn(&session_id)
            {
                warn!(
                    ?error,
                    %session_id,
                    "取消失效 session chain 的 current_turn 失败"
                );
            }

            sidecar.active_execution_chain = None;
            sidecar.ownership = ExecutionOwnership::default();
            sidecar.status = if sidecar.recovery_id.is_some() {
                SessionExecutionSidecarStatus::RecoveryLinked
            } else {
                SessionExecutionSidecarStatus::Detached
            };
            sidecar.updated_at = UtcMillis::now();
            // current_turn 已经被 cancel_current_turn 覆盖到最新状态，这里重新读取
            // 一次，避免 upsert 时把刚 cancel 掉的 turn 又抬回非终态。
            if let Some(latest) = self.session_store.runtime_sidecar(&session_id) {
                sidecar.current_turn = latest.current_turn;
            }
            self.session_store.upsert_runtime_sidecar(sidecar);
        }

        // chain 已经清理完，再统一处理 chain 缺失但 current_turn 仍非终态的 sidecar。
        self.cancel_orphan_non_terminal_current_turns();

        self.flush_reconciled_runtime_sidecars("清理失效 session task chain 后持久化 sidecar 失败");
        warn!(
            stale_count,
            interrupted_task_count, "已清理指向缺失 root task 的 session task chain"
        );
        interrupted_task_count
    }

    fn fail_interrupted_session_task_chains(&self, task_store: &TaskStore) -> usize {
        let root_task_ids = self
            .session_store
            .runtime_sidecars()
            .into_iter()
            .filter_map(|sidecar| {
                let chain = sidecar.active_execution_chain?;
                let root_task = task_store.get_task(&chain.root_task_id)?;
                if matches!(root_task.status, TaskStatus::Completed | TaskStatus::Killed) {
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
                (turn_is_active || has_in_memory_task).then_some(chain.root_task_id)
            })
            .collect::<HashSet<_>>();

        let mut failed_count = 0usize;
        for root_task_id in root_task_ids {
            for task_id in task_store.collect_subtree_ids(&root_task_id) {
                let Some(task) = task_store.get_task(&task_id) else {
                    continue;
                };
                if matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                    && task_store
                        .update_status(&task_id, TaskStatus::Failed)
                        .is_ok()
                {
                    failed_count += 1;
                }
            }
        }
        failed_count
    }

    fn flush_reconciled_runtime_sidecars(&self, warning: &'static str) {
        let repository = StateRepository::new(self.state_root.clone());
        let persistence = RuntimeSidecarPersistence::new(
            repository,
            self.session_store.clone(),
            self.workspace_store.clone(),
            self.worker_runtime.clone(),
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
            if let Err(error) = self.session_store.cancel_current_turn(&session_id) {
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

    pub(crate) fn router(&self, service_name: String) -> axum::Router {
        build_router(self.build_api_state(service_name))
    }

    #[cfg(test)]
    pub(crate) fn router_with_state_for_tests(
        &self,
        service_name: String,
    ) -> (axum::Router, ApiState) {
        let state = self.build_api_state_with_options(
            service_name,
            &[],
            Some(Arc::new(StaticTestModelBridgeClient)),
        );
        (build_router(state.clone()), state)
    }

    #[cfg(test)]
    pub(crate) fn router_with_bridge_env_for_tests(
        &self,
        service_name: String,
        bridge_env: &[(&str, &str)],
    ) -> (axum::Router, ApiState) {
        let state = self.build_api_state_with_options(service_name, bridge_env, None);
        (build_router(state.clone()), state)
    }

    fn restore_ledger(
        state_repository: &StateRepository,
        event_bus: &Arc<InMemoryEventBus>,
    ) -> Result<(), DaemonError> {
        let audit_usage_ledger = state_repository.load_audit_usage_ledger()?;
        event_bus.import_audit_usage_ledger_snapshot(audit_usage_ledger);
        event_bus.set_audit_usage_ledger_persistence(state_repository.audit_usage_ledger_path());
        if let Err(error) = event_bus.refresh_audit_usage_ledger_persistence() {
            warn!(error = %error, "审计/用量账本初始刷新失败，后续事件仍会继续运行");
        }
        publish_ledger_status_event(event_bus, "system-ledger-ready", "system.ledger.ready");
        Ok(())
    }

    fn bootstrap_runtime_state(
        config: &DaemonConfig,
        state_repository: &StateRepository,
        runtime_persistence: &RuntimeSidecarPersistence,
        session_store: &Arc<SessionStore>,
        workspace_store: &Arc<WorkspaceStore>,
    ) -> Result<(), DaemonError> {
        bootstrap_state(
            session_store,
            workspace_store,
            &config.bootstrap_workspace_root,
            &config.bootstrap_worktree_root,
        );
        // 会话保存到各工作区的 .magi/sessions.json，不再写全局路径
        let durable = session_store.durable_state();
        for workspace in workspace_store.workspaces() {
            let root = std::path::PathBuf::from(workspace.root_path.as_str());
            let ws_id = workspace.workspace_id.to_string();
            let ws_sessions: Vec<_> = durable
                .sessions
                .iter()
                .filter(|s| s.workspace_id.as_deref() == Some(&ws_id))
                .cloned()
                .collect();
            let session_ids = ws_sessions
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<HashSet<_>>();
            let ws_state = magi_session_store::SessionDurableState {
                sessions: ws_sessions,
                current_session_id: durable.current_session_id.clone(),
                timeline: durable
                    .timeline
                    .iter()
                    .filter(|entry| session_ids.contains(&entry.session_id))
                    .cloned()
                    .collect(),
                canonical_turns: durable
                    .canonical_turns
                    .iter()
                    .filter(|turn| session_ids.contains(&turn.session_id))
                    .cloned()
                    .collect(),
                notifications: durable
                    .notifications
                    .iter()
                    .filter(|notification| session_ids.contains(&notification.session_id))
                    .cloned()
                    .collect(),
            };
            state_repository.save_workspace_session_state(&root, &ws_state)?;
        }
        state_repository.save_workspace_durable_state(&workspace_store.durable_state())?;
        runtime_persistence.flush_runtime_sidecars()?;
        Ok(())
    }

    fn bridge_loopback_transport_with_env(
        binary_name: &str,
        bridge_env: &[(&str, &str)],
    ) -> Arc<dyn BridgeTransport> {
        let transport = bridge_env.iter().fold(
            JsonRpcStdioTransport::new(Self::bridge_loopback_executable(binary_name)),
            |transport, (key, value)| transport.with_env(*key, *value),
        );
        Arc::new(transport)
    }

    /// Build an `HttpModelBridgeClient` from configuration.
    ///
    /// 业务模型配置来源（仅这两类，按优先级）：
    /// 1. `bridge_env` overrides（测试场景注入）
    /// 2. 进程级 env（`MAGI_OPENAI_COMPAT_*`）
    ///
    /// **不再回退**读 `settings.json` 的 `auxiliary` 段 —— aux 段是辅助模型专用配置，
    /// 业务模型与辅助模型混读同一份字段会造成"改 aux 设置静默切换业务模型"的
    /// 配置错位。业务模型未配置时返回 `None`，调用方应据此走 unavailable-client 提示。
    ///
    /// Returns the client together with a [`DirectHttpModelProbeConfig`] that
    /// the cutover-smoke provider can use for its own independent probe.
    fn try_build_http_model_client(
        bridge_env: &[(&str, &str)],
    ) -> Option<(HttpModelBridgeClient, DirectHttpModelProbeConfig)> {
        let config = Self::openai_compat_env_config(bridge_env)?;
        let base_url = config.base_url;
        let api_key = config.api_key;
        let model = config.model;
        let protocol = HttpModelBridgeProtocol::ChatCompletions;

        let probe_config = DirectHttpModelProbeConfig {
            base_url: base_url.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
        };
        Some((
            HttpModelBridgeClient::new_with_protocol(base_url, api_key, model, protocol, None),
            probe_config,
        ))
    }

    fn seed_orchestrator_settings_from_env_if_empty(
        settings_store: &Arc<SettingsStore>,
        bridge_env: &[(&str, &str)],
    ) {
        if orchestrator_settings_is_configured(&settings_store.get_section("orchestrator")) {
            return;
        }
        let Some(config) = Self::openai_compat_env_config(bridge_env) else {
            return;
        };
        let mut section = serde_json::json!({
            "baseUrl": config.base_url,
            "model": config.model,
            "urlMode": "standard",
            "reasoningEffort": "medium"
        });
        if let Some(api_key) = config.api_key {
            section["apiKey"] = serde_json::Value::String(api_key);
        }
        settings_store.set_section("orchestrator", section);
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

    fn bridge_loopback_executable(binary_name: &str) -> String {
        let env_key = format!("CARGO_BIN_EXE_{binary_name}");
        if let Some(path) = env::var_os(&env_key) {
            return path.to_string_lossy().to_string();
        }

        let mut path = env::current_exe().expect("current executable should exist");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.push(Self::binary_name(binary_name));
        path.to_string_lossy().to_string()
    }

    fn binary_name(binary_name: &str) -> String {
        format!("{binary_name}{}", env::consts::EXE_SUFFIX)
    }
}

fn push_terminal_task_result(
    receiver: &Arc<EventBasedResultReceiver>,
    task_id: &magi_core::TaskId,
    new_status: TaskStatus,
) {
    match new_status {
        TaskStatus::Completed => {
            receiver.push_result(TaskResult {
                task_id: task_id.clone(),
                lease_id: LeaseId::new(format!("lease-result-{}", task_id)),
                outcome: TaskOutcome::Completed {
                    output_refs: Vec::new(),
                },
            });
        }
        TaskStatus::Failed => {
            receiver.push_result(TaskResult {
                task_id: task_id.clone(),
                lease_id: LeaseId::new(format!("lease-result-{}", task_id)),
                outcome: TaskOutcome::Failed {
                    error: "task store reported terminal failure".to_string(),
                },
            });
        }
        _ => {
            // 非终态重置代表任务将重新派发；必须清掉旧终态结果，避免 runner
            // 下一轮先消费陈旧失败/完成事件。
            receiver.clear_task_result_state(task_id);
        }
    }
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
    let event = EventEnvelope::domain(
        EventId::new(format!(
            "event-task-status-changed-{}-{}",
            task_id,
            UtcMillis::now().0
        )),
        magi_event_bus::task_events::TASK_STATUS_CHANGED,
        serde_json::json!({
            "task_id": task_id.to_string(),
            "root_task_id": task.root_task_id.to_string(),
            "mission_id": task.mission_id.to_string(),
            "title": task.title.as_str(),
            "old_status": format!("{:?}", old_status),
            "new_status": format!("{:?}", new_status),
            "kind": format!("{:?}", task.kind),
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

fn task_status_event_scope(
    session_store: &SessionStore,
    task: &magi_core::Task,
) -> Option<(SessionId, Option<magi_core::WorkspaceId>)> {
    session_store
        .active_execution_sidecars()
        .into_iter()
        .find(|sidecar| task_matches_runtime_sidecar(sidecar, task))
        .map(|sidecar| {
            let workspace_id = sidecar
                .active_execution_chain
                .as_ref()
                .and_then(|chain| chain.workspace_id.clone())
                .or_else(|| sidecar.ownership.workspace_id.clone());
            (sidecar.session_id, workspace_id)
        })
}

fn task_matches_runtime_sidecar(sidecar: &SessionRuntimeSidecar, task: &magi_core::Task) -> bool {
    let active_chain_matches = sidecar
        .active_execution_chain
        .as_ref()
        .is_some_and(|chain| {
            chain.root_task_id == task.root_task_id
                || chain.root_task_id == task.task_id
                || chain
                    .active_branch_task_ids
                    .iter()
                    .any(|task_id| task_id == &task.task_id)
                || chain
                    .branches
                    .iter()
                    .any(|branch| branch.task_id == task.task_id)
        });
    let turn_matches = sidecar.current_turn.as_ref().is_some_and(|turn| {
        turn.items
            .iter()
            .any(|item| item.task_id.as_ref() == Some(&task.task_id))
    });
    active_chain_matches || turn_matches
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonRuntime, SettingsBackedMcpBridgeClient, build_agent_role_catalog_provider,
        publish_task_status_changed_event,
    };
    use crate::daemon::config::DaemonConfig;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_api::SettingsStore;
    use magi_bridge_client::{
        BridgeResponse, McpBridgeClient, McpServerConfig, McpToolCallRequest, StdioMcpBridgeClient,
    };
    use magi_core::{
        MissionId, SessionId, Task, TaskId, TaskKind, TaskRuntimePayload, TaskStatus, ThreadId,
        UtcMillis, WorkerId, WorkspaceId,
    };
    use magi_event_bus::AuditUsageLedgerSnapshot;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{
        ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
        ActiveExecutionTurn, SessionStore,
    };
    use serde_json::{Value, json};
    use std::{
        collections::{BTreeMap, HashMap},
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        path::PathBuf,
        sync::{Arc, Mutex, RwLock, mpsc},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use tower::util::ServiceExt;

    const BACKGROUND_TEST_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Default)]
    struct RecordingMcpClient {
        calls: Arc<Mutex<Vec<McpToolCallRequest>>>,
    }

    impl McpBridgeClient for RecordingMcpClient {
        fn call_tool(
            &self,
            request: McpToolCallRequest,
        ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
            self.calls
                .lock()
                .expect("recording mcp client mutex should lock")
                .push(request);
            Ok(BridgeResponse {
                ok: true,
                payload: "fallback-ok".to_string(),
            })
        }
    }

    fn temp_state_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "magi-daemon-runtime-test-{name}-{}",
            magi_core::UtcMillis::now().0
        ));
        fs::create_dir_all(&root).expect("temp state root should be creatable");
        root
    }

    fn spawn_graph_restore_task(
        task_id: &str,
        root_task_id: &str,
        parent_task_id: Option<&str>,
        status: TaskStatus,
        created_at: u64,
    ) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-spawn-graph-restore"),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent_task_id.map(TaskId::new),
            kind: TaskKind::LocalAgent,
            title: format!("task {task_id}"),
            goal: format!("run task {task_id}"),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: UtcMillis(created_at),
            updated_at: UtcMillis(created_at + 1),
        }
    }

    #[test]
    fn agent_role_catalog_provider_exports_spawnable_roles() {
        let registry = std::sync::Arc::new(magi_agent_role::AgentRoleRegistry::load_default());
        let provider = build_agent_role_catalog_provider(registry);
        let roles = provider();

        assert!(
            roles
                .iter()
                .any(|role| role.role_id == "executor" && role.spawnable),
            "executor 应作为 agent_spawn 可派发角色暴露"
        );
        assert!(
            roles
                .iter()
                .any(|role| role.role_id == "coordinator" && !role.spawnable),
            "coordinator 是主线编排身份，不应作为可派发代理暴露"
        );
        assert!(
            roles.iter().any(|role| role
                .supported_kinds
                .iter()
                .any(|kind| kind == "local_agent")),
            "代理角色目录应暴露 supported_kinds，便于 tool_catalog 诊断"
        );
    }

    #[test]
    fn external_tool_catalog_uses_stable_bridge_labels() {
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            skill_id: "catalog-skill".to_string(),
            title: "Catalog Skill".to_string(),
            instruction: "test".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "test".to_string(),
                tags: Vec::new(),
            },
            allowed_tools: Vec::new(),
            custom_tool_bindings: vec![magi_skill_runtime::CustomToolBinding {
                binding_id: "catalog-binding".to_string(),
                tool_name: "catalog.inspect".to_string(),
                description: "Inspect catalog".to_string(),
                bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp,
                dispatch_action: magi_bridge_client::BridgeDispatchAction::McpToolCall,
                bridge_target: "local-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        let provider = super::build_external_tool_catalog_provider(
            std::sync::Arc::new(super::SettingsStore::default()),
            std::sync::Arc::new(magi_skill_runtime::SkillRuntime::new(registry)),
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        );

        let snapshot = provider();
        let tool = snapshot
            .skill_tools
            .iter()
            .find(|tool| tool.name == "catalog.inspect")
            .expect("skill custom tool should be exported");

        assert_eq!(tool.bridge_kind, "mcp");
        assert_eq!(tool.dispatch_action, "mcp_tool_call");
        assert_eq!(tool.access_profile_behavior, "restricted_requires_approval");
        assert_eq!(tool.approval_requirement, "required");
    }

    #[test]
    fn task_status_changed_event_uses_active_session_workspace_scope() {
        let event_bus = magi_event_bus::InMemoryEventBus::new(16);
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-task-status-scope");
        let workspace_id = WorkspaceId::new("workspace-task-status-scope");
        let mission_id = MissionId::new("mission-task-status-scope");
        let task_id = TaskId::new("task-status-scope");
        let worker_id = WorkerId::new("worker-task-status-scope");
        let now = UtcMillis::now();
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task status scope",
                Some(workspace_id.to_string()),
            )
            .expect("session should be created");
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: task_id.clone(),
                    execution_chain_ref: "chain-task-status-scope".to_string(),
                    workspace_id: Some(workspace_id.clone()),
                    active_branch_task_ids: vec![task_id.clone()],
                    active_worker_bindings: vec![worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: task_id.clone(),
                        worker_id,
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                        thread_id: ThreadId::new("thread-task-status-scope"),
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-task-status-scope".to_string(),
                        trimmed_text: Some("run scoped task".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-task-status-scope".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("run scoped task".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active execution chain should be stored");
        let mut task = spawn_graph_restore_task(
            task_id.as_str(),
            task_id.as_str(),
            None,
            TaskStatus::Running,
            now.0,
        );
        task.mission_id = mission_id;

        publish_task_status_changed_event(
            &event_bus,
            &session_store,
            &task_id,
            TaskStatus::Pending,
            TaskStatus::Running,
            &task,
        );

        let event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "task.status.changed")
            .expect("task status event should be published");
        assert_eq!(event.session_id.as_ref(), Some(&session_id));
        assert_eq!(event.workspace_id.as_ref(), Some(&workspace_id));
        assert_eq!(event.payload["session_id"], json!(session_id.as_str()));
        assert_eq!(event.payload["workspace_id"], json!(workspace_id.as_str()));
        assert_eq!(event.payload["root_task_id"], json!(task_id.as_str()));
        assert_eq!(event.payload["title"], json!(task.title));
    }

    #[test]
    fn external_mcp_catalog_redacts_raw_error_text() {
        let connections =
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
        let raw = json!({
            "id": "local-mcp",
            "name": "Local MCP",
            "enabled": true,
            "error": "/Users/xie/.mcp/server failed: ENOENT",
        });

        let entry = super::external_mcp_server_catalog_entry(&raw, &connections)
            .expect("mcp catalog entry should be built");

        assert_eq!(entry.error.as_deref(), Some("mcp_connection_failed"));
    }

    #[test]
    fn external_mcp_catalog_keeps_disabled_servers_non_error() {
        let connections =
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
        let raw = json!({
            "id": "disabled-mcp",
            "name": "Disabled MCP",
            "enabled": false,
            "toolCount": 3,
            "error": "/private/raw error"
        });

        let entry = super::external_mcp_server_catalog_entry(&raw, &connections)
            .expect("mcp catalog entry should be built");

        assert!(!entry.enabled);
        assert!(!entry.connected);
        assert_eq!(entry.health, "disabled");
        assert_eq!(entry.tool_count, None);
        assert_eq!(entry.error, None);
    }

    #[test]
    fn settings_backed_mcp_bridge_delegates_unconfigured_targets_to_default_client() {
        let settings_store = Arc::new(SettingsStore::new());
        let fallback = Arc::new(RecordingMcpClient::default());
        let calls = fallback.calls.clone();
        let client = SettingsBackedMcpBridgeClient::new(
            settings_store,
            Arc::new(RwLock::new(HashMap::new())),
            fallback,
        );

        let response = client
            .call_tool(McpToolCallRequest {
                server_name: "loopback-mcp".to_string(),
                tool_name: "echo.inspect".to_string(),
                input: "{}".to_string(),
            })
            .expect("unconfigured target should use default bridge client");

        assert!(response.ok);
        let calls = calls
            .lock()
            .expect("recording mcp client mutex should lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].server_name, "loopback-mcp");
    }

    #[test]
    fn settings_backed_mcp_bridge_rejects_disabled_configured_target_before_default_client() {
        let settings_store = Arc::new(SettingsStore::new());
        settings_store.set_section(
            "mcpServers",
            json!([
                {
                    "id": "disabled-mcp",
                    "name": "disabled-mcp",
                    "command": "node",
                    "enabled": false
                }
            ]),
        );
        let fallback = Arc::new(RecordingMcpClient::default());
        let calls = fallback.calls.clone();
        let connections = Arc::new(RwLock::new(HashMap::new()));
        connections
            .write()
            .expect("mcp connection pool should lock")
            .insert(
                "disabled-mcp".to_string(),
                Arc::new(StdioMcpBridgeClient::new(McpServerConfig {
                    command: "node".to_string(),
                    args: Vec::new(),
                    working_directory: None,
                    env: BTreeMap::new(),
                })),
            );
        let client =
            SettingsBackedMcpBridgeClient::new(settings_store, connections.clone(), fallback);

        let error = client
            .call_tool(McpToolCallRequest {
                server_name: "disabled-mcp".to_string(),
                tool_name: "echo.inspect".to_string(),
                input: "{}".to_string(),
            })
            .expect_err("disabled configured target should not use default bridge client");

        assert!(matches!(
            error,
            magi_bridge_client::BridgeClientError::MissingClient { .. }
        ));
        assert!(
            calls
                .lock()
                .expect("recording mcp client mutex should lock")
                .is_empty(),
            "configured disabled target must not fall through to the default bridge client"
        );
        assert!(
            !connections
                .read()
                .expect("mcp connection pool should lock")
                .contains_key("disabled-mcp"),
            "disabled configured target must clear stale settings-backed connection"
        );
    }

    #[tokio::test]
    async fn daemon_tools_catalog_route_exposes_runtime_catalog() {
        let state_root = temp_state_root("tools-catalog-route");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        fs::create_dir_all(config.bootstrap_workspace_root.join("src"))
            .expect("bootstrap workspace source directory should be creatable");
        fs::write(
            config.bootstrap_workspace_root.join("src/lib.rs"),
            "pub fn tool_catalog_route_probe() -> bool { true }\n",
        )
        .expect("bootstrap workspace source file should be writable");
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap state");
        let catalog = get_json(
            runtime.router("daemon-test".to_string()),
            "/api/tools/catalog?workspaceId=test-workspace-001&sessionId=test-session-001&includeInternal=true&includeSchema=true",
        )
        .await;

        assert_eq!(catalog["tool"], "tool_catalog");
        assert_eq!(catalog["status"], "succeeded");
        assert_eq!(catalog["external_catalog_status"], "available");
        assert_eq!(catalog["agent_role_catalog_status"], "available");
        assert!(
            catalog["spawnable_agent_role_count"]
                .as_u64()
                .expect("spawnable agent role count should serialize")
                > 0,
            "daemon tool catalog should expose agent_spawn-capable roles"
        );
        let workspace_code_index = catalog["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies should be an array")
            .iter()
            .find(|dependency| dependency["name"] == "workspace_code_index")
            .expect("workspace_code_index dependency should be exposed");
        assert_eq!(workspace_code_index["workspace_id"], "test-workspace-001");
        assert_eq!(workspace_code_index["status"], "ready");
        let context_runtime = catalog["runtime_dependencies"]
            .as_array()
            .expect("runtime dependencies should be an array")
            .iter()
            .find(|dependency| dependency["name"] == "context_runtime")
            .expect("context_runtime dependency should be exposed");
        assert_eq!(context_runtime["workspace_id"], "test-workspace-001");
        assert_eq!(context_runtime["session_id"], "test-session-001");
        assert_eq!(context_runtime["status"], "ready");
        let process_launch = catalog["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .find(|tool| tool["name"] == "process_launch")
            .expect("includeInternal=true should expose process_launch");
        assert_eq!(process_launch["public"], false);
        assert_eq!(process_launch["parameters_schema"]["type"], "object");
    }

    #[tokio::test]
    async fn daemon_restore_rebuilds_spawn_graph_from_task_store_checkpoint() {
        let state_root = temp_state_root("spawn-graph-restore");
        let task_store = TaskStore::new();
        task_store.insert_task(spawn_graph_restore_task(
            "task-root-spawn-restore",
            "task-root-spawn-restore",
            None,
            TaskStatus::Running,
            1,
        ));
        task_store.insert_task(spawn_graph_restore_task(
            "task-child-spawn-restore",
            "task-root-spawn-restore",
            Some("task-root-spawn-restore"),
            TaskStatus::Pending,
            2,
        ));
        task_store
            .checkpoint_to_file(&state_root.join("task-store.json"))
            .expect("task store checkpoint should be written");

        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime = DaemonRuntime::restore(&config)
            .expect("runtime restore should load task-store checkpoint");
        let (_router, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
        let graph = state
            .spawn_graph
            .lock()
            .expect("spawn graph lock should not poison");

        assert_eq!(
            graph.parent_of(&TaskId::new("task-child-spawn-restore")),
            Some(&TaskId::new("task-root-spawn-restore")),
            "daemon restore should rebuild SpawnGraph from persisted Task.parent_task_id"
        );
    }

    #[test]
    fn restore_bootstraps_empty_state_and_persists_runtime_files() {
        let state_root = temp_state_root("bootstrap");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
        let workspace_root = config.bootstrap_workspace_root.clone();

        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");

        assert!(runtime.session_store.current_session().is_some());
        assert_eq!(runtime.workspace_store.snapshots().len(), 1);
        assert!(workspace_root.join(".magi").join("sessions.json").exists());
        assert!(!state_root.join("sessions.json").exists());
        assert!(state_root.join("workspaces.json").exists());
        assert!(state_root.join("session-sidecars.json").exists());
        assert!(state_root.join("workspace-recovery-sidecars.json").exists());
        assert!(state_root.join("audit-usage-ledger.json").exists());

        let ledger = serde_json::from_slice::<AuditUsageLedgerSnapshot>(
            &fs::read(state_root.join("audit-usage-ledger.json"))
                .expect("audit usage ledger should be readable"),
        )
        .expect("audit usage ledger should deserialize");
        assert!(ledger.audit_entries.is_empty());
        assert!(ledger.usage_entries.is_empty());
    }

    #[tokio::test]
    async fn knowledge_endpoint_returns_code_index_after_bootstrap_scan() {
        let state_root = temp_state_root("knowledge-code-index");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
        let workspace_root = config.bootstrap_workspace_root.clone();

        // 在引导工作区中创建模拟源文件，供代码扫描器发现
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(
            workspace_root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .unwrap();
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        let runtime = DaemonRuntime::restore(&config)
            .expect("runtime restore should bootstrap and scan code");

        // 验证知识存储中有代码索引摘要
        let summary = runtime
            .knowledge_store
            .code_index_summary_for_workspace(&WorkspaceId::new("test-workspace-001"))
            .expect("code index summary should exist after bootstrap scan");
        assert!(
            !summary.files.is_empty(),
            "code index should contain scanned files"
        );
        assert!(
            summary.tech_stack.iter().any(|t| t == "Rust"),
            "tech stack should detect Rust"
        );
        assert!(
            summary.entry_points.iter().any(|e| e.ends_with("main.rs")),
            "entry points should detect main.rs"
        );

        // 通过 API 路由验证返回结构
        let app = runtime.router("daemon-test".to_string());
        let knowledge = get_json(app, "/api/knowledge?workspaceId=test-workspace-001").await;
        assert!(
            knowledge.get("codeIndex").is_some_and(|v| !v.is_null()),
            "API should return non-null codeIndex"
        );
        let code_index = knowledge["codeIndex"]
            .as_object()
            .expect("codeIndex should be an object");
        let files = code_index["files"]
            .as_array()
            .expect("files should be an array");
        assert!(!files.is_empty(), "codeIndex.files should not be empty");
        assert!(
            code_index["techStack"]
                .as_array()
                .is_some_and(|t| !t.is_empty()),
            "codeIndex.techStack should not be empty"
        );
    }

    #[tokio::test]
    async fn restore_rebuilds_code_index_for_all_registered_workspaces() {
        let state_root = temp_state_root("multi-workspace-code-index");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
        let secondary_root = state_root.join("secondary-workspace");
        fs::create_dir_all(secondary_root.join("src")).unwrap();
        fs::write(
            secondary_root.join("src/lib.rs"),
            "pub fn secondary_workspace_probe() -> bool { true }\n",
        )
        .unwrap();
        fs::write(
            secondary_root.join("Cargo.toml"),
            "[package]\nname = \"secondary-workspace\"\n",
        )
        .unwrap();

        let secondary_workspace_id = {
            let runtime = DaemonRuntime::restore(&config)
                .expect("initial runtime restore should bootstrap state");
            let (status, body) = post_json(
                runtime.router("daemon-test".to_string()),
                "/api/workspaces/register",
                json!({ "path": secondary_root.to_string_lossy() }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let workspace_id = body["workspaceId"]
                .as_str()
                .expect("registered workspace id")
                .to_string();
            assert!(
                runtime
                    .knowledge_store
                    .workspace_index_ready(&WorkspaceId::new(&workspace_id)),
                "newly registered workspace should build an in-process search index"
            );
            workspace_id
        };

        let restored = DaemonRuntime::restore(&config)
            .expect("runtime restore should rebuild indexes for every registered workspace");
        assert!(
            restored
                .knowledge_store
                .workspace_index_ready(&WorkspaceId::new(&secondary_workspace_id)),
            "non-active registered workspace search index should be rebuilt after daemon restart"
        );

        let bootstrap = get_json(
            restored.router("daemon-test".to_string()),
            &format!("/api/settings/bootstrap?scope=core&workspaceId={secondary_workspace_id}"),
        )
        .await;
        let workspace_code_index = bootstrap["capabilityDependencies"]
            .as_array()
            .expect("capability dependencies should be array")
            .iter()
            .find(|dependency| dependency["name"] == "workspace_code_index")
            .expect("workspace_code_index dependency should be exposed");
        assert_eq!(workspace_code_index["status"], "ready");
    }

    async fn post_json(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let status = response.status();
        let body = serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be valid json");
        (status, body)
    }

    async fn get_json(app: axum::Router, path: &str) -> Value {
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should read"),
        )
        .expect("response should be valid json")
    }

    async fn get_task_projection(
        app: axum::Router,
        root_task_id: &str,
        session_id: &str,
        workspace_id: &str,
    ) -> Value {
        get_json(
            app,
            &format!(
                "/api/tasks/projection/{root_task_id}?workspaceId={workspace_id}&sessionId={session_id}"
            ),
        )
        .await
    }

    async fn wait_for_task_projection_completed(
        app: axum::Router,
        root_task_id: &str,
        session_id: &str,
        workspace_id: &str,
    ) -> Value {
        let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
        loop {
            let projection =
                get_task_projection(app.clone(), root_task_id, session_id, workspace_id).await;
            let total_tasks = projection["progress_summary"]["total_tasks"]
                .as_u64()
                .unwrap_or(0);
            let completed_tasks = projection["progress_summary"]["completed_tasks"]
                .as_u64()
                .unwrap_or(0);
            if total_tasks >= 1
                && completed_tasks == total_tasks
                && projection["root_task"]["status"] == "completed"
            {
                return projection;
            }
            if Instant::now() >= deadline {
                return projection;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_execution_group(
        app: axum::Router,
        mission_id: &str,
        mut is_ready: impl FnMut(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
        loop {
            let read_model = get_json(app.clone(), "/runtime/read-model").await;
            if let Some(group) = read_model["details"]["execution_groups"]
                .as_array()
                .and_then(|groups| {
                    groups
                        .iter()
                        .find(|entry| entry["mission_id"] == mission_id)
                })
            {
                if is_ready(group) || Instant::now() >= deadline {
                    return group.clone();
                }
            }
            if Instant::now() >= deadline {
                panic!("execution group {mission_id} did not appear before timeout");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn assert_completed_two_task_projection(projection: &Value) {
        let total_tasks = projection["progress_summary"]["total_tasks"]
            .as_u64()
            .expect("total_tasks should serialize as integer");
        let completed_tasks = projection["progress_summary"]["completed_tasks"]
            .as_u64()
            .expect("completed_tasks should serialize as integer");
        // ExecutionChain：单 worker 任务只产出 1 个 root task；coordinator 才会扩展为多任务。
        assert!(
            total_tasks >= 1,
            "task projection should include the root task"
        );
        assert_eq!(completed_tasks, total_tasks);
        assert_eq!(projection["progress_summary"]["failed_tasks"], 0);
        assert_eq!(projection["root_task"]["status"], "completed");
    }

    fn service_entries_by_kind(snapshot: &Value) -> BTreeMap<String, &Value> {
        snapshot["services"]
            .as_array()
            .expect("services should be an array")
            .iter()
            .map(|entry| {
                (
                    entry["server_kind"]
                        .as_str()
                        .expect("server kind should serialize as string")
                        .to_string(),
                    entry,
                )
            })
            .collect()
    }

    fn test_bridge_binary_path(binary_name: &str) -> PathBuf {
        let env_key = format!("CARGO_BIN_EXE_{binary_name}");
        if let Some(path) = std::env::var_os(&env_key) {
            return PathBuf::from(path);
        }

        let mut path = std::env::current_exe().expect("current exe should exist");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.push(format!("{binary_name}{}", std::env::consts::EXE_SUFFIX));
        path
    }

    #[derive(Debug)]
    struct RecordedHttpRequest {
        request_line: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    fn spawn_http_stub(
        status: u16,
        response_body: Value,
    ) -> (String, mpsc::Receiver<RecordedHttpRequest>, JoinHandle<()>) {
        // Default to 2 connections: one for the JSON-RPC bridge loopback
        // and one for the direct HTTP model provider probe.
        spawn_http_stub_multi(status, response_body, 2)
    }

    fn spawn_http_stub_multi(
        status: u16,
        response_body: Value,
        connection_count: usize,
    ) -> (String, mpsc::Receiver<RecordedHttpRequest>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("http stub should bind");
        let address = listener.local_addr().expect("http stub addr should exist");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            for _ in 0..connection_count {
                let (mut stream, _) = listener.accept().expect("http stub should accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout should set");
                let request = read_http_request(&mut stream);
                let body = response_body.to_string();
                let response = format!(
                    "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    status_reason(status),
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("http stub should write");
                stream.flush().expect("http stub should flush");
                sender.send(request).expect("request should send to test");
            }
        });

        (format!("http://{address}/v1"), receiver, handle)
    }

    fn read_http_request(stream: &mut TcpStream) -> RecordedHttpRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("http request should read");
            assert!(read > 0, "http request should include headers");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = find_header_end(&buffer) {
                break index + 4;
            }
        };

        let header_text =
            String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().expect("request line should exist").to_string();
        let mut headers = BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once(':')
                .expect("header should contain separator");
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }

        let content_length = headers
            .get("content-length")
            .expect("content-length should exist")
            .parse::<usize>()
            .expect("content-length should parse");
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).expect("http body should read");
            assert!(read > 0, "http request should include body");
            buffer.extend_from_slice(&chunk[..read]);
        }

        let body = String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
            .expect("request body should be utf-8");
        RecordedHttpRequest {
            request_line,
            headers,
            body,
        }
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn status_reason(status: u16) -> &'static str {
        match status {
            200 => "OK",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            _ => "Test Response",
        }
    }

    #[tokio::test]
    async fn router_session_action_auto_extraction_is_consumed_on_followup_dispatch() {
        let state_root = temp_state_root("router-session-action");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
        let active_workspace_id = state
            .workspace_registry
            .active_workspace_id()
            .expect("bootstrap workspace should exist");
        if state
            .session_store
            .session(&magi_core::SessionId::new("test-session-001"))
            .is_none()
        {
            state
                .session_store
                .create_session_for_workspace(
                    magi_core::SessionId::new("test-session-001"),
                    "runtime session".to_string(),
                    Some(active_workspace_id.to_string()),
                )
                .expect("runtime session should be creatable");
        }

        let (status, first_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "test-session-001",
                "text": "remember parser constraint",
                "skillName": "refactor",
                "images": [],
                "workspaceId": active_workspace_id.to_string(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {first_body:?}");

        let first_accepted_at = first_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let first_mission_id = format!("mission-session-action-{first_accepted_at}");
        let first_root_task_id = first_body["rootTaskId"]
            .as_str()
            .expect("root_task_id should serialize as string");
        let read_model = get_json(app.clone(), "/runtime/read-model").await;
        let first_execution_group = read_model["details"]["execution_groups"]
            .as_array()
            .expect("execution groups should be an array")
            .iter()
            .find(|entry| entry["mission_id"] == first_mission_id)
            .expect("first execution group should exist");
        assert_eq!(first_execution_group["context_used_memory_count"], 0);
        assert_eq!(
            first_execution_group["context_memory_extraction_refs"]
                .as_array()
                .expect("refs should be array")
                .len(),
            0
        );
        let first_projection = wait_for_task_projection_completed(
            app.clone(),
            first_root_task_id,
            "test-session-001",
            active_workspace_id.as_str(),
        )
        .await;
        assert_completed_two_task_projection(&first_projection);

        let (status, second_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "test-session-001",
                "text": "follow up parser work",
                "skillName": "refactor",
                "images": [],
                "workspaceId": active_workspace_id.to_string(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {second_body:?}");

        let _second_accepted_at = second_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        // session 一生一 mission：第二次派发复用第一次派发创建的 mission_id
        let second_mission_id = format!("mission-session-action-{first_accepted_at}");
        let second_root_task_id = second_body["rootTaskId"]
            .as_str()
            .expect("root_task_id should serialize as string");
        let expected_extraction_id = format!("extract-session-action-{first_accepted_at}");

        let second_execution_group =
            wait_for_execution_group(app.clone(), &second_mission_id, |entry| {
                entry["context_memory_extraction_refs"] == json!([expected_extraction_id])
            })
            .await;
        assert_eq!(second_execution_group["context_used_memory_count"], 1);
        assert_eq!(second_execution_group["context_extracted_memory_count"], 1);
        assert_eq!(
            second_execution_group["context_memory_extraction_refs"],
            json!([expected_extraction_id])
        );
        let second_projection = wait_for_task_projection_completed(
            app,
            second_root_task_id,
            "test-session-001",
            active_workspace_id.as_str(),
        )
        .await;
        assert_completed_two_task_projection(&second_projection);
    }

    #[tokio::test]
    async fn router_regular_session_turn_uses_daemon_session_turn_dispatcher() {
        let state_root = temp_state_root("router-regular-session-turn");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
        let active_workspace_id = state
            .workspace_registry
            .active_workspace_id()
            .expect("bootstrap workspace should exist");
        let session_id = magi_core::SessionId::new("test-session-chat");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "runtime chat session".to_string(),
                Some(active_workspace_id.to_string()),
            )
            .expect("runtime session should be creatable");

        let (status, body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": session_id.to_string(),
                "text": "这是一条普通对话",
                "skillName": null,
                "images": [],
                "workspaceId": active_workspace_id.to_string(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body:?}");
        assert_eq!(body["route"], "chat");
        assert!(body.get("rootTaskId").is_none());

        let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
        loop {
            let read_model = get_json(app.clone(), "/runtime/read-model").await;
            let session = read_model["details"]["sessions"]
                .as_array()
                .and_then(|sessions| {
                    sessions
                        .iter()
                        .find(|entry| entry["session_id"] == session_id.to_string())
                })
                .expect("session should appear in read model");
            let status = session["current_turn"]["status"].as_str().unwrap_or("");
            if status == "completed" {
                assert!(
                    session["turn_items"]
                        .as_array()
                        .expect("turn items should be an array")
                        .iter()
                        .any(|item| item["kind"] == "assistant_final"),
                    "regular chat turn should produce assistant_final item: {session:?}"
                );
                assert!(
                    read_model["details"]["tasks"]
                        .as_array()
                        .expect("tasks should be an array")
                        .is_empty(),
                    "regular chat turn must not create task projection"
                );
                break;
            }
            assert_ne!(
                status, "failed",
                "regular chat turn should not fail: {session:?}"
            );
            if Instant::now() >= deadline {
                panic!("regular chat turn did not complete before timeout: {session:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn router_recovery_resume_writeback_is_consumed_on_followup_dispatch() {
        let state_root = temp_state_root("router-recovery-resume");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
        let active_workspace_id = state
            .workspace_registry
            .active_workspace_id()
            .expect("bootstrap workspace should exist");
        if state
            .session_store
            .session(&magi_core::SessionId::new("test-session-001"))
            .is_none()
        {
            state
                .session_store
                .create_session_for_workspace(
                    magi_core::SessionId::new("test-session-001"),
                    "runtime recovery session".to_string(),
                    Some(active_workspace_id.to_string()),
                )
                .expect("runtime recovery session should be creatable");
        }

        let (status, seed_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "test-session-001",
                "text": "seed recovery route state",
                "skillName": "refactor",
                "images": [],
                "workspaceId": active_workspace_id.to_string(),
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected seed body: {seed_body:?}"
        );
        // session 一生一 mission：后续 followup dispatch 复用 seed 的 mission_id
        let seed_accepted_at = seed_body["acceptedAt"]
            .as_u64()
            .expect("seed accepted_at should serialize as integer");

        let session_id = magi_core::SessionId::new("test-session-001");
        let ownership = runtime
            .session_store
            .execution_ownership(&session_id)
            .expect("seed session action should bind execution ownership");
        let workspace_id = ownership
            .workspace_id
            .clone()
            .expect("seed execution ownership should include workspace");
        let recovery_task_id = ownership
            .task_id
            .clone()
            .expect("seed execution ownership should include task");
        state
            .task_store()
            .expect("task store should be configured")
            .update_status(&recovery_task_id, TaskStatus::Failed)
            .expect("seed task should become recoverable");
        runtime
            .session_store
            .cancel_current_turn(&session_id)
            .expect("seed current turn should cancel for recovery");
        let snapshot = runtime.workspace_store.append_execution_snapshot(
            workspace_id.clone(),
            ownership.clone(),
            "snapshot-daemon-recovery-route",
            "Daemon recovery snapshot",
        );
        let recovery = runtime.workspace_store.prepare_recovery_entry(
            workspace_id.clone(),
            ownership,
            snapshot.snapshot_id,
            "recovery-daemon-route",
            Some("resume daemon route followup".to_string()),
        );
        runtime
            .workspace_store
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        runtime
            .session_store
            .attach_recovery_ref(&session_id, Some(recovery.recovery_id.clone()))
            .expect("recovery ref should attach to session");

        let (status, recovery_body) = post_json(
            app.clone(),
            "/api/session/continue",
            json!({
                "sessionId": session_id.to_string(),
                "workspaceId": workspace_id.to_string(),
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected recovery body: {recovery_body:?}"
        );
        assert_eq!(recovery_body["sessionId"], session_id.to_string());
        assert_eq!(recovery_body["status"], "continued");

        let (status, followup_body) = post_json(
            app.clone(),
            "/api/session/turn",
            json!({
                "sessionId": "test-session-001",
                "text": "consume recovery memory",
                "skillName": "refactor",
                "images": [],
                "workspaceId": workspace_id.to_string(),
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected followup body: {followup_body:?}"
        );

        let _followup_accepted_at = followup_body["acceptedAt"]
            .as_u64()
            .expect("accepted_at should serialize as integer");
        let followup_mission_id = format!("mission-session-action-{seed_accepted_at}");
        let expected_extraction_id = "extract-session-continue-recovery-daemon-route";
        let followup_execution_group =
            wait_for_execution_group(app, &followup_mission_id, |entry| {
                entry["context_memory_extraction_refs"]
                    .as_array()
                    .is_some_and(|refs| refs.iter().any(|value| value == expected_extraction_id))
            })
            .await;
        let extraction_refs = followup_execution_group["context_memory_extraction_refs"]
            .as_array()
            .expect("refs should be array");
        assert!(
            extraction_refs
                .iter()
                .any(|value| value == expected_extraction_id),
            "followup execution group should consume recovery extraction, got {extraction_refs:?}"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_services_exports_loopback_model_host_and_mcp_catalogs() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-services");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let app = runtime.router("daemon-test".to_string());

        let snapshot = get_json(app, "/bridges/services").await;
        let services = service_entries_by_kind(&snapshot);
        assert_eq!(
            services.len(),
            2,
            "unexpected bridge snapshot: {snapshot:?}"
        );

        let model = services
            .get("model")
            .expect("model bridge snapshot should exist");
        assert_eq!(model["health"]["status"], "ok");
        assert_eq!(model["health"]["ok"], true);
        assert!(
            model["service_catalog"]["services"]
                .as_array()
                .expect("model services should be an array")
                .iter()
                .any(|service| service["service_name"] == "loopback-model"),
            "model catalog should include loopback-model: {model:?}"
        );
        assert!(
            model["service_catalog"]["services"]
                .as_array()
                .expect("model services should be an array")
                .iter()
                .any(|service| service["service_name"] == "openai-compatible"),
            "model catalog should include openai-compatible: {model:?}"
        );

        let mcp = services
            .get("mcp")
            .expect("mcp bridge snapshot should exist");
        assert_eq!(mcp["health"]["status"], "ok");
        assert_eq!(mcp["health"]["ok"], true);
        assert_eq!(
            mcp["service_catalog"]["services"][0]["service_name"],
            "loopback-mcp-manager"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_preflight_executes_loopback_model_and_mcp_smokes() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-preflight");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let app = runtime.router("daemon-test".to_string());
        let services_snapshot = get_json(app.clone(), "/bridges/services").await;

        let snapshot = get_json(app, "/bridges/preflight").await;
        let services = service_entries_by_kind(&snapshot);
        assert_eq!(
            services.len(),
            2,
            "unexpected bridge preflight: {snapshot:?}"
        );

        let model = services.get("model").expect("model preflight should exist");
        assert!(
            model["checks"]
                .as_array()
                .expect("model checks should be an array")
                .iter()
                .any(|check| check["target"] == "loopback-model" && check["ok"] == true),
            "model preflight should include loopback-model invoke: {model:?}"
        );
        let openai_ready = services_snapshot["services"]
            .as_array()
            .expect("bridge services should be an array")
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .and_then(|entry| entry["service_catalog"]["services"].as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|service| service["service_name"] == "openai-compatible")
            })
            .and_then(|service| service["service_health"].as_str())
            == Some("ready");
        if openai_ready {
            assert!(
                model["checks"]
                    .as_array()
                    .expect("model checks should be an array")
                    .iter()
                    .any(|check| check["target"] == "openai-compatible" && check["ok"] == true),
                "ready openai-compatible provider should execute preflight smoke: {model:?}"
            );
        } else {
            assert!(
                model["checks"]
                    .as_array()
                    .expect("model checks should be an array")
                    .iter()
                    .all(|check| check["target"] != "openai-compatible"),
                "non-ready openai-compatible provider should not execute preflight smoke: {model:?}"
            );
        }

        let mcp = services.get("mcp").expect("mcp preflight should exist");
        assert!(
            mcp["checks"]
                .as_array()
                .expect("mcp checks should be an array")
                .iter()
                .any(|check| check["check_name"] == "list_servers" && check["ok"] == true),
            "mcp preflight should include manager list_servers: {mcp:?}"
        );
        assert!(
            mcp["checks"]
                .as_array()
                .expect("mcp checks should be an array")
                .iter()
                .any(|check| check["target"] == "loopback-mcp.echo.inspect" && check["ok"] == true),
            "mcp preflight should include loopback-mcp echo.inspect: {mcp:?}"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_exports_contract_snapshots() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-cutover");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let app = runtime.router("daemon-test".to_string());
        let services_snapshot = get_json(app.clone(), "/bridges/services").await;
        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);
        assert_eq!(
            services.len(),
            2,
            "unexpected bridge cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["checked_service_count"], 2);
        let blocking_issues = snapshot["blocking_issues"]
            .as_array()
            .expect("blocking issues should serialize as array");
        let failing_check_count = services
            .values()
            .flat_map(|service| {
                service["checks"]
                    .as_array()
                    .expect("checks should serialize as array")
                    .iter()
            })
            .filter(|check| check["ok"] != true)
            .count();
        assert_eq!(
            snapshot["blocking_check_count"]
                .as_u64()
                .expect("blocking count should serialize as u64") as usize,
            failing_check_count
        );
        assert_eq!(blocking_issues.len(), failing_check_count);
        let reason_code_counts = snapshot["blocking_issue_counts_by_reason_code"]
            .as_object()
            .expect("reason-code counts should serialize as object");
        let server_kind_counts = snapshot["blocking_issue_counts_by_server_kind"]
            .as_object()
            .expect("server-kind counts should serialize as object");

        let model = services.get("model").expect("model cutover should exist");
        let model_service_ok = model["service_ok"]
            .as_bool()
            .expect("model service_ok should serialize as bool");
        assert!(
            model["checks"]
                .as_array()
                .expect("model checks should be an array")
                .iter()
                .any(|check| check["target"] == "loopback-model" && check["ok"] == true),
            "loopback-model cutover contract should always be present: {model:?}"
        );
        let openai_health = services_snapshot["services"]
            .as_array()
            .expect("bridge services should be an array")
            .iter()
            .find(|entry| entry["server_kind"] == "model")
            .and_then(|entry| entry["service_catalog"]["services"].as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|service| service["service_name"] == "openai-compatible")
            })
            .and_then(|service| service["service_health"].as_str());
        if let Some(openai_health) = openai_health {
            let openai = model["checks"]
                .as_array()
                .expect("model checks should be an array")
                .iter()
                .find(|check| check["target"] == "openai-compatible")
                .expect("cataloged openai-compatible cutover check should exist");
            if openai_health == "ready" {
                assert!(model_service_ok);
                assert_eq!(openai["ok"], true);
                assert_eq!(openai["model_contract"]["contract_ok"], true);
            } else {
                assert!(!model_service_ok);
                assert_eq!(openai["ok"], false);
                assert_eq!(openai["blocking_reason"], "bridge invocation failed");
                assert_eq!(openai["error"]["layer"], "RemoteBusiness");
            }
        }

        let mcp = services.get("mcp").expect("mcp cutover should exist");
        let mcp_route_gate = &mcp["mcp_default_route_gate"];
        let route_status = mcp_route_gate["route_status"]
            .as_str()
            .expect("route status should serialize as string");
        match route_status {
            "ready" => {
                assert_eq!(mcp["service_ok"], true);
                assert_eq!(mcp["blocking_check_count"], 0);
                assert_eq!(mcp_route_gate["contract_ok"], true);
                assert_eq!(mcp["checks"][0]["ok"], true);
                assert_eq!(
                    mcp_route_gate["route_status"],
                    mcp["checks"][0]["mcp_contract"]["route_status"]
                );
                assert_eq!(
                    mcp_route_gate["route_target"],
                    mcp["checks"][0]["mcp_contract"]["route_target"]
                );
                assert_eq!(
                    mcp_route_gate["resolved_server"],
                    mcp["checks"][0]["mcp_contract"]["resolved_server"]
                );
                assert_eq!(
                    mcp_route_gate["contract_ok"],
                    mcp["checks"][0]["mcp_contract"]["contract_ok"]
                );
                assert_eq!(
                    mcp_route_gate["resolved_server"],
                    mcp_route_gate["route_target"]
                );
                if model_service_ok {
                    assert_eq!(snapshot["overall_ok"], true);
                    assert_eq!(snapshot["blocking_check_count"], 0);
                    assert!(
                        reason_code_counts.is_empty(),
                        "ready snapshot should not export reason-code counts: {snapshot:?}"
                    );
                    assert!(
                        server_kind_counts.is_empty(),
                        "ready snapshot should not export server-kind counts: {snapshot:?}"
                    );
                    assert!(
                        snapshot["blocking_services"]
                            .as_array()
                            .expect("blocking services should serialize as array")
                            .is_empty()
                    );
                    assert!(blocking_issues.is_empty());
                } else {
                    assert_eq!(snapshot["overall_ok"], false);
                    assert!(
                        snapshot["blocking_check_count"]
                            .as_u64()
                            .expect("blocking count should serialize as u64")
                            >= 1
                    );
                    assert_eq!(
                        server_kind_counts
                            .get("model")
                            .and_then(|value| value.as_u64()),
                        Some(1)
                    );
                    assert!(
                        snapshot["blocking_services"]
                            .as_array()
                            .expect("blocking services should serialize as array")
                            .iter()
                            .any(|service| service == "model"),
                        "blocking summary should include model: {snapshot:?}"
                    );
                }
            }
            "fallback-only" | "unavailable" => {
                assert_eq!(snapshot["overall_ok"], false);
                assert!(
                    snapshot["blocking_check_count"]
                        .as_u64()
                        .expect("blocking count should serialize as u64")
                        >= 1
                );
                assert_eq!(mcp["service_ok"], false);
                assert!(
                    mcp["blocking_check_count"]
                        .as_u64()
                        .expect("service blocking count should serialize as u64")
                        >= 1
                );
                assert_eq!(blocking_issues.len(), 1);
                assert_eq!(mcp_route_gate["contract_ok"], false);
                assert_eq!(
                    blocking_issues[0]["server_kind"],
                    serde_json::Value::String("mcp".to_string())
                );
                assert_eq!(
                    blocking_issues[0]["reason_code"],
                    serde_json::Value::String(
                        if route_status == "fallback-only" {
                            "mcp_default_route_status_fallback_only"
                        } else {
                            "mcp_default_route_status_unavailable"
                        }
                        .to_string()
                    )
                );
                assert_eq!(
                    reason_code_counts
                        .get(if route_status == "fallback-only" {
                            "mcp_default_route_status_fallback_only"
                        } else {
                            "mcp_default_route_status_unavailable"
                        })
                        .and_then(|value| value.as_u64()),
                    Some(1)
                );
                assert_eq!(
                    server_kind_counts
                        .get("mcp")
                        .and_then(|value| value.as_u64()),
                    Some(1)
                );
                assert_eq!(mcp["checks"][0]["ok"], false);
                assert!(
                    mcp["checks"][0]["blocking_reason"].is_string(),
                    "blocking route should explain itself: {mcp:?}"
                );
                assert!(
                    blocking_issues.iter().any(|issue| {
                        issue["server_kind"] == serde_json::Value::String("mcp".to_string())
                            && issue["reason_code"]
                                == serde_json::Value::String(
                                    if route_status == "fallback-only" {
                                        "mcp_default_route_status_fallback_only"
                                    } else {
                                        "mcp_default_route_status_unavailable"
                                    }
                                    .to_string(),
                                )
                    }),
                    "blocking issues should retain the mcp route failure: {snapshot:?}"
                );
                assert!(
                    snapshot["blocking_services"]
                        .as_array()
                        .expect("blocking services should serialize as array")
                        .iter()
                        .any(|service| service == "mcp"),
                    "blocking summary should include mcp: {snapshot:?}"
                );
            }
            other => panic!("unexpected mcp default route status {other}: {mcp:?}"),
        }
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_reflects_env_backed_provider_and_mcp_routes() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {
                        "content": "hello from env-backed cutover smoke"
                    }
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=healthy,loopback-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], true,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issue_counts_by_reason_code"]
                .as_object()
                .expect("reason-code counts should serialize as object")
                .is_empty(),
            "ready env-backed snapshot should not export reason-code counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .as_object()
                .expect("server-kind counts should serialize as object")
                .is_empty(),
            "ready env-backed snapshot should not export server-kind counts: {snapshot:?}"
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(openai["ok"], true);
        assert_eq!(openai["model_contract"]["contract_ok"], true);

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-key")
        );
        let body: Value = serde_json::from_str(&request.body).expect("request body should be json");
        assert_eq!(body["model"], "gpt-test");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_settings_bootstrap_exposes_env_seeded_orchestrator_config() {
        let state_root = temp_state_root("router-settings-bootstrap-env-model");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", "http://127.0.0.1:8317/v1"),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
            ],
        );

        let bootstrap = get_json(app, "/api/settings/bootstrap?scope=core").await;

        assert_eq!(
            bootstrap["orchestratorConfig"]["baseUrl"],
            json!("http://127.0.0.1:8317/v1")
        );
        assert_eq!(bootstrap["orchestratorConfig"]["apiKey"], json!("test-key"));
        assert_eq!(bootstrap["orchestratorConfig"]["model"], json!("gpt-test"));
        assert_eq!(
            state.settings_store.get_section("orchestrator")["model"],
            json!("gpt-test"),
            "环境模型必须在启动期进入 settings store，避免前端另走一条配置读取链路"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_failure_with_ready_mcp_route()
     {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            401,
            json!({
                "error": {
                    "message": "bad api key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-failure");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=healthy,loopback-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 2);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_rejected"],
            2
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 2);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(2)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32006);
        assert!(
            openai["error"]["message"]
                .as_str()
                .expect("model error should serialize as string")
                .contains("桥接服务返回失败状态"),
            "provider failure should expose public bridge error: {openai:?}"
        );

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        let _direct_http_request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive direct HTTP probe request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_cataloged_degraded_provider_with_ready_mcp_route()
     {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bridge-cutover-env-degraded-provider");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=healthy,loopback-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_unavailable"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 1);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("cataloged degraded openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32003);
        assert!(
            openai["error"]["message"]
                .as_str()
                .expect("model error should serialize as string")
                .contains("桥接服务返回失败状态"),
            "degraded provider should expose public bridge error: {openai:?}"
        );

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_invalid_response_with_ready_mcp_route()
     {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {}
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-invalid-response");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=healthy,loopback-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 2);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_invalid_response"],
            2
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 2);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(2)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32007);
        let error_message = openai["error"]["message"]
            .as_str()
            .expect("model error should serialize as string");
        assert!(
            error_message.contains("桥接服务返回失败状态"),
            "invalid provider response should expose public bridge error: {openai:?}"
        );

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        let _direct_http_request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive direct HTTP probe request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_fallback_only_route() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {
                        "content": "hello from env-backed cutover smoke"
                    }
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-fallback-only");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability,loopback-mcp",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=degraded,loopback-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["mcp_default_route_status_fallback_only"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["mcp"], 1);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("model")
                .is_none(),
            "ready provider route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "mcp"),
            "blocking summary should include mcp: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(1)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], true);
        assert_eq!(openai["ok"], true);
        assert_eq!(openai["model_contract"]["contract_ok"], true);

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], false);
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_status"],
            "fallback-only"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], false);
        assert_eq!(mcp["checks"][0]["ok"], false);
        assert_eq!(
            mcp["checks"][0]["blocking_reason"],
            "default route is fallback-only"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_unavailable_route() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let (base_url, receiver, handle) = spawn_http_stub(
            200,
            json!({
                "choices": [{
                    "message": {
                        "content": "hello from env-backed cutover smoke"
                    }
                }]
            }),
        );

        let state_root = temp_state_root("router-bridge-cutover-env-unavailable");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", "loopback-mcp"),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=unavailable",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unexpected cutover snapshot: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 1);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["mcp_default_route_status_unavailable"],
            1
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["mcp"], 1);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("model")
                .is_none(),
            "ready provider route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "mcp"),
            "blocking summary should include mcp: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(1)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], true);
        assert_eq!(openai["ok"], true);
        assert_eq!(openai["model_contract"]["contract_ok"], true);

        let request = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("http stub should receive env-backed provider request");
        handle.join().expect("http stub should join");
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], false);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "unavailable");
        assert_eq!(mcp["mcp_default_route_gate"]["route_target"], "<none>");
        assert!(mcp["mcp_default_route_gate"]["resolved_server"].is_null());
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], false);
        assert_eq!(mcp["checks"][0]["target"], "<none>");
        assert_eq!(mcp["checks"][0]["ok"], false);
        assert_eq!(
            mcp["checks"][0]["blocking_reason"],
            "default route is unavailable"
        );
    }

    #[tokio::test]
    async fn daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_transport_failure_with_ready_mcp_route()
     {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        // Bind a port, capture the address, then drop the listener so nothing
        // is listening — any connection attempt will be refused.
        let unreachable_address = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind should succeed");
            let address = listener.local_addr().expect("bound address should exist");
            drop(listener);
            address
        };
        let unreachable_url = format!("http://{unreachable_address}/v1");

        let state_root = temp_state_root("router-bridge-cutover-env-transport-failure");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, _) = runtime.router_with_bridge_env_for_tests(
            "daemon-test".to_string(),
            &[
                ("MAGI_OPENAI_COMPAT_BASE_URL", unreachable_url.as_str()),
                ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
                ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
                ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
                (
                    "MAGI_MCP_MANAGER_ENABLED_SERVERS",
                    "loopback-mcp-observability",
                ),
                ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
                (
                    "MAGI_MCP_MANAGER_SERVER_HEALTHS",
                    "loopback-mcp-observability=healthy,loopback-mcp=healthy",
                ),
            ],
        );

        let snapshot = get_json(app, "/bridges/cutover-smoke").await;
        let services = service_entries_by_kind(&snapshot);

        assert_eq!(
            snapshot["overall_ok"], false,
            "unreachable provider should block cutover: {snapshot:?}"
        );
        assert_eq!(snapshot["blocking_check_count"], 2);
        assert_eq!(
            snapshot["blocking_issue_counts_by_reason_code"]["model_provider_transport_failed"],
            2
        );
        assert_eq!(snapshot["blocking_issue_counts_by_server_kind"]["model"], 2);
        assert!(
            snapshot["blocking_issue_counts_by_server_kind"]
                .get("mcp")
                .is_none(),
            "ready MCP route should not contribute blocking server-kind counts: {snapshot:?}"
        );
        assert!(
            snapshot["blocking_services"]
                .as_array()
                .expect("blocking services should serialize as array")
                .iter()
                .any(|service| service == "model"),
            "blocking summary should include model: {snapshot:?}"
        );
        assert_eq!(
            snapshot["blocking_issues"].as_array().map(Vec::len),
            Some(2)
        );

        let model = services
            .get("model")
            .expect("model cutover snapshot should exist");
        let openai = model["checks"]
            .as_array()
            .expect("model checks should serialize as array")
            .iter()
            .find(|check| check["target"] == "openai-compatible")
            .expect("env-backed openai-compatible cutover check should exist");
        assert_eq!(model["service_ok"], false);
        assert_eq!(openai["ok"], false);
        assert_eq!(openai["blocking_reason"], "bridge invocation failed");
        assert_eq!(openai["error"]["layer"], "RemoteBusiness");
        assert_eq!(openai["error"]["code"], -32005);
        let error_message = openai["error"]["message"]
            .as_str()
            .expect("model error should serialize as string");
        assert!(
            error_message.contains("桥接服务返回失败状态"),
            "transport failure should expose public bridge error: {openai:?}"
        );

        let mcp = services
            .get("mcp")
            .expect("mcp cutover snapshot should exist");
        assert_eq!(mcp["service_ok"], true);
        assert_eq!(mcp["mcp_default_route_gate"]["route_status"], "ready");
        assert_eq!(
            mcp["mcp_default_route_gate"]["route_target"],
            "loopback-mcp-observability"
        );
        assert_eq!(
            mcp["mcp_default_route_gate"]["resolved_server"],
            "loopback-mcp-observability"
        );
        assert_eq!(mcp["mcp_default_route_gate"]["contract_ok"], true);
        assert_eq!(mcp["checks"][0]["ok"], true);
    }

    #[tokio::test]
    async fn daemon_router_bridge_routes_do_not_touch_execution_state() {
        let state_root = temp_state_root("router-bridge-guard");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());

        let before_runtime_read_model = serde_json::to_value(state.runtime_read_model_dto())
            .expect("runtime read model should serialize");
        let before_session_sidecars =
            serde_json::to_value(state.session_store.execution_sidecar_exports())
                .expect("session sidecars should serialize");
        let before_workspace_sidecars =
            serde_json::to_value(state.workspace_registry.recovery_sidecar_exports())
                .expect("workspace sidecars should serialize");

        let _ = get_json(app.clone(), "/bridges/preflight").await;
        let _ = get_json(app, "/bridges/cutover-smoke").await;

        assert_eq!(
            serde_json::to_value(state.runtime_read_model_dto())
                .expect("runtime read model should serialize"),
            before_runtime_read_model
        );
        assert_eq!(
            serde_json::to_value(state.session_store.execution_sidecar_exports())
                .expect("session sidecars should serialize"),
            before_session_sidecars
        );
        assert_eq!(
            serde_json::to_value(state.workspace_registry.recovery_sidecar_exports())
                .expect("workspace sidecars should serialize"),
            before_workspace_sidecars
        );

        assert!(
            state
                .execution_pipeline()
                .expect("execution pipeline should exist")
                .memory_store
                .extraction_results_for_session(&magi_core::SessionId::new("bridge-route-guard"))
                .is_empty()
        );
    }

    #[tokio::test]
    async fn daemon_bootstrap_exports_bridge_services_and_preflight_snapshots() {
        for binary_name in ["model_bridge_loopback", "mcp_bridge_loopback"] {
            let path = test_bridge_binary_path(binary_name);
            assert!(
                path.exists(),
                "expected loopback binary {binary_name} at {}",
                path.display()
            );
        }

        let state_root = temp_state_root("router-bootstrap-bridges");
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
        let runtime =
            DaemonRuntime::restore(&config).expect("runtime restore should bootstrap empty state");
        let app = runtime.router("daemon-test".to_string());

        let bootstrap = get_json(app.clone(), "/bootstrap").await;
        let bridge_services = get_json(app.clone(), "/bridges/services").await;
        let bridge_preflight = get_json(app, "/bridges/preflight").await;

        assert_eq!(bootstrap["bridgeServices"], bridge_services);
        assert_eq!(bootstrap["bridgePreflight"], bridge_preflight);

        let services = service_entries_by_kind(&bootstrap["bridgeServices"]);
        assert_eq!(
            services.len(),
            2,
            "unexpected bootstrap bridge services: {bootstrap:?}"
        );

        let preflight = service_entries_by_kind(&bootstrap["bridgePreflight"]);
        assert_eq!(
            preflight.len(),
            2,
            "unexpected bootstrap bridge preflight: {bootstrap:?}"
        );
    }
}
