use magi_bridge_client::{
    BridgeClientError, BridgeResponse, BridgeServerKind, BridgeServerServiceCatalog,
    BridgeTransport, HttpModelBridgeClient, JsonRpcBridgeServerProbeClient, JsonRpcMcpBridgeClient,
    JsonRpcMcpManagerClient, JsonRpcModelBridgeClient, McpBridgeClient,
    McpManagerServerSelectionRequest, McpToolCallRequest, ModelBridgeClient,
    ModelInvocationRequest, LOOPBACK_MODEL_PROVIDER,
};
use serde_json::Value;
use std::sync::Arc;

use super::super::bridge_contracts::{BridgeMcpDefaultRouteContractDto, evaluate_model_contract};
use super::super::bridge_reason_codes::{
    MCP_BLANK_SELECTION_INVOCATION_FAILED_REASON, MCP_BLANK_SELECTION_RESPONSE_NOT_OK_REASON,
    MCP_DEFAULT_ROUTE_FALLBACK_ONLY_REASON, MCP_DEFAULT_ROUTE_METADATA_DRIFT_REASON,
    MCP_DEFAULT_ROUTE_RESOLVED_SERVER_MISMATCH_REASON, MCP_DEFAULT_ROUTE_UNAVAILABLE_REASON,
    MCP_MANAGER_LIST_SERVERS_FAILED_REASON,
};
use super::super::bridges::{
    BridgeCutoverCheckDto, BridgeCutoverServiceDto, BridgeCutoverSmokeProvider,
    BridgeCutoverSmokeSnapshotDto, BridgeProbeErrorDto,
};
use super::common::{BridgeTransportBinding, excerpt};

#[derive(Clone, Default)]
pub struct BridgeCutoverSmokeSnapshotProvider {
    bindings: Vec<BridgeTransportBinding>,
    direct_http_probe: Option<DirectHttpModelProbeConfig>,
}

/// direct HTTP 模型 provider 探测配置。
/// 该链路绕过 loopback bridge，因此与 JSON-RPC transport 绑定分开保存。
#[derive(Clone, Debug)]
pub struct DirectHttpModelProbeConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

impl DirectHttpModelProbeConfig {
    /// 返回移除内嵌凭据后的 base URL。
    pub fn sanitized_base_url(&self) -> String {
        // 如果 URL 含 user-info，避免在诊断输出中泄露凭据。
        if let Some(at_pos) = self.base_url.find('@') {
            if let Some(scheme_end) = self.base_url.find("://") {
                let after_scheme = scheme_end + 3;
                if at_pos > after_scheme {
                    return format!(
                        "{}{}",
                        &self.base_url[..after_scheme],
                        &self.base_url[at_pos + 1..]
                    );
                }
            }
        }
        self.base_url.clone()
    }
}

impl BridgeCutoverSmokeSnapshotProvider {
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

    pub fn register_direct_http_probe(&mut self, config: DirectHttpModelProbeConfig) {
        self.direct_http_probe = Some(config);
    }

    fn capture_binding_snapshot(binding: &BridgeTransportBinding) -> BridgeCutoverServiceDto {
        let checks = match binding.server_kind {
            BridgeServerKind::Model => capture_model_cutover_checks(binding.transport.clone()),
            BridgeServerKind::Mcp => capture_mcp_cutover_checks(binding.transport.clone()),
        };

        BridgeCutoverServiceDto::from_checks(binding.server_kind, checks)
    }
}

impl BridgeCutoverSmokeProvider for BridgeCutoverSmokeSnapshotProvider {
    fn cutover_smoke_snapshot(&self) -> BridgeCutoverSmokeSnapshotDto {
        let mut services: Vec<BridgeCutoverServiceDto> = self
            .bindings
            .iter()
            .map(Self::capture_binding_snapshot)
            .collect();

        // 如果配置了 direct HTTP 模型 provider，将探测结果合并到 model service。
        if let Some(ref probe_config) = self.direct_http_probe {
            let check = capture_direct_http_model_cutover_check(probe_config);
            if let Some(model_service) = services
                .iter_mut()
                .find(|service| service.server_kind == BridgeServerKind::Model)
            {
                model_service.checks.push(check);
                // 追加 direct HTTP 检查后重新计算 service 汇总。
                *model_service = BridgeCutoverServiceDto::from_checks(
                    BridgeServerKind::Model,
                    std::mem::take(&mut model_service.checks),
                );
            } else {
                services.push(BridgeCutoverServiceDto::from_checks(
                    BridgeServerKind::Model,
                    vec![check],
                ));
            }
        }

        BridgeCutoverSmokeSnapshotDto::from_services(services)
    }
}

fn capture_model_cutover_checks(transport: Arc<dyn BridgeTransport>) -> Vec<BridgeCutoverCheckDto> {
    let probe = JsonRpcBridgeServerProbeClient::new(transport.clone());
    let client = JsonRpcModelBridgeClient::new(transport);
    let catalog = probe.describe_services().ok();
    let mut checks = Vec::new();

    let loopback_profile = model_capability_profile(catalog.as_ref(), LOOPBACK_MODEL_PROVIDER)
        .unwrap_or_else(|| "model-bridge-payload-v1".to_string());
    checks.push(bridge_cutover_model_check(
        "invoke_contract",
        LOOPBACK_MODEL_PROVIDER,
        loopback_profile,
        client.invoke(ModelInvocationRequest {
            provider: LOOPBACK_MODEL_PROVIDER.to_string(),
            prompt: "bridge cutover smoke".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        }),
    ));

    if let Some(openai_descriptor) = catalog
        .as_ref()
        .and_then(|catalog| model_service_descriptor(catalog, "openai-compatible"))
    {
        let profile = openai_descriptor
            .capability_profile
            .clone()
            .unwrap_or_else(|| "openai-compatible-chat-completions-v1".to_string());
        checks.push(bridge_cutover_model_check(
            "invoke_contract",
            "openai-compatible",
            profile,
            client.invoke(ModelInvocationRequest {
                provider: "openai-compatible".to_string(),
                prompt: "bridge cutover smoke".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            }),
        ));
    }

    checks
}

fn capture_direct_http_model_cutover_check(
    config: &DirectHttpModelProbeConfig,
) -> BridgeCutoverCheckDto {
    let target = format!(
        "direct-http://{}  model={}",
        config.sanitized_base_url(),
        config.model,
    );
    let client = HttpModelBridgeClient::new(
        config.base_url.clone(),
        config.api_key.clone(),
        config.model.clone(),
    );
    bridge_cutover_model_check(
        "direct_http_provider_probe",
        target,
        "openai-compatible-direct-http-v1".to_string(),
        client.invoke(ModelInvocationRequest {
            provider: "direct-http".to_string(),
            prompt: "bridge cutover smoke".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        }),
    )
}

fn capture_mcp_cutover_checks(transport: Arc<dyn BridgeTransport>) -> Vec<BridgeCutoverCheckDto> {
    let manager = JsonRpcMcpManagerClient::new(transport.clone());
    let client = JsonRpcMcpBridgeClient::new(transport);
    let list = match manager.list_servers() {
        Ok(list) => list,
        Err(error) => {
            return vec![BridgeCutoverCheckDto {
                check_name: "default_route_contract".to_string(),
                target: "loopback-mcp-manager".to_string(),
                ok: false,
                blocking_reason: Some(MCP_MANAGER_LIST_SERVERS_FAILED_REASON.to_string()),
                response_excerpt: None,
                error: Some(BridgeProbeErrorDto::from_client_error(error)),
                model_contract: None,
                mcp_contract: None,
            }];
        }
    };

    let route_status = list.default_route_status.clone();
    let route_target = list.default_route_target.clone();
    let mut describe_ok = false;
    let mut blank_selection_ok = false;
    let mut resolved_server = None;
    let mut response_excerpt = None;
    let mut describe_error = None;
    let mut blank_selection_error = None;

    let base_blocking_reason = match route_status.as_str() {
        "fallback-only" => Some(MCP_DEFAULT_ROUTE_FALLBACK_ONLY_REASON.to_string()),
        "unavailable" => Some(MCP_DEFAULT_ROUTE_UNAVAILABLE_REASON.to_string()),
        "ready" => None,
        other => Some(format!("unsupported default route status {other}")),
    };

    if route_status == "ready" || route_status == "fallback-only" {
        match manager.describe_server(McpManagerServerSelectionRequest {
            server_name: route_target.clone(),
        }) {
            Ok(_) => describe_ok = true,
            Err(client_error) => {
                describe_error = Some(BridgeProbeErrorDto::from_client_error(client_error));
            }
        }
    }

    let blank_selection_result = client.call_tool(McpToolCallRequest {
        server_name: String::new(),
        tool_name: "echo.describe".to_string(),
        input: String::new(),
    });

    let mut metadata_matches = false;
    let mut route_matches = false;
    match blank_selection_result {
        Ok(response) => {
            blank_selection_ok = response.ok;
            response_excerpt = Some(excerpt(&response.payload));
            if let Ok(payload) = serde_json::from_str::<Value>(&response.payload) {
                resolved_server = payload
                    .get("server_name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                metadata_matches = payload.get("default_route_status").and_then(Value::as_str)
                    == Some(route_status.as_str())
                    && payload.get("default_route_target").and_then(Value::as_str)
                        == Some(route_target.as_str());
            }
            route_matches = resolved_server.as_deref() == Some(route_target.as_str());
        }
        Err(client_error) => {
            blank_selection_error = Some(BridgeProbeErrorDto::from_client_error(client_error));
        }
    }

    let mut blocking_reason = base_blocking_reason;
    if blocking_reason.is_none() && !describe_ok {
        blocking_reason = Some(format!(
            "default route target {route_target} could not be described"
        ));
    }
    if blocking_reason.is_none() && !blank_selection_ok {
        blocking_reason = Some(if blank_selection_error.is_some() {
            MCP_BLANK_SELECTION_INVOCATION_FAILED_REASON.to_string()
        } else {
            MCP_BLANK_SELECTION_RESPONSE_NOT_OK_REASON.to_string()
        });
    }
    if blocking_reason.is_none() && !metadata_matches {
        blocking_reason = Some(MCP_DEFAULT_ROUTE_METADATA_DRIFT_REASON.to_string());
    }
    if blocking_reason.is_none() && !route_matches {
        blocking_reason = Some(MCP_DEFAULT_ROUTE_RESOLVED_SERVER_MISMATCH_REASON.to_string());
    }

    let error = if !describe_ok {
        describe_error.or(blank_selection_error)
    } else if !blank_selection_ok {
        blank_selection_error
    } else {
        None
    };

    let contract_ok = route_status == "ready"
        && describe_ok
        && blank_selection_ok
        && metadata_matches
        && route_matches;
    let mcp_contract = BridgeMcpDefaultRouteContractDto {
        route_status,
        route_target: route_target.clone(),
        resolved_server,
        describe_ok,
        blank_selection_ok,
        contract_ok,
        blocking_reason: blocking_reason.clone(),
    };

    vec![BridgeCutoverCheckDto {
        check_name: "default_route_contract".to_string(),
        target: route_target,
        ok: contract_ok,
        blocking_reason,
        response_excerpt,
        error,
        model_contract: None,
        mcp_contract: Some(mcp_contract),
    }]
}

fn bridge_cutover_model_check(
    check_name: impl Into<String>,
    target: impl Into<String>,
    contract_profile: String,
    result: Result<BridgeResponse, BridgeClientError>,
) -> BridgeCutoverCheckDto {
    let target = target.into();
    match result {
        Ok(response) => {
            let contract = evaluate_model_contract(&response.payload, contract_profile);
            let mut blocking_reason = contract.blocking_reason.clone();
            if blocking_reason.is_none() && !response.ok {
                blocking_reason = Some("bridge response was not ok".to_string());
            }
            BridgeCutoverCheckDto {
                check_name: check_name.into(),
                target,
                ok: response.ok && contract.contract_ok,
                blocking_reason,
                response_excerpt: Some(excerpt(&response.payload)),
                error: None,
                model_contract: Some(contract),
                mcp_contract: None,
            }
        }
        Err(error) => BridgeCutoverCheckDto {
            check_name: check_name.into(),
            target,
            ok: false,
            blocking_reason: Some("bridge invocation failed".to_string()),
            response_excerpt: None,
            error: Some(BridgeProbeErrorDto::from_client_error(error)),
            model_contract: None,
            mcp_contract: None,
        },
    }
}

fn model_service_descriptor<'a>(
    catalog: &'a BridgeServerServiceCatalog,
    service_name: &str,
) -> Option<&'a magi_bridge_client::BridgeServerServiceDescriptor> {
    catalog
        .services
        .iter()
        .find(|descriptor| descriptor.service_name == service_name)
}

fn model_capability_profile(
    catalog: Option<&BridgeServerServiceCatalog>,
    service_name: &str,
) -> Option<String> {
    catalog
        .and_then(|catalog| model_service_descriptor(catalog, service_name))
        .and_then(|descriptor| descriptor.capability_profile.clone())
}
