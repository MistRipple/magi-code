use super::*;
use magi_core::{AssignmentId, MissionId, SessionId, WorkerId};

fn make_params(content: &str) -> CreateTodoParams {
    CreateTodoParams {
        session_id: Some(SessionId::new("sess-1")),
        mission_id: MissionId::new("mission-1"),
        assignment_id: AssignmentId::new("assign-1"),
        parent_id: None,
        source: None,
        content: content.to_string(),
        reasoning: "test".to_string(),
        todo_type: TodoType::Implementation,
        worker_id: WorkerId::new("worker-1"),
        priority: Some(3),
        expected_output: None,
        prompt: None,
        depends_on: None,
        required_contracts: None,
        produces_contracts: None,
        target_files: None,
        required: None,
        effort_weight: None,
        timeout_ms: None,
        max_retries: None,
    }
}

#[test]
fn test_create_and_get() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("implement feature A")).unwrap();
    assert_eq!(todo.status, TodoStatus::Pending);
    assert_eq!(todo.content, "implement feature A");

    let fetched = mgr.get(&todo.id).unwrap();
    assert_eq!(fetched.content, "implement feature A");
}

#[test]
fn test_lifecycle_pending_running_completed() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("task 1")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Running);

    mgr.complete(
        &id,
        Some(TodoOutput {
            success: true,
            summary: "done".into(),
            modified_files: vec!["a.rs".into()],
            new_contracts: None,
            issues: None,
            error: None,
            duration_ms: 100,
            token_usage: None,
        }),
    )
    .unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Completed);
    assert_eq!(mgr.get(&id).unwrap().progress, 100);
}

#[test]
fn test_fail_and_retry() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("task retry")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.fail(&id, "some error".into()).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Failed);

    mgr.retry(&id).unwrap();
    let retried = mgr.get(&id).unwrap();
    assert_eq!(retried.status, TodoStatus::Pending);
    assert_eq!(retried.retry_count, 1);
}

#[test]
fn test_retry_max_retries() {
    let mut mgr = TodoManager::new();
    let mut params = make_params("limited retries");
    params.max_retries = Some(1);
    let todo = mgr.create(params).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.fail(&id, "err".into()).unwrap();
    mgr.retry(&id).unwrap();
    mgr.start(&id).unwrap();
    mgr.fail(&id, "err again".into()).unwrap();

    let result = mgr.retry(&id);
    assert!(result.is_err());
}

#[test]
fn test_skip() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("skippable")).unwrap();
    let id = todo.id.clone();

    mgr.skip(&id, Some("not needed".into())).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Skipped);
}

#[test]
fn test_cancel() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("cancellable")).unwrap();
    let id = todo.id.clone();

    mgr.cancel(&id, Some("user cancelled".into())).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Cancelled);
}

#[test]
fn test_cancel_running() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("cancel running")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.cancel(&id, None).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Cancelled);
}

#[test]
fn test_dependency_blocking() {
    let mut mgr = TodoManager::new();

    let t1 = mgr.create(make_params("step 1")).unwrap();
    let t1_id = t1.id.clone();

    let mut p2 = make_params("step 2 depends on step 1");
    p2.depends_on = Some(vec![t1_id.clone()]);
    let t2 = mgr.create(p2).unwrap();
    let t2_id = t2.id.clone();

    let gate = mgr.get_execution_gate(&t2_id).unwrap();
    assert!(!gate.executable);
    assert_eq!(
        gate.blocked_by,
        Some(TodoExecutionGateBlocker::Dependencies)
    );

    mgr.start(&t1_id).unwrap();
    mgr.complete(&t1_id, None).unwrap();

    let gate2 = mgr.get_execution_gate(&t2_id).unwrap();
    assert!(gate2.executable);
}

#[test]
fn test_contract_blocking() {
    let mut mgr = TodoManager::new();

    let mut p = make_params("needs contract");
    p.required_contracts = Some(vec!["api-ready".into()]);
    let todo = mgr.create(p).unwrap();
    let id = todo.id.clone();

    let gate = mgr.get_execution_gate(&id).unwrap();
    assert!(!gate.executable);
    assert_eq!(gate.blocked_by, Some(TodoExecutionGateBlocker::Contracts));

    mgr.register_contract("api-ready");

    let gate2 = mgr.get_execution_gate(&id).unwrap();
    assert!(gate2.executable);
}

#[test]
fn test_approval_flow() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("out of scope task")).unwrap();
    let id = todo.id.clone();

    mgr.request_approval(&id, Some("needs approval".into()), None)
        .unwrap();
    let t = mgr.get(&id).unwrap();
    assert!(t.out_of_scope);
    assert_eq!(t.approval_status, Some(ApprovalStatus::Pending));

    let gate = mgr.get_execution_gate(&id).unwrap();
    assert!(!gate.executable);
    assert!(gate.awaiting_approval);

    mgr.approve(&id, None).unwrap();
    let gate2 = mgr.get_execution_gate(&id).unwrap();
    assert!(gate2.executable);
}

#[test]
fn test_rejection_skips() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("reject me")).unwrap();
    let id = todo.id.clone();

    mgr.request_approval(&id, None, None).unwrap();
    mgr.reject(&id, "not needed".into()).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Skipped);
}

#[test]
fn test_queue_priority() {
    let mut mgr = TodoManager::new();

    let mut p1 = make_params("low priority");
    p1.priority = Some(5);
    let _t1 = mgr.create(p1).unwrap();

    let mut p2 = make_params("high priority");
    p2.priority = Some(1);
    let t2 = mgr.create(p2).unwrap();

    let mut p3 = make_params("medium priority");
    p3.priority = Some(3);
    let _t3 = mgr.create(p3).unwrap();

    let first = mgr.peek().unwrap();
    assert_eq!(first.priority, 1);
    assert_eq!(first.id, t2.id);

    let batch = mgr.dequeue_batch(3);
    assert_eq!(batch.len(), 3);
    assert_eq!(batch[0].priority, 1);
    assert_eq!(batch[1].priority, 3);
    assert_eq!(batch[2].priority, 5);
}

#[test]
fn test_find_claimable() {
    let mut mgr = TodoManager::new();

    let t1 = mgr.create(make_params("for worker-1")).unwrap();

    let mut p2 = make_params("for worker-2");
    p2.worker_id = WorkerId::new("worker-2");
    let _t2 = mgr.create(p2).unwrap();

    let claimable = mgr.find_claimable("mission-1", Some("worker-1"));
    assert_eq!(claimable.len(), 1);
    assert_eq!(claimable[0].id, t1.id);

    let all_claimable = mgr.find_claimable("mission-1", None);
    assert_eq!(all_claimable.len(), 2);
}

#[test]
fn test_try_claim() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("claim me")).unwrap();
    let id = todo.id.clone();

    let claimed = mgr.try_claim(&id).unwrap();
    assert!(claimed.is_some());
    assert_eq!(claimed.unwrap().status, TodoStatus::Running);

    let second = mgr.try_claim(&id).unwrap();
    assert!(second.is_none());
}

#[test]
fn test_mission_completion_check() {
    let mut mgr = TodoManager::new();

    let t1 = mgr.create(make_params("task a")).unwrap();
    let t2 = mgr.create(make_params("task b")).unwrap();

    let check = mgr.check_mission_completion("mission-1");
    assert!(!check.all_done);
    assert_eq!(check.pending, 2);

    mgr.start(&t1.id).unwrap();
    mgr.complete(&t1.id, None).unwrap();
    mgr.start(&t2.id).unwrap();
    mgr.complete(&t2.id, None).unwrap();

    let check2 = mgr.check_mission_completion("mission-1");
    assert!(check2.all_done);
    assert_eq!(check2.completed, 2);
}

#[test]
fn test_parent_auto_complete() {
    let mut mgr = TodoManager::new();

    let parent = mgr.create(make_params("parent task")).unwrap();
    let parent_id = parent.id.clone();

    let mut cp1 = make_params("child 1");
    cp1.parent_id = Some(parent_id.clone());
    let c1 = mgr.create(cp1).unwrap();

    let mut cp2 = make_params("child 2");
    cp2.parent_id = Some(parent_id.clone());
    let c2 = mgr.create(cp2).unwrap();

    mgr.start(&c1.id).unwrap();
    mgr.complete(&c1.id, None).unwrap();

    assert_ne!(
        mgr.get(&parent_id).unwrap().status,
        TodoStatus::Completed
    );

    mgr.start(&c2.id).unwrap();
    mgr.complete(&c2.id, None).unwrap();

    assert_eq!(
        mgr.get(&parent_id).unwrap().status,
        TodoStatus::Completed
    );
}

#[test]
fn test_stats() {
    let mut mgr = TodoManager::new();

    mgr.create(make_params("a")).unwrap();
    let t2 = mgr.create(make_params("b")).unwrap();
    mgr.start(&t2.id).unwrap();
    mgr.complete(&t2.id, None).unwrap();

    let stats = mgr.get_stats();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.by_status.pending, 1);
    assert_eq!(stats.by_status.completed, 1);
    assert!(stats.completion_rate > 0.0);
}

#[test]
fn test_events_emitted() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("event test")).unwrap();
    let id = todo.id.clone();

    let events = mgr.drain_events();
    assert!(events.iter().any(|e| matches!(e, TodoEvent::Created { .. })));

    mgr.start(&id).unwrap();
    let events = mgr.drain_events();
    assert!(events.iter().any(|e| matches!(e, TodoEvent::Started { .. })));

    mgr.complete(&id, None).unwrap();
    let events = mgr.drain_events();
    assert!(events
        .iter()
        .any(|e| matches!(e, TodoEvent::Completed { .. })));
}

#[test]
fn test_reset_to_pending() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("reset me")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.complete(&id, None).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Completed);

    mgr.reset_to_pending(&id, false).unwrap();
    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Pending);
}

#[test]
fn test_revise_plan() {
    let mut mgr = TodoManager::new();
    let t1 = mgr.create(make_params("original task")).unwrap();

    let feedback = PlanReviewFeedback {
        status: ReviewStatus::NeedsRevision,
        todos_to_add: vec![make_params("new task from review")],
        todos_to_remove: vec![t1.id.clone()],
        todos_to_modify: vec![],
        comments: Some("need restructure".into()),
        rejection_reason: None,
    };

    let result = mgr.revise_plan("mission-1", feedback).unwrap();
    assert_eq!(result.todos_added, 1);
    assert_eq!(result.todos_removed, 1);
    assert!(mgr.get(&t1.id).is_none());
}

#[test]
fn test_handle_timeout() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("timeout task")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.handle_timeout(&id);

    assert_eq!(mgr.get(&id).unwrap().status, TodoStatus::Failed);
    let events = mgr.drain_events();
    assert!(events.iter().any(|e| matches!(e, TodoEvent::Timeout { .. })));
}

#[test]
fn test_idempotent_complete() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("idempotent")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.complete(&id, None).unwrap();
    let result = mgr.complete(&id, None);
    assert!(result.is_ok());
}

#[test]
fn test_cleanup() {
    let mut mgr = TodoManager::new();
    let todo = mgr.create(make_params("old task")).unwrap();
    let id = todo.id.clone();

    mgr.start(&id).unwrap();
    mgr.complete(&id, None).unwrap();

    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 100_000;
    let cleaned = mgr.cleanup(future);
    assert_eq!(cleaned, 1);
    assert!(mgr.get(&id).is_none());
}
