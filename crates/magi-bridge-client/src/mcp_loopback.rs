use crate::{
    BridgeResponse, LOOPBACK_MCP_SERVER_NAME, LOOPBACK_MCP_TOOL_NAME,
    McpManagerDescribeServerResponse, McpManagerLifecycleEvent as McpLifecycleEvent,
    McpManagerLifecycleEventKind as McpLifecycleEventKind, McpManagerListServersResponse,
    McpManagerServerHealthUpdateRequest, McpManagerServerLifecycleState as McpServerLifecycleState,
    McpManagerServerOperationResponse, McpManagerServerRegistrationRequest,
    McpManagerServerSelectionRequest, McpToolCallRequest,
    local_process_protocol::{
        BridgeServerCommandCapabilityProfile, BridgeServerContextResolutionBoundary,
        BridgeServerKind, BridgeServerServiceCatalog, BridgeServerServiceDescriptor,
        LOCAL_BRIDGE_PROTOCOL_VERSION, LocalProcessBridgeRequest, LocalProcessBridgeRpcError,
        LocalProcessBridgeServerError, run_local_process_bridge_server_with_methods,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::env;

const MCP_MANAGER_DEFAULT_SERVER_ENV: &str = "MAGI_MCP_MANAGER_DEFAULT_SERVER";
const MCP_MANAGER_ENABLED_SERVERS_ENV: &str = "MAGI_MCP_MANAGER_ENABLED_SERVERS";
const MCP_MANAGER_DISABLED_SERVERS_ENV: &str = "MAGI_MCP_MANAGER_DISABLED_SERVERS";
const MCP_MANAGER_SERVER_HEALTHS_ENV: &str = "MAGI_MCP_MANAGER_SERVER_HEALTHS";
const MCP_CALL_TOOL_METHOD: &str = "mcp.call_tool";
const MCP_LIST_SERVERS_METHOD: &str = "mcp.list_servers";
const MCP_DESCRIBE_SERVER_METHOD: &str = "mcp.describe_server";
const MCP_ENABLE_SERVER_METHOD: &str = "mcp.enable_server";
const MCP_DISABLE_SERVER_METHOD: &str = "mcp.disable_server";
const MCP_REGISTER_SERVER_METHOD: &str = "mcp.register_server";
const MCP_START_SERVER_METHOD: &str = "mcp.start_server";
const MCP_STOP_SERVER_METHOD: &str = "mcp.stop_server";
const MCP_DEREGISTER_SERVER_METHOD: &str = "mcp.deregister_server";
const MCP_UPDATE_HEALTH_METHOD: &str = "mcp.update_health";
const MCP_MANAGER_METHODS: &[&str] = &[
    MCP_CALL_TOOL_METHOD,
    MCP_LIST_SERVERS_METHOD,
    MCP_DESCRIBE_SERVER_METHOD,
    MCP_ENABLE_SERVER_METHOD,
    MCP_DISABLE_SERVER_METHOD,
    MCP_REGISTER_SERVER_METHOD,
    MCP_START_SERVER_METHOD,
    MCP_STOP_SERVER_METHOD,
    MCP_DEREGISTER_SERVER_METHOD,
    MCP_UPDATE_HEALTH_METHOD,
];

pub fn run_mcp_manager_server() -> Result<(), LocalProcessBridgeServerError> {
    let registry = McpServerRegistry::loopback();
    run_local_process_bridge_server_with_methods(
        BridgeServerKind::Mcp,
        MCP_MANAGER_METHODS,
        registry.service_catalog(),
        handle_mcp_request,
    )
}

pub fn run_mcp_bridge_loopback_server() -> Result<(), LocalProcessBridgeServerError> {
    run_mcp_manager_server()
}

#[derive(Clone, Debug)]
struct McpServerRegistry {
    manager_name: &'static str,
    manager_version: &'static str,
    registry_profile: &'static str,
    selection_strategy: &'static str,
    default_server_preference: Option<String>,
    servers: Vec<McpServerDescriptor>,
    config_issues: Vec<String>,
    lifecycle_events: Vec<McpLifecycleEvent>,
}

#[derive(Clone, Debug)]
struct McpServerDescriptor {
    server_name: &'static str,
    server_version: &'static str,
    implementation_source: &'static str,
    capability_profile: &'static str,
    selection_key: &'static str,
    enabled: bool,
    health_status: &'static str,
    lifecycle_state: McpServerLifecycleState,
    tools: Vec<McpToolShim>,
}

#[derive(Clone, Debug)]
struct McpToolShim {
    tool_name: &'static str,
}

impl McpToolShim {
    fn inspect() -> Self {
        Self {
            tool_name: LOOPBACK_MCP_TOOL_NAME,
        }
    }

    fn describe() -> Self {
        Self {
            tool_name: "echo.describe",
        }
    }

    fn dynamic(tool_name: String) -> Self {
        Self {
            tool_name: leak_string(tool_name),
        }
    }

    fn tool_name(&self) -> &'static str {
        self.tool_name
    }
}

fn parse_list_env(key: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_server_health(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "healthy" => Some("healthy"),
        "degraded" => Some("degraded"),
        "unavailable" => Some("unavailable"),
        "disabled" => Some("disabled"),
        _ => None,
    }
}

fn lifecycle_state_for_health(enabled: bool, health_status: &str) -> McpServerLifecycleState {
    match (enabled, health_status) {
        (_, "unavailable") => McpServerLifecycleState::Failed,
        (_, "disabled") => McpServerLifecycleState::Stopped,
        (true, _) => McpServerLifecycleState::Running,
        (false, _) => McpServerLifecycleState::Registered,
    }
}

fn leak_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

impl McpManagerServerRegistrationRequest {
    fn into_descriptor(self) -> Result<McpServerDescriptor, LocalProcessBridgeRpcError> {
        let health_status = parse_server_health(&self.health_status).ok_or_else(|| {
            LocalProcessBridgeRpcError::invalid_params(format!(
                "unsupported health status {}",
                self.health_status
            ))
        })?;
        let enabled = self.enabled && health_status != "disabled";
        let lifecycle_state = lifecycle_state_for_health(enabled, health_status);

        Ok(McpServerDescriptor {
            server_name: leak_string(self.server_name),
            server_version: leak_string(self.server_version),
            implementation_source: leak_string(self.implementation_source),
            capability_profile: leak_string(self.capability_profile),
            selection_key: leak_string(self.selection_key),
            enabled,
            health_status,
            lifecycle_state,
            tools: self
                .tool_names
                .into_iter()
                .map(McpToolShim::dynamic)
                .collect(),
        })
    }
}

impl McpServerDescriptor {
    fn enabled(
        server_name: &'static str,
        server_version: &'static str,
        capability_profile: &'static str,
        selection_key: &'static str,
        health_status: &'static str,
        tools: Vec<McpToolShim>,
    ) -> Self {
        Self {
            server_name,
            server_version,
            implementation_source: "loopback-server-prehost",
            capability_profile,
            selection_key,
            enabled: true,
            health_status,
            lifecycle_state: McpServerLifecycleState::Running,
            tools,
        }
    }

    fn disabled(
        server_name: &'static str,
        server_version: &'static str,
        capability_profile: &'static str,
        selection_key: &'static str,
        health_status: &'static str,
        tools: Vec<McpToolShim>,
    ) -> Self {
        Self {
            server_name,
            server_version,
            implementation_source: "loopback-server-prehost",
            capability_profile,
            selection_key,
            enabled: false,
            health_status,
            lifecycle_state: McpServerLifecycleState::Stopped,
            tools,
        }
    }

    fn manifest_name(&self) -> String {
        format!("{}@{}", self.server_name, self.server_version)
    }

    fn matches_requested_server(&self, requested_server: &str) -> bool {
        let requested_server = requested_server.trim();
        requested_server == self.server_name || requested_server == self.selection_key
    }

    fn service_descriptor(&self) -> BridgeServerServiceDescriptor {
        let mut capabilities = vec![
            format!("server:{}", self.server_name),
            format!("server_version:{}", self.server_version),
            format!("server_manifest:{}", self.manifest_name()),
            format!("implementation_source:{}", self.implementation_source),
            format!("capability_profile:{}", self.capability_profile),
            format!("selection_key:{}", self.selection_key),
            format!("server_enabled:{}", self.enabled),
            format!("server_health:{}", self.health_status),
            format!("lifecycle_state:{}", self.lifecycle_state),
            format!("tool_count:{}", self.tools.len()),
        ];
        capabilities.extend(
            self.tools
                .iter()
                .map(|tool| format!("tool:{}", tool.tool_name())),
        );

        BridgeServerServiceDescriptor {
            service_name: self.server_name.to_string(),
            shim_kind: "mcp-server-descriptor".to_string(),
            supported_operations: vec![
                "call_tool".to_string(),
                "list_tools".to_string(),
                "describe_server".to_string(),
            ],
            capabilities,
            service_health: Some(self.health_status.to_string()),
            service_health_reason: (!self.enabled).then(|| "server disabled".to_string()),
            implementation_source: Some(self.implementation_source.to_string()),
            capability_profile: Some(self.capability_profile.to_string()),
            workspace_roots_source: None,
            manager_version: None,
            registry_profile: None,
            registry_manifest: None,
            selection_strategy: None,
            default_server: None,
            default_server_health: None,
            default_server_selection_key: None,
            default_route_status: None,
            default_route_target: None,
            selection_targets: None,
            selection_key: Some(self.selection_key.to_string()),
            server_manifest: Some(self.manifest_name()),
            shell_manifest: None,
            shell_profile: None,
            command_capability_profiles: Some(self.command_capability_profiles()),
            session_descriptor: None,
            workspace_context: None,
            context_resolution_boundary: Some(self.context_resolution_boundary()),
        }
    }

    fn command_capability_profiles(&self) -> Vec<BridgeServerCommandCapabilityProfile> {
        self.tools
            .iter()
            .map(|tool| BridgeServerCommandCapabilityProfile {
                command_name: tool.tool_name().to_string(),
                capability_id: format!("mcp-tool-{}-{}", self.server_name, tool.tool_name()),
                interaction_mode: "request-response".to_string(),
                side_effect_level: if tool.tool_name() == LOOPBACK_MCP_TOOL_NAME {
                    "read".to_string()
                } else {
                    "read-derive".to_string()
                },
                requires_session_context: false,
                requires_workspace_context: false,
                path_argument_policy: "opaque-input".to_string(),
            })
            .collect()
    }

    fn context_resolution_boundary(&self) -> BridgeServerContextResolutionBoundary {
        BridgeServerContextResolutionBoundary {
            request_binding: "explicit-server-selection".to_string(),
            session_resolution_strategy: "none".to_string(),
            workspace_resolution_strategy: "none".to_string(),
            session_resolution_source: "mcp.call_tool.server_name".to_string(),
            workspace_resolution_source: "mcp.call_tool.server_name".to_string(),
        }
    }

    fn supports_tool(&self, tool_name: &str) -> bool {
        self.tools.iter().any(|tool| tool.tool_name == tool_name)
    }

    fn is_routable(&self) -> bool {
        self.enabled && self.health_status != "disabled" && self.health_status != "unavailable"
    }

    fn tool_names(&self) -> Vec<&'static str> {
        self.tools.iter().map(McpToolShim::tool_name).collect()
    }

    fn execute(
        &self,
        call: &McpToolCallRequest,
        registry: &McpServerRegistry,
    ) -> Result<BridgeResponse, LocalProcessBridgeRpcError> {
        let requested_server = if call.server_name.trim().is_empty() {
            self.server_name.to_string()
        } else {
            call.server_name.trim().to_string()
        };
        if !self.matches_requested_server(&requested_server) {
            return Err(LocalProcessBridgeRpcError::remote_business(
                -32011,
                "unknown server",
                Some(json!({
                    "server_name": call.server_name,
                    "supported_server": self.server_name,
                    "selection_key": self.selection_key,
                })),
            ));
        }

        if !self.enabled {
            return Err(LocalProcessBridgeRpcError::remote_business(
                -32014,
                "server disabled",
                Some(json!({
                    "server_name": self.server_name,
                    "health_status": self.health_status,
                })),
            ));
        }
        if self.health_status == "unavailable" {
            return Err(LocalProcessBridgeRpcError::remote_business(
                -32016,
                "server unavailable",
                Some(json!({
                    "server_name": self.server_name,
                    "health_status": self.health_status,
                    "lifecycle_state": self.lifecycle_state,
                })),
            ));
        }

        let tool_name = call.tool_name.trim().to_string();
        let known_tools = self.tool_names();
        if !self.supports_tool(&tool_name) {
            return Err(LocalProcessBridgeRpcError::remote_business(
                -32012,
                "unsupported tool",
                Some(json!({
                    "server_name": self.server_name,
                    "tool_name": call.tool_name,
                    "supported_tools": known_tools,
                })),
            ));
        }

        let payload = match tool_name.as_str() {
            "echo.inspect" => json!({
                "implementation_source": self.implementation_source,
                "server_name": self.server_name,
                "requested_server": requested_server,
                "server_version": self.server_version,
                "server_manifest": self.manifest_name(),
                "capability_profile": self.capability_profile,
                "selection_key": self.selection_key,
                "manager_version": registry.manager_version,
                "registry_profile": registry.registry_profile,
                "registry_manifest": registry.registry_manifest(),
                "selection_strategy": registry.selection_strategy,
                "default_server": registry.default_server_name(),
                "default_route_status": registry.default_route_status(),
                "default_route_target": registry.default_route_target(),
                "manager_health": registry.manager_service_health(),
                "manager_health_reason": registry.manager_service_health_reason(),
                "tool_name": tool_name,
                "status": "ok",
                "input": call.input,
            }),
            "echo.describe" => json!({
                "implementation_source": self.implementation_source,
                "server_name": self.server_name,
                "requested_server": requested_server,
                "server_version": self.server_version,
                "server_manifest": self.manifest_name(),
                "capability_profile": self.capability_profile,
                "selection_key": self.selection_key,
                "manager_version": registry.manager_version,
                "registry_profile": registry.registry_profile,
                "registry_manifest": registry.registry_manifest(),
                "selection_strategy": registry.selection_strategy,
                "default_server": registry.default_server_name(),
                "default_route_status": registry.default_route_status(),
                "default_route_target": registry.default_route_target(),
                "manager_health": registry.manager_service_health(),
                "manager_health_reason": registry.manager_service_health_reason(),
                "tool_name": tool_name,
                "status": "ok",
                "known_tools": known_tools,
                "tool_count": known_tools.len(),
            }),
            other => {
                return Err(LocalProcessBridgeRpcError::remote_business(
                    -32013,
                    "unsupported registry tool",
                    Some(json!({
                        "tool_name": other,
                    })),
                ));
            }
        };

        Ok(BridgeResponse {
            ok: true,
            payload: payload.to_string(),
        })
    }
}

impl McpServerRegistry {
    fn loopback() -> Self {
        let mut registry = Self {
            manager_name: "loopback-mcp-manager",
            manager_version: "1.0.0-loopback",
            registry_profile: "loopback-mcp-registry-v1",
            selection_strategy: "explicit-or-selection-key",
            default_server_preference: None,
            servers: vec![
                McpServerDescriptor::enabled(
                    LOOPBACK_MCP_SERVER_NAME,
                    "1.2.0-loopback",
                    "inspection-core-v1",
                    "inspection-default",
                    "healthy",
                    vec![McpToolShim::inspect(), McpToolShim::describe()],
                ),
                McpServerDescriptor::disabled(
                    "loopback-mcp-observability",
                    "0.4.0-loopback",
                    "observability-readonly-v1",
                    "observability-default",
                    "disabled",
                    vec![McpToolShim::describe()],
                ),
            ],
            config_issues: Vec::new(),
            lifecycle_events: Vec::new(),
        };
        registry.apply_env_overrides();
        registry
    }

    fn enable_server(&mut self, server_name: &str) -> Result<(), String> {
        let server = self
            .find_server_mut(server_name)
            .ok_or_else(|| format!("server {server_name} not found in registry"))?;
        if server.lifecycle_state == McpServerLifecycleState::Deregistered {
            return Err(format!(
                "server {} has been deregistered",
                server.server_name
            ));
        }

        let target_health =
            if server.health_status == "disabled" || server.health_status == "unavailable" {
                "healthy"
            } else {
                server.health_status
            };
        if server.enabled
            && server.health_status == target_health
            && server.lifecycle_state == McpServerLifecycleState::Running
        {
            return Ok(());
        }

        let previous_state = server.lifecycle_state.clone();
        let canonical_name = server.server_name.to_string();
        server.enabled = true;
        server.health_status = target_health;
        server.lifecycle_state = McpServerLifecycleState::Running;
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: canonical_name,
            event_kind: McpLifecycleEventKind::Started,
            previous_state,
            new_state: McpServerLifecycleState::Running,
            reason: "enabled via enable_server".to_string(),
        });
        Ok(())
    }

    fn disable_server(&mut self, server_name: &str) -> Result<(), String> {
        let server = self
            .find_server_mut(server_name)
            .ok_or_else(|| format!("server {server_name} not found in registry"))?;
        if server.lifecycle_state == McpServerLifecycleState::Deregistered {
            return Err(format!(
                "server {} has been deregistered",
                server.server_name
            ));
        }
        if !server.enabled
            && server.health_status == "disabled"
            && server.lifecycle_state == McpServerLifecycleState::Stopped
        {
            return Ok(());
        }

        let previous_state = server.lifecycle_state.clone();
        let canonical_name = server.server_name.to_string();
        server.enabled = false;
        server.health_status = "disabled";
        server.lifecycle_state = McpServerLifecycleState::Stopped;
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: canonical_name,
            event_kind: McpLifecycleEventKind::Stopped,
            previous_state,
            new_state: McpServerLifecycleState::Stopped,
            reason: "disabled via disable_server".to_string(),
        });
        Ok(())
    }

    fn register_server(&mut self, descriptor: McpServerDescriptor) -> bool {
        if self
            .servers
            .iter()
            .any(|s| s.server_name == descriptor.server_name)
        {
            return false;
        }
        let server_name = descriptor.server_name.to_string();
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: server_name.clone(),
            event_kind: McpLifecycleEventKind::Registered,
            previous_state: McpServerLifecycleState::Deregistered,
            new_state: McpServerLifecycleState::Registered,
            reason: "registered via register_server".to_string(),
        });
        self.servers.push(descriptor);
        true
    }

    fn start_server(&mut self, server_name: &str) -> Result<(), String> {
        let server = self
            .find_server_mut(server_name)
            .ok_or_else(|| format!("server {server_name} not found in registry"))?;
        match server.lifecycle_state {
            McpServerLifecycleState::Running => {
                return Err(format!("server {} is already running", server.server_name));
            }
            McpServerLifecycleState::Deregistered => {
                return Err(format!(
                    "server {} has been deregistered",
                    server.server_name
                ));
            }
            _ => {}
        }
        let previous_state = server.lifecycle_state.clone();
        let canonical_name = server.server_name.to_string();
        server.lifecycle_state = McpServerLifecycleState::Running;
        server.enabled = true;
        if server.health_status == "disabled" || server.health_status == "unavailable" {
            server.health_status = "healthy";
        }
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: canonical_name,
            event_kind: McpLifecycleEventKind::Started,
            previous_state,
            new_state: McpServerLifecycleState::Running,
            reason: "started via start_server".to_string(),
        });
        Ok(())
    }

    fn stop_server(&mut self, server_name: &str) -> Result<(), String> {
        let server = self
            .find_server_mut(server_name)
            .ok_or_else(|| format!("server {server_name} not found in registry"))?;
        match server.lifecycle_state {
            McpServerLifecycleState::Stopped => {
                return Err(format!("server {} is already stopped", server.server_name));
            }
            McpServerLifecycleState::Deregistered => {
                return Err(format!(
                    "server {} has been deregistered",
                    server.server_name
                ));
            }
            _ => {}
        }
        let previous_state = server.lifecycle_state.clone();
        let canonical_name = server.server_name.to_string();
        server.lifecycle_state = McpServerLifecycleState::Stopped;
        server.enabled = false;
        server.health_status = "disabled";
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: canonical_name,
            event_kind: McpLifecycleEventKind::Stopped,
            previous_state,
            new_state: McpServerLifecycleState::Stopped,
            reason: "stopped via stop_server".to_string(),
        });
        Ok(())
    }

    fn deregister_server(&mut self, server_name: &str) -> Result<(), String> {
        let server = self
            .find_server_mut(server_name)
            .ok_or_else(|| format!("server {server_name} not found in registry"))?;
        if server.lifecycle_state == McpServerLifecycleState::Deregistered {
            return Err(format!(
                "server {} is already deregistered",
                server.server_name
            ));
        }
        let previous_state = server.lifecycle_state.clone();
        let canonical_name = server.server_name.to_string();
        server.lifecycle_state = McpServerLifecycleState::Deregistered;
        server.enabled = false;
        server.health_status = "unavailable";
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: canonical_name,
            event_kind: McpLifecycleEventKind::Deregistered,
            previous_state,
            new_state: McpServerLifecycleState::Deregistered,
            reason: "deregistered via deregister_server".to_string(),
        });
        Ok(())
    }

    fn update_server_health(
        &mut self,
        server_name: &str,
        new_health: &'static str,
    ) -> Result<(), String> {
        let server = self
            .find_server_mut(server_name)
            .ok_or_else(|| format!("server {server_name} not found in registry"))?;
        if server.lifecycle_state == McpServerLifecycleState::Deregistered {
            return Err(format!(
                "server {} has been deregistered",
                server.server_name
            ));
        }
        let old_health = server.health_status;
        if old_health == new_health {
            return Ok(());
        }
        let previous_state = server.lifecycle_state.clone();
        let canonical_name = server.server_name.to_string();
        server.health_status = new_health;
        if new_health == "disabled" {
            server.enabled = false;
        }
        server.lifecycle_state = lifecycle_state_for_health(server.enabled, server.health_status);
        let new_state = server.lifecycle_state.clone();
        self.lifecycle_events.push(McpLifecycleEvent {
            server_name: canonical_name,
            event_kind: McpLifecycleEventKind::HealthChanged,
            previous_state,
            new_state,
            reason: format!("health changed from {old_health} to {new_health}"),
        });
        Ok(())
    }

    fn lifecycle_events_for(&self, server_name: &str) -> Vec<&McpLifecycleEvent> {
        self.lifecycle_events
            .iter()
            .filter(|event| event.server_name == server_name.trim())
            .collect()
    }

    fn find_server(&self, requested_server: &str) -> Option<&McpServerDescriptor> {
        self.servers
            .iter()
            .find(|server| server.matches_requested_server(requested_server))
    }

    fn find_server_mut(&mut self, requested_server: &str) -> Option<&mut McpServerDescriptor> {
        self.servers
            .iter_mut()
            .find(|server| server.matches_requested_server(requested_server))
    }

    fn lifecycle_event_count(&self) -> usize {
        self.lifecycle_events.len()
    }

    fn apply_env_overrides(&mut self) {
        self.default_server_preference = env::var(MCP_MANAGER_DEFAULT_SERVER_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(default_server) = self.default_server_preference.as_deref()
            && !self
                .servers
                .iter()
                .any(|server| server.matches_requested_server(default_server))
        {
            self.config_issues.push(format!(
                "default server target {default_server} does not match any registered server"
            ));
        }

        for enabled_server in parse_list_env(MCP_MANAGER_ENABLED_SERVERS_ENV) {
            if let Some(server) = self
                .servers
                .iter_mut()
                .find(|server| server.matches_requested_server(&enabled_server))
            {
                server.enabled = true;
                if server.health_status == "disabled" {
                    server.health_status = "healthy";
                }
                server.lifecycle_state =
                    lifecycle_state_for_health(server.enabled, server.health_status);
            } else {
                self.config_issues.push(format!(
                    "enabled server target {enabled_server} does not match any registered server"
                ));
            }
        }

        for disabled_server in parse_list_env(MCP_MANAGER_DISABLED_SERVERS_ENV) {
            if let Some(server) = self
                .servers
                .iter_mut()
                .find(|server| server.matches_requested_server(&disabled_server))
            {
                server.enabled = false;
                server.health_status = "disabled";
                server.lifecycle_state = McpServerLifecycleState::Stopped;
            } else {
                self.config_issues.push(format!(
                    "disabled server target {disabled_server} does not match any registered server"
                ));
            }
        }

        if let Ok(raw) = env::var(MCP_MANAGER_SERVER_HEALTHS_ENV) {
            for override_pair in raw
                .split(',')
                .map(str::trim)
                .filter(|pair| !pair.is_empty())
            {
                let Some((server_name, health_status)) = override_pair.split_once('=') else {
                    self.config_issues.push(format!(
                        "health override {override_pair} is missing '=' separator"
                    ));
                    continue;
                };
                let Some(health_status) = parse_server_health(health_status) else {
                    self.config_issues.push(format!(
                        "health override for {} uses unsupported status {}",
                        server_name.trim(),
                        health_status.trim()
                    ));
                    continue;
                };
                if let Some(server) = self
                    .servers
                    .iter_mut()
                    .find(|server| server.matches_requested_server(server_name.trim()))
                {
                    server.health_status = health_status;
                    if health_status == "disabled" {
                        server.enabled = false;
                    }
                    server.lifecycle_state =
                        lifecycle_state_for_health(server.enabled, server.health_status);
                } else {
                    self.config_issues.push(format!(
                        "health override target {} does not match any registered server",
                        server_name.trim()
                    ));
                }
            }
        }
    }

    fn configured_default_server(&self) -> Option<&McpServerDescriptor> {
        self.default_server_preference
            .as_deref()
            .and_then(|target| {
                self.servers
                    .iter()
                    .find(|server| server.matches_requested_server(target))
            })
    }

    fn manager_service_health(&self) -> &'static str {
        if !self.config_issues.is_empty() {
            return if self.default_server_descriptor().is_some() {
                "degraded"
            } else {
                "unavailable"
            };
        }
        match self.default_route_status() {
            "ready" => "healthy",
            "fallback-only" => "degraded",
            _ => "unavailable",
        }
    }

    fn manager_service_health_reason(&self) -> String {
        if !self.config_issues.is_empty() {
            return format!("registry config issues: {}", self.config_issues.join("; "));
        }
        if self.default_server_descriptor().is_none() {
            return "no enabled server available".to_string();
        }
        if let Some(preferred_server) = self.configured_default_server() {
            if !preferred_server.enabled {
                return format!(
                    "configured default server {} is disabled",
                    preferred_server.server_name
                );
            }
            if preferred_server.health_status != "healthy" {
                return format!(
                    "configured default server {} is {}",
                    preferred_server.server_name, preferred_server.health_status
                );
            }
        }
        if self.default_route_status() == "fallback-only" {
            return "using enabled fallback server because no healthy default route is available"
                .to_string();
        }
        "registry healthy".to_string()
    }

    fn default_route_status(&self) -> &'static str {
        match self.default_server_descriptor() {
            None => "unavailable",
            Some(server) if server.health_status == "healthy" => {
                if let Some(preferred_server) = self.configured_default_server() {
                    if preferred_server.server_name == server.server_name {
                        "ready"
                    } else {
                        "fallback-only"
                    }
                } else {
                    "ready"
                }
            }
            Some(_) => "fallback-only",
        }
    }

    fn default_route_target(&self) -> String {
        self.default_server_descriptor()
            .map(|server| server.server_name.to_string())
            .unwrap_or_else(|| "<none>".to_string())
    }

    fn manager_descriptor(&self) -> BridgeServerServiceDescriptor {
        let default_server = self.default_server_descriptor();
        let default_server_name = self.default_server_name();
        let selection_targets = self.selection_targets();
        let manager_service_health = self.manager_service_health();
        let manager_service_health_reason = self.manager_service_health_reason();
        let default_route_status = self.default_route_status();
        let default_route_target = self.default_route_target();
        BridgeServerServiceDescriptor {
            service_name: self.manager_name.to_string(),
            shim_kind: "mcp-manager-registry".to_string(),
            supported_operations: vec![
                "list_servers".to_string(),
                "describe_server".to_string(),
                "enable_server".to_string(),
                "disable_server".to_string(),
                "register_server".to_string(),
                "start_server".to_string(),
                "stop_server".to_string(),
                "deregister_server".to_string(),
                "update_health".to_string(),
            ],
            capabilities: vec![
                "registry:loopback".to_string(),
                "implementation_source:loopback-manager-prehost".to_string(),
                format!("manager_version:{}", self.manager_version),
                format!("registry_profile:{}", self.registry_profile),
                format!("registry_manifest:{}", self.registry_manifest()),
                format!("selection_strategy:{}", self.selection_strategy),
                format!("service_health:{manager_service_health}"),
                format!("service_health_reason:{manager_service_health_reason}"),
                format!(
                    "registry_config_status:{}",
                    if self.config_issues.is_empty() {
                        "clean"
                    } else {
                        "misconfigured"
                    }
                ),
                format!("config_issue_count:{}", self.config_issue_count()),
                format!("default_route_status:{default_route_status}"),
                format!("default_route_target:{default_route_target}"),
                format!("server_count:{}", self.servers.len()),
                format!("enabled_server_count:{}", self.enabled_server_count()),
                format!("healthy_server_count:{}", self.healthy_server_count()),
                format!("default_server:{}", default_server_name.unwrap_or("<none>")),
                format!(
                    "default_server_health:{}",
                    default_server
                        .map(|server| server.health_status)
                        .unwrap_or("unavailable")
                ),
                format!(
                    "default_server_selection_key:{}",
                    default_server
                        .map(|server| server.selection_key)
                        .unwrap_or("<none>")
                ),
                format!("selection_target_count:{}", selection_targets.len()),
                format!("lifecycle_event_count:{}", self.lifecycle_event_count()),
                self.servers
                    .iter()
                    .map(|server| format!("server:{}", server.server_name))
                    .collect::<Vec<_>>()
                    .join(","),
            ],
            service_health: Some(manager_service_health.to_string()),
            service_health_reason: Some(manager_service_health_reason),
            implementation_source: Some("loopback-manager-prehost".to_string()),
            capability_profile: Some("loopback-mcp-manager-v1".to_string()),
            workspace_roots_source: None,
            manager_version: Some(self.manager_version.to_string()),
            registry_profile: Some(self.registry_profile.to_string()),
            registry_manifest: Some(self.registry_manifest()),
            selection_strategy: Some(self.selection_strategy.to_string()),
            default_server: default_server_name.map(str::to_string),
            default_server_health: default_server.map(|server| server.health_status.to_string()),
            default_server_selection_key: default_server
                .map(|server| server.selection_key.to_string()),
            default_route_status: Some(default_route_status.to_string()),
            default_route_target: Some(default_route_target),
            selection_targets: Some(selection_targets),
            selection_key: None,
            server_manifest: None,
            shell_manifest: None,
            shell_profile: None,
            command_capability_profiles: None,
            session_descriptor: None,
            workspace_context: None,
            context_resolution_boundary: Some(BridgeServerContextResolutionBoundary {
                request_binding: "manager-registry-selection".to_string(),
                session_resolution_strategy: "none".to_string(),
                workspace_resolution_strategy: "none".to_string(),
                session_resolution_source: "registry-metadata".to_string(),
                workspace_resolution_source: "registry-metadata".to_string(),
            }),
        }
    }

    fn enabled_server_count(&self) -> usize {
        self.servers.iter().filter(|server| server.enabled).count()
    }

    fn config_issue_count(&self) -> usize {
        self.config_issues.len()
    }

    fn registry_manifest(&self) -> String {
        format!("{}@{}", self.manager_name, self.manager_version)
    }

    fn healthy_server_count(&self) -> usize {
        self.servers
            .iter()
            .filter(|server| server.enabled && server.health_status == "healthy")
            .count()
    }

    fn default_server_name(&self) -> Option<&'static str> {
        self.default_server_descriptor()
            .map(|server| server.server_name)
    }

    fn default_server_descriptor(&self) -> Option<&McpServerDescriptor> {
        if let Some(preferred_server) = self.configured_default_server()
            && preferred_server.is_routable()
        {
            return Some(preferred_server);
        }
        self.servers.iter().find(|server| server.is_routable())
    }

    fn selection_targets(&self) -> Vec<String> {
        self.servers
            .iter()
            .flat_map(|server| {
                [
                    server.server_name.to_string(),
                    format!("selection-key:{}", server.selection_key),
                ]
            })
            .collect()
    }

    fn service_catalog(&self) -> BridgeServerServiceCatalog {
        let mut services = vec![self.manager_descriptor()];
        services.extend(
            self.servers
                .iter()
                .map(McpServerDescriptor::service_descriptor),
        );
        BridgeServerServiceCatalog {
            protocol_version: LOCAL_BRIDGE_PROTOCOL_VERSION.to_string(),
            server_kind: BridgeServerKind::Mcp,
            services,
        }
    }

    fn handle(
        &mut self,
        method: &str,
        request: LocalProcessBridgeRequest,
    ) -> Result<Value, LocalProcessBridgeRpcError> {
        let _request_id = request.id;

        match method {
            MCP_CALL_TOOL_METHOD => self.handle_call_tool(request.params),
            MCP_LIST_SERVERS_METHOD => Ok(self.list_servers_value()),
            MCP_DESCRIBE_SERVER_METHOD => {
                let params: McpManagerServerSelectionRequest = decode_params(request.params)?;
                self.describe_server_value(&params.server_name)
            }
            MCP_ENABLE_SERVER_METHOD => {
                let params: McpManagerServerSelectionRequest = decode_params(request.params)?;
                let resolved = self
                    .resolve_server(&params.server_name)?
                    .server_name
                    .to_string();
                let previous_event_count = self.lifecycle_event_count();
                self.enable_server(&params.server_name).map_err(|reason| {
                    lifecycle_operation_error("enable_server", &resolved, reason)
                })?;
                self.server_operation_value("enable_server", &resolved, previous_event_count)
            }
            MCP_DISABLE_SERVER_METHOD => {
                let params: McpManagerServerSelectionRequest = decode_params(request.params)?;
                let resolved = self
                    .resolve_server(&params.server_name)?
                    .server_name
                    .to_string();
                let previous_event_count = self.lifecycle_event_count();
                self.disable_server(&params.server_name).map_err(|reason| {
                    lifecycle_operation_error("disable_server", &resolved, reason)
                })?;
                self.server_operation_value("disable_server", &resolved, previous_event_count)
            }
            MCP_REGISTER_SERVER_METHOD => {
                let params: McpManagerServerRegistrationRequest = decode_params(request.params)?;
                let requested_name = params.server_name.clone();
                let previous_event_count = self.lifecycle_event_count();
                let descriptor = params.into_descriptor()?;
                if !self.register_server(descriptor) {
                    return Err(LocalProcessBridgeRpcError::remote_business(
                        -32016,
                        "server already registered",
                        Some(json!({
                            "server_name": requested_name,
                        })),
                    ));
                }
                self.server_operation_value(
                    "register_server",
                    &requested_name,
                    previous_event_count,
                )
            }
            MCP_START_SERVER_METHOD => {
                let params: McpManagerServerSelectionRequest = decode_params(request.params)?;
                let resolved = self
                    .resolve_server(&params.server_name)?
                    .server_name
                    .to_string();
                let previous_event_count = self.lifecycle_event_count();
                self.start_server(&params.server_name).map_err(|reason| {
                    lifecycle_operation_error("start_server", &resolved, reason)
                })?;
                self.server_operation_value("start_server", &resolved, previous_event_count)
            }
            MCP_STOP_SERVER_METHOD => {
                let params: McpManagerServerSelectionRequest = decode_params(request.params)?;
                let resolved = self
                    .resolve_server(&params.server_name)?
                    .server_name
                    .to_string();
                let previous_event_count = self.lifecycle_event_count();
                self.stop_server(&params.server_name).map_err(|reason| {
                    lifecycle_operation_error("stop_server", &resolved, reason)
                })?;
                self.server_operation_value("stop_server", &resolved, previous_event_count)
            }
            MCP_DEREGISTER_SERVER_METHOD => {
                let params: McpManagerServerSelectionRequest = decode_params(request.params)?;
                let resolved = self
                    .resolve_server(&params.server_name)?
                    .server_name
                    .to_string();
                let previous_event_count = self.lifecycle_event_count();
                self.deregister_server(&params.server_name)
                    .map_err(|reason| {
                        lifecycle_operation_error("deregister_server", &resolved, reason)
                    })?;
                self.server_operation_value("deregister_server", &resolved, previous_event_count)
            }
            MCP_UPDATE_HEALTH_METHOD => {
                let params: McpManagerServerHealthUpdateRequest = decode_params(request.params)?;
                let resolved = self
                    .resolve_server(&params.server_name)?
                    .server_name
                    .to_string();
                let previous_event_count = self.lifecycle_event_count();
                let health_status =
                    parse_server_health(&params.health_status).ok_or_else(|| {
                        LocalProcessBridgeRpcError::invalid_params(format!(
                            "unsupported health status {}",
                            params.health_status
                        ))
                    })?;
                self.update_server_health(&params.server_name, health_status)
                    .map_err(|reason| {
                        lifecycle_operation_error("update_health", &resolved, reason)
                    })?;
                self.server_operation_value("update_health", &resolved, previous_event_count)
            }
            _ => Err(LocalProcessBridgeRpcError::remote_business(
                -32601,
                "method not found",
                Some(json!({
                    "method": method,
                })),
            )),
        }
    }

    fn handle_call_tool(&self, params: Value) -> Result<Value, LocalProcessBridgeRpcError> {
        let call: McpToolCallRequest = decode_params(params)?;
        let server = self.resolve_server(&call.server_name)?;

        let mut normalized_call = call.clone();
        if normalized_call.server_name.trim().is_empty() {
            normalized_call.server_name = server.server_name.to_string();
        }

        let response = server.execute(&normalized_call, self)?;
        serde_json::to_value(response).map_err(|error| {
            LocalProcessBridgeRpcError::invalid_params(format!(
                "serialize mcp bridge response failed: {error}"
            ))
        })
    }

    fn list_servers_value(&self) -> Value {
        encode_value(McpManagerListServersResponse {
            manager: self.manager_descriptor(),
            servers: self
                .servers
                .iter()
                .map(McpServerDescriptor::service_descriptor)
                .collect(),
            selection_targets: self.selection_targets(),
            default_route_status: self.default_route_status().to_string(),
            default_route_target: self.default_route_target(),
        })
        .expect("manager list response should serialize")
    }

    fn describe_server_value(
        &self,
        server_name: &str,
    ) -> Result<Value, LocalProcessBridgeRpcError> {
        let server = self.resolve_server(server_name)?;
        let events = self
            .lifecycle_events_for(server.server_name)
            .into_iter()
            .cloned()
            .collect();
        encode_value(McpManagerDescribeServerResponse {
            manager: self.manager_descriptor(),
            server: server.service_descriptor(),
            lifecycle_events: events,
        })
    }

    fn server_operation_value(
        &self,
        operation: &str,
        server_name: &str,
        previous_event_count: usize,
    ) -> Result<Value, LocalProcessBridgeRpcError> {
        let server = self.resolve_server(server_name)?;
        let server_events = self
            .lifecycle_events_for(server.server_name)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let lifecycle_event = (self.lifecycle_event_count() > previous_event_count)
            .then(|| server_events.last().cloned())
            .flatten();
        encode_value(McpManagerServerOperationResponse {
            operation: operation.to_string(),
            manager: self.manager_descriptor(),
            server: server.service_descriptor(),
            lifecycle_event_count: self.lifecycle_event_count(),
            lifecycle_event,
            server_events,
        })
    }

    fn resolve_server(
        &self,
        server_name: &str,
    ) -> Result<&McpServerDescriptor, LocalProcessBridgeRpcError> {
        if server_name.trim().is_empty() {
            return self.default_server_descriptor().ok_or_else(|| {
                LocalProcessBridgeRpcError::remote_business(
                    -32015,
                    "default server unavailable",
                    Some(json!({
                        "manager_name": self.manager_name,
                        "default_server": self.default_server_name(),
                        "default_route_status": self.default_route_status(),
                        "default_route_target": self.default_route_target(),
                        "manager_health": self.manager_service_health(),
                        "manager_health_reason": self.manager_service_health_reason(),
                    })),
                )
            });
        }

        self.find_server(server_name)
            .ok_or_else(|| self.unknown_server_error(server_name))
    }

    fn unknown_server_error(&self, server_name: &str) -> LocalProcessBridgeRpcError {
        LocalProcessBridgeRpcError::remote_business(
            -32011,
            "unknown server",
            Some(json!({
                "server_name": server_name,
                "supported_servers": self
                    .servers
                    .iter()
                    .map(|server| json!({
                        "server_name": server.server_name,
                        "selection_key": server.selection_key,
                        "server_version": server.server_version,
                        "capability_profile": server.capability_profile,
                        "server_enabled": server.enabled,
                        "health_status": server.health_status,
                    }))
                    .collect::<Vec<_>>(),
                "default_route_status": self.default_route_status(),
                "default_route_target": self.default_route_target(),
            })),
        )
    }
}

fn handle_mcp_request(
    method: &str,
    request: LocalProcessBridgeRequest,
) -> Result<Value, LocalProcessBridgeRpcError> {
    let mut registry = McpServerRegistry::loopback();
    registry.handle(method, request)
}

#[cfg(test)]
fn handle_mcp_call(
    request: LocalProcessBridgeRequest,
) -> Result<Value, LocalProcessBridgeRpcError> {
    handle_mcp_request(MCP_CALL_TOOL_METHOD, request)
}

fn decode_params<T: serde::de::DeserializeOwned>(
    params: Value,
) -> Result<T, LocalProcessBridgeRpcError> {
    serde_json::from_value(params)
        .map_err(|error| LocalProcessBridgeRpcError::invalid_params(error.to_string()))
}

fn encode_value<T: Serialize>(value: T) -> Result<Value, LocalProcessBridgeRpcError> {
    serde_json::to_value(value).map_err(|error| {
        LocalProcessBridgeRpcError::invalid_params(format!(
            "serialize mcp manager payload failed: {error}"
        ))
    })
}

fn lifecycle_operation_error(
    operation: &str,
    server_name: &str,
    reason: String,
) -> LocalProcessBridgeRpcError {
    LocalProcessBridgeRpcError::remote_business(
        -32017,
        format!("{operation} rejected"),
        Some(json!({
            "server_name": server_name,
            "reason": reason,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BridgeTransport, BridgeTransportError, BridgeTransportRequest, BridgeTransportResponse,
        JsonRpcMcpManagerClient,
    };
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct StatefulRegistryTransport {
        registry: Mutex<McpServerRegistry>,
    }

    impl StatefulRegistryTransport {
        fn loopback() -> Self {
            Self {
                registry: Mutex::new(McpServerRegistry::loopback()),
            }
        }
    }

    impl BridgeTransport for StatefulRegistryTransport {
        fn call(
            &self,
            request: BridgeTransportRequest,
        ) -> Result<BridgeTransportResponse, BridgeTransportError> {
            let mut registry = self.registry.lock().expect("lock poisoned");
            let payload = registry
                .handle(
                    &request.method,
                    LocalProcessBridgeRequest {
                        id: Value::from(1),
                        params: request.params,
                    },
                )
                .map_err(map_registry_error)?;
            Ok(BridgeTransportResponse { payload })
        }
    }

    fn map_registry_error(error: LocalProcessBridgeRpcError) -> BridgeTransportError {
        match error.code() {
            -32700 | -32600 | -32601 | -32602 | -32603 => BridgeTransportError::Protocol {
                message: format!(
                    "json-rpc protocol error [{}]: {}",
                    error.code(),
                    error.message()
                ),
            },
            code => BridgeTransportError::RemoteBusiness {
                code,
                message: error.message().to_string(),
                data: error.data().cloned(),
            },
        }
    }

    #[test]
    fn mcp_catalog_exposes_loopback_server_and_tool_registry() {
        let catalog = McpServerRegistry::loopback().service_catalog();
        assert_eq!(catalog.server_kind, BridgeServerKind::Mcp);
        assert_eq!(catalog.services.len(), 3);
        assert_eq!(catalog.services[0].service_name, "loopback-mcp-manager");
        assert_eq!(
            catalog.services[0]
                .implementation_source
                .as_deref()
                .expect("manager implementation source should exist"),
            "loopback-manager-prehost"
        );
        assert_eq!(
            catalog.services[0]
                .manager_version
                .as_deref()
                .expect("manager version should exist"),
            "1.0.0-loopback"
        );
        assert_eq!(
            catalog.services[0]
                .registry_profile
                .as_deref()
                .expect("registry profile should exist"),
            "loopback-mcp-registry-v1"
        );
        assert_eq!(
            catalog.services[0]
                .registry_manifest
                .as_deref()
                .expect("registry manifest should exist"),
            "loopback-mcp-manager@1.0.0-loopback"
        );
        assert_eq!(
            catalog.services[0]
                .selection_strategy
                .as_deref()
                .expect("selection strategy should exist"),
            "explicit-or-selection-key"
        );
        assert_eq!(
            catalog.services[0]
                .default_server
                .as_deref()
                .expect("default server should exist"),
            LOOPBACK_MCP_SERVER_NAME
        );
        assert_eq!(
            catalog.services[0]
                .default_server_health
                .as_deref()
                .expect("default server health should exist"),
            "healthy"
        );
        assert_eq!(
            catalog.services[0]
                .default_server_selection_key
                .as_deref()
                .expect("default server selection key should exist"),
            "inspection-default"
        );
        assert_eq!(
            catalog.services[0]
                .selection_targets
                .as_ref()
                .expect("selection targets should exist"),
            &vec![
                "loopback-mcp".to_string(),
                "selection-key:inspection-default".to_string(),
                "loopback-mcp-observability".to_string(),
                "selection-key:observability-default".to_string(),
            ]
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"registry:loopback".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"manager_version:1.0.0-loopback".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"registry_profile:loopback-mcp-registry-v1".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"registry_manifest:loopback-mcp-manager@1.0.0-loopback".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"selection_strategy:explicit-or-selection-key".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"default_server:loopback-mcp".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"default_server_health:healthy".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"default_server_selection_key:inspection-default".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"selection_target_count:4".to_string())
        );
        assert!(
            catalog.services[0]
                .supported_operations
                .contains(&"enable_server".to_string())
        );
        assert_eq!(
            catalog.services[1]
                .implementation_source
                .as_deref()
                .expect("server implementation source should exist"),
            "loopback-server-prehost"
        );
        assert_eq!(
            catalog.services[1]
                .capability_profile
                .as_deref()
                .expect("server capability profile should exist"),
            "inspection-core-v1"
        );
        assert_eq!(
            catalog.services[1]
                .selection_key
                .as_deref()
                .expect("server selection key should exist"),
            "inspection-default"
        );
        assert_eq!(
            catalog.services[1]
                .server_manifest
                .as_deref()
                .expect("server manifest should exist"),
            "loopback-mcp@1.2.0-loopback"
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"server_enabled:true".to_string())
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"server_version:1.2.0-loopback".to_string())
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"server_manifest:loopback-mcp@1.2.0-loopback".to_string())
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"capability_profile:inspection-core-v1".to_string())
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"selection_key:inspection-default".to_string())
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"tool:echo.inspect".to_string())
        );
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"tool:echo.describe".to_string())
        );
        assert!(
            catalog.services[2]
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
        assert_eq!(
            catalog.services[2]
                .implementation_source
                .as_deref()
                .expect("observability implementation source should exist"),
            "loopback-server-prehost"
        );
        assert_eq!(
            catalog.services[2]
                .capability_profile
                .as_deref()
                .expect("observability capability profile should exist"),
            "observability-readonly-v1"
        );
        assert_eq!(
            catalog.services[2]
                .selection_key
                .as_deref()
                .expect("observability selection key should exist"),
            "observability-default"
        );
        assert!(
            catalog.services[2]
                .capabilities
                .contains(&"capability_profile:observability-readonly-v1".to_string())
        );
        assert!(
            catalog.services[2]
                .capabilities
                .contains(&"tool:echo.describe".to_string())
        );
    }

    #[test]
    fn blank_selection_falls_back_to_default_server() {
        let response = McpServerRegistry::loopback()
            .handle(
                MCP_CALL_TOOL_METHOD,
                LocalProcessBridgeRequest {
                    id: Value::from(1),
                    params: serde_json::json!({
                        "server_name": "   ",
                        "tool_name": LOOPBACK_MCP_TOOL_NAME,
                        "input": "{}",
                    }),
                },
            )
            .expect("blank server selection should resolve to default server");
        let response: BridgeResponse =
            serde_json::from_value(response).expect("response should deserialize");
        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should deserialize");
        assert_eq!(payload["server_name"], LOOPBACK_MCP_SERVER_NAME);
        assert_eq!(payload["default_server"], LOOPBACK_MCP_SERVER_NAME);
        assert_eq!(payload["selection_strategy"], "explicit-or-selection-key");
    }

    #[test]
    fn unknown_server_returns_remote_business_error() {
        let error = super::handle_mcp_call(LocalProcessBridgeRequest {
            id: Value::from(1),
            params: serde_json::json!({
                "server_name": "other-mcp",
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}",
            }),
        })
        .expect_err("unknown server should return remote business error");

        assert_eq!(error.code(), -32011);
    }

    #[test]
    fn disabled_server_returns_remote_business_error() {
        let registry = McpServerRegistry::loopback();
        let disabled = registry
            .servers
            .iter()
            .find(|server| server.server_name == "loopback-mcp-observability")
            .expect("disabled server should exist");
        let error = disabled
            .execute(
                &McpToolCallRequest {
                    server_name: "loopback-mcp-observability".to_string(),
                    tool_name: "echo.describe".to_string(),
                    input: "{}".to_string(),
                },
                &registry,
            )
            .expect_err("disabled server should reject calls");

        assert_eq!(error.code(), -32014);
    }

    #[test]
    fn list_servers_value_decodes_into_typed_manager_contract() {
        let registry = McpServerRegistry::loopback();

        let response: McpManagerListServersResponse =
            serde_json::from_value(registry.list_servers_value())
                .expect("list response should deserialize");

        assert_eq!(response.manager.service_name, "loopback-mcp-manager");
        assert_eq!(response.servers.len(), 2);
        assert_eq!(response.default_route_status, "ready");
        assert_eq!(response.default_route_target, LOOPBACK_MCP_SERVER_NAME);
    }

    #[test]
    fn registration_request_defaults_match_manager_contract() {
        let request: McpManagerServerRegistrationRequest = serde_json::from_value(json!({
            "server_name": "loopback-mcp-dynamic",
            "server_version": "0.1.0",
            "capability_profile": "dynamic-readonly-v1",
            "selection_key": "dynamic-default"
        }))
        .expect("registration request should deserialize");

        assert_eq!(request.implementation_source, "loopback-server-prehost");
        assert_eq!(request.health_status, "healthy");
        assert!(!request.enabled);
        assert!(request.tool_names.is_empty());
    }

    #[test]
    fn server_operation_value_decodes_into_typed_manager_contract() {
        let mut registry = McpServerRegistry::loopback();
        registry
            .start_server("loopback-mcp-observability")
            .expect("start should succeed");

        let response: McpManagerServerOperationResponse = serde_json::from_value(
            registry
                .server_operation_value("start_server", "loopback-mcp-observability", 0)
                .expect("operation response should serialize"),
        )
        .expect("operation response should deserialize");

        assert_eq!(response.operation, "start_server");
        assert_eq!(response.server.service_name, "loopback-mcp-observability");
        assert_eq!(response.lifecycle_event_count, 1);
        assert_eq!(
            response
                .lifecycle_event
                .as_ref()
                .expect("lifecycle event should exist")
                .event_kind,
            McpLifecycleEventKind::Started
        );
        assert_eq!(response.server_events.len(), 1);
    }

    #[test]
    fn registry_enable_disable_updates_service_catalog() {
        let mut registry = McpServerRegistry::loopback();
        registry
            .disable_server(LOOPBACK_MCP_SERVER_NAME)
            .expect("disable should succeed");
        let catalog = registry.service_catalog();
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
        registry
            .enable_server(LOOPBACK_MCP_SERVER_NAME)
            .expect("enable should succeed");
        let catalog = registry.service_catalog();
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"server_enabled:true".to_string())
        );
    }

    #[test]
    fn selection_key_routes_to_canonical_server() {
        let response = McpServerRegistry::loopback()
            .handle(
                MCP_CALL_TOOL_METHOD,
                LocalProcessBridgeRequest {
                    id: Value::from(1),
                    params: serde_json::json!({
                        "server_name": "inspection-default",
                        "tool_name": LOOPBACK_MCP_TOOL_NAME,
                        "input": "{}",
                    }),
                },
            )
            .expect("selection key should resolve to canonical server");
        let response: BridgeResponse =
            serde_json::from_value(response).expect("response should deserialize");
        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should deserialize");
        assert_eq!(payload["server_name"], LOOPBACK_MCP_SERVER_NAME);
        assert_eq!(payload["requested_server"], "inspection-default");
        assert_eq!(payload["capability_profile"], "inspection-core-v1");
    }

    #[test]
    fn blank_selection_errors_when_no_enabled_default_server_exists() {
        let mut registry = McpServerRegistry::loopback();
        registry
            .disable_server(LOOPBACK_MCP_SERVER_NAME)
            .expect("disable should succeed");
        registry
            .disable_server("loopback-mcp-observability")
            .expect("disable should succeed");

        let error = registry
            .handle(
                MCP_CALL_TOOL_METHOD,
                LocalProcessBridgeRequest {
                    id: Value::from(1),
                    params: serde_json::json!({
                        "server_name": "   ",
                        "tool_name": LOOPBACK_MCP_TOOL_NAME,
                        "input": "{}",
                    }),
                },
            )
            .expect_err("blank selection should fail when no enabled default server exists");

        assert_eq!(error.code(), -32015);
        let data = error.data().expect("error data should exist");
        assert_eq!(data["default_server"], Value::Null);
        assert_eq!(data["default_route_target"], "<none>");
    }

    #[test]
    fn mcp_catalog_does_not_report_manager_name_as_default_server_when_no_route_exists() {
        let mut registry = McpServerRegistry::loopback();
        registry
            .disable_server(LOOPBACK_MCP_SERVER_NAME)
            .expect("disable should succeed");
        registry
            .disable_server("loopback-mcp-observability")
            .expect("disable should succeed");

        let catalog = registry.service_catalog();
        assert_eq!(catalog.services[0].default_server, None);
        assert_eq!(catalog.services[0].default_server_health, None);
        assert_eq!(catalog.services[0].default_server_selection_key, None);
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"default_server:<none>".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"default_server_health:unavailable".to_string())
        );
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"default_server_selection_key:<none>".to_string())
        );
    }

    #[test]
    fn lifecycle_state_exposed_in_server_descriptor_capabilities() {
        let registry = McpServerRegistry::loopback();
        let catalog = registry.service_catalog();
        // Enabled server should have lifecycle_state:running
        assert!(
            catalog.services[1]
                .capabilities
                .contains(&"lifecycle_state:running".to_string())
        );
        // Disabled server should have lifecycle_state:stopped
        assert!(
            catalog.services[2]
                .capabilities
                .contains(&"lifecycle_state:stopped".to_string())
        );
    }

    #[test]
    fn register_server_adds_to_registry_and_emits_lifecycle_event() {
        let mut registry = McpServerRegistry::loopback();
        let initial_count = registry.servers.len();
        let new_server = McpServerDescriptor {
            server_name: "test-new-server",
            server_version: "0.1.0",
            implementation_source: "loopback-server-prehost",
            capability_profile: "test-v1",
            selection_key: "test-default",
            enabled: false,
            health_status: "healthy",
            lifecycle_state: McpServerLifecycleState::Registered,
            tools: vec![McpToolShim::describe()],
        };
        assert!(registry.register_server(new_server));
        assert_eq!(registry.servers.len(), initial_count + 1);

        // Duplicate registration should fail
        let dup = McpServerDescriptor {
            server_name: "test-new-server",
            server_version: "0.2.0",
            implementation_source: "loopback-server-prehost",
            capability_profile: "test-v2",
            selection_key: "test-default-2",
            enabled: false,
            health_status: "healthy",
            lifecycle_state: McpServerLifecycleState::Registered,
            tools: vec![],
        };
        assert!(!registry.register_server(dup));

        // Check lifecycle event
        let events = registry.lifecycle_events_for("test-new-server");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, McpLifecycleEventKind::Registered);
        assert_eq!(
            events[0].previous_state,
            McpServerLifecycleState::Deregistered
        );
        assert_eq!(events[0].new_state, McpServerLifecycleState::Registered);
    }

    #[test]
    fn start_stop_server_transitions_lifecycle_state_and_emits_events() {
        let mut registry = McpServerRegistry::loopback();
        // loopback-mcp-observability starts as disabled/stopped
        assert_eq!(
            registry
                .servers
                .iter()
                .find(|s| s.server_name == "loopback-mcp-observability")
                .unwrap()
                .lifecycle_state,
            McpServerLifecycleState::Stopped
        );

        // Start it
        registry
            .start_server("loopback-mcp-observability")
            .expect("start should succeed");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == "loopback-mcp-observability")
            .unwrap();
        assert_eq!(server.lifecycle_state, McpServerLifecycleState::Running);
        assert!(server.enabled);
        assert_eq!(server.health_status, "healthy");

        // Starting again should error
        assert!(registry.start_server("loopback-mcp-observability").is_err());

        // Stop it
        registry
            .stop_server("loopback-mcp-observability")
            .expect("stop should succeed");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == "loopback-mcp-observability")
            .unwrap();
        assert_eq!(server.lifecycle_state, McpServerLifecycleState::Stopped);
        assert!(!server.enabled);
        assert_eq!(server.health_status, "disabled");

        // Stopping again should error
        assert!(registry.stop_server("loopback-mcp-observability").is_err());

        // Check lifecycle events
        let events = registry.lifecycle_events_for("loopback-mcp-observability");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_kind, McpLifecycleEventKind::Started);
        assert_eq!(events[0].previous_state, McpServerLifecycleState::Stopped);
        assert_eq!(events[0].new_state, McpServerLifecycleState::Running);
        assert_eq!(events[1].event_kind, McpLifecycleEventKind::Stopped);
        assert_eq!(events[1].previous_state, McpServerLifecycleState::Running);
        assert_eq!(events[1].new_state, McpServerLifecycleState::Stopped);
    }

    #[test]
    fn deregister_server_transitions_to_deregistered_and_blocks_further_ops() {
        let mut registry = McpServerRegistry::loopback();
        registry
            .deregister_server("loopback-mcp-observability")
            .expect("deregister should succeed");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == "loopback-mcp-observability")
            .unwrap();
        assert_eq!(
            server.lifecycle_state,
            McpServerLifecycleState::Deregistered
        );
        assert!(!server.enabled);
        assert_eq!(server.health_status, "unavailable");

        // Double deregister should error
        assert!(
            registry
                .deregister_server("loopback-mcp-observability")
                .is_err()
        );
        // Start after deregister should error
        assert!(registry.start_server("loopback-mcp-observability").is_err());
        // Health update after deregister should error
        assert!(
            registry
                .update_server_health("loopback-mcp-observability", "healthy")
                .is_err()
        );

        let events = registry.lifecycle_events_for("loopback-mcp-observability");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, McpLifecycleEventKind::Deregistered);
    }

    #[test]
    fn update_server_health_emits_health_changed_event_and_transitions_state() {
        let mut registry = McpServerRegistry::loopback();
        // loopback-mcp starts as healthy/running
        registry
            .update_server_health(LOOPBACK_MCP_SERVER_NAME, "degraded")
            .expect("health update should succeed");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == LOOPBACK_MCP_SERVER_NAME)
            .unwrap();
        assert_eq!(server.health_status, "degraded");
        assert_eq!(server.lifecycle_state, McpServerLifecycleState::Running);

        // Transition to unavailable -> lifecycle_state should become Failed
        registry
            .update_server_health(LOOPBACK_MCP_SERVER_NAME, "unavailable")
            .expect("health update should succeed");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == LOOPBACK_MCP_SERVER_NAME)
            .unwrap();
        assert_eq!(server.health_status, "unavailable");
        assert_eq!(server.lifecycle_state, McpServerLifecycleState::Failed);

        // Transition to disabled -> lifecycle_state should become Stopped, enabled false
        registry
            .update_server_health(LOOPBACK_MCP_SERVER_NAME, "disabled")
            .expect("health update should succeed");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == LOOPBACK_MCP_SERVER_NAME)
            .unwrap();
        assert_eq!(server.health_status, "disabled");
        assert_eq!(server.lifecycle_state, McpServerLifecycleState::Stopped);
        assert!(!server.enabled);

        // Same health should be a no-op (no event emitted)
        registry
            .update_server_health(LOOPBACK_MCP_SERVER_NAME, "disabled")
            .expect("no-op health update should succeed");

        let events = registry.lifecycle_events_for(LOOPBACK_MCP_SERVER_NAME);
        assert_eq!(events.len(), 3); // degraded, unavailable, disabled
        assert!(
            events
                .iter()
                .all(|e| e.event_kind == McpLifecycleEventKind::HealthChanged)
        );
    }

    #[test]
    fn update_server_health_recovers_failed_server_back_to_running() {
        let mut registry = McpServerRegistry::loopback();

        registry
            .update_server_health(LOOPBACK_MCP_SERVER_NAME, "unavailable")
            .expect("unavailable update should succeed");
        let failed_server = registry
            .servers
            .iter()
            .find(|s| s.server_name == LOOPBACK_MCP_SERVER_NAME)
            .expect("loopback server should exist");
        assert_eq!(
            failed_server.lifecycle_state,
            McpServerLifecycleState::Failed
        );
        assert!(failed_server.enabled);

        registry
            .update_server_health(LOOPBACK_MCP_SERVER_NAME, "healthy")
            .expect("healthy update should recover running server");
        let recovered_server = registry
            .servers
            .iter()
            .find(|s| s.server_name == LOOPBACK_MCP_SERVER_NAME)
            .expect("loopback server should exist");
        assert_eq!(recovered_server.health_status, "healthy");
        assert_eq!(
            recovered_server.lifecycle_state,
            McpServerLifecycleState::Running
        );
        assert!(recovered_server.enabled);

        let catalog = registry.service_catalog();
        let service = catalog
            .services
            .iter()
            .find(|service| service.service_name == LOOPBACK_MCP_SERVER_NAME)
            .expect("loopback service should appear in catalog");
        assert!(
            service
                .capabilities
                .contains(&"lifecycle_state:running".to_string())
        );
    }

    #[test]
    fn update_server_health_on_disabled_server_restores_registered_state_without_enabling() {
        let mut registry = McpServerRegistry::loopback();

        registry
            .update_server_health("loopback-mcp-observability", "healthy")
            .expect("healthy update should be allowed for disabled server");
        let server = registry
            .servers
            .iter()
            .find(|s| s.server_name == "loopback-mcp-observability")
            .expect("observability server should exist");
        assert_eq!(server.health_status, "healthy");
        assert_eq!(server.lifecycle_state, McpServerLifecycleState::Registered);
        assert!(!server.enabled);

        let catalog = registry.service_catalog();
        let service = catalog
            .services
            .iter()
            .find(|service| service.service_name == "loopback-mcp-observability")
            .expect("observability service should appear in catalog");
        assert!(
            service
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
        assert!(
            service
                .capabilities
                .contains(&"lifecycle_state:registered".to_string())
        );
    }

    #[test]
    fn enable_disable_emit_lifecycle_events() {
        let mut registry = McpServerRegistry::loopback();
        let initial_events = registry.lifecycle_event_count();

        registry
            .disable_server(LOOPBACK_MCP_SERVER_NAME)
            .expect("disable should succeed");
        assert_eq!(registry.lifecycle_event_count(), initial_events + 1);

        registry
            .enable_server(LOOPBACK_MCP_SERVER_NAME)
            .expect("enable should succeed");
        assert_eq!(registry.lifecycle_event_count(), initial_events + 2);

        let events = registry.lifecycle_events_for(LOOPBACK_MCP_SERVER_NAME);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_kind, McpLifecycleEventKind::Stopped);
        assert_eq!(events[0].previous_state, McpServerLifecycleState::Running);
        assert_eq!(events[0].new_state, McpServerLifecycleState::Stopped);
        assert_eq!(events[1].event_kind, McpLifecycleEventKind::Started);
        assert_eq!(events[1].previous_state, McpServerLifecycleState::Stopped);
        assert_eq!(events[1].new_state, McpServerLifecycleState::Running);
    }

    #[test]
    fn lifecycle_event_count_exposed_in_manager_descriptor() {
        let mut registry = McpServerRegistry::loopback();
        // No events initially from loopback()
        let catalog = registry.service_catalog();
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"lifecycle_event_count:0".to_string())
        );

        // After some operations, event count should reflect
        registry.start_server("loopback-mcp-observability").unwrap();
        registry.stop_server("loopback-mcp-observability").unwrap();
        let catalog = registry.service_catalog();
        assert!(
            catalog.services[0]
                .capabilities
                .contains(&"lifecycle_event_count:2".to_string())
        );
    }

    #[test]
    fn full_lifecycle_round_trip_register_start_health_stop_deregister() {
        let mut registry = McpServerRegistry::loopback();
        // Register a new server
        let new_server = McpServerDescriptor {
            server_name: "lifecycle-test-server",
            server_version: "0.1.0",
            implementation_source: "loopback-server-prehost",
            capability_profile: "lifecycle-test-v1",
            selection_key: "lifecycle-test",
            enabled: false,
            health_status: "healthy",
            lifecycle_state: McpServerLifecycleState::Registered,
            tools: vec![McpToolShim::describe()],
        };
        assert!(registry.register_server(new_server));

        // Start
        registry.start_server("lifecycle-test-server").unwrap();
        // Health change
        registry
            .update_server_health("lifecycle-test-server", "degraded")
            .unwrap();
        // Stop
        registry.stop_server("lifecycle-test-server").unwrap();
        // Deregister
        registry.deregister_server("lifecycle-test-server").unwrap();

        // Verify full lifecycle event trail
        let events = registry.lifecycle_events_for("lifecycle-test-server");
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].event_kind, McpLifecycleEventKind::Registered);
        assert_eq!(events[1].event_kind, McpLifecycleEventKind::Started);
        assert_eq!(events[2].event_kind, McpLifecycleEventKind::HealthChanged);
        assert_eq!(events[3].event_kind, McpLifecycleEventKind::Stopped);
        assert_eq!(events[4].event_kind, McpLifecycleEventKind::Deregistered);

        // Verify state transitions are correct
        assert_eq!(events[0].new_state, McpServerLifecycleState::Registered);
        assert_eq!(
            events[1].previous_state,
            McpServerLifecycleState::Registered
        );
        assert_eq!(events[1].new_state, McpServerLifecycleState::Running);
        assert_eq!(events[2].previous_state, McpServerLifecycleState::Running);
        assert_eq!(events[3].previous_state, McpServerLifecycleState::Running);
        assert_eq!(events[3].new_state, McpServerLifecycleState::Stopped);
        assert_eq!(events[4].previous_state, McpServerLifecycleState::Stopped);
        assert_eq!(events[4].new_state, McpServerLifecycleState::Deregistered);

        // Verify the service catalog reflects the final deregistered state
        let catalog = registry.service_catalog();
        let lifecycle_server = catalog
            .services
            .iter()
            .find(|s| s.service_name == "lifecycle-test-server")
            .expect("registered server should appear in catalog");
        assert!(
            lifecycle_server
                .capabilities
                .contains(&"lifecycle_state:deregistered".to_string())
        );
        assert!(
            lifecycle_server
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
    }

    #[test]
    fn manager_supported_operations_include_lifecycle_ops() {
        let registry = McpServerRegistry::loopback();
        let catalog = registry.service_catalog();
        let manager = &catalog.services[0];
        assert!(
            manager
                .supported_operations
                .contains(&"register_server".to_string())
        );
        assert!(
            manager
                .supported_operations
                .contains(&"start_server".to_string())
        );
        assert!(
            manager
                .supported_operations
                .contains(&"stop_server".to_string())
        );
        assert!(
            manager
                .supported_operations
                .contains(&"deregister_server".to_string())
        );
        assert!(
            manager
                .supported_operations
                .contains(&"update_health".to_string())
        );
    }

    #[test]
    fn nonexistent_server_operations_return_errors() {
        let mut registry = McpServerRegistry::loopback();
        assert!(registry.start_server("nonexistent").is_err());
        assert!(registry.stop_server("nonexistent").is_err());
        assert!(registry.deregister_server("nonexistent").is_err());
        assert!(
            registry
                .update_server_health("nonexistent", "healthy")
                .is_err()
        );
    }

    #[test]
    fn typed_manager_client_round_trips_full_lifecycle_over_stateful_transport() {
        let client = JsonRpcMcpManagerClient::new(Arc::new(StatefulRegistryTransport::loopback()));
        let registration: McpManagerServerRegistrationRequest =
            serde_json::from_value(serde_json::json!({
                "server_name": "loopback-mcp-dynamic",
                "server_version": "0.1.0",
                "capability_profile": "dynamic-readonly-v1",
                "selection_key": "dynamic-default"
            }))
            .expect("registration defaults should deserialize");

        let register = client
            .register_server(registration)
            .expect("register_server should succeed");
        assert_eq!(register.operation, "register_server");
        assert_eq!(register.server.service_name, "loopback-mcp-dynamic");
        assert_eq!(
            register.server.implementation_source.as_deref(),
            Some("loopback-server-prehost")
        );
        assert_eq!(
            register.server.selection_key.as_deref(),
            Some("dynamic-default")
        );
        assert_eq!(register.server.service_health.as_deref(), Some("healthy"));
        assert_eq!(
            register.server.service_health_reason.as_deref(),
            Some("server disabled")
        );
        assert!(
            register
                .server
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
        assert!(
            register
                .server
                .capabilities
                .contains(&"lifecycle_state:registered".to_string())
        );
        assert_eq!(register.lifecycle_event_count, 1);
        assert_eq!(register.server_events.len(), 1);
        assert_eq!(
            register
                .lifecycle_event
                .as_ref()
                .expect("register lifecycle event should exist")
                .event_kind,
            McpLifecycleEventKind::Registered
        );

        let list = client.list_servers().expect("list_servers should succeed");
        assert_eq!(list.servers.len(), 3);
        assert!(
            list.selection_targets
                .contains(&"loopback-mcp-dynamic".to_string())
        );
        assert!(
            list.selection_targets
                .contains(&"selection-key:dynamic-default".to_string())
        );

        let describe = client
            .describe_server(McpManagerServerSelectionRequest {
                server_name: "dynamic-default".to_string(),
            })
            .expect("describe_server should succeed");
        assert_eq!(describe.server.service_name, "loopback-mcp-dynamic");
        assert_eq!(describe.lifecycle_events.len(), 1);
        assert_eq!(
            describe.lifecycle_events[0].event_kind,
            McpLifecycleEventKind::Registered
        );

        let start = client
            .start_server(McpManagerServerSelectionRequest {
                server_name: "dynamic-default".to_string(),
            })
            .expect("start_server should succeed");
        assert_eq!(start.lifecycle_event_count, 2);
        assert_eq!(start.server_events.len(), 2);
        assert_eq!(start.server.service_health.as_deref(), Some("healthy"));
        assert!(
            start
                .server
                .capabilities
                .contains(&"server_enabled:true".to_string())
        );
        assert!(
            start
                .server
                .capabilities
                .contains(&"lifecycle_state:running".to_string())
        );
        assert_eq!(
            start
                .lifecycle_event
                .as_ref()
                .expect("start lifecycle event should exist")
                .event_kind,
            McpLifecycleEventKind::Started
        );

        let update_health = client
            .update_health(McpManagerServerHealthUpdateRequest {
                server_name: "loopback-mcp-dynamic".to_string(),
                health_status: "degraded".to_string(),
            })
            .expect("update_health should succeed");
        assert_eq!(update_health.lifecycle_event_count, 3);
        assert_eq!(update_health.server_events.len(), 3);
        assert_eq!(
            update_health.server.service_health.as_deref(),
            Some("degraded")
        );
        assert!(
            update_health
                .server
                .capabilities
                .contains(&"lifecycle_state:running".to_string())
        );
        assert_eq!(
            update_health
                .lifecycle_event
                .as_ref()
                .expect("health lifecycle event should exist")
                .event_kind,
            McpLifecycleEventKind::HealthChanged
        );

        let stop = client
            .stop_server(McpManagerServerSelectionRequest {
                server_name: "dynamic-default".to_string(),
            })
            .expect("stop_server should succeed");
        assert_eq!(stop.lifecycle_event_count, 4);
        assert_eq!(stop.server_events.len(), 4);
        assert_eq!(stop.server.service_health.as_deref(), Some("disabled"));
        assert!(
            stop.server
                .capabilities
                .contains(&"lifecycle_state:stopped".to_string())
        );
        assert_eq!(
            stop.lifecycle_event
                .as_ref()
                .expect("stop lifecycle event should exist")
                .event_kind,
            McpLifecycleEventKind::Stopped
        );

        let deregister = client
            .deregister_server(McpManagerServerSelectionRequest {
                server_name: "loopback-mcp-dynamic".to_string(),
            })
            .expect("deregister_server should succeed");
        assert_eq!(deregister.lifecycle_event_count, 5);
        assert_eq!(deregister.server_events.len(), 5);
        assert_eq!(
            deregister.server.service_health.as_deref(),
            Some("unavailable")
        );
        assert!(
            deregister
                .server
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
        assert!(
            deregister
                .server
                .capabilities
                .contains(&"lifecycle_state:deregistered".to_string())
        );
        assert_eq!(
            deregister
                .lifecycle_event
                .as_ref()
                .expect("deregister lifecycle event should exist")
                .event_kind,
            McpLifecycleEventKind::Deregistered
        );

        let describe_after_deregister = client
            .describe_server(McpManagerServerSelectionRequest {
                server_name: "dynamic-default".to_string(),
            })
            .expect("describe_server should keep exposing lifecycle history");
        assert_eq!(describe_after_deregister.lifecycle_events.len(), 5);
        assert!(
            describe_after_deregister
                .server
                .capabilities
                .contains(&"lifecycle_state:deregistered".to_string())
        );
    }

    #[test]
    fn typed_manager_client_surfaces_health_recovery_lifecycle_transitions() {
        let client = JsonRpcMcpManagerClient::new(Arc::new(StatefulRegistryTransport::loopback()));

        let unavailable = client
            .update_health(McpManagerServerHealthUpdateRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
                health_status: "unavailable".to_string(),
            })
            .expect("unavailable update should succeed");
        assert_eq!(
            unavailable.server.service_health.as_deref(),
            Some("unavailable")
        );
        assert!(
            unavailable
                .server
                .capabilities
                .contains(&"lifecycle_state:failed".to_string())
        );

        let recovered = client
            .update_health(McpManagerServerHealthUpdateRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
                health_status: "healthy".to_string(),
            })
            .expect("healthy update should recover the server");
        assert_eq!(recovered.server.service_health.as_deref(), Some("healthy"));
        assert!(
            recovered
                .server
                .capabilities
                .contains(&"server_enabled:true".to_string())
        );
        assert!(
            recovered
                .server
                .capabilities
                .contains(&"lifecycle_state:running".to_string())
        );
        assert_eq!(
            recovered
                .lifecycle_event
                .as_ref()
                .expect("recovery lifecycle event should exist")
                .previous_state,
            McpServerLifecycleState::Failed
        );
        assert_eq!(
            recovered
                .lifecycle_event
                .as_ref()
                .expect("recovery lifecycle event should exist")
                .new_state,
            McpServerLifecycleState::Running
        );

        let disabled_recovered = client
            .update_health(McpManagerServerHealthUpdateRequest {
                server_name: "loopback-mcp-observability".to_string(),
                health_status: "healthy".to_string(),
            })
            .expect("healthy update on disabled server should succeed");
        assert_eq!(
            disabled_recovered.server.service_health.as_deref(),
            Some("healthy")
        );
        assert!(
            disabled_recovered
                .server
                .capabilities
                .contains(&"server_enabled:false".to_string())
        );
        assert!(
            disabled_recovered
                .server
                .capabilities
                .contains(&"lifecycle_state:registered".to_string())
        );
    }

    #[test]
    fn typed_manager_client_update_health_noop_does_not_replay_stale_lifecycle_event() {
        let client = JsonRpcMcpManagerClient::new(Arc::new(StatefulRegistryTransport::loopback()));

        let unavailable = client
            .update_health(McpManagerServerHealthUpdateRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
                health_status: "unavailable".to_string(),
            })
            .expect("first health transition should succeed");
        assert_eq!(unavailable.lifecycle_event_count, 1);
        assert_eq!(
            unavailable
                .lifecycle_event
                .as_ref()
                .expect("first health transition should emit an event")
                .event_kind,
            McpLifecycleEventKind::HealthChanged
        );

        let repeated = client
            .update_health(McpManagerServerHealthUpdateRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
                health_status: "unavailable".to_string(),
            })
            .expect("same health update should be treated as a no-op");
        assert_eq!(repeated.lifecycle_event_count, 1);
        assert!(repeated.lifecycle_event.is_none());
        assert_eq!(repeated.server_events.len(), 1);
        assert_eq!(
            repeated.server.service_health.as_deref(),
            Some("unavailable")
        );
        assert!(
            repeated
                .server
                .capabilities
                .contains(&"lifecycle_state:failed".to_string())
        );
    }

    #[test]
    fn typed_manager_client_enable_disable_noop_do_not_replay_stale_lifecycle_events() {
        let client = JsonRpcMcpManagerClient::new(Arc::new(StatefulRegistryTransport::loopback()));

        let disabled = client
            .disable_server(McpManagerServerSelectionRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            })
            .expect("first disable should succeed");
        assert_eq!(disabled.lifecycle_event_count, 1);
        assert_eq!(
            disabled
                .lifecycle_event
                .as_ref()
                .expect("first disable should emit an event")
                .event_kind,
            McpLifecycleEventKind::Stopped
        );

        let repeated_disable = client
            .disable_server(McpManagerServerSelectionRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            })
            .expect("repeated disable should be treated as a no-op");
        assert_eq!(repeated_disable.lifecycle_event_count, 1);
        assert!(repeated_disable.lifecycle_event.is_none());
        assert_eq!(repeated_disable.server_events.len(), 1);
        assert_eq!(
            repeated_disable.server.service_health.as_deref(),
            Some("disabled")
        );
        assert!(
            repeated_disable
                .server
                .capabilities
                .contains(&"lifecycle_state:stopped".to_string())
        );

        let enabled = client
            .enable_server(McpManagerServerSelectionRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            })
            .expect("enable after disable should succeed");
        assert_eq!(enabled.lifecycle_event_count, 2);
        assert_eq!(
            enabled
                .lifecycle_event
                .as_ref()
                .expect("enable after disable should emit an event")
                .event_kind,
            McpLifecycleEventKind::Started
        );

        let repeated_enable = client
            .enable_server(McpManagerServerSelectionRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            })
            .expect("repeated enable should be treated as a no-op");
        assert_eq!(repeated_enable.lifecycle_event_count, 2);
        assert!(repeated_enable.lifecycle_event.is_none());
        assert_eq!(repeated_enable.server_events.len(), 2);
        assert_eq!(
            repeated_enable.server.service_health.as_deref(),
            Some("healthy")
        );
        assert!(
            repeated_enable
                .server
                .capabilities
                .contains(&"server_enabled:true".to_string())
        );
        assert!(
            repeated_enable
                .server
                .capabilities
                .contains(&"lifecycle_state:running".to_string())
        );
    }
}
