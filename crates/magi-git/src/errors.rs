use super::*;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GitError {
    #[error("路径不在 Git worktree 中: {path}")]
    NotRepository { path: PathBuf },
    #[error("Git 输入无效: {message}")]
    InvalidInput { message: String },
    #[error("工作区存在未提交改动，已拒绝 {operation}")]
    DirtyWorkspace {
        operation: String,
        dirty: GitDirtySummary,
    },
    #[error("Git 上下文已变化，调用方必须刷新后重试")]
    StaleContext {
        expected: Box<GitPrecondition>,
        actual_branch: Option<String>,
        actual_head: Option<String>,
        actual_worktree_path: PathBuf,
    },
    #[error("不能删除当前分支 {branch}")]
    CurrentBranch { branch: String },
    #[error("分支 {branch} 正被其他 worktree 使用")]
    BranchInUse {
        branch: String,
        worktree_paths: Vec<PathBuf>,
    },
    #[error("高风险操作需要显式二次确认: {operation}")]
    ConfirmationRequired { operation: String },
    #[error("代理改动无法干净地应用到目标工作树")]
    ApplyConflict {
        conflicted_paths: Vec<String>,
        stderr: String,
    },
    #[error("合并产生冲突")]
    MergeConflict {
        target: String,
        conflicted_paths: Vec<String>,
        stdout: String,
        stderr: String,
    },
    #[error("Git 操作失败: {operation}: {stderr}")]
    CommandFailed {
        operation: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("无法执行 Git: {0}")]
    Io(String),
}

impl GitError {
    /// 稳定的错误码。REST 的 `error.kind` 与结构化 Git 工具的 `error_code` 共用这一份映射。
    ///
    /// Git 命令自身失败时，按 stderr 里确定无疑的特征区分网络、认证、非快进推送和远端不存在，
    /// 其余保持 `git_command_failed`（调用方据此判断是否值得用 force 重试）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRepository { .. } => "git_not_repository",
            Self::InvalidInput { .. } => "git_invalid_input",
            Self::DirtyWorkspace { .. } => "git_dirty_workspace",
            Self::StaleContext { .. } => "git_stale_context",
            Self::CurrentBranch { .. } => "git_current_branch",
            Self::BranchInUse { .. } => "git_branch_in_use",
            Self::ConfirmationRequired { .. } => "git_confirmation_required",
            Self::MergeConflict { .. } => "git_merge_conflict",
            Self::ApplyConflict { .. } => "git_apply_conflict",
            Self::CommandFailed { stderr, .. } => classify_command_failure(stderr),
            Self::Io(_) => "git_io_error",
        }
    }

    /// 命令确实执行并失败（或没能执行）：与「前置条件不满足、被规则拒绝」不同，
    /// 它是执行失败，不是拒绝。
    pub fn is_execution_failure(&self) -> bool {
        matches!(self, Self::CommandFailed { .. } | Self::Io(_))
    }
}

pub(super) fn classify_command_failure(stderr: &str) -> &'static str {
    let text = stderr.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if contains_any(&[
        "could not resolve host",
        "connection timed out",
        "connection refused",
        "network is unreachable",
        "failed to connect to",
        "operation timed out",
        "unable to look up",
    ]) {
        "git_network_unreachable"
    } else if contains_any(&[
        "authentication failed",
        "permission denied (publickey)",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "invalid username or password",
    ]) {
        "git_authentication_failed"
    } else if contains_any(&[
        "non-fast-forward",
        "updates were rejected because the tip",
        "failed to push some refs",
    ]) {
        "git_push_rejected"
    } else if contains_any(&[
        "repository not found",
        "does not appear to be a git repository",
        "could not read from remote repository",
    ]) {
        "git_remote_unavailable"
    } else {
        "git_command_failed"
    }
}
