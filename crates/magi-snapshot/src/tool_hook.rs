use std::path::PathBuf;

/// 工具调用前后回调上下文。
///
/// session_turn_writeback 在 dispatch tool batch 前后调用 SnapshotSession::tool_hook_*，
/// 让 ledger 强制对工具改写过的路径拍前后 hash。
#[derive(Clone, Debug)]
pub struct ToolHookCtx {
    pub tool_call_id: String,
    pub worker_id: Option<String>,
    pub execution_group_id: Option<String>,
    /// 如果工具自报了改写路径（例如 file_write），优先针对这些路径拍 hash；
    /// 为空时由 ChangeLog 全树对账兜底。
    pub declared_paths: Vec<PathBuf>,
}

/// 让外部模块（session_turn_writeback）持有的强引用 trait。
/// 实现由 `crate::session::SnapshotSession`。
pub trait ToolHook: Send + Sync {
    fn before_tool(&self, ctx: &ToolHookCtx);
    fn after_tool(&self, ctx: &ToolHookCtx);
}
