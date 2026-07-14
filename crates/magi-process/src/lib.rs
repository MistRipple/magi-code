use std::{ffi::OsStr, process::Command};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 创建继承 Magi 平台进程策略的同步子进程命令。
pub fn std_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    apply_platform_policy(&mut command);
    command
}

/// 创建继承 Magi 平台进程策略的异步子进程命令。
pub fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    apply_platform_policy(command.as_std_mut());
    command
}

fn apply_platform_policy(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{std_command, tokio_command};

    #[test]
    fn factories_preserve_the_requested_program() {
        assert_eq!(std_command("magi-test").get_program(), "magi-test");
        assert_eq!(
            tokio_command("magi-async-test").as_std().get_program(),
            "magi-async-test"
        );
    }

    #[cfg(windows)]
    #[test]
    fn std_factory_runs_windows_console_commands() {
        let status = std_command("cmd")
            .args(["/C", "exit", "0"])
            .status()
            .expect("cmd should start through the shared process factory");
        assert!(status.success());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn tokio_factory_runs_windows_console_commands() {
        let status = tokio_command("cmd")
            .args(["/C", "exit", "0"])
            .status()
            .await
            .expect("cmd should start through the shared async process factory");
        assert!(status.success());
    }

    #[test]
    fn production_code_uses_the_shared_process_factory() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("magi-process should live under <workspace>/crates");
        let mut violations = Vec::new();

        for source_root in [workspace_root.join("apps"), workspace_root.join("crates")] {
            collect_direct_command_constructors(&source_root, &mut violations);
        }

        assert!(
            violations.is_empty(),
            "生产代码必须通过 magi-process 创建子进程，仍存在直接构造：\n{}",
            violations.join("\n")
        );
    }

    fn collect_direct_command_constructors(root: &Path, violations: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.ends_with("magi-process") || path.ends_with("tests") {
                    continue;
                }
                collect_direct_command_constructors(&path, violations);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("rs")
                || path.file_name().and_then(|value| value.to_str()) == Some("tests.rs")
            {
                continue;
            }
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            for (line_index, line) in source.lines().enumerate() {
                if [
                    "Command::new(",
                    "StdCommand::new(",
                    "std::process::Command::new(",
                    "tokio::process::Command::new(",
                ]
                .iter()
                .any(|pattern| line.contains(pattern))
                {
                    violations.push(format!(
                        "{}:{}: {}",
                        path.strip_prefix(workspace_root(root))
                            .unwrap_or(&path)
                            .display(),
                        line_index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    fn workspace_root(path: &Path) -> &Path {
        path.ancestors()
            .find(|candidate| {
                candidate.join("Cargo.toml").is_file() && candidate.join("crates").is_dir()
            })
            .unwrap_or(path)
    }
}
