use crate::blob_store::BlobStore;
use crate::error::{SnapshotError, SnapshotResult};
use crate::session::SnapshotSession;
use std::collections::HashMap;
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
