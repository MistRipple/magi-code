use super::config::DaemonError;
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_knowledge_store::KnowledgeState;
use magi_session_store::{SessionDurableState, SessionExecutionSidecarStoreState, SessionStore};
use magi_worker_runtime::{WorkerRuntime, WorkerRuntimeDurableSnapshot};
use magi_workspace::{WorkspaceDurableState, WorkspaceRecoverySidecarStoreState, WorkspaceStore};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::warn;

#[derive(Clone, Debug)]
pub(crate) struct ShadowStateRepository {
    state_root: PathBuf,
}

impl ShadowStateRepository {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self { state_root }
    }

    pub(crate) fn session_durable_state_path(&self) -> PathBuf {
        self.state_root.join("sessions.json")
    }

    pub(crate) fn load_session_durable_state(&self) -> Result<SessionDurableState, DaemonError> {
        self.read_json_or_default(self.session_durable_state_path())
    }

    /// 从指定工作区的 .magi/sessions.json 加载会话
    pub(crate) fn load_workspace_session_state(
        &self,
        workspace_root: &Path,
    ) -> Result<SessionDurableState, DaemonError> {
        let path = workspace_root.join(".magi").join("sessions.json");
        if path.exists() {
            self.read_json_or_default(path)
        } else {
            Ok(SessionDurableState::default())
        }
    }

    /// 遍历所有工作区加载会话，合并为统一的 SessionDurableState。
    /// 如果全局 sessions.json 仍存在（旧数据），执行一次性迁移后删除。
    pub(crate) fn load_sessions_from_workspaces(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<SessionDurableState, DaemonError> {
        let global_path = self.session_durable_state_path();

        // 迁移：旧全局文件里如果仍携带 workspace 绑定会话，则分发回各工作区；
        // 未绑定工作区的会话继续保留在全局 sessions.json。
        if global_path.exists() {
            let legacy = self.load_session_durable_state()?;
            let (global_state, workspace_states) = legacy.partition_by_workspace();
            for (workspace_id, workspace_state) in workspace_states {
                if let Some((_, root)) = workspace_roots.iter().find(|(id, _)| id == &workspace_id)
                {
                    let mut ws_state = self.load_workspace_session_state(root)?;
                    if !workspace_state.sessions.is_empty() {
                        ws_state.sessions.extend(workspace_state.sessions);
                        ws_state.timeline.extend(workspace_state.timeline);
                        ws_state.notifications.extend(workspace_state.notifications);
                        if ws_state.current_session_id.is_none() {
                            ws_state.current_session_id = workspace_state.current_session_id;
                        }
                        self.save_workspace_session_state(root, &ws_state)?;
                    }
                }
            }
            if global_state.is_empty() {
                let _ = fs::remove_file(&global_path);
            } else {
                self.save_session_durable_state(&global_state)?;
            }
        }

        // 从全局未绑定会话 + 各工作区 .magi/sessions.json 合并加载
        let mut merged = self.load_session_durable_state()?;
        for (_, root_path) in workspace_roots {
            let ws_state = self.load_workspace_session_state(root_path)?;
            merged.sessions.extend(ws_state.sessions);
            merged.timeline.extend(ws_state.timeline);
            merged.notifications.extend(ws_state.notifications);
            if merged.current_session_id.is_none() {
                merged.current_session_id = ws_state.current_session_id;
            }
        }
        Ok(merged)
    }

    /// 保存会话到指定工作区的 .magi/sessions.json
    pub(crate) fn save_workspace_session_state(
        &self,
        workspace_root: &Path,
        state: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        let path = workspace_root.join(".magi").join("sessions.json");
        self.write_json_atomically(path, state)
    }

    pub(crate) fn save_session_durable_state(
        &self,
        state: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.session_durable_state_path(), state)
    }

    pub(crate) fn session_sidecars_path(&self) -> PathBuf {
        self.state_root.join("session-sidecars.json")
    }

    pub(crate) fn load_session_sidecars(
        &self,
    ) -> Result<SessionExecutionSidecarStoreState, DaemonError> {
        let path = self.session_sidecars_path();
        if path.exists() {
            return self.read_json_or_default(path);
        }
        self.read_json_with_required_field_or_default(
            self.session_durable_state_path(),
            "runtime_sidecars",
        )
    }

    pub(crate) fn save_session_sidecars(
        &self,
        state: &SessionExecutionSidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.session_sidecars_path(), state)
    }

    pub(crate) fn load_workspace_durable_state(
        &self,
    ) -> Result<WorkspaceDurableState, DaemonError> {
        self.read_json_or_default(self.state_root.join("workspaces.json"))
    }

    pub(crate) fn save_workspace_durable_state(
        &self,
        state: &WorkspaceDurableState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.state_root.join("workspaces.json"), state)
    }

    pub(crate) fn worker_runtime_snapshot_path(&self) -> PathBuf {
        self.state_root.join("worker-runtime.json")
    }

    pub(crate) fn load_worker_runtime_snapshot(
        &self,
    ) -> Result<WorkerRuntimeDurableSnapshot, DaemonError> {
        self.read_json_or_default(self.worker_runtime_snapshot_path())
    }

    pub(crate) fn save_worker_runtime_snapshot(
        &self,
        snapshot: &WorkerRuntimeDurableSnapshot,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.worker_runtime_snapshot_path(), snapshot)
    }

    pub(crate) fn workspace_recovery_sidecars_path(&self) -> PathBuf {
        self.state_root.join("workspace-recovery-sidecars.json")
    }

    pub(crate) fn load_workspace_recovery_sidecars(
        &self,
    ) -> Result<WorkspaceRecoverySidecarStoreState, DaemonError> {
        let path = self.workspace_recovery_sidecars_path();
        if path.exists() {
            return self.read_json_or_default(path);
        }
        self.read_json_with_required_field_or_default(
            self.state_root.join("workspaces.json"),
            "recovery_handles",
        )
    }

    pub(crate) fn save_workspace_recovery_sidecars(
        &self,
        state: &WorkspaceRecoverySidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.workspace_recovery_sidecars_path(), state)
    }

    pub(crate) fn audit_usage_ledger_path(&self) -> PathBuf {
        self.state_root.join("audit-usage-ledger.json")
    }

    pub(crate) fn load_audit_usage_ledger(&self) -> Result<AuditUsageLedgerSnapshot, DaemonError> {
        self.read_json_or_default(self.audit_usage_ledger_path())
    }

    pub(crate) fn knowledge_state_path(&self) -> PathBuf {
        self.state_root.join("knowledge.json")
    }

    pub(crate) fn load_knowledge_state(&self) -> Result<KnowledgeState, DaemonError> {
        self.read_json_or_default(self.knowledge_state_path())
    }

    pub(crate) fn save_knowledge_state(
        &self,
        state: &KnowledgeState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.knowledge_state_path(), state)
    }

    fn read_json_or_default<T>(&self, path: PathBuf) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        self.read_json_or_default_with_legacy(path, None)
    }

    fn read_json_or_default_with_legacy<T>(
        &self,
        path: PathBuf,
        legacy_required_field: Option<&str>,
    ) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        match legacy_required_field {
            Some(field_name) => self.read_json_with_required_field_or_default(path, field_name),
            None => {
                if !path.exists() {
                    return Ok(T::default());
                }
                self.read_json_value_or_default(path)
            }
        }
    }

    fn read_json_value_or_default<T>(&self, path: PathBuf) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        let content = fs::read_to_string(&path)?;
        match serde_json::from_str(&content) {
            Ok(value) => Ok(value),
            Err(error) => {
                let backup_path = stale_backup_path(&path);
                warn!(
                    path = %path.display(),
                    backup_path = %backup_path.display(),
                    error = %error,
                    "影子状态文件与当前 schema 不兼容，已转存并回退到默认状态"
                );
                fs::rename(&path, &backup_path)?;
                Ok(T::default())
            }
        }
    }

    fn read_json_with_required_field_or_default<T>(
        &self,
        path: PathBuf,
        field_name: &str,
    ) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        if !path.exists() {
            return Ok(T::default());
        }
        let content = fs::read_to_string(&path)?;
        let value: serde_json::Value = match serde_json::from_str(&content) {
            Ok(value) => value,
            Err(error) => {
                let backup_path = stale_backup_path(&path);
                warn!(
                    path = %path.display(),
                    backup_path = %backup_path.display(),
                    error = %error,
                    "影子状态文件与当前 schema 不兼容，已转存并回退到默认状态"
                );
                fs::rename(&path, &backup_path)?;
                return Ok(T::default());
            }
        };
        if value.get(field_name).is_none() {
            return Ok(T::default());
        }
        serde_json::from_value(value).map_err(DaemonError::from)
    }

    fn write_json_atomically<T>(&self, path: PathBuf, value: &T) -> Result<(), DaemonError>
    where
        T: serde::Serialize,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = temp_path_for(&path);
        let content = serde_json::to_vec_pretty(value)?;
        fs::write(&temp_path, content)?;
        fs::rename(temp_path, path)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeSidecarFlushReport {
    pub(crate) session_sidecars_flushed: bool,
    pub(crate) workspace_recovery_sidecars_flushed: bool,
    pub(crate) worker_runtime_snapshot_flushed: bool,
}

#[derive(Clone)]
pub(crate) struct ShadowRuntimeSidecarPersistence {
    state_repository: ShadowStateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    worker_runtime: WorkerRuntime,
}

impl ShadowRuntimeSidecarPersistence {
    pub(crate) fn new(
        state_repository: ShadowStateRepository,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
        worker_runtime: WorkerRuntime,
    ) -> Self {
        Self {
            state_repository,
            session_store,
            workspace_store,
            worker_runtime,
        }
    }

    pub(crate) fn worker_runtime_snapshot_dirty(&self) -> bool {
        self.worker_runtime.durable_snapshot_dirty()
    }

    pub(crate) fn flush_runtime_sidecars(&self) -> Result<RuntimeSidecarFlushReport, DaemonError> {
        let worker_runtime_snapshot_flushed =
            self.worker_runtime
                .flush_durable_snapshot_with(|snapshot| {
                    self.state_repository.save_worker_runtime_snapshot(snapshot)
                })?;
        let session_sidecars_flushed =
            self.session_store.flush_execution_sidecars_with(|state| {
                self.state_repository.save_session_sidecars(state)
            })?;
        let workspace_recovery_sidecars_flushed =
            self.workspace_store.flush_recovery_sidecars_with(|state| {
                self.state_repository
                    .save_workspace_recovery_sidecars(state)
            })?;
        Ok(RuntimeSidecarFlushReport {
            session_sidecars_flushed,
            workspace_recovery_sidecars_flushed,
            worker_runtime_snapshot_flushed,
        })
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "shadow-state.json".to_string());
    let unique_suffix = format!(
        ".{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    file_name.push_str(&unique_suffix);
    path.with_file_name(file_name)
}

fn stale_backup_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "shadow-state.json".to_string());
    file_name.push_str(".stale");
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{SessionId, SessionLifecycleStatus, UtcMillis};
    use magi_session_store::{
        NotificationRecord, SessionDurableState, SessionRecord, TimelineEntry, TimelineEntryKind,
    };

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::create_dir_all(&path).expect("temp dir should create");
        path
    }

    #[test]
    fn load_sessions_from_workspaces_merges_timeline_and_notifications() {
        let state_root = unique_temp_dir("magi-persistence-state");
        let workspace_root = unique_temp_dir("magi-persistence-workspace");
        let repository = ShadowStateRepository::new(state_root.clone());
        let session_id = SessionId::new("session-persisted-timeline");
        let now = UtcMillis::now();
        let workspace_state = SessionDurableState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "持久化会话".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: now,
                updated_at: now,
                message_count: Some(1),
                workspace_id: Some("workspace-persisted".to_string()),
            }],
            timeline: vec![TimelineEntry {
                entry_id: "timeline-persisted-user".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::UserMessage,
                message: "恢复后的用户消息".to_string(),
                occurred_at: now,
            }],
            notifications: vec![NotificationRecord {
                notification_id: "notification-persisted".to_string(),
                session_id: session_id.clone(),
                kind: "info".to_string(),
                message: "恢复后的通知".to_string(),
                created_at: now,
                handled: false,
            }],
        };
        repository
            .save_workspace_session_state(&workspace_root, &workspace_state)
            .expect("workspace session state should save");

        let merged = repository
            .load_sessions_from_workspaces(&[(
                "workspace-persisted".to_string(),
                workspace_root.clone(),
            )])
            .expect("workspace session state should load");

        assert_eq!(merged.sessions.len(), 1);
        assert_eq!(merged.timeline.len(), 1);
        assert_eq!(merged.notifications.len(), 1);
        assert_eq!(merged.current_session_id, Some(session_id));

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn load_sessions_from_workspaces_preserves_global_unbound_sessions() {
        let state_root = unique_temp_dir("magi-persistence-global-session");
        let workspace_root = unique_temp_dir("magi-persistence-global-workspace");
        let repository = ShadowStateRepository::new(state_root.clone());
        let now = UtcMillis::now();
        let global_session_id = SessionId::new("session-global-unbound");
        let workspace_session_id = SessionId::new("session-workspace-bound");

        repository
            .save_session_durable_state(&SessionDurableState {
                current_session_id: Some(global_session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id: global_session_id.clone(),
                    title: "全局会话".to_string(),
                    status: SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: Some(1),
                    workspace_id: None,
                }],
                timeline: vec![TimelineEntry {
                    entry_id: "timeline-global-session".to_string(),
                    session_id: global_session_id.clone(),
                    kind: TimelineEntryKind::UserMessage,
                    message: "全局未绑定消息".to_string(),
                    occurred_at: now,
                }],
                notifications: vec![],
            })
            .expect("global session durable state should save");

        repository
            .save_workspace_session_state(
                &workspace_root,
                &SessionDurableState {
                    current_session_id: Some(workspace_session_id.clone()),
                    sessions: vec![SessionRecord {
                        session_id: workspace_session_id.clone(),
                        title: "工作区会话".to_string(),
                        status: SessionLifecycleStatus::Active,
                        created_at: now,
                        updated_at: now,
                        message_count: Some(2),
                        workspace_id: Some("workspace-bound".to_string()),
                    }],
                    timeline: vec![TimelineEntry {
                        entry_id: "timeline-workspace-session".to_string(),
                        session_id: workspace_session_id.clone(),
                        kind: TimelineEntryKind::UserMessage,
                        message: "工作区绑定消息".to_string(),
                        occurred_at: now,
                    }],
                    notifications: vec![],
                },
            )
            .expect("workspace session durable state should save");

        let merged = repository
            .load_sessions_from_workspaces(&[(
                "workspace-bound".to_string(),
                workspace_root.clone(),
            )])
            .expect("session durable states should merge");

        assert_eq!(merged.sessions.len(), 2);
        assert!(merged.sessions.iter().any(
            |session| session.session_id == global_session_id && session.workspace_id.is_none()
        ));
        assert!(merged.sessions.iter().any(|session| {
            session.session_id == workspace_session_id
                && session.workspace_id.as_deref() == Some("workspace-bound")
        }));
        assert_eq!(merged.current_session_id, Some(global_session_id));
        assert_eq!(merged.timeline.len(), 2);

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn load_session_sidecars_does_not_treat_current_global_session_file_as_legacy_sidecars() {
        let state_root = unique_temp_dir("magi-persistence-session-sidecar-legacy-guard");
        let repository = ShadowStateRepository::new(state_root.clone());
        let now = UtcMillis::now();
        let session_id = SessionId::new("session-no-legacy-sidecar");

        repository
            .save_session_durable_state(&SessionDurableState {
                current_session_id: Some(session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id,
                    title: "普通会话".to_string(),
                    status: SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: Some(0),
                    workspace_id: None,
                }],
                timeline: vec![],
                notifications: vec![],
            })
            .expect("global session durable state should save");

        let sidecars = repository
            .load_session_sidecars()
            .expect("current global session file should not be parsed as legacy sidecars");
        assert!(sidecars.runtime_sidecars.is_empty());
        assert!(repository.session_durable_state_path().exists());

        let _ = fs::remove_dir_all(state_root);
    }
}
