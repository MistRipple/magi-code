use super::super::bridges::{
    BridgeCutoverBlockingFacet, BridgeCutoverBlockingReasonCode, BridgeCutoverSmokeProvider,
    BridgePreflightProvider, BridgeProbeErrorDto, BridgeSnapshotProvider,
};
use super::*;
use magi_bridge_client::{
    BridgeErrorLayer, BridgeResponse, BridgeServerHandshake, BridgeServerHealth, BridgeServerKind,
    BridgeServerServiceCatalog, BridgeServerServiceDescriptor, BridgeTransport,
    BridgeTransportError, BridgeTransportRequest, BridgeTransportResponse,
    LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD, LOCAL_BRIDGE_HANDSHAKE_METHOD,
    LOCAL_BRIDGE_HEALTH_METHOD, LOOPBACK_MCP_SERVER_NAME, LOOPBACK_MCP_TOOL_NAME,
    LOOPBACK_MODEL_PROVIDER, McpManagerListServersResponse,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
enum FakeTransportOutcome {
    Payload(Value),
    RemoteBusiness {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    Protocol {
        message: String,
    },
}

struct FakeTransport {
    responses: Mutex<HashMap<String, FakeTransportOutcome>>,
}

impl FakeTransport {
    fn new(responses: HashMap<String, FakeTransportOutcome>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

impl BridgeTransport for FakeTransport {
    fn call(
        &self,
        request: BridgeTransportRequest,
    ) -> Result<BridgeTransportResponse, BridgeTransportError> {
        let responses = self.responses.lock().expect("responses lock should hold");
        let outcome = responses.get(&request.method).cloned().unwrap_or_else(|| {
            FakeTransportOutcome::Protocol {
                message: format!("unexpected method {}", request.method),
            }
        });
        match outcome {
            FakeTransportOutcome::Payload(payload) => Ok(BridgeTransportResponse { payload }),
            FakeTransportOutcome::RemoteBusiness {
                code,
                message,
                data,
            } => Err(BridgeTransportError::RemoteBusiness {
                code,
                message,
                data,
            }),
            FakeTransportOutcome::Protocol { message } => {
                Err(BridgeTransportError::Protocol { message })
            }
        }
    }
}

fn handshake(kind: BridgeServerKind) -> Value {
    serde_json::to_value(BridgeServerHandshake {
        protocol_version: "local-bridge-v1".to_string(),
        server_kind: kind,
        health_method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
        supported_methods: vec!["bridge.describe_services".to_string()],
    })
    .expect("handshake should serialize")
}

fn health(kind: BridgeServerKind, status: &str, ok: bool) -> Value {
    serde_json::to_value(BridgeServerHealth {
        protocol_version: "local-bridge-v1".to_string(),
        server_kind: kind,
        status: status.to_string(),
        ok,
    })
    .expect("health should serialize")
}

fn catalog(kind: BridgeServerKind) -> Value {
    serde_json::to_value(BridgeServerServiceCatalog {
        protocol_version: "local-bridge-v1".to_string(),
        server_kind: kind,
        services: vec![],
    })
    .expect("catalog should serialize")
}

fn descriptor(service_name: &str) -> BridgeServerServiceDescriptor {
    descriptor_with_health(service_name, None)
}

fn descriptor_with_health(
    service_name: &str,
    service_health: Option<&str>,
) -> BridgeServerServiceDescriptor {
    descriptor_with_details(service_name, service_health, None, None, None)
}

fn descriptor_with_profile(
    service_name: &str,
    service_health: Option<&str>,
    capability_profile: Option<&str>,
) -> BridgeServerServiceDescriptor {
    descriptor_with_details(service_name, service_health, capability_profile, None, None)
}

fn descriptor_with_route(
    service_name: &str,
    default_route_status: &str,
    default_route_target: &str,
) -> BridgeServerServiceDescriptor {
    descriptor_with_details(
        service_name,
        None,
        None,
        Some(default_route_status),
        Some(default_route_target),
    )
}

fn descriptor_with_details(
    service_name: &str,
    service_health: Option<&str>,
    capability_profile: Option<&str>,
    default_route_status: Option<&str>,
    default_route_target: Option<&str>,
) -> BridgeServerServiceDescriptor {
    BridgeServerServiceDescriptor {
        service_name: service_name.to_string(),
        shim_kind: format!("{service_name}-shim"),
        supported_operations: vec![],
        capabilities: vec![],
        service_health: service_health.map(str::to_string),
        service_health_reason: None,
        implementation_source: None,
        capability_profile: capability_profile.map(str::to_string),
        workspace_roots_source: None,
        manager_version: None,
        registry_profile: None,
        registry_manifest: None,
        selection_strategy: None,
        default_server: None,
        default_server_health: None,
        default_server_selection_key: None,
        default_route_status: default_route_status.map(str::to_string),
        default_route_target: default_route_target.map(str::to_string),
        selection_targets: None,
        selection_key: None,
        server_manifest: None,
        shell_manifest: None,
        shell_profile: None,
        command_capability_profiles: None,
        session_descriptor: None,
        workspace_context: None,
        context_resolution_boundary: None,
    }
}

fn bridge_response(payload: &str) -> Value {
    serde_json::to_value(BridgeResponse {
        ok: true,
        payload: payload.to_string(),
    })
    .expect("bridge response should serialize")
}

fn bridge_response_with_status(ok: bool, payload: &str) -> Value {
    serde_json::to_value(BridgeResponse {
        ok,
        payload: payload.to_string(),
    })
    .expect("bridge response should serialize")
}

fn structured_bridge_response(payload: Value) -> Value {
    serde_json::to_value(BridgeResponse {
        ok: true,
        payload: payload.to_string(),
    })
    .expect("structured bridge response should serialize")
}

fn mcp_list_response(default_route_status: &str, default_route_target: &str) -> Value {
    serde_json::to_value(McpManagerListServersResponse {
        manager: descriptor_with_route(
            "loopback-mcp-manager",
            default_route_status,
            default_route_target,
        ),
        servers: vec![
            descriptor_with_profile(
                LOOPBACK_MCP_SERVER_NAME,
                Some("ready"),
                Some("inspection-core-v1"),
            ),
            descriptor_with_profile(
                "loopback-mcp-observability",
                Some("ready"),
                Some("observability-v1"),
            ),
        ],
        selection_targets: vec![
            LOOPBACK_MCP_SERVER_NAME.to_string(),
            "loopback-mcp-observability".to_string(),
        ],
        default_route_status: default_route_status.to_string(),
        default_route_target: default_route_target.to_string(),
    })
    .expect("list response should serialize")
}

#[test]
fn probe_snapshot_provider_collects_bridge_probe_exports() {
    let mut provider = BridgeProbeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                LOCAL_BRIDGE_HANDSHAKE_METHOD.to_string(),
                FakeTransportOutcome::Payload(handshake(BridgeServerKind::Model)),
            ),
            (
                LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
                FakeTransportOutcome::Payload(health(BridgeServerKind::Model, "healthy", true)),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(catalog(BridgeServerKind::Model)),
            ),
        ]))),
    );

    let snapshot = provider.services_snapshot();
    assert_eq!(snapshot.services.len(), 1);
    let service = &snapshot.services[0];
    assert_eq!(service.server_kind, BridgeServerKind::Model);
    assert!(service.handshake.is_some());
    assert!(service.health.is_some());
    assert!(service.service_catalog.is_some());
    assert!(service.handshake_error.is_none());
    assert!(service.health_error.is_none());
    assert!(service.service_catalog_error.is_none());
}

#[test]
fn probe_snapshot_provider_preserves_partial_failures() {
    let mut provider = BridgeProbeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                LOCAL_BRIDGE_HANDSHAKE_METHOD.to_string(),
                FakeTransportOutcome::Payload(handshake(BridgeServerKind::Model)),
            ),
            (
                LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32042,
                    message: "probe degraded".to_string(),
                    data: Some(json!({ "service": "vscode" })),
                },
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(catalog(BridgeServerKind::Model)),
            ),
        ]))),
    );

    let snapshot = provider.services_snapshot();
    let service = &snapshot.services[0];
    assert!(service.handshake.is_some());
    assert!(service.health.is_none());
    assert!(service.service_catalog.is_some());
    assert_eq!(
        service.health_error,
        Some(BridgeProbeErrorDto {
            layer: Some(BridgeErrorLayer::RemoteBusiness),
            code: Some(-32042),
            message: "桥接调用失败[RemoteBusiness]: remote business error [-32042]: probe degraded"
                .to_string(),
        })
    );
}

#[test]
fn preflight_snapshot_provider_executes_real_smoke_checks_from_transports() {
    let mut provider = BridgePreflightSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::Payload(bridge_response(
                    "loopback-model::bridge preflight ping",
                )),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![descriptor(LOOPBACK_MODEL_PROVIDER)],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(McpManagerListServersResponse {
                        manager: descriptor("loopback-mcp-manager"),
                        servers: vec![descriptor(LOOPBACK_MCP_SERVER_NAME)],
                        selection_targets: vec![LOOPBACK_MCP_SERVER_NAME.to_string()],
                        default_route_status: "available".to_string(),
                        default_route_target: LOOPBACK_MCP_SERVER_NAME.to_string(),
                    })
                    .expect("list_servers should serialize"),
                ),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(bridge_response("echo.inspect::ok")),
            ),
        ]))),
    );

    let snapshot = provider.preflight_snapshot();
    assert_eq!(snapshot.services.len(), 2);
    assert_eq!(
        snapshot.services[0].checks[0].target,
        LOOPBACK_MODEL_PROVIDER
    );
    assert!(snapshot.services[0].checks[0].ok);
    assert_eq!(snapshot.services[1].checks[0].check_name, "list_servers");
    assert!(snapshot.services[1].checks[0].ok);
    assert_eq!(
        snapshot.services[1].checks[1].target,
        format!("{LOOPBACK_MCP_SERVER_NAME}.{LOOPBACK_MCP_TOOL_NAME}")
    );
}

#[test]
fn preflight_snapshot_provider_includes_openai_compatible_smoke_when_model_catalog_is_ready() {
    let mut provider = BridgePreflightSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::Payload(bridge_response("bridge preflight ping")),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor(LOOPBACK_MODEL_PROVIDER),
                            descriptor_with_health("openai-compatible", Some("ready")),
                        ],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );

    let snapshot = provider.preflight_snapshot();
    let checks = &snapshot.services[0].checks;
    assert_eq!(checks.len(), 2, "unexpected model preflight: {checks:?}");
    assert!(
        checks
            .iter()
            .any(|check| check.target == LOOPBACK_MODEL_PROVIDER && check.ok),
        "loopback-model smoke should still be present: {checks:?}"
    );
    assert!(
        checks
            .iter()
            .any(|check| check.target == "openai-compatible" && check.ok),
        "ready openai-compatible smoke should be appended: {checks:?}"
    );
}

#[test]
fn preflight_snapshot_provider_preserves_smoke_failures() {
    let mut provider = BridgePreflightSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32006,
                    message: "provider rejected".to_string(),
                    data: None,
                },
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![descriptor(LOOPBACK_MODEL_PROVIDER)],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );

    let snapshot = provider.preflight_snapshot();
    let check = &snapshot.services[0].checks[0];
    assert!(!check.ok);
    assert_eq!(check.check_name, "invoke");
    assert_eq!(check.target, LOOPBACK_MODEL_PROVIDER);
    assert_eq!(
        check.error,
        Some(BridgeProbeErrorDto {
            layer: Some(BridgeErrorLayer::RemoteBusiness),
            code: Some(-32006),
            message:
                "桥接调用失败[RemoteBusiness]: remote business error [-32006]: provider rejected"
                    .to_string(),
        })
    );
}

#[test]
fn cutover_smoke_snapshot_provider_evaluates_ready_model_and_mcp_contracts() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "content": "hello from provider",
                    "finish_reason": "tool_calls",
                    "usage": {
                        "total_tokens": 17,
                    },
                    "tool_calls": [{
                        "function": {
                            "name": "demo.lookup",
                            "arguments": "{\"city\":\"Paris\"}",
                        }
                    }],
                }))),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor_with_profile(
                                LOOPBACK_MODEL_PROVIDER,
                                Some("ready"),
                                Some("model-bridge-payload-v1"),
                            ),
                            descriptor_with_profile(
                                "openai-compatible",
                                Some("ready"),
                                Some("openai-compatible-chat-completions-v1"),
                            ),
                        ],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "ready",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::Payload(json!({
                    "manager": descriptor_with_route(
                        "loopback-mcp-manager",
                        "ready",
                        "loopback-mcp-observability",
                    ),
                    "server": descriptor_with_profile(
                        "loopback-mcp-observability",
                        Some("ready"),
                        Some("observability-v1"),
                    ),
                    "lifecycle_events": [],
                })),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "server_name": "loopback-mcp-observability",
                    "default_route_status": "ready",
                    "default_route_target": "loopback-mcp-observability",
                    "tool_name": "echo.describe",
                }))),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert_eq!(snapshot.services.len(), 2);
    assert!(snapshot.overall_ok);
    assert_eq!(snapshot.checked_service_count, 2);
    assert_eq!(snapshot.blocking_check_count, 0);
    assert!(snapshot.blocking_services.is_empty());
    assert!(snapshot.blocking_issues.is_empty());

    let model = snapshot
        .services
        .iter()
        .find(|service| service.server_kind == BridgeServerKind::Model)
        .expect("model cutover should exist");
    assert!(model.service_ok);
    assert_eq!(model.blocking_check_count, 0);
    assert!(model.blocking_targets.is_empty());
    assert_eq!(model.checks.len(), 2);
    let openai = model
        .checks
        .iter()
        .find(|check| check.target == "openai-compatible")
        .expect("openai-compatible contract should exist");
    assert!(openai.ok, "model contract should pass: {openai:?}");
    let model_contract = openai
        .model_contract
        .as_ref()
        .expect("model contract should be attached");
    assert_eq!(
        model_contract.contract_profile,
        "openai-compatible-chat-completions-v1"
    );
    assert_eq!(model_contract.payload_kind, "structured_json");
    assert!(model_contract.has_content);
    assert!(model_contract.has_finish_reason);
    assert!(model_contract.has_usage);
    assert_eq!(model_contract.tool_call_count, 1);

    let mcp = snapshot
        .services
        .iter()
        .find(|service| service.server_kind == BridgeServerKind::Mcp)
        .expect("mcp cutover should exist");
    assert!(mcp.service_ok);
    assert_eq!(mcp.blocking_check_count, 0);
    assert!(mcp.blocking_targets.is_empty());
    let mcp_gate = mcp
        .mcp_default_route_gate
        .as_ref()
        .expect("mcp route gate should be attached");
    assert_eq!(mcp_gate.route_status, "ready");
    assert_eq!(mcp_gate.route_target, "loopback-mcp-observability");
    assert_eq!(
        mcp_gate.resolved_server.as_deref(),
        Some("loopback-mcp-observability")
    );
    assert!(mcp_gate.contract_ok);
    let check = &mcp.checks[0];
    assert!(check.ok, "mcp contract should pass: {check:?}");
    let contract = check
        .mcp_contract
        .as_ref()
        .expect("mcp contract should be attached");
    assert_eq!(contract.route_status, "ready");
    assert_eq!(contract.route_target, "loopback-mcp-observability");
    assert_eq!(
        contract.resolved_server.as_deref(),
        Some("loopback-mcp-observability")
    );
    assert!(contract.describe_ok);
    assert!(contract.blank_selection_ok);
    assert!(contract.contract_ok);
}

#[test]
fn cutover_smoke_snapshot_provider_blocks_invalid_model_payload_contract() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "finish_reason": "stop",
                    "usage": {
                        "total_tokens": 5,
                    },
                }))),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor_with_profile(
                                LOOPBACK_MODEL_PROVIDER,
                                Some("ready"),
                                Some("model-bridge-payload-v1"),
                            ),
                            descriptor_with_profile(
                                "openai-compatible",
                                Some("ready"),
                                Some("openai-compatible-chat-completions-v1"),
                            ),
                        ],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.checked_service_count, 1);
    assert_eq!(snapshot.blocking_check_count, 2);
    assert_eq!(snapshot.blocking_services, vec![BridgeServerKind::Model]);
    assert_eq!(snapshot.blocking_issues.len(), 2);
    assert!(snapshot.blocking_issues.iter().all(|issue| {
        issue.server_kind == BridgeServerKind::Model
            && issue.facet == BridgeCutoverBlockingFacet::ModelContract
            && issue.reason_code
                == BridgeCutoverBlockingReasonCode::ModelStructuredPayloadMissingContentOrToolCalls
    }));
    let model = snapshot
        .services
        .iter()
        .find(|service| service.server_kind == BridgeServerKind::Model)
        .expect("model cutover should exist");
    assert!(!model.service_ok);
    assert_eq!(model.blocking_check_count, 2);
    assert_eq!(
        model.blocking_targets,
        vec![
            LOOPBACK_MODEL_PROVIDER.to_string(),
            "openai-compatible".to_string()
        ]
    );
    let openai = model
        .checks
        .iter()
        .find(|check| check.target == "openai-compatible")
        .expect("openai-compatible contract should exist");
    let openai_issue = snapshot
        .blocking_issues
        .iter()
        .find(|issue| issue.target == "openai-compatible")
        .expect("openai-compatible blocking issue should exist");
    assert_eq!(openai_issue.check_name, "invoke_contract");
    assert_eq!(
        openai_issue.reason_code,
        BridgeCutoverBlockingReasonCode::ModelStructuredPayloadMissingContentOrToolCalls
    );
    assert!(!openai.ok);
    assert_eq!(
        openai.blocking_reason.as_deref(),
        Some("structured payload missing content or tool_calls")
    );
    assert_eq!(
        openai
            .model_contract
            .as_ref()
            .expect("model contract should exist")
            .contract_ok,
        false
    );
}

#[test]
fn cutover_smoke_snapshot_provider_does_not_skip_degraded_openai_compatible() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32003,
                    message: "provider unavailable".to_string(),
                    data: None,
                },
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![
                            descriptor_with_profile(
                                LOOPBACK_MODEL_PROVIDER,
                                Some("ready"),
                                Some("model-bridge-payload-v1"),
                            ),
                            descriptor_with_profile(
                                "openai-compatible",
                                Some("degraded"),
                                Some("openai-compatible-chat-completions-v1"),
                            ),
                        ],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    let model = snapshot
        .services
        .iter()
        .find(|service| service.server_kind == BridgeServerKind::Model)
        .expect("model cutover should exist");
    let openai = model
        .checks
        .iter()
        .find(|check| check.target == "openai-compatible")
        .expect("cataloged degraded openai-compatible should still be checked");
    let openai_issue = snapshot
        .blocking_issues
        .iter()
        .find(|issue| issue.target == "openai-compatible")
        .expect("degraded openai-compatible should surface as blocking");

    assert!(!snapshot.overall_ok);
    assert!(!model.service_ok);
    assert!(!openai.ok);
    assert_eq!(
        openai.blocking_reason.as_deref(),
        Some("bridge invocation failed")
    );
    assert_eq!(
        openai_issue.reason_code,
        BridgeCutoverBlockingReasonCode::ModelProviderUnavailable
    );
    assert_eq!(
        snapshot.blocking_issue_counts_by_server_kind.get("model"),
        Some(&2)
    );
    assert!(
        model
            .blocking_targets
            .contains(&"openai-compatible".to_string()),
        "degraded provider should remain in blocking targets: {model:?}"
    );
}

#[test]
fn cutover_smoke_snapshot_provider_blocks_unavailable_mcp_default_route() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response("unavailable", "<none>")),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32015,
                    message: "default server unavailable".to_string(),
                    data: Some(json!({
                        "default_route_status": "unavailable",
                        "default_route_target": "<none>",
                    })),
                },
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.checked_service_count, 1);
    assert_eq!(snapshot.blocking_check_count, 1);
    assert_eq!(snapshot.blocking_services, vec![BridgeServerKind::Mcp]);
    assert_eq!(snapshot.blocking_issues.len(), 1);
    let mcp = snapshot
        .services
        .iter()
        .find(|service| service.server_kind == BridgeServerKind::Mcp)
        .expect("mcp cutover should exist");
    assert!(!mcp.service_ok);
    assert_eq!(mcp.blocking_check_count, 1);
    assert_eq!(mcp.blocking_targets, vec!["<none>".to_string()]);
    let mcp_gate = mcp
        .mcp_default_route_gate
        .as_ref()
        .expect("mcp route gate should exist");
    assert_eq!(mcp_gate.route_status, "unavailable");
    assert_eq!(mcp_gate.route_target, "<none>");
    assert_eq!(mcp_gate.resolved_server, None);
    assert!(!mcp_gate.contract_ok);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(issue.server_kind, BridgeServerKind::Mcp);
    assert_eq!(issue.check_name, "default_route_contract");
    assert_eq!(issue.target, "<none>");
    assert_eq!(issue.facet, BridgeCutoverBlockingFacet::McpDefaultRoute);
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpDefaultRouteStatusUnavailable
    );
    let check = &mcp.checks[0];
    assert!(!check.ok);
    assert_eq!(
        check.blocking_reason.as_deref(),
        Some("default route is unavailable")
    );
    assert_eq!(
        check.error.as_ref().and_then(|error| error.layer),
        Some(BridgeErrorLayer::RemoteBusiness)
    );
    let contract = check
        .mcp_contract
        .as_ref()
        .expect("mcp contract should exist");
    assert_eq!(contract.route_status, "unavailable");
    assert_eq!(contract.route_target, "<none>");
    assert!(!contract.blank_selection_ok);
    assert!(!contract.contract_ok);
}

#[test]
fn cutover_smoke_snapshot_provider_reports_mcp_manager_list_failure_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([(
            "mcp.list_servers".to_string(),
            FakeTransportOutcome::RemoteBusiness {
                code: -32041,
                message: "manager unavailable".to_string(),
                data: None,
            },
        )]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.blocking_check_count, 1);
    assert_eq!(snapshot.blocking_services, vec![BridgeServerKind::Mcp]);
    assert_eq!(snapshot.blocking_issues.len(), 1);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(issue.server_kind, BridgeServerKind::Mcp);
    assert_eq!(issue.target, "loopback-mcp-manager");
    assert_eq!(issue.facet, BridgeCutoverBlockingFacet::McpDefaultRoute);
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpListServersFailed
    );
    assert_eq!(issue.blocking_reason, "mcp manager list_servers failed");
    assert_eq!(
        issue.error.as_ref().and_then(|error| error.layer),
        Some(BridgeErrorLayer::RemoteBusiness)
    );
}

#[test]
fn cutover_smoke_snapshot_provider_reports_empty_model_payload_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::Payload(bridge_response("")),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![descriptor_with_profile(
                            LOOPBACK_MODEL_PROVIDER,
                            Some("ready"),
                            Some("model-bridge-payload-v1"),
                        )],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.blocking_issues.len(), 1);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(issue.facet, BridgeCutoverBlockingFacet::ModelContract);
    assert_eq!(issue.target, LOOPBACK_MODEL_PROVIDER);
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::ModelPayloadEmpty
    );
    assert_eq!(issue.blocking_reason, "bridge payload was empty");
}

#[test]
fn cutover_smoke_snapshot_provider_reports_invalid_model_tool_calls_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Model,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "model.invoke".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "tool_calls": [{
                        "function": {
                            "name": "demo.lookup",
                            "arguments": { "city": "Paris" },
                        }
                    }],
                }))),
            ),
            (
                LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                FakeTransportOutcome::Payload(
                    serde_json::to_value(BridgeServerServiceCatalog {
                        protocol_version: "local-bridge-v1".to_string(),
                        server_kind: BridgeServerKind::Model,
                        services: vec![descriptor_with_profile(
                            LOOPBACK_MODEL_PROVIDER,
                            Some("ready"),
                            Some("model-bridge-payload-v1"),
                        )],
                    })
                    .expect("catalog should serialize"),
                ),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.blocking_issues.len(), 1);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::ModelStructuredPayloadInvalidToolCalls
    );
    assert_eq!(
        issue.blocking_reason,
        "structured payload contains invalid tool_calls"
    );
}

#[test]
fn cutover_smoke_snapshot_provider_reports_fallback_only_mcp_route_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "fallback-only",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::Payload(json!({
                    "manager": descriptor_with_route(
                        "loopback-mcp-manager",
                        "fallback-only",
                        "loopback-mcp-observability",
                    ),
                    "server": descriptor_with_profile(
                        "loopback-mcp-observability",
                        Some("degraded"),
                        Some("observability-v1"),
                    ),
                    "lifecycle_events": [],
                })),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "server_name": "loopback-mcp-observability",
                    "default_route_status": "fallback-only",
                    "default_route_target": "loopback-mcp-observability",
                    "tool_name": "echo.describe",
                }))),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.blocking_issues.len(), 1);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpDefaultRouteStatusFallbackOnly
    );
    assert_eq!(issue.blocking_reason, "default route is fallback-only");
}

#[test]
fn cutover_smoke_snapshot_provider_reports_unsupported_mcp_route_status_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "degraded",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32015,
                    message: "route unsupported".to_string(),
                    data: None,
                },
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(snapshot.blocking_issues.len(), 1);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpDefaultRouteStatusUnsupported
    );
    assert_eq!(
        issue.blocking_reason,
        "unsupported default route status degraded"
    );
}

#[test]
fn cutover_smoke_snapshot_provider_reports_mcp_describe_failure_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "ready",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32022,
                    message: "missing server".to_string(),
                    data: None,
                },
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "server_name": "loopback-mcp-observability",
                    "default_route_status": "ready",
                    "default_route_target": "loopback-mcp-observability",
                    "tool_name": "echo.describe",
                }))),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpDefaultRouteTargetDescribeFailed
    );
    assert_eq!(
        issue.blocking_reason,
        "default route target loopback-mcp-observability could not be described"
    );
    assert_eq!(
        issue.error.as_ref().and_then(|error| error.layer),
        Some(BridgeErrorLayer::RemoteBusiness)
    );
    assert_eq!(
        issue.error.as_ref().map(|error| error.message.as_str()),
        Some("桥接调用失败[RemoteBusiness]: remote business error [-32022]: missing server")
    );
}

#[test]
fn cutover_smoke_snapshot_provider_reports_mcp_blank_selection_invocation_failure_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "ready",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::Payload(json!({
                    "manager": descriptor_with_route(
                        "loopback-mcp-manager",
                        "ready",
                        "loopback-mcp-observability",
                    ),
                    "server": descriptor_with_profile(
                        "loopback-mcp-observability",
                        Some("ready"),
                        Some("observability-v1"),
                    ),
                    "lifecycle_events": [],
                })),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32015,
                    message: "default route unavailable".to_string(),
                    data: None,
                },
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpBlankSelectionInvocationFailed
    );
    assert_eq!(issue.blocking_reason, "blank selection invocation failed");
    assert_eq!(
        issue.error.as_ref().and_then(|error| error.layer),
        Some(BridgeErrorLayer::RemoteBusiness)
    );
}

#[test]
fn cutover_smoke_snapshot_provider_reports_mcp_blank_selection_response_not_ok_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "ready",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::Payload(json!({
                    "manager": descriptor_with_route(
                        "loopback-mcp-manager",
                        "ready",
                        "loopback-mcp-observability",
                    ),
                    "server": descriptor_with_profile(
                        "loopback-mcp-observability",
                        Some("ready"),
                        Some("observability-v1"),
                    ),
                    "lifecycle_events": [],
                })),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(bridge_response_with_status(false, "denied")),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpBlankSelectionResponseNotOk
    );
    assert_eq!(issue.blocking_reason, "blank selection response was not ok");
    assert_eq!(issue.response_excerpt.as_deref(), Some("denied"));
    assert_eq!(issue.error, None);
    let contract = issue
        .mcp_contract
        .as_ref()
        .expect("mcp contract should exist");
    assert!(!contract.blank_selection_ok);
    assert!(contract.describe_ok);
    assert!(!contract.contract_ok);
}

#[test]
fn cutover_smoke_snapshot_provider_reports_mcp_metadata_drift_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "ready",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::Payload(json!({
                    "manager": descriptor_with_route(
                        "loopback-mcp-manager",
                        "ready",
                        "loopback-mcp-observability",
                    ),
                    "server": descriptor_with_profile(
                        "loopback-mcp-observability",
                        Some("ready"),
                        Some("observability-v1"),
                    ),
                    "lifecycle_events": [],
                })),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "server_name": "loopback-mcp-observability",
                    "default_route_status": "ready",
                    "default_route_target": "loopback-mcp-inspection",
                    "tool_name": "echo.describe",
                }))),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpDefaultRouteMetadataDrift
    );
    assert_eq!(
        issue.blocking_reason,
        "blank selection payload drifted from manager metadata"
    );
}

#[test]
fn cutover_smoke_snapshot_provider_reports_mcp_resolved_server_mismatch_issue() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([
            (
                "mcp.list_servers".to_string(),
                FakeTransportOutcome::Payload(mcp_list_response(
                    "ready",
                    "loopback-mcp-observability",
                )),
            ),
            (
                "mcp.describe_server".to_string(),
                FakeTransportOutcome::Payload(json!({
                    "manager": descriptor_with_route(
                        "loopback-mcp-manager",
                        "ready",
                        "loopback-mcp-observability",
                    ),
                    "server": descriptor_with_profile(
                        "loopback-mcp-observability",
                        Some("ready"),
                        Some("observability-v1"),
                    ),
                    "lifecycle_events": [],
                })),
            ),
            (
                "mcp.call_tool".to_string(),
                FakeTransportOutcome::Payload(structured_bridge_response(json!({
                    "server_name": "loopback-mcp-inspection",
                    "default_route_status": "ready",
                    "default_route_target": "loopback-mcp-observability",
                    "tool_name": "echo.describe",
                }))),
            ),
        ]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    let issue = &snapshot.blocking_issues[0];
    assert_eq!(
        issue.reason_code,
        BridgeCutoverBlockingReasonCode::McpDefaultRouteResolvedServerMismatch
    );
    assert_eq!(
        issue.blocking_reason,
        "blank selection resolved to the wrong MCP server"
    );
}

#[test]
fn cutover_smoke_snapshot_provider_keeps_blocking_issue_counts_consistent() {
    let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
    provider.register_transport(
        BridgeServerKind::Mcp,
        Arc::new(FakeTransport::new(HashMap::from([(
            "mcp.list_servers".to_string(),
            FakeTransportOutcome::RemoteBusiness {
                code: -32041,
                message: "manager unavailable".to_string(),
                data: None,
            },
        )]))),
    );

    let snapshot = provider.cutover_smoke_snapshot();
    assert!(!snapshot.overall_ok);
    assert_eq!(
        snapshot.blocking_check_count,
        snapshot.blocking_issues.len()
    );
    assert_eq!(snapshot.blocking_services, vec![BridgeServerKind::Mcp]);
    assert_eq!(
        snapshot
            .blocking_issue_counts_by_reason_code
            .get("mcp_manager_list_servers_failed"),
        Some(&1)
    );
    assert_eq!(
        snapshot.blocking_issue_counts_by_server_kind.get("mcp"),
        Some(&1)
    );
}
