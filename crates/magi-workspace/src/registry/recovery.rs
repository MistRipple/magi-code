use super::{RecoverySidecarFlushState, WorkspaceStore};
use crate::models::{
    RecoveryHandle, RecoveryStatus, WorkspaceRecoveryFlushMetadata,
    WorkspaceRecoveryFlushReason, WorkspaceRecoverySidecarExport,
    WorkspaceRecoverySidecarStoreState,
};
use magi_core::{
    DomainError, DomainResult, ExecutionOwnership, RecoveryResumeInput, UtcMillis, WorkspaceId,
};

fn flush_metadata(flush_state: &RecoverySidecarFlushState) -> WorkspaceRecoveryFlushMetadata {
    WorkspaceRecoveryFlushMetadata {
        current_version: flush_state.current_version,
        flushed_version: flush_state.flushed_version,
        last_dirty_at: flush_state.last_dirty_at,
        last_dirty_reason: flush_state.last_dirty_reason.clone(),
        last_flush_at: flush_state.last_flush_at,
        next_flush_hint: if flush_state.current_version == flush_state.flushed_version {
            None
        } else {
            flush_state.next_flush_hint.or(flush_state.last_dirty_at)
        },
    }
}

impl WorkspaceStore {
    pub fn ensure_recovery_ready(&self, recovery_id: &str) -> DomainResult<()> {
        let handle = self
            .read_state()
            .recovery_sidecar_store
            .recovery_handle(recovery_id)
            .ok_or(DomainError::NotFound {
                entity: "recovery_handle",
            })?;
        match handle.status {
            RecoveryStatus::Ready => Ok(()),
            RecoveryStatus::Prepared => Err(DomainError::InvalidState {
                message: format!("recovery_handle {recovery_id} 还未进入 Ready，不能构建恢复输入"),
            }),
            RecoveryStatus::Consumed => Err(DomainError::InvalidState {
                message: format!("recovery_handle {recovery_id} 已被消费，不能再构建恢复输入"),
            }),
        }
    }

    fn mark_recovery_sidecar_dirty(&self, reason: WorkspaceRecoveryFlushReason) {
        let mut flush_state = self.write_flush_state();
        flush_state.current_version = flush_state.current_version.saturating_add(1);
        let now = UtcMillis::now();
        flush_state.last_dirty_at = Some(now);
        flush_state.last_dirty_reason = Some(reason);
        flush_state.next_flush_hint = Some(now);
    }

    pub fn register_recovery_handle(
        &self,
        workspace_id: WorkspaceId,
        snapshot_id: impl Into<String>,
        recovery_id: impl Into<String>,
    ) -> RecoveryHandle {
        self.prepare_recovery_entry(
            workspace_id,
            ExecutionOwnership::default(),
            snapshot_id,
            recovery_id,
            None,
        )
    }

    pub fn prepare_recovery_entry(
        &self,
        workspace_id: WorkspaceId,
        mut ownership: ExecutionOwnership,
        snapshot_id: impl Into<String>,
        recovery_id: impl Into<String>,
        diagnostic_summary: Option<String>,
    ) -> RecoveryHandle {
        let now = UtcMillis::now();
        let handle = RecoveryHandle {
            recovery_id: recovery_id.into(),
            workspace_id: workspace_id.clone(),
            ownership: {
                ownership.workspace_id = Some(workspace_id);
                ownership
            },
            snapshot_id: snapshot_id.into(),
            diagnostic_summary,
            status: RecoveryStatus::Prepared,
            created_at: now,
            updated_at: now,
            consumed_at: None,
        };
        self.write_state()
            .recovery_sidecar_store
            .upsert_recovery_handle(handle.clone());
        self.mark_recovery_sidecar_dirty(WorkspaceRecoveryFlushReason::PrepareRecoveryEntry);
        handle
    }

    pub fn mark_recovery_ready(&self, recovery_id: &str) -> DomainResult<RecoveryHandle> {
        let updated = self
            .write_state()
            .recovery_sidecar_store
            .mark_recovery_ready(recovery_id)?;
        self.mark_recovery_sidecar_dirty(WorkspaceRecoveryFlushReason::MarkRecoveryReady);
        Ok(updated)
    }

    pub fn consume_recovery(&self, recovery_id: &str) -> DomainResult<RecoveryHandle> {
        let updated = self
            .write_state()
            .recovery_sidecar_store
            .consume_recovery(recovery_id)?;
        self.mark_recovery_sidecar_dirty(WorkspaceRecoveryFlushReason::ConsumeRecovery);
        Ok(updated)
    }

    pub fn consume_recovery_with_ownership(
        &self,
        recovery_id: &str,
        ownership: ExecutionOwnership,
    ) -> DomainResult<RecoveryHandle> {
        let updated = self
            .write_state()
            .recovery_sidecar_store
            .consume_recovery_with_ownership(recovery_id, ownership)?;
        self.mark_recovery_sidecar_dirty(WorkspaceRecoveryFlushReason::ConsumeRecovery);
        Ok(updated)
    }

    pub fn recovery_entry_points(&self, workspace_id: &WorkspaceId) -> Vec<RecoveryHandle> {
        self.read_state()
            .recovery_sidecar_store
            .recovery_handles_for_workspace(workspace_id)
    }

    pub fn active_recovery_handles(&self, workspace_id: &WorkspaceId) -> Vec<RecoveryHandle> {
        self.recovery_entry_points(workspace_id)
            .into_iter()
            .filter(|handle| !matches!(handle.status, RecoveryStatus::Consumed))
            .collect()
    }

    pub fn build_recovery_resume_input(
        &self,
        recovery_id: &str,
    ) -> DomainResult<RecoveryResumeInput> {
        self.ensure_recovery_ready(recovery_id)?;
        let handle = self
            .read_state()
            .recovery_sidecar_store
            .recovery_handle(recovery_id)
            .ok_or(DomainError::NotFound {
                entity: "recovery_handle",
            })?;

        Ok(RecoveryResumeInput {
            recovery_id: handle.recovery_id,
            snapshot_id: handle.snapshot_id,
            ownership: handle.ownership,
            diagnostic_summary: handle.diagnostic_summary,
            created_at: handle.created_at,
            updated_at: handle.updated_at,
        })
    }

    pub fn recovery_handles(&self) -> Vec<RecoveryHandle> {
        self.read_state().recovery_sidecar_store.recovery_handles()
    }

    pub fn recovery_sidecar_exports(&self) -> Vec<WorkspaceRecoverySidecarExport> {
        self.read_state().recovery_sidecar_store.export_views()
    }

    pub fn recovery_sidecar_export(
        &self,
        recovery_id: &str,
    ) -> Option<WorkspaceRecoverySidecarExport> {
        self.read_state()
            .recovery_sidecar_store
            .recovery_handle(recovery_id)
            .map(|handle| handle.export_view())
    }

    pub fn active_recovery_handles_all(&self) -> Vec<RecoveryHandle> {
        self.recovery_handles()
            .into_iter()
            .filter(|handle| !matches!(handle.status, RecoveryStatus::Consumed))
            .collect()
    }

    pub fn recovery_sidecar_store_state(&self) -> WorkspaceRecoverySidecarStoreState {
        self.read_state().recovery_sidecar_store.clone()
    }

    pub fn recovery_sidecar_flush_metadata(&self) -> WorkspaceRecoveryFlushMetadata {
        let flush_state = self.read_flush_state();
        flush_metadata(&flush_state)
    }

    pub fn flush_recovery_sidecars_with<E, F>(&self, persist: F) -> Result<bool, E>
    where
        F: FnOnce(&WorkspaceRecoverySidecarStoreState) -> Result<(), E>,
    {
        let version = {
            let flush_state = self.read_flush_state();
            if flush_state.current_version == flush_state.flushed_version {
                return Ok(false);
            }
            flush_state.current_version
        };

        let snapshot = self.recovery_sidecar_store_state();
        persist(&snapshot)?;

        let mut flush_state = self.write_flush_state();
        flush_state.flushed_version = flush_state.flushed_version.max(version);
        let now = UtcMillis::now();
        flush_state.last_flush_at = Some(now);
        if flush_state.current_version == flush_state.flushed_version {
            flush_state.next_flush_hint = None;
        } else if flush_state.next_flush_hint.is_none() {
            flush_state.next_flush_hint = flush_state.last_dirty_at.or(Some(now));
        }
        Ok(true)
    }
}
