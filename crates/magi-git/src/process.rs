use super::*;

pub(super) async fn run_git(path: &Path, args: &[&str]) -> Result<CommandOutput, GitError> {
    let output = tokio_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| GitError::Io(error.to_string()))?;
    Ok(CommandOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    })
}

pub(super) static SNAPSHOT_INDEX_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// 运行时提交使用固定身份，不依赖用户是否配置 user.name/user.email。
pub(super) fn runtime_commit_identity() -> [(&'static str, &'static std::ffi::OsStr); 4] {
    let name = std::ffi::OsStr::new("Magi Agent");
    let email = std::ffi::OsStr::new("agent@magi.local");
    [
        ("GIT_AUTHOR_NAME", name),
        ("GIT_AUTHOR_EMAIL", email),
        ("GIT_COMMITTER_NAME", name),
        ("GIT_COMMITTER_EMAIL", email),
    ]
}

pub(super) async fn run_git_with_env(
    path: &Path,
    args: &[&str],
    envs: &[(&str, &std::ffi::OsStr)],
) -> Result<CommandOutput, GitError> {
    let mut command = tokio_command("git");
    command.arg("-C").arg(path).args(args).stdin(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .await
        .map_err(|error| GitError::Io(error.to_string()))?;
    Ok(CommandOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    })
}

/// 从 `git apply` 的错误输出中提取出错路径。
pub(super) fn apply_error_paths(stderr: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stderr.lines() {
        let path = line
            .strip_prefix("error: patch failed: ")
            .and_then(|rest| rest.rsplit_once(':').map(|(path, _)| path))
            .or_else(|| {
                line.strip_prefix("error: ")
                    .and_then(|rest| rest.split_once(": ").map(|(path, _)| path))
            });
        if let Some(path) = path.map(str::trim).filter(|path| !path.is_empty())
            && !paths.iter().any(|existing| existing == path)
        {
            paths.push(path.to_string());
        }
    }
    paths
}

pub(super) async fn run_git_bytes(
    path: &Path,
    args: &[&str],
) -> Result<BinaryCommandOutput, GitError> {
    let output = tokio_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| GitError::Io(error.to_string()))?;
    Ok(BinaryCommandOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    })
}

pub(super) async fn read_git_object(
    worktree_path: &Path,
    object_id: &str,
    entry_path: &str,
) -> Result<Vec<u8>, GitError> {
    let output = run_git_bytes(worktree_path, &["cat-file", "blob", object_id]).await?;
    if output.success {
        Ok(output.stdout)
    } else {
        Err(GitError::CommandFailed {
            operation: format!("git_tree_blob:{entry_path}"),
            exit_code: output.code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: output.stderr,
        })
    }
}

pub(super) fn ensure_success(operation: &str, output: CommandOutput) -> Result<String, GitError> {
    if output.success {
        Ok(output.stdout)
    } else {
        Err(command_failed(operation, output))
    }
}

pub(super) fn command_failed(operation: &str, output: CommandOutput) -> GitError {
    GitError::CommandFailed {
        operation: operation.to_string(),
        exit_code: output.code,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

pub(super) fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub(super) fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn resolve_git_path(worktree_path: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        worktree_path.join(path)
    }
}

/// Compare paths by their canonical filesystem identity when available.
///
/// This also normalizes platform-specific spellings such as Windows extended
/// length paths, which can differ between `std::fs::canonicalize` and a host
/// path decoded from the public path reference.
pub fn same_path(left: &Path, right: &Path) -> bool {
    canonical_or_original(left) == canonical_or_original(right)
}

pub(super) fn is_conflicted_status(index_status: char, worktree_status: char) -> bool {
    matches!(
        (index_status, worktree_status),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}
