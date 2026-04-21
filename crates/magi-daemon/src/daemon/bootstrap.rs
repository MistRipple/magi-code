use magi_core::{AbsolutePath, ExecutionOwnership, SessionId, WorkspaceId};
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;

pub(super) fn bootstrap_shadow_state(
    session_store: &SessionStore,
    workspace_registry: &WorkspaceStore,
) {
    if session_store.is_empty() {
        session_store
            .create_session(
                SessionId::new("shadow-session-001"),
                "Rust 影子后端初始化会话",
            )
            .expect("bootstrap session should be creatable");
        session_store.append_notification(
            SessionId::new("shadow-session-001"),
            "notification-shadow-bootstrap",
            "system.bootstrap",
            "Rust 影子后端已完成初始引导",
        );
    }
    if workspace_registry.is_empty() {
        let ownership = ExecutionOwnership {
            session_id: Some(SessionId::new("shadow-session-001")),
            workspace_id: Some(WorkspaceId::new("shadow-workspace-001")),
            execution_chain_ref: Some("shadow-execution-chain-001".to_string()),
            ..ExecutionOwnership::default()
        };
        workspace_registry
            .register(
                WorkspaceId::new("shadow-workspace-001"),
                AbsolutePath::new("/Users/xie/code/magi"),
            )
            .expect("bootstrap workspace should be creatable");
        workspace_registry
            .activate(&WorkspaceId::new("shadow-workspace-001"))
            .expect("bootstrap workspace should be activatable");
        workspace_registry
            .assign_worktree_root_for_execution(
                &WorkspaceId::new("shadow-workspace-001"),
                ownership.clone(),
                AbsolutePath::new(
                    "/Users/xie/code/magi-rust-rewrite/tmp/worktrees/shadow-worktree-001",
                ),
            )
            .expect("bootstrap worktree should be assignable");
        let snapshot = workspace_registry.append_execution_snapshot(
            WorkspaceId::new("shadow-workspace-001"),
            ownership.clone(),
            "snapshot-shadow-bootstrap",
            "初始工作区快照",
        );
        let recovery = workspace_registry.prepare_recovery_entry(
            WorkspaceId::new("shadow-workspace-001"),
            ownership.clone(),
            snapshot.snapshot_id,
            "recovery-shadow-bootstrap",
            Some("初始影子恢复入口".to_string()),
        );
        session_store.bind_execution_ownership(
            SessionId::new("shadow-session-001"),
            ownership,
        );
        session_store
            .attach_recovery_ref(
                &SessionId::new("shadow-session-001"),
                Some(recovery.recovery_id),
            )
            .expect("bootstrap recovery ref should be attachable");
    }
}
