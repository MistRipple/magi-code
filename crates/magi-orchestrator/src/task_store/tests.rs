use super::*;
use magi_core::{TaskCompletionEvidence, TaskEvidenceRequirement, TaskKind};
use serde_json::json;
use std::sync::{Arc, Barrier, Mutex};

fn task(task_id: &str, status: TaskStatus) -> Task {
    let now = UtcMillis::now();
    Task {
        task_id: TaskId::new(task_id),
        mission_id: MissionId::new("mission-task-store-tests"),
        root_task_id: TaskId::new("root-task-store-tests"),
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: task_id.to_string(),
        goal: task_id.to_string(),
        status,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        completion_contract: magi_core::TaskCompletionContract::default(),
        recovery_checkpoint: None,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: TaskRuntimePayload::default(),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn project_directory_inheritance_rejects_expansion_and_corrupt_reload() {
    let root = tempfile::tempdir().unwrap();
    let grant = |id: &str| magi_core::ProjectSourceFolderGrant {
        source_folder_id: id.into(),
        source_path_ref: magi_core::HostPath::from_path(root.path().join(id))
            .to_path_ref()
            .as_str()
            .into(),
    };
    let directories = magi_core::ProjectDirectoryBinding {
        workspace_id: magi_core::WorkspaceId::new("project"),
        project_revision: 1,
        default_source_folder_id: "a".into(),
        source_folders: vec![grant("a"), grant("b")],
    };
    let store = TaskStore::new();
    let mut parent = rooted_task("directory-parent", "directory-parent");
    let mut executor = magi_core::TaskExecutorBinding::for_role("coordinator");
    executor.project_directories = Some(directories.clone());
    parent.executor_binding = Some(executor);
    store.insert_task(parent.clone()).unwrap();
    let mut child = rooted_task("directory-parent", "directory-child");
    child.parent_task_id = Some(parent.task_id.clone());
    assert!(
        store.insert_task(child.clone()).is_err(),
        "不得通过缺少绑定回落其他目录"
    );
    let mut inherited = directories.clone();
    inherited.source_folders = vec![grant("b")];
    inherited.default_source_folder_id = "b".into();
    let mut executor = magi_core::TaskExecutorBinding::for_role("executor");
    executor.project_directories = Some(inherited);
    child.executor_binding = Some(executor);
    store.insert_task(child.clone()).unwrap();
    let projection = root.path().join("tasks");
    TaskStore::checkpoint_snapshot_to_projection_directory(&store.snapshot(), &projection).unwrap();
    let restored = TaskStore::restore_from_projection_directory(&projection)
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.get_task(&child.task_id).unwrap().executor_binding,
        child.executor_binding
    );
    let mut snapshot = store.snapshot();
    let bad_child = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.task_id == child.task_id)
        .unwrap();
    bad_child
        .executor_binding
        .as_mut()
        .unwrap()
        .project_directories
        .as_mut()
        .unwrap()
        .source_folders
        .push(grant("outside"));
    assert!(
        TaskStore::restore_checked(TaskStoreCheckpoint {
            tasks: snapshot.tasks,
            leases: snapshot.leases,
        })
        .is_err()
    );
    let mut unbound_parent = rooted_task("other-parent", "other-parent");
    unbound_parent.executor_binding = None;
    store.insert_task(unbound_parent.clone()).unwrap();
    child.task_id = TaskId::new("invented-project-child");
    child.root_task_id = unbound_parent.task_id.clone();
    child.parent_task_id = Some(unbound_parent.task_id);
    assert!(store.insert_task(child).is_err());
}

#[test]
fn agent_context_delivery_is_one_atomic_checkpoint_and_rejects_stale_acceptance() {
    let store = TaskStore::new();
    let root = rooted_task("delivery-root", "delivery-root");
    let mut child = rooted_task("delivery-root", "delivery-child");
    child.status = TaskStatus::Pending;
    child.parent_task_id = Some(root.task_id.clone());
    child.runtime_payload = TaskRuntimePayload::AgentContext {
        package: Box::new(AgentContextPackage {
            package_id: "delivery-package".into(),
            revision: 1,
            parent_task_id: root.task_id.clone(),
            summary: "inspect".into(),
            constraints: Vec::new(),
            expected_output: "evidence".into(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }),
        accesses: Vec::new(),
    };
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child.clone()).unwrap();
    let supplement = AgentContextSupplement {
        supplement_id: "delivery-input".into(),
        author_task_id: root.task_id.clone(),
        message: "inspect auth".into(),
        references: Vec::new(),
        estimated_tokens: 3,
        created_at: UtcMillis(2),
    };
    let access = AgentContextAccessRecord {
        record_id: "delivery-access".into(),
        operation: magi_core::AgentContextAccessOperation::Send,
        query: Some(supplement.message.clone()),
        reference_ids: Vec::new(),
        estimated_tokens: 3,
        occurred_at: UtcMillis(2),
    };
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::Persistence {
            message: "injected".into(),
        })
    }));
    assert!(
        store
            .deliver_agent_context_supplement(
                &child.task_id,
                TaskStatus::Pending,
                supplement.clone(),
                access.clone()
            )
            .is_err()
    );
    let failed = store.get_task(&child.task_id).unwrap();
    assert_eq!(failed.agent_context_package().unwrap().revision, 1);
    assert!(
        failed
            .agent_context_package()
            .unwrap()
            .supplements
            .is_empty()
    );
    assert!(failed.agent_context_accesses().is_empty());
    let checkpoints = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = Arc::clone(&checkpoints);
    store.set_checkpoint_callback(Box::new(move |snapshot| {
        captured.fetch_add(1, Ordering::SeqCst);
        let child = snapshot
            .tasks
            .iter()
            .find(|task| task.task_id.as_str() == "delivery-child")
            .unwrap();
        assert_eq!(child.agent_context_package().unwrap().revision, 2);
        assert_eq!(child.agent_context_package().unwrap().supplements.len(), 1);
        assert_eq!(child.agent_context_accesses().len(), 1);
        Ok(())
    }));
    assert!(
        store
            .deliver_agent_context_supplement(
                &child.task_id,
                TaskStatus::Running,
                supplement.clone(),
                access.clone()
            )
            .is_err(),
        "过期的接纳状态不能写入"
    );
    store
        .deliver_agent_context_supplement(
            &child.task_id,
            TaskStatus::Pending,
            supplement.clone(),
            access.clone(),
        )
        .unwrap();
    assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
    assert!(
        store
            .deliver_agent_context_supplement(
                &child.task_id,
                TaskStatus::Pending,
                supplement,
                access
            )
            .is_err(),
        "重复身份不能再增加 revision"
    );
    assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
    let dir = tempfile::tempdir().unwrap();
    store
        .checkpoint_to_projection_directory(dir.path())
        .unwrap();
    let restored = TaskStore::restore_from_projection_directory(dir.path()).unwrap();
    let child = restored.unwrap().get_task(&child.task_id).unwrap();
    assert_eq!(child.agent_context_package().unwrap().supplements.len(), 1);
    assert_eq!(child.agent_context_accesses().len(), 1);
}

fn successful_attempt() -> TaskCompletionAttempt {
    TaskCompletionAttempt {
        output_refs: vec!["output://task-store".to_string()],
        final_response: Some("任务已完成".to_string()),
        evidence: Vec::new(),
    }
}

#[test]
fn grant_lease_and_start_task_commits_task_and_lease_together() {
    let store = TaskStore::new();
    let root = rooted_task("root-grant-atomic", "root-grant-atomic");
    let mut child = rooted_task("root-grant-atomic", "task-grant-atomic");
    child.status = TaskStatus::Pending;
    store.insert_task(root).expect("root should insert");
    store
        .insert_task(child.clone())
        .expect("child should insert");

    let snapshots = Arc::new(Mutex::new(Vec::<TaskStoreSnapshot>::new()));
    let observed_snapshots = Arc::clone(&snapshots);
    store.set_checkpoint_callback(Box::new(move |snapshot| {
        observed_snapshots
            .lock()
            .expect("snapshot lock should not poison")
            .push(snapshot.clone());
        Ok(())
    }));

    let lease = store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-grant-atomic"),
            "executor",
            60_000,
        )
        .expect("grant should succeed")
        .expect("task should receive a lease");

    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        store
            .get_active_lease(&child.task_id)
            .expect("lease should be active")
            .lease_id,
        lease.lease_id
    );
    let snapshots = snapshots.lock().expect("snapshot lock should not poison");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0]
            .tasks
            .iter()
            .find(|task| task.task_id == child.task_id)
            .expect("snapshot should contain child")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        snapshots[0]
            .leases
            .iter()
            .find(|snapshot_lease| snapshot_lease.lease_id == lease.lease_id)
            .expect("snapshot should contain lease")
            .lease_status,
        TaskLeaseState::Active
    );
}

#[test]
fn grant_lease_and_start_task_rejects_wrong_root_and_rolls_back_on_checkpoint_failure() {
    let store = TaskStore::new();
    let root = rooted_task("root-grant-owner", "root-grant-owner");
    let other_root = rooted_task("root-grant-other", "root-grant-other");
    let mut child = rooted_task("root-grant-owner", "task-grant-owner");
    child.status = TaskStatus::Pending;
    store.insert_task(root).expect("root should insert");
    store
        .insert_task(other_root.clone())
        .expect("other root should insert");
    store
        .insert_task(child.clone())
        .expect("child should insert");

    let wrong_root_error = store
        .grant_lease_and_start_task(
            &child.task_id,
            &other_root.task_id,
            &WorkerId::new("worker-grant-owner"),
            "executor",
            60_000,
        )
        .expect_err("wrong root must be rejected");
    assert!(matches!(wrong_root_error, DomainError::InvalidState { .. }));
    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Pending
    );
    assert!(store.get_active_lease(&child.task_id).is_none());

    let now = UtcMillis::now();
    let wrong_owner_lease = TaskLease {
        lease_id: LeaseId::new("lease-wrong-owner"),
        task_id: child.task_id.clone(),
        root_task_id: other_root.task_id.clone(),
        worker_id: WorkerId::new("worker-grant-owner"),
        role: "executor".to_string(),
        granted_at: now,
        expires_at: UtcMillis(now.0.saturating_add(60_000)),
        heartbeat_at: now,
        lease_status: TaskLeaseState::Active,
    };
    let wrong_owner_error = store
        .insert_lease(wrong_owner_lease)
        .expect_err("lease belonging to another root must be rejected");
    assert!(matches!(
        wrong_owner_error,
        DomainError::InvalidState { .. }
    ));

    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::Persistence {
            message: "grant checkpoint unavailable".to_string(),
        })
    }));
    let persistence_error = store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-grant-owner"),
            "executor",
            60_000,
        )
        .expect_err("checkpoint failure must reject grant");
    assert!(matches!(persistence_error, DomainError::Persistence { .. }));
    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Pending
    );
    assert!(store.get_active_lease(&child.task_id).is_none());
}

#[test]
fn complete_lease_and_task_commits_both_terminal_facts_or_neither() {
    let store = TaskStore::new();
    let root = rooted_task("root-complete-atomic", "root-complete-atomic");
    let mut child = rooted_task("root-complete-atomic", "task-complete-atomic");
    child.status = TaskStatus::Pending;
    store.insert_task(root).expect("root should insert");
    store
        .insert_task(child.clone())
        .expect("child should insert");
    let lease = store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-complete-atomic"),
            "executor",
            60_000,
        )
        .expect("grant should succeed")
        .expect("lease should be created");

    let wrong_root = rooted_task("root-complete-wrong", "root-complete-wrong");
    store
        .insert_task(wrong_root.clone())
        .expect("wrong root should insert");
    let ownership_error = store
        .complete_lease_and_task(
            &child.task_id,
            &wrong_root.task_id,
            &lease.lease_id,
            successful_attempt(),
        )
        .expect_err("completion with the wrong root must be rejected");
    assert!(matches!(ownership_error, DomainError::InvalidState { .. }));
    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should remain running")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        store
            .get_lease(&lease.lease_id)
            .expect("lease should remain active")
            .lease_status,
        TaskLeaseState::Active
    );

    let snapshots = Arc::new(Mutex::new(Vec::<TaskStoreSnapshot>::new()));
    let observed_snapshots = Arc::clone(&snapshots);
    store.set_checkpoint_callback(Box::new(move |snapshot| {
        observed_snapshots
            .lock()
            .expect("snapshot lock should not poison")
            .push(snapshot.clone());
        Ok(())
    }));
    assert!(
        store
            .complete_lease_and_task(
                &child.task_id,
                &child.root_task_id,
                &lease.lease_id,
                successful_attempt(),
            )
            .expect("completion should succeed")
    );
    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Completed
    );
    assert_eq!(
        store
            .get_lease(&lease.lease_id)
            .expect("completed lease should remain as history")
            .lease_status,
        TaskLeaseState::Completed
    );
    let snapshots = snapshots.lock().expect("snapshot lock should not poison");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0]
            .tasks
            .iter()
            .find(|task| task.task_id == child.task_id)
            .expect("snapshot should contain child")
            .status,
        TaskStatus::Completed
    );
    assert_eq!(
        snapshots[0]
            .leases
            .iter()
            .find(|snapshot_lease| snapshot_lease.lease_id == lease.lease_id)
            .expect("snapshot should contain lease")
            .lease_status,
        TaskLeaseState::Completed
    );

    let failed_store = TaskStore::new();
    let root = rooted_task("root-complete-persistence", "root-complete-persistence");
    let mut child = rooted_task("root-complete-persistence", "task-complete-persistence");
    child.status = TaskStatus::Pending;
    failed_store.insert_task(root).expect("root should insert");
    failed_store
        .insert_task(child.clone())
        .expect("child should insert");
    let lease = failed_store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-complete-persistence"),
            "executor",
            60_000,
        )
        .expect("grant should succeed")
        .expect("lease should be created");
    failed_store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::Persistence {
            message: "complete checkpoint unavailable".to_string(),
        })
    }));
    let error = failed_store
        .complete_lease_and_task(
            &child.task_id,
            &child.root_task_id,
            &lease.lease_id,
            successful_attempt(),
        )
        .expect_err("checkpoint failure must reject completion");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_eq!(
        failed_store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        failed_store
            .get_active_lease(&child.task_id)
            .expect("lease should remain active")
            .lease_id,
        lease.lease_id
    );
}

#[test]
fn revoke_lease_and_set_task_terminal_is_atomic_for_failed_and_killed() {
    let store = TaskStore::new();
    let root = rooted_task("root-terminal-atomic", "root-terminal-atomic");
    let mut child = rooted_task("root-terminal-atomic", "task-terminal-failed");
    child.status = TaskStatus::Pending;
    store.insert_task(root).expect("root should insert");
    store
        .insert_task(child.clone())
        .expect("child should insert");
    let lease = store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-terminal-atomic"),
            "executor",
            60_000,
        )
        .expect("grant should succeed")
        .expect("lease should be created");
    assert!(
        store
            .revoke_lease_and_set_task_terminal(
                &child.task_id,
                &child.root_task_id,
                Some(&lease.lease_id),
                TaskStatus::Failed,
                vec!["失败原因".to_string()],
            )
            .expect("failure transition should succeed")
    );
    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Failed
    );
    assert_eq!(
        store
            .get_lease(&lease.lease_id)
            .expect("failed lease should remain as history")
            .lease_status,
        TaskLeaseState::Revoked
    );

    let failed_store = TaskStore::new();
    let root = rooted_task("root-terminal-persistence", "root-terminal-persistence");
    let mut child = rooted_task("root-terminal-persistence", "task-terminal-persistence");
    child.status = TaskStatus::Pending;
    failed_store.insert_task(root).expect("root should insert");
    failed_store
        .insert_task(child.clone())
        .expect("child should insert");
    let lease = failed_store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-terminal-persistence"),
            "executor",
            60_000,
        )
        .expect("grant should succeed")
        .expect("lease should be created");
    failed_store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::Persistence {
            message: "terminal checkpoint unavailable".to_string(),
        })
    }));
    let error = failed_store
        .revoke_lease_and_set_task_terminal(
            &child.task_id,
            &child.root_task_id,
            Some(&lease.lease_id),
            TaskStatus::Failed,
            vec!["失败原因".to_string()],
        )
        .expect_err("checkpoint failure must reject terminal transition");
    assert!(matches!(error, DomainError::Persistence { .. }));
    assert_eq!(
        failed_store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        failed_store
            .get_active_lease(&child.task_id)
            .expect("lease should remain active")
            .lease_id,
        lease.lease_id
    );

    let kill_store = TaskStore::new();
    let root = rooted_task("root-terminal-killed", "root-terminal-killed");
    let mut pending = rooted_task("root-terminal-killed", "task-terminal-killed");
    pending.status = TaskStatus::Pending;
    kill_store.insert_task(root).expect("root should insert");
    kill_store
        .insert_task(pending.clone())
        .expect("pending task should insert");
    assert!(
        kill_store
            .revoke_active_lease_and_set_task_terminal(
                &pending.task_id,
                &pending.root_task_id,
                TaskStatus::Killed,
                Vec::new(),
            )
            .expect("pending task should be killable without lease")
    );
    assert_eq!(
        kill_store
            .get_task(&pending.task_id)
            .expect("pending task should exist")
            .status,
        TaskStatus::Killed
    );
    assert!(kill_store.get_active_lease(&pending.task_id).is_none());

    let active_kill_store = TaskStore::new();
    let root = rooted_task("root-active-kill", "root-active-kill");
    let mut running = rooted_task("root-active-kill", "task-active-kill");
    running.status = TaskStatus::Pending;
    active_kill_store
        .insert_task(root)
        .expect("active-kill root should insert");
    active_kill_store
        .insert_task(running.clone())
        .expect("active-kill task should insert");
    let lease = active_kill_store
        .grant_lease_and_start_task(
            &running.task_id,
            &running.root_task_id,
            &WorkerId::new("worker-active-kill"),
            "executor",
            60_000,
        )
        .expect("active-kill lease should grant")
        .expect("active-kill lease should exist");
    assert!(
        active_kill_store
            .revoke_active_lease_and_set_task_terminal(
                &running.task_id,
                &running.root_task_id,
                TaskStatus::Killed,
                Vec::new(),
            )
            .expect("atomic cancellation should revoke the current lease")
    );
    assert_eq!(
        active_kill_store
            .get_task(&running.task_id)
            .expect("active-kill task should remain")
            .status,
        TaskStatus::Killed
    );
    assert_eq!(
        active_kill_store
            .get_lease(&lease.lease_id)
            .expect("revoked lease should remain as history")
            .lease_status,
        TaskLeaseState::Revoked
    );
}

#[test]
fn active_lease_cancellation_is_linearizable_with_runner_lease_handoff() {
    for attempt in 0..32 {
        let store = Arc::new(TaskStore::new());
        let root_task_id = TaskId::new(format!("root-cancel-lease-handoff-{attempt}"));
        let task_id = TaskId::new(format!("task-cancel-lease-handoff-{attempt}"));
        let root = rooted_task(root_task_id.as_str(), root_task_id.as_str());
        let mut task = rooted_task(root_task_id.as_str(), task_id.as_str());
        task.status = TaskStatus::Pending;
        store.insert_task(root).expect("root should insert");
        store.insert_task(task).expect("pending task should insert");

        let barrier = Arc::new(Barrier::new(2));
        let grant_store = Arc::clone(&store);
        let grant_barrier = Arc::clone(&barrier);
        let grant_task_id = task_id.clone();
        let grant_root_task_id = root_task_id.clone();
        let grant = std::thread::spawn(move || {
            grant_barrier.wait();
            grant_store.grant_lease_and_start_task(
                &grant_task_id,
                &grant_root_task_id,
                &WorkerId::new(format!("worker-cancel-lease-handoff-{attempt}")),
                "executor",
                60_000,
            )
        });

        barrier.wait();
        assert!(
            store
                .revoke_active_lease_and_set_task_terminal(
                    &task_id,
                    &root_task_id,
                    TaskStatus::Killed,
                    Vec::new(),
                )
                .expect("cancellation must serialize with lease handoff")
        );
        let grant_result = grant.join().expect("lease handoff thread should finish");
        match grant_result {
            Ok(Some(lease)) => assert_eq!(
                store
                    .get_lease(&lease.lease_id)
                    .expect("won handoff lease should remain as history")
                    .lease_status,
                TaskLeaseState::Revoked,
                "if lease handoff wins the race, cancellation must revoke that exact active lease"
            ),
            Ok(None) => {}
            other => {
                panic!("cancellation and lease handoff had an invalid race result: {other:?}")
            }
        }
        let cancelled_task = store
            .get_task(&task_id)
            .expect("cancelled task should remain");
        assert_eq!(cancelled_task.status, TaskStatus::Killed);
        assert!(
            store.get_active_lease(&cancelled_task.task_id).is_none(),
            "linearized cancellation must leave no active lease"
        );
    }
}

#[test]
fn completed_can_only_be_written_through_completion_gate() {
    let store = TaskStore::new();
    let task_id = TaskId::new("task-status-gate");
    store
        .insert_task(task(task_id.as_str(), TaskStatus::Running))
        .expect("任务应插入");

    let error = store
        .update_status_checked(&task_id, TaskStatus::Completed)
        .expect_err("直接写入 Completed 必须被拒绝");

    assert!(matches!(error, DomainError::InvalidState { .. }));
    assert_eq!(
        store.get_task(&task_id).expect("任务应保留").status,
        TaskStatus::Running
    );
}

#[test]
fn completion_gate_rejects_active_lease_without_mutating_either_fact() {
    let store = TaskStore::new();
    let root = rooted_task("root-completion-lease-gate", "root-completion-lease-gate");
    let mut child = rooted_task("root-completion-lease-gate", "task-completion-lease-gate");
    child.status = TaskStatus::Pending;
    store.insert_task(root).expect("root should insert");
    store
        .insert_task(child.clone())
        .expect("child should insert");
    let lease = store
        .grant_lease_and_start_task(
            &child.task_id,
            &child.root_task_id,
            &WorkerId::new("worker-completion-lease-gate"),
            "executor",
            60_000,
        )
        .expect("lease should be granted")
        .expect("task should receive a lease");

    let error = store
        .complete_task(&child.task_id, successful_attempt())
        .expect_err("active lease must use the atomic lease completion path");
    assert!(matches!(error, DomainError::InvalidState { .. }));
    assert_eq!(
        store
            .get_task(&child.task_id)
            .expect("child should exist")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        store
            .get_lease(&lease.lease_id)
            .expect("lease should remain")
            .lease_status,
        TaskLeaseState::Active
    );
}

#[test]
fn completed_root_reopen_for_recovery_is_checkpointed_atomically() {
    let store = TaskStore::new();
    let root_id = TaskId::new("root-reopen-recovery");
    let mut root = rooted_task(root_id.as_str(), root_id.as_str());
    root.status = TaskStatus::Completed;
    store.insert_task(root).expect("root should insert");

    let snapshots = Arc::new(Mutex::new(Vec::<TaskStoreSnapshot>::new()));
    let observed_snapshots = Arc::clone(&snapshots);
    store.set_checkpoint_callback(Box::new(move |snapshot| {
        observed_snapshots
            .lock()
            .expect("snapshot lock should not poison")
            .push(snapshot.clone());
        Ok(())
    }));

    store
        .reopen_completed_root_for_recovery(&root_id)
        .expect("completed root should reopen for recovery");
    assert_eq!(
        store.get_task(&root_id).expect("root should exist").status,
        TaskStatus::Failed
    );
    assert_eq!(
        snapshots
            .lock()
            .expect("snapshot lock should not poison")
            .last()
            .and_then(|snapshot| snapshot.tasks.first())
            .map(|task| task.status),
        Some(TaskStatus::Failed)
    );
}

#[test]
fn recovery_generation_is_durable_and_failed_checkpoint_does_not_advance_it() {
    for start_root in [false, true] {
        let store = TaskStore::new();
        let id = TaskId::new("recovery-generation");
        let mut root = rooted_task(id.as_str(), id.as_str());
        root.status = TaskStatus::Failed;
        store.insert_task(root.clone()).unwrap();
        let recover = || {
            if start_root {
                store.start_failed_root_for_recovery(&id)
            } else {
                store.reopen_failed_task_for_recovery(&id)
            }
        };
        store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::InvalidState {
                message: "checkpoint failure".into(),
            })
        }));
        assert!(recover().is_err());
        assert_eq!(
            serde_json::to_value(store.get_task(&id).unwrap()).unwrap(),
            serde_json::to_value(root).unwrap()
        );
        let checkpoint = Arc::new(Mutex::new(None));
        let observed = checkpoint.clone();
        store.set_checkpoint_callback(Box::new(move |snapshot| {
            *observed.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        }));
        recover().unwrap();
        let recovered = store.get_task(&id).unwrap();
        assert_eq!(recovered.retry_count, 1);
        assert_eq!(
            recovered.status,
            if start_root {
                TaskStatus::Running
            } else {
                TaskStatus::Pending
            }
        );
        assert_eq!(
            serde_json::to_value(&checkpoint.lock().unwrap().as_ref().unwrap().tasks).unwrap(),
            serde_json::to_value(vec![recovered]).unwrap()
        );
    }
}

#[test]
fn completion_gate_rejects_missing_final_response_without_mutating_task() {
    let store = TaskStore::new();
    let task_id = TaskId::new("task-final-response-required");
    store
        .insert_task(task(task_id.as_str(), TaskStatus::Running))
        .expect("任务应插入");

    let error = store
        .complete_task(
            &task_id,
            TaskCompletionAttempt {
                final_response: Some("  ".to_string()),
                ..successful_attempt()
            },
        )
        .expect_err("空最终回复不能提交完成");

    assert!(matches!(error, DomainError::InvalidState { .. }));
    let persisted = store.get_task(&task_id).expect("任务应保留");
    assert_eq!(persisted.status, TaskStatus::Running);
    assert!(persisted.output_refs.is_empty());
    assert!(persisted.evidence_refs.is_empty());
}

#[test]
fn completion_gate_requires_matching_tool_evidence_and_persists_success() {
    let store = TaskStore::new();
    let task_id = TaskId::new("task-evidence-gate");
    let mut task = task(task_id.as_str(), TaskStatus::Running);
    task.completion_contract = magi_core::TaskCompletionContract::default()
        .with_evidence_requirements(vec![TaskEvidenceRequirement::SuccessfulToolCall {
            tool_name: "diagram_render".to_string(),
            minimum_successes: 1,
            argument_equals: [("/format".to_string(), json!("svg"))]
                .into_iter()
                .collect(),
            result_contains: vec!["rendered".to_string()],
        }]);
    store.insert_task(task).expect("任务应插入");

    let missing_error = store
        .complete_task(&task_id, successful_attempt())
        .expect_err("缺少工具成功证据不能完成");
    assert!(matches!(missing_error, DomainError::InvalidState { .. }));

    let attempt = TaskCompletionAttempt {
        evidence: vec![TaskCompletionEvidence::SuccessfulToolCall {
            call_id: "call-diagram".to_string(),
            tool_name: "diagram_render".to_string(),
            arguments: json!({"format": "svg"}),
            result: "rendered successfully".to_string(),
        }],
        ..successful_attempt()
    };
    store
        .complete_task(&task_id, attempt)
        .expect("满足合同后应提交完成");

    let persisted = store.get_task(&task_id).expect("任务应保留");
    assert_eq!(persisted.status, TaskStatus::Completed);
    assert_eq!(persisted.output_refs, ["output://task-store"]);
    assert_eq!(persisted.evidence_refs.len(), 1);
}

fn rooted_task(root_task_id: &str, task_id: &str) -> Task {
    let mut value = task(task_id, TaskStatus::Running);
    value.root_task_id = TaskId::new(root_task_id);
    value.parent_task_id = (task_id != root_task_id).then(|| TaskId::new(root_task_id));
    value
}

#[test]
fn execution_lease_rejects_terminal_ancestors_before_cascade_cleanup() {
    for status in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let store = TaskStore::new();
        let root = rooted_task("guard-root", "guard-root");
        let parent = rooted_task("guard-root", "guard-parent");
        let mut child = rooted_task("guard-root", "guard-child");
        child.parent_task_id = Some(parent.task_id.clone());
        for task in [&root, &parent, &child] {
            store.insert_task(task.clone()).unwrap();
        }
        let lease = store
            .grant_lease(
                &child.task_id,
                &root.task_id,
                &WorkerId::new("guard-worker"),
                "executor",
                60_000,
            )
            .unwrap();
        assert!(store.execution_lease_is_current(&child.task_id, &lease.lease_id));
        if status == TaskStatus::Completed {
            store
                .complete_task(&parent.task_id, successful_attempt())
                .unwrap();
        } else {
            store
                .revoke_active_lease_and_set_task_terminal(
                    &parent.task_id,
                    &root.task_id,
                    status,
                    vec![],
                )
                .unwrap();
        }
        assert!(
            store.get_active_lease(&child.task_id).is_some(),
            "不依赖 Runner 已经级联清理子任务"
        );
        assert!(!store.execution_lease_is_current(&child.task_id, &lease.lease_id));
    }
}

#[test]
fn lease_admission_rechecks_parent_after_runnable_scan() {
    for terminal in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let store = TaskStore::new();
        let root = rooted_task("admission-race-root", "admission-race-root");
        let parent = rooted_task("admission-race-root", "admission-race-parent");
        let mut child = rooted_task("admission-race-root", "admission-race-child");
        child.parent_task_id = Some(parent.task_id.clone());
        child.status = TaskStatus::Pending;
        for task in [&root, &parent, &child] {
            store.insert_task(task.clone()).unwrap();
        }
        assert!(
            store
                .get_runnable_leaves(&root.task_id)
                .iter()
                .any(|task| task.task_id == child.task_id)
        );
        if terminal == TaskStatus::Completed {
            store
                .complete_task(&parent.task_id, successful_attempt())
                .unwrap();
        } else {
            store
                .revoke_active_lease_and_set_task_terminal(
                    &parent.task_id,
                    &root.task_id,
                    terminal,
                    vec![],
                )
                .unwrap();
        }
        let before = store.snapshot();
        assert!(
            store
                .grant_lease_and_start_task(
                    &child.task_id,
                    &root.task_id,
                    &WorkerId::new("race-worker"),
                    "executor",
                    60_000
                )
                .unwrap()
                .is_none()
        );
        let after = store.snapshot();
        assert_eq!(
            serde_json::to_value(&after.tasks).unwrap(),
            serde_json::to_value(&before.tasks).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&after.leases).unwrap(),
            serde_json::to_value(&before.leases).unwrap()
        );
        assert!(store.get_active_lease(&child.task_id).is_none());
    }
}

#[test]
fn lease_handoff_loser_does_not_change_winning_execution() {
    let store = TaskStore::new();
    let root = rooted_task("handoff-root", "handoff-root");
    let mut child = rooted_task("handoff-root", "handoff-child");
    child.status = TaskStatus::Pending;
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child.clone()).unwrap();
    let winner = store
        .grant_lease_and_start_task(
            &child.task_id,
            &root.task_id,
            &WorkerId::new("winner"),
            "executor",
            60_000,
        )
        .unwrap()
        .unwrap();
    store.set_checkpoint_callback(Box::new(|_| panic!("losing admission must not checkpoint")));
    assert!(
        store
            .grant_lease_and_start_task(
                &child.task_id,
                &root.task_id,
                &WorkerId::new("loser"),
                "executor",
                60_000
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get_active_lease(&child.task_id).unwrap().lease_id,
        winner.lease_id
    );
    assert_eq!(
        store.get_task(&child.task_id).unwrap().status,
        TaskStatus::Running
    );
}

#[test]
fn runnable_and_lease_admission_reject_broken_ancestor_lineage() {
    for invalid in ["cycle", "missing", "mission", "root"] {
        let store = TaskStore::new();
        let root = rooted_task("lineage-root", "lineage-root");
        let parent = rooted_task("lineage-root", "lineage-parent");
        let mut child = rooted_task("lineage-root", "lineage-child");
        child.parent_task_id = Some(parent.task_id.clone());
        child.status = TaskStatus::Pending;
        for task in [&root, &parent, &child] {
            store.insert_task(task.clone()).unwrap();
        }
        let mut tasks = store.tasks.write().unwrap();
        let parent = tasks.get_mut(&parent.task_id).unwrap();
        match invalid {
            "cycle" => parent.parent_task_id = Some(parent.task_id.clone()),
            "missing" => parent.parent_task_id = Some(TaskId::new("missing")),
            "mission" => parent.mission_id = MissionId::new("other-mission"),
            "root" => parent.root_task_id = TaskId::new("other-root"),
            _ => unreachable!(),
        }
        drop(tasks);
        assert!(
            store.get_runnable_leaves(&root.task_id).is_empty(),
            "{invalid}"
        );
        assert!(
            store
                .grant_lease_and_start_task(
                    &child.task_id,
                    &root.task_id,
                    &WorkerId::new("lineage-worker"),
                    "executor",
                    60_000
                )
                .unwrap()
                .is_none(),
            "{invalid}"
        );
        assert!(store.get_active_lease(&child.task_id).is_none());
    }
}

#[test]
fn lease_completion_drops_result_after_ancestor_closed_before_commit() {
    for terminal in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let store = TaskStore::new();
        let root = rooted_task("completion-race-root", "completion-race-root");
        let parent = rooted_task("completion-race-root", "completion-race-parent");
        let mut child = rooted_task("completion-race-root", "completion-race-child");
        child.parent_task_id = Some(parent.task_id.clone());
        child.status = TaskStatus::Pending;
        for task in [&root, &parent, &child] {
            store.insert_task(task.clone()).unwrap();
        }
        let lease = store
            .grant_lease_and_start_task(
                &child.task_id,
                &root.task_id,
                &WorkerId::new("race-worker"),
                "executor",
                60_000,
            )
            .unwrap()
            .unwrap();
        assert!(store.execution_lease_is_current(&child.task_id, &lease.lease_id));
        if terminal == TaskStatus::Completed {
            store
                .complete_task(&parent.task_id, successful_attempt())
                .unwrap();
        } else {
            store
                .revoke_active_lease_and_set_task_terminal(
                    &parent.task_id,
                    &root.task_id,
                    terminal,
                    vec![],
                )
                .unwrap();
        }
        let before = store.snapshot();
        assert!(
            !store
                .complete_lease_and_task(
                    &child.task_id,
                    &root.task_id,
                    &lease.lease_id,
                    successful_attempt()
                )
                .unwrap()
        );
        let after = store.snapshot();
        assert_eq!(
            serde_json::to_value(&after.tasks).unwrap(),
            serde_json::to_value(&before.tasks).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&after.leases).unwrap(),
            serde_json::to_value(&before.leases).unwrap()
        );
        assert_eq!(
            store.get_task(&child.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            store.get_active_lease(&child.task_id).unwrap().lease_id,
            lease.lease_id
        );
    }
}

#[test]
fn execution_lease_rejects_missing_or_cyclic_parent_chain() {
    let store = TaskStore::new();
    let root = rooted_task("guard-lineage", "guard-lineage");
    let child = rooted_task("guard-lineage", "guard-lineage-child");
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child.clone()).unwrap();
    let lease = store
        .grant_lease(
            &child.task_id,
            &root.task_id,
            &WorkerId::new("guard-worker"),
            "executor",
            60_000,
        )
        .unwrap();
    for parent in ["missing-parent", child.task_id.as_str()] {
        store
            .tasks
            .write()
            .unwrap()
            .get_mut(&child.task_id)
            .unwrap()
            .parent_task_id = Some(TaskId::new(parent));
        assert!(!store.execution_lease_is_current(&child.task_id, &lease.lease_id));
    }
}

fn activation_task(root: &str, id: &str, previous: Option<&Task>) -> Task {
    let mut value = rooted_task(root, id);
    value.status = TaskStatus::Pending;
    let source = previous.and_then(Task::agent_activation);
    let mut binding = magi_core::TaskExecutorBinding::for_role("executor")
        .with_capability_ids(vec!["general_engineering".into()]);
    binding.agent_activation = Some(magi_core::AgentActivation {
        agent_id: source.map_or_else(|| value.task_id.clone(), |source| source.agent_id.clone()),
        session_id: magi_core::SessionId::new("activation-session"),
        lifetime: magi_core::AgentLifetime::Session,
        generation: source.map_or(1, |source| source.generation + 1),
        previous_task_id: previous.map(|task| task.task_id.clone()),
    });
    value.executor_binding = Some(binding);
    value
}

fn transfer_fixture() -> (TaskStore, Task, Task, Task, magi_core::AgentRunTransfer) {
    let store = TaskStore::new();
    let mut root = rooted_task("transfer-root", "transfer-root");
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    let mut child = activation_task("transfer-root", "transfer-agent", None);
    child.executor_binding.as_mut().unwrap().delegation_allowed = true;
    let mut leaf = activation_task("transfer-root", "transfer-leaf", None);
    leaf.parent_task_id = Some(child.task_id.clone());
    for task in [&root, &child, &leaf] {
        store.insert_task(task.clone()).unwrap();
    }
    let transfer = magi_core::AgentRunTransfer {
        operation_id: "handoff-operation".into(),
        session_id: magi_core::SessionId::new("activation-session"),
        source_turn_id: "turn-transfer-source".into(),
        transferred_by: root.task_id.clone(),
        activation_generation: 1,
        owner: magi_core::AgentRunOwner::Session,
        transferred_at: UtcMillis::now(),
    };
    (store, root, child, leaf, transfer)
}

#[test]
fn goal_task_activity_uses_durable_status_and_rejects_missing_facts() {
    let session = magi_core::SessionId::new("goal-session");
    let goal = magi_core::GoalId::new("goal");
    for status in [
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let store = TaskStore::new();
        let root = rooted_task("goal-root", "goal-root");
        let mut child = rooted_task("goal-root", "goal-child");
        child.status = status;
        store.insert_task(root.clone()).unwrap();
        store.insert_task(child.clone()).unwrap();
        let snapshot = store.snapshot();
        let restored = TaskStore::restore_checked(TaskStoreCheckpoint {
            tasks: snapshot.tasks,
            leases: snapshot.leases,
        })
        .unwrap();
        for store in [&store, &restored] {
            assert_eq!(
                store
                    .goal_has_active_bound_tasks(
                        &root.task_id,
                        &session,
                        &goal,
                        &[root.task_id.clone(), child.task_id.clone()]
                    )
                    .unwrap(),
                matches!(status, TaskStatus::Pending | TaskStatus::Running)
            );
            assert!(
                store
                    .goal_has_active_bound_tasks(
                        &root.task_id,
                        &session,
                        &goal,
                        &[TaskId::new("missing")]
                    )
                    .is_err()
            );
        }
    }
}

#[test]
fn goal_task_activity_follows_session_and_goal_transfers() {
    let goal = magi_core::GoalId::new("goal-owner");
    for goal_owned in [false, true] {
        let (_, root, mut child, mut leaf, mut transfer) = transfer_fixture();
        if goal_owned {
            for task in [&mut child, &mut leaf] {
                task.executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_activation
                    .as_mut()
                    .unwrap()
                    .lifetime = magi_core::AgentLifetime::Goal {
                    goal_id: goal.clone(),
                };
            }
            transfer.owner = magi_core::AgentRunOwner::Goal {
                goal_id: goal.clone(),
                control_revision: 1,
            };
        }
        let store = TaskStore::new();
        for task in [&root, &child, &leaf] {
            store.insert_task(task.clone()).unwrap();
        }
        let session = transfer.session_id.clone();
        let bound = [child.task_id.clone(), leaf.task_id.clone()];
        assert!(
            store
                .goal_has_active_bound_tasks(&root.task_id, &session, &goal, &bound)
                .unwrap()
        );
        store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
            .unwrap();
        assert_eq!(
            store
                .goal_has_active_bound_tasks(&root.task_id, &session, &goal, &bound)
                .unwrap(),
            goal_owned
        );
        assert!(
            !store
                .goal_has_active_bound_tasks(
                    &root.task_id,
                    &session,
                    &magi_core::GoalId::new("other-goal"),
                    &bound
                )
                .unwrap()
        );
        assert!(
            store
                .goal_has_active_bound_tasks(
                    &root.task_id,
                    &magi_core::SessionId::new("other-session"),
                    &goal,
                    &bound
                )
                .is_err()
        );
        let snapshot = store.snapshot();
        let restored = TaskStore::restore_checked(TaskStoreCheckpoint {
            tasks: snapshot.tasks,
            leases: snapshot.leases,
        })
        .unwrap();
        assert_eq!(
            restored
                .goal_has_active_bound_tasks(&root.task_id, &session, &goal, &bound)
                .unwrap(),
            goal_owned
        );
    }
}

#[test]
fn restart_reconciles_queued_agent_scope_without_worker_snapshot_or_mainline() {
    let (store, root, child, leaf, transfer) = transfer_fixture();
    store
        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
        .unwrap();
    let sibling = rooted_task(root.task_id.as_str(), "untransferred-pending-sibling");
    store.insert_task(sibling.clone()).unwrap();
    let snapshot = store.snapshot();
    let restored = TaskStore::restore_checked(TaskStoreCheckpoint {
        tasks: snapshot.tasks,
        leases: snapshot.leases,
    })
    .unwrap();
    let before = restored.snapshot();
    restored.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::Persistence {
            message: "restart checkpoint failed".into(),
        })
    }));
    assert!(restored.reconcile_volatile_runtime_after_restore().is_err());
    assert_eq!(
        serde_json::to_value(restored.snapshot().tasks).unwrap(),
        serde_json::to_value(before.tasks).unwrap()
    );
    restored.set_checkpoint_callback(Box::new(|_| Ok(())));
    assert_eq!(
        restored.reconcile_volatile_runtime_after_restore().unwrap(),
        (0, 4)
    );
    for task in [root, child, leaf, sibling] {
        assert_eq!(
            restored.get_task(&task.task_id).unwrap().status,
            TaskStatus::Failed
        );
    }
    assert_eq!(
        restored.reconcile_volatile_runtime_after_restore().unwrap(),
        (0, 0)
    );
}

#[test]
fn orphaned_execution_cleanup_is_atomic_and_preserves_transferred_agents() {
    let (store, root, child, leaf, transfer) = transfer_fixture();
    let sibling = activation_task(root.task_id.as_str(), "orphan-sibling", None);
    store.insert_task(sibling.clone()).unwrap();
    store
        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
        .unwrap();
    store
        .revoke_active_lease_and_set_task_terminal(
            &root.task_id,
            &root.task_id,
            TaskStatus::Failed,
            vec!["source failed".into()],
        )
        .unwrap();
    let before = {
        let snapshot = store.snapshot();
        serde_json::to_value((&snapshot.tasks, &snapshot.leases)).unwrap()
    };
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::InvalidState {
            message: "orphan checkpoint failed".into(),
        })
    }));
    assert!(
        store
            .revoke_orphaned_execution_descendants(&root.task_id)
            .is_err()
    );
    assert_eq!(
        {
            let snapshot = store.snapshot();
            serde_json::to_value((&snapshot.tasks, &snapshot.leases)).unwrap()
        },
        before
    );
    store.set_checkpoint_callback(Box::new(|_| Ok(())));
    assert!(
        store
            .revoke_orphaned_execution_descendants(&root.task_id)
            .unwrap()
    );
    assert!(
        !store
            .revoke_orphaned_execution_descendants(&root.task_id)
            .unwrap()
    );
    assert_eq!(
        store.get_task(&sibling.task_id).unwrap().status,
        TaskStatus::Killed
    );
    for original in [&child, &leaf] {
        assert_eq!(
            store.get_task(&original.task_id).unwrap().status,
            original.status
        );
    }
    // 转交根自己结束后，由它的拥有方收口内部后代，不能沿原 root 清理。
    store
        .revoke_active_lease_and_set_task_terminal(
            &child.task_id,
            &root.task_id,
            TaskStatus::Failed,
            vec![],
        )
        .unwrap();
    assert!(
        !store
            .revoke_orphaned_execution_descendants(&root.task_id)
            .unwrap()
    );
    assert!(
        store
            .revoke_orphaned_execution_descendants(&child.task_id)
            .unwrap()
    );
    assert_eq!(
        store.get_task(&leaf.task_id).unwrap().status,
        TaskStatus::Killed
    );
}

#[test]
fn unrunnable_execution_failure_rechecks_candidates_and_rolls_back_failed_commit() {
    for changed in ["transferred", "completed", "started", "active"] {
        let (store, root, child, _, transfer) = transfer_fixture();
        match changed {
            "transferred" => {
                store
                    .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
                    .unwrap();
            }
            "completed" => {
                let lease = store
                    .claim_task_for_execution_scope(
                        &child.task_id,
                        &root.task_id,
                        &WorkerId::new("completion-worker"),
                        "executor",
                        60_000,
                    )
                    .unwrap()
                    .unwrap();
                store
                    .complete_lease_and_task(
                        &child.task_id,
                        &root.task_id,
                        &lease.lease_id,
                        TaskCompletionAttempt {
                            output_refs: vec![],
                            final_response: Some("completed while runner scanned".into()),
                            evidence: vec![],
                        },
                    )
                    .unwrap();
            }
            "started" => {
                assert!(
                    store
                        .claim_task_for_execution_scope(
                            &child.task_id,
                            &root.task_id,
                            &WorkerId::new("late-worker"),
                            "executor",
                            60_000,
                        )
                        .unwrap()
                        .is_some()
                );
            }
            _ => {}
        }
        let before = {
            let snapshot = store.snapshot();
            serde_json::to_value((&snapshot.tasks, &snapshot.leases)).unwrap()
        };
        store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::InvalidState {
                message: "failure checkpoint failed".into(),
            })
        }));
        let result = store.fail_unrunnable_execution_scope(
            &root.task_id,
            std::slice::from_ref(&child.task_id),
            vec!["old scan".into()],
        );
        if changed == "active" {
            assert!(result.is_err());
        } else {
            assert!(
                !result.unwrap(),
                "stale {changed} candidate must not fail the source"
            );
        }
        assert_eq!(
            {
                let snapshot = store.snapshot();
                serde_json::to_value((&snapshot.tasks, &snapshot.leases)).unwrap()
            },
            before
        );
        store.set_checkpoint_callback(Box::new(|_| Ok(())));
        assert_eq!(
            store
                .fail_unrunnable_execution_scope(
                    &root.task_id,
                    std::slice::from_ref(&child.task_id),
                    vec!["confirmed unavailable".into()],
                )
                .unwrap(),
            changed == "active"
        );
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            if changed == "active" {
                TaskStatus::Failed
            } else {
                TaskStatus::Running
            }
        );
    }
}

#[test]
fn unrunnable_execution_failure_serializes_with_agent_transfer() {
    for _ in 0..24 {
        let (store, root, child, leaf, transfer) = transfer_fixture();
        let store = Arc::new(store);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let failure_store = store.clone();
        let failure_barrier = barrier.clone();
        let root_id = root.task_id.clone();
        let child_id = child.task_id.clone();
        let failure = std::thread::spawn(move || {
            failure_barrier.wait();
            failure_store
                .fail_unrunnable_execution_scope(&root_id, &[child_id], vec![])
                .unwrap()
        });
        barrier.wait();
        let transferred = store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
            .is_ok();
        assert_ne!(failure.join().unwrap(), transferred);
        for task in [&root, &child, &leaf] {
            assert_eq!(
                store.get_task(&task.task_id).unwrap().status,
                if transferred {
                    task.status
                } else {
                    TaskStatus::Failed
                }
            );
        }
    }
}

#[test]
fn transferred_execution_scope_survives_parent_terminal_and_keeps_audit_leases() {
    for terminal in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let (store, root, child, leaf, transfer) = transfer_fixture();
        store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
            .unwrap();
        assert_eq!(store.collect_subtree_ids(&root.task_id).len(), 3);
        assert_eq!(
            store.collect_execution_subtree_ids(&root.task_id),
            vec![root.task_id.clone()]
        );
        assert_eq!(
            store.execution_scope_root(&leaf.task_id).unwrap(),
            child.task_id
        );
        // 原 Runner 的旧扫描不能在转交提交后领取子任务。
        assert!(
            store
                .claim_task_for_execution_scope(
                    &child.task_id,
                    &root.task_id,
                    &WorkerId::new("stale-parent-runner"),
                    "executor",
                    60_000
                )
                .unwrap()
                .is_none()
        );
        let lease = store
            .claim_task_for_execution_scope(
                &child.task_id,
                &child.task_id,
                &WorkerId::new("owner-runner"),
                "executor",
                60_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(lease.root_task_id, root.task_id, "租约保留原审计 root");
        if terminal == TaskStatus::Completed {
            store
                .complete_task(&root.task_id, successful_attempt())
                .unwrap();
        } else {
            store
                .revoke_active_leases_and_set_execution_subtree_terminal(
                    &root.task_id,
                    terminal,
                    vec![],
                )
                .unwrap();
        }
        assert!(store.execution_lease_is_current(&child.task_id, &lease.lease_id));
        assert!(
            store
                .collect_leases_for_execution_scope(&root.task_id, false)
                .is_empty()
        );
        assert_eq!(
            store.collect_active_leases(&child.task_id),
            vec![(child.task_id.clone(), lease.lease_id.clone())]
        );
        assert!(!store.heartbeat_lease_for_execution_scope(
            &root.task_id,
            &child.task_id,
            &lease.lease_id
        ));
        assert!(store.heartbeat_lease_for_execution_scope(
            &child.task_id,
            &child.task_id,
            &lease.lease_id
        ));
        store
            .validate_agent_delegation(&store.get_task(&child.task_id).unwrap())
            .unwrap();
        let leaf_lease = store
            .claim_task_for_execution_scope(
                &leaf.task_id,
                &child.task_id,
                &WorkerId::new("leaf-runner"),
                "executor",
                60_000,
            )
            .unwrap()
            .unwrap();
        assert!(
            store
                .complete_lease_and_task(
                    &leaf.task_id,
                    &root.task_id,
                    &leaf_lease.lease_id,
                    successful_attempt()
                )
                .unwrap()
        );
        assert!(
            store
                .complete_lease_and_task(
                    &child.task_id,
                    &root.task_id,
                    &lease.lease_id,
                    successful_attempt()
                )
                .unwrap()
        );
        let restored = TaskStore::restore_checked(TaskStoreCheckpoint {
            tasks: store.snapshot().tasks,
            leases: store.snapshot().leases,
        })
        .unwrap();
        assert_eq!(
            restored.execution_scope_root(&leaf.task_id).unwrap(),
            child.task_id
        );
        assert_eq!(restored.get_task(&root.task_id).unwrap().status, terminal);
    }
}

#[test]
fn transferred_execution_scope_cancel_is_atomic_and_full_shutdown_crosses_boundaries() {
    let (store, root, child, leaf, transfer) = transfer_fixture();
    store
        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
        .unwrap();
    let lease = store
        .claim_task_for_execution_scope(
            &child.task_id,
            &child.task_id,
            &WorkerId::new("owner-worker"),
            "executor",
            60_000,
        )
        .unwrap()
        .unwrap();
    let before = store.snapshot();
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::InvalidState {
            message: "disk unavailable".into(),
        })
    }));
    assert!(
        store
            .revoke_active_leases_and_set_execution_subtree_terminal(
                &child.task_id,
                TaskStatus::Killed,
                vec![]
            )
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(store.snapshot().tasks).unwrap(),
        serde_json::to_value(before.tasks).unwrap()
    );
    assert!(store.execution_lease_is_current(&child.task_id, &lease.lease_id));
    store.set_checkpoint_callback(Box::new(|_| Ok(())));
    store
        .revoke_active_leases_and_set_execution_subtree_terminal(
            &root.task_id,
            TaskStatus::Killed,
            vec![],
        )
        .unwrap();
    assert!(store.execution_lease_is_current(&child.task_id, &lease.lease_id));
    // 关闭 Session/整个任务树必须仍能停止所有已转交执行，不能只结束原拥有方。
    store
        .revoke_active_leases_and_set_subtree_terminal(&root.task_id, TaskStatus::Killed, vec![])
        .unwrap();
    for task in [&root, &child, &leaf] {
        assert_eq!(
            store.get_task(&task.task_id).unwrap().status,
            TaskStatus::Killed
        );
        assert!(store.get_active_lease(&task.task_id).is_none());
    }
    assert!(!store.execution_lease_is_current(&child.task_id, &lease.lease_id));
}

#[test]
fn transferred_execution_scope_rechecks_expiry_after_scan_and_before_commit() {
    let (store, root, child, _, transfer) = transfer_fixture();
    let lease = store
        .claim_task_for_execution_scope(
            &child.task_id,
            &root.task_id,
            &WorkerId::new("old-scope"),
            "executor",
            1,
        )
        .unwrap()
        .unwrap();
    // 固定旧租约的过期事实，不用真实 sleep 制造竞争窗口。
    let mut checkpoint = store.snapshot();
    let observed = checkpoint
        .leases
        .iter_mut()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    observed.granted_at = UtcMillis(1);
    observed.heartbeat_at = UtcMillis(1);
    observed.expires_at = UtcMillis(2);
    let store = TaskStore::restore_checked(TaskStoreCheckpoint {
        tasks: checkpoint.tasks,
        leases: checkpoint.leases,
    })
    .unwrap();
    assert_eq!(
        store.collect_expired_leases(&root.task_id),
        vec![(child.task_id.clone(), lease.lease_id.clone())]
    );
    store
        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
        .unwrap();
    assert!(
        !store
            .expire_lease_for_execution_scope(&root.task_id, &child.task_id, &lease.lease_id)
            .unwrap(),
        "旧扫描不能结束转交后的执行"
    );
    assert!(
        store
            .expire_lease_for_execution_scope(&child.task_id, &child.task_id, &lease.lease_id)
            .unwrap()
    );
    assert_eq!(
        store.get_task(&child.task_id).unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(
        store.get_lease(&lease.lease_id).unwrap().root_task_id,
        root.task_id
    );
    assert_eq!(
        store.get_task(&root.task_id).unwrap().status,
        TaskStatus::Running
    );
}

#[test]
fn transferred_execution_scope_claim_and_source_cancel_serialize_with_handoff() {
    for _ in 0..16 {
        let (store, root, child, _, transfer) = transfer_fixture();
        let store = Arc::new(store);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let cancel = std::thread::spawn({
            let store = store.clone();
            let barrier = barrier.clone();
            let root = root.task_id.clone();
            move || {
                barrier.wait();
                store
                    .revoke_active_leases_and_set_execution_subtree_terminal(
                        &root,
                        TaskStatus::Killed,
                        vec![],
                    )
                    .unwrap();
            }
        });
        barrier.wait();
        let transferred = store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
            .is_ok();
        cancel.join().unwrap();
        let result = store
            .claim_task_for_execution_scope(
                &child.task_id,
                &child.task_id,
                &WorkerId::new("owner"),
                "executor",
                60_000,
            )
            .unwrap();
        assert_eq!(result.is_some(), transferred);
        assert!(
            store
                .claim_task_for_execution_scope(
                    &child.task_id,
                    &root.task_id,
                    &WorkerId::new("old-owner"),
                    "executor",
                    60_000
                )
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn subtree_terminal_commit_is_atomic_durable_and_notifies_after_all_nodes_settle() {
    use std::sync::atomic::AtomicUsize;
    let (store, root, child, leaf, _) = transfer_fixture();
    let store = Arc::new(store);
    let lease = store
        .grant_lease_and_start_task(
            &child.task_id,
            &root.task_id,
            &WorkerId::new("atomic-cancel-worker"),
            "executor",
            60_000,
        )
        .unwrap()
        .unwrap();
    let before = store.snapshot();
    let notices = Arc::new(AtomicUsize::new(0));
    let received = notices.clone();
    let source = Arc::downgrade(&store);
    store.set_status_change_callback(Box::new(move |_, _, _, _| {
        let snapshot = source.upgrade().unwrap().snapshot();
        assert!(
            snapshot
                .tasks
                .iter()
                .all(|task| task.status == TaskStatus::Killed)
        );
        assert!(
            snapshot
                .leases
                .iter()
                .all(|lease| lease.lease_status != TaskLeaseState::Active)
        );
        received.fetch_add(1, Ordering::SeqCst);
    }));
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::InvalidState {
            message: "disk failed".into(),
        })
    }));
    assert!(
        store
            .revoke_active_leases_and_set_subtree_terminal(
                &root.task_id,
                TaskStatus::Killed,
                vec![]
            )
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(store.snapshot().tasks).unwrap(),
        serde_json::to_value(before.tasks).unwrap()
    );
    assert_eq!(
        store.get_lease(&lease.lease_id).unwrap().lease_status,
        TaskLeaseState::Active
    );
    assert_eq!(notices.load(Ordering::SeqCst), 0);
    let commits = Arc::new(AtomicUsize::new(0));
    let recorded = commits.clone();
    store.set_checkpoint_callback(Box::new(move |snapshot| {
        assert_eq!(snapshot.changed_root_ids, [TaskId::new("transfer-root")]);
        assert!(
            snapshot
                .tasks
                .iter()
                .all(|task| task.status == TaskStatus::Killed)
        );
        assert!(
            snapshot
                .leases
                .iter()
                .all(|lease| lease.lease_status != TaskLeaseState::Active)
        );
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    assert!(
        store
            .revoke_active_leases_and_set_subtree_terminal(
                &root.task_id,
                TaskStatus::Killed,
                vec!["cancelled".into()]
            )
            .unwrap()
    );
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(notices.load(Ordering::SeqCst), 3);
    assert!(
        !store
            .revoke_active_leases_and_set_subtree_terminal(
                &root.task_id,
                TaskStatus::Killed,
                vec![]
            )
            .unwrap()
    );
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    let directory = tempfile::tempdir().unwrap();
    store
        .checkpoint_to_projection_directory(directory.path())
        .unwrap();
    let restored = TaskStore::restore_from_projection_directory(directory.path())
        .unwrap()
        .unwrap();
    for task in [root, child, leaf] {
        assert_eq!(
            restored.get_task(&task.task_id).unwrap().status,
            TaskStatus::Killed
        );
    }
    assert_eq!(
        restored.get_lease(&lease.lease_id).unwrap().lease_status,
        TaskLeaseState::Revoked
    );
}

#[test]
fn subtree_terminal_commit_supports_direct_agent_scope_and_preserves_siblings() {
    let (store, root, child, leaf, _) = transfer_fixture();
    let sibling = activation_task(root.task_id.as_str(), "cancel-sibling", None);
    store.insert_task(sibling.clone()).unwrap();
    store
        .revoke_active_leases_and_set_subtree_terminal(&child.task_id, TaskStatus::Killed, vec![])
        .unwrap();
    for task in [&child, &leaf] {
        assert_eq!(
            store.get_task(&task.task_id).unwrap().status,
            TaskStatus::Killed
        );
    }
    assert_eq!(store.get_task(&root.task_id).unwrap().status, root.status);
    assert_eq!(
        store.get_task(&sibling.task_id).unwrap().status,
        sibling.status
    );
}

#[test]
fn subtree_terminal_commit_reports_bad_node_without_leaving_valid_descendants_running() {
    let (store, root, child, leaf, _) = transfer_fixture();
    store
        .tasks
        .write()
        .unwrap()
        .get_mut(&child.task_id)
        .unwrap()
        .mission_id = MissionId::new("corrupt-mission");
    let error = store
        .revoke_active_leases_and_set_subtree_terminal(&root.task_id, TaskStatus::Killed, vec![])
        .unwrap_err();
    assert!(error.to_string().contains(child.task_id.as_str()));
    assert_eq!(
        store.get_task(&root.task_id).unwrap().status,
        TaskStatus::Killed
    );
    assert_eq!(
        store.get_task(&leaf.task_id).unwrap().status,
        TaskStatus::Killed
    );
    assert_eq!(store.get_task(&child.task_id).unwrap().status, child.status);
}

#[test]
fn subtree_terminal_commit_serializes_with_new_activation_and_runner_lease() {
    for attempt in 0..24 {
        let (store, root, child, _, _) = transfer_fixture();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(3));
        let insert_store = store.clone();
        let insert_barrier = barrier.clone();
        let mut late = activation_task(
            root.task_id.as_str(),
            &format!("late-agent-{attempt}"),
            None,
        );
        late.parent_task_id = Some(child.task_id.clone());
        let insertion = std::thread::spawn(move || {
            insert_barrier.wait();
            insert_store.insert_task(late)
        });
        let lease_store = store.clone();
        let lease_barrier = barrier.clone();
        let lease_task = child.clone();
        let leasing = std::thread::spawn(move || {
            lease_barrier.wait();
            lease_store.grant_lease_and_start_task(
                &lease_task.task_id,
                &lease_task.root_task_id,
                &WorkerId::new("racing-worker"),
                "executor",
                60_000,
            )
        });
        barrier.wait();
        store
            .revoke_active_leases_and_set_subtree_terminal(
                &root.task_id,
                TaskStatus::Killed,
                vec![],
            )
            .unwrap();
        let _ = insertion.join().unwrap();
        let _ = leasing.join().unwrap().unwrap();
        assert!(
            store
                .snapshot()
                .tasks
                .iter()
                .all(|task| task.status == TaskStatus::Killed)
        );
        assert!(store.collect_all_active_leases().is_empty());
    }
}

#[test]
fn agent_transfer_query_distinguishes_untransferred_missing_and_broken_source() {
    let (store, root, child, _, _) = transfer_fixture();
    assert!(
        store
            .agent_run_transfer_for_task(&child.task_id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.agent_run_transfer_for_task(&TaskId::new("missing")),
        Err(DomainError::NotFound { .. })
    ));
    for mutation in 0..3 {
        let (store, root, child, _, _) = transfer_fixture();
        {
            let mut tasks = store.tasks.write().unwrap();
            match mutation {
                0 => {
                    tasks.remove(&root.task_id);
                }
                1 => {
                    tasks.get_mut(&root.task_id).unwrap().mission_id = MissionId::new("foreign");
                }
                _ => {
                    tasks.get_mut(&root.task_id).unwrap().parent_task_id =
                        Some(child.task_id.clone());
                }
            }
        }
        assert!(
            store.agent_run_transfer_for_task(&child.task_id).is_err(),
            "{mutation}"
        );
    }
    assert!(
        store
            .agent_run_transfer_for_task(&root.task_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn agent_subtree_transfer_is_durable_atomic_and_keeps_task_parentage() {
    let (store, root, child, leaf, transfer) = transfer_fixture();
    let before = store.snapshot();
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::InvalidState {
            message: "disk failed".into(),
        })
    }));
    assert!(
        store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone())
            .is_err()
    );
    assert!(
        store
            .agent_run_transfer_for_task(&child.task_id)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .agent_run_transfer_for_task(&leaf.task_id)
            .unwrap()
            .is_none()
    );
    store.set_checkpoint_callback(Box::new(|_| Ok(())));
    assert!(
        store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone())
            .unwrap()
    );
    assert!(
        !store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone())
            .unwrap()
    );
    let temp = tempfile::tempdir().unwrap();
    store
        .checkpoint_to_projection_directory(temp.path())
        .unwrap();
    let restored = TaskStore::restore_from_projection_directory(temp.path())
        .unwrap()
        .unwrap();
    let inherited = restored
        .agent_run_transfer_for_task(&leaf.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(inherited.task_id, child.task_id);
    assert_eq!(inherited.agent_run_transfer(), Some(&transfer));
    assert!(
        restored
            .agent_run_transfer_for_task(&root.task_id)
            .unwrap()
            .is_none()
    );
    let after = restored.snapshot();
    assert_eq!(before.tasks.len(), after.tasks.len());
    for original in before.tasks {
        let current = restored.get_task(&original.task_id).unwrap();
        assert_eq!(current.parent_task_id, original.parent_task_id);
        assert_eq!(current.root_task_id, original.root_task_id);
        assert_eq!(current.policy_snapshot, original.policy_snapshot);
        assert_eq!(current.agent_activation(), original.agent_activation());
    }
    let mut changed = transfer;
    changed.source_turn_id = "different-turn".into();
    assert!(
        restored
            .transfer_agent_subtree(&root.task_id, &child.task_id, changed)
            .is_err()
    );
}

#[test]
fn agent_subtree_transfer_rejects_cross_owner_dependencies_on_commit_and_restore() {
    for mutation in 0..5 {
        let (store, root, child, leaf, transfer) = transfer_fixture();
        let mut checkpoint = store.snapshot();
        let target = if mutation < 2 {
            &root.task_id
        } else {
            &leaf.task_id
        };
        let task = checkpoint
            .tasks
            .iter_mut()
            .find(|task| &task.task_id == target)
            .unwrap();
        match mutation {
            0 => task.required_children.push(child.task_id.clone()),
            1 => task.dependency_ids.push(child.task_id.clone()),
            2 => task.dependency_ids.push(root.task_id.clone()),
            3 => task.required_children.push(root.task_id.clone()),
            _ => {
                task.executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_activation
                    .as_mut()
                    .unwrap()
                    .session_id = magi_core::SessionId::new("foreign-session")
            }
        }
        let forged = TaskStoreCheckpoint {
            tasks: checkpoint.tasks.clone(),
            leases: checkpoint.leases.clone(),
        };
        let store = TaskStore::restore_checked(TaskStoreCheckpoint {
            tasks: checkpoint.tasks,
            leases: checkpoint.leases,
        })
        .unwrap();
        assert!(
            store
                .transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone())
                .is_err(),
            "commit mutation {mutation}"
        );
        assert!(
            store
                .agent_run_transfer_for_task(&leaf.task_id)
                .unwrap()
                .is_none()
        );
        let mut forged = forged;
        forged
            .tasks
            .iter_mut()
            .find(|task| task.task_id == child.task_id)
            .unwrap()
            .executor_binding
            .as_mut()
            .unwrap()
            .agent_run_transfer = Some(transfer);
        assert!(
            TaskStore::restore_checked(forged).is_err(),
            "restore mutation {mutation}"
        );
    }
}

#[test]
fn agent_subtree_transfer_serializes_with_parent_cancellation() {
    for _ in 0..16 {
        let (store, root, child, _, transfer) = transfer_fixture();
        let store = Arc::new(store);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let task = root.clone();
        let cancel_store = Arc::clone(&store);
        let cancel_barrier = Arc::clone(&barrier);
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            cancel_store
                .revoke_active_lease_and_set_task_terminal(
                    &task.task_id,
                    &task.task_id,
                    TaskStatus::Killed,
                    Vec::new(),
                )
                .unwrap();
        });
        barrier.wait();
        let result = store.transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone());
        cancel.join().unwrap();
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            TaskStatus::Killed
        );
        match result {
            Ok(true) => {
                assert_eq!(
                    store
                        .agent_run_transfer_for_task(&child.task_id)
                        .unwrap()
                        .unwrap()
                        .agent_run_transfer(),
                    Some(&transfer)
                );
                // 已提交操作在父任务终态后仍能幂等核对，不能创造第二个拥有方。
                assert!(
                    !store
                        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
                        .unwrap()
                );
            }
            Err(_) => assert!(
                store
                    .agent_run_transfer_for_task(&child.task_id)
                    .unwrap()
                    .is_none()
            ),
            Ok(false) => panic!("首次转交不能为重放"),
        }
    }
}

#[test]
fn agent_subtree_transfer_preserves_lineage_and_validates_later_children() {
    let (store, root, child, _, transfer) = transfer_fixture();
    store
        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone())
        .unwrap();
    let before = store.snapshot();
    assert!(store.remove_task(&root.task_id).is_err());
    assert!(store.remove_task(&child.task_id).is_err());
    assert_eq!(
        serde_json::to_value(store.snapshot().tasks).unwrap(),
        serde_json::to_value(before.tasks).unwrap()
    );
    for mutation in 0..4 {
        let mut leaf = activation_task(
            root.task_id.as_str(),
            &format!("later-leaf-{mutation}"),
            None,
        );
        leaf.parent_task_id = Some(child.task_id.clone());
        match mutation {
            0 => {
                leaf.executor_binding.as_mut().unwrap().plan_item_id =
                    Some(magi_core::PlanItemId::new("mainline-plan"))
            }
            1 => leaf.dependency_ids.push(root.task_id.clone()),
            2 => {
                leaf.executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_activation
                    .as_mut()
                    .unwrap()
                    .session_id = magi_core::SessionId::new("different-session")
            }
            _ => {
                leaf.parent_task_id = Some(root.task_id.clone());
                leaf.required_children.push(child.task_id.clone());
            }
        }
        assert!(
            store.insert_task(leaf).is_err(),
            "insert mutation {mutation}"
        );
    }
    let mut leaf = activation_task(root.task_id.as_str(), "later-leaf", None);
    leaf.parent_task_id = Some(child.task_id.clone());
    store.insert_task(leaf.clone()).unwrap();
    assert_eq!(
        store
            .agent_run_transfer_for_task(&leaf.task_id)
            .unwrap()
            .unwrap()
            .agent_run_transfer(),
        Some(&transfer)
    );
    let snapshot = store.snapshot();
    TaskStore::restore_checked(TaskStoreCheckpoint {
        tasks: snapshot.tasks,
        leases: snapshot.leases,
    })
    .unwrap();
    // 会话/mission 删除仍是整域结算；不能把来源保护变成无法删除的历史。
    assert_eq!(
        store
            .remove_tasks_by_mission(&root.mission_id)
            .unwrap()
            .len(),
        4
    );
    assert!(store.agent_run_transfer_for_task(&leaf.task_id).is_err());
}

#[test]
fn agent_subtree_transfer_rejects_invalid_owner_plan_terminal_and_forged_restore() {
    for mutation in 0..8 {
        let (store, root, child, leaf, mut transfer) = transfer_fixture();
        let target = if mutation == 0 {
            leaf.task_id.clone()
        } else {
            child.task_id.clone()
        };
        match mutation {
            1 => transfer.session_id = magi_core::SessionId::new("foreign-session"),
            2 => transfer.activation_generation += 1,
            3 => {
                transfer.owner = magi_core::AgentRunOwner::Goal {
                    goal_id: magi_core::GoalId::new("goal"),
                    control_revision: 1,
                }
            }
            4 => transfer.transferred_by = TaskId::new("someone-else"),
            5 => {
                store
                    .revoke_active_lease_and_set_task_terminal(
                        &root.task_id,
                        &root.task_id,
                        TaskStatus::Killed,
                        Vec::new(),
                    )
                    .unwrap();
            }
            6 => {
                store
                    .revoke_active_lease_and_set_task_terminal(
                        &child.task_id,
                        &root.task_id,
                        TaskStatus::Killed,
                        Vec::new(),
                    )
                    .unwrap();
            }
            7 => transfer.source_turn_id.clear(),
            _ => {}
        }
        assert!(
            store
                .transfer_agent_subtree(&root.task_id, &target, transfer)
                .is_err(),
            "mutation {mutation}"
        );
        assert!(
            store
                .agent_run_transfer_for_task(&child.task_id)
                .unwrap()
                .is_none()
        );
    }
    let (store, root, child, _, transfer) = transfer_fixture();
    let mut snapshot = store.snapshot();
    let index = snapshot
        .tasks
        .iter()
        .position(|task| task.task_id == child.task_id)
        .unwrap();
    snapshot.tasks[index]
        .executor_binding
        .as_mut()
        .unwrap()
        .plan_item_id = Some(magi_core::PlanItemId::new("shared"));
    let store = TaskStore::restore_checked(TaskStoreCheckpoint {
        tasks: snapshot.tasks,
        leases: snapshot.leases,
    })
    .unwrap();
    assert!(
        store
            .transfer_agent_subtree(&root.task_id, &child.task_id, transfer.clone())
            .is_err()
    );
    // 恢复不接受绕过转交事务、与原父任务或生命周期不匹配的记录。
    let (store, root, child, _, transfer) = transfer_fixture();
    store
        .transfer_agent_subtree(&root.task_id, &child.task_id, transfer)
        .unwrap();
    for mutation in 0..4 {
        let mut snapshot = store.snapshot();
        let binding = snapshot
            .tasks
            .iter_mut()
            .find(|task| task.task_id == child.task_id)
            .unwrap()
            .executor_binding
            .as_mut()
            .unwrap();
        match mutation {
            0 => {
                binding
                    .agent_run_transfer
                    .as_mut()
                    .unwrap()
                    .activation_generation += 1
            }
            1 => {
                binding.agent_run_transfer.as_mut().unwrap().transferred_by = child.task_id.clone()
            }
            2 => binding.agent_activation = None,
            _ => binding.plan_item_id = Some(magi_core::PlanItemId::new("shared")),
        }
        assert!(
            TaskStore::restore_checked(TaskStoreCheckpoint {
                tasks: snapshot.tasks,
                leases: snapshot.leases
            })
            .is_err(),
            "restore mutation {mutation}"
        );
    }
}

#[test]
fn nested_agent_delegation_preserves_policy_depth_and_parent_lineage() {
    let store = TaskStore::new();
    let mut root = rooted_task("nested-root", "nested-root");
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    store.insert_task(root.clone()).unwrap();
    let mut parent = activation_task("nested-root", "parent-agent", None);
    parent.executor_binding.as_mut().unwrap().delegation_allowed = true;
    store.insert_task(parent.clone()).unwrap();
    let mut leaf = activation_task("nested-root", "leaf-agent", None);
    leaf.parent_task_id = Some(parent.task_id.clone());
    assert!(store.insert_task(leaf.clone()).is_ok());
    let snapshot = store.snapshot();
    let checkpoint = TaskStoreCheckpoint {
        tasks: snapshot.tasks,
        leases: snapshot.leases,
    };
    TaskStore::restore_checked(TaskStoreCheckpoint {
        tasks: checkpoint.tasks.clone(),
        leases: checkpoint.leases.clone(),
    })
    .unwrap();
    assert!(store.validate_agent_delegation(&leaf).is_err());
    let mut forged = leaf.clone();
    forged.task_id = TaskId::new("forged-leaf");
    forged
        .executor_binding
        .as_mut()
        .unwrap()
        .agent_activation
        .as_mut()
        .unwrap()
        .agent_id = forged.task_id.clone();
    forged.executor_binding.as_mut().unwrap().delegation_allowed = true;
    assert!(store.insert_task(forged).is_err());
    for mutation in 0..4 {
        let mut copy = TaskStoreCheckpoint {
            tasks: checkpoint.tasks.clone(),
            leases: checkpoint.leases.clone(),
        };
        let index = copy
            .tasks
            .iter()
            .position(|task| task.task_id == leaf.task_id)
            .unwrap();
        let parent_index = copy
            .tasks
            .iter()
            .position(|task| task.task_id == parent.task_id)
            .unwrap();
        match mutation {
            0 => {
                copy.tasks[index]
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .delegation_allowed = true
            }
            1 => copy.tasks[index].workspace_scope = Some("foreign".into()),
            2 => {
                copy.tasks[parent_index]
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .delegation_allowed = false
            }
            _ => {
                copy.tasks[parent_index]
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_activation = None
            }
        }
        assert!(
            TaskStore::restore_checked(copy).is_err(),
            "mutation {mutation}"
        );
    }
    let sibling = activation_task("nested-root", "sibling-agent", None);
    store.insert_task(sibling.clone()).unwrap();
    assert!(store.agent_resume_parent_matches(&leaf, &parent));
    assert!(!store.agent_resume_parent_matches(&leaf, &sibling));
    assert!(!store.agent_resume_parent_matches(&leaf, &root));
    // 数据来自持久事实，checkpoint 失败不能偷偷放入已派发代理。
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::InvalidState {
            message: "disk unavailable".into(),
        })
    }));
    let mut next = leaf.clone();
    next.task_id = TaskId::new("uncommitted-leaf");
    next.executor_binding
        .as_mut()
        .unwrap()
        .agent_activation
        .as_mut()
        .unwrap()
        .agent_id = next.task_id.clone();
    assert!(store.insert_task(next.clone()).is_err());
    assert!(store.get_task(&next.task_id).is_none());
}

#[test]
fn agent_activation_concurrent_resume_accepts_only_one_successor() {
    let store = Arc::new(TaskStore::new());
    store
        .insert_task(rooted_task("old-root", "old-root"))
        .unwrap();
    store
        .insert_task(rooted_task("new-root", "new-root"))
        .unwrap();
    let mut source = activation_task("old-root", "agent-first", None);
    source.status = TaskStatus::Completed;
    store.insert_task(source.clone()).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            let child = activation_task("new-root", &format!("next-{index}"), Some(&source));
            std::thread::spawn(move || {
                barrier.wait();
                store.insert_task(child)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .filter(Result::is_ok)
            .count(),
        1
    );
    let heads = store.agent_heads_for_session(&magi_core::SessionId::new("activation-session"));
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].agent_activation().unwrap().generation, 2);
    assert!(store.remove_task(&source.task_id).is_err());
}

#[test]
fn agent_activation_failed_checkpoint_does_not_advance_identity() {
    let store = TaskStore::new();
    store.insert_task(rooted_task("root", "root")).unwrap();
    let mut source = activation_task("root", "first", None);
    source.status = TaskStatus::Completed;
    store.insert_task(source.clone()).unwrap();
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::InvalidState {
            message: "disk unavailable".into(),
        })
    }));
    let next = activation_task("root", "second", Some(&source));
    assert!(store.insert_task(next.clone()).is_err());
    assert!(store.get_task(&next.task_id).is_none());
    assert_eq!(
        store.agent_heads_for_session(&magi_core::SessionId::new("activation-session"))[0].task_id,
        source.task_id
    );
    store.set_checkpoint_callback(Box::new(|_| Ok(())));
    store.insert_task(next).unwrap();
}

#[test]
fn agent_activation_restore_rejects_cross_scope_and_duplicate_lineage() {
    let root1 = rooted_task("root1", "root1");
    let root2 = rooted_task("root2", "root2");
    let mut first = activation_task("root1", "first", None);
    first.status = TaskStatus::Completed;
    let next = activation_task("root2", "second", Some(&first));
    let valid = TaskStoreCheckpoint {
        tasks: vec![root1, root2, first, next],
        leases: Vec::new(),
    };
    let copy = || TaskStoreCheckpoint {
        tasks: valid.tasks.clone(),
        leases: valid.leases.clone(),
    };
    TaskStore::restore_checked(copy()).unwrap();
    for mutation in 0..7 {
        let mut checkpoint = copy();
        match mutation {
            0 => {
                checkpoint.tasks.remove(2);
            }
            1 => {
                checkpoint.tasks[3].workspace_scope = Some("other workspace".into());
            }
            2 => {
                checkpoint.tasks[3].write_scope = Some("other scope".into());
            }
            3 => {
                checkpoint.tasks[2].status = TaskStatus::Running;
            }
            4 => {
                checkpoint.tasks[3]
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .agent_activation
                    .as_mut()
                    .unwrap()
                    .session_id = magi_core::SessionId::new("other session");
            }
            5 => {
                checkpoint.tasks[3]
                    .executor_binding
                    .as_mut()
                    .unwrap()
                    .capability_ids
                    .push("extra".into());
            }
            _ => {
                let mut duplicate = checkpoint.tasks[3].clone();
                duplicate.task_id = TaskId::new("duplicate");
                checkpoint.tasks.push(duplicate);
            }
        }
        assert!(
            TaskStore::restore_checked(checkpoint).is_err(),
            "mutation {mutation}"
        );
    }
}

fn projection_fixture(dir: &Path, generation: u64, projections: &[TaskRootProjection]) {
    let generation_dir = dir
        .join(TASK_PROJECTION_GENERATIONS_DIR)
        .join(TaskStore::generation_directory_name(generation));
    fs::create_dir_all(&generation_dir).expect("fixture generation should create");
    let roots = projections
        .iter()
        .map(|projection| {
            let file_name = TaskStore::projection_file_name(projection.root_task_id.as_str());
            fs::write(
                generation_dir.join(&file_name),
                serde_json::to_vec_pretty(projection).expect("fixture projection should encode"),
            )
            .expect("fixture projection should write");
            TaskProjectionManifestRoot {
                root_task_id: projection.root_task_id.clone(),
                file_name,
                generation,
                content_hash: TaskStore::projection_content_hash(projection)
                    .expect("fixture projection hash should compute"),
            }
        })
        .collect();
    fs::create_dir_all(dir).expect("fixture root should create");
    fs::write(
        dir.join(TASK_PROJECTION_MANIFEST_FILE),
        serde_json::to_vec_pretty(&TaskProjectionManifest {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation,
            roots,
        })
        .expect("fixture manifest should encode"),
    )
    .expect("fixture manifest should write");
}

#[test]
fn projection_restore_ignores_uncommitted_multi_root_generation() {
    let dir = tempfile::tempdir().expect("projection root");
    let projection_dir = dir.path().join("tasks");
    let store = TaskStore::new();
    store
        .insert_task(rooted_task("root-a", "root-a"))
        .expect("root a should insert");
    store
        .insert_task(rooted_task("root-b", "root-b"))
        .expect("root b should insert");
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("first generation should commit");

    let uncommitted = projection_dir
        .join(TASK_PROJECTION_GENERATIONS_DIR)
        .join(TaskStore::generation_directory_name(2));
    fs::create_dir_all(&uncommitted).expect("partial generation should create");
    fs::write(uncommitted.join("root-a.json"), b"{\"partial\":true}")
        .expect("partial root should write");

    let restored = TaskStore::restore_from_projection_directory(&projection_dir)
        .expect("committed generation should restore")
        .expect("manifest represents an initialized store");
    assert!(restored.get_task(&TaskId::new("root-a")).is_some());
    assert!(restored.get_task(&TaskId::new("root-b")).is_some());
}

#[test]
fn projection_restore_uses_manifest_even_when_stale_generation_remains() {
    let dir = tempfile::tempdir().expect("projection root");
    let projection_dir = dir.path().join("tasks");
    let store = TaskStore::new();
    store
        .insert_task(rooted_task("root-current", "root-current"))
        .expect("root should insert");
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("generation should commit");

    let stale = projection_dir
        .join(TASK_PROJECTION_GENERATIONS_DIR)
        .join(TaskStore::generation_directory_name(99));
    fs::create_dir_all(&stale).expect("stale generation should create");
    fs::write(stale.join("invalid.json"), b"not-json").expect("stale projection should write");

    let restored = TaskStore::restore_from_projection_directory(&projection_dir)
        .expect("manifest generation should restore")
        .expect("store should exist");
    assert!(restored.get_task(&TaskId::new("root-current")).is_some());
}

#[test]
fn projection_restore_preserves_canonical_checkpoint_order() {
    let dir = tempfile::tempdir().expect("projection root");
    let projection_dir = dir.path().join("tasks");
    let store = TaskStore::new();

    // The child IDs intentionally sort across root boundaries. A projection
    // restore must produce the same canonical order as a full checkpoint,
    // regardless of root grouping in the manifest.
    store
        .insert_task(rooted_task("root-z", "root-z"))
        .expect("root z should insert");
    store
        .insert_task(rooted_task("root-z", "a-child"))
        .expect("child a should insert");
    store
        .insert_task(rooted_task("root-a", "root-a"))
        .expect("root a should insert");
    store
        .insert_task(rooted_task("root-a", "z-child"))
        .expect("child z should insert");

    let canonical = |store: &TaskStore| {
        let snapshot = store.snapshot();
        json!({ "tasks": snapshot.tasks, "leases": snapshot.leases })
    };
    let expected = canonical(&store);
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("projection checkpoint should commit");

    let restored = TaskStore::restore_from_projection_directory(&projection_dir)
        .expect("projection should restore")
        .expect("projection should contain a store");
    assert_eq!(canonical(&restored), expected);
}

#[test]
fn concurrent_checkpoints_cannot_commit_an_older_snapshot_last() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().expect("projection root");
    let projection_dir = dir.path().join("tasks");
    let store = Arc::new(TaskStore::new());
    store
        .insert_task(rooted_task("root-race", "root-race"))
        .expect("root should insert");
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("initial generation should commit");

    let barrier = Arc::new(Barrier::new(2));
    let first_hook_call = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let hooked_projection_dir = projection_dir.clone();
    *TASK_PROJECTION_AFTER_SNAPSHOT_HOOK
        .lock()
        .expect("test hook should lock") = Some(Arc::new({
        let barrier = barrier.clone();
        let first_hook_call = first_hook_call.clone();
        move |path| {
            if path != hooked_projection_dir {
                return;
            }
            if first_hook_call.swap(false, Ordering::SeqCst) {
                barrier.wait();
                barrier.wait();
            }
        }
    }));

    let old_store = store.clone();
    let old_path = projection_dir.clone();
    let old_writer =
        std::thread::spawn(move || old_store.checkpoint_to_projection_directory(&old_path));
    barrier.wait();
    store
        .update_task_goal(&TaskId::new("root-race"), "new snapshot".to_string())
        .expect("goal should update");
    let new_store = store.clone();
    let new_path = projection_dir.clone();
    let new_writer =
        std::thread::spawn(move || new_store.checkpoint_to_projection_directory(&new_path));
    barrier.wait();
    old_writer
        .join()
        .expect("old writer should join")
        .expect("old generation should commit first");
    new_writer
        .join()
        .expect("new writer should join")
        .expect("new generation should commit last");
    *TASK_PROJECTION_AFTER_SNAPSHOT_HOOK
        .lock()
        .expect("test hook should lock") = None;

    let restored = TaskStore::restore_from_projection_directory(&projection_dir)
        .expect("latest generation should restore")
        .expect("store should exist");
    assert_eq!(
        restored
            .get_task(&TaskId::new("root-race"))
            .expect("root should restore")
            .goal,
        "new snapshot"
    );
}

#[test]
fn checkpoint_removes_every_stale_generation_after_manifest_commit() {
    let dir = tempfile::tempdir().expect("projection root");
    let projection_dir = dir.path().join("tasks");
    let store = TaskStore::new();
    store
        .insert_task(rooted_task("root-cleanup", "root-cleanup"))
        .expect("root should insert");
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("first generation should commit");
    let stale = projection_dir
        .join(TASK_PROJECTION_GENERATIONS_DIR)
        .join(TaskStore::generation_directory_name(77));
    fs::create_dir_all(&stale).expect("stale generation should create");
    store
        .update_task_goal(&TaskId::new("root-cleanup"), "updated".to_string())
        .expect("goal should update");
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("second generation should commit and clean stale directories");

    let generations = fs::read_dir(projection_dir.join(TASK_PROJECTION_GENERATIONS_DIR))
        .expect("generations should read")
        .collect::<Result<Vec<_>, _>>()
        .expect("generation entries should read");
    assert_eq!(generations.len(), 1);
}

#[cfg(unix)]
#[test]
fn checkpoint_reuses_unchanged_root_projection_files() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().expect("projection root");
    let projection_dir = dir.path().join("tasks");
    let store = TaskStore::new();
    store
        .insert_task(rooted_task("root-a", "root-a"))
        .expect("root a should insert");
    store
        .insert_task(rooted_task("root-b", "root-b"))
        .expect("root b should insert");
    store
        .checkpoint_to_projection_directory(&projection_dir)
        .expect("first generation should commit");

    let first_manifest = TaskStore::read_projection_manifest_if_present(&projection_dir)
        .expect("manifest should read")
        .expect("first manifest should exist");
    let first_generation_dir = projection_dir.join(TASK_PROJECTION_GENERATIONS_DIR).join(
        TaskStore::generation_directory_name(first_manifest.generation),
    );
    let root_a_file = first_generation_dir.join(TaskStore::projection_file_name("root-a"));
    let root_b_file = first_generation_dir.join(TaskStore::projection_file_name("root-b"));
    let root_a_inode = fs::metadata(&root_a_file)
        .expect("root a projection should exist")
        .ino();
    let root_b_inode = fs::metadata(&root_b_file)
        .expect("root b projection should exist")
        .ino();

    store
        .update_task_goal(&TaskId::new("root-a"), "updated root a".to_string())
        .expect("root a goal should update");
    let mut snapshot = store.snapshot();
    snapshot.changed_root_ids = vec![TaskId::new("root-a")];
    TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection_dir)
        .expect("second generation should commit");

    let second_manifest = TaskStore::read_projection_manifest_if_present(&projection_dir)
        .expect("manifest should read")
        .expect("second manifest should exist");
    let second_generation_dir = projection_dir.join(TASK_PROJECTION_GENERATIONS_DIR).join(
        TaskStore::generation_directory_name(second_manifest.generation),
    );
    let second_root_a_inode =
        fs::metadata(second_generation_dir.join(TaskStore::projection_file_name("root-a")))
            .expect("updated root a projection should exist")
            .ino();
    let second_root_b_inode =
        fs::metadata(second_generation_dir.join(TaskStore::projection_file_name("root-b")))
            .expect("unchanged root b projection should exist")
            .ino();

    assert_ne!(second_root_a_inode, root_a_inode);
    assert_eq!(second_root_b_inode, root_b_inode);
}

#[test]
fn projection_restore_rejects_duplicate_tasks_and_invalid_leases() {
    let duplicate_dir = tempfile::tempdir().expect("duplicate fixture");
    let shared_a = rooted_task("root-a", "shared-task");
    let shared_b = rooted_task("root-b", "shared-task");
    projection_fixture(
        duplicate_dir.path(),
        1,
        &[
            TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-a"),
                tasks: vec![rooted_task("root-a", "root-a"), shared_a],
                leases: Vec::new(),
            },
            TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-b"),
                tasks: vec![rooted_task("root-b", "root-b"), shared_b],
                leases: Vec::new(),
            },
        ],
    );
    assert_eq!(
        TaskStore::restore_from_projection_directory(duplicate_dir.path())
            .err()
            .expect("duplicate task IDs must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let lease_dir = tempfile::tempdir().expect("lease fixture");
    let now = UtcMillis::now();
    projection_fixture(
        lease_dir.path(),
        1,
        &[TaskRootProjection {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation: 1,
            root_task_id: TaskId::new("root-lease"),
            tasks: vec![rooted_task("root-lease", "root-lease")],
            leases: vec![TaskLease {
                lease_id: LeaseId::new("lease-orphan"),
                task_id: TaskId::new("missing-task"),
                root_task_id: TaskId::new("root-lease"),
                worker_id: WorkerId::new("worker-lease"),
                role: "executor".to_string(),
                granted_at: now,
                expires_at: now,
                heartbeat_at: now,
                lease_status: TaskLeaseState::Active,
            }],
        }],
    );
    assert_eq!(
        TaskStore::restore_from_projection_directory(lease_dir.path())
            .err()
            .expect("orphan lease must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let duplicate_lease_dir = tempfile::tempdir().expect("duplicate lease fixture");
    let duplicate_lease_id = LeaseId::new("lease-duplicate");
    projection_fixture(
        duplicate_lease_dir.path(),
        1,
        &[
            TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-lease-a"),
                tasks: vec![rooted_task("root-lease-a", "root-lease-a")],
                leases: vec![TaskLease {
                    lease_id: duplicate_lease_id.clone(),
                    task_id: TaskId::new("root-lease-a"),
                    root_task_id: TaskId::new("root-lease-a"),
                    worker_id: WorkerId::new("worker-a"),
                    role: "executor".to_string(),
                    granted_at: now,
                    expires_at: now,
                    heartbeat_at: now,
                    lease_status: TaskLeaseState::Active,
                }],
            },
            TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-lease-b"),
                tasks: vec![rooted_task("root-lease-b", "root-lease-b")],
                leases: vec![TaskLease {
                    lease_id: duplicate_lease_id,
                    task_id: TaskId::new("root-lease-b"),
                    root_task_id: TaskId::new("root-lease-b"),
                    worker_id: WorkerId::new("worker-b"),
                    role: "executor".to_string(),
                    granted_at: now,
                    expires_at: now,
                    heartbeat_at: now,
                    lease_status: TaskLeaseState::Active,
                }],
            },
        ],
    );
    assert_eq!(
        TaskStore::restore_from_projection_directory(duplicate_lease_dir.path())
            .err()
            .expect("duplicate lease IDs must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let wrong_lease_owner_dir = tempfile::tempdir().expect("wrong lease owner fixture");
    projection_fixture(
        wrong_lease_owner_dir.path(),
        1,
        &[
            TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-owner-a"),
                tasks: vec![rooted_task("root-owner-a", "root-owner-a")],
                leases: vec![TaskLease {
                    lease_id: LeaseId::new("lease-wrong-owner"),
                    task_id: TaskId::new("root-owner-b"),
                    root_task_id: TaskId::new("root-owner-a"),
                    worker_id: WorkerId::new("worker-owner"),
                    role: "executor".to_string(),
                    granted_at: now,
                    expires_at: now,
                    heartbeat_at: now,
                    lease_status: TaskLeaseState::Active,
                }],
            },
            TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: TaskId::new("root-owner-b"),
                tasks: vec![rooted_task("root-owner-b", "root-owner-b")],
                leases: Vec::new(),
            },
        ],
    );
    assert_eq!(
        TaskStore::restore_from_projection_directory(wrong_lease_owner_dir.path())
            .err()
            .expect("lease assigned to another root must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn checkpoint_and_restore_reject_terminal_task_with_active_lease() {
    for (label, status) in [
        ("completed", TaskStatus::Completed),
        ("failed", TaskStatus::Failed),
        ("killed", TaskStatus::Killed),
    ] {
        let root_task_id = TaskId::new(format!("root-invalid-active-{label}"));
        let mut task = rooted_task(root_task_id.as_str(), root_task_id.as_str());
        task.status = status;
        let now = UtcMillis::now();
        let snapshot = TaskStoreSnapshot {
            tasks: vec![task],
            leases: vec![TaskLease {
                lease_id: LeaseId::new(format!("lease-invalid-active-{label}")),
                task_id: root_task_id.clone(),
                root_task_id: root_task_id.clone(),
                worker_id: WorkerId::new(format!("worker-invalid-active-{label}")),
                role: "executor".to_string(),
                granted_at: now,
                expires_at: UtcMillis(now.0.saturating_add(60_000)),
                heartbeat_at: now,
                lease_status: TaskLeaseState::Active,
            }],
            changed_root_ids: Vec::new(),
        };
        let checkpoint_dir = tempfile::tempdir().expect("checkpoint directory");
        let projection_dir = checkpoint_dir.path().join("projection");
        assert_eq!(
            TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection_dir)
                .expect_err("invalid active lease must reject checkpoint")
                .kind(),
            io::ErrorKind::InvalidData
        );

        let restore_dir = tempfile::tempdir().expect("restore directory");
        projection_fixture(
            restore_dir.path(),
            1,
            &[TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation: 1,
                root_task_id: root_task_id.clone(),
                tasks: snapshot.tasks.clone(),
                leases: snapshot.leases.clone(),
            }],
        );
        assert_eq!(
            TaskStore::restore_from_projection_directory(restore_dir.path())
                .err()
                .expect("invalid active lease must reject restore")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn checkpoint_and_restore_reject_duplicate_active_leases_for_one_task() {
    let root_task_id = TaskId::new("root-duplicate-active-leases");
    let mut task = rooted_task(root_task_id.as_str(), root_task_id.as_str());
    task.status = TaskStatus::Running;
    let now = UtcMillis::now();
    let leases = vec![
        TaskLease {
            lease_id: LeaseId::new("lease-duplicate-active-a"),
            task_id: root_task_id.clone(),
            root_task_id: root_task_id.clone(),
            worker_id: WorkerId::new("worker-duplicate-active-a"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0.saturating_add(60_000)),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        },
        TaskLease {
            lease_id: LeaseId::new("lease-duplicate-active-b"),
            task_id: root_task_id.clone(),
            root_task_id: root_task_id.clone(),
            worker_id: WorkerId::new("worker-duplicate-active-b"),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0.saturating_add(60_000)),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        },
    ];
    let snapshot = TaskStoreSnapshot {
        tasks: vec![task],
        leases,
        changed_root_ids: Vec::new(),
    };
    let checkpoint_dir = tempfile::tempdir().expect("checkpoint directory");
    let projection_dir = checkpoint_dir.path().join("projection");
    assert_eq!(
        TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection_dir)
            .expect_err("duplicate active leases must reject checkpoint")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let restore_dir = tempfile::tempdir().expect("restore directory");
    projection_fixture(
        restore_dir.path(),
        1,
        &[TaskRootProjection {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation: 1,
            root_task_id,
            tasks: snapshot.tasks,
            leases: snapshot.leases,
        }],
    );
    assert_eq!(
        TaskStore::restore_from_projection_directory(restore_dir.path())
            .err()
            .expect("duplicate active leases must reject restore")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn lease_creation_rejects_non_running_active_lease_contract() {
    for (label, status) in [
        ("completed", TaskStatus::Completed),
        ("failed", TaskStatus::Failed),
        ("killed", TaskStatus::Killed),
    ] {
        let root_task_id = TaskId::new(format!("root-lease-contract-{label}"));
        let mut task = rooted_task(root_task_id.as_str(), root_task_id.as_str());
        task.status = status;
        let store = TaskStore::new();
        store.insert_task(task.clone()).expect("task should insert");
        let now = UtcMillis::now();
        let lease = TaskLease {
            lease_id: LeaseId::new(format!("lease-contract-{label}")),
            task_id: task.task_id.clone(),
            root_task_id: task.root_task_id.clone(),
            worker_id: WorkerId::new(format!("worker-contract-{label}")),
            role: "executor".to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0.saturating_add(60_000)),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        };
        assert!(matches!(
            store.insert_lease(lease),
            Err(DomainError::InvalidState { .. })
        ));
        assert!(
            store
                .grant_lease(
                    &task.task_id,
                    &task.root_task_id,
                    &WorkerId::new(format!("worker-grant-contract-{label}")),
                    "executor",
                    60_000,
                )
                .is_none()
        );
        assert!(store.get_active_lease(&task.task_id).is_none());
    }
}

#[test]
fn projection_restore_rejects_wrong_filename_empty_and_missing_schema() {
    let wrong_name_dir = tempfile::tempdir().expect("wrong name fixture");
    projection_fixture(
        wrong_name_dir.path(),
        1,
        &[TaskRootProjection {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation: 1,
            root_task_id: TaskId::new("root-name"),
            tasks: vec![rooted_task("root-name", "root-name")],
            leases: Vec::new(),
        }],
    );
    let mut manifest = TaskStore::read_projection_manifest(wrong_name_dir.path())
        .expect("fixture manifest should read");
    manifest.roots[0].file_name = "another-root.json".to_string();
    fs::write(
        wrong_name_dir.path().join(TASK_PROJECTION_MANIFEST_FILE),
        serde_json::to_vec(&manifest).expect("manifest should encode"),
    )
    .expect("manifest should rewrite");
    assert!(TaskStore::restore_from_projection_directory(wrong_name_dir.path()).is_err());

    let empty_dir = tempfile::tempdir().expect("empty fixture");
    projection_fixture(
        empty_dir.path(),
        1,
        &[TaskRootProjection {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation: 1,
            root_task_id: TaskId::new("root-empty"),
            tasks: Vec::new(),
            leases: Vec::new(),
        }],
    );
    assert!(TaskStore::restore_from_projection_directory(empty_dir.path()).is_err());

    let schema_dir = tempfile::tempdir().expect("schema fixture");
    let generation_dir = schema_dir
        .path()
        .join(TASK_PROJECTION_GENERATIONS_DIR)
        .join(TaskStore::generation_directory_name(1));
    fs::create_dir_all(&generation_dir).expect("generation should create");
    fs::write(generation_dir.join("root-schema.json"), b"{}")
        .expect("invalid projection should write");
    fs::write(
        schema_dir.path().join(TASK_PROJECTION_MANIFEST_FILE),
        serde_json::to_vec(&TaskProjectionManifest {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation: 1,
            roots: vec![TaskProjectionManifestRoot {
                root_task_id: TaskId::new("root-schema"),
                file_name: "root-schema.json".to_string(),
                generation: 1,
                content_hash: String::new(),
            }],
        })
        .expect("manifest should encode"),
    )
    .expect("manifest should write");
    assert!(TaskStore::restore_from_projection_directory(schema_dir.path()).is_err());

    let missing_task_field_dir = tempfile::tempdir().expect("missing task field fixture");
    projection_fixture(
        missing_task_field_dir.path(),
        1,
        &[TaskRootProjection {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation: 1,
            root_task_id: TaskId::new("root-required-field"),
            tasks: vec![rooted_task("root-required-field", "root-required-field")],
            leases: Vec::new(),
        }],
    );
    let projection_path = missing_task_field_dir
        .path()
        .join(TASK_PROJECTION_GENERATIONS_DIR)
        .join(TaskStore::generation_directory_name(1))
        .join("root-required-field.json");
    let mut projection: serde_json::Value = serde_json::from_slice(
        &fs::read(&projection_path).expect("projection fixture should read"),
    )
    .expect("projection fixture should parse");
    projection["tasks"][0]
        .as_object_mut()
        .expect("task fixture should be an object")
        .remove("runtime_payload");
    fs::write(
        projection_path,
        serde_json::to_vec_pretty(&projection).expect("projection fixture should encode"),
    )
    .expect("projection fixture should rewrite");
    assert_eq!(
        TaskStore::restore_from_projection_directory(missing_task_field_dir.path())
            .err()
            .expect("missing required task fields must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn checkpoint_callback_failure_is_returned_for_terminal_and_delete_mutations() {
    fn failing_callback(_: &TaskStoreSnapshot) -> DomainResult<()> {
        Err(DomainError::Persistence {
            message: "checkpoint unavailable".to_string(),
        })
    }

    let failed_store = TaskStore::new();
    failed_store
        .insert_task(rooted_task("root-failed", "root-failed"))
        .expect("root should insert before callback");
    failed_store.set_checkpoint_callback(Box::new(failing_callback));
    assert!(
        failed_store
            .revoke_lease_and_set_task_terminal(
                &TaskId::new("root-failed"),
                &TaskId::new("root-failed"),
                None,
                TaskStatus::Failed,
                Vec::new(),
            )
            .is_err()
    );

    let completed_store = TaskStore::new();
    completed_store
        .insert_task(rooted_task("root-completed", "root-completed"))
        .expect("root should insert before callback");
    completed_store.set_checkpoint_callback(Box::new(failing_callback));
    assert!(
        completed_store
            .complete_task(&TaskId::new("root-completed"), successful_attempt())
            .is_err()
    );
    assert_eq!(
        completed_store
            .get_task(&TaskId::new("root-completed"))
            .expect("failed completion must leave task in memory")
            .status,
        TaskStatus::Running
    );

    let delete_store = TaskStore::new();
    delete_store
        .insert_task(rooted_task("root-delete", "root-delete"))
        .expect("root should insert before callback");
    delete_store.set_checkpoint_callback(Box::new(failing_callback));
    assert!(
        delete_store
            .remove_task(&TaskId::new("root-delete"))
            .is_err()
    );
    assert!(delete_store.get_task(&TaskId::new("root-delete")).is_some());
    let retried_checkpoint_count = std::sync::Arc::new(AtomicU64::new(0));
    let observed_retry_count = retried_checkpoint_count.clone();
    delete_store.set_checkpoint_callback(Box::new(move |_| {
        observed_retry_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    assert!(
        delete_store
            .remove_task(&TaskId::new("root-delete"))
            .expect("retry should checkpoint")
            .is_some()
    );
    assert_eq!(retried_checkpoint_count.load(Ordering::SeqCst), 1);
}
