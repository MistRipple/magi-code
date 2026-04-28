use std::env;

#[derive(Debug, Clone)]
pub(super) struct TerminalPolicy {
    pub(super) mode: String,
    pub(super) source: String,
    pub(super) allowed_commands: Vec<String>,
    pub(super) timeout_ms: u64,
    denied_argument_patterns: Vec<String>,
}

const TERMINAL_DENIED_ARG_PATTERNS_DEFAULT: &[&str] =
    &["--force", "-rf", "--no-preserve-root", "sudo", "rm -rf /"];
const TERMINAL_EXEC_TIMEOUT_MS_DEFAULT: u64 = 10_000;
const TERMINAL_EXEC_TIMEOUT_MS_MIN: u64 = 100;
const TERMINAL_EXEC_TIMEOUT_MS_MAX: u64 = 120_000;

impl TerminalPolicy {
    pub(super) fn from_env() -> Self {
        let mode = env::var("MAGI_VSCODE_PREHOST_TERMINAL_MODE")
            .unwrap_or_else(|_| "disabled".to_string())
            .trim()
            .to_ascii_lowercase();
        let source = if env::var("MAGI_VSCODE_PREHOST_TERMINAL_MODE").is_ok() {
            "env:MAGI_VSCODE_PREHOST_TERMINAL_MODE"
        } else {
            "default:disabled"
        }
        .to_string();
        let allowed_commands = env::var("MAGI_VSCODE_PREHOST_ALLOWED_COMMANDS")
            .unwrap_or_else(|_| "pwd".to_string())
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect();
        let denied_argument_patterns = env::var("MAGI_VSCODE_PREHOST_DENIED_ARG_PATTERNS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| {
                TERMINAL_DENIED_ARG_PATTERNS_DEFAULT
                    .iter()
                    .map(|pattern| pattern.to_string())
                    .collect()
            });
        let timeout_ms = env::var("MAGI_VSCODE_PREHOST_TERMINAL_TIMEOUT_MS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .unwrap_or(TERMINAL_EXEC_TIMEOUT_MS_DEFAULT)
            .clamp(TERMINAL_EXEC_TIMEOUT_MS_MIN, TERMINAL_EXEC_TIMEOUT_MS_MAX);
        Self {
            mode,
            source,
            allowed_commands,
            timeout_ms,
            denied_argument_patterns,
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.mode == "allowlisted"
    }

    pub(super) fn is_command_allowed(&self, command_name: &str) -> bool {
        self.allowed_commands
            .iter()
            .any(|allowed| allowed == command_name)
    }

    pub(super) fn validate_arguments(
        &self,
        _command_name: &str,
        args: &[&str],
    ) -> Result<(), String> {
        for arg in args {
            for denied in &self.denied_argument_patterns {
                if *arg == denied.as_str() {
                    return Err(format!(
                        "argument '{}' matches denied pattern '{}'",
                        arg, denied
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_policy_defaults_to_disabled() {
        let policy = TerminalPolicy {
            mode: "disabled".to_string(),
            source: "default:disabled".to_string(),
            allowed_commands: vec!["pwd".to_string()],
            timeout_ms: TERMINAL_EXEC_TIMEOUT_MS_DEFAULT,
            denied_argument_patterns: TERMINAL_DENIED_ARG_PATTERNS_DEFAULT
                .iter()
                .map(|p| p.to_string())
                .collect(),
        };
        assert_eq!(policy.mode, "disabled");
        assert!(!policy.is_enabled());
        assert_eq!(policy.source, "default:disabled");
        assert_eq!(policy.allowed_commands, vec!["pwd"]);
        assert_eq!(policy.timeout_ms, TERMINAL_EXEC_TIMEOUT_MS_DEFAULT);
        assert!(!policy.denied_argument_patterns.is_empty());
    }

    #[test]
    fn terminal_policy_validates_denied_arguments() {
        let policy = TerminalPolicy {
            mode: "allowlisted".to_string(),
            source: "test".to_string(),
            allowed_commands: vec!["git".to_string(), "ls".to_string()],
            timeout_ms: TERMINAL_EXEC_TIMEOUT_MS_DEFAULT,
            denied_argument_patterns: vec![
                "--force".to_string(),
                "-rf".to_string(),
                "sudo".to_string(),
            ],
        };

        assert!(policy.validate_arguments("git", &["status"]).is_ok());
        assert!(
            policy
                .validate_arguments("git", &["log", "--oneline"])
                .is_ok()
        );
        assert!(
            policy
                .validate_arguments("git", &["push", "--force"])
                .is_err()
        );
        assert!(policy.validate_arguments("rm", &["-rf", "/"]).is_err());

        let err = policy
            .validate_arguments("git", &["push", "--force"])
            .expect_err("force push should be rejected");
        assert!(err.contains("--force"));
        assert!(err.contains("denied pattern"));
    }

    #[test]
    fn terminal_policy_command_allowlist() {
        let policy = TerminalPolicy {
            mode: "allowlisted".to_string(),
            source: "test".to_string(),
            allowed_commands: vec!["pwd".to_string(), "ls".to_string(), "git".to_string()],
            timeout_ms: TERMINAL_EXEC_TIMEOUT_MS_DEFAULT,
            denied_argument_patterns: vec![],
        };

        assert!(policy.is_command_allowed("pwd"));
        assert!(policy.is_command_allowed("ls"));
        assert!(policy.is_command_allowed("git"));
        assert!(!policy.is_command_allowed("rm"));
        assert!(!policy.is_command_allowed("curl"));
        assert!(!policy.is_command_allowed(""));
    }
}
