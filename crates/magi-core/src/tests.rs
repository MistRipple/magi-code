use crate::{
    AbsolutePath, DispatchReason, ExecutionOwnership, RecoveryResumeInput, SessionId,
    WorkspaceRootPath, WorktreeRootPath,
};

#[test]
fn root_exports_remain_usable() {
    let ownership = ExecutionOwnership {
        session_id: Some(SessionId::new("session-1")),
        execution_chain_ref: Some("chain-1".to_string()),
        ..ExecutionOwnership::default()
    };
    let resume = RecoveryResumeInput {
        recovery_id: "recovery-1".to_string(),
        snapshot_id: "snapshot-1".to_string(),
        ownership,
        diagnostic_summary: None,
        created_at: crate::UtcMillis(1),
        updated_at: crate::UtcMillis(2),
    };

    assert_eq!(
        resume.ownership.session_id.as_ref().map(SessionId::as_str),
        Some("session-1")
    );
    assert!(matches!(
        DispatchReason::ManualResume,
        DispatchReason::ManualResume
    ));
}

#[test]
fn path_value_objects_provide_consistent_accessors() {
    let absolute_path = AbsolutePath::new("/tmp/project");
    let workspace_root = WorkspaceRootPath::new("/tmp/workspace");
    let worktree_root = WorktreeRootPath::from("/tmp/worktree");

    assert_eq!(absolute_path.as_str(), "/tmp/project");
    assert_eq!(workspace_root.as_str(), "/tmp/workspace");
    assert_eq!(worktree_root.as_str(), "/tmp/worktree");
    assert_eq!(absolute_path.to_string(), "/tmp/project");
    assert_eq!(workspace_root.to_string(), "/tmp/workspace");
    assert_eq!(worktree_root.to_string(), "/tmp/worktree");
}

// ---------------------------------------------------------------------------
// Task domain type tests
// ---------------------------------------------------------------------------

use crate::task::*;
use crate::{LeaseId, MissionId, TaskId, UtcMillis, WorkerId};

#[test]
fn task_kind_serialization_roundtrip() {
    let kinds = vec![
        TaskKind::Objective,
        TaskKind::Phase,
        TaskKind::WorkPackage,
        TaskKind::Action,
        TaskKind::Validation,
        TaskKind::Repair,
        TaskKind::Decision,
    ];
    for kind in &kinds {
        let json = serde_json::to_string(kind).expect("序列化失败");
        let deserialized: TaskKind = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(*kind, deserialized);
    }
}

#[test]
fn task_status_serialization_roundtrip() {
    let statuses = vec![
        TaskStatus::Draft,
        TaskStatus::Ready,
        TaskStatus::Running,
        TaskStatus::Blocked,
        TaskStatus::AwaitingApproval,
        TaskStatus::Verifying,
        TaskStatus::Repairing,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
        TaskStatus::Skipped,
    ];
    for status in &statuses {
        let json = serde_json::to_string(status).expect("序列化失败");
        let deserialized: TaskStatus = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(*status, deserialized);
    }
}

#[test]
fn task_serialization_roundtrip() {
    let task = Task {
        task_id: TaskId::new("task-1"),
        mission_id: MissionId::new("mission-1"),
        root_task_id: TaskId::new("root-1"),
        parent_task_id: Some(TaskId::new("parent-1")),
        kind: TaskKind::Action,
        title: "Test action".to_string(),
        goal: "Do something".to_string(),
        status: TaskStatus::Ready,
        dependency_ids: vec![TaskId::new("dep-1")],
        required_children: Vec::new(),
        policy_snapshot: Some(TaskPolicy {
            autonomy_level: "full".to_string(),
            approval_mode: "none".to_string(),
            allowed_tools: vec!["search".to_string()],
            denied_tools: Vec::new(),
            allowed_paths: vec!["/src".to_string()],
            denied_paths: Vec::new(),
            network_mode: "restricted".to_string(),
            command_mode: "sandboxed".to_string(),
            retry_limit: 3,
            repair_limit: 1,
            validation_profile: Some("standard".to_string()),
            checkpoint_mode: "auto".to_string(),
            background_allowed: false,
            escalation_conditions: vec!["high_risk".to_string()],
        }),
        executor_binding: Some(ExecutorBinding {
            target_role: "developer".to_string(),
            capability_requirements: vec!["rust".to_string()],
            parallelism_group: Some("group-a".to_string()),
            exclusive_scope: None,
            worker_selector: None,
        }),
        context_refs: vec!["ctx-1".to_string()],
        knowledge_refs: Vec::new(),
        workspace_scope: Some("/workspace".to_string()),
        write_scope: Some("/workspace/src".to_string()),
        input_refs: vec!["input-1".to_string()],
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        variant: TaskVariant::default(),
        created_at: UtcMillis(1000),
        updated_at: UtcMillis(2000),
    };

    let json = serde_json::to_string(&task).expect("序列化失败");
    let deserialized: Task = serde_json::from_str(&json).expect("反序列化失败");

    assert_eq!(deserialized.task_id.to_string(), "task-1");
    assert_eq!(deserialized.kind, TaskKind::Action);
    assert_eq!(deserialized.status, TaskStatus::Ready);
    assert_eq!(deserialized.dependency_ids.len(), 1);
    assert!(deserialized.policy_snapshot.is_some());
    assert!(deserialized.executor_binding.is_some());
    assert!(deserialized.variant.is_local_agent());
}

#[test]
fn task_variant_local_bash_serialization_roundtrip() {
    let variant = TaskVariant::LocalBash {
        command: "echo hi".to_string(),
        working_dir: Some("/tmp".to_string()),
    };
    let json = serde_json::to_string(&variant).expect("序列化失败");
    let parsed: TaskVariant = serde_json::from_str(&json).expect("反序列化失败");
    assert!(parsed.is_local_bash());

    let legacy = "{\"kind\":\"local_agent\"}";
    let parsed_legacy: TaskVariant = serde_json::from_str(legacy).expect("反序列化失败");
    assert!(parsed_legacy.is_local_agent());
}

#[test]
fn lease_status_serialization_roundtrip() {
    let statuses = vec![
        LeaseStatus::Active,
        LeaseStatus::Completed,
        LeaseStatus::Expired,
        LeaseStatus::Revoked,
    ];
    for status in &statuses {
        let json = serde_json::to_string(status).expect("序列化失败");
        let deserialized: LeaseStatus = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(*status, deserialized);
    }
}

#[test]
fn assignment_lease_serialization_roundtrip() {
    let lease = AssignmentLease {
        lease_id: LeaseId::new("lease-1"),
        task_id: TaskId::new("task-1"),
        root_task_id: TaskId::new("task-1"),
        worker_id: WorkerId::new("worker-1"),
        role: "executor".to_string(),
        granted_at: UtcMillis(1000),
        expires_at: UtcMillis(61000),
        heartbeat_at: UtcMillis(1000),
        lease_status: LeaseStatus::Active,
    };

    let json = serde_json::to_string(&lease).expect("序列化失败");
    let deserialized: AssignmentLease = serde_json::from_str(&json).expect("反序列化失败");

    assert_eq!(deserialized.lease_id.to_string(), "lease-1");
    assert_eq!(deserialized.lease_status, LeaseStatus::Active);
}

#[test]
fn decision_option_and_payload_serialization() {
    let payload = DecisionTaskPayload {
        decision_context: "需要选择实现方案".to_string(),
        blocked_reason: "存在多个可行方案".to_string(),
        target_task_id: Some(TaskId::new("target-1")),
        options: vec![
            DecisionOption {
                option_id: "opt-1".to_string(),
                label: "方案 A".to_string(),
                description: "使用内存存储".to_string(),
            },
            DecisionOption {
                option_id: "opt-2".to_string(),
                label: "方案 B".to_string(),
                description: "使用数据库存储".to_string(),
            },
        ],
        risk_notes: vec!["方案 A 不支持持久化".to_string()],
        recommended_option: Some("opt-2".to_string()),
        required_user_input: true,
        decision_evidence: Some(serde_json::json!({"analysis": "detailed"})),
    };

    let json = serde_json::to_string(&payload).expect("序列化失败");
    let deserialized: DecisionTaskPayload = serde_json::from_str(&json).expect("反序列化失败");

    assert_eq!(deserialized.options.len(), 2);
    assert_eq!(deserialized.recommended_option, Some("opt-2".to_string()));
    assert!(deserialized.required_user_input);
}

#[test]
fn progress_summary_default() {
    let summary = ProgressSummary::default();
    assert_eq!(summary.total_tasks, 0);
    assert_eq!(summary.completed_tasks, 0);
    assert_eq!(summary.settled_tasks, 0);
    assert_eq!(summary.failed_tasks, 0);
    assert_eq!(summary.running_tasks, 0);
    assert_eq!(summary.blocked_tasks, 0);
}

#[test]
fn task_id_and_lease_id_follow_id_patterns() {
    let task_id = TaskId::new("my-task");
    assert_eq!(task_id.as_str(), "my-task");
    assert_eq!(task_id.to_string(), "my-task");

    let task_id_from_string = TaskId::from("my-task".to_string());
    assert_eq!(task_id, task_id_from_string);

    let task_id_from_str = TaskId::from("my-task");
    assert_eq!(task_id, task_id_from_str);

    let lease_id = LeaseId::new("my-lease");
    assert_eq!(lease_id.as_str(), "my-lease");
    assert_eq!(lease_id.to_string(), "my-lease");
}
