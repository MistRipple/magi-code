use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, BridgeTransportError, BridgeTransportRequest,
    JsonRpcBridgeServerProbeClient, JsonRpcMcpBridgeClient, JsonRpcMcpManagerClient,
    JsonRpcStdioTransport, McpBridgeClient, McpManagerLifecycleEventKind,
    McpManagerServerHealthUpdateRequest, McpManagerServerRegistrationRequest,
    McpManagerServerSelectionRequest, McpToolCallRequest, LOOPBACK_MCP_SERVER_NAME,
    LOOPBACK_MCP_TOOL_NAME,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn loopback_transport() -> JsonRpcStdioTransport {
    let mut path = std::env::current_exe().expect("current exe should exist");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("mcp_bridge_loopback");
    JsonRpcStdioTransport::new(path.to_string_lossy().to_string())
}

fn loopback_transport_with_env(envs: &[(&str, &str)]) -> JsonRpcStdioTransport {
    envs.iter()
        .fold(loopback_transport(), |transport, (key, value)| {
            transport.with_env(*key, *value)
        })
}

#[test]
fn mcp_client_falls_back_to_default_server_when_selection_is_blank() {
    let client = JsonRpcMcpBridgeClient::new(Arc::new(loopback_transport()));

    let response = client
        .call_tool(McpToolCallRequest {
            server_name: "   ".to_string(),
            tool_name: LOOPBACK_MCP_TOOL_NAME.to_string(),
            input: "{}".to_string(),
        })
        .expect("blank selection should use default server");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["server_name"], LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(payload["default_server"], LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(payload["selection_strategy"], "explicit-or-selection-key");
}

#[test]
fn mcp_client_round_trips_echo_inspect() {
    let client = JsonRpcMcpBridgeClient::new(Arc::new(loopback_transport()));

    let response = client
        .call_tool(McpToolCallRequest {
            server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            tool_name: LOOPBACK_MCP_TOOL_NAME.to_string(),
            input: r#"{"message":"hello"}"#.to_string(),
        })
        .expect("mcp echo.inspect should succeed");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["server_name"], LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(payload["tool_name"], LOOPBACK_MCP_TOOL_NAME);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["input"], r#"{"message":"hello"}"#);
    assert_eq!(payload["implementation_source"], "loopback-server-prehost");
    assert_eq!(payload["capability_profile"], "inspection-core-v1");
    assert_eq!(payload["selection_key"], "inspection-default");
    assert_eq!(payload["default_server"], LOOPBACK_MCP_SERVER_NAME);
}

#[test]
fn mcp_client_round_trips_echo_describe() {
    let client = JsonRpcMcpBridgeClient::new(Arc::new(loopback_transport()));

    let response = client
        .call_tool(McpToolCallRequest {
            server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            tool_name: "echo.describe".to_string(),
            input: "{}".to_string(),
        })
        .expect("mcp echo.describe should succeed");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["tool_name"], "echo.describe");
    assert_eq!(payload["tool_count"], 2);
    assert_eq!(payload["known_tools"][0], LOOPBACK_MCP_TOOL_NAME);
    assert_eq!(payload["default_server"], LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(payload["default_route_status"], "ready");
    assert_eq!(payload["manager_health"], "healthy");
}

#[test]
fn mcp_loopback_exposes_shared_handshake_and_health() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport()));

    let handshake = probe.handshake().expect("mcp handshake should succeed");
    assert_eq!(handshake.server_kind, BridgeServerKind::Mcp);
    assert!(
        handshake
            .supported_methods
            .contains(&"mcp.call_tool".to_string())
    );
    assert!(
        handshake
            .supported_methods
            .contains(&"mcp.list_servers".to_string())
    );
    assert!(
        handshake
            .supported_methods
            .contains(&"mcp.enable_server".to_string())
    );
    assert!(
        handshake
            .supported_methods
            .contains(&"mcp.update_health".to_string())
    );

    let health = probe.health().expect("mcp health should succeed");
    assert_eq!(health.server_kind, BridgeServerKind::Mcp);
    assert!(health.ok);

    let catalog = probe
        .describe_services()
        .expect("mcp service catalog should succeed");
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
            .service_health
            .as_deref()
            .expect("manager service health should exist"),
        "healthy"
    );
    assert_eq!(
        catalog.services[0]
            .default_route_status
            .as_deref()
            .expect("default route status should exist"),
        "ready"
    );
    assert_eq!(
        catalog.services[0]
            .default_route_target
            .as_deref()
            .expect("default route target should exist"),
        LOOPBACK_MCP_SERVER_NAME
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
            .supported_operations
            .contains(&"enable_server".to_string())
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"implementation_source:loopback-manager-prehost".to_string())
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
            .contains(&"registry_config_status:clean".to_string())
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"config_issue_count:0".to_string())
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
            .contains(&"service_health:healthy".to_string())
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"default_route_status:ready".to_string())
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"selection_target_count:4".to_string())
    );
    assert_eq!(catalog.services[1].service_name, LOOPBACK_MCP_SERVER_NAME);
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
            .contains(&"implementation_source:loopback-server-prehost".to_string())
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
            .contains(&format!("tool:{LOOPBACK_MCP_TOOL_NAME}"))
    );
    assert!(
        catalog.services[1]
            .capabilities
            .contains(&"tool:echo.describe".to_string())
    );
    assert_eq!(catalog.services[2].service_name, "loopback-mcp-observability");
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
            .contains(&"server_enabled:false".to_string())
    );
    assert!(
        catalog.services[2]
            .capabilities
            .contains(&"implementation_source:loopback-server-prehost".to_string())
    );
    assert!(
        catalog.services[2]
            .supported_operations
            .contains(&"describe_server".to_string())
    );
    assert!(
        catalog.services[2]
            .capabilities
            .contains(&"selection_key:observability-default".to_string())
    );
}

#[test]
fn mcp_catalog_reflects_env_configured_default_server_and_route_health() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
        (
            "MAGI_MCP_MANAGER_ENABLED_SERVERS",
            "loopback-mcp-observability",
        ),
        ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
        (
            "MAGI_MCP_MANAGER_SERVER_HEALTHS",
            "loopback-mcp=degraded,loopback-mcp-observability=healthy",
        ),
    ])));

    let catalog = probe
        .describe_services()
        .expect("mcp service catalog should succeed");
    assert_eq!(
        catalog.services[0].default_server.as_deref(),
        Some("loopback-mcp-observability")
    );
    assert_eq!(
        catalog.services[0].default_server_health.as_deref(),
        Some("healthy")
    );
    assert_eq!(
        catalog.services[0].default_server_selection_key.as_deref(),
        Some("observability-default")
    );
    assert_eq!(
        catalog.services[0].service_health.as_deref(),
        Some("healthy")
    );
    assert_eq!(
        catalog.services[0].default_route_status.as_deref(),
        Some("ready")
    );
    assert_eq!(
        catalog.services[0].default_route_target.as_deref(),
        Some("loopback-mcp-observability")
    );
}

#[test]
fn blank_selection_uses_env_configured_default_server_when_available() {
    let client = JsonRpcMcpBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "observability-default"),
        (
            "MAGI_MCP_MANAGER_ENABLED_SERVERS",
            "loopback-mcp-observability",
        ),
        ("MAGI_MCP_MANAGER_DISABLED_SERVERS", ""),
        (
            "MAGI_MCP_MANAGER_SERVER_HEALTHS",
            "loopback-mcp=degraded,loopback-mcp-observability=healthy",
        ),
    ])));

    let response = client
        .call_tool(McpToolCallRequest {
            server_name: " ".to_string(),
            tool_name: "echo.describe".to_string(),
            input: "{}".to_string(),
        })
        .expect("blank selection should use configured default server");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["server_name"], "loopback-mcp-observability");
    assert_eq!(payload["default_server"], "loopback-mcp-observability");
    assert_eq!(payload["default_route_status"], "ready");
    assert_eq!(payload["manager_health"], "healthy");
}

#[test]
fn mcp_catalog_reports_unavailable_when_all_servers_are_disabled() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_MCP_MANAGER_DISABLED_SERVERS",
        "loopback-mcp,loopback-mcp-observability",
    )])));

    let catalog = probe
        .describe_services()
        .expect("mcp service catalog should succeed");
    assert_eq!(catalog.services[0].default_server, None);
    assert_eq!(catalog.services[0].default_server_health, None);
    assert_eq!(catalog.services[0].default_server_selection_key, None);
    assert_eq!(
        catalog.services[0].service_health.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        catalog.services[0].default_route_status.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        catalog.services[0].default_route_target.as_deref(),
        Some("<none>")
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"default_server:<none>".to_string())
    );
}

#[test]
fn mcp_catalog_reports_degraded_when_default_server_config_is_invalid_but_fallback_exists() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_MCP_MANAGER_DEFAULT_SERVER",
        "missing-server",
    )])));

    let catalog = probe
        .describe_services()
        .expect("mcp service catalog should succeed");
    assert_eq!(
        catalog.services[0].service_health.as_deref(),
        Some("degraded")
    );
    assert_eq!(
        catalog.services[0].service_health_reason.as_deref(),
        Some(
            "registry config issues: default server target missing-server does not match any registered server"
        )
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"registry_config_status:misconfigured".to_string())
    );
    assert!(
        catalog.services[0]
            .capabilities
            .contains(&"config_issue_count:1".to_string())
    );
    assert_eq!(
        catalog.services[0].default_route_status.as_deref(),
        Some("ready")
    );
    assert_eq!(
        catalog.services[0].default_route_target.as_deref(),
        Some("loopback-mcp")
    );
}

#[test]
fn mcp_catalog_reports_unavailable_when_registry_config_is_invalid_and_no_fallback_exists() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_MCP_MANAGER_DEFAULT_SERVER", "missing-server"),
        (
            "MAGI_MCP_MANAGER_DISABLED_SERVERS",
            "loopback-mcp,loopback-mcp-observability",
        ),
    ])));

    let catalog = probe
        .describe_services()
        .expect("mcp service catalog should succeed");
    assert_eq!(
        catalog.services[0].service_health.as_deref(),
        Some("unavailable")
    );
    assert!(
        catalog.services[0]
            .service_health_reason
            .as_deref()
            .expect("service health reason should exist")
            .contains("default server target missing-server does not match any registered server")
    );
    assert_eq!(
        catalog.services[0].default_route_status.as_deref(),
        Some("unavailable")
    );
}

#[test]
fn mcp_client_can_select_server_via_registry_selection_key() {
    let client = JsonRpcMcpBridgeClient::new(Arc::new(loopback_transport()));

    let response = client
        .call_tool(McpToolCallRequest {
            server_name: "inspection-default".to_string(),
            tool_name: LOOPBACK_MCP_TOOL_NAME.to_string(),
            input: "{}".to_string(),
        })
        .expect("selection key should route to canonical server");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["server_name"], LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(payload["requested_server"], "inspection-default");
    assert_eq!(payload["server_manifest"], "loopback-mcp@1.2.0-loopback");
    assert_eq!(payload["selection_key"], "inspection-default");
    assert_eq!(payload["default_server"], LOOPBACK_MCP_SERVER_NAME);
}

#[test]
fn mcp_manager_list_and_describe_servers_are_callable_over_json_rpc() {
    let client = JsonRpcMcpManagerClient::new(Arc::new(loopback_transport()));

    let list = client
        .list_servers()
        .expect("list_servers should succeed over json-rpc");
    assert_eq!(list.manager.service_name, "loopback-mcp-manager");
    assert_eq!(list.servers.len(), 2);
    assert_eq!(list.servers[0].service_name, LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(list.servers[1].service_name, "loopback-mcp-observability");
    assert_eq!(list.default_route_status, "ready");

    let describe = client
        .describe_server(McpManagerServerSelectionRequest {
            server_name: "observability-default".to_string(),
        })
        .expect("describe_server should succeed over json-rpc");
    assert_eq!(describe.server.service_name, "loopback-mcp-observability");
    assert_eq!(describe.server.service_health.as_deref(), Some("disabled"));
    assert!(describe.lifecycle_events.is_empty());
}

#[test]
fn mcp_manager_enable_and_disable_servers_are_callable_over_json_rpc() {
    let client = JsonRpcMcpManagerClient::new(Arc::new(loopback_transport()));

    let enable = client
        .enable_server(McpManagerServerSelectionRequest {
            server_name: "loopback-mcp-observability".to_string(),
        })
        .expect("enable_server should succeed over json-rpc");
    assert_eq!(enable.operation, "enable_server");
    assert_eq!(enable.server.service_name, "loopback-mcp-observability");
    assert_eq!(enable.server.service_health.as_deref(), Some("healthy"));
    assert!(
        enable
            .server
            .capabilities
            .contains(&"server_enabled:true".to_string())
    );
    assert_eq!(
        enable
            .lifecycle_event
            .as_ref()
            .expect("enable lifecycle event should exist")
            .event_kind,
        McpManagerLifecycleEventKind::Started
    );

    let disable = client
        .disable_server(McpManagerServerSelectionRequest {
            server_name: "inspection-default".to_string(),
        })
        .expect("disable_server should succeed over json-rpc");
    assert_eq!(disable.operation, "disable_server");
    assert_eq!(disable.server.service_name, LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(disable.server.service_health.as_deref(), Some("disabled"));
    assert!(
        disable
            .server
            .capabilities
            .contains(&"lifecycle_state:stopped".to_string())
    );
    assert_eq!(
        disable
            .lifecycle_event
            .as_ref()
            .expect("disable lifecycle event should exist")
            .event_kind,
        McpManagerLifecycleEventKind::Stopped
    );
}

#[test]
fn mcp_manager_start_stop_update_health_and_register_are_callable_over_json_rpc() {
    let client = JsonRpcMcpManagerClient::new(Arc::new(loopback_transport()));

    let start = client
        .start_server(McpManagerServerSelectionRequest {
            server_name: "loopback-mcp-observability".to_string(),
        })
        .expect("start_server should succeed over json-rpc");
    assert_eq!(start.server.service_name, "loopback-mcp-observability");
    assert_eq!(start.server.service_health.as_deref(), Some("healthy"));
    assert_eq!(
        start
            .lifecycle_event
            .as_ref()
            .expect("start lifecycle event should exist")
            .event_kind,
        McpManagerLifecycleEventKind::Started
    );

    let stop = client
        .stop_server(McpManagerServerSelectionRequest {
            server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
        })
        .expect("stop_server should succeed over json-rpc");
    assert_eq!(stop.server.service_name, LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(stop.server.service_health.as_deref(), Some("disabled"));
    assert_eq!(
        stop.lifecycle_event
            .as_ref()
            .expect("stop lifecycle event should exist")
            .event_kind,
        McpManagerLifecycleEventKind::Stopped
    );

    let health = client
        .update_health(McpManagerServerHealthUpdateRequest {
            server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
            health_status: "unavailable".to_string(),
        })
        .expect("update_health should succeed over json-rpc");
    assert_eq!(health.server.service_name, LOOPBACK_MCP_SERVER_NAME);
    assert_eq!(health.server.service_health.as_deref(), Some("unavailable"));
    assert!(
        health
            .server
            .capabilities
            .contains(&"lifecycle_state:failed".to_string())
    );
    assert_eq!(
        health
            .lifecycle_event
            .as_ref()
            .expect("health lifecycle event should exist")
            .event_kind,
        McpManagerLifecycleEventKind::HealthChanged
    );

    let register = client
        .register_server(McpManagerServerRegistrationRequest {
            server_name: "loopback-mcp-dynamic".to_string(),
            server_version: "0.1.0".to_string(),
            capability_profile: "dynamic-readonly-v1".to_string(),
            selection_key: "dynamic-default".to_string(),
            implementation_source: "loopback-server-prehost".to_string(),
            health_status: "healthy".to_string(),
            enabled: true,
            tool_names: vec!["echo.describe".to_string()],
        })
        .expect("register_server should succeed over json-rpc");
    assert_eq!(register.server.service_name, "loopback-mcp-dynamic");
    assert_eq!(
        register.server.selection_key.as_deref(),
        Some("dynamic-default")
    );
    assert_eq!(
        register
            .lifecycle_event
            .as_ref()
            .expect("register lifecycle event should exist")
            .event_kind,
        McpManagerLifecycleEventKind::Registered
    );
}

#[test]
fn unsupported_method_returns_protocol_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.not_supported".to_string(),
            params: json!({
                "server_name": LOOPBACK_MCP_SERVER_NAME,
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}"
            }),
        })
        .expect_err("unsupported method should return protocol error");

    assert!(matches!(error, BridgeTransportError::Protocol { .. }));
}

#[test]
fn unknown_server_returns_remote_business_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({
                "server_name": "other-mcp",
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}"
            }),
        })
        .expect_err("unknown server should return remote business error");

    match error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32011);
            assert_eq!(message, "unknown server");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn unsupported_tool_returns_remote_business_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({
                "server_name": LOOPBACK_MCP_SERVER_NAME,
                "tool_name": "other.inspect",
                "input": "{}"
            }),
        })
        .expect_err("unsupported tool should return remote business error");

    match error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32012);
            assert_eq!(message, "unsupported tool");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn mcp_client_blank_selection_rejects_unavailable_enabled_server() {
    let transport = loopback_transport_with_env(&[
        ("MAGI_MCP_MANAGER_ENABLED_SERVERS", LOOPBACK_MCP_SERVER_NAME),
        (
            "MAGI_MCP_MANAGER_DISABLED_SERVERS",
            "loopback-mcp-observability",
        ),
        ("MAGI_MCP_MANAGER_SERVER_HEALTHS", "loopback-mcp=unavailable"),
    ]);
    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({
                "server_name": "   ",
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}"
            }),
        })
        .expect_err("blank selection should reject unavailable enabled server");

    match error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32015);
            assert_eq!(message, "default server unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn mcp_blank_selection_error_reports_no_default_server_instead_of_manager_name() {
    let transport = loopback_transport_with_env(&[(
        "MAGI_MCP_MANAGER_DISABLED_SERVERS",
        "loopback-mcp,loopback-mcp-observability",
    )]);
    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({
                "server_name": "   ",
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}"
            }),
        })
        .expect_err("blank selection should fail when there is no routable default server");

    match error {
        BridgeTransportError::RemoteBusiness {
            code,
            message,
            data,
        } => {
            assert_eq!(code, -32015);
            assert_eq!(message, "default server unavailable");
            let data = data.expect("blank selection error should include data");
            assert_eq!(data["manager_name"], "loopback-mcp-manager");
            assert_eq!(data["default_server"], Value::Null);
            assert_eq!(data["default_route_status"], "unavailable");
            assert_eq!(data["default_route_target"], "<none>");
            assert_eq!(data["manager_health"], "unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn mcp_client_explicit_call_rejects_unavailable_server_even_when_enabled() {
    let transport = loopback_transport_with_env(&[
        ("MAGI_MCP_MANAGER_ENABLED_SERVERS", LOOPBACK_MCP_SERVER_NAME),
        (
            "MAGI_MCP_MANAGER_DISABLED_SERVERS",
            "loopback-mcp-observability",
        ),
        ("MAGI_MCP_MANAGER_SERVER_HEALTHS", "loopback-mcp=unavailable"),
    ]);
    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({
                "server_name": LOOPBACK_MCP_SERVER_NAME,
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}"
            }),
        })
        .expect_err("explicit selection should reject unavailable server");

    match error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32016);
            assert_eq!(message, "server unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn broken_subprocess_returns_transport_error() {
    let transport =
        JsonRpcStdioTransport::new("sh").with_args(vec!["-c".to_string(), "exit 2".to_string()]);

    let error = transport
        .call(BridgeTransportRequest {
            method: "mcp.call_tool".to_string(),
            params: json!({
                "server_name": LOOPBACK_MCP_SERVER_NAME,
                "tool_name": LOOPBACK_MCP_TOOL_NAME,
                "input": "{}"
            }),
        })
        .expect_err("non-zero exit should be transport error");

    assert!(matches!(error, BridgeTransportError::Transport { .. }));
}
