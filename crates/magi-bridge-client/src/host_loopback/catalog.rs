use super::{VSCODE_PREHOST_WORKSPACE_ROOTS_ENV, descriptors::host_kind_label};
use crate::{
    HostKind,
    local_process_protocol::{
        BridgeServerCommandCapabilityProfile, BridgeServerContextResolutionBoundary,
        BridgeServerKind, BridgeServerServiceCatalog, BridgeServerServiceDescriptor,
        BridgeServerSessionDescriptor, BridgeServerShellManifest, BridgeServerShellProfile,
        BridgeServerWorkspaceContext, LOCAL_BRIDGE_PROTOCOL_VERSION, LocalProcessBridgeRpcError,
    },
};
use serde_json::json;
use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub(super) struct HostServiceShim {
    pub(super) host_kind: HostKind,
    pub(super) implementation_source: &'static str,
    pub(super) capability_profile: &'static str,
}

#[derive(Clone, Debug)]
pub(super) struct HostRuntimeStatus {
    pub(super) workspace_roots: Vec<String>,
    pub(super) workspace_roots_source: String,
    pub(super) service_health: String,
    pub(super) service_health_reason: Option<String>,
    pub(super) terminal_exec_mode: String,
    pub(super) runtime_mode: String,
}

impl HostServiceShim {
    fn shell_family(&self) -> &'static str {
        match self.host_kind {
            HostKind::Vscode => "vscode-extension-host",
            HostKind::Idea => "jetbrains-plugin-host",
        }
    }

    pub(super) fn runtime_status(&self) -> HostRuntimeStatus {
        match self.host_kind {
            HostKind::Vscode => {
                let terminal_exec_mode = env::var("MAGI_VSCODE_PREHOST_TERMINAL_MODE")
                    .unwrap_or_else(|_| "disabled".to_string())
                    .trim()
                    .to_ascii_lowercase();
                let configured_roots =
                    env::var(VSCODE_PREHOST_WORKSPACE_ROOTS_ENV)
                        .ok()
                        .map(|raw| {
                            raw.split(',')
                                .map(str::trim)
                                .filter(|entry| !entry.is_empty())
                                .filter_map(canonicalize_workspace_root)
                                .map(|path| path.to_string_lossy().to_string())
                                .collect::<Vec<_>>()
                        });
                let (workspace_roots, workspace_roots_source) =
                    if let Some(roots) = configured_roots {
                        (roots, format!("env:{VSCODE_PREHOST_WORKSPACE_ROOTS_ENV}"))
                    } else {
                        let roots = env::current_dir()
                            .ok()
                            .map(|path| path.canonicalize().unwrap_or(path))
                            .map(|path| vec![path.to_string_lossy().to_string()])
                            .unwrap_or_default();
                        (roots, "filesystem:current_dir".to_string())
                    };
                let (service_health, service_health_reason) = if workspace_roots.is_empty() {
                    (
                        "unavailable".to_string(),
                        Some("no valid workspace roots resolved".to_string()),
                    )
                } else {
                    ("healthy".to_string(), None)
                };

                HostRuntimeStatus {
                    workspace_roots,
                    workspace_roots_source,
                    service_health,
                    service_health_reason,
                    terminal_exec_mode,
                    runtime_mode: "real-prehost".to_string(),
                }
            }
            HostKind::Idea => HostRuntimeStatus {
                workspace_roots: Vec::new(),
                workspace_roots_source: "unimplemented:idea-host-shell".to_string(),
                service_health: "unavailable".to_string(),
                service_health_reason: Some("idea host shell not implemented".to_string()),
                terminal_exec_mode: "unavailable".to_string(),
                runtime_mode: "boundary-only".to_string(),
            },
        }
    }

    pub(super) fn ensure_vscode_prehost_ready(
        &self,
        command_name: &str,
    ) -> Result<HostRuntimeStatus, LocalProcessBridgeRpcError> {
        let runtime_status = self.runtime_status();
        if runtime_status.service_health != "healthy" {
            return Err(LocalProcessBridgeRpcError::remote_business(
                -32030,
                "vscode prehost workspace roots unavailable",
                Some(json!({
                    "command": command_name,
                    "service_health": runtime_status.service_health,
                    "service_health_reason": runtime_status.service_health_reason,
                    "workspace_roots_source": runtime_status.workspace_roots_source,
                })),
            ));
        }
        Ok(runtime_status)
    }

    fn service_descriptor(&self) -> BridgeServerServiceDescriptor {
        let runtime_status = self.runtime_status();
        BridgeServerServiceDescriptor {
            service_name: self.shell_id(),
            shim_kind: "host-service-shim".to_string(),
            supported_operations: vec![
                "OpenFile".to_string(),
                "RevealDiff".to_string(),
                "ReadDiagnostics".to_string(),
                "ReadSymbols".to_string(),
                "workspace_roots".to_string(),
                "terminal_exec".to_string(),
            ],
            capabilities: vec![
                format!("host_shell_id:{}", self.shell_id()),
                format!("host_kind:{}", host_kind_label(self.host_kind)),
                format!("implementation_source:{}", self.implementation_source),
                format!("capability_profile:{}", self.capability_profile),
                format!(
                    "workspace_roots_source:{}",
                    runtime_status.workspace_roots_source
                ),
                format!("service_health:{}", runtime_status.service_health),
                format!("runtime_mode:{}", runtime_status.runtime_mode),
                format!("terminal_exec_mode:{}", runtime_status.terminal_exec_mode),
                format!(
                    "workspace_root_count:{}",
                    runtime_status.workspace_roots.len()
                ),
                "capability_version:host-shell-v1".to_string(),
                "shell_profile:loopback-host-shell-profile-v1".to_string(),
                "context_resolution:loopback-host-context-resolution-v1".to_string(),
                "minimum_version:0.1.0".to_string(),
                "session_scope:session-scoped".to_string(),
                "workspace_scope:workspace-scoped".to_string(),
                "command:OpenFile".to_string(),
                "command:RevealDiff".to_string(),
                "command:ReadDiagnostics".to_string(),
                "command:ReadSymbols".to_string(),
                "workspace:roots".to_string(),
                "terminal:exec".to_string(),
            ],
            service_health: Some(runtime_status.service_health),
            service_health_reason: runtime_status.service_health_reason,
            implementation_source: Some(self.implementation_source.to_string()),
            capability_profile: Some(self.capability_profile.to_string()),
            workspace_roots_source: Some(runtime_status.workspace_roots_source),
            manager_version: None,
            registry_profile: None,
            registry_manifest: None,
            selection_strategy: None,
            default_server: None,
            default_server_health: None,
            default_server_selection_key: None,
            default_route_status: None,
            default_route_target: None,
            selection_targets: None,
            selection_key: None,
            server_manifest: None,
            shell_manifest: Some(self.shell_manifest()),
            shell_profile: Some(self.shell_profile()),
            command_capability_profiles: Some(self.command_capability_profiles()),
            session_descriptor: Some(self.session_descriptor()),
            workspace_context: Some(self.workspace_context()),
            context_resolution_boundary: Some(self.context_resolution_boundary("host.call")),
        }
    }

    pub(super) fn handles(&self, host_kind: HostKind) -> bool {
        self.host_kind == host_kind
    }

    pub(super) fn shell_id(&self) -> String {
        format!("loopback-host-{}", host_kind_label(self.host_kind))
    }

    pub(super) fn shell_manifest(&self) -> BridgeServerShellManifest {
        let runtime_status = self.runtime_status();
        BridgeServerShellManifest {
            shell_id: self.shell_id(),
            minimum_version: "0.1.0".to_string(),
            capability_version: "host-shell-v1".to_string(),
            implementation_source: self.implementation_source.to_string(),
            capability_profile: self.capability_profile.to_string(),
            workspace_roots_source: runtime_status.workspace_roots_source,
        }
    }

    pub(super) fn shell_profile(&self) -> BridgeServerShellProfile {
        BridgeServerShellProfile {
            profile_id: format!("{}-profile", self.shell_id()),
            shell_id: self.shell_id(),
            host_kind: host_kind_label(self.host_kind).to_string(),
            shell_family: self.shell_family().to_string(),
            minimum_version: "0.1.0".to_string(),
            capability_version: "host-shell-v1".to_string(),
        }
    }

    pub(super) fn session_descriptor(&self) -> BridgeServerSessionDescriptor {
        BridgeServerSessionDescriptor {
            session_id: format!("loopback-host-session-{}", host_kind_label(self.host_kind)),
            session_scope: "session-scoped".to_string(),
        }
    }

    pub(super) fn workspace_context(&self) -> BridgeServerWorkspaceContext {
        let runtime_status = self.runtime_status();
        BridgeServerWorkspaceContext {
            workspace_id: format!("test-workspace-{}", host_kind_label(self.host_kind)),
            workspace_scope: "workspace-scoped".to_string(),
            workspace_roots_source: runtime_status.workspace_roots_source,
        }
    }

    pub(super) fn command_capability_profiles(&self) -> Vec<BridgeServerCommandCapabilityProfile> {
        vec![
            self.command_capability_profile("OpenFile"),
            self.command_capability_profile("RevealDiff"),
            self.command_capability_profile("ReadDiagnostics"),
            self.command_capability_profile("ReadSymbols"),
            self.command_capability_profile("WorkspaceRoots"),
            self.command_capability_profile("TerminalExec"),
        ]
    }

    pub(super) fn command_capability_profile(
        &self,
        command_name: &str,
    ) -> BridgeServerCommandCapabilityProfile {
        let (interaction_mode, side_effect_level, path_argument_policy) = match command_name {
            "OpenFile" => ("editor_navigation", "ui_only", "single_absolute_path"),
            "RevealDiff" => ("editor_navigation", "ui_only", "paired_absolute_paths"),
            "ReadDiagnostics" => ("query", "read_only", "single_absolute_path"),
            "ReadSymbols" => ("query", "read_only", "single_absolute_path"),
            "WorkspaceRoots" => ("query", "read_only", "workspace_root_listing"),
            "TerminalExec" => (
                "terminal_exec",
                "workspace_write_possible",
                "working_directory",
            ),
            _ => ("query", "read_only", "opaque"),
        };

        BridgeServerCommandCapabilityProfile {
            command_name: command_name.to_string(),
            capability_id: format!(
                "{}::{}::capability",
                self.shell_id(),
                command_name.to_ascii_lowercase()
            ),
            interaction_mode: interaction_mode.to_string(),
            side_effect_level: side_effect_level.to_string(),
            requires_session_context: true,
            requires_workspace_context: true,
            path_argument_policy: path_argument_policy.to_string(),
        }
    }

    pub(super) fn context_resolution_boundary(
        &self,
        command_name: &str,
    ) -> BridgeServerContextResolutionBoundary {
        BridgeServerContextResolutionBoundary {
            request_binding: format!(
                "bridge.describe_services -> host.call({}:{})",
                host_kind_label(self.host_kind),
                command_name
            ),
            session_resolution_strategy: "host-kind-derived session mapping".to_string(),
            workspace_resolution_strategy: "host-kind-derived workspace mapping".to_string(),
            session_resolution_source: "host_kind + loopback session shim".to_string(),
            workspace_resolution_source: "host_kind + loopback workspace shim".to_string(),
        }
    }

    pub(super) fn workspace_roots(&self) -> Vec<String> {
        self.runtime_status().workspace_roots
    }
}

pub(super) fn host_service_shims() -> Vec<HostServiceShim> {
    vec![
        HostServiceShim {
            host_kind: HostKind::Vscode,
            implementation_source: "real-prehost",
            capability_profile: "vscode-host-shell-prehost-v1",
        },
        HostServiceShim {
            host_kind: HostKind::Idea,
            implementation_source: "boundary-placeholder",
            capability_profile: "idea-host-shell-boundary-v1",
        },
    ]
}

pub(super) fn host_service_catalog(shims: &[HostServiceShim]) -> BridgeServerServiceCatalog {
    BridgeServerServiceCatalog {
        protocol_version: LOCAL_BRIDGE_PROTOCOL_VERSION.to_string(),
        server_kind: BridgeServerKind::Host,
        services: shims
            .iter()
            .map(HostServiceShim::service_descriptor)
            .collect(),
    }
}

fn canonicalize_workspace_root(path: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(path);
    if !candidate.is_absolute() {
        return None;
    }
    candidate.canonicalize().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_service_catalog_lists_both_loopback_host_shims() {
        let catalog = host_service_catalog(&host_service_shims());
        assert_eq!(catalog.server_kind, BridgeServerKind::Host);
        assert_eq!(catalog.services.len(), 2);
        let vscode = catalog
            .services
            .iter()
            .find(|service| service.service_name == "loopback-host-vscode")
            .expect("vscode host service should exist");
        assert_eq!(
            vscode
                .implementation_source
                .as_deref()
                .expect("vscode implementation source should exist"),
            "real-prehost"
        );
        assert_eq!(
            vscode
                .capability_profile
                .as_deref()
                .expect("vscode capability profile should exist"),
            "vscode-host-shell-prehost-v1"
        );
        let idea = catalog
            .services
            .iter()
            .find(|service| service.service_name == "loopback-host-idea")
            .expect("idea host service should exist");
        assert_eq!(
            idea.implementation_source
                .as_deref()
                .expect("idea implementation source should exist"),
            "boundary-placeholder"
        );
        assert_eq!(
            idea.capability_profile
                .as_deref()
                .expect("idea capability profile should exist"),
            "idea-host-shell-boundary-v1"
        );
        assert_eq!(idea.service_health.as_deref(), Some("unavailable"));
        assert_eq!(
            idea.service_health_reason.as_deref(),
            Some("idea host shell not implemented")
        );
        for service in &catalog.services {
            assert!(
                service
                    .capabilities
                    .contains(&"command:OpenFile".to_string())
            );
            assert!(
                service
                    .capabilities
                    .contains(&"command:RevealDiff".to_string())
            );
            assert!(
                service
                    .capabilities
                    .contains(&"command:ReadDiagnostics".to_string())
            );
            assert!(
                service
                    .capabilities
                    .contains(&"command:ReadSymbols".to_string())
            );
        }
    }

    #[test]
    fn vscode_prehost_workspace_context_and_session_descriptor_are_populated() {
        let vscode_shim = HostServiceShim {
            host_kind: HostKind::Vscode,
            implementation_source: "real-prehost",
            capability_profile: "vscode-host-shell-prehost-v1",
        };

        let session = vscode_shim.session_descriptor();
        assert!(session.session_id.contains("vscode"));
        assert_eq!(session.session_scope, "session-scoped");

        let workspace = vscode_shim.workspace_context();
        assert!(workspace.workspace_id.contains("vscode"));
        assert_eq!(workspace.workspace_scope, "workspace-scoped");
        assert!(!workspace.workspace_roots_source.is_empty());
    }

    #[test]
    fn command_capability_profiles_cover_all_commands() {
        let shim = HostServiceShim {
            host_kind: HostKind::Vscode,
            implementation_source: "real-prehost",
            capability_profile: "vscode-host-shell-prehost-v1",
        };
        let profiles = shim.command_capability_profiles();
        assert_eq!(profiles.len(), 6);

        let command_names: Vec<&str> = profiles.iter().map(|p| p.command_name.as_str()).collect();
        assert!(command_names.contains(&"OpenFile"));
        assert!(command_names.contains(&"RevealDiff"));
        assert!(command_names.contains(&"ReadDiagnostics"));
        assert!(command_names.contains(&"ReadSymbols"));
        assert!(command_names.contains(&"WorkspaceRoots"));
        assert!(command_names.contains(&"TerminalExec"));

        let terminal = profiles
            .iter()
            .find(|p| p.command_name == "TerminalExec")
            .expect("should find TerminalExec profile");
        assert_eq!(terminal.interaction_mode, "terminal_exec");
        assert_eq!(terminal.side_effect_level, "workspace_write_possible");

        let read_diag = profiles
            .iter()
            .find(|p| p.command_name == "ReadDiagnostics")
            .expect("should find ReadDiagnostics profile");
        assert_eq!(read_diag.interaction_mode, "query");
        assert_eq!(read_diag.side_effect_level, "read_only");
    }
}
