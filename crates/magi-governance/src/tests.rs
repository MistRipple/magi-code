use magi_core::{ApprovalRequirement, RiskLevel};

use crate::{
    GovernanceAction, GovernanceOutcome, GovernanceService, GovernanceTarget, PathAccessRequest,
    SandboxRequest, ToolExecutionRequest, ToolKind, WorkerControlKind, WorkerControlRequest,
};

#[test]
fn governance_service_can_distinguish_tool_and_worker_control_paths() {
    let service = GovernanceService::default();

    let tool_allowed = service.evaluate_tool_request(&ToolExecutionRequest {
        tool_name: "file_read".to_string(),
        tool_kind: ToolKind::Builtin,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
    });
    assert_eq!(tool_allowed.outcome, GovernanceOutcome::Allowed);
    assert!(tool_allowed.allowed);

    let tool_needs_approval = service.evaluate_tool_request(&ToolExecutionRequest {
        tool_name: "shell_exec".to_string(),
        tool_kind: ToolKind::Builtin,
        risk_level: RiskLevel::High,
        approval_requirement: ApprovalRequirement::None,
    });
    assert_eq!(tool_needs_approval.outcome, GovernanceOutcome::NeedsApproval);
    assert!(tool_needs_approval.requires_approval);

    let worker_blocked = service.evaluate_worker_control_request(&WorkerControlRequest {
        worker_id: None,
        mission_id: None,
        assignment_id: None,
        task_id: None,
        action: WorkerControlKind::Execute,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 0,
        blocked: true,
        reason: Some("blocked".to_string()),
    });
    assert_eq!(worker_blocked.outcome, GovernanceOutcome::Blocked);
    assert!(!worker_blocked.allowed);

    let worker_retry_rejected = service.evaluate_worker_control_request(&WorkerControlRequest {
        worker_id: None,
        mission_id: None,
        assignment_id: None,
        task_id: None,
        action: WorkerControlKind::RepairRetry,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 0,
        blocked: false,
        reason: Some("retry".to_string()),
    });
    assert_eq!(worker_retry_rejected.outcome, GovernanceOutcome::Rejected);
    assert!(!worker_retry_rejected.allowed);
}

#[test]
fn governance_service_enforces_sandbox_and_path_policy_boundaries() {
    let service = GovernanceService::default();

    let sandbox_blocked = service.evaluate_sandbox(&SandboxRequest {
        command: "cargo test".to_string(),
        working_directory: String::new(),
    });
    assert_eq!(sandbox_blocked.outcome, GovernanceOutcome::Blocked);

    let path_blocked = service.evaluate_path_access(&PathAccessRequest {
        absolute_path: "relative/path".to_string(),
    });
    assert_eq!(path_blocked.outcome, GovernanceOutcome::Blocked);

    let path_allowed = service.evaluate_path_access(&PathAccessRequest {
        absolute_path: "/tmp/allowed".to_string(),
    });
    assert_eq!(path_allowed.outcome, GovernanceOutcome::Allowed);
}

#[test]
fn governance_service_can_emit_tool_decision_trace() {
    let service = GovernanceService::default();

    let trace = service.trace_tool_request(&ToolExecutionRequest {
        tool_name: "shell_exec".to_string(),
        tool_kind: ToolKind::Builtin,
        risk_level: RiskLevel::High,
        approval_requirement: ApprovalRequirement::None,
    });

    assert_eq!(trace.action, GovernanceAction::RequiresManualApproval);
    assert_eq!(trace.decision.outcome, GovernanceOutcome::NeedsApproval);
    assert!(trace.summary.contains("tool:shell_exec"));
    assert!(trace.summary.contains("needs_approval"));
    match trace.target {
        GovernanceTarget::Tool { tool_name, tool_kind } => {
            assert_eq!(tool_name, "shell_exec");
            assert_eq!(tool_kind, ToolKind::Builtin);
        }
        other => panic!("unexpected trace target: {other:?}"),
    }
}

#[test]
fn governance_service_can_emit_worker_and_path_traces() {
    let service = GovernanceService::default();

    let worker_trace = service.trace_worker_control_request(&WorkerControlRequest {
        worker_id: None,
        mission_id: None,
        assignment_id: None,
        task_id: None,
        action: WorkerControlKind::RepairRetry,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 0,
        blocked: false,
        reason: Some("retry".to_string()),
    });
    assert_eq!(worker_trace.action, GovernanceAction::Rejected);
    assert!(worker_trace.summary.contains("worker_control:repair_retry"));
    assert!(worker_trace.summary.contains("rejected"));

    let path_trace = service.trace_path_access(&PathAccessRequest {
        absolute_path: "relative/path".to_string(),
    });
    assert_eq!(path_trace.action, GovernanceAction::Blocked);
    assert!(path_trace.summary.contains("path:relative/path"));
    assert!(path_trace.summary.contains("blocked"));
}
