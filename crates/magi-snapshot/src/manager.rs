use crate::blob_store::BlobStore;
use crate::error::{SnapshotError, SnapshotResult};
use crate::session::SnapshotSession;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

/// 顶层管理：跨 session 共享 BlobStore，按 session_id 维持 SnapshotSession 实例。
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
        if let Some(session) = self.get_session(&session_id) {
            return Ok(session);
        }

        let _start_guard = self.start_lock.lock().await;
        if let Some(session) = self.get_session(&session_id) {
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
