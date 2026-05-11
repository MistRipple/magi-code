use serde::{Deserialize, Serialize};

pub const TEXT_BLOB_LIMIT: u64 = 5 * 1024 * 1024;
pub const BINARY_BLOB_LIMIT: u64 = 50 * 1024 * 1024;
pub const LARGE_TEXT_SUMMARY_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    LargeText,
    Binary,
    Symlink,
    Special,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Tool,
    Watcher,
    External,
    Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymlinkInfo {
    pub target: String,
}

/// 单个文件在 baseline 与 ChangeLog 中的元信息。
///
/// `blob_hash` 为 `None` 表示该文件未被 blob 化（常见情形：
/// 超大二进制 / 特殊文件 / 读失败）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub path: String,
    pub content_kind: ContentKind,
    pub size: u64,
    pub mime: Option<String>,
    pub blob_hash: Option<String>,
    pub mtime_ms: Option<u64>,
    pub symlink: Option<SymlinkInfo>,
    pub error: Option<String>,
}

/// `events.log` 中追加的事件。`before`/`after` 都是事件发生时的快照视图。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEvent {
    pub event_id: String,
    pub timestamp_ms: u64,
    pub change_kind: ChangeKind,
    pub source: SourceKind,
    pub tool_call_id: Option<String>,
    pub worker_id: Option<String>,
    pub execution_group_id: Option<String>,
    pub before: Option<FileMeta>,
    pub after: Option<FileMeta>,
}

/// 投影到前端的 pending change 视图，由 `ChangeLog` + `BaselineIndex` 计算得到。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChange {
    pub path: String,
    pub change_kind: ChangeKind,
    /// `Renamed` 时指向 rename 之前的 baseline 路径。其余情况为 None。
    pub old_path: Option<String>,
    pub source: SourceKind,
    pub tool_call_id: Option<String>,
    pub worker_id: Option<String>,
    pub execution_group_id: Option<String>,
    pub content_kind: ContentKind,
    pub size: u64,
    pub mime: Option<String>,
    pub error: Option<String>,
    pub symlink_target: Option<String>,
    /// 仅当 content_kind == Text 时才包含完整原文（来自 baseline blob）。
    pub original_content: Option<String>,
    /// 仅当 content_kind == Text 时才包含完整新文（来自 disk）。
    pub preview_content: Option<String>,
    /// content_kind == LargeText 时给出头/尾摘要，便于前端展示。
    pub head_summary: Option<String>,
    pub tail_summary: Option<String>,
    pub unified_diff: Option<String>,
    pub timestamp_ms: u64,
}
