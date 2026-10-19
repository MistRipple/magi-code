//! 把 session-store 抛出的生命周期事件转成 SnapshotManager 调用。
//!
//! 创建会话不启动快照扫描；归档/删除在当前 tokio runtime 上释放已经创建的资源。

use std::sync::Arc;

use magi_core::SessionId;
use magi_session_store::SessionLifecycleObserver;
use magi_snapshot::SnapshotManager;
use tracing::warn;

pub(crate) struct SnapshotLifecycleObserver {
    manager: Arc<SnapshotManager>,
}

impl SnapshotLifecycleObserver {
    pub fn new(manager: Arc<SnapshotManager>) -> Self {
        Self { manager }
    }
}

impl SessionLifecycleObserver for SnapshotLifecycleObserver {
    // 创建会话不扫描工作区；首次写入/执行或打开变更视图时由唯一 ensure 入口建立。
    fn on_session_created(&self, _session_id: &SessionId, _workspace_id: Option<&str>) {}

    fn on_session_archived(&self, session_id: &SessionId) {
        let manager = self.manager.clone();
        let session_id_str = session_id.as_str().to_string();
        tokio::spawn(async move {
            if let Err(err) = manager.archive_session(&session_id_str).await {
                warn!(session_id = %session_id_str, error = %err, "snapshot lifecycle: 归档 session 失败");
            }
        });
    }

    fn on_session_deleted(&self, session_id: &SessionId) {
        let manager = self.manager.clone();
        let session_id_str = session_id.as_str().to_string();
        tokio::spawn(async move {
            if let Err(err) = manager.drop_session(&session_id_str).await {
                warn!(session_id = %session_id_str, error = %err, "snapshot lifecycle: 删除 session 失败");
            }
        });
    }
}
