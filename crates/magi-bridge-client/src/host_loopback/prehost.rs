use super::{
    catalog::HostServiceShim,
    descriptors::{host_kind_label, success_response},
    host_command_name,
    static_scan::{collect_prehost_diagnostics, collect_prehost_symbols},
    terminal_policy::TerminalPolicy,
};
use crate::{BridgeResponse, HostBridgeCommand};
use serde_json::json;
use std::{fs, path::PathBuf, process::Command};

impl HostServiceShim {
    pub(super) fn execute(
        &self,
        command: HostBridgeCommand,
    ) -> Result<BridgeResponse, crate::local_process_protocol::LocalProcessBridgeRpcError> {
        if self.host_kind == crate::HostKind::Vscode {
            return self.execute_vscode_prehost(command);
        }
        self.execute_unimplemented_boundary(command)
    }

    fn execute_unimplemented_boundary(
        &self,
        command: HostBridgeCommand,
    ) -> Result<BridgeResponse, crate::local_process_protocol::LocalProcessBridgeRpcError> {
        let command_name = host_command_name(&command);
        let runtime_status = self.runtime_status();
        Err(
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32032,
                "idea host shell not implemented",
                Some(json!({
                    "command": command_name,
                    "host_kind": host_kind_label(self.host_kind),
                    "implementation_source": self.implementation_source,
                    "capability_profile": self.capability_profile,
                    "service_health": runtime_status.service_health,
                    "service_health_reason": runtime_status.service_health_reason,
                    "runtime_mode": runtime_status.runtime_mode,
                })),
            ),
        )
    }

    fn execute_vscode_prehost(
        &self,
        command: HostBridgeCommand,
    ) -> Result<BridgeResponse, crate::local_process_protocol::LocalProcessBridgeRpcError> {
        match command {
            HostBridgeCommand::WorkspaceRoots => {
                let runtime_status = self.ensure_vscode_prehost_ready("WorkspaceRoots")?;
                Ok(success_response(
                    self,
                    "WorkspaceRoots",
                    json!({
                        "workspace_roots": runtime_status.workspace_roots,
                        "workspace_roots_source": runtime_status.workspace_roots_source,
                        "implementation_mode": "filesystem-prehost",
                    }),
                ))
            }
            HostBridgeCommand::OpenFile {
                absolute_path,
                line,
                column,
            } => {
                self.ensure_vscode_prehost_ready("OpenFile")?;
                let canonical_path =
                    ensure_host_path_within_workspace_roots(self, &absolute_path, "absolute_path")?;
                let metadata = fs::metadata(&canonical_path).map_err(|error| {
                    crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                        -32024,
                        "open file failed",
                        Some(json!({
                            "absolute_path": absolute_path,
                            "reason": error.to_string(),
                        })),
                    )
                })?;
                Ok(success_response(
                    self,
                    "OpenFile",
                    json!({
                        "absolute_path": canonical_path.to_string_lossy().to_string(),
                        "line": line,
                        "column": column,
                        "file_type": if metadata.is_dir() { "directory" } else { "file" },
                        "byte_len": metadata.len(),
                        "implementation_mode": "filesystem-prehost",
                    }),
                ))
            }
            HostBridgeCommand::RevealDiff {
                left_path,
                right_path,
            } => {
                self.ensure_vscode_prehost_ready("RevealDiff")?;
                let left =
                    ensure_host_path_within_workspace_roots(self, &left_path, "left_path")?;
                let right =
                    ensure_host_path_within_workspace_roots(self, &right_path, "right_path")?;
                let left_content = read_text_file(&left)?;
                let right_content = read_text_file(&right)?;
                let same_content = left_content == right_content;
                Ok(success_response(
                    self,
                    "RevealDiff",
                    json!({
                        "left_path": left.to_string_lossy().to_string(),
                        "right_path": right.to_string_lossy().to_string(),
                        "same_content": same_content,
                        "left_line_count": line_count(&left_content),
                        "right_line_count": line_count(&right_content),
                        "byte_delta": (left_content.len() as i64) - (right_content.len() as i64),
                        "diff_summary": if same_content { "files are identical" } else { "files differ" },
                        "implementation_mode": "filesystem-prehost",
                    }),
                ))
            }
            HostBridgeCommand::ReadDiagnostics { absolute_path } => {
                self.ensure_vscode_prehost_ready("ReadDiagnostics")?;
                let canonical_path =
                    ensure_host_path_within_workspace_roots(self, &absolute_path, "absolute_path")?;
                let content = read_text_file(&canonical_path)?;
                Ok(success_response(
                    self,
                    "ReadDiagnostics",
                    json!({
                        "absolute_path": canonical_path.to_string_lossy().to_string(),
                        "analysis_mode": "prehost-static-scan",
                        "diagnostics": collect_prehost_diagnostics(&content),
                        "implementation_mode": "filesystem-prehost",
                    }),
                ))
            }
            HostBridgeCommand::ReadSymbols { absolute_path } => {
                self.ensure_vscode_prehost_ready("ReadSymbols")?;
                let canonical_path =
                    ensure_host_path_within_workspace_roots(self, &absolute_path, "absolute_path")?;
                let content = read_text_file(&canonical_path)?;
                Ok(success_response(
                    self,
                    "ReadSymbols",
                    json!({
                        "absolute_path": canonical_path.to_string_lossy().to_string(),
                        "analysis_mode": "prehost-symbol-scan",
                        "symbols": collect_prehost_symbols(&content),
                        "implementation_mode": "filesystem-prehost",
                    }),
                ))
            }
            HostBridgeCommand::TerminalExec {
                command,
                working_directory,
            } => {
                self.ensure_vscode_prehost_ready("TerminalExec")?;
                execute_vscode_terminal_exec(self, &command, &working_directory)
            }
        }
    }
}

fn execute_vscode_terminal_exec(
    shim: &HostServiceShim,
    command: &str,
    working_directory: &str,
) -> Result<BridgeResponse, crate::local_process_protocol::LocalProcessBridgeRpcError> {
    let policy = TerminalPolicy::from_env();
    if !policy.is_enabled() {
        return Err(
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32025,
                "terminal exec unavailable in vscode prehost",
                Some(json!({
                    "command": command,
                    "working_directory": working_directory,
                    "implementation_source": shim.implementation_source,
                    "terminal_mode": policy.mode,
                    "policy_source": policy.source,
                })),
            ),
        );
    }

    let requested_directory =
        ensure_host_path_within_workspace_roots(shim, working_directory, "working_directory")?;
    let canonical_roots = canonical_workspace_roots(shim)?;

    let command_name = command
        .split_whitespace()
        .next()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32027,
                "terminal exec command is empty",
                Some(json!({
                    "command": command,
                })),
            )
        })?;

    if !policy.is_command_allowed(command_name) {
        return Err(
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32028,
                "terminal exec command is not allowlisted",
                Some(json!({
                    "command": command,
                    "command_name": command_name,
                    "allowed_commands": policy.allowed_commands,
                    "policy_source": policy.source,
                })),
            ),
        );
    }

    let args: Vec<&str> = command.split_whitespace().skip(1).collect();
    if let Err(violation) = policy.validate_arguments(command_name, &args) {
        return Err(
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32033,
                "terminal exec argument policy violation",
                Some(json!({
                    "command": command,
                    "command_name": command_name,
                    "violation": violation,
                    "policy_source": policy.source,
                })),
            ),
        );
    }

    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(&requested_directory)
        .output()
        .map_err(|error| {
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32029,
                "terminal exec failed",
                Some(json!({
                    "command": command,
                    "working_directory": requested_directory.to_string_lossy().to_string(),
                    "reason": error.to_string(),
                })),
            )
        })?;

    Ok(BridgeResponse {
        ok: output.status.success(),
        payload: super::descriptors::shadow_host_payload(
            shim,
            "TerminalExec",
            json!({
                "command": command,
                "command_name": command_name,
                "working_directory": requested_directory.to_string_lossy().to_string(),
                "workspace_roots": canonical_roots
                    .iter()
                    .map(|root| root.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
                "allowed_commands": policy.allowed_commands,
                "stdout": String::from_utf8_lossy(&output.stdout).trim().to_string(),
                "stderr": String::from_utf8_lossy(&output.stderr).trim().to_string(),
                "exit_code": output.status.code(),
                "implementation_mode": "allowlisted-terminal-prehost",
                "terminal_mode": policy.mode,
                "policy_source": policy.source,
            }),
        ),
    })
}

fn canonical_workspace_roots(
    shim: &HostServiceShim,
) -> Result<Vec<PathBuf>, crate::local_process_protocol::LocalProcessBridgeRpcError> {
    shim.workspace_roots()
        .iter()
        .map(|root| canonicalize_host_path(root))
        .collect()
}

fn ensure_host_path_within_workspace_roots(
    shim: &HostServiceShim,
    path: &str,
    path_field: &str,
) -> Result<PathBuf, crate::local_process_protocol::LocalProcessBridgeRpcError> {
    let canonical_path = canonicalize_host_path(path)?;
    let canonical_roots = canonical_workspace_roots(shim)?;
    if canonical_roots.iter().any(|root| canonical_path.starts_with(root)) {
        return Ok(canonical_path);
    }
    Err(
        crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
            -32031,
            "host path is outside workspace roots",
            Some(json!({
                path_field: canonical_path.to_string_lossy().to_string(),
                "workspace_roots": canonical_roots
                    .iter()
                    .map(|root| root.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            })),
        ),
    )
}

fn canonicalize_host_path(
    path: &str,
) -> Result<PathBuf, crate::local_process_protocol::LocalProcessBridgeRpcError> {
    let candidate = PathBuf::from(path);
    if !candidate.is_absolute() {
        return Err(
            crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
                -32021,
                "host path must be absolute",
                Some(json!({
                    "absolute_path": path,
                })),
            ),
        );
    }
    candidate.canonicalize().map_err(|error| {
        crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
            -32022,
            "host path not found",
            Some(json!({
                "absolute_path": path,
                "reason": error.to_string(),
            })),
        )
    })
}

fn read_text_file(
    path: &PathBuf,
) -> Result<String, crate::local_process_protocol::LocalProcessBridgeRpcError> {
    fs::read_to_string(path).map_err(|error| {
        crate::local_process_protocol::LocalProcessBridgeRpcError::remote_business(
            -32023,
            "host prehost cannot read file",
            Some(json!({
                "absolute_path": path.to_string_lossy().to_string(),
                "reason": error.to_string(),
            })),
        )
    })
}

fn line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idea_host_returns_unimplemented_boundary_error() {
        let idea_shim = HostServiceShim {
            host_kind: crate::HostKind::Idea,
            implementation_source: "boundary-placeholder",
            capability_profile: "idea-host-shell-boundary-v1",
        };
        let result = idea_shim.execute(HostBridgeCommand::WorkspaceRoots);
        assert!(result.is_err(), "IDEA host should return error");
    }
}
