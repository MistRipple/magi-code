use magi_core::{
    AbsolutePath, DomainError, DomainResult, ExecutionOwnership, UtcMillis, WorkspaceId,
    WorkspaceLifecycleStatus,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub workspace_id: WorkspaceId,
    pub name: Option<String>,
    pub root_path: AbsolutePath,
    pub worktree_root: Option<AbsolutePath>,
    pub status: WorkspaceLifecycleStatus,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeAllocation {
    pub allocation_id: String,
    pub workspace_id: WorkspaceId,
    pub ownership: ExecutionOwnership,
    pub worktree_root: AbsolutePath,
    pub active: bool,
    pub created_at: UtcMillis,
    pub released_at: Option<UtcMillis>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub snapshot_id: String,
    pub workspace_id: WorkspaceId,
    pub ownership: ExecutionOwnership,
    pub label: String,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryStatus {
    Prepared,
    Ready,
    Consumed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryHandle {
    pub recovery_id: String,
    pub workspace_id: WorkspaceId,
    pub ownership: ExecutionOwnership,
    pub snapshot_id: String,
    pub diagnostic_summary: Option<String>,
    pub status: RecoveryStatus,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
    pub consumed_at: Option<UtcMillis>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceRecoverySidecarExport {
    pub recovery_ref: String,
    pub workspace_id: WorkspaceId,
    #[serde(alias = "status")]
    pub current_status: RecoveryStatus,
    #[serde(alias = "updated_at")]
    pub last_update: UtcMillis,
    pub ownership: ExecutionOwnership,
    pub execution_chain_ref: Option<String>,
    pub snapshot_id: String,
    pub diagnostic_summary: Option<String>,
    pub consumed_at: Option<UtcMillis>,
}

impl RecoveryHandle {
    pub fn export_view(&self) -> WorkspaceRecoverySidecarExport {
        WorkspaceRecoverySidecarExport {
            recovery_ref: self.recovery_id.clone(),
            workspace_id: self.workspace_id.clone(),
            current_status: self.status.clone(),
            last_update: self.updated_at,
            ownership: self.ownership.clone(),
            execution_chain_ref: self.ownership.execution_chain_ref.clone(),
            snapshot_id: self.snapshot_id.clone(),
            diagnostic_summary: self.diagnostic_summary.clone(),
            consumed_at: self.consumed_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceRecoveryFlushReason {
    PrepareRecoveryEntry,
    MarkRecoveryReady,
    ConsumeRecovery,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRecoveryFlushMetadata {
    pub current_version: u64,
    pub flushed_version: u64,
    pub last_dirty_at: Option<UtcMillis>,
    pub last_dirty_reason: Option<WorkspaceRecoveryFlushReason>,
    pub last_flush_at: Option<UtcMillis>,
    pub next_flush_hint: Option<UtcMillis>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceRecoverySidecarStoreState {
    pub recovery_handles: Vec<RecoveryHandle>,
}

impl WorkspaceRecoverySidecarStoreState {
    fn sort_recovery_handles(recovery_handles: &mut Vec<RecoveryHandle>) {
        recovery_handles.sort_by(|left, right| {
            left.workspace_id
                .as_str()
                .cmp(right.workspace_id.as_str())
                .then_with(|| left.created_at.0.cmp(&right.created_at.0))
                .then_with(|| left.recovery_id.cmp(&right.recovery_id))
        });
    }

    pub fn upsert_recovery_handle(&mut self, handle: RecoveryHandle) {
        if let Some(existing) = self
            .recovery_handles
            .iter_mut()
            .find(|existing| existing.recovery_id == handle.recovery_id)
        {
            *existing = handle;
        } else {
            self.recovery_handles.push(handle);
        }
        Self::sort_recovery_handles(&mut self.recovery_handles);
    }

    pub fn recovery_handle(&self, recovery_id: &str) -> Option<RecoveryHandle> {
        self.recovery_handles
            .iter()
            .find(|handle| handle.recovery_id == recovery_id)
            .cloned()
    }

    pub fn recovery_handles(&self) -> Vec<RecoveryHandle> {
        let mut recovery_handles = self.recovery_handles.clone();
        Self::sort_recovery_handles(&mut recovery_handles);
        recovery_handles
    }

    pub fn recovery_handles_for_workspace(&self, workspace_id: &WorkspaceId) -> Vec<RecoveryHandle> {
        let mut recovery_handles = self
            .recovery_handles()
            .into_iter()
            .filter(|handle| &handle.workspace_id == workspace_id)
            .collect::<Vec<_>>();
        Self::sort_recovery_handles(&mut recovery_handles);
        recovery_handles
    }

    pub fn export_views(&self) -> Vec<WorkspaceRecoverySidecarExport> {
        let mut exports = self
            .recovery_handles()
            .into_iter()
            .map(|handle| handle.export_view())
            .collect::<Vec<_>>();
        exports.sort_by(|left, right| {
            left.workspace_id
                .as_str()
                .cmp(right.workspace_id.as_str())
                .then_with(|| left.last_update.0.cmp(&right.last_update.0))
                .then_with(|| left.recovery_ref.cmp(&right.recovery_ref))
        });
        exports
    }

    pub fn recovery_handle_mut(&mut self, recovery_id: &str) -> Option<&mut RecoveryHandle> {
        self.recovery_handles
            .iter_mut()
            .find(|handle| handle.recovery_id == recovery_id)
    }

    pub fn mark_recovery_ready(&mut self, recovery_id: &str) -> DomainResult<RecoveryHandle> {
        let handle = self
            .recovery_handle_mut(recovery_id)
            .ok_or(DomainError::NotFound {
                entity: "recovery_handle",
            })?;
        if matches!(handle.status, RecoveryStatus::Consumed) {
            return Err(DomainError::InvalidState {
                message: format!("recovery_handle {recovery_id} 已被消费，不能再次标记为 Ready"),
            });
        }
        handle.status = RecoveryStatus::Ready;
        handle.updated_at = UtcMillis::now();
        Ok(handle.clone())
    }

    pub fn consume_recovery(&mut self, recovery_id: &str) -> DomainResult<RecoveryHandle> {
        let ownership = self
            .recovery_handle(recovery_id)
            .map(|handle| handle.ownership.clone())
            .ok_or(DomainError::NotFound {
                entity: "recovery_handle",
            })?;
        self.consume_recovery_with_ownership(recovery_id, ownership)
    }

    pub fn consume_recovery_with_ownership(
        &mut self,
        recovery_id: &str,
        ownership: ExecutionOwnership,
    ) -> DomainResult<RecoveryHandle> {
        let updated = {
            let handle = self
                .recovery_handle_mut(recovery_id)
                .ok_or(DomainError::NotFound {
                    entity: "recovery_handle",
                })?;
            match handle.status {
                RecoveryStatus::Ready => {}
                RecoveryStatus::Prepared => {
                    return Err(DomainError::InvalidState {
                        message: format!("recovery_handle {recovery_id} 还未进入 Ready，不能消费"),
                    })
                }
                RecoveryStatus::Consumed => {
                    return Err(DomainError::InvalidState {
                        message: format!("recovery_handle {recovery_id} 已被消费"),
                    })
                }
            }
            handle.ownership = ExecutionOwnership {
                session_id: ownership.session_id.or(handle.ownership.session_id.clone()),
                workspace_id: ownership
                    .workspace_id
                    .or_else(|| Some(handle.workspace_id.clone())),
                mission_id: ownership.mission_id.or(handle.ownership.mission_id.clone()),
                task_id: ownership.task_id.or(handle.ownership.task_id.clone()),
                worker_id: ownership.worker_id.or(handle.ownership.worker_id.clone()),
                execution_chain_ref: ownership
                    .execution_chain_ref
                    .or(handle.ownership.execution_chain_ref.clone()),
            };
            handle.status = RecoveryStatus::Consumed;
            handle.updated_at = UtcMillis::now();
            handle.consumed_at = Some(UtcMillis::now());
            handle.clone()
        };
        Ok(updated)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceStoreState {
    pub active_workspace_id: Option<WorkspaceId>,
    pub workspaces: Vec<WorkspaceRecord>,
    pub worktree_allocations: Vec<WorktreeAllocation>,
    pub snapshots: Vec<SnapshotRecord>,
    #[serde(default, flatten)]
    pub recovery_sidecar_store: WorkspaceRecoverySidecarStoreState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceDurableState {
    pub active_workspace_id: Option<WorkspaceId>,
    pub workspaces: Vec<WorkspaceRecord>,
    pub worktree_allocations: Vec<WorktreeAllocation>,
    pub snapshots: Vec<SnapshotRecord>,
}

impl WorkspaceStoreState {
    pub fn from_persisted_parts(
        durable_state: WorkspaceDurableState,
        recovery_sidecar_store: WorkspaceRecoverySidecarStoreState,
    ) -> Self {
        Self {
            active_workspace_id: durable_state.active_workspace_id,
            workspaces: durable_state.workspaces,
            worktree_allocations: durable_state.worktree_allocations,
            snapshots: durable_state.snapshots,
            recovery_sidecar_store,
        }
    }

    pub fn durable_state(&self) -> WorkspaceDurableState {
        WorkspaceDurableState {
            active_workspace_id: self.active_workspace_id.clone(),
            workspaces: self.workspaces.clone(),
            worktree_allocations: self.worktree_allocations.clone(),
            snapshots: self.snapshots.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceProjectionInput {
    pub active_workspace_id: Option<WorkspaceId>,
    pub workspaces: Vec<WorkspaceRecord>,
    pub worktree_allocations: Vec<WorktreeAllocation>,
    pub snapshots: Vec<SnapshotRecord>,
    pub recovery_handles: Vec<RecoveryHandle>,
}
