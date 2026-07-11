use magi_bridge_client::{
    BridgeClientError, BridgeResponse, BridgeServerKind, BridgeTransport,
    JsonRpcBridgeServerProbeClient, JsonRpcMcpBridgeClient, JsonRpcMcpManagerClient,
    JsonRpcModelBridgeClient, LOOPBACK_MCP_SERVER_NAME, LOOPBACK_MCP_TOOL_NAME,
    LOOPBACK_MODEL_PROVIDER, McpBridgeClient, McpToolCallRequest, ModelBridgeClient,
    ModelInvocationRequest,
};
use std::sync::Arc;

use super::super::bridges::{
    BridgePreflightCheckDto, BridgePreflightProvider, BridgePreflightServiceDto,
    BridgePreflightSnapshotDto, BridgeProbeErrorDto,
};
use super::common::{BridgeTransportBinding, excerpt};

#[derive(Clone, Default)]
pub struct BridgePreflightSnapshotProvider {
    bindings: Vec<BridgeTransportBinding>,
}

impl BridgePreflightSnapshotProvider {
    pub fn register_transport(
        &mut self,
        server_kind: BridgeServerKind,
        transport: Arc<dyn BridgeTransport>,
    ) {
        self.bindings
            .retain(|binding| binding.server_kind != server_kind);
        self.bindings.push(BridgeTransportBinding {
            server_kind,
            transport,
        });
    }

    fn capture_binding_snapshot(binding: &BridgeTransportBinding) -> BridgePreflightServiceDto {
        let checks = match binding.server_kind {
            BridgeServerKind::Model => capture_model_preflight_checks(binding.transport.clone()),
            BridgeServerKind::Mcp => capture_mcp_preflight_checks(binding.transport.clone()),
        };

        BridgePreflightServiceDto {
            server_kind: binding.server_kind,
            checks,
        }
    }
}

impl BridgePreflightProvider for BridgePreflightSnapshotProvider {
    fn preflight_snapshot(&self) -> BridgePreflightSnapshotDto {
        BridgePreflightSnapshotDto {
            services: self
                .bindings
                .iter()
                .map(Self::capture_binding_snapshot)
                .collect(),
        }
    }
}

fn capture_model_preflight_checks(
    transport: Arc<dyn BridgeTransport>,
) -> Vec<BridgePreflightCheckDto> {
    let probe = JsonRpcBridgeServerProbeClient::new(transport.clone());
    let client = JsonRpcModelBridgeClient::new(transport);
    let mut checks = vec![bridge_response_check(
        "invoke",
        LOOPBACK_MODEL_PROVIDER,
        client.invoke(ModelInvocationRequest {
            provider: LOOPBACK_MODEL_PROVIDER.to_string(),
            prompt: "bridge preflight ping".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        }),
    )];

    if let Ok(catalog) = probe.describe_services()
        && catalog.services.iter().any(|service| {
            service.service_name == "openai-compatible"
                && service.service_health.as_deref() == Some("ready")
        })
    {
        checks.push(bridge_response_check(
            "invoke",
            "openai-compatible",
            client.invoke(ModelInvocationRequest {
                provider: "openai-compatible".to_string(),
                prompt: "bridge preflight ping".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            }),
        ));
    }

    checks
}

fn capture_mcp_preflight_checks(
    transport: Arc<dyn BridgeTransport>,
) -> Vec<BridgePreflightCheckDto> {
    let manager = JsonRpcMcpManagerClient::new(transport.clone());
    let client = JsonRpcMcpBridgeClient::new(transport);
    vec![
        summary_check(
            "list_servers",
            "loopback-mcp-manager",
            manager.list_servers().map(|response| {
                format!(
                    "servers:{} default_route:{}->{}",
                    response.servers.len(),
                    response.default_route_status,
                    response.default_route_target
                )
            }),
        ),
        bridge_response_check(
            "call_tool",
            format!("{LOOPBACK_MCP_SERVER_NAME}.{LOOPBACK_MCP_TOOL_NAME}"),
            client.call_tool(McpToolCallRequest {
                server_name: LOOPBACK_MCP_SERVER_NAME.to_string(),
                tool_name: LOOPBACK_MCP_TOOL_NAME.to_string(),
                input: String::new(),
            }),
        ),
    ]
}

fn bridge_response_check(
    check_name: impl Into<String>,
    target: impl Into<String>,
    result: Result<BridgeResponse, BridgeClientError>,
) -> BridgePreflightCheckDto {
    match result {
        Ok(response) => BridgePreflightCheckDto {
            check_name: check_name.into(),
            target: target.into(),
            ok: response.ok,
            response_excerpt: Some(excerpt(&response.payload)),
            error: None,
        },
        Err(error) => BridgePreflightCheckDto {
            check_name: check_name.into(),
            target: target.into(),
            ok: false,
            response_excerpt: None,
            error: Some(BridgeProbeErrorDto::from_client_error(error)),
        },
    }
}

fn summary_check(
    check_name: impl Into<String>,
    target: impl Into<String>,
    result: Result<String, BridgeClientError>,
) -> BridgePreflightCheckDto {
    match result {
        Ok(summary) => BridgePreflightCheckDto {
            check_name: check_name.into(),
            target: target.into(),
            ok: true,
            response_excerpt: Some(excerpt(&summary)),
            error: None,
        },
        Err(error) => BridgePreflightCheckDto {
            check_name: check_name.into(),
            target: target.into(),
            ok: false,
            response_excerpt: None,
            error: Some(BridgeProbeErrorDto::from_client_error(error)),
        },
    }
}
