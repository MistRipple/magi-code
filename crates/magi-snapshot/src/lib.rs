//! magi-snapshot — workspace 文件变更账本
//!
//! 单一真相源：BlobStore + BaselineIndex + ChangeLog。
//! 不读 git，也不写 git。Edits 面板的所有数据由本 crate 提供。

pub mod baseline_index;
pub mod blob_store;
pub mod change_log;
pub mod error;
pub mod manager;
pub mod scan;
pub mod session;
pub mod tool_hook;
pub mod types;
pub mod watcher;

pub use error::{SnapshotError, SnapshotResult};
pub use manager::SnapshotManager;
pub use session::SnapshotSession;
pub use tool_hook::{ToolHook, ToolHookCtx};
pub use types::{
    ChangeEvent, ChangeKind, ContentKind, FileMeta, PendingChange, SourceKind, SymlinkInfo,
    SymlinkTargetKind,
};
