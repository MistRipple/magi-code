//! 把 session-store 抛出的生命周期事件转成 SnapshotManager 调用。
//!
//! 创建会话不启动快照扫描；归档/删除在当前 tokio runtime 上释放已经创建的资源。

use std::sync::Arc;

use magi_core::SessionId;
use magi_session_store::SessionLifecycleObserver;
use magi_snapshot::{SnapshotError, SnapshotManager};
use tracing::warn;

/// daemon 与真实执行 harness 共用固定状态布局，避免直接执行与 Git rebase
/// 两条入口把同一账本写到不同位置。
pub fn managed_snapshot_manager(
    state_root: std::path::PathBuf,
    sessions: Arc<magi_session_store::SessionStore>,
) -> SnapshotManager {
    SnapshotManager::new().with_storage_root_resolver(Arc::new(move |session_id, root| {
        use sha2::{Digest, Sha256};
        let session = sessions
            .session(&SessionId::new(session_id))
            .ok_or_else(|| SnapshotError::SessionNotFound(session_id.into()))?;
        let owner_root = match session.workspace_id.as_deref() {
            Some(id) => {
                magi_core::paths::project_state_root(&state_root, &magi_core::WorkspaceId::new(id))
            }
            None => state_root.join("personal-snapshot-state").join(
                session_id
                    .as_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            ),
        };
        let host = magi_core::HostPath::from_path(root.to_path_buf());
        let root_key = Sha256::digest(host.to_path_ref().as_str().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(owner_root.join(".magi/snapshots").join(root_key))
    }))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn project_and_personal_ledgers_use_managed_storage_outside_source_folders() {
        let state = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let personal = state.path().join("personal-sessions/personal");
        std::fs::create_dir_all(&personal).unwrap();
        for root in [a.path(), b.path(), &personal] {
            std::fs::write(root.join("same.txt"), "baseline").unwrap();
        }
        let sessions = Arc::new(magi_session_store::SessionStore::new());
        sessions
            .create_session_for_workspace(
                SessionId::new("project-session"),
                "project",
                Some("project/../stable".into()),
            )
            .unwrap();
        sessions
            .create_session(SessionId::new("personal"), "personal")
            .unwrap();
        let manager = managed_snapshot_manager(state.path().into(), sessions.clone());
        for (id, root) in [
            ("project-session", a.path()),
            ("project-session", b.path()),
            ("personal", personal.as_path()),
        ] {
            let snapshot = manager.start_session(id.into(), root.into()).await.unwrap();
            std::fs::write(root.join("same.txt"), id).unwrap();
            snapshot.reconcile().unwrap();
            assert_eq!(
                snapshot.pending_changes().unwrap()[0]
                    .original_content
                    .as_deref(),
                Some("baseline")
            );
            assert!(
                !root.join(".magi/snapshots").exists(),
                "源目录不能成为新账本写入口"
            );
        }
        manager.archive_session("project-session").await.unwrap();
        manager.archive_session("personal").await.unwrap();
        let project_state = magi_core::paths::project_state_root(
            state.path(),
            &magi_core::WorkspaceId::new("project/../stable"),
        );
        assert_eq!(
            std::fs::read_dir(project_state.join(".magi/snapshots"))
                .unwrap()
                .count(),
            2
        );
        let restarted = managed_snapshot_manager(state.path().into(), sessions);
        let restored = restarted
            .start_session("project-session".into(), b.path().into())
            .await
            .unwrap();
        assert_eq!(
            restored.pending_changes().unwrap()[0]
                .original_content
                .as_deref(),
            Some("baseline")
        );
        let restored_personal = restarted
            .start_session("personal".into(), personal)
            .await
            .unwrap();
        assert_eq!(
            restored_personal.pending_changes().unwrap()[0]
                .preview_content
                .as_deref(),
            Some("personal")
        );
    }
}
