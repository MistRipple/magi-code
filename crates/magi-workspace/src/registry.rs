mod recovery;
mod worktree;

use crate::models::{
    SnapshotRecord, WorkspaceDurableState, WorkspaceProjectionInput, WorkspaceRecord,
    WorkspaceRecoveryFlushReason, WorkspaceRecoverySidecarStoreState, WorkspaceSourceFolder,
    WorkspaceStoreState, WorktreeAllocation,
};
use magi_core::{
    AbsolutePath, DomainError, DomainResult, ExecutionOwnership, UtcMillis, WorkspaceId,
    WorkspaceLifecycleStatus,
};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

#[derive(Clone, Debug, Default)]
pub(super) struct RecoverySidecarFlushState {
    current_version: u64,
    flushed_version: u64,
    last_dirty_at: Option<UtcMillis>,
    last_dirty_reason: Option<WorkspaceRecoveryFlushReason>,
    last_flush_at: Option<UtcMillis>,
    next_flush_hint: Option<UtcMillis>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceStore {
    state: Arc<RwLock<WorkspaceStoreState>>,
    recovery_sidecar_flush_state: Arc<RwLock<RecoverySidecarFlushState>>,
}

impl Default for WorkspaceStore {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(WorkspaceStoreState::default())),
            recovery_sidecar_flush_state: Arc::new(RwLock::new(
                RecoverySidecarFlushState::default(),
            )),
        }
    }
}

impl WorkspaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_state(state: WorkspaceStoreState) -> Self {
        Self {
            state: Arc::new(RwLock::new(state)),
            recovery_sidecar_flush_state: Arc::new(RwLock::new(
                RecoverySidecarFlushState::default(),
            )),
        }
    }

    pub(super) fn read_state(&self) -> RwLockReadGuard<'_, WorkspaceStoreState> {
        self.state
            .read()
            .expect("workspace state read lock poisoned")
    }

    pub(super) fn write_state(&self) -> RwLockWriteGuard<'_, WorkspaceStoreState> {
        self.state
            .write()
            .expect("workspace state write lock poisoned")
    }

    pub(super) fn read_flush_state(&self) -> RwLockReadGuard<'_, RecoverySidecarFlushState> {
        self.recovery_sidecar_flush_state
            .read()
            .expect("workspace sidecar flush state read lock poisoned")
    }

    pub(super) fn write_flush_state(&self) -> RwLockWriteGuard<'_, RecoverySidecarFlushState> {
        self.recovery_sidecar_flush_state
            .write()
            .expect("workspace sidecar flush state write lock poisoned")
    }

    pub(super) fn sort_worktree_allocations(allocations: &mut [WorktreeAllocation]) {
        allocations.sort_by(|left, right| {
            left.workspace_id
                .as_str()
                .cmp(right.workspace_id.as_str())
                .then_with(|| left.created_at.0.cmp(&right.created_at.0))
                .then_with(|| left.allocation_id.cmp(&right.allocation_id))
        });
    }

    fn sort_snapshots(snapshots: &mut [SnapshotRecord]) {
        snapshots.sort_by(|left, right| {
            left.workspace_id
                .as_str()
                .cmp(right.workspace_id.as_str())
                .then_with(|| left.created_at.0.cmp(&right.created_at.0))
                .then_with(|| left.snapshot_id.cmp(&right.snapshot_id))
        });
    }

    pub fn from_persisted_parts(
        durable_state: WorkspaceDurableState,
        recovery_sidecar_store: WorkspaceRecoverySidecarStoreState,
    ) -> Self {
        Self::from_state(WorkspaceStoreState::from_persisted_parts(
            durable_state,
            recovery_sidecar_store,
        ))
    }

    pub fn export_state(&self) -> WorkspaceStoreState {
        self.read_state().clone()
    }

    pub fn durable_state(&self) -> WorkspaceDurableState {
        self.export_state().durable_state()
    }

    /// 导航等非管理入口保存注册表时也保持读锁，避免旧快照晚于项目管理事务落盘。
    pub fn with_durable_state<R>(&self, persist: impl FnOnce(&WorkspaceDurableState) -> R) -> R {
        let state = self.read_state();
        let result = persist(&state.durable_state());
        drop(state);
        result
    }

    pub fn projection_input(&self) -> WorkspaceProjectionInput {
        let mut state = self.export_state();
        state
            .workspaces
            .sort_by(|left, right| left.workspace_id.as_str().cmp(right.workspace_id.as_str()));
        Self::sort_worktree_allocations(&mut state.worktree_allocations);
        Self::sort_snapshots(&mut state.snapshots);
        WorkspaceProjectionInput {
            active_workspace_id: state.active_workspace_id,
            workspaces: state.workspaces,
            worktree_allocations: state.worktree_allocations,
            snapshots: state.snapshots,
            recovery_handles: state.recovery_sidecar_store.recovery_handles(),
        }
    }

    pub fn workspace_roots(&self) -> Vec<AbsolutePath> {
        let mut roots = self
            .read_state()
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .source_folders
                    .iter()
                    .map(|folder| folder.path.clone())
            })
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| left.0.cmp(&right.0));
        roots
    }

    pub fn register(
        &self,
        workspace_id: WorkspaceId,
        root_path: AbsolutePath,
    ) -> DomainResult<WorkspaceRecord> {
        self.register_native_path(workspace_id, PathBuf::from(root_path.as_str()))
    }

    pub fn register_native_path(
        &self,
        workspace_id: WorkspaceId,
        root_path: PathBuf,
    ) -> DomainResult<WorkspaceRecord> {
        self.register_native_path_with_persistence(workspace_id, root_path, |_| Ok(()))
    }

    pub fn register_native_path_with_persistence(
        &self,
        workspace_id: WorkspaceId,
        root_path: PathBuf,
        persist: impl FnOnce(&WorkspaceDurableState) -> DomainResult<()>,
    ) -> DomainResult<WorkspaceRecord> {
        let host_path = magi_core::HostPath::canonicalize(&root_path)
            .unwrap_or_else(|_| magi_core::HostPath::from_path(root_path));
        let display_path = AbsolutePath::new(host_path.display_string());
        let root_path_ref = host_path.to_path_ref().as_str().to_string();
        self.register_with_path_ref(workspace_id, display_path, root_path_ref, persist)
    }

    fn register_with_path_ref(
        &self,
        workspace_id: WorkspaceId,
        root_path: AbsolutePath,
        root_path_ref: String,
        persist: impl FnOnce(&WorkspaceDurableState) -> DomainResult<()>,
    ) -> DomainResult<WorkspaceRecord> {
        let now = UtcMillis::now();
        let workspace = WorkspaceRecord {
            workspace_id: workspace_id.clone(),
            name: None,
            worktree_root: None,
            status: WorkspaceLifecycleStatus::Registered,
            created_at: now,
            updated_at: now,
            source_folders: vec![WorkspaceSourceFolder {
                source_folder_id: format!("{workspace_id}-source-0"),
                path: root_path,
                path_ref: root_path_ref,
                name: None,
                created_at: now,
                updated_at: now,
            }],
            primary_source_folder_id: format!("{workspace_id}-source-0"),
            revision: 1,
        };
        self.register_project(workspace, persist)
    }

    pub fn activate(&self, workspace_id: &WorkspaceId) -> DomainResult<WorkspaceRecord> {
        let mut state = self.write_state();
        let workspace = state
            .workspaces
            .iter_mut()
            .find(|workspace| &workspace.workspace_id == workspace_id)
            .ok_or(DomainError::NotFound {
                entity: "workspace",
            })?;
        workspace.status = WorkspaceLifecycleStatus::Active;
        workspace.updated_at = UtcMillis::now();
        let updated = workspace.clone();
        state.active_workspace_id = Some(workspace_id.clone());
        Ok(updated)
    }

    pub fn append_execution_snapshot(
        &self,
        workspace_id: WorkspaceId,
        mut ownership: ExecutionOwnership,
        snapshot_id: impl Into<String>,
        label: impl Into<String>,
    ) -> SnapshotRecord {
        ownership.workspace_id = Some(workspace_id.clone());
        let snapshot = SnapshotRecord {
            snapshot_id: snapshot_id.into(),
            workspace_id,
            ownership,
            label: label.into(),
            created_at: UtcMillis::now(),
        };
        self.write_state().snapshots.push(snapshot.clone());
        snapshot
    }

    pub fn workspaces(&self) -> Vec<WorkspaceRecord> {
        let mut workspaces = self.read_state().workspaces.clone();
        workspaces
            .sort_by(|left, right| left.workspace_id.as_str().cmp(right.workspace_id.as_str()));
        workspaces
    }

    pub fn active_workspace_id(&self) -> Option<WorkspaceId> {
        self.read_state().active_workspace_id.clone()
    }

    pub fn snapshots(&self) -> Vec<SnapshotRecord> {
        let mut snapshots = self.read_state().snapshots.clone();
        Self::sort_snapshots(&mut snapshots);
        snapshots
    }

    pub fn is_empty(&self) -> bool {
        self.read_state().workspaces.is_empty()
    }

    pub fn deregister(&self, workspace_id: &WorkspaceId) -> DomainResult<()> {
        self.deregister_with_persistence(workspace_id, |_| Ok(()))
    }

    pub fn deregister_with_persistence(
        &self,
        workspace_id: &WorkspaceId,
        persist: impl FnOnce(&WorkspaceDurableState) -> DomainResult<()>,
    ) -> DomainResult<()> {
        let mut state = self.write_state();
        let pos = state
            .workspaces
            .iter()
            .position(|project| &project.workspace_id == workspace_id)
            .ok_or(DomainError::NotFound {
                entity: "workspace",
            })?;
        let mut next = state.durable_state();
        next.workspaces.remove(pos);
        if next.active_workspace_id.as_ref() == Some(workspace_id) {
            next.active_workspace_id = next
                .workspaces
                .first()
                .map(|project| project.workspace_id.clone());
        }
        persist(&next)?;
        state.active_workspace_id = next.active_workspace_id;
        state.workspaces = next.workspaces;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
