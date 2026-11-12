use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum SessionContextKey {
    Repository { session_id: String, root: PathBuf },
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionContextError {
    #[error("session {session_id} 尚未绑定代码上下文")]
    Missing { session_id: String },
    #[error("session Git context revision 已变化: expected={expected}, actual={actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("task {task_id} 尚未绑定 agent worktree")]
    MissingAgentWorktree { task_id: String },
}
