use crate::protocol::*;
use crate::session_context::*;
use crate::session_registry::*;
use crate::workspace_container::*;

#[test]
fn protocol_serialize_handshake_request() {
    let msg = ClientToAgentMessage::HandshakeRequest {
        identity: AgentClientIdentity {
            client_id: "vscode-1".to_string(),
            client_kind: ClientKind::Vscode,
            client_version: "1.0.0".to_string(),
            platform: Platform::Macos,
        },
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("agent.handshake.request"));
    assert!(json.contains("vscode-1"));
}

#[test]
fn protocol_serialize_handshake_response() {
    let msg = AgentToClientMessage::HandshakeResponse {
        agent_version: "0.1.0".to_string(),
        capabilities: AgentCapabilities::default(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("agent.handshake.response"));
    assert!(json.contains("runtimeRelay"));
}

#[test]
fn protocol_serialize_disconnect() {
    let msg = AgentToClientMessage::Disconnect {
        reason: DisconnectReason::AgentStopped,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("agent.disconnect"));
    assert!(json.contains("agent_stopped"));
}

#[test]
fn protocol_serialize_error() {
    let msg = AgentToClientMessage::Error {
        code: AgentErrorCode::WorkspaceNotFound,
        message: "工作区未找到".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("workspace_not_found"));
}

#[test]
fn session_context_basic_lifecycle() {
    let mut ctx = SessionExecutionContext::new("s1");
    assert_eq!(ctx.session_id(), "s1");

    ctx.touch();
    assert!(ctx.last_touched_at() >= ctx.created_at());
}

#[test]
fn session_context_pending_recovery() {
    let mut ctx = SessionExecutionContext::new("s1");
    assert!(ctx.pending_recovery().is_none());

    ctx.set_pending_recovery(Some(PendingRecoveryContext {
        task_id: "task-1".to_string(),
        prompt: "fix it".to_string(),
        session_id: "s1".to_string(),
        chain_id: None,
        runtime_reason: None,
        errors: vec!["compile error".to_string()],
        can_retry: true,
        can_rollback: false,
    }));
    assert!(ctx.pending_recovery().is_some());

    ctx.clear_pending_recovery();
    assert!(ctx.pending_recovery().is_none());
}

#[test]
fn session_context_plan_approval() {
    let mut ctx = SessionExecutionContext::new("s1");
    ctx.set_pending_plan_approval(
        "req-1".to_string(),
        PendingPlanApprovalContext {
            plan_id: "plan-1".to_string(),
            session_id: "s1".to_string(),
            prompt: "approve?".to_string(),
            chain_id: None,
        },
    );
    assert!(ctx.get_pending_plan_approval("req-1").is_some());
    assert!(ctx.delete_pending_plan_approval("req-1"));
    assert!(ctx.get_pending_plan_approval("req-1").is_none());
}

#[test]
fn session_context_clear_transient() {
    let mut ctx = SessionExecutionContext::new("s1");
    ctx.set_pending_recovery(Some(PendingRecoveryContext {
        task_id: "task-1".to_string(),
        prompt: "x".to_string(),
        session_id: "s1".to_string(),
        chain_id: None,
        runtime_reason: None,
        errors: vec![],
        can_retry: false,
        can_rollback: false,
    }));
    ctx.clear_transient_state();
    assert!(ctx.pending_recovery().is_none());
}

#[test]
fn session_context_stats() {
    let ctx = SessionExecutionContext::new("s1");
    let stats = ctx.stats();
    assert_eq!(stats.session_id, "s1");
    assert!(!stats.has_pending_recovery);
    assert_eq!(stats.pending_approval_count, 0);
}

#[test]
fn session_registry_basic() {
    let mut reg = SessionRuntimeRegistry::default();
    assert_eq!(reg.size(), 0);

    {
        let ctx = reg.get_or_create("s1");
        ctx.set_pending_plan_approval(
            "req-1".to_string(),
            PendingPlanApprovalContext {
                plan_id: "plan-1".to_string(),
                session_id: "s1".to_string(),
                prompt: "approve?".to_string(),
                chain_id: None,
            },
        );
    }
    assert_eq!(reg.size(), 1);
    assert!(reg.has("s1"));
    assert!(!reg.has("s2"));

    assert!(reg.get("s1").is_some());
    assert!(
        reg.get("s1")
            .unwrap()
            .get_pending_plan_approval("req-1")
            .is_some()
    );
}

#[test]
fn session_registry_remove() {
    let mut reg = SessionRuntimeRegistry::default();
    reg.get_or_create("s1");
    assert!(reg.remove("s1"));
    assert!(!reg.has("s1"));
    assert!(!reg.remove("s1"));
}

#[test]
fn session_registry_list_and_clear() {
    let mut reg = SessionRuntimeRegistry::default();
    reg.get_or_create("s1");
    reg.get_or_create("s2");
    let ids = reg.list_ids();
    assert_eq!(ids.len(), 2);
    reg.clear();
    assert_eq!(reg.size(), 0);
}

#[test]
fn workspace_container_basic() {
    let container = WorkspaceRuntimeContainer::new(WorkspaceDescriptor {
        workspace_id: "ws-1".to_string(),
        name: "my-project".to_string(),
        root_path: "/home/user/project".to_string(),
    });
    assert_eq!(container.workspace_id(), "ws-1");
    assert_eq!(container.workspace_name(), "my-project");
    assert_eq!(container.workspace_root(), "/home/user/project");

    let stats = container.stats();
    assert_eq!(stats.session_count, 0);
}

#[test]
fn workspace_container_session_management() {
    let mut container = WorkspaceRuntimeContainer::new(WorkspaceDescriptor {
        workspace_id: "ws-1".to_string(),
        name: "project".to_string(),
        root_path: "/tmp/project".to_string(),
    });
    container.session_registry_mut().get_or_create("s1");
    container.session_registry_mut().get_or_create("s2");
    assert_eq!(container.stats().session_count, 2);

    container.clear();
    assert_eq!(container.stats().session_count, 0);
}
