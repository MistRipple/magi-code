use super::catalog::HostServiceShim;
use crate::{BridgeResponse, HostKind};
use serde_json::{Value, json};

pub(super) fn host_kind_label(host_kind: HostKind) -> &'static str {
    match host_kind {
        HostKind::Vscode => "vscode",
        HostKind::Idea => "idea",
    }
}

pub(super) fn shadow_host_payload(shim: &HostServiceShim, command: &str, details: Value) -> String {
    let runtime_status = shim.runtime_status();
    json!({
        "bridge_kind": "host",
        "implementation_source": shim.implementation_source,
        "capability_profile": shim.capability_profile,
        "workspace_roots_source": runtime_status.workspace_roots_source,
        "service_health": runtime_status.service_health,
        "service_health_reason": runtime_status.service_health_reason,
        "runtime_mode": runtime_status.runtime_mode,
        "terminal_exec_mode": runtime_status.terminal_exec_mode,
        "host_shell_manifest": shim.shell_manifest(),
        "host_shell_profile": shim.shell_profile(),
        "host_command_capability_profile": shim.command_capability_profile(command),
        "host_session": shim.session_descriptor(),
        "workspace_context": shim.workspace_context(),
        "context_resolution_boundary": shim.context_resolution_boundary(command),
        "host_kind": host_kind_label(shim.host_kind),
        "command": command,
        "status": "ok",
        "details": details,
    })
    .to_string()
}

pub(super) fn success_response(
    shim: &HostServiceShim,
    command: &str,
    details: Value,
) -> BridgeResponse {
    BridgeResponse {
        ok: true,
        payload: shadow_host_payload(shim, command, details),
    }
}
