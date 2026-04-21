use crate::{
    local_process_protocol::{
        run_local_process_bridge_server, BridgeServerKind, LocalProcessBridgeRequest,
        LocalProcessBridgeRpcError, LocalProcessBridgeServerError,
    },
    HostBridgeCommand, HostBridgeRequest,
};
use serde_json::json;

mod catalog;
mod descriptors;
mod prehost;
mod static_scan;
mod terminal_policy;

use catalog::{host_service_catalog, host_service_shims};
use descriptors::host_kind_label;

const VSCODE_PREHOST_WORKSPACE_ROOTS_ENV: &str = "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS";

pub fn run_vscode_host_shell_server() -> Result<(), LocalProcessBridgeServerError> {
    let shims = host_service_shims();
    run_local_process_bridge_server(
        BridgeServerKind::Host,
        "host.call",
        host_service_catalog(&shims),
        move |request| handle_host_call(&shims, request),
    )
}

pub fn run_host_bridge_loopback_server() -> Result<(), LocalProcessBridgeServerError> {
    run_vscode_host_shell_server()
}

fn handle_host_call(
    shims: &[catalog::HostServiceShim],
    request: LocalProcessBridgeRequest,
) -> Result<serde_json::Value, LocalProcessBridgeRpcError> {
    let _request_id = request.id;
    let host_request: HostBridgeRequest = match serde_json::from_value(request.params) {
        Ok(request) => request,
        Err(error) => {
            return Err(LocalProcessBridgeRpcError::invalid_params(error.to_string()));
        }
    };

    let shim = shims
        .iter()
        .find(|shim| shim.handles(host_request.host_kind))
        .ok_or_else(|| {
            LocalProcessBridgeRpcError::remote_business(
                -32013,
                "unsupported host kind",
                Some(json!({
                    "host_kind": host_kind_label(host_request.host_kind),
                })),
            )
        })?;

    let response = shim.execute(host_request.command)?;

    serde_json::to_value(response).map_err(|error| {
        LocalProcessBridgeRpcError::invalid_params(format!(
            "serialize host bridge response failed: {error}"
        ))
    })
}

fn host_command_name(command: &HostBridgeCommand) -> &'static str {
    match command {
        HostBridgeCommand::WorkspaceRoots => "WorkspaceRoots",
        HostBridgeCommand::OpenFile { .. } => "OpenFile",
        HostBridgeCommand::RevealDiff { .. } => "RevealDiff",
        HostBridgeCommand::ReadDiagnostics { .. } => "ReadDiagnostics",
        HostBridgeCommand::ReadSymbols { .. } => "ReadSymbols",
        HostBridgeCommand::TerminalExec { .. } => "TerminalExec",
    }
}
