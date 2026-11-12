use super::*;
use crate::{
    execution_admission::{ExecutionAdmissionLimits, ExecutionAdmissionPermit},
    task_execution_registry::TaskExecutionRegistry,
    task_runner_bridge::TaskResult,
};
use magi_core::{
    DomainError, MissionId, TaskCompletionAttempt, TaskKind, TaskRuntimePayload, UtcMillis,
    WorkerId,
};
use std::sync::Mutex;

struct RejectingDispatcher;

impl TaskDispatcher for RejectingDispatcher {
    fn dispatch(
        &self,
        _task: &Task,
        _worker: &WorkerInfo,
        _lease: &magi_orchestrator::task_store::TaskLease,
        _admission_permit: crate::execution_admission::ExecutionAdmissionPermit,
    ) -> Result<(), String> {
        Err("test dispatcher should not run".to_string())
    }
}

struct HoldingDispatcher {
    permits: Mutex<Vec<ExecutionAdmissionPermit>>,
}

impl HoldingDispatcher {
    fn release_all(&self) {
        self.permits
            .lock()
            .expect("held permits lock should not poison")
            .clear();
    }
}

impl TaskDispatcher for HoldingDispatcher {
    fn dispatch(
        &self,
        _task: &Task,
        _worker: &WorkerInfo,
        _lease: &magi_orchestrator::task_store::TaskLease,
        admission_permit: ExecutionAdmissionPermit,
    ) -> Result<(), String> {
        self.permits
            .lock()
            .expect("held permits lock should not poison")
            .push(admission_permit);
        Ok(())
    }
}

fn test_task(task_id: &str, root_task_id: &str, parent: Option<TaskId>) -> Task {
    let now = UtcMillis(1_000);
    Task {
        task_id: TaskId::new(task_id),
        mission_id: MissionId::new("mission-child-result"),
        root_task_id: TaskId::new(root_task_id),
        parent_task_id: parent,
        kind: TaskKind::LocalAgent,
        title: task_id.to_string(),
        goal: format!("run {task_id}"),
        status: TaskStatus::Pending,
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
fn subtree_terminal_failure_keeps_entire_subtree_and_pending_admission() {
    for mode in ["kill", "execution_kill", "unexpected_failure", "unrunnable"] {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("task-root-kill", "task-root-kill", None);
        root.status = TaskStatus::Running;
        let child = test_task(
            "task-child-kill",
            "task-root-kill",
            Some(root.task_id.clone()),
        );
        let grandchild = test_task(
            "task-leaf-kill",
            "task-root-kill",
            Some(child.task_id.clone()),
        );
        for task in [root.clone(), child.clone(), grandchild.clone()] {
            store.insert_task(task).unwrap();
        }
        let admission = Arc::new(ExecutionAdmissionController::new(
            ExecutionAdmissionLimits {
                max_active_tasks: 1,
                max_active_tasks_per_session: 1,
                max_active_tasks_per_role: 1,
                min_available_memory_bytes: 0,
            },
        ));
        assert!(
            admission
                .reserve(root.task_id.clone(), None, "executor")
                .is_none()
        );
        assert!(
            admission
                .reserve(child.task_id.clone(), None, "executor")
                .is_some()
        );
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
        )
        .with_execution_admission(admission.clone(), None);
        let before = serde_json::to_value(store.snapshot().tasks).unwrap();
        store.set_checkpoint_callback(Box::new(|_| {
            Err(DomainError::InvalidState {
                message: "disk failed".into(),
            })
        }));
        let settle = || match mode {
            "kill" => runner.kill_tree(&root.task_id),
            "execution_kill" => runner.kill_execution_scope(&root.task_id),
            "unexpected_failure" => {
                runner.finalize_unexpected_failure(&root.task_id, "worker failed")
            }
            _ => runner
                .finalize_unrunnable_outcome(&root.task_id, std::slice::from_ref(&child.task_id))
                .map(|_| ()),
        };
        assert!(settle().is_err());
        assert_eq!(
            serde_json::to_value(store.snapshot().tasks).unwrap(),
            before
        );
        assert_eq!(admission.snapshot().active_task_count, 1);
        assert_eq!(admission.snapshot().queued_task_count, 1);
        store.set_checkpoint_callback(Box::new(|_| Ok(())));
        settle().unwrap();
        for task in [root, child, grandchild] {
            assert_eq!(
                store.get_task(&task.task_id).unwrap().status,
                if matches!(mode, "kill" | "execution_kill") {
                    TaskStatus::Killed
                } else {
                    TaskStatus::Failed
                }
            );
        }
        assert_eq!(admission.snapshot().active_task_count, 0);
        assert_eq!(admission.snapshot().queued_task_count, 0);
    }
}

#[test]
fn failed_root_kills_running_and_queued_children_instead_of_dispatching_them() {
    let store = Arc::new(TaskStore::new());
    let mut root = test_task("task-root-failed", "task-root-failed", None);
    root.status = TaskStatus::Failed;
    let mut running_child = test_task(
        "task-running-orphan",
        "task-root-failed",
        Some(root.task_id.clone()),
    );
    running_child.status = TaskStatus::Running;
    let queued_child = test_task(
        "task-queued-orphan",
        "task-root-failed",
        Some(root.task_id.clone()),
    );
    for task in [root.clone(), running_child.clone(), queued_child.clone()] {
        store.insert_task(task).expect("任务应插入");
    }
    // 父任务已结束时排队中的子代理不能再被派发；RejectingDispatcher 被调用即报错。
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        vec![WorkerInfo {
            worker_id: WorkerId::new("worker-orphan"),
            role: "executor".to_string(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: None,
            system_prompt_template: None,
        }],
        Arc::new(RejectingDispatcher),
    );

    let outcome = runner.run_cycle(&root.task_id);

    assert!(
        matches!(outcome, RunCycleOutcome::Error(ref error) if error.contains("任务执行失败")),
        "根任务失败后应直接收口：{outcome:?}"
    );
    for task_id in [&running_child.task_id, &queued_child.task_id] {
        assert_eq!(
            store.get_task(task_id).expect("任务应存在").status,
            TaskStatus::Killed,
            "{task_id} 必须随失败的根任务一起终止"
        );
    }
}

#[test]
fn transferred_subtree_has_its_own_runner_after_source_turn_task_finishes() {
    for terminal in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Killed,
    ] {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("scope-root", "scope-root", None);
        root.status = TaskStatus::Running;
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
        store.insert_task(root.clone()).unwrap();
        let mut child = test_task("scope-child", "scope-root", Some(root.task_id.clone()));
        let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
        binding.delegation_allowed = true;
        binding.agent_activation = Some(magi_core::AgentActivation {
            agent_id: child.task_id.clone(),
            session_id: magi_core::SessionId::new("scope-session"),
            lifetime: magi_core::AgentLifetime::Session,
            generation: 1,
            previous_task_id: None,
        });
        child.executor_binding = Some(binding.clone());
        let mut leaf = test_task("scope-leaf", "scope-root", Some(child.task_id.clone()));
        binding.delegation_allowed = false;
        binding.agent_activation.as_mut().unwrap().agent_id = leaf.task_id.clone();
        leaf.executor_binding = Some(binding);
        for task in [&child, &leaf] {
            store.insert_task(task.clone()).unwrap();
        }
        store
            .transfer_agent_subtree(
                &root.task_id,
                &child.task_id,
                magi_core::AgentRunTransfer {
                    operation_id: "runner-transfer".into(),
                    session_id: magi_core::SessionId::new("scope-session"),
                    source_turn_id: "source-turn".into(),
                    transferred_by: root.task_id.clone(),
                    activation_generation: 1,
                    owner: magi_core::AgentRunOwner::Session,
                    transferred_at: UtcMillis::now(),
                },
            )
            .unwrap();
        if terminal == TaskStatus::Completed {
            store
                .complete_task(
                    &root.task_id,
                    TaskCompletionAttempt {
                        output_refs: vec![],
                        final_response: Some("主线结束".into()),
                        evidence: vec![],
                    },
                )
                .unwrap();
        } else {
            store
                .revoke_active_lease_and_set_task_terminal(
                    &root.task_id,
                    &root.task_id,
                    terminal,
                    vec![],
                )
                .unwrap();
        }
        let dispatcher = Arc::new(HoldingDispatcher {
            permits: Mutex::new(vec![]),
        });
        let runner = TaskRunner::with_dispatcher(
            store.clone(),
            vec![WorkerInfo {
                worker_id: WorkerId::new("scope-worker"),
                role: "executor".into(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }],
            dispatcher.clone(),
        );
        // 原 Runner 可收口，不得级联杀死或等待已转交子树。
        let source = runner.run_cycle(&root.task_id);
        assert!(
            match terminal {
                TaskStatus::Completed => source == RunCycleOutcome::AllComplete,
                _ => matches!(source, RunCycleOutcome::Error(_)),
            },
            "{source:?}"
        );
        assert_eq!(
            store.get_task(&child.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(runner.run_cycle(&child.task_id), RunCycleOutcome::Continue);
        let child_lease = store.get_active_lease(&child.task_id).unwrap();
        assert_eq!(child_lease.root_task_id, root.task_id);
        assert!(
            !crate::task_execution_guard::TaskExecutionGuard::new(
                &store,
                &magi_session_store::SessionStore::new(),
                &TaskExecutionRegistry::default(),
                &magi_core::SessionId::new("scope-session"),
                &child.task_id,
                &child_lease.lease_id,
            )
            .is_active(),
            "没有 canonical 拥有方装配时，领域转交不能单独授予执行权"
        );
        assert_eq!(runner.run_cycle(&child.task_id), RunCycleOutcome::Continue);
        let leaf_lease = store.get_active_lease(&leaf.task_id).unwrap();
        for (task, lease) in [(&leaf, &leaf_lease), (&child, &child_lease)] {
            assert!(
                store
                    .complete_lease_and_task(
                        &task.task_id,
                        &root.task_id,
                        &lease.lease_id,
                        TaskCompletionAttempt {
                            output_refs: vec![],
                            final_response: Some("独立拥有方完成".into()),
                            evidence: vec![]
                        }
                    )
                    .unwrap()
            );
        }
        assert_eq!(
            runner.run_cycle(&child.task_id),
            RunCycleOutcome::AllComplete
        );
        assert_eq!(store.get_task(&root.task_id).unwrap().status, terminal);
        dispatcher.release_all();
    }
}

#[test]
fn old_runner_scan_cannot_claim_a_subtree_transferred_during_admission() {
    let store = Arc::new(TaskStore::new());
    let mut root = test_task("claim-root", "claim-root", None);
    root.status = TaskStatus::Running;
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    store.insert_task(root.clone()).unwrap();
    let mut child = test_task("claim-child", "claim-root", Some(root.task_id.clone()));
    let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
    binding.agent_activation = Some(magi_core::AgentActivation {
        agent_id: child.task_id.clone(),
        session_id: magi_core::SessionId::new("claim-session"),
        lifetime: magi_core::AgentLifetime::Session,
        generation: 1,
        previous_task_id: None,
    });
    child.executor_binding = Some(binding);
    store.insert_task(child.clone()).unwrap();
    let runtime = store.clone();
    let parent = root.clone();
    let runner = TaskRunner::with_dispatcher(
        store.clone(),
        vec![WorkerInfo {
            worker_id: WorkerId::new("claim-worker"),
            role: "executor".into(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: None,
            system_prompt_template: None,
        }],
        Arc::new(RejectingDispatcher),
    )
    .with_dispatch_gate(Arc::new(move |task| {
        runtime
            .transfer_agent_subtree(
                &parent.task_id,
                &task.task_id,
                magi_core::AgentRunTransfer {
                    operation_id: "during-admission".into(),
                    session_id: magi_core::SessionId::new("claim-session"),
                    source_turn_id: "source-turn".into(),
                    transferred_by: parent.task_id.clone(),
                    activation_generation: 1,
                    owner: magi_core::AgentRunOwner::Session,
                    transferred_at: UtcMillis::now(),
                },
            )
            .unwrap();
        Ok(TaskDispatchGateDecision::Allow)
    }));
    assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Waiting);
    assert_eq!(
        store.get_task(&child.task_id).unwrap().status,
        TaskStatus::Pending
    );
    assert!(store.get_active_lease(&child.task_id).is_none());
}

#[test]
fn stale_unrunnable_child_does_not_fail_mainline_or_release_transferred_admission() {
    for changed in ["transferred", "completed", "started", "active"] {
        let store = Arc::new(TaskStore::new());
        let mut root = test_task("unrunnable-root", "unrunnable-root", None);
        root.status = TaskStatus::Running;
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
        store.insert_task(root.clone()).unwrap();
        let mut child = test_task(
            "unrunnable-child",
            "unrunnable-root",
            Some(root.task_id.clone()),
        );
        let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
        binding.agent_activation = Some(magi_core::AgentActivation {
            agent_id: child.task_id.clone(),
            session_id: magi_core::SessionId::new("unrunnable-session"),
            lifetime: magi_core::AgentLifetime::Session,
            generation: 1,
            previous_task_id: None,
        });
        child.executor_binding = Some(binding);
        store.insert_task(child.clone()).unwrap();
        let admission = Arc::new(ExecutionAdmissionController::default());
        admission.reserve(child.task_id.clone(), None, "executor");
        let runner =
            TaskRunner::with_dispatcher(store.clone(), vec![], Arc::new(RejectingDispatcher))
                .with_execution_admission(admission.clone(), None);
        let RunCycleOutcome::Unrunnable(candidates) = runner.run_cycle(&root.task_id) else {
            panic!("missing worker must produce a candidate before the ownership change");
        };
        assert_eq!(candidates, [child.task_id.clone()]);
        match changed {
            "transferred" => {
                store
                    .transfer_agent_subtree(
                        &root.task_id,
                        &child.task_id,
                        magi_core::AgentRunTransfer {
                            operation_id: "unrunnable-transfer".into(),
                            session_id: magi_core::SessionId::new("unrunnable-session"),
                            source_turn_id: "source-turn".into(),
                            transferred_by: root.task_id.clone(),
                            activation_generation: 1,
                            owner: magi_core::AgentRunOwner::Session,
                            transferred_at: UtcMillis::now(),
                        },
                    )
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
                            final_response: Some("completed by another cycle".into()),
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
        assert_eq!(
            runner
                .finalize_unrunnable_outcome(&root.task_id, &candidates)
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
        if changed == "transferred" {
            assert_eq!(
                store.get_task(&child.task_id).unwrap().status,
                TaskStatus::Pending
            );
            assert_eq!(admission.snapshot().active_task_count, 1);
        } else if changed == "active" {
            assert_eq!(admission.snapshot().active_task_count, 0);
        } else if changed == "started" {
            assert_eq!(
                store.get_task(&child.task_id).unwrap().status,
                TaskStatus::Running
            );
            assert_eq!(admission.snapshot().active_task_count, 1);
        }
    }
}

#[test]
fn resumed_agent_parent_starts_before_its_pending_descendants() {
    let store = Arc::new(TaskStore::new());
    let root = test_task("resume-parent", "resume-parent", None);
    let child = test_task("resume-child", "resume-parent", Some(root.task_id.clone()));
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child.clone()).unwrap();
    let runnable = store.get_runnable_leaves(&root.task_id);
    assert_eq!(
        runnable
            .iter()
            .map(|task| &task.task_id)
            .collect::<Vec<_>>(),
        vec![&root.task_id]
    );
    store
        .grant_lease_and_start_task(
            &root.task_id,
            &root.task_id,
            &WorkerId::new("resume-worker"),
            "executor",
            60_000,
        )
        .unwrap()
        .unwrap();
    let runnable = store.get_runnable_leaves(&root.task_id);
    assert_eq!(
        runnable
            .iter()
            .map(|task| &task.task_id)
            .collect::<Vec<_>>(),
        vec![&child.task_id]
    );
}

#[test]
fn child_of_completed_parent_is_not_runnable() {
    let store = TaskStore::new();
    let mut root = test_task("task-root-done", "task-root-done", None);
    root.status = TaskStatus::Completed;
    let child = test_task(
        "task-late-child",
        "task-root-done",
        Some(root.task_id.clone()),
    );
    store.insert_task(root.clone()).expect("根任务应插入");
    store.insert_task(child).expect("子任务应插入");

    assert!(store.get_runnable_leaves(&root.task_id).is_empty());
}

#[test]
fn dispatcher_receives_updates_committed_after_runnable_scan_before_lease() {
    struct SnapshotDispatcher;
    impl TaskDispatcher for SnapshotDispatcher {
        fn dispatch(
            &self,
            task: &Task,
            _worker: &WorkerInfo,
            _lease: &magi_orchestrator::task_store::TaskLease,
            _permit: ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            assert_eq!(task.goal, "updated before grant");
            assert_eq!(task.status, TaskStatus::Running);
            Ok(())
        }
    }
    let store = Arc::new(TaskStore::new());
    let root = test_task("fresh-dispatch-root", "fresh-dispatch-root", None);
    store.insert_task(root.clone()).unwrap();
    let gate_store = Arc::clone(&store);
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        vec![WorkerInfo {
            worker_id: WorkerId::new("fresh-dispatch-worker"),
            role: "executor".into(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: None,
            system_prompt_template: None,
        }],
        Arc::new(SnapshotDispatcher),
    )
    .with_dispatch_gate(Arc::new(move |task| {
        gate_store
            .update_task_goal(&task.task_id, "updated before grant".into())
            .unwrap();
        Ok(TaskDispatchGateDecision::Allow)
    }));
    assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
}

#[test]
fn parent_closing_during_admission_does_not_fail_unrelated_branch() {
    struct RecordingDispatcher(Arc<Mutex<Vec<TaskId>>>);
    impl TaskDispatcher for RecordingDispatcher {
        fn dispatch(
            &self,
            task: &Task,
            _worker: &WorkerInfo,
            _lease: &magi_orchestrator::task_store::TaskLease,
            _permit: ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push(task.task_id.clone());
            Ok(())
        }
    }
    let store = Arc::new(TaskStore::new());
    let mut root = test_task("admission-root", "admission-root", None);
    root.status = TaskStatus::Running;
    let mut parent = test_task(
        "admission-parent",
        "admission-root",
        Some(root.task_id.clone()),
    );
    parent.status = TaskStatus::Running;
    let child = test_task(
        "admission-child",
        "admission-root",
        Some(parent.task_id.clone()),
    );
    let sibling = test_task(
        "admission-sibling",
        "admission-root",
        Some(root.task_id.clone()),
    );
    for task in [&root, &parent, &child, &sibling] {
        store.insert_task(task.clone()).unwrap();
    }
    let gate_store = Arc::clone(&store);
    let gate_child = child.task_id.clone();
    let gate_parent = parent.task_id.clone();
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        vec![WorkerInfo {
            worker_id: WorkerId::new("admission-worker"),
            role: "executor".into(),
            supported_kinds: vec![TaskKind::LocalAgent],
            parallelism_limit: None,
            system_prompt_template: None,
        }],
        Arc::new(RecordingDispatcher(Arc::clone(&dispatched))),
    )
    .with_dispatch_gate(Arc::new(move |task| {
        if task.task_id == gate_child {
            gate_store
                .complete_task(
                    &gate_parent,
                    TaskCompletionAttempt {
                        final_response: Some("父任务已在接纳窗口完成".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        Ok(TaskDispatchGateDecision::Allow)
    }));
    assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
    assert_eq!(*dispatched.lock().unwrap(), vec![sibling.task_id]);
    assert!(store.get_active_lease(&child.task_id).is_none());
    assert_eq!(
        store.get_task(&child.task_id).unwrap().status,
        TaskStatus::Pending
    );
    assert_eq!(
        store.get_task(&root.task_id).unwrap().status,
        TaskStatus::Running
    );
}

#[test]
fn dispatch_gate_blocks_without_lease_or_status_change() {
    let store = Arc::new(TaskStore::new());
    let root = test_task("task-root-gated", "task-root-gated", None);
    store.insert_task(root.clone()).expect("根任务应插入");
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        Vec::new(),
        Arc::new(RejectingDispatcher),
    )
    .with_dispatch_gate(Arc::new(|task: &Task| {
        Ok(TaskDispatchGateDecision::Blocked(format!(
            "blocked for {}",
            task.task_id
        )))
    }));

    let outcome = runner.run_cycle(&root.task_id);

    assert!(matches!(
        outcome,
        RunCycleOutcome::Blocked { ref task_ids, ref reason }
            if task_ids == &vec![root.task_id.clone()]
                && reason.contains("blocked for task-root-gated")
    ));
    assert_eq!(
        store.get_task(&root.task_id).unwrap().status,
        TaskStatus::Pending
    );
    assert!(store.get_active_lease(&root.task_id).is_none());
}

#[test]
fn unmatched_task_is_reported_as_unrunnable() {
    let store = Arc::new(TaskStore::new());
    let root = test_task("task-root-unmatched", "task-root-unmatched", None);
    store.insert_task(root.clone()).expect("根任务应插入");
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        Vec::new(),
        Arc::new(RejectingDispatcher),
    );

    assert_eq!(
        runner.run_cycle(&root.task_id),
        RunCycleOutcome::Unrunnable(vec![root.task_id.clone()])
    );
    assert_eq!(
        store
            .get_task(&root.task_id)
            .expect("task should remain available for failure reporting")
            .status,
        TaskStatus::Pending
    );
}

#[test]
fn explicit_unknown_role_is_not_silently_reassigned_to_executor() {
    let store = Arc::new(TaskStore::new());
    let mut root = test_task(
        "task-explicit-unknown-role",
        "task-explicit-unknown-role",
        None,
    );
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("missing-role"));
    store.insert_task(root.clone()).expect("根任务应插入");
    let executor = WorkerInfo {
        worker_id: WorkerId::new("worker-executor-reassignment-check"),
        role: "executor".to_string(),
        supported_kinds: vec![TaskKind::LocalAgent],
        parallelism_limit: None,
        system_prompt_template: None,
    };
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        vec![executor],
        Arc::new(RejectingDispatcher),
    );

    assert_eq!(
        runner.run_cycle(&root.task_id),
        RunCycleOutcome::Unrunnable(vec![root.task_id.clone()])
    );
    assert_eq!(
        store
            .get_task(&root.task_id)
            .expect("task should remain pending")
            .status,
        TaskStatus::Pending
    );
}

#[test]
fn dynamic_worker_catalog_provider_sees_role_added_after_runner_creation() {
    let store = Arc::new(TaskStore::new());
    let mut root = test_task("task-dynamic-role", "task-dynamic-role", None);
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("dynamic-role"));
    store.insert_task(root.clone()).expect("任务应插入");

    let role_dir = tempfile::tempdir().expect("角色目录应创建");
    let registry = AgentRoleRegistry::builtin().with_user_role_dir(role_dir.path());
    let provider_registry = registry.clone();
    let provider = Arc::new(move || {
        let role_ids = provider_registry.spawnable_agent_role_ids();
        magi_orchestrator::task_worker_catalog::build_worker_catalog_for_roles(
            &provider_registry,
            role_ids,
        )
    });
    let dispatcher = Arc::new(HoldingDispatcher {
        permits: Mutex::new(Vec::new()),
    });
    let runner = TaskRunner::with_dispatcher(Arc::clone(&store), Vec::new(), dispatcher)
        .with_agent_role_registry(registry.clone())
        .with_worker_catalog_provider(provider);

    let mut role = registry.get("executor").expect("内置 executor 应存在");
    role.id = "dynamic-role".to_string();
    role.display_name = "动态角色".to_string();
    role.role = "动态角色".to_string();
    role.system_prompt = "你是动态加载的角色。".to_string();
    registry
        .save_user_role(role, None)
        .expect("运行中新增角色应保存成功");

    assert_eq!(
        runner.run_cycle(&root.task_id),
        RunCycleOutcome::Continue,
        "Runner 创建后新增的用户角色应能匹配后续任务"
    );
    let lease = store
        .get_active_lease(&root.task_id)
        .expect("任务应获得动态角色的活跃租约");
    assert_eq!(lease.role, "dynamic-role");
    assert_eq!(lease.worker_id.as_str(), "task-worker-dynamic-role");
}

#[test]
fn cancellation_receipt_is_lease_bound_idempotent_and_cannot_become_failure() {
    let store = TaskStore::new();
    let mut task = test_task("cancel-receipt", "cancel-receipt", None);
    task.status = TaskStatus::Running;
    store.insert_task(task.clone()).unwrap();
    let worker = WorkerId::new("cancel-worker");
    let old = store
        .grant_lease(
            &task.task_id,
            &task.root_task_id,
            &worker,
            "executor",
            DEFAULT_LEASE_DURATION_MS,
        )
        .unwrap();
    assert!(store.revoke_lease(&task.task_id, &old.lease_id));
    let current = store
        .grant_lease(
            &task.task_id,
            &task.root_task_id,
            &worker,
            "executor",
            DEFAULT_LEASE_DURATION_MS,
        )
        .unwrap();
    assert!(
        !apply_task_result(
            &store,
            TaskResult {
                task_id: task.task_id.clone(),
                lease_id: old.lease_id,
                outcome: TaskOutcome::Cancelled,
            }
        )
        .unwrap()
    );
    assert_eq!(
        store.get_task(&task.task_id).unwrap().status,
        TaskStatus::Running
    );
    assert!(
        apply_task_result(
            &store,
            TaskResult {
                task_id: task.task_id.clone(),
                lease_id: current.lease_id.clone(),
                outcome: TaskOutcome::Cancelled,
            }
        )
        .unwrap()
    );
    for outcome in [
        TaskOutcome::Cancelled,
        TaskOutcome::Failed {
            error: "late failure".into(),
        },
    ] {
        assert!(
            !apply_task_result(
                &store,
                TaskResult {
                    task_id: task.task_id.clone(),
                    lease_id: current.lease_id.clone(),
                    outcome,
                }
            )
            .unwrap()
        );
    }
    assert_eq!(
        store.get_task(&task.task_id).unwrap().status,
        TaskStatus::Killed
    );
}

#[test]
fn stale_lease_result_cannot_overwrite_restarted_task() {
    let store = Arc::new(TaskStore::new());
    let mut root = test_task("task-stale-result", "task-stale-result", None);
    root.status = TaskStatus::Running;
    store.insert_task(root.clone()).expect("根任务应插入");
    let worker_id = WorkerId::new("worker-stale-result");
    let stale_lease = store
        .grant_lease(
            &root.task_id,
            &root.root_task_id,
            &worker_id,
            "executor",
            DEFAULT_LEASE_DURATION_MS,
        )
        .expect("stale lease should grant");
    assert!(store.revoke_lease(&root.task_id, &stale_lease.lease_id));
    let current_lease = store
        .grant_lease(
            &root.task_id,
            &root.root_task_id,
            &worker_id,
            "executor",
            DEFAULT_LEASE_DURATION_MS,
        )
        .expect("current lease should grant");
    let result = apply_task_result(
        &store,
        TaskResult {
            task_id: root.task_id.clone(),
            lease_id: stale_lease.lease_id,
            outcome: TaskOutcome::Failed {
                error: "旧执行轮迟到失败".to_string(),
            },
        },
    );
    assert!(!result.unwrap(), "迟到结果不能修改当前任务或租约");
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        Vec::new(),
        Arc::new(RejectingDispatcher),
    );

    assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
    assert_eq!(
        store
            .get_task(&root.task_id)
            .expect("task should exist")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        store
            .get_active_lease(&root.task_id)
            .expect("current lease should remain")
            .lease_id,
        current_lease.lease_id
    );
}

#[test]
fn completion_persistence_error_is_reported_without_probing_task_memory_state() {
    let store = Arc::new(TaskStore::new());
    let mut root = test_task(
        "task-completion-persistence",
        "task-completion-persistence",
        None,
    );
    root.status = TaskStatus::Running;
    store.insert_task(root.clone()).expect("root should insert");
    let lease = store
        .grant_lease(
            &root.task_id,
            &root.root_task_id,
            &WorkerId::new("worker-completion-persistence"),
            "executor",
            DEFAULT_LEASE_DURATION_MS,
        )
        .expect("lease should grant");
    store.set_checkpoint_callback(Box::new(|_| {
        Err(DomainError::Persistence {
            message: "completion checkpoint unavailable".to_string(),
        })
    }));

    let result = apply_task_result(
        &store,
        TaskResult {
            task_id: root.task_id.clone(),
            lease_id: lease.lease_id.clone(),
            outcome: TaskOutcome::Completed {
                attempt: TaskCompletionAttempt {
                    output_refs: vec!["output://completion".to_string()],
                    final_response: Some("完成".to_string()),
                    evidence: Vec::new(),
                },
            },
        },
    );
    let runner = TaskRunner::with_dispatcher(
        Arc::clone(&store),
        Vec::new(),
        Arc::new(RejectingDispatcher),
    );

    let error = result.unwrap_err();
    assert!(error.contains("完成事实持久化失败"), "{error}");
    assert!(
        error.contains("completion checkpoint unavailable"),
        "{error}"
    );
    assert_eq!(runner.run_cycle(&root.task_id), RunCycleOutcome::Continue);
    assert_eq!(
        store
            .get_task(&root.task_id)
            .expect("task should exist")
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        store
            .get_active_lease(&root.task_id)
            .expect("lease should remain active")
            .lease_id,
        lease.lease_id
    );
}

#[test]
fn concurrent_runners_claim_one_task_once_without_a_conversation_lock() {
    let store = Arc::new(TaskStore::new());
    let task = test_task("single-dispatch", "single-dispatch", None);
    store.insert_task(task.clone()).unwrap();
    let dispatcher = Arc::new(HoldingDispatcher {
        permits: Mutex::new(Vec::new()),
    });
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let outcomes = std::thread::scope(|scope| {
        (0..8)
            .map(|_| {
                let store = store.clone();
                let dispatcher = dispatcher.clone();
                let barrier = barrier.clone();
                let task_id = task.task_id.clone();
                scope.spawn(move || {
                    TaskRunner::with_dispatcher(
                        store,
                        vec![WorkerInfo {
                            worker_id: WorkerId::new("single-dispatch-worker"),
                            role: "executor".into(),
                            supported_kinds: vec![TaskKind::LocalAgent],
                            parallelism_limit: None,
                            system_prompt_template: None,
                        }],
                        dispatcher,
                    )
                    // 各 Runner 使用独立准入器，强制验证 TaskStore 的原子租约，
                    // 不能由共享容量计数碰巧挡住重复派发。
                    .with_execution_admission(
                        Arc::new(ExecutionAdmissionController::new(
                            ExecutionAdmissionLimits {
                                min_available_memory_bytes: 0,
                                ..Default::default()
                            },
                        )),
                        None,
                    )
                    .with_dispatch_gate(Arc::new(move |_| {
                        barrier.wait();
                        Ok(TaskDispatchGateDecision::Allow)
                    }))
                    .run_cycle(&task_id)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|run| run.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == RunCycleOutcome::Continue)
            .count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(dispatcher.permits.lock().unwrap().len(), 1);
    assert_eq!(
        store.get_task(&task.task_id).unwrap().status,
        TaskStatus::Running
    );
    assert!(store.get_active_lease(&task.task_id).is_some());
}

#[test]
fn shared_execution_admission_blocks_other_runners_until_the_running_task_finishes() {
    let store = Arc::new(TaskStore::new());
    let first_root = test_task("task-admission-first", "task-admission-first", None);
    let second_root = test_task("task-admission-second", "task-admission-second", None);
    store
        .insert_task(first_root.clone())
        .expect("第一个根任务应插入");
    store
        .insert_task(second_root.clone())
        .expect("第二个根任务应插入");
    let worker = WorkerInfo {
        worker_id: WorkerId::new("worker-admission-executor"),
        role: "executor".to_string(),
        supported_kinds: vec![TaskKind::LocalAgent],
        parallelism_limit: None,
        system_prompt_template: None,
    };
    let controller = Arc::new(
        crate::execution_admission::ExecutionAdmissionController::new(ExecutionAdmissionLimits {
            max_active_tasks: 1,
            max_active_tasks_per_session: 1,
            max_active_tasks_per_role: 1,
            min_available_memory_bytes: 0,
        }),
    );
    let dispatcher = Arc::new(HoldingDispatcher {
        permits: Mutex::new(Vec::new()),
    });
    let first_runner =
        TaskRunner::with_dispatcher(Arc::clone(&store), vec![worker.clone()], dispatcher.clone())
            .with_execution_admission(
                Arc::clone(&controller),
                Some(SessionId::new("session-admission-first")),
            );
    let second_runner =
        TaskRunner::with_dispatcher(Arc::clone(&store), vec![worker], dispatcher.clone())
            .with_execution_admission(
                Arc::clone(&controller),
                Some(SessionId::new("session-admission-second")),
            );

    assert_eq!(
        first_runner.run_cycle(&first_root.task_id),
        RunCycleOutcome::Continue
    );
    let blocked = second_runner.run_cycle(&second_root.task_id);
    assert!(matches!(
        blocked,
        RunCycleOutcome::Blocked { ref task_ids, ref reason }
            if task_ids == &vec![second_root.task_id.clone()]
                && reason.contains("全局执行容量已满")
    ));
    assert_eq!(
        store
            .get_task(&second_root.task_id)
            .expect("queued task should exist")
            .status,
        TaskStatus::Pending
    );
    assert!(store.get_active_lease(&second_root.task_id).is_none());
    assert_eq!(controller.snapshot().queued_task_count, 1);

    dispatcher.release_all();
    assert_eq!(
        second_runner.run_cycle(&second_root.task_id),
        RunCycleOutcome::Continue
    );
    assert_eq!(controller.snapshot().active_task_count, 1);
}
