use crate::task_store::TaskStore;
use magi_core::{MissionId, Task, TaskId, TaskKind, TaskStatus, UtcMillis, WorkerId};
use std::sync::Arc;

fn root_task_id_for_mission(mission_id: &MissionId) -> TaskId {
    TaskId::new(format!("task-root-{}", mission_id.as_str()))
}

fn seed_action_tasks(
    task_store: &TaskStore,
    mission_id: &MissionId,
    mission_title: &str,
    tasks: &[(TaskId, &str, TaskStatus)],
) -> TaskId {
    let root_task_id = root_task_id_for_mission(mission_id);
    let now = UtcMillis::now();
    task_store
        .insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: mission_title.to_string(),
            goal: mission_title.to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: tasks
                .iter()
                .map(|(task_id, _, _)| task_id.clone())
                .collect(),
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
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        })
        .expect("根任务应插入");
    for (task_id, title, status) in tasks {
        task_store
            .insert_task(Task {
                task_id: task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id: Some(root_task_id.clone()),
                kind: TaskKind::LocalAgent,
                title: (*title).to_string(),
                goal: (*title).to_string(),
                status: *status,
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
                runtime_payload: magi_core::TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            })
            .expect("子任务应插入");
    }
    root_task_id
}

#[test]
fn task_store_remove_mission_removes_tasks_leases_and_checkpoints_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let task_store = TaskStore::new();
    let mission_id = MissionId::new("mission-delete");
    let child_task_id = TaskId::new("task-child-delete");
    let root_task_id = seed_action_tasks(
        &task_store,
        &mission_id,
        "delete mission",
        &[(child_task_id.clone(), "child", TaskStatus::Running)],
    );
    task_store
        .grant_lease(
            &child_task_id,
            &root_task_id,
            &WorkerId::new("worker-delete"),
            "executor",
            60_000,
        )
        .expect("lease should be granted");
    let checkpoint_count = Arc::new(AtomicUsize::new(0));
    let observed_checkpoint_count = checkpoint_count.clone();
    task_store.set_checkpoint_callback(Box::new(move |_| {
        observed_checkpoint_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));

    let removed = task_store
        .remove_tasks_by_mission(&mission_id)
        .expect("mission removal should checkpoint");

    assert_eq!(removed.len(), 2);
    assert!(task_store.get_tasks_by_mission(&mission_id).is_empty());
    assert!(task_store.get_task(&root_task_id).is_none());
    assert!(task_store.get_task(&child_task_id).is_none());
    assert!(task_store.snapshot().leases.is_empty());
    assert_eq!(checkpoint_count.load(Ordering::SeqCst), 1);
}
