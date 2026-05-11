//! 把 session-store 抛出的生命周期事件转成 SnapshotManager 调用。
//!
//! session-store 调 observer 时仍处在同步路径里，但 SnapshotManager 全是 async 接口，
//! 因此本 observer 把每次调用 spawn 到当前 tokio runtime；失败仅写日志，不阻塞 session 创建。

use std::path::PathBuf;
use std::sync::Arc;

use magi_core::{SessionId, WorkspaceId};
use magi_session_store::SessionLifecycleObserver;
use magi_snapshot::SnapshotManager;
use magi_workspace::WorkspaceStore;
use tracing::{error, warn};

pub(crate) struct SnapshotLifecycleObserver {
    manager: Arc<SnapshotManager>,
    workspaces: Arc<WorkspaceStore>,
}

impl SnapshotLifecycleObserver {
    pub fn new(manager: Arc<SnapshotManager>, workspaces: Arc<WorkspaceStore>) -> Self {
        Self {
            manager,
            workspaces,
        }
    }

    fn workspace_root_for(&self, workspace_id: &str) -> Option<PathBuf> {
        let target = WorkspaceId::new(workspace_id);
        self.workspaces
            .workspaces()
            .into_iter()
            .find(|workspace| workspace.workspace_id == target)
            .map(|workspace| PathBuf::from(workspace.root_path.as_str()))
    }
}

impl SessionLifecycleObserver for SnapshotLifecycleObserver {
    fn on_session_created(&self, session_id: &SessionId, workspace_id: Option<&str>) {
        let Some(workspace_id) = workspace_id else {
            // 未绑定 workspace 的 session 不需要快照账本（无文件追踪范围）。
            return;
        };
        let Some(root) = self.workspace_root_for(workspace_id) else {
            warn!(
                ?session_id,
                workspace_id, "snapshot lifecycle: workspace_id 未在 WorkspaceStore 注册，跳过快照启动"
            );
            return;
        };
        let manager = self.manager.clone();
        let session_id_str = session_id.as_str().to_string();
        tokio::spawn(async move {
            if let Err(err) = manager.start_session(session_id_str.clone(), root).await {
                error!(session_id = %session_id_str, error = %err, "snapshot lifecycle: 启动 session 失败");
            }
        });
    }

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
