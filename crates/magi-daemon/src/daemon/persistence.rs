use super::config::DaemonError;
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_knowledge_store::KnowledgeState;
use magi_session_store::{
    SessionDurableState, SessionExecutionSidecarStoreState, SessionStore,
};
use magi_workspace::{
    WorkspaceDurableState, WorkspaceRecoverySidecarStoreState, WorkspaceStore,
};
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

    #[cfg(test)]
    pub(crate) fn load_session_durable_state(&self) -> Result<SessionDurableState, DaemonError> {
        self.read_json_or_default(self.state_root.join("sessions.json"))
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
        let global_path = self.state_root.join("sessions.json");

        // 迁移：旧全局文件存在时，按 workspace_id 分发到各工作区目录
        if global_path.exists() {
            let legacy: SessionDurableState = self.read_json_or_default(global_path.clone())?;
            if !legacy.sessions.is_empty() {
                let default_ws = workspace_roots.first().map(|(id, _)| id.clone());
                for session in &legacy.sessions {
                    let ws_id = session.workspace_id.clone()
                        .or_else(|| default_ws.clone())
                        .unwrap_or_default();
                    if let Some((_, root)) = workspace_roots.iter().find(|(id, _)| id == &ws_id) {
                        let mut ws_state = self.load_workspace_session_state(root)?;
                        if !ws_state.sessions.iter().any(|s| s.session_id == session.session_id) {
                            ws_state.sessions.push(session.clone());
                            self.save_workspace_session_state(root, &ws_state)?;
                        }
                    }
                }
                let _ = fs::remove_file(&global_path);
            }
        }

        // 从各工作区 .magi/sessions.json 合并加载
        let mut merged = SessionDurableState::default();
        for (_, root_path) in workspace_roots {
            let ws_state = self.load_workspace_session_state(root_path)?;
            merged.sessions.extend(ws_state.sessions);
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

    #[cfg(test)]
    pub(crate) fn save_session_durable_state(
        &self,
        state: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.state_root.join("sessions.json"), state)
    }

    pub(crate) fn session_sidecars_path(&self) -> PathBuf {
        self.state_root.join("session-sidecars.json")
    }

    pub(crate) fn load_session_sidecars(
        &self,
    ) -> Result<SessionExecutionSidecarStoreState, DaemonError> {
        self.read_json_or_default_with_legacy(
            self.session_sidecars_path(),
            Some(self.state_root.join("sessions.json")),
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

    pub(crate) fn workspace_recovery_sidecars_path(&self) -> PathBuf {
        self.state_root.join("workspace-recovery-sidecars.json")
    }

    pub(crate) fn load_workspace_recovery_sidecars(
        &self,
    ) -> Result<WorkspaceRecoverySidecarStoreState, DaemonError> {
        self.read_json_or_default_with_legacy(
            self.workspace_recovery_sidecars_path(),
            Some(self.state_root.join("workspaces.json")),
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

    pub(crate) fn load_audit_usage_ledger(
        &self,
    ) -> Result<AuditUsageLedgerSnapshot, DaemonError> {
        self.read_json_or_default(self.audit_usage_ledger_path())
    }

    pub(crate) fn knowledge_state_path(&self) -> PathBuf {
        self.state_root.join("knowledge.json")
    }

    pub(crate) fn load_knowledge_state(&self) -> Result<KnowledgeState, DaemonError> {
        self.read_json_or_default(self.knowledge_state_path())
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
        legacy_path: Option<PathBuf>,
    ) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        if !path.exists() {
            if let Some(legacy_path) = legacy_path && legacy_path.exists() {
                return self.read_json_value_or_default(legacy_path);
            }
            return Ok(T::default());
        }
        self.read_json_value_or_default(path)
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
}

#[derive(Clone)]
pub(crate) struct ShadowRuntimeSidecarPersistence {
    state_repository: ShadowStateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
}

impl ShadowRuntimeSidecarPersistence {
    pub(crate) fn new(
        state_repository: ShadowStateRepository,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
    ) -> Self {
        Self {
            state_repository,
            session_store,
            workspace_store,
        }
    }

    pub(crate) fn flush_runtime_sidecars(
        &self,
    ) -> Result<RuntimeSidecarFlushReport, DaemonError> {
        let session_sidecars_flushed = self
            .session_store
            .flush_execution_sidecars_with(|state| self.state_repository.save_session_sidecars(state))?;
        let workspace_recovery_sidecars_flushed =
            self.workspace_store.flush_recovery_sidecars_with(|state| {
                self.state_repository.save_workspace_recovery_sidecars(state)
            })?;
        Ok(RuntimeSidecarFlushReport {
            session_sidecars_flushed,
            workspace_recovery_sidecars_flushed,
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
