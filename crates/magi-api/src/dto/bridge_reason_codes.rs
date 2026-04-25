use super::bridge_contracts::{BridgeMcpDefaultRouteContractDto, BridgeModelContractDto};
use super::bridges::{
    BridgeCutoverBlockingFacet, BridgeCutoverBlockingIssueDto, BridgeCutoverCheckDto,
};
use magi_bridge_client::BridgeServerKind;
use serde::Serialize;
use std::collections::BTreeMap;

pub(crate) const MCP_MANAGER_LIST_SERVERS_FAILED_REASON: &str = "mcp manager list_servers failed";
pub(crate) const MCP_DEFAULT_ROUTE_FALLBACK_ONLY_REASON: &str = "default route is fallback-only";
pub(crate) const MCP_DEFAULT_ROUTE_UNAVAILABLE_REASON: &str = "default route is unavailable";
pub(crate) const MCP_BLANK_SELECTION_INVOCATION_FAILED_REASON: &str =
    "blank selection invocation failed";
pub(crate) const MCP_BLANK_SELECTION_RESPONSE_NOT_OK_REASON: &str =
    "blank selection response was not ok";
pub(crate) const MCP_DEFAULT_ROUTE_METADATA_DRIFT_REASON: &str =
    "blank selection payload drifted from manager metadata";
pub(crate) const MCP_DEFAULT_ROUTE_RESOLVED_SERVER_MISMATCH_REASON: &str =
    "blank selection resolved to the wrong MCP server";

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

pub(crate) fn blocking_issue_counts_by_reason_code(
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

pub(crate) fn blocking_issue_counts_by_server_kind(
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

impl BridgeCutoverCheckDto {
    pub(crate) fn blocking_issue(
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

fn bridge_server_kind_key(kind: BridgeServerKind) -> &'static str {
    match kind {
        BridgeServerKind::Model => "model",
        BridgeServerKind::Host => "host",
        BridgeServerKind::Mcp => "mcp",
    }
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
