use crate::blob_store::BlobStore;
use crate::error::{SnapshotError, SnapshotResult};
use crate::session::SnapshotSession;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

/// 顶层管理：跨 session 共享 BlobStore，按 session_id 维持 SnapshotSession 实例。
/// session_id 在 session-store 内是全局唯一键；这里额外校验 workspace_root，
/// 防止旧 URL 或错误调用把同一个 session 复用到另一个工作区快照账本。
///
/// 每个 workspace 有自己的 `.magi/snapshots/blobs` 目录，blob 在 workspace 内跨 session 共享。
///
/// `blob_stores` 因为构造涉及磁盘 I/O，使用 async RwLock。
/// `sessions` 仅做 HashMap 查找，使用 std::sync::RwLock 让查询路径不需要 async 上下文，
/// 便于 sync 投影（bootstrap、HTTP 同步处理器）按需取出 SnapshotSession。
pub struct SnapshotManager {
    blob_stores: AsyncRwLock<HashMap<PathBuf, Arc<BlobStore>>>,
    sessions: StdRwLock<HashMap<String, Arc<SnapshotSession>>>,
    start_lock: AsyncMutex<()>,
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self {
            blob_stores: AsyncRwLock::new(HashMap::new()),
            sessions: StdRwLock::new(HashMap::new()),
            start_lock: AsyncMutex::new(()),
        }
    }

    /// 启动新 session（或在已有 session 上恢复）。
    pub async fn start_session(
        &self,
        session_id: String,
        workspace_root: PathBuf,
    ) -> SnapshotResult<Arc<SnapshotSession>> {
        let workspace_root = canonical_workspace_root(&workspace_root)?;
        if let Some(session) = self.get_session(&session_id) {
            ensure_same_workspace(&session_id, &workspace_root, session.workspace_root())?;
            return Ok(session);
        }

        let _start_guard = self.start_lock.lock().await;
        if let Some(session) = self.get_session(&session_id) {
            ensure_same_workspace(&session_id, &workspace_root, session.workspace_root())?;
            return Ok(session);
        }

        self.create_session(session_id, workspace_root).await
    }

    /// Git branch/HEAD 被结构化操作切换后，以新的代码树重建该 session 的快照基线。
    ///
    /// 调用方必须已经持有 workspace Git mutation/turn 排他权。这里继续使用 start_lock，
    /// 避免与 lazy start 并发创建两个 watcher 或恢复旧 baseline。
    pub async fn rebase_session(
        &self,
        session_id: String,
        workspace_root: PathBuf,
    ) -> SnapshotResult<Arc<SnapshotSession>> {
        let workspace_root = canonical_workspace_root(&workspace_root)?;
        let _start_guard = self.start_lock.lock().await;
        if let Some(session) = self.get_session(&session_id) {
            ensure_same_workspace(&session_id, &workspace_root, session.workspace_root())?;
            self.sessions
                .write()
                .expect("snapshot sessions registry poisoned")
                .remove(&session_id);
            session.drop_session().await?;
        }
        self.create_session(session_id, workspace_root).await
    }

    async fn create_session(
        &self,
        session_id: String,
        workspace_root: PathBuf,
    ) -> SnapshotResult<Arc<SnapshotSession>> {
        ensure_snapshot_storage_git_excluded(&workspace_root)?;
        let snapshots_root = snapshots_root_for(&workspace_root);
        let blobs_dir = snapshots_root.join("blobs");

        let blobs = {
            let read = self.blob_stores.read().await;
            if let Some(s) = read.get(&workspace_root) {
                s.clone()
            } else {
                drop(read);
                let mut write = self.blob_stores.write().await;
                if let Some(s) = write.get(&workspace_root) {
                    s.clone()
                } else {
                    let store = Arc::new(BlobStore::new(&blobs_dir)?);
                    write.insert(workspace_root.clone(), store.clone());
                    store
                }
            }
        };

        let respect_gitignore = workspace_root.join(".git").is_dir();
        let session = SnapshotSession::start(
            session_id.clone(),
            workspace_root,
            blobs,
            snapshots_root,
            respect_gitignore,
        )
        .await?;

        self.sessions
            .write()
            .expect("snapshot sessions registry poisoned")
            .insert(session_id, session.clone());
        Ok(session)
    }

    pub fn get_session(&self, session_id: &str) -> Option<Arc<SnapshotSession>> {
        self.sessions
            .read()
            .expect("snapshot sessions registry poisoned")
            .get(session_id)
            .cloned()
    }

    pub fn get_session_for_workspace(
        &self,
        session_id: &str,
        workspace_root: &Path,
    ) -> Option<Arc<SnapshotSession>> {
        let workspace_root = canonical_workspace_root(workspace_root).ok()?;
        self.get_session(session_id)
            .filter(|session| session.workspace_root() == workspace_root.as_path())
    }

    /// 关闭 watcher，但保留磁盘账本（archive 语义）。
    pub async fn archive_session(&self, session_id: &str) -> SnapshotResult<()> {
        let session = self
            .sessions
            .read()
            .expect("snapshot sessions registry poisoned")
            .get(session_id)
            .cloned()
            .ok_or_else(|| SnapshotError::SessionNotFound(session_id.to_string()))?;
        session.archive().await;
        session.release_runtime_blob_ownership()?;
        // 从内存映射移除，下次 start_session 会从磁盘恢复。
        self.sessions
            .write()
            .expect("snapshot sessions registry poisoned")
            .remove(session_id);
        Ok(())
    }

    /// 删除 session 与其磁盘账本，并释放 blob 引用。
    pub async fn drop_session(&self, session_id: &str) -> SnapshotResult<()> {
        let session = self
            .sessions
            .write()
            .expect("snapshot sessions registry poisoned")
            .remove(session_id)
            .ok_or_else(|| SnapshotError::SessionNotFound(session_id.to_string()))?;
        session.drop_session().await
    }
}

/// Snapshot 账本属于 Magi 的本地运行态，不能反过来把用户仓库标成 dirty。
///
/// 只写 repository-local 的 `info/exclude`，不改用户受版本控制的 `.gitignore`；
/// linked worktree 则沿 `commondir` 定位共享 Git 目录。若这些路径中的文件
/// 已经被显式跟踪，Git 仍会正常报告改动，避免隐藏用户真实文件。
fn ensure_snapshot_storage_git_excluded(workspace_root: &Path) -> SnapshotResult<()> {
    let dot_git = workspace_root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let marker = std::fs::read_to_string(&dot_git)
            .map_err(|error| SnapshotError::io(&dot_git, error))?;
        let raw = marker
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SnapshotError::InvalidRoot(format!(
                    "invalid linked-worktree .git marker: {}",
                    dot_git.display()
                ))
            })?;
        resolve_relative_path(workspace_root, Path::new(raw))
    } else {
        return Ok(());
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(raw) => resolve_relative_path(&git_dir, Path::new(raw.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir,
        Err(error) => return Err(SnapshotError::io(git_dir.join("commondir"), error)),
    };
    let exclude_path = common_dir.join("info").join("exclude");
    let existing = match std::fs::read_to_string(&exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(SnapshotError::io(&exclude_path, error)),
    };
    const PATTERNS: [&str; 2] = ["/.magi/snapshots/", "/.magi/cache/"];
    let missing = PATTERNS
        .into_iter()
        .filter(|pattern| !existing.lines().any(|line| line.trim() == *pattern))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| SnapshotError::io(parent, error))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|error| SnapshotError::io(&exclude_path, error))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")
            .map_err(|error| SnapshotError::io(&exclude_path, error))?;
    }
    for pattern in missing {
        file.write_all(format!("{pattern}\n").as_bytes())
            .map_err(|error| SnapshotError::io(&exclude_path, error))?;
    }
    file.sync_all()
        .map_err(|error| SnapshotError::io(&exclude_path, error))
}

fn resolve_relative_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

impl Default for SnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn snapshots_root_for(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".magi").join("snapshots")
}

fn canonical_workspace_root(workspace_root: &Path) -> SnapshotResult<PathBuf> {
    if !workspace_root.is_absolute() {
        return Err(SnapshotError::InvalidRoot(format!(
            "workspace_root must be absolute: {}",
            workspace_root.display()
        )));
    }
    if !workspace_root.is_dir() {
        return Err(SnapshotError::InvalidRoot(format!(
            "workspace_root not a directory: {}",
            workspace_root.display()
        )));
    }
    std::fs::canonicalize(workspace_root).map_err(|error| SnapshotError::io(workspace_root, error))
}

fn ensure_same_workspace(
    session_id: &str,
    requested_workspace_root: &Path,
    existing_workspace_root: &Path,
) -> SnapshotResult<()> {
    if requested_workspace_root == existing_workspace_root {
        return Ok(());
    }
    Err(SnapshotError::InvalidRoot(format!(
        "snapshot session {session_id} already bound to workspace {}, requested {}",
        existing_workspace_root.display(),
        requested_workspace_root.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rebase_session_replaces_old_branch_baseline_with_current_tree() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("branch.txt"), "main\n").expect("main fixture");
        let manager = SnapshotManager::new();
        let original = manager
            .start_session(
                "session-git-rebase".to_string(),
                workspace.path().to_path_buf(),
            )
            .await
            .expect("start snapshot");

        std::fs::write(workspace.path().join("branch.txt"), "feature\n").expect("feature fixture");
        let rebased = manager
            .rebase_session(
                "session-git-rebase".to_string(),
                workspace.path().to_path_buf(),
            )
            .await
            .expect("rebase snapshot");

        assert!(!Arc::ptr_eq(&original, &rebased));
        assert!(
            rebased
                .pending_changes()
                .expect("pending changes")
                .is_empty(),
            "新 branch 当前文件树必须成为新 baseline"
        );
    }
}
