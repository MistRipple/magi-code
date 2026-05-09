use magi_core::{AbsolutePath, DomainError, ExecutionOwnership, SessionId, WorkspaceId};
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::path::Path;

pub(super) fn bootstrap_state(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    bootstrap_workspace_root: &Path,
    bootstrap_worktree_root: &Path,
) {
    let bootstrap_session_id = SessionId::new("test-session-001");
    let bootstrap_workspace_id = WorkspaceId::new("test-workspace-001");
    if session_store.is_empty() {
        session_store
            .create_session(bootstrap_session_id.clone(), "Rust 影子后端初始化会话")
            .expect("bootstrap session should be creatable");
        session_store.append_notification(
            bootstrap_session_id.clone(),
            "notification-bootstrap",
            "system.bootstrap",
            "Rust 影子后端已完成初始引导",
        );
    }
    let session_id = session_store
        .current_session()
        .map(|session| session.session_id)
        .unwrap_or_else(|| bootstrap_session_id.clone());
    if workspace_registry.is_empty() {
        let ownership = ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(bootstrap_workspace_id.clone()),
            execution_chain_ref: Some("test-execution-chain-001".to_string()),
            ..ExecutionOwnership::default()
        };
        workspace_registry
            .register(
                bootstrap_workspace_id.clone(),
                AbsolutePath::new(bootstrap_workspace_root.to_string_lossy().to_string()),
            )
            .expect("bootstrap workspace should be creatable");
        workspace_registry
            .activate(&bootstrap_workspace_id)
            .expect("bootstrap workspace should be activatable");
        let requested_worktree_root =
            AbsolutePath::new(bootstrap_worktree_root.to_string_lossy().to_string());
        if let Err(error) = workspace_registry.assign_worktree_root_for_execution(
            &bootstrap_workspace_id,
            ownership.clone(),
            requested_worktree_root,
        ) {
            match error {
                DomainError::AlreadyExists {
                    entity: "worktree_allocation",
                } => {
                    let existing_root = workspace_registry
                        .active_worktree_allocations(&bootstrap_workspace_id)
                        .into_iter()
                        .find(|allocation| {
                            allocation.ownership.session_id == ownership.session_id
                                && allocation.ownership.task_id == ownership.task_id
                                && allocation.ownership.worker_id == ownership.worker_id
                        })
                        .map(|allocation| allocation.worktree_root)
                        .expect("bootstrap worktree allocation should exist");
                    workspace_registry
                        .assign_worktree_root_for_execution(
                            &bootstrap_workspace_id,
                            ownership.clone(),
                            existing_root,
                        )
                        .expect("bootstrap worktree should reuse existing allocation");
                }
                other => panic!("bootstrap worktree should be assignable: {other:?}"),
            }
        }
        let snapshot = workspace_registry
            .resolve_snapshot("snapshot-bootstrap")
            .unwrap_or_else(|| {
                workspace_registry.append_execution_snapshot(
                    bootstrap_workspace_id.clone(),
                    ownership.clone(),
                    "snapshot-bootstrap",
                    "初始工作区快照",
                )
            });
        let recovery = workspace_registry.prepare_recovery_entry(
            bootstrap_workspace_id.clone(),
            ownership.clone(),
            snapshot.snapshot_id,
            "recovery-bootstrap",
            Some("初始影子恢复入口".to_string()),
        );
        session_store.bind_execution_ownership(session_id.clone(), ownership);
        session_store
            .attach_recovery_ref(&session_id, Some(recovery.recovery_id))
            .expect("bootstrap recovery ref should be attachable");
    }
    ensure_bootstrap_workspace_session_binding(
        session_store,
        workspace_registry,
        &session_id,
        &bootstrap_workspace_id,
    );
}

fn ensure_bootstrap_workspace_session_binding(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
    session_id: &SessionId,
    workspace_id: &WorkspaceId,
) {
    let workspace_exists = workspace_registry
        .workspaces()
        .iter()
        .any(|workspace| &workspace.workspace_id == workspace_id);
    let has_scoped_session = !session_store
        .sessions_for_workspace(workspace_id.as_str())
        .is_empty();
    if !workspace_exists || has_scoped_session {
        return;
    }

    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: Some(workspace_id.clone()),
        execution_chain_ref: Some("test-execution-chain-001".to_string()),
        ..ExecutionOwnership::default()
    };
    session_store.bind_execution_ownership(session_id.clone(), ownership);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_binds_existing_current_session_when_workspace_is_recreated() {
        let session_store = SessionStore::new();
        let session_id = SessionId::new("existing-session");
        session_store
            .create_session(session_id.clone(), "Existing Session")
            .expect("existing session should be creatable");
        let workspace_store = WorkspaceStore::new();

        bootstrap_state(
            &session_store,
            &workspace_store,
            Path::new("/tmp/magi-bootstrap-workspace"),
            Path::new("/tmp/magi-bootstrap-worktree"),
        );

        let scoped_sessions = session_store.sessions_for_workspace("test-workspace-001");
        assert_eq!(scoped_sessions.len(), 1);
        assert_eq!(scoped_sessions[0].session_id, session_id);
        let sidecar = session_store
            .execution_sidecar_export(&session_id)
            .expect("bootstrap should bind existing session sidecar");
        assert_eq!(
            sidecar
                .ownership
                .workspace_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("test-workspace-001")
        );
    }

    #[test]
    fn bootstrap_repairs_existing_workspace_with_unbound_current_session() {
        let session_store = SessionStore::new();
        let session_id = SessionId::new("unbound-session");
        session_store
            .create_session(session_id.clone(), "Unbound Session")
            .expect("unbound session should be creatable");
        let workspace_store = WorkspaceStore::new();
        workspace_store
            .register(
                WorkspaceId::new("test-workspace-001"),
                AbsolutePath::new("/tmp/magi-bootstrap-workspace"),
            )
            .expect("bootstrap workspace should be registrable");

        bootstrap_state(
            &session_store,
            &workspace_store,
            Path::new("/tmp/magi-bootstrap-workspace"),
            Path::new("/tmp/magi-bootstrap-worktree"),
        );

        let scoped_sessions = session_store.sessions_for_workspace("test-workspace-001");
        assert_eq!(scoped_sessions.len(), 1);
        assert_eq!(scoped_sessions[0].session_id, session_id);
        let sidecar = session_store
            .execution_sidecar_export(&session_id)
            .expect("bootstrap should repair missing session sidecar");
        assert_eq!(
            sidecar
                .ownership
                .workspace_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("test-workspace-001")
        );
    }
}
