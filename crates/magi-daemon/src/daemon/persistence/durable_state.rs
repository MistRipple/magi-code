use super::*;

impl StateRepository {
    pub(crate) fn task_store_projection_path(&self) -> PathBuf {
        self.state_root.join("task-store-projections")
    }

    /// 在 repository 写锁内提交 task manifest。manifest 是 task projection 的唯一提交点；
    /// 提交后所有更早接纳的 task（包括已删除的 task）都已被该完整快照覆盖。
    pub(crate) fn checkpoint_task_store(
        &self,
        task_store: &TaskStore,
    ) -> Result<usize, DaemonError> {
        let snapshot = task_store.snapshot();
        self.checkpoint_task_store_snapshot(&snapshot)
    }

    pub(crate) fn checkpoint_task_store_snapshot(
        &self,
        snapshot: &TaskStoreSnapshot,
    ) -> Result<usize, DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        self.ensure_state_layout_marker_locked()?;
        Ok(TaskStore::checkpoint_snapshot_to_projection_directory(
            snapshot,
            &self.task_store_projection_path(),
        )?)
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
        self.read_json_or_default(self.workspace_recovery_sidecars_path())
    }

    pub(crate) fn save_workspace_recovery_sidecars(
        &self,
        state: &WorkspaceRecoverySidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.workspace_recovery_sidecars_path(), state)
    }

    /// 审计/用量账本的追加写段目录（见 docs/durable-log-compaction-design.md §1）。
    pub(crate) fn audit_usage_ledger_path(&self) -> PathBuf {
        self.state_root.join("audit-usage-ledger")
    }

    pub(crate) fn load_audit_usage_ledger(&self) -> Result<AuditUsageLedgerSnapshot, DaemonError> {
        AuditUsageLedgerSnapshot::load_from_dir(
            &self.audit_usage_ledger_path(),
            magi_core::UtcMillis::now(),
        )
        .map_err(|error| {
            DaemonError::internal(format!(
                "审计/用量账本损坏，拒绝以空账本继续 {}: {error}",
                self.audit_usage_ledger_path().display()
            ))
        })
    }

    pub(crate) fn knowledge_state_path(&self) -> PathBuf {
        self.state_root.join("knowledge.json")
    }

    pub(crate) fn load_knowledge_state(&self) -> Result<KnowledgeState, DaemonError> {
        self.read_json_or_default(self.knowledge_state_path())
    }
}
