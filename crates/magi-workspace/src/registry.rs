mod recovery;
mod worktree;

use crate::models::{
    SnapshotRecord, WorkspaceDurableState, WorkspaceProjectionInput, WorkspaceRecord,
    WorkspaceRecoveryFlushReason, WorkspaceRecoverySidecarStoreState, WorkspaceStoreState,
    WorktreeAllocation,
};
use magi_core::{
    AbsolutePath, DomainError, DomainResult, ExecutionOwnership, UtcMillis, WorkspaceId,
    WorkspaceLifecycleStatus,
};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

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

    pub(super) fn sort_worktree_allocations(allocations: &mut Vec<WorktreeAllocation>) {
        allocations.sort_by(|left, right| {
            left.workspace_id
                .as_str()
                .cmp(right.workspace_id.as_str())
                .then_with(|| left.created_at.0.cmp(&right.created_at.0))
                .then_with(|| left.allocation_id.cmp(&right.allocation_id))
        });
    }

    fn sort_snapshots(snapshots: &mut Vec<SnapshotRecord>) {
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
            .map(|workspace| workspace.root_path.clone())
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| left.0.cmp(&right.0));
        roots
    }

    pub fn register(
        &self,
        workspace_id: WorkspaceId,
        root_path: AbsolutePath,
    ) -> DomainResult<WorkspaceRecord> {
        let mut state = self.write_state();
        if state
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == workspace_id)
        {
            return Err(DomainError::AlreadyExists {
                entity: "workspace",
            });
        }
        if state
            .workspaces
            .iter()
            .any(|workspace| workspace.root_path == root_path)
        {
            return Err(DomainError::AlreadyExists {
                entity: "workspace_root_path",
            });
        }

        let now = UtcMillis::now();
        let workspace = WorkspaceRecord {
            workspace_id: workspace_id.clone(),
            name: None,
            root_path,
            worktree_root: None,
            status: WorkspaceLifecycleStatus::Registered,
            created_at: now,
            updated_at: now,
        };
        state.workspaces.push(workspace.clone());
        if state.active_workspace_id.is_none() {
            state.active_workspace_id = Some(workspace_id);
        }
        Ok(workspace)
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

    pub fn append_snapshot(
        &self,
        workspace_id: WorkspaceId,
        snapshot_id: impl Into<String>,
        mission_id: Option<magi_core::MissionId>,
        label: impl Into<String>,
    ) -> SnapshotRecord {
        let ownership = ExecutionOwnership {
            mission_id,
            workspace_id: Some(workspace_id.clone()),
            ..ExecutionOwnership::default()
        };
        self.append_execution_snapshot(workspace_id, ownership, snapshot_id, label)
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

    pub fn resolve_snapshot(&self, snapshot_id: &str) -> Option<SnapshotRecord> {
        self.read_state()
            .snapshots
            .iter()
            .find(|snapshot| snapshot.snapshot_id == snapshot_id)
            .cloned()
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
        let mut state = self.write_state();
        let pos = state
            .workspaces
            .iter()
            .position(|w| &w.workspace_id == workspace_id)
            .ok_or(DomainError::NotFound {
                entity: "workspace",
            })?;
        state.workspaces.remove(pos);
        if state.active_workspace_id.as_ref() == Some(workspace_id) {
            state.active_workspace_id = state.workspaces.first().map(|w| w.workspace_id.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RecoveryStatus;
    use magi_core::{AbsolutePath, ExecutionOwnership, SessionId, WorkspaceId};
    use serde_json::json;

    #[test]
    fn recovery_sidecar_store_keeps_status_and_round_trips() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-1");
        store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("workspace should be registrable");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-1")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-1".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-1",
            "snapshot label",
        );

        let recovery = store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-1")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-1".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-1",
            Some("diagnostic".to_string()),
        );
        assert_eq!(recovery.status, RecoveryStatus::Prepared);
        assert_eq!(
            store
                .mark_recovery_ready(&recovery.recovery_id)
                .expect("ready")
                .status,
            RecoveryStatus::Ready
        );
        assert_eq!(
            store
                .consume_recovery(&recovery.recovery_id)
                .expect("consume")
                .status,
            RecoveryStatus::Consumed
        );

        let state = store.export_state();
        let roundtrip: WorkspaceStoreState =
            serde_json::from_str(&serde_json::to_string(&state).expect("serialize state"))
                .expect("deserialize state");
        assert_eq!(roundtrip.recovery_sidecar_store.recovery_handles.len(), 1);
        assert_eq!(
            roundtrip
                .recovery_sidecar_store
                .recovery_handles
                .first()
                .map(|handle| handle.status.clone()),
            Some(RecoveryStatus::Consumed)
        );
    }

    #[test]
    fn recovery_sidecar_flush_metadata_tracks_recovery_lifecycle() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-metadata");
        store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("workspace should be registrable");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-metadata")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-metadata".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-metadata",
            "metadata snapshot",
        );
        store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-metadata")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-metadata".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-metadata",
            Some("diagnostic metadata".to_string()),
        );
        let prepared_metadata = store.recovery_sidecar_flush_metadata();
        assert_eq!(prepared_metadata.current_version, 1);
        assert_eq!(
            prepared_metadata.last_dirty_reason,
            Some(WorkspaceRecoveryFlushReason::PrepareRecoveryEntry)
        );
        assert!(prepared_metadata.last_dirty_at.is_some());
        assert_eq!(
            prepared_metadata.next_flush_hint,
            prepared_metadata.last_dirty_at
        );

        let recovery = store
            .mark_recovery_ready("recovery-metadata")
            .expect("recovery should become ready");
        assert_eq!(recovery.status, RecoveryStatus::Ready);
        let ready_metadata = store.recovery_sidecar_flush_metadata();
        assert_eq!(ready_metadata.current_version, 2);
        assert_eq!(
            ready_metadata.last_dirty_reason,
            Some(WorkspaceRecoveryFlushReason::MarkRecoveryReady)
        );

        let consumed = store
            .consume_recovery("recovery-metadata")
            .expect("recovery should be consumable");
        assert_eq!(consumed.status, RecoveryStatus::Consumed);
        let consumed_metadata = store.recovery_sidecar_flush_metadata();
        assert_eq!(consumed_metadata.current_version, 3);
        assert_eq!(
            consumed_metadata.last_dirty_reason,
            Some(WorkspaceRecoveryFlushReason::ConsumeRecovery)
        );
        assert!(consumed_metadata.last_dirty_at.is_some());
        assert_eq!(
            consumed_metadata.next_flush_hint,
            consumed_metadata.last_dirty_at
        );

        let mut flushes = Vec::new();
        assert!(
            store
                .flush_recovery_sidecars_with(|state| {
                    flushes.push(state.recovery_handles.len());
                    Ok::<_, std::io::Error>(())
                })
                .expect("dirty recovery sidecar flush should succeed")
        );
        assert_eq!(flushes, vec![1]);
        let flushed_metadata = store.recovery_sidecar_flush_metadata();
        assert_eq!(
            flushed_metadata.current_version,
            flushed_metadata.flushed_version
        );
        assert!(flushed_metadata.last_flush_at.is_some());
        assert_eq!(flushed_metadata.next_flush_hint, None);
    }

    #[test]
    fn legacy_top_level_recovery_handles_deserialize() {
        let payload = json!({
            "active_workspace_id": null,
            "workspaces": [],
            "worktree_allocations": [],
            "snapshots": [],
            "recovery_handles": [{
                "recovery_id": "recovery-legacy",
                "workspace_id": "workspace-legacy",
                "ownership": {
                    "session_id": null,
                    "workspace_id": "workspace-legacy",
                    "mission_id": null,
                    "task_id": null,
                    "worker_id": null,
                    "execution_chain_ref": "chain-legacy"
                },
                "snapshot_id": "snapshot-legacy",
                "diagnostic_summary": "legacy",
                "status": "Ready",
                "created_at": 1,
                "updated_at": 2,
                "consumed_at": null
            }]
        });

        let state: WorkspaceStoreState =
            serde_json::from_value(payload).expect("legacy payload should deserialize");
        let handle = state
            .recovery_sidecar_store
            .recovery_handles
            .first()
            .expect("recovery handle should exist");
        assert_eq!(handle.recovery_id, "recovery-legacy");
        assert_eq!(handle.status, RecoveryStatus::Ready);
    }

    #[test]
    fn register_rejects_duplicate_root_path_with_different_workspace_id() {
        let store = WorkspaceStore::new();
        store
            .register(
                WorkspaceId::new("workspace-original"),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("first workspace should register");

        let error = store
            .register(
                WorkspaceId::new("workspace-duplicate"),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect_err("same root path must not create another workspace identity");

        assert!(matches!(
            error,
            DomainError::AlreadyExists {
                entity: "workspace_root_path"
            }
        ));
        assert_eq!(store.workspaces().len(), 1);
    }

    #[test]
    fn recovery_sidecar_export_exposes_stable_fields() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-1");
        store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("workspace should be registrable");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-1")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-1".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-1",
            "snapshot label",
        );
        let recovery = store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-1")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-1".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-1",
            Some("diagnostic".to_string()),
        );
        let export = store
            .recovery_sidecar_export(&recovery.recovery_id)
            .expect("recovery export should exist");
        assert_eq!(export.recovery_ref, "recovery-1");
        assert_eq!(export.current_status, RecoveryStatus::Prepared);
        assert_eq!(export.last_update, recovery.updated_at);
        assert_eq!(export.execution_chain_ref.as_deref(), Some("chain-1"));
        assert_eq!(
            export.ownership.execution_chain_ref.as_deref(),
            Some("chain-1")
        );
    }

    #[test]
    fn consume_recovery_with_ownership_updates_exported_runtime_fields() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-consume-runtime");
        store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("workspace should be registrable");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-consume-runtime")),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(magi_core::MissionId::new("mission-original")),
                execution_chain_ref: Some("chain-original".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-consume-runtime",
            "snapshot label",
        );
        let recovery = store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-consume-runtime")),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(magi_core::MissionId::new("mission-original")),
                execution_chain_ref: Some("chain-original".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-consume-runtime",
            Some("diagnostic".to_string()),
        );
        store
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        store
            .consume_recovery_with_ownership(
                &recovery.recovery_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("session-consume-runtime")),
                    workspace_id: Some(workspace_id),
                    mission_id: Some(magi_core::MissionId::new("mission-updated")),
                    task_id: Some(magi_core::TaskId::new("task-updated")),
                    worker_id: Some(magi_core::WorkerId::new("worker-updated")),
                    execution_chain_ref: Some("chain-updated".to_string()),
                },
            )
            .expect("consuming with resolved ownership should succeed");

        let export = store
            .recovery_sidecar_export("recovery-consume-runtime")
            .expect("recovery export should exist");
        assert_eq!(export.current_status, RecoveryStatus::Consumed);
        assert_eq!(
            export
                .ownership
                .mission_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("mission-updated")
        );
        assert_eq!(
            export
                .ownership
                .task_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("task-updated")
        );
        assert_eq!(
            export
                .ownership
                .worker_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("worker-updated")
        );
        assert_eq!(export.execution_chain_ref.as_deref(), Some("chain-updated"));
    }

    #[test]
    fn persisted_parts_round_trip_preserves_recovery_sidecars() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-persisted");
        store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("workspace should be registrable");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-persisted")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-persisted".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-persisted",
            "persisted snapshot",
        );
        let recovery = store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-persisted")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-persisted".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-persisted",
            Some("persisted diagnostic".to_string()),
        );
        store
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");

        let durable_state = store.durable_state();
        let sidecar_store = store.recovery_sidecar_store_state();
        let restored = WorkspaceStore::from_persisted_parts(durable_state, sidecar_store);

        let export = restored
            .recovery_sidecar_export("recovery-persisted")
            .expect("restored recovery export should exist");
        assert_eq!(export.current_status, RecoveryStatus::Ready);
        assert_eq!(
            export.execution_chain_ref.as_deref(),
            Some("chain-persisted")
        );
        assert_eq!(export.recovery_ref, "recovery-persisted");
    }

    #[test]
    fn recovery_sidecar_flush_hook_only_persists_dirty_sidecars() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-flush");
        store
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("workspace should be registrable");

        let mut flushes = Vec::new();
        assert!(
            !store
                .flush_recovery_sidecars_with(|state| {
                    flushes.push(state.recovery_handles.len());
                    Ok::<_, std::io::Error>(())
                })
                .expect("empty recovery sidecar flush should succeed")
        );
        assert!(flushes.is_empty());

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-flush".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-flush",
            "flush snapshot",
        );
        store.prepare_recovery_entry(
            workspace_id,
            ExecutionOwnership {
                execution_chain_ref: Some("chain-flush".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-flush",
            None,
        );
        assert!(
            store
                .flush_recovery_sidecars_with(|state| {
                    flushes.push(state.recovery_handles.len());
                    Ok::<_, std::io::Error>(())
                })
                .expect("dirty recovery sidecar flush should succeed")
        );
        assert_eq!(flushes, vec![1]);
        assert!(
            !store
                .flush_recovery_sidecars_with(|_| Ok::<_, std::io::Error>(()))
                .expect("clean recovery sidecar flush should be skipped")
        );
    }

    #[test]
    fn release_worktree_allocation_releases_single_allocation() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-alloc");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        // 分配 2 个 worktree
        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t1")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("alloc 1");
        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t2")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt2"),
            )
            .expect("alloc 2");

        let all = store.worktree_allocations();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|a| a.active));

        // 释放第一个
        let first_id = all[0].allocation_id.clone();
        let released = store
            .release_worktree_allocation(&first_id)
            .expect("release single");
        assert!(!released.active);
        assert!(released.released_at.is_some());

        // 第二个仍然活跃
        let remaining = store.active_worktree_allocations(&workspace_id);
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].ownership.task_id.as_ref().map(|t| t.as_str()),
            Some("t2")
        );

        // workspace.worktree_root 仍存在（还有活跃分配）
        let workspaces = store.workspaces();
        let ws = workspaces
            .iter()
            .find(|w| w.workspace_id == workspace_id)
            .unwrap();
        assert!(ws.worktree_root.is_some());
    }

    #[test]
    fn worktree_assignment_is_idempotent_for_same_ownership_and_root() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-idempotent");
        let ownership = ExecutionOwnership {
            session_id: Some(SessionId::new("s1")),
            task_id: Some(magi_core::TaskId::new("t1")),
            ..ExecutionOwnership::default()
        };
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ownership.clone(),
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("first allocation should succeed");
        let repeated = store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ownership.clone(),
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("same ownership and root should be idempotent");

        assert_eq!(
            repeated.worktree_root,
            Some(AbsolutePath::new("/tmp/ws/wt1"))
        );
        assert_eq!(store.active_worktree_allocations(&workspace_id).len(), 1);

        let conflicting = store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ownership,
                AbsolutePath::new("/tmp/ws/wt2"),
            )
            .expect_err("same ownership with different root remains a conflict");
        assert!(matches!(
            conflicting,
            DomainError::AlreadyExists {
                entity: "worktree_allocation"
            }
        ));
    }

    #[test]
    fn release_worktree_root_releases_all_allocations_for_workspace() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-release-root");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t1")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("alloc 1");
        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t2")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt2"),
            )
            .expect("alloc 2");

        let released_workspace = store
            .release_worktree_root(&workspace_id)
            .expect("release workspace root");
        assert!(released_workspace.worktree_root.is_none());
        assert!(store.active_worktree_allocations(&workspace_id).is_empty());
        assert!(
            store
                .worktree_allocations()
                .into_iter()
                .all(|allocation| !allocation.active && allocation.released_at.is_some())
        );
    }

    #[test]
    fn release_last_allocation_clears_workspace_worktree_root() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-last");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t1")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("alloc");

        let all = store.worktree_allocations();
        assert_eq!(all.len(), 1);

        // 释放唯一的分配 → workspace.worktree_root 应被清除
        store
            .release_worktree_allocation(&all[0].allocation_id)
            .expect("release last");

        let workspaces = store.workspaces();
        let ws = workspaces
            .iter()
            .find(|w| w.workspace_id == workspace_id)
            .unwrap();
        assert!(ws.worktree_root.is_none());
    }

    #[test]
    fn release_already_released_allocation_returns_error() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-double");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("alloc");

        let all = store.worktree_allocations();
        let id = &all[0].allocation_id;

        store
            .release_worktree_allocation(id)
            .expect("release first time");
        let err = store
            .release_worktree_allocation(id)
            .expect_err("double release should fail");
        assert!(matches!(err, DomainError::InvalidState { .. }));
    }

    #[test]
    fn build_recovery_resume_input_rejects_prepared_handle_until_ready() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-prepared-recovery");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-prepared-recovery")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-prepared-recovery".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-prepared-recovery",
            "snapshot label",
        );

        let recovery = store.prepare_recovery_entry(
            workspace_id,
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-prepared-recovery")),
                execution_chain_ref: Some("chain-prepared-recovery".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-prepared-recovery",
            Some("diagnostic".to_string()),
        );

        let err = store
            .build_recovery_resume_input(&recovery.recovery_id)
            .expect_err("prepared recovery should be rejected before ready");
        assert!(matches!(err, DomainError::InvalidState { .. }));

        store
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        let input = store
            .build_recovery_resume_input(&recovery.recovery_id)
            .expect("ready recovery should build resume input");
        assert_eq!(input.recovery_id, "recovery-prepared-recovery");
    }

    #[test]
    fn recovery_diagnostic_summary_is_public_for_storage_export_and_resume_input() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-public-recovery");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        let snapshot = store.append_execution_snapshot(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-public-recovery")),
                workspace_id: Some(workspace_id.clone()),
                execution_chain_ref: Some("chain-public-recovery".to_string()),
                ..ExecutionOwnership::default()
            },
            "snapshot-public-recovery",
            "snapshot label",
        );

        let recovery = store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-public-recovery")),
                execution_chain_ref: Some("chain-public-recovery".to_string()),
                ..ExecutionOwnership::default()
            },
            snapshot.snapshot_id,
            "recovery-public-recovery",
            Some(
                "resume failed at /Users/xie/.magi/recovery.json with Bearer abcdef and sk-secret"
                    .to_string(),
            ),
        );

        store
            .mark_recovery_ready(&recovery.recovery_id)
            .expect("recovery should become ready");
        let export = store
            .recovery_sidecar_exports()
            .into_iter()
            .find(|export| export.recovery_ref == recovery.recovery_id)
            .expect("recovery export should exist");
        let input = store
            .build_recovery_resume_input(&recovery.recovery_id)
            .expect("resume input should build");

        for public_summary in [
            recovery.diagnostic_summary.as_deref(),
            export.diagnostic_summary.as_deref(),
            input.diagnostic_summary.as_deref(),
        ] {
            let public_summary = public_summary.expect("diagnostic summary should exist");
            assert!(public_summary.contains("[path]"));
            assert!(public_summary.contains("Bearer [redacted]"));
            assert!(public_summary.contains("sk-[redacted]"));
            assert!(!public_summary.contains("/Users/xie"));
            assert!(!public_summary.contains("abcdef"));
            assert!(!public_summary.contains("sk-secret"));
        }
    }

    #[test]
    fn duplicate_ownership_allocation_rejected() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-dup");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        let ownership = ExecutionOwnership {
            session_id: Some(SessionId::new("s1")),
            task_id: Some(magi_core::TaskId::new("t1")),
            ..ExecutionOwnership::default()
        };

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ownership.clone(),
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("first alloc");

        let err = store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ownership,
                AbsolutePath::new("/tmp/ws/wt2"),
            )
            .expect_err("duplicate ownership should be rejected");
        assert!(matches!(err, DomainError::AlreadyExists { .. }));
    }

    #[test]
    fn worktree_allocations_by_ownership_filters_correctly() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-query");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        let s1 = SessionId::new("session-a");
        let s2 = SessionId::new("session-b");

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(s1.clone()),
                    task_id: Some(magi_core::TaskId::new("task-1")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("alloc 1");
        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(s2.clone()),
                    task_id: Some(magi_core::TaskId::new("task-2")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt2"),
            )
            .expect("alloc 2");

        // 按 session_id 查询
        let by_s1 = store.worktree_allocations_by_ownership(Some(&s1), None, None);
        assert_eq!(by_s1.len(), 1);
        assert_eq!(by_s1[0].ownership.session_id.as_ref(), Some(&s1));

        // 按 task_id 查询
        let task_2 = magi_core::TaskId::new("task-2");
        let by_task = store.worktree_allocations_by_ownership(None, Some(&task_2), None);
        assert_eq!(by_task.len(), 1);
        assert_eq!(by_task[0].ownership.session_id.as_ref(), Some(&s2));

        // 无匹配
        let s3 = SessionId::new("session-c");
        let by_s3 = store.worktree_allocations_by_ownership(Some(&s3), None, None);
        assert!(by_s3.is_empty());
    }

    #[test]
    fn active_worktree_allocations_returns_only_active() {
        let store = WorkspaceStore::new();
        let workspace_id = WorkspaceId::new("workspace-active-filter");
        store
            .register(workspace_id.clone(), AbsolutePath::new("/tmp/ws"))
            .expect("register");

        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t1")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt1"),
            )
            .expect("alloc 1");
        store
            .assign_worktree_root_for_execution(
                &workspace_id,
                ExecutionOwnership {
                    session_id: Some(SessionId::new("s1")),
                    task_id: Some(magi_core::TaskId::new("t2")),
                    ..ExecutionOwnership::default()
                },
                AbsolutePath::new("/tmp/ws/wt2"),
            )
            .expect("alloc 2");

        let all = store.worktree_allocations();
        assert_eq!(all.len(), 2);

        // 释放第一个
        store
            .release_worktree_allocation(&all[0].allocation_id)
            .expect("release");

        let active = store.active_worktree_allocations(&workspace_id);
        assert_eq!(active.len(), 1);
        assert!(active[0].active);
    }
}
