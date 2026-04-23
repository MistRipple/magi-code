use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, BridgeResponse, BridgeServerHandshake, BridgeServerHealth,
    BridgeServerKind, BridgeServerServiceCatalog, BridgeTransport, HostBridgeClient,
    HostBridgeCommand, HostBridgeRequest, HostKind, HttpModelBridgeClient,
    JsonRpcBridgeServerProbeClient, JsonRpcHostBridgeClient, JsonRpcMcpBridgeClient,
    JsonRpcMcpManagerClient, JsonRpcModelBridgeClient, McpBridgeClient,
    McpManagerServerSelectionRequest, McpToolCallRequest, ModelBridgeClient,
    ModelInvocationRequest, SHADOW_MCP_SERVER_NAME, SHADOW_MCP_TOOL_NAME, SHADOW_MODEL_PROVIDER,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub type BridgeHandshakeDto = BridgeServerHandshake;
pub type BridgeHealthDto = BridgeServerHealth;
pub type BridgeServiceCatalogDto = BridgeServerServiceCatalog;

pub trait BridgeSnapshotProvider: Send + Sync {
    fn services_snapshot(&self) -> BridgeServicesSnapshotDto;
}

pub trait BridgePreflightProvider: Send + Sync {
    fn preflight_snapshot(&self) -> BridgePreflightSnapshotDto;
}

pub trait BridgeCutoverSmokeProvider: Send + Sync {
    fn cutover_smoke_snapshot(&self) -> BridgeCutoverSmokeSnapshotDto;
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BridgeServicesSnapshotDto {
    pub services: Vec<BridgeServiceSnapshotDto>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BridgePreflightSnapshotDto {
    pub services: Vec<BridgePreflightServiceDto>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeCutoverSmokeSnapshotDto {
    pub overall_ok: bool,
    pub checked_service_count: usize,
    pub blocking_check_count: usize,
    pub blocking_services: Vec<BridgeServerKind>,
    pub blocking_issue_counts_by_reason_code: BTreeMap<String, usize>,
    pub blocking_issue_counts_by_server_kind: BTreeMap<String, usize>,
    pub blocking_issues: Vec<BridgeCutoverBlockingIssueDto>,
    pub services: Vec<BridgeCutoverServiceDto>,
}

impl BridgeCutoverSmokeSnapshotDto {
    fn from_services(services: Vec<BridgeCutoverServiceDto>) -> Self {
        let blocking_services = services
            .iter()
            .filter(|service| service.checks.iter().any(|check| !check.ok))
            .map(|service| service.server_kind)
            .collect::<Vec<_>>();
        let blocking_check_count = services
            .iter()
            .flat_map(|service| service.checks.iter())
            .filter(|check| !check.ok)
            .count();
        let blocking_issues = services
            .iter()
            .flat_map(BridgeCutoverServiceDto::blocking_issues)
            .collect::<Vec<_>>();
        let blocking_issue_counts_by_reason_code =
            blocking_issue_counts_by_reason_code(&blocking_issues);
        let blocking_issue_counts_by_server_kind =
            blocking_issue_counts_by_server_kind(&blocking_issues);
        let checked_service_count = services.len();
        let overall_ok = blocking_check_count == 0;

        Self {
            overall_ok,
            checked_service_count,
            blocking_check_count,
            blocking_services,
            blocking_issue_counts_by_reason_code,
            blocking_issue_counts_by_server_kind,
            blocking_issues,
            services,
        }
    }
}

impl Default for BridgeCutoverSmokeSnapshotDto {
    fn default() -> Self {
        Self::from_services(Vec::new())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BridgeServiceSnapshotDto {
    pub server_kind: BridgeServerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake: Option<BridgeHandshakeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake_error: Option<BridgeProbeErrorDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<BridgeHealthDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_error: Option<BridgeProbeErrorDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_catalog: Option<BridgeServiceCatalogDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_catalog_error: Option<BridgeProbeErrorDto>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BridgePreflightServiceDto {
    pub server_kind: BridgeServerKind,
    pub checks: Vec<BridgePreflightCheckDto>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeCutoverServiceDto {
    pub service_ok: bool,
    pub blocking_check_count: usize,
    pub blocking_targets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_default_route_gate: Option<BridgeMcpDefaultRouteGateDto>,
    pub server_kind: BridgeServerKind,
    pub checks: Vec<BridgeCutoverCheckDto>,
}

impl BridgeCutoverServiceDto {
    fn from_checks(server_kind: BridgeServerKind, checks: Vec<BridgeCutoverCheckDto>) -> Self {
        let blocking_targets = checks
            .iter()
            .filter(|check| !check.ok)
            .map(|check| check.target.clone())
            .collect::<Vec<_>>();
        let blocking_check_count = blocking_targets.len();
        let service_ok = blocking_check_count == 0;
        let mcp_default_route_gate = checks.iter().find_map(|check| {
            check
                .mcp_contract
                .as_ref()
                .map(BridgeMcpDefaultRouteGateDto::from_contract)
        });

        Self {
            service_ok,
            blocking_check_count,
            blocking_targets,
            mcp_default_route_gate,
            server_kind,
            checks,
        }
    }

    fn blocking_issues(&self) -> Vec<BridgeCutoverBlockingIssueDto> {
        self.checks
            .iter()
            .filter_map(|check| check.blocking_issue(self.server_kind))
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeMcpDefaultRouteGateDto {
    pub route_status: String,
    pub route_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_server: Option<String>,
    pub contract_ok: bool,
}

impl BridgeMcpDefaultRouteGateDto {
    fn from_contract(contract: &BridgeMcpDefaultRouteContractDto) -> Self {
        Self {
            route_status: contract.route_status.clone(),
            route_target: contract.route_target.clone(),
            resolved_server: contract.resolved_server.clone(),
            contract_ok: contract.contract_ok,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeCutoverBlockingIssueDto {
    pub server_kind: BridgeServerKind,
    pub check_name: String,
    pub target: String,
    pub facet: BridgeCutoverBlockingFacet,
    pub reason_code: BridgeCutoverBlockingReasonCode,
    pub blocking_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeProbeErrorDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_contract: Option<BridgeModelContractDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_contract: Option<BridgeMcpDefaultRouteContractDto>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BridgeCutoverBlockingFacet {
    BridgeResponse,
    ModelContract,
    McpDefaultRoute,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BridgeCutoverBlockingReasonCode {
    BridgeInvocationFailed,
    BridgeResponseNotOk,
    BridgeResponseEmptyPayload,
    ModelProviderUnavailable,
    ModelProviderMisconfigured,
    ModelProviderTransportFailed,
    ModelProviderRejected,
    ModelProviderInvalidResponse,
    ModelPayloadEmpty,
    ModelStructuredPayloadInvalidToolCalls,
    ModelStructuredPayloadMissingContentOrToolCalls,
    McpListServersFailed,
    McpDefaultRouteStatusFallbackOnly,
    McpDefaultRouteStatusUnavailable,
    McpDefaultRouteStatusUnsupported,
    McpDefaultRouteTargetDescribeFailed,
    McpBlankSelectionInvocationFailed,
    McpBlankSelectionResponseNotOk,
    McpDefaultRouteMetadataDrift,
    McpDefaultRouteResolvedServerMismatch,
    ModelProviderDirectHttpOk,
}

const MCP_MANAGER_LIST_SERVERS_FAILED_REASON: &str = "mcp manager list_servers failed";
const MCP_DEFAULT_ROUTE_FALLBACK_ONLY_REASON: &str = "default route is fallback-only";
const MCP_DEFAULT_ROUTE_UNAVAILABLE_REASON: &str = "default route is unavailable";
const MCP_BLANK_SELECTION_INVOCATION_FAILED_REASON: &str = "blank selection invocation failed";
const MCP_BLANK_SELECTION_RESPONSE_NOT_OK_REASON: &str = "blank selection response was not ok";
const MCP_DEFAULT_ROUTE_METADATA_DRIFT_REASON: &str =
    "blank selection payload drifted from manager metadata";
const MCP_DEFAULT_ROUTE_RESOLVED_SERVER_MISMATCH_REASON: &str =
    "blank selection resolved to the wrong MCP server";

impl BridgeCutoverBlockingReasonCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::BridgeInvocationFailed => "bridge_invocation_failed",
            Self::BridgeResponseNotOk => "bridge_response_not_ok",
            Self::BridgeResponseEmptyPayload => "bridge_response_empty_payload",
            Self::ModelProviderUnavailable => "model_provider_unavailable",
            Self::ModelProviderMisconfigured => "model_provider_misconfigured",
            Self::ModelProviderTransportFailed => "model_provider_transport_failed",
            Self::ModelProviderRejected => "model_provider_rejected",
            Self::ModelProviderInvalidResponse => "model_provider_invalid_response",
            Self::ModelPayloadEmpty => "model_payload_empty",
            Self::ModelStructuredPayloadInvalidToolCalls => {
                "model_structured_payload_invalid_tool_calls"
            }
            Self::ModelStructuredPayloadMissingContentOrToolCalls => {
                "model_structured_payload_missing_content_or_tool_calls"
            }
            Self::McpListServersFailed => "mcp_manager_list_servers_failed",
            Self::McpDefaultRouteStatusFallbackOnly => "mcp_default_route_status_fallback_only",
            Self::McpDefaultRouteStatusUnavailable => "mcp_default_route_status_unavailable",
            Self::McpDefaultRouteStatusUnsupported => "mcp_default_route_status_unsupported",
            Self::McpDefaultRouteTargetDescribeFailed => "mcp_default_route_target_describe_failed",
            Self::McpBlankSelectionInvocationFailed => "mcp_blank_selection_invocation_failed",
            Self::McpBlankSelectionResponseNotOk => "mcp_blank_selection_response_not_ok",
            Self::McpDefaultRouteMetadataDrift => "mcp_default_route_metadata_drift",
            Self::McpDefaultRouteResolvedServerMismatch => {
                "mcp_default_route_resolved_server_mismatch"
            }
            Self::ModelProviderDirectHttpOk => "model_provider_direct_http_ok",
        }
    }

    fn default_blocking_reason(self) -> &'static str {
        match self {
            Self::BridgeInvocationFailed => "bridge invocation failed",
            Self::BridgeResponseNotOk => "bridge response was not ok",
            Self::BridgeResponseEmptyPayload => "bridge response payload was empty",
            Self::ModelProviderUnavailable => "provider unavailable",
            Self::ModelProviderMisconfigured => "provider misconfigured",
            Self::ModelProviderTransportFailed => "provider transport failed",
            Self::ModelProviderRejected => "provider rejected request",
            Self::ModelProviderInvalidResponse => "provider response invalid",
            Self::ModelPayloadEmpty => "bridge payload was empty",
            Self::ModelStructuredPayloadInvalidToolCalls => {
                "structured payload contains invalid tool_calls"
            }
            Self::ModelStructuredPayloadMissingContentOrToolCalls => {
                "structured payload missing content or tool_calls"
            }
            Self::McpListServersFailed => MCP_MANAGER_LIST_SERVERS_FAILED_REASON,
            Self::McpDefaultRouteStatusFallbackOnly => MCP_DEFAULT_ROUTE_FALLBACK_ONLY_REASON,
            Self::McpDefaultRouteStatusUnavailable => MCP_DEFAULT_ROUTE_UNAVAILABLE_REASON,
            Self::McpDefaultRouteStatusUnsupported => "default route status is unsupported",
            Self::McpDefaultRouteTargetDescribeFailed => {
                "default route target could not be described"
            }
            Self::McpBlankSelectionInvocationFailed => MCP_BLANK_SELECTION_INVOCATION_FAILED_REASON,
            Self::McpBlankSelectionResponseNotOk => MCP_BLANK_SELECTION_RESPONSE_NOT_OK_REASON,
            Self::McpDefaultRouteMetadataDrift => MCP_DEFAULT_ROUTE_METADATA_DRIFT_REASON,
            Self::McpDefaultRouteResolvedServerMismatch => {
                MCP_DEFAULT_ROUTE_RESOLVED_SERVER_MISMATCH_REASON
            }
            Self::ModelProviderDirectHttpOk => "direct HTTP provider probe succeeded",
        }
    }
}

fn bridge_server_kind_key(kind: BridgeServerKind) -> &'static str {
    match kind {
        BridgeServerKind::Model => "model",
        BridgeServerKind::Host => "host",
        BridgeServerKind::Mcp => "mcp",
    }
}

fn blocking_issue_counts_by_reason_code(
    issues: &[BridgeCutoverBlockingIssueDto],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for issue in issues {
        *counts
            .entry(issue.reason_code.as_str().to_string())
            .or_insert(0) += 1;
    }
    counts
}

fn blocking_issue_counts_by_server_kind(
    issues: &[BridgeCutoverBlockingIssueDto],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for issue in issues {
        *counts
            .entry(bridge_server_kind_key(issue.server_kind).to_string())
            .or_insert(0) += 1;
    }
    counts
}

#[derive(Clone, Debug, Serialize)]
pub struct BridgePreflightCheckDto {
    pub check_name: String,
    pub target: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeProbeErrorDto>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeCutoverCheckDto {
    pub check_name: String,
    pub target: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeProbeErrorDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_contract: Option<BridgeModelContractDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_contract: Option<BridgeMcpDefaultRouteContractDto>,
}

impl BridgeCutoverCheckDto {
    fn blocking_issue(
        &self,
        server_kind: BridgeServerKind,
    ) -> Option<BridgeCutoverBlockingIssueDto> {
        if self.ok {
            return None;
        }

        let (facet, reason_code) = if matches!(
            self.blocking_reason.as_deref(),
            Some(MCP_MANAGER_LIST_SERVERS_FAILED_REASON)
        ) {
            (
                BridgeCutoverBlockingFacet::McpDefaultRoute,
                BridgeCutoverBlockingReasonCode::McpListServersFailed,
            )
        } else if let Some(contract) = self.mcp_contract.as_ref() {
            (
                BridgeCutoverBlockingFacet::McpDefaultRoute,
                infer_mcp_reason_code(self, contract),
            )
        } else if let Some(contract) = self.model_contract.as_ref() {
            (
                BridgeCutoverBlockingFacet::ModelContract,
                infer_model_reason_code(self, Some(contract)),
            )
        } else if server_kind == BridgeServerKind::Model {
            (
                BridgeCutoverBlockingFacet::BridgeResponse,
                infer_model_reason_code(self, None),
            )
        } else {
            (
                BridgeCutoverBlockingFacet::BridgeResponse,
                infer_bridge_response_reason_code(self),
            )
        };

        Some(BridgeCutoverBlockingIssueDto {
            server_kind,
            check_name: self.check_name.clone(),
            target: self.target.clone(),
            facet,
            reason_code,
            blocking_reason: self
                .blocking_reason
                .clone()
                .unwrap_or_else(|| reason_code.default_blocking_reason().to_string()),
            response_excerpt: self.response_excerpt.clone(),
            error: self.error.clone(),
            model_contract: self.model_contract.clone(),
            mcp_contract: self.mcp_contract.clone(),
        })
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeModelContractDto {
    pub contract_profile: String,
    pub payload_kind: String,
    pub contract_ok: bool,
    pub has_content: bool,
    pub has_finish_reason: bool,
    pub has_usage: bool,
    pub tool_call_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BridgeMcpDefaultRouteContractDto {
    pub route_status: String,
    pub route_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_server: Option<String>,
    pub describe_ok: bool,
    pub blank_selection_ok: bool,
    pub contract_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
}

fn infer_bridge_response_reason_code(
    check: &BridgeCutoverCheckDto,
) -> BridgeCutoverBlockingReasonCode {
    if check.error.is_some() {
        return BridgeCutoverBlockingReasonCode::BridgeInvocationFailed;
    }

    match check.blocking_reason.as_deref() {
        Some("bridge response payload was empty") | Some("bridge payload was empty") => {
            BridgeCutoverBlockingReasonCode::BridgeResponseEmptyPayload
        }
        Some("bridge response was not ok") => BridgeCutoverBlockingReasonCode::BridgeResponseNotOk,
        _ => BridgeCutoverBlockingReasonCode::BridgeResponseNotOk,
    }
}

fn infer_model_reason_code(
    check: &BridgeCutoverCheckDto,
    contract: Option<&BridgeModelContractDto>,
) -> BridgeCutoverBlockingReasonCode {
    if let Some(reason_code) =
        infer_model_provider_reason_code(check.error.as_ref().and_then(|error| error.code))
    {
        return reason_code;
    }

    if let Some(contract) = contract.filter(|contract| !contract.contract_ok) {
        return match contract.blocking_reason.as_deref() {
            Some("bridge payload was empty") => BridgeCutoverBlockingReasonCode::ModelPayloadEmpty,
            Some("structured payload contains invalid tool_calls") => {
                BridgeCutoverBlockingReasonCode::ModelStructuredPayloadInvalidToolCalls
            }
            Some("structured payload missing content or tool_calls") => {
                BridgeCutoverBlockingReasonCode::ModelStructuredPayloadMissingContentOrToolCalls
            }
            _ => BridgeCutoverBlockingReasonCode::BridgeResponseNotOk,
        };
    }

    infer_bridge_response_reason_code(check)
}

fn infer_model_provider_reason_code(
    error_code: Option<i64>,
) -> Option<BridgeCutoverBlockingReasonCode> {
    match error_code {
        Some(-32003) => Some(BridgeCutoverBlockingReasonCode::ModelProviderUnavailable),
        Some(-32004) => Some(BridgeCutoverBlockingReasonCode::ModelProviderMisconfigured),
        Some(-32005) => Some(BridgeCutoverBlockingReasonCode::ModelProviderTransportFailed),
        Some(-32006) => Some(BridgeCutoverBlockingReasonCode::ModelProviderRejected),
        Some(-32007) => Some(BridgeCutoverBlockingReasonCode::ModelProviderInvalidResponse),
        _ => None,
    }
}

fn infer_mcp_reason_code(
    check: &BridgeCutoverCheckDto,
    contract: &BridgeMcpDefaultRouteContractDto,
) -> BridgeCutoverBlockingReasonCode {
    if matches!(
        check.blocking_reason.as_deref(),
        Some(MCP_MANAGER_LIST_SERVERS_FAILED_REASON)
    ) {
        return BridgeCutoverBlockingReasonCode::McpListServersFailed;
    }

    match contract.route_status.as_str() {
        "fallback-only" => BridgeCutoverBlockingReasonCode::McpDefaultRouteStatusFallbackOnly,
        "unavailable" => BridgeCutoverBlockingReasonCode::McpDefaultRouteStatusUnavailable,
        "ready" => {
            if !contract.describe_ok {
                BridgeCutoverBlockingReasonCode::McpDefaultRouteTargetDescribeFailed
            } else if let Some(reason_code) = infer_ready_mcp_blank_selection_reason_code(
                contract.blocking_reason.as_deref(),
                check.error.is_some(),
                contract.blank_selection_ok,
            ) {
                reason_code
            } else if !contract.contract_ok
                && contract.resolved_server.as_deref() != Some(contract.route_target.as_str())
            {
                BridgeCutoverBlockingReasonCode::McpDefaultRouteResolvedServerMismatch
            } else if !contract.contract_ok {
                BridgeCutoverBlockingReasonCode::McpDefaultRouteMetadataDrift
            } else {
                BridgeCutoverBlockingReasonCode::McpBlankSelectionResponseNotOk
            }
        }
        _ => BridgeCutoverBlockingReasonCode::McpDefaultRouteStatusUnsupported,
    }
}

fn infer_ready_mcp_blank_selection_reason_code(
    blocking_reason: Option<&str>,
    has_error: bool,
    blank_selection_ok: bool,
) -> Option<BridgeCutoverBlockingReasonCode> {
    match blocking_reason {
        Some(MCP_BLANK_SELECTION_INVOCATION_FAILED_REASON) => {
            Some(BridgeCutoverBlockingReasonCode::McpBlankSelectionInvocationFailed)
        }
        Some(MCP_BLANK_SELECTION_RESPONSE_NOT_OK_REASON) => {
            Some(BridgeCutoverBlockingReasonCode::McpBlankSelectionResponseNotOk)
        }
        Some(MCP_DEFAULT_ROUTE_METADATA_DRIFT_REASON) => {
            Some(BridgeCutoverBlockingReasonCode::McpDefaultRouteMetadataDrift)
        }
        Some(MCP_DEFAULT_ROUTE_RESOLVED_SERVER_MISMATCH_REASON) => {
            Some(BridgeCutoverBlockingReasonCode::McpDefaultRouteResolvedServerMismatch)
        }
        _ if !blank_selection_ok && has_error => {
            Some(BridgeCutoverBlockingReasonCode::McpBlankSelectionInvocationFailed)
        }
        _ if !blank_selection_ok => {
            Some(BridgeCutoverBlockingReasonCode::McpBlankSelectionResponseNotOk)
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BridgeProbeErrorDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<BridgeErrorLayer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
    pub message: String,
}

impl BridgeProbeErrorDto {
    fn from_client_error(error: BridgeClientError) -> Self {
        Self {
            layer: error.layer(),
            code: error.code(),
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Default)]
pub struct BridgeProbeSnapshotProvider {
    probes: Vec<BridgeProbeBinding>,
}

#[derive(Clone, Default)]
pub struct BridgePreflightSnapshotProvider {
    bindings: Vec<BridgeTransportBinding>,
}

#[derive(Clone, Default)]
pub struct BridgeCutoverSmokeSnapshotProvider {
    bindings: Vec<BridgeTransportBinding>,
    direct_http_probe: Option<DirectHttpModelProbeConfig>,
}

/// Configuration for probing a direct HTTP model provider endpoint in the
/// cutover-smoke diagnostics. Stored separately from the JSON-RPC transport
/// bindings because the direct HTTP path bypasses the loopback bridge.
#[derive(Clone, Debug)]
pub struct DirectHttpModelProbeConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

impl DirectHttpModelProbeConfig {
    /// Return the base URL with any embedded credentials stripped.
    pub fn sanitized_base_url(&self) -> String {
        // Strip user-info (user:password@) from the URL if present.
        // This is a defensive measure -- the base_url should never contain
        // credentials, but we avoid leaking them in diagnostic output.
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

impl BridgeProbeSnapshotProvider {
    pub fn register_probe(
        &mut self,
        server_kind: BridgeServerKind,
        probe: JsonRpcBridgeServerProbeClient,
    ) {
        self.probes
            .retain(|binding| binding.server_kind != server_kind);
        self.probes.push(BridgeProbeBinding { server_kind, probe });
    }

    pub fn register_transport(
        &mut self,
        server_kind: BridgeServerKind,
        transport: Arc<dyn BridgeTransport>,
    ) {
        self.register_probe(server_kind, JsonRpcBridgeServerProbeClient::new(transport));
    }

    fn capture_binding_snapshot(binding: &BridgeProbeBinding) -> BridgeServiceSnapshotDto {
        let (handshake, handshake_error) = match binding.probe.handshake() {
            Ok(handshake) => (Some(handshake), None),
            Err(error) => (None, Some(BridgeProbeErrorDto::from_client_error(error))),
        };
        let (health, health_error) = match binding.probe.health() {
            Ok(health) => (Some(health), None),
            Err(error) => (None, Some(BridgeProbeErrorDto::from_client_error(error))),
        };
        let (service_catalog, service_catalog_error) = match binding.probe.describe_services() {
            Ok(service_catalog) => (Some(service_catalog), None),
            Err(error) => (None, Some(BridgeProbeErrorDto::from_client_error(error))),
        };

        BridgeServiceSnapshotDto {
            server_kind: binding.server_kind,
            handshake,
            handshake_error,
            health,
            health_error,
            service_catalog,
            service_catalog_error,
        }
    }
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
            BridgeServerKind::Host => capture_host_preflight_checks(binding.transport.clone()),
            BridgeServerKind::Model => capture_model_preflight_checks(binding.transport.clone()),
            BridgeServerKind::Mcp => capture_mcp_preflight_checks(binding.transport.clone()),
        };

        BridgePreflightServiceDto {
            server_kind: binding.server_kind,
            checks,
        }
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
            BridgeServerKind::Host => capture_host_cutover_checks(binding.transport.clone()),
            BridgeServerKind::Model => capture_model_cutover_checks(binding.transport.clone()),
            BridgeServerKind::Mcp => capture_mcp_cutover_checks(binding.transport.clone()),
        };

        BridgeCutoverServiceDto::from_checks(binding.server_kind, checks)
    }
}

impl BridgeSnapshotProvider for BridgeProbeSnapshotProvider {
    fn services_snapshot(&self) -> BridgeServicesSnapshotDto {
        BridgeServicesSnapshotDto {
            services: self
                .probes
                .iter()
                .map(Self::capture_binding_snapshot)
                .collect(),
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

impl BridgeCutoverSmokeProvider for BridgeCutoverSmokeSnapshotProvider {
    fn cutover_smoke_snapshot(&self) -> BridgeCutoverSmokeSnapshotDto {
        let mut services: Vec<BridgeCutoverServiceDto> = self
            .bindings
            .iter()
            .map(Self::capture_binding_snapshot)
            .collect();

        // If a direct HTTP model provider is configured, probe it and merge
        // the resulting check into the model service entry.
        if let Some(ref probe_config) = self.direct_http_probe {
            let check = capture_direct_http_model_cutover_check(probe_config);
            if let Some(model_service) = services
                .iter_mut()
                .find(|service| service.server_kind == BridgeServerKind::Model)
            {
                model_service.checks.push(check);
                // Recompute service-level summary after appending the new check.
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

#[derive(Clone)]
struct BridgeProbeBinding {
    server_kind: BridgeServerKind,
    probe: JsonRpcBridgeServerProbeClient,
}

#[derive(Clone)]
struct BridgeTransportBinding {
    server_kind: BridgeServerKind,
    transport: Arc<dyn BridgeTransport>,
}

fn capture_host_preflight_checks(
    transport: Arc<dyn BridgeTransport>,
) -> Vec<BridgePreflightCheckDto> {
    let client = JsonRpcHostBridgeClient::new(transport);
    vec![bridge_response_check(
        "workspace_roots",
        "vscode.workspace_roots",
        client.call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::WorkspaceRoots,
        }),
    )]
}

fn capture_model_preflight_checks(
    transport: Arc<dyn BridgeTransport>,
) -> Vec<BridgePreflightCheckDto> {
    let probe = JsonRpcBridgeServerProbeClient::new(transport.clone());
    let client = JsonRpcModelBridgeClient::new(transport);
    let mut checks = vec![bridge_response_check(
        "invoke",
        SHADOW_MODEL_PROVIDER,
        client.invoke(ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt: "bridge preflight ping".to_string(),
            messages: None,
            tools: None,
        }),
    )];

    if let Ok(catalog) = probe.describe_services() {
        if catalog.services.iter().any(|service| {
            service.service_name == "openai-compatible"
                && service.service_health.as_deref() == Some("ready")
        }) {
            checks.push(bridge_response_check(
                "invoke",
                "openai-compatible",
                client.invoke(ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "bridge preflight ping".to_string(),
                    messages: None,
                    tools: None,
                }),
            ));
        }
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
            "shadow-mcp-manager",
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
            format!("{SHADOW_MCP_SERVER_NAME}.{SHADOW_MCP_TOOL_NAME}"),
            client.call_tool(McpToolCallRequest {
                server_name: SHADOW_MCP_SERVER_NAME.to_string(),
                tool_name: SHADOW_MCP_TOOL_NAME.to_string(),
                input: String::new(),
            }),
        ),
    ]
}

fn capture_host_cutover_checks(transport: Arc<dyn BridgeTransport>) -> Vec<BridgeCutoverCheckDto> {
    let client = JsonRpcHostBridgeClient::new(transport);
    vec![bridge_cutover_response_check(
        "workspace_roots_contract",
        "vscode.workspace_roots",
        client.call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::WorkspaceRoots,
        }),
    )]
}

fn capture_model_cutover_checks(transport: Arc<dyn BridgeTransport>) -> Vec<BridgeCutoverCheckDto> {
    let probe = JsonRpcBridgeServerProbeClient::new(transport.clone());
    let client = JsonRpcModelBridgeClient::new(transport);
    let catalog = probe.describe_services().ok();
    let mut checks = Vec::new();

    let shadow_profile = model_capability_profile(catalog.as_ref(), SHADOW_MODEL_PROVIDER)
        .unwrap_or_else(|| "shadow-model-bridge-payload-v1".to_string());
    checks.push(bridge_cutover_model_check(
        "invoke_contract",
        SHADOW_MODEL_PROVIDER,
        shadow_profile,
        client.invoke(ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt: "bridge cutover smoke".to_string(),
            messages: None,
            tools: None,
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
                target: "shadow-mcp-manager".to_string(),
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

fn bridge_cutover_response_check(
    check_name: impl Into<String>,
    target: impl Into<String>,
    result: Result<BridgeResponse, BridgeClientError>,
) -> BridgeCutoverCheckDto {
    match result {
        Ok(response) => {
            let has_payload = !response.payload.trim().is_empty();
            let ok = response.ok && has_payload;
            let blocking_reason = if !response.ok {
                Some("bridge response was not ok".to_string())
            } else if !has_payload {
                Some("bridge response payload was empty".to_string())
            } else {
                None
            };
            BridgeCutoverCheckDto {
                check_name: check_name.into(),
                target: target.into(),
                ok,
                blocking_reason,
                response_excerpt: Some(excerpt(&response.payload)),
                error: None,
                model_contract: None,
                mcp_contract: None,
            }
        }
        Err(error) => BridgeCutoverCheckDto {
            check_name: check_name.into(),
            target: target.into(),
            ok: false,
            blocking_reason: Some("bridge invocation failed".to_string()),
            response_excerpt: None,
            error: Some(BridgeProbeErrorDto::from_client_error(error)),
            model_contract: None,
            mcp_contract: None,
        },
    }
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

fn evaluate_model_contract(payload: &str, contract_profile: String) -> BridgeModelContractDto {
    if payload.trim().is_empty() {
        return BridgeModelContractDto {
            contract_profile,
            payload_kind: "plain_text".to_string(),
            contract_ok: false,
            has_content: false,
            has_finish_reason: false,
            has_usage: false,
            tool_call_count: 0,
            blocking_reason: Some("bridge payload was empty".to_string()),
        };
    }

    if let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(payload) {
        let has_content = payload
            .get("content")
            .and_then(Value::as_str)
            .map(|content| !content.trim().is_empty())
            .unwrap_or(false);
        let has_finish_reason = payload
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(|reason| !reason.trim().is_empty())
            .unwrap_or(false);
        let has_usage = payload.get("usage").is_some();
        let tool_calls = payload
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tool_call_count = tool_calls.len();
        let tool_calls_valid = tool_calls.iter().all(|tool_call| {
            tool_call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(|name| !name.trim().is_empty())
                .unwrap_or(false)
                && tool_call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                    .is_some()
        });
        let contract_ok = (has_content || tool_call_count > 0) && tool_calls_valid;
        let blocking_reason = if !tool_calls_valid {
            Some("structured payload contains invalid tool_calls".to_string())
        } else if !has_content && tool_call_count == 0 {
            Some("structured payload missing content or tool_calls".to_string())
        } else {
            None
        };

        return BridgeModelContractDto {
            contract_profile,
            payload_kind: "structured_json".to_string(),
            contract_ok,
            has_content,
            has_finish_reason,
            has_usage,
            tool_call_count,
            blocking_reason,
        };
    }

    BridgeModelContractDto {
        contract_profile,
        payload_kind: "plain_text".to_string(),
        contract_ok: true,
        has_content: true,
        has_finish_reason: false,
        has_usage: false,
        tool_call_count: 0,
        blocking_reason: None,
    }
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

fn excerpt(value: &str) -> String {
    let mut chars = value.chars();
    let excerpt: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{
        BridgeResponse, BridgeServerServiceDescriptor, BridgeTransportError,
        BridgeTransportRequest, BridgeTransportResponse, LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD,
        LOCAL_BRIDGE_HANDSHAKE_METHOD, LOCAL_BRIDGE_HEALTH_METHOD, McpManagerListServersResponse,
    };
    use serde_json::{Value, json};
    use std::{collections::HashMap, sync::Mutex};

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
            protocol_version: "shadow-local-bridge-v1".to_string(),
            server_kind: kind,
            health_method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
            supported_methods: vec!["bridge.describe_services".to_string()],
        })
        .expect("handshake should serialize")
    }

    fn health(kind: BridgeServerKind, status: &str, ok: bool) -> Value {
        serde_json::to_value(BridgeServerHealth {
            protocol_version: "shadow-local-bridge-v1".to_string(),
            server_kind: kind,
            status: status.to_string(),
            ok,
        })
        .expect("health should serialize")
    }

    fn catalog(kind: BridgeServerKind) -> Value {
        serde_json::to_value(BridgeServerServiceCatalog {
            protocol_version: "shadow-local-bridge-v1".to_string(),
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
                "shadow-mcp-manager",
                default_route_status,
                default_route_target,
            ),
            servers: vec![
                descriptor_with_profile(
                    SHADOW_MCP_SERVER_NAME,
                    Some("ready"),
                    Some("inspection-core-v1"),
                ),
                descriptor_with_profile(
                    "shadow-mcp-observability",
                    Some("ready"),
                    Some("observability-v1"),
                ),
            ],
            selection_targets: vec![
                SHADOW_MCP_SERVER_NAME.to_string(),
                "shadow-mcp-observability".to_string(),
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
            BridgeServerKind::Host,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    LOCAL_BRIDGE_HANDSHAKE_METHOD.to_string(),
                    FakeTransportOutcome::Payload(handshake(BridgeServerKind::Host)),
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
                    FakeTransportOutcome::Payload(catalog(BridgeServerKind::Host)),
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
                message:
                    "桥接调用失败[RemoteBusiness]: remote business error [-32042]: probe degraded"
                        .to_string(),
            })
        );
    }

    #[test]
    fn preflight_snapshot_provider_executes_real_smoke_checks_from_transports() {
        let mut provider = BridgePreflightSnapshotProvider::default();
        provider.register_transport(
            BridgeServerKind::Host,
            Arc::new(FakeTransport::new(HashMap::from([(
                "host.call".to_string(),
                FakeTransportOutcome::Payload(bridge_response("workspace:///repo")),
            )]))),
        );
        provider.register_transport(
            BridgeServerKind::Model,
            Arc::new(FakeTransport::new(HashMap::from([
                (
                    "model.invoke".to_string(),
                    FakeTransportOutcome::Payload(bridge_response(
                        "shadow-model::bridge preflight ping",
                    )),
                ),
                (
                    LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                    FakeTransportOutcome::Payload(
                        serde_json::to_value(BridgeServerServiceCatalog {
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![descriptor(SHADOW_MODEL_PROVIDER)],
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
                            manager: descriptor("shadow-mcp-manager"),
                            servers: vec![descriptor(SHADOW_MCP_SERVER_NAME)],
                            selection_targets: vec![SHADOW_MCP_SERVER_NAME.to_string()],
                            default_route_status: "available".to_string(),
                            default_route_target: SHADOW_MCP_SERVER_NAME.to_string(),
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
        assert_eq!(snapshot.services.len(), 3);
        assert_eq!(snapshot.services[0].checks[0].check_name, "workspace_roots");
        assert!(snapshot.services[0].checks[0].ok);
        assert_eq!(snapshot.services[1].checks[0].target, SHADOW_MODEL_PROVIDER);
        assert!(snapshot.services[1].checks[0].ok);
        assert_eq!(snapshot.services[2].checks[0].check_name, "list_servers");
        assert!(snapshot.services[2].checks[0].ok);
        assert_eq!(
            snapshot.services[2].checks[1].target,
            format!("{SHADOW_MCP_SERVER_NAME}.{SHADOW_MCP_TOOL_NAME}")
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![
                                descriptor(SHADOW_MODEL_PROVIDER),
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
                .any(|check| check.target == SHADOW_MODEL_PROVIDER && check.ok),
            "shadow-model smoke should still be present: {checks:?}"
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![descriptor(SHADOW_MODEL_PROVIDER)],
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
        assert_eq!(check.target, SHADOW_MODEL_PROVIDER);
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![
                                descriptor_with_profile(
                                    SHADOW_MODEL_PROVIDER,
                                    Some("ready"),
                                    Some("shadow-model-bridge-payload-v1"),
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
                        "shadow-mcp-observability",
                    )),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
                            Some("ready"),
                            Some("observability-v1"),
                        ),
                        "lifecycle_events": [],
                    })),
                ),
                (
                    "mcp.call_tool".to_string(),
                    FakeTransportOutcome::Payload(structured_bridge_response(json!({
                        "server_name": "shadow-mcp-observability",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-observability",
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
        assert_eq!(mcp_gate.route_target, "shadow-mcp-observability");
        assert_eq!(
            mcp_gate.resolved_server.as_deref(),
            Some("shadow-mcp-observability")
        );
        assert!(mcp_gate.contract_ok);
        let check = &mcp.checks[0];
        assert!(check.ok, "mcp contract should pass: {check:?}");
        let contract = check
            .mcp_contract
            .as_ref()
            .expect("mcp contract should be attached");
        assert_eq!(contract.route_status, "ready");
        assert_eq!(contract.route_target, "shadow-mcp-observability");
        assert_eq!(
            contract.resolved_server.as_deref(),
            Some("shadow-mcp-observability")
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![
                                descriptor_with_profile(
                                    SHADOW_MODEL_PROVIDER,
                                    Some("ready"),
                                    Some("shadow-model-bridge-payload-v1"),
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
        assert!(snapshot
            .blocking_issues
            .iter()
            .all(|issue| issue.server_kind == BridgeServerKind::Model
                && issue.facet == BridgeCutoverBlockingFacet::ModelContract
                && issue.reason_code
                    == BridgeCutoverBlockingReasonCode::ModelStructuredPayloadMissingContentOrToolCalls));
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
                SHADOW_MODEL_PROVIDER.to_string(),
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![
                                descriptor_with_profile(
                                    SHADOW_MODEL_PROVIDER,
                                    Some("ready"),
                                    Some("shadow-model-bridge-payload-v1"),
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
    fn cutover_smoke_snapshot_provider_reports_empty_host_payload_issue() {
        let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
        provider.register_transport(
            BridgeServerKind::Host,
            Arc::new(FakeTransport::new(HashMap::from([(
                "host.call".to_string(),
                FakeTransportOutcome::Payload(bridge_response("")),
            )]))),
        );

        let snapshot = provider.cutover_smoke_snapshot();
        assert!(!snapshot.overall_ok);
        assert_eq!(snapshot.blocking_check_count, 1);
        assert_eq!(snapshot.blocking_services, vec![BridgeServerKind::Host]);
        assert_eq!(snapshot.blocking_issues.len(), 1);
        let issue = &snapshot.blocking_issues[0];
        assert_eq!(issue.server_kind, BridgeServerKind::Host);
        assert_eq!(issue.check_name, "workspace_roots_contract");
        assert_eq!(issue.target, "vscode.workspace_roots");
        assert_eq!(issue.facet, BridgeCutoverBlockingFacet::BridgeResponse);
        assert_eq!(
            issue.reason_code,
            BridgeCutoverBlockingReasonCode::BridgeResponseEmptyPayload
        );
        assert_eq!(issue.blocking_reason, "bridge response payload was empty");
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
        assert_eq!(issue.target, "shadow-mcp-manager");
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
    fn cutover_smoke_snapshot_provider_reports_bridge_invocation_failure_issue() {
        let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
        provider.register_transport(
            BridgeServerKind::Host,
            Arc::new(FakeTransport::new(HashMap::from([(
                "host.call".to_string(),
                FakeTransportOutcome::RemoteBusiness {
                    code: -32021,
                    message: "workspace unavailable".to_string(),
                    data: None,
                },
            )]))),
        );

        let snapshot = provider.cutover_smoke_snapshot();
        assert!(!snapshot.overall_ok);
        assert_eq!(snapshot.blocking_issues.len(), 1);
        let issue = &snapshot.blocking_issues[0];
        assert_eq!(issue.facet, BridgeCutoverBlockingFacet::BridgeResponse);
        assert_eq!(
            issue.reason_code,
            BridgeCutoverBlockingReasonCode::BridgeInvocationFailed
        );
        assert_eq!(issue.blocking_reason, "bridge invocation failed");
        assert_eq!(
            issue.error.as_ref().and_then(|error| error.layer),
            Some(BridgeErrorLayer::RemoteBusiness)
        );
    }

    #[test]
    fn cutover_smoke_snapshot_provider_reports_bridge_response_not_ok_issue() {
        let mut provider = BridgeCutoverSmokeSnapshotProvider::default();
        provider.register_transport(
            BridgeServerKind::Host,
            Arc::new(FakeTransport::new(HashMap::from([(
                "host.call".to_string(),
                FakeTransportOutcome::Payload(bridge_response_with_status(false, "denied")),
            )]))),
        );

        let snapshot = provider.cutover_smoke_snapshot();
        assert!(!snapshot.overall_ok);
        assert_eq!(snapshot.blocking_issues.len(), 1);
        let issue = &snapshot.blocking_issues[0];
        assert_eq!(
            issue.reason_code,
            BridgeCutoverBlockingReasonCode::BridgeResponseNotOk
        );
        assert_eq!(issue.blocking_reason, "bridge response was not ok");
        assert_eq!(issue.response_excerpt.as_deref(), Some("denied"));
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![descriptor_with_profile(
                                SHADOW_MODEL_PROVIDER,
                                Some("ready"),
                                Some("shadow-model-bridge-payload-v1"),
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
        assert_eq!(issue.target, SHADOW_MODEL_PROVIDER);
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
                            protocol_version: "shadow-local-bridge-v1".to_string(),
                            server_kind: BridgeServerKind::Model,
                            services: vec![descriptor_with_profile(
                                SHADOW_MODEL_PROVIDER,
                                Some("ready"),
                                Some("shadow-model-bridge-payload-v1"),
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
                        "shadow-mcp-observability",
                    )),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "fallback-only",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
                            Some("degraded"),
                            Some("observability-v1"),
                        ),
                        "lifecycle_events": [],
                    })),
                ),
                (
                    "mcp.call_tool".to_string(),
                    FakeTransportOutcome::Payload(structured_bridge_response(json!({
                        "server_name": "shadow-mcp-observability",
                        "default_route_status": "fallback-only",
                        "default_route_target": "shadow-mcp-observability",
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
                        "shadow-mcp-observability",
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
                        "shadow-mcp-observability",
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
                        "server_name": "shadow-mcp-observability",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-observability",
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
            "default route target shadow-mcp-observability could not be described"
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
                        "shadow-mcp-observability",
                    )),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
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
                        "shadow-mcp-observability",
                    )),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
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
                        "shadow-mcp-observability",
                    )),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
                            Some("ready"),
                            Some("observability-v1"),
                        ),
                        "lifecycle_events": [],
                    })),
                ),
                (
                    "mcp.call_tool".to_string(),
                    FakeTransportOutcome::Payload(structured_bridge_response(json!({
                        "server_name": "shadow-mcp-observability",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-inspection",
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
                        "shadow-mcp-observability",
                    )),
                ),
                (
                    "mcp.describe_server".to_string(),
                    FakeTransportOutcome::Payload(json!({
                        "manager": descriptor_with_route(
                            "shadow-mcp-manager",
                            "ready",
                            "shadow-mcp-observability",
                        ),
                        "server": descriptor_with_profile(
                            "shadow-mcp-observability",
                            Some("ready"),
                            Some("observability-v1"),
                        ),
                        "lifecycle_events": [],
                    })),
                ),
                (
                    "mcp.call_tool".to_string(),
                    FakeTransportOutcome::Payload(structured_bridge_response(json!({
                        "server_name": "shadow-mcp-inspection",
                        "default_route_status": "ready",
                        "default_route_target": "shadow-mcp-observability",
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
            BridgeServerKind::Host,
            Arc::new(FakeTransport::new(HashMap::from([(
                "host.call".to_string(),
                FakeTransportOutcome::Payload(bridge_response("")),
            )]))),
        );
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
        assert_eq!(
            snapshot.blocking_services,
            vec![BridgeServerKind::Host, BridgeServerKind::Mcp]
        );
        assert_eq!(
            snapshot
                .blocking_issue_counts_by_reason_code
                .get("bridge_response_empty_payload"),
            Some(&1)
        );
        assert_eq!(
            snapshot
                .blocking_issue_counts_by_reason_code
                .get("mcp_manager_list_servers_failed"),
            Some(&1)
        );
        assert_eq!(
            snapshot.blocking_issue_counts_by_server_kind.get("host"),
            Some(&1)
        );
        assert_eq!(
            snapshot.blocking_issue_counts_by_server_kind.get("mcp"),
            Some(&1)
        );
    }
}
