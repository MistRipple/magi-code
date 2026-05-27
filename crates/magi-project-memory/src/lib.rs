//! Task System v2 — L14 ProjectMemory：跨 session、跨 conversation 的项目记忆。
//!
//! 参考 claude-code 的 memory 系统：
//! - 4 类 memory：user / feedback / project / reference。
//! - 物理存储在 `~/.magi/projects/{slug}/memory/`。
//! - `MEMORY.md` 是索引（一行一条 pointer），多条 typed 文件存正文。
//! - 每次 Conversation 启动自动加载 `MEMORY.md` 索引并注入 system prompt。
//! - 提供 auto-save 接口（`save_entry` / `delete_entry`），调用方（v2 `memory_write` 工具）
//!   把读写权交给 LLM，自行决定何时持久化记忆。
//!
//! Slug 派生策略：与 claude-code 一致——取 workspace 绝对路径，把 `/` 替换为 `-`，
//! 例如 `/Users/foo/code/proj` → `-Users-foo-code-proj`。便于人手在 `~/.magi/projects/`
//! 下肉眼定位是哪个项目。

use magi_core::WorkspaceRootPath;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// MemoryKind / MemoryEntry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "user" => Some(Self::User),
            "feedback" => Some(Self::Feedback),
            "project" => Some(Self::Project),
            "reference" => Some(Self::Reference),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }
}

/// 一条 memory entry，对应 `~/.magi/projects/{slug}/memory/<file_name>` 一个 .md 文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// 文件名，不含扩展名。例如 `feedback_test_maintenance`。
    pub file_stem: String,
    /// 索引行展示名（frontmatter `name`）。
    pub name: String,
    /// 一句话描述（frontmatter `description`），用于索引行 hook。
    pub description: String,
    pub kind: MemoryKind,
    /// 正文 markdown。
    pub body: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ProjectMemoryError {
    #[error("无法解析 home 目录")]
    HomeDirUnavailable,

    #[error("无效的 file_stem `{0}`：只允许字母、数字、`-`、`_`")]
    InvalidFileStem(String),

    #[error("memory entry 字段不完整：{0}")]
    InvalidEntry(&'static str),

    #[error("memory 文件 IO 失败：path={path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// ---------------------------------------------------------------------------
// ProjectMemoryStore
// ---------------------------------------------------------------------------

/// 单一项目的 memory 仓库，绑定 `~/.magi/projects/{slug}/memory/` 物理目录。
/// 读写操作直落文件系统：单 process 内通过 `ProjectMemoryRegistry` 共享同一实例
/// （`RwLock` 包裹），跨 process 时假定调用方不会同时写同一份文件——目前 v2
/// 还没有 multi-process 写入需求。
pub struct ProjectMemoryStore {
    root: PathBuf,
    lock: RwLock<()>,
}

impl ProjectMemoryStore {
    /// 以 magi home（默认 `~/.magi`）下的 projects 目录初始化。会立即 mkdir -p
    /// 目标路径，避免下游每次写入都判断。
    pub fn open(workspace_root: &WorkspaceRootPath) -> Result<Self, ProjectMemoryError> {
        let magi_home = magi_home_dir()?;
        Self::open_with_home(&magi_home, workspace_root)
    }

    /// 测试入口：允许指定 magi home（替代 `~/.magi`）。
    pub fn open_with_home(
        magi_home: &Path,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Self, ProjectMemoryError> {
        let root = magi_core::paths::project_memory_root(magi_home, workspace_root);
        fs::create_dir_all(&root).map_err(|source| ProjectMemoryError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self {
            root,
            lock: RwLock::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 读取 `MEMORY.md` 内容；不存在时返回 `None`。
    pub fn load_index(&self) -> Result<Option<String>, ProjectMemoryError> {
        let _guard = self.lock.read().expect("ProjectMemoryStore lock poisoned");
        let path = self.root.join("MEMORY.md");
        match fs::read_to_string(&path) {
            Ok(content) => Ok(Some(content)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ProjectMemoryError::Io { path, source }),
        }
    }

    /// 读取一条 entry。文件不存在返回 `None`。
    pub fn load_entry(&self, file_stem: &str) -> Result<Option<MemoryEntry>, ProjectMemoryError> {
        validate_file_stem(file_stem)?;
        let _guard = self.lock.read().expect("ProjectMemoryStore lock poisoned");
        let path = self.entry_path(file_stem);
        let raw = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(ProjectMemoryError::Io { path, source }),
        };
        parse_entry(file_stem, &raw).map(Some)
    }

    /// 写入或覆盖一条 entry，并同步刷新 `MEMORY.md` 索引。
    pub fn save_entry(&self, entry: &MemoryEntry) -> Result<(), ProjectMemoryError> {
        validate_entry(entry)?;
        let _guard = self.lock.write().expect("ProjectMemoryStore lock poisoned");
        let path = self.entry_path(&entry.file_stem);
        let body = render_entry(entry);
        fs::write(&path, body).map_err(|source| ProjectMemoryError::Io {
            path: path.clone(),
            source,
        })?;
        self.rewrite_index_locked()?;
        Ok(())
    }

    /// 删除一条 entry，并同步刷新索引。entry 不存在时返回 `Ok(false)`。
    pub fn delete_entry(&self, file_stem: &str) -> Result<bool, ProjectMemoryError> {
        validate_file_stem(file_stem)?;
        let _guard = self.lock.write().expect("ProjectMemoryStore lock poisoned");
        let path = self.entry_path(file_stem);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(ProjectMemoryError::Io { path, source }),
        }
        self.rewrite_index_locked()?;
        Ok(true)
    }

    /// 枚举当前所有 entry（按 file_stem 排序，方便 deterministic 渲染）。
    pub fn list_entries(&self) -> Result<Vec<MemoryEntry>, ProjectMemoryError> {
        let _guard = self.lock.read().expect("ProjectMemoryStore lock poisoned");
        self.list_entries_locked()
    }

    /// 渲染用于 system prompt 的概要：
    /// - 没有任何 entry → 返回 `None`。
    /// - 否则返回带前言的索引视图（包含 entries 的 stem + name + description）。
    pub fn render_for_prompt(&self) -> Result<Option<String>, ProjectMemoryError> {
        self.render_for_prompt_with_write_hint(true)
    }

    pub fn render_for_prompt_read_only(&self) -> Result<Option<String>, ProjectMemoryError> {
        self.render_for_prompt_with_write_hint(false)
    }

    fn render_for_prompt_with_write_hint(
        &self,
        include_write_hint: bool,
    ) -> Result<Option<String>, ProjectMemoryError> {
        let entries = self.list_entries()?;
        if entries.is_empty() {
            return Ok(None);
        }
        let mut out = String::new();
        out.push_str("项目 ProjectMemory（位于 `~/.magi/projects/<slug>/memory/`，跨 session 持久化的项目级记忆）：\n");
        for entry in &entries {
            out.push_str(&format!(
                "- [{kind}] {stem}.md — {name}：{description}\n",
                kind = entry.kind.as_str(),
                stem = entry.file_stem,
                name = entry.name,
                description = entry.description,
            ));
        }
        if include_write_hint {
            out.push_str("\n如需新增 / 修改 / 删除项目记忆，请调用 `memory_write` 工具（kind = user|feedback|project|reference）。");
        }
        Ok(Some(out))
    }

    fn entry_path(&self, file_stem: &str) -> PathBuf {
        self.root.join(format!("{file_stem}.md"))
    }

    fn list_entries_locked(&self) -> Result<Vec<MemoryEntry>, ProjectMemoryError> {
        let read_dir = match fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ProjectMemoryError::Io {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        let mut out = Vec::new();
        for dirent in read_dir {
            let dirent = match dirent {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(error = %err, "ProjectMemoryStore: 跳过无法读取的 dirent");
                    continue;
                }
            };
            let path = dirent.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            if stem == "MEMORY" {
                continue;
            }
            if validate_file_stem(stem).is_err() {
                tracing::warn!(file = %path.display(), "ProjectMemoryStore: 非法文件名，跳过");
                continue;
            }
            let raw = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(file = %path.display(), error = %err, "ProjectMemoryStore: 读取失败，跳过");
                    continue;
                }
            };
            match parse_entry(stem, &raw) {
                Ok(entry) => out.push(entry),
                Err(err) => {
                    tracing::warn!(file = %path.display(), error = %err, "ProjectMemoryStore: 解析失败，跳过");
                }
            }
        }
        out.sort_by(|a, b| a.file_stem.cmp(&b.file_stem));
        Ok(out)
    }

    fn rewrite_index_locked(&self) -> Result<(), ProjectMemoryError> {
        let entries = self.list_entries_locked()?;
        let path = self.root.join("MEMORY.md");
        if entries.is_empty() {
            // 没有任何 entry 时移除索引，避免遗留空文件。
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(ProjectMemoryError::Io { path, source }),
            }
            return Ok(());
        }
        let mut out = String::new();
        for entry in entries {
            out.push_str(&format!(
                "- [{name}]({stem}.md) — {description}\n",
                name = entry.name,
                stem = entry.file_stem,
                description = entry.description,
            ));
        }
        fs::write(&path, out).map_err(|source| ProjectMemoryError::Io { path, source })
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// 同 process 内复用 store 实例的注册表。key = workspace_root 字符串。
pub struct ProjectMemoryRegistry {
    inner: RwLock<HashMap<String, Arc<ProjectMemoryStore>>>,
    magi_home: PathBuf,
}

impl ProjectMemoryRegistry {
    /// 解析 magi home（`~/.magi`）；解析失败时回退到 `$TMPDIR/magi-project-memory`
    /// 以保证 dispatcher 在 CI / 沙箱环境下仍可构造。生产环境总是命中 `~/.magi`。
    pub fn new() -> Self {
        let magi_home =
            magi_home_dir().unwrap_or_else(|_| std::env::temp_dir().join("magi-project-memory"));
        Self {
            inner: RwLock::new(HashMap::new()),
            magi_home,
        }
    }

    pub fn with_home(magi_home: PathBuf) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            magi_home,
        }
    }

    pub fn get_or_open(
        &self,
        workspace_root: &WorkspaceRootPath,
    ) -> Result<Arc<ProjectMemoryStore>, ProjectMemoryError> {
        if let Some(existing) = self
            .inner
            .read()
            .expect("ProjectMemoryRegistry lock poisoned")
            .get(workspace_root.as_str())
            .cloned()
        {
            return Ok(existing);
        }
        let store = ProjectMemoryStore::open_with_home(&self.magi_home, workspace_root)?;
        let arc = Arc::new(store);
        self.inner
            .write()
            .expect("ProjectMemoryRegistry lock poisoned")
            .insert(workspace_root.as_str().to_string(), Arc::clone(&arc));
        Ok(arc)
    }
}

// ---------------------------------------------------------------------------
// memory_write tool 参数解析
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum MemoryWriteError {
    #[error("memory_write 参数不是合法 JSON：{0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("memory_write 缺少 `action` 字段，必须为 `save` 或 `delete`")]
    MissingAction,

    #[error("memory_write `action` 仅支持 save / delete，收到 `{0}`")]
    UnknownAction(String),

    #[error("memory_write 缺少 `file_stem` 字段")]
    MissingFileStem,

    #[error("memory_write save 模式缺少 `{0}` 字段")]
    MissingField(&'static str),

    #[error("memory_write `kind` 未识别：`{0}`")]
    UnknownKind(String),
}

#[derive(Clone, Debug)]
pub enum MemoryWriteAction {
    Save(MemoryEntry),
    Delete { file_stem: String },
}

/// 解析 `memory_write` 工具的 JSON 参数。
///
/// 入参 schema（save）：
/// ```json
/// { "action": "save", "file_stem": "feedback_test",
///   "name": "标题", "description": "一句话",
///   "kind": "feedback", "body": "正文 markdown" }
/// ```
///
/// 入参 schema（delete）：
/// ```json
/// { "action": "delete", "file_stem": "feedback_test" }
/// ```
pub fn parse_memory_write_arguments(raw: &str) -> Result<MemoryWriteAction, MemoryWriteError> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let obj = value
        .as_object()
        .ok_or_else(|| MemoryWriteError::MissingAction)?;
    let action = obj
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or(MemoryWriteError::MissingAction)?
        .to_ascii_lowercase();
    let file_stem = obj
        .get("file_stem")
        .and_then(|v| v.as_str())
        .ok_or(MemoryWriteError::MissingFileStem)?
        .trim()
        .to_string();
    if file_stem.is_empty() {
        return Err(MemoryWriteError::MissingFileStem);
    }
    match action.as_str() {
        "delete" | "remove" => Ok(MemoryWriteAction::Delete { file_stem }),
        "save" | "write" | "update" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(MemoryWriteError::MissingField("name"))?
                .to_string();
            let description = obj
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(MemoryWriteError::MissingField("description"))?
                .to_string();
            let kind_raw = obj
                .get("kind")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(MemoryWriteError::MissingField("kind"))?;
            let kind = MemoryKind::parse(kind_raw)
                .ok_or_else(|| MemoryWriteError::UnknownKind(kind_raw.to_string()))?;
            let body = obj
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or(MemoryWriteError::MissingField("body"))?
                .trim_end()
                .to_string();
            if body.is_empty() {
                return Err(MemoryWriteError::MissingField("body"));
            }
            Ok(MemoryWriteAction::Save(MemoryEntry {
                file_stem,
                name,
                description,
                kind,
                body,
            }))
        }
        other => Err(MemoryWriteError::UnknownAction(other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn magi_home_dir() -> Result<PathBuf, ProjectMemoryError> {
    dirs::home_dir()
        .map(|home| home.join(".magi"))
        .ok_or(ProjectMemoryError::HomeDirUnavailable)
}

fn validate_file_stem(stem: &str) -> Result<(), ProjectMemoryError> {
    if stem.is_empty() {
        return Err(ProjectMemoryError::InvalidFileStem(stem.to_string()));
    }
    if stem == "MEMORY" {
        return Err(ProjectMemoryError::InvalidFileStem(stem.to_string()));
    }
    if !stem
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ProjectMemoryError::InvalidFileStem(stem.to_string()));
    }
    Ok(())
}

fn validate_entry(entry: &MemoryEntry) -> Result<(), ProjectMemoryError> {
    validate_file_stem(&entry.file_stem)?;
    if entry.name.trim().is_empty() {
        return Err(ProjectMemoryError::InvalidEntry("name 不能为空"));
    }
    if entry.description.trim().is_empty() {
        return Err(ProjectMemoryError::InvalidEntry("description 不能为空"));
    }
    if entry.body.trim().is_empty() {
        return Err(ProjectMemoryError::InvalidEntry("body 不能为空"));
    }
    Ok(())
}

fn render_entry(entry: &MemoryEntry) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", entry.name));
    out.push_str(&format!("description: {}\n", entry.description));
    out.push_str(&format!("type: {}\n", entry.kind.as_str()));
    out.push_str("---\n\n");
    out.push_str(entry.body.trim_end());
    out.push('\n');
    out
}

fn parse_entry(file_stem: &str, raw: &str) -> Result<MemoryEntry, ProjectMemoryError> {
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut kind: Option<MemoryKind> = None;
    let body;

    let trimmed = raw.trim_start_matches('\u{FEFF}'); // strip BOM
    if let Some(rest) = trimmed.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let frontmatter = &rest[..end];
            for line in frontmatter.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let k = k.trim();
                    let v = v.trim();
                    match k {
                        "name" => name = Some(v.to_string()),
                        "description" => description = Some(v.to_string()),
                        "type" => kind = MemoryKind::parse(v),
                        _ => {}
                    }
                }
            }
            let after = &rest[end + "\n---".len()..];
            body = after.trim_start_matches('\n').trim_end().to_string();
        } else {
            body = trimmed.trim_end().to_string();
        }
    } else {
        body = trimmed.trim_end().to_string();
    }

    Ok(MemoryEntry {
        file_stem: file_stem.to_string(),
        name: name.unwrap_or_else(|| file_stem.to_string()),
        description: description.unwrap_or_default(),
        kind: kind.unwrap_or(MemoryKind::Project),
        body,
    })
}

// ---------------------------------------------------------------------------
// Tool entry：`memory_write` 工具执行体
// ---------------------------------------------------------------------------

/// S10 工具下沉：把 `memory_write` 完整执行体收口在本 crate，conversation_loop
/// 不再持有该业务。`store: Option<...>` 仍由调用方按 workspace 绑定决定是否注入；
/// 未绑定 workspace 时直接失败，避免静默丢弃记忆请求。
pub fn execute_memory_write_tool(
    event_bus: &magi_event_bus::InMemoryEventBus,
    store: Option<&ProjectMemoryStore>,
    session_id: &magi_core::SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    task_id: &magi_core::TaskId,
    mission_id: &magi_core::MissionId,
    arguments: &str,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::{EventId, ExecutionResultStatus, UtcMillis};
    use magi_event_bus::{EventContext, EventEnvelope};
    let Some(store) = store else {
        return (
            serde_json::json!({
                "tool": "memory_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位项目记忆目录",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let parsed = match parse_memory_write_arguments(arguments) {
        Ok(action) => action,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "memory_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let (event_kind, payload, status) = match parsed {
        MemoryWriteAction::Save(entry) => {
            let file_stem = entry.file_stem.clone();
            match store.save_entry(&entry) {
                Ok(()) => (
                    "save",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "succeeded",
                        "action": "save",
                        "file_stem": file_stem,
                        "kind": entry.kind.as_str(),
                    }),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(err) => (
                    "save",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "failed",
                        "action": "save",
                        "file_stem": file_stem,
                        "error": err.to_string(),
                    }),
                    ExecutionResultStatus::Failed,
                ),
            }
        }
        MemoryWriteAction::Delete { file_stem } => match store.delete_entry(&file_stem) {
            Ok(true) => (
                "delete",
                serde_json::json!({
                    "tool": "memory_write",
                    "status": "succeeded",
                    "action": "delete",
                    "file_stem": file_stem,
                }),
                ExecutionResultStatus::Succeeded,
            ),
            Ok(false) => (
                "delete",
                serde_json::json!({
                    "tool": "memory_write",
                    "status": "succeeded",
                    "action": "delete",
                    "file_stem": file_stem,
                    "note": "entry 不存在，已视为幂等删除",
                }),
                ExecutionResultStatus::Succeeded,
            ),
            Err(err) => (
                "delete",
                serde_json::json!({
                    "tool": "memory_write",
                    "status": "failed",
                    "action": "delete",
                    "file_stem": file_stem,
                    "error": err.to_string(),
                }),
                ExecutionResultStatus::Failed,
            ),
        },
    };
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-project-memory-updated-{}",
                UtcMillis::now().0
            )),
            "task.project_memory.updated",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(ToString::to_string),
                "action": event_kind,
                "result": payload,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), status)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(home: &TempDir, ws: &str) -> ProjectMemoryStore {
        ProjectMemoryStore::open_with_home(home.path(), &WorkspaceRootPath::new(ws))
            .expect("open store")
    }

    #[test]
    fn save_entry_creates_files_and_updates_index() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        let entry = MemoryEntry {
            file_stem: "feedback_tests".into(),
            name: "重构期对待过期测试的策略".into(),
            description: "过期 fixture 直接清理".into(),
            kind: MemoryKind::Feedback,
            body: "正文若干行。".into(),
        };
        store.save_entry(&entry).unwrap();

        let file = store.root().join("feedback_tests.md");
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(raw.starts_with("---\n"));
        assert!(raw.contains("type: feedback"));
        assert!(raw.contains("正文若干行。"));

        let index = std::fs::read_to_string(store.root().join("MEMORY.md")).unwrap();
        assert!(index.contains("[重构期对待过期测试的策略](feedback_tests.md)"));
        assert!(index.contains("过期 fixture 直接清理"));
    }

    #[test]
    fn load_entry_round_trips_frontmatter() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        let entry = MemoryEntry {
            file_stem: "user_role".into(),
            name: "用户角色".into(),
            description: "Senior Rust dev".into(),
            kind: MemoryKind::User,
            body: "正文。".into(),
        };
        store.save_entry(&entry).unwrap();
        let loaded = store.load_entry("user_role").unwrap().unwrap();
        assert_eq!(loaded, entry);
    }

    #[test]
    fn delete_entry_removes_file_and_index_row() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        let entry = MemoryEntry {
            file_stem: "project_freeze".into(),
            name: "项目冻结".into(),
            description: "deadline".into(),
            kind: MemoryKind::Project,
            body: "正文".into(),
        };
        store.save_entry(&entry).unwrap();
        assert!(store.delete_entry("project_freeze").unwrap());
        assert!(!store.root().join("project_freeze.md").exists());
        // 索引文件因为没条目了应该被清掉
        assert!(!store.root().join("MEMORY.md").exists());
        // 再删一次 → false
        assert!(!store.delete_entry("project_freeze").unwrap());
    }

    #[test]
    fn list_entries_skips_index_and_invalid_files() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        store
            .save_entry(&MemoryEntry {
                file_stem: "a_one".into(),
                name: "A".into(),
                description: "a".into(),
                kind: MemoryKind::Project,
                body: "正文 a".into(),
            })
            .unwrap();
        store
            .save_entry(&MemoryEntry {
                file_stem: "b_two".into(),
                name: "B".into(),
                description: "b".into(),
                kind: MemoryKind::Reference,
                body: "正文 b".into(),
            })
            .unwrap();
        // 写一个非法 .md
        std::fs::write(store.root().join("bad name.md"), "x").unwrap();
        std::fs::write(store.root().join("README.txt"), "x").unwrap();
        let entries = store.list_entries().unwrap();
        let stems: Vec<_> = entries.iter().map(|e| e.file_stem.as_str()).collect();
        assert_eq!(stems, vec!["a_one", "b_two"]);
    }

    #[test]
    fn render_for_prompt_returns_none_when_empty() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        assert!(store.render_for_prompt().unwrap().is_none());
    }

    #[test]
    fn render_for_prompt_lists_entries() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        store
            .save_entry(&MemoryEntry {
                file_stem: "feedback_tests".into(),
                name: "测试策略".into(),
                description: "fixture 直接清理".into(),
                kind: MemoryKind::Feedback,
                body: "正文".into(),
            })
            .unwrap();
        let rendered = store.render_for_prompt().unwrap().unwrap();
        assert!(rendered.contains("[feedback] feedback_tests.md"));
        assert!(rendered.contains("测试策略"));
        assert!(rendered.contains("memory_write"));
        let read_only = store.render_for_prompt_read_only().unwrap().unwrap();
        assert!(read_only.contains("[feedback] feedback_tests.md"));
        assert!(!read_only.contains("memory_write"));
    }

    #[test]
    fn invalid_file_stem_rejected() {
        let home = TempDir::new().unwrap();
        let store = store(&home, "/Users/x/proj");
        let bad = MemoryEntry {
            file_stem: "bad name".into(),
            name: "x".into(),
            description: "x".into(),
            kind: MemoryKind::Project,
            body: "x".into(),
        };
        assert!(matches!(
            store.save_entry(&bad),
            Err(ProjectMemoryError::InvalidFileStem(_))
        ));
    }

    #[test]
    fn parse_memory_write_save() {
        let raw = r#"{
            "action": "save",
            "file_stem": "user_role",
            "name": "用户角色",
            "description": "Senior Rust dev",
            "kind": "user",
            "body": "正文若干。"
        }"#;
        let action = parse_memory_write_arguments(raw).unwrap();
        match action {
            MemoryWriteAction::Save(entry) => {
                assert_eq!(entry.file_stem, "user_role");
                assert_eq!(entry.kind, MemoryKind::User);
                assert_eq!(entry.body, "正文若干。");
            }
            _ => panic!("expected Save"),
        }
    }

    #[test]
    fn parse_memory_write_delete() {
        let raw = r#"{ "action": "delete", "file_stem": "user_role" }"#;
        let action = parse_memory_write_arguments(raw).unwrap();
        match action {
            MemoryWriteAction::Delete { file_stem } => assert_eq!(file_stem, "user_role"),
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn parse_memory_write_rejects_unknown_action() {
        let raw = r#"{ "action": "purge", "file_stem": "x" }"#;
        assert!(matches!(
            parse_memory_write_arguments(raw),
            Err(MemoryWriteError::UnknownAction(_))
        ));
    }

    #[test]
    fn parse_memory_write_rejects_missing_body() {
        let raw = r#"{
            "action": "save", "file_stem": "u", "name": "n",
            "description": "d", "kind": "user"
        }"#;
        assert!(matches!(
            parse_memory_write_arguments(raw),
            Err(MemoryWriteError::MissingField("body"))
        ));
    }

    #[test]
    fn registry_returns_same_arc_for_same_workspace() {
        let home = TempDir::new().unwrap();
        let reg = ProjectMemoryRegistry::with_home(home.path().to_path_buf());
        let ws = WorkspaceRootPath::new("/Users/x/proj");
        let a = reg.get_or_open(&ws).unwrap();
        let b = reg.get_or_open(&ws).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
