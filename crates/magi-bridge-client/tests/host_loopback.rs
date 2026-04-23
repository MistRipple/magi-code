use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, BridgeTransportError, BridgeTransportRequest,
    HostBridgeClient, HostBridgeCommand, HostBridgeRequest, HostKind,
    JsonRpcBridgeServerProbeClient, JsonRpcHostBridgeClient, JsonRpcStdioTransport,
};
use serde_json::{Value, json};
use std::{fs, sync::Arc};

fn loopback_transport() -> JsonRpcStdioTransport {
    let mut path = std::env::current_exe().expect("current exe should exist");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("host_bridge_loopback");
    JsonRpcStdioTransport::new(path.to_string_lossy().to_string())
}

fn loopback_transport_with_env(envs: &[(&str, &str)]) -> JsonRpcStdioTransport {
    let mut transport = loopback_transport();
    for (key, value) in envs {
        transport = transport.with_env(*key, *value);
    }
    transport
}

fn current_workspace_root() -> String {
    let root = std::env::current_dir().expect("current dir should exist");
    let canonical = root.canonicalize().unwrap_or(root);
    canonical.to_string_lossy().to_string()
}

fn temp_file(name: &str, content: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "magi-host-loopback-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos()
    ));
    fs::write(&path, content).expect("temp file should be writable");
    path.to_string_lossy().to_string()
}

fn temp_dir(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "magi-host-loopback-dir-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("temp dir should be creatable");
    path.to_string_lossy().to_string()
}

fn workspace_temp_file(name: &str, content: &str) -> String {
    let root = std::env::current_dir().expect("current dir should exist");
    let path = root.join(format!(
        ".magi-host-loopback-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos()
    ));
    fs::write(&path, content).expect("workspace temp file should be writable");
    path.to_string_lossy().to_string()
}

#[test]
fn host_client_round_trips_workspace_roots() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));

    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::WorkspaceRoots,
        })
        .expect("host workspace roots should succeed");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["bridge_kind"], "host");
    assert_eq!(payload["host_kind"], "vscode");
    assert_eq!(payload["command"], "WorkspaceRoots");
    assert_eq!(
        payload["host_shell_manifest"]["shell_id"],
        "shadow-host-vscode"
    );
    assert_eq!(payload["host_shell_manifest"]["minimum_version"], "0.1.0");
    assert_eq!(
        payload["host_shell_manifest"]["capability_version"],
        "host-shell-v1"
    );
    assert_eq!(
        payload["host_shell_manifest"]["implementation_source"],
        "real-prehost"
    );
    assert_eq!(
        payload["host_shell_manifest"]["capability_profile"],
        "vscode-host-shell-prehost-v1"
    );
    assert_eq!(
        payload["host_shell_manifest"]["workspace_roots_source"],
        "filesystem:current_dir"
    );
    assert_eq!(
        payload["host_shell_profile"]["profile_id"],
        "shadow-host-vscode-profile"
    );
    assert_eq!(
        payload["host_shell_profile"]["shell_family"],
        "vscode-extension-host"
    );
    assert_eq!(
        payload["host_command_capability_profile"]["capability_id"],
        "shadow-host-vscode::workspaceroots::capability"
    );
    assert_eq!(
        payload["host_command_capability_profile"]["interaction_mode"],
        "query"
    );
    assert_eq!(
        payload["host_session"]["session_id"],
        "shadow-host-session-vscode"
    );
    assert_eq!(
        payload["workspace_context"]["workspace_id"],
        "shadow-workspace-vscode"
    );
    assert_eq!(
        payload["workspace_context"]["workspace_roots_source"],
        "filesystem:current_dir"
    );
    assert_eq!(payload["implementation_source"], "real-prehost");
    assert_eq!(
        payload["capability_profile"],
        "vscode-host-shell-prehost-v1"
    );
    assert_eq!(payload["workspace_roots_source"], "filesystem:current_dir");
    assert_eq!(payload["service_health"], "healthy");
    assert_eq!(payload["runtime_mode"], "real-prehost");
    assert_eq!(payload["terminal_exec_mode"], "disabled");
    assert_eq!(
        payload["context_resolution_boundary"]["session_resolution_strategy"],
        "host-kind-derived session mapping"
    );
    assert_eq!(
        payload["details"]["workspace_roots"][0],
        current_workspace_root()
    );
    assert_eq!(
        payload["details"]["workspace_roots_source"],
        "filesystem:current_dir"
    );
}

#[test]
fn host_loopback_exposes_shared_handshake_and_health() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport()));

    let handshake = probe.handshake().expect("host handshake should succeed");
    assert_eq!(handshake.server_kind, BridgeServerKind::Host);
    assert!(
        handshake
            .supported_methods
            .contains(&"host.call".to_string())
    );

    let health = probe.health().expect("host health should succeed");
    assert_eq!(health.server_kind, BridgeServerKind::Host);
    assert!(health.ok);

    let catalog = probe
        .describe_services()
        .expect("host service catalog should succeed");
    assert_eq!(catalog.server_kind, BridgeServerKind::Host);
    assert_eq!(catalog.services.len(), 2);
    assert!(
        catalog
            .services
            .iter()
            .any(|service| service.service_name == "shadow-host-vscode")
    );
    assert!(
        catalog
            .services
            .iter()
            .any(|service| service.service_name == "shadow-host-idea")
    );
    assert!(catalog.services.iter().all(|service| {
        service
            .capabilities
            .contains(&"command:OpenFile".to_string())
    }));
    assert!(catalog.services.iter().all(|service| {
        service
            .capabilities
            .contains(&"capability_version:host-shell-v1".to_string())
    }));
    assert!(catalog.services.iter().all(|service| {
        service
            .capabilities
            .contains(&"shell_profile:shadow-host-shell-profile-v1".to_string())
    }));
    assert!(
        catalog
            .services
            .iter()
            .all(|service| service.shell_manifest.as_ref().is_some())
    );
    assert!(
        catalog
            .services
            .iter()
            .all(|service| service.shell_profile.as_ref().is_some())
    );
    assert!(
        catalog
            .services
            .iter()
            .all(|service| service.command_capability_profiles.as_ref().is_some())
    );
    let vscode_service = catalog
        .services
        .iter()
        .find(|service| service.service_name == "shadow-host-vscode")
        .expect("vscode host service should exist");
    assert_eq!(vscode_service.service_health.as_deref(), Some("healthy"));
    let idea_service = catalog
        .services
        .iter()
        .find(|service| service.service_name == "shadow-host-idea")
        .expect("idea host service should exist");
    assert_eq!(idea_service.service_health.as_deref(), Some("unavailable"));
    assert_eq!(
        idea_service.service_health_reason.as_deref(),
        Some("idea host shell not implemented")
    );
    assert_eq!(
        idea_service.implementation_source.as_deref(),
        Some("boundary-placeholder")
    );
    let manifest = vscode_service
        .shell_manifest
        .as_ref()
        .expect("host manifest should exist");
    assert_eq!(manifest.shell_id, "shadow-host-vscode");
    assert_eq!(manifest.minimum_version, "0.1.0");
    assert_eq!(manifest.capability_version, "host-shell-v1");
    let shell_profile = vscode_service
        .shell_profile
        .as_ref()
        .expect("host shell profile should exist");
    assert_eq!(shell_profile.profile_id, "shadow-host-vscode-profile");
    assert_eq!(shell_profile.host_kind, "vscode");
    assert_eq!(shell_profile.shell_family, "vscode-extension-host");
    let session = vscode_service
        .session_descriptor
        .as_ref()
        .expect("host session descriptor should exist");
    assert_eq!(session.session_id, "shadow-host-session-vscode");
    assert_eq!(session.session_scope, "session-scoped");
    let workspace = vscode_service
        .workspace_context
        .as_ref()
        .expect("host workspace context should exist");
    assert_eq!(workspace.workspace_id, "shadow-workspace-vscode");
    assert_eq!(workspace.workspace_scope, "workspace-scoped");
    let command_profiles = vscode_service
        .command_capability_profiles
        .as_ref()
        .expect("host command capability profiles should exist");
    let terminal_profile = command_profiles
        .iter()
        .find(|profile| profile.command_name == "TerminalExec")
        .expect("terminal capability profile should exist");
    assert_eq!(
        terminal_profile.side_effect_level,
        "workspace_write_possible"
    );
    assert_eq!(terminal_profile.path_argument_policy, "working_directory");
    let context_boundary = vscode_service
        .context_resolution_boundary
        .as_ref()
        .expect("host context resolution boundary should exist");
    assert_eq!(
        context_boundary.request_binding,
        "bridge.describe_services -> host.call(vscode:host.call)"
    );
    assert_eq!(
        context_boundary.workspace_resolution_source,
        "host_kind + shadow workspace shim"
    );
}

#[test]
fn host_catalog_reflects_env_configured_workspace_roots() {
    let workspace_root = temp_dir("env-root");
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS",
        &workspace_root,
    )])));

    let catalog = probe
        .describe_services()
        .expect("host service catalog should succeed");
    let vscode_service = catalog
        .services
        .iter()
        .find(|service| service.service_name == "shadow-host-vscode")
        .expect("vscode host service should exist");
    assert_eq!(
        vscode_service.workspace_roots_source.as_deref(),
        Some("env:MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS")
    );
    assert_eq!(vscode_service.service_health.as_deref(), Some("healthy"));

    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS",
        &workspace_root,
    )])));
    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::WorkspaceRoots,
        })
        .expect("workspace roots should resolve from env");
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(
        payload["workspace_roots_source"],
        "env:MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS"
    );
    assert_eq!(payload["service_health"], "healthy");
    assert_eq!(
        payload["details"]["workspace_roots"][0],
        fs::canonicalize(workspace_root)
            .expect("workspace root should canonicalize")
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn vscode_prehost_reports_unavailable_when_workspace_roots_invalid() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS",
        "/definitely/missing/root",
    )])));

    let catalog = probe
        .describe_services()
        .expect("host service catalog should succeed");
    let vscode_service = catalog
        .services
        .iter()
        .find(|service| service.service_name == "shadow-host-vscode")
        .expect("vscode host service should exist");
    assert_eq!(
        vscode_service.service_health.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        vscode_service.service_health_reason.as_deref(),
        Some("no valid workspace roots resolved")
    );

    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS",
        "/definitely/missing/root",
    )])));
    let error = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::WorkspaceRoots,
        })
        .expect_err("invalid workspace roots should reject vscode prehost calls");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
}

#[test]
fn idea_host_boundary_rejects_terminal_exec() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));

    let error = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Idea,
            command: HostBridgeCommand::TerminalExec {
                command: "echo hello".to_string(),
                working_directory: "/tmp".to_string(),
            },
        })
        .expect_err("idea host shell should reject until real implementation exists");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
}

#[test]
fn host_client_round_trips_open_file() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));
    let path = workspace_temp_file("open-file", "fn sample_open() {}\n");

    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::OpenFile {
                absolute_path: path.clone(),
                line: Some(12),
                column: Some(4),
            },
        })
        .expect("host open file should succeed");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["command"], "OpenFile");
    assert_eq!(
        payload["details"]["absolute_path"],
        fs::canonicalize(path)
            .expect("temp file should canonicalize")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(payload["details"]["line"], 12);
    assert_eq!(payload["details"]["column"], 4);
    assert_eq!(payload["details"]["file_type"], "file");
    assert_eq!(
        payload["details"]["implementation_mode"],
        "filesystem-prehost"
    );
}

#[test]
fn vscode_open_file_rejects_paths_outside_configured_workspace_roots() {
    let workspace_root = temp_dir("bounded-root");
    let outside_path = temp_file("outside-open-file", "fn outside() {}\n");
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS",
        &workspace_root,
    )])));

    let error = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::OpenFile {
                absolute_path: outside_path,
                line: Some(1),
                column: Some(1),
            },
        })
        .expect_err("open file outside workspace roots should be rejected");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
}

#[test]
fn host_client_round_trips_reveal_diff() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));
    let left = workspace_temp_file("left", "fn alpha() {}\n");
    let right = workspace_temp_file("right", "fn alpha() {}\nfn beta() {}\n");

    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::RevealDiff {
                left_path: left.clone(),
                right_path: right.clone(),
            },
        })
        .expect("host reveal diff should succeed");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["command"], "RevealDiff");
    assert_eq!(
        payload["details"]["left_path"],
        fs::canonicalize(left)
            .expect("left file should canonicalize")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(
        payload["details"]["right_path"],
        fs::canonicalize(right)
            .expect("right file should canonicalize")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(payload["details"]["same_content"], false);
    assert_eq!(
        payload["details"]["implementation_mode"],
        "filesystem-prehost"
    );
}

#[test]
fn vscode_reveal_diff_rejects_paths_outside_configured_workspace_roots() {
    let workspace_root = temp_dir("bounded-diff-root");
    let inside_path = std::path::PathBuf::from(&workspace_root).join("inside.rs");
    fs::write(&inside_path, "fn inside() {}\n").expect("inside file should be writable");
    let outside_path = temp_file("outside-diff-file", "fn outside() {}\n");
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport_with_env(&[(
        "MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS",
        &workspace_root,
    )])));

    let error = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::RevealDiff {
                left_path: inside_path.to_string_lossy().to_string(),
                right_path: outside_path,
            },
        })
        .expect_err("reveal diff outside workspace roots should be rejected");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
}

#[test]
fn host_client_round_trips_read_diagnostics() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));
    let path = workspace_temp_file(
        "diagnostics",
        "// TODO: clean up\nfn diagnostics() { unwrap(); }\n// FIXME: handle edge case\n",
    );

    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::ReadDiagnostics {
                absolute_path: path.clone(),
            },
        })
        .expect("host read diagnostics should succeed");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["command"], "ReadDiagnostics");
    assert_eq!(
        payload["details"]["absolute_path"],
        fs::canonicalize(path)
            .expect("temp file should canonicalize")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(payload["details"]["analysis_mode"], "prehost-static-scan");
    assert_eq!(
        payload["details"]["diagnostics"].as_array().unwrap().len(),
        3
    );
}

#[test]
fn host_client_round_trips_read_symbols() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));
    let path = workspace_temp_file(
        "symbols",
        "pub struct Widget {}\nfn compute() {}\nmod tools {}\n",
    );

    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::ReadSymbols {
                absolute_path: path.clone(),
            },
        })
        .expect("host read symbols should succeed");

    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["command"], "ReadSymbols");
    assert_eq!(
        payload["details"]["absolute_path"],
        fs::canonicalize(path)
            .expect("temp file should canonicalize")
            .to_string_lossy()
            .to_string()
    );
    let symbols = payload["details"]["symbols"].as_array().unwrap();
    assert_eq!(symbols.len(), 3);
    assert_eq!(symbols[0]["name"], "Widget");
    assert_eq!(symbols[1]["name"], "compute");
    assert_eq!(symbols[2]["name"], "tools");
}

#[test]
fn vscode_terminal_exec_is_rejected_in_prehost_mode() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport()));

    let error = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::TerminalExec {
                command: "echo hello".to_string(),
                working_directory: std::env::temp_dir().to_string_lossy().to_string(),
            },
        })
        .expect_err("vscode prehost terminal exec should be rejected");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
}

#[test]
fn vscode_terminal_exec_can_run_allowlisted_command_when_enabled() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_VSCODE_PREHOST_TERMINAL_MODE", "allowlisted"),
        ("MAGI_VSCODE_PREHOST_ALLOWED_COMMANDS", "pwd"),
    ])));

    let response = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::TerminalExec {
                command: "pwd".to_string(),
                working_directory: current_workspace_root(),
            },
        })
        .expect("vscode prehost allowlisted terminal exec should succeed");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["command"], "TerminalExec");
    assert_eq!(payload["details"]["command_name"], "pwd");
    assert_eq!(
        payload["details"]["working_directory"],
        current_workspace_root()
    );
    assert_eq!(
        payload["details"]["implementation_mode"],
        "allowlisted-terminal-prehost"
    );
    assert_eq!(payload["details"]["terminal_mode"], "allowlisted");
    assert_eq!(payload["details"]["stdout"], current_workspace_root());
    assert_eq!(payload["details"]["stderr"], "");
    assert_eq!(payload["details"]["exit_code"], 0);
}

#[test]
fn vscode_terminal_exec_rejects_commands_outside_allowlist() {
    let client = JsonRpcHostBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_VSCODE_PREHOST_TERMINAL_MODE", "allowlisted"),
        ("MAGI_VSCODE_PREHOST_ALLOWED_COMMANDS", "pwd"),
    ])));

    let error = client
        .call(HostBridgeRequest {
            host_kind: HostKind::Vscode,
            command: HostBridgeCommand::TerminalExec {
                command: "echo hello".to_string(),
                working_directory: current_workspace_root(),
            },
        })
        .expect_err("non-allowlisted command should be rejected");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
}

#[test]
fn unsupported_method_returns_protocol_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "host.not_supported".to_string(),
            params: json!({
                "host_kind": "Vscode",
                "command": { "WorkspaceRoots": {} }
            }),
        })
        .expect_err("unsupported method should return protocol error");

    assert!(matches!(error, BridgeTransportError::Protocol { .. }));
}

#[test]
fn broken_subprocess_returns_transport_error() {
    let transport =
        JsonRpcStdioTransport::new("sh").with_args(vec!["-c".to_string(), "exit 2".to_string()]);

    let error = transport
        .call(BridgeTransportRequest {
            method: "host.call".to_string(),
            params: json!({
                "host_kind": "Vscode",
                "command": {
                    "WorkspaceRoots": {}
                }
            }),
        })
        .expect_err("non-zero exit should be transport error");

    assert!(matches!(error, BridgeTransportError::Transport { .. }));
}
