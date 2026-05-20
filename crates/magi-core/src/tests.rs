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
use crate::{LeaseId, MissionId, TaskId, UtcMillis};

#[test]
fn task_kind_serialization_roundtrip() {
    let kinds = vec![
        TaskKind::LocalAgent,
        TaskKind::LocalWorkflow,
        TaskKind::RemoteAgent,
        TaskKind::MonitorMcp,
        TaskKind::InProcessTeammate,
        TaskKind::Dream,
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
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
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
        kind: TaskKind::LocalAgent,
        title: "Test action".to_string(),
        goal: "Do something".to_string(),
        status: TaskStatus::Pending,
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
            validation_profile: Some("standard".to_string()),
            checkpoint_mode: "auto".to_string(),
            task_tier: TaskTier::ExecutionChain,
            background_allowed: false,
            escalation_conditions: vec!["high_risk".to_string()],
        }),
        executor_binding: Some(serde_json::json!({
            "target_role": "developer",
            "capability_requirements": ["rust"],
            "parallelism_group": "group-a",
            "exclusive_scope": null,
            "worker_selector": null,
        })),
        knowledge_refs: Vec::new(),
        workspace_scope: Some("/workspace".to_string()),
        write_scope: Some("/workspace/src".to_string()),
        input_refs: vec!["input-1".to_string()],
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: TaskRuntimePayload::default(),
        created_at: UtcMillis(1000),
        updated_at: UtcMillis(2000),
    };

    let json = serde_json::to_string(&task).expect("序列化失败");
    let deserialized: Task = serde_json::from_str(&json).expect("反序列化失败");

    assert_eq!(deserialized.task_id.to_string(), "task-1");
    assert_eq!(deserialized.kind, TaskKind::LocalAgent);
    assert_eq!(deserialized.status, TaskStatus::Pending);
    assert_eq!(deserialized.dependency_ids.len(), 1);
    assert!(deserialized.policy_snapshot.is_some());
    assert!(deserialized.executor_binding.is_some());
    assert!(matches!(
        deserialized.runtime_payload,
        TaskRuntimePayload::None
    ));
}

#[test]
fn task_runtime_payload_none_serialization_roundtrip() {
    let none_payload = "{\"kind\":\"none\"}";
    let parsed_none: TaskRuntimePayload = serde_json::from_str(none_payload).expect("反序列化失败");
    assert!(matches!(parsed_none, TaskRuntimePayload::None));
}

#[test]
fn progress_summary_default() {
    let summary = ProgressSummary::default();
    assert_eq!(summary.total_tasks, 0);
    assert_eq!(summary.pending_tasks, 0);
    assert_eq!(summary.running_tasks, 0);
    assert_eq!(summary.completed_tasks, 0);
    assert_eq!(summary.failed_tasks, 0);
    assert_eq!(summary.killed_tasks, 0);
    assert_eq!(summary.settled_tasks, 0);
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
