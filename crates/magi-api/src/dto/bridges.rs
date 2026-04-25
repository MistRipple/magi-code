use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, BridgeServerHandshake, BridgeServerHealth,
    BridgeServerKind, BridgeServerServiceCatalog,
};
use serde::Serialize;
use std::collections::BTreeMap;

pub use super::bridge_contracts::{BridgeMcpDefaultRouteContractDto, BridgeModelContractDto};
pub use super::bridge_reason_codes::BridgeCutoverBlockingReasonCode;
use super::bridge_reason_codes::{
    blocking_issue_counts_by_reason_code, blocking_issue_counts_by_server_kind,
};
pub use super::bridge_snapshot_providers::{
    BridgeCutoverSmokeSnapshotProvider, BridgePreflightSnapshotProvider,
    BridgeProbeSnapshotProvider, DirectHttpModelProbeConfig,
};

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
    pub(crate) fn from_services(services: Vec<BridgeCutoverServiceDto>) -> Self {
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
    pub(crate) fn from_checks(
        server_kind: BridgeServerKind,
        checks: Vec<BridgeCutoverCheckDto>,
    ) -> Self {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BridgeProbeErrorDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<BridgeErrorLayer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
    pub message: String,
}

impl BridgeProbeErrorDto {
    pub(crate) fn from_client_error(error: BridgeClientError) -> Self {
        Self {
            layer: error.layer(),
            code: error.code(),
            message: error.to_string(),
        }
    }
}
