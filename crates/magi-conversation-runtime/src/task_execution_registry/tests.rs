use super::*;
use magi_core::{MissionId, Task, TaskKind, TaskRuntimePayload, TaskStatus, UtcMillis};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, SessionStore,
};

fn test_task(task_id: &str, root_task_id: &str, mission_id: &MissionId) -> Task {
    let now = UtcMillis(1_000);
    Task {
        task_id: TaskId::new(task_id),
        mission_id: mission_id.clone(),
        root_task_id: TaskId::new(root_task_id),
        parent_task_id: Some(TaskId::new(root_task_id)),
        kind: TaskKind::LocalAgent,
        title: format!("task {task_id}"),
        goal: format!("run task {task_id}"),
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
fn spawned_local_agent_child_registration_is_atomic_runtime_source() {
    use magi_settings_store::SettingsStore;

    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let registry = TaskExecutionRegistry::default();
    let session_id = SessionId::new("session-atomic-spawn");
    let workspace_id = Some(WorkspaceId::new("workspace-atomic-spawn"));
    let mission_id = MissionId::new("mission-atomic-spawn");
    let root_task_id = TaskId::new("task-root");
    let parent_worker_id = WorkerId::new("worker-parent");
    let now = UtcMillis(10_000);
    let mut parent = test_task(root_task_id.as_str(), root_task_id.as_str(), &mission_id);
    parent.parent_task_id = None;
    parent.status = TaskStatus::Running;
    parent.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    task_store.insert_task(parent).expect("真实父任务应注册");
    session_store
        .create_session(session_id.clone(), "spawn fixture")
        .unwrap();
    let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
    let admitted_turn = ActiveExecutionTurn {
        turn_id: "turn-atomic-spawn".to_string(),
        turn_seq: 1,
        accepted_at: now,
        completed_at: None,
        status: "running".to_string(),
        user_message: Some("spawn child".to_string()),
        items: Vec::new(),
    };
    session_store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            magi_session_store::TimelineEntryInput::new(
                format!("timeline-chain-{}", admitted_turn.turn_id),
                magi_session_store::TimelineEntryKind::UserMessage,
                admitted_turn.user_message.clone().unwrap_or_default(),
                admitted_turn.accepted_at,
            ),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-atomic-spawn".to_string(),
                workspace_id: workspace_id.clone(),
                branches: vec![ActiveExecutionBranch {
                    task_id: root_task_id.clone(),
                    worker_id: parent_worker_id,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-atomic-spawn-parent"),
                }],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "timeline-atomic-spawn".to_string(),
                    trimmed_text: Some("spawn child".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
        )
        .expect("active chain should be accepted");

    let parent_settings = SettingsStore::new();
    let parent_settings_snapshot = Arc::new(parent_settings.execution_snapshot());
    registry
        .insert(
            root_task_id.clone(),
            TaskExecutionPlan::Dispatch {
                target: magi_core::TaskExecutionTarget {
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    task_id: root_task_id.clone(),
                    requested_worker_id: None,
                    recovery_id: None,
                    execution_chain_ref: Some("chain-atomic-spawn".to_string()),
                },
                worker_id: WorkerId::new("worker-parent"),
                thread_id: ThreadId::new("thread-atomic-spawn-parent"),
                is_primary: true,
                session_id: session_id.clone(),
                turn_id: "turn-atomic-spawn".to_string(),
                workspace_id: workspace_id.clone(),
                execution_root: None,
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: None,
                images: Vec::new(),
                execution_settings_snapshot: Some(parent_settings_snapshot.clone()),
            },
        )
        .expect("父任务执行计划应严格插入注册表");
    registry
        .update_active_skill(
            &root_task_id,
            &session_store,
            &session_id,
            "turn-atomic-spawn",
            "code-review".to_string(),
        )
        .expect("dynamic parent skill activation should update runtime ownership");

    let child = test_task("task-child", root_task_id.as_str(), &mission_id);
    let skills = magi_skill_runtime::SkillRegistry::new();
    let skill = |restricted| magi_skill_runtime::SkillDefinition {
        description: String::new(),
        skill_id: "code-review".into(),
        title: "Review".into(),
        instruction: "Review".into(),
        metadata: magi_skill_runtime::SkillMetadata {
            category: "test".into(),
            tags: vec![],
        },
        restrict_standard_tools: restricted,
        allowed_tools: vec!["agent_spawn".into(), "file_read".into()],
        custom_tool_bindings: vec![],
        prompt_priority: 50,
    };
    skills.register(skill(true));
    registry
        .clone()
        .with_agent_spawn_preflight_runtime(AgentSpawnPreflightRuntime {
            skill_runtime: Some(Arc::new(magi_skill_runtime::SkillRuntime::new(
                skills.clone(),
            ))),
            ..Default::default()
        });
    assert!(
        registry
            .validate_spawn_skill_inheritance(&root_task_id, false)
            .is_err()
    );
    let rejected = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect_err("封闭范围不能通过不继承 Skill 绕过");
    assert!(rejected.to_string().contains("inherit_skill=true"));
    assert!(task_store.get_task(&child.task_id).is_none());
    let registered = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: true,
            now,
        })
        .expect("spawned child runtime registration should succeed");
    skills.register(skill(false));
    assert!(
        registry
            .validate_spawn_skill_inheritance(&root_task_id, false)
            .is_ok()
    );

    assert_eq!(registered.execution_chain_ref, "chain-atomic-spawn");
    assert!(
        task_store.get_task(&child.task_id).is_some(),
        "child task should be inserted by the atomic runtime registration entry"
    );
    assert_eq!(
        task_store
            .get_task(&child.task_id)
            .and_then(|task| task.parent_task_id),
        Some(root_task_id.clone())
    );

    let plan = registry
        .get(&child.task_id)
        .expect("child execution plan should be registered atomically");
    match plan {
        TaskExecutionPlan::Dispatch {
            thread_id,
            session_id: plan_session_id,
            workspace_id: plan_workspace_id,
            is_primary,
            use_tools,
            skill_name,
            execution_settings_snapshot,
            ..
        } => {
            assert_eq!(thread_id, registered.thread_id);
            assert_eq!(plan_session_id, session_id);
            assert_eq!(plan_workspace_id, workspace_id);
            assert!(!is_primary);
            assert!(use_tools);
            assert_eq!(skill_name.as_deref(), Some("code-review"));
            assert!(
                execution_settings_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| Arc::ptr_eq(snapshot, &parent_settings_snapshot)),
                "agent_spawn 子任务必须继承父任务执行快照"
            );
        }
    }

    let chain = session_store
        .active_execution_chain(&session_id)
        .expect("active execution chain should remain available");
    let child_branch = chain
        .branches
        .iter()
        .find(|branch| branch.task_id == child.task_id && branch.worker_id == registered.worker_id)
        .expect("child branch should exist");
    assert_eq!(child_branch.thread_id, registered.thread_id);
    assert_eq!(child_branch.skill_name.as_deref(), Some("code-review"));
    assert!(chain.contains_task(&child.task_id));
    assert!(
        chain
            .branches
            .iter()
            .any(|branch| branch.worker_id == registered.worker_id)
    );

    let task_state_before = format!("{:?}", task_store.get_task(&child.task_id));
    let plan_state_before = format!("{:?}", registry.get(&child.task_id));
    let chain_state_before = format!("{:?}", session_store.active_execution_chain(&session_id));
    let threads_state_before = format!("{:?}", session_store.thread_registry_snapshot(&session_id));

    let duplicate_plan = registry
        .get(&child.task_id)
        .expect("重复插入测试需要保留原执行计划");
    assert!(
        registry
            .insert(child.task_id.clone(), duplicate_plan)
            .is_err(),
        "执行注册表不得覆盖已有执行计划"
    );
    let duplicate_error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect_err("重复注册子任务必须被拒绝");
    assert!(matches!(
        duplicate_error,
        SpawnedChildExecutionError::InvalidState(message)
            if message.contains("已存在")
    ));
    assert_eq!(
        format!("{:?}", task_store.get_task(&child.task_id)),
        task_state_before,
        "重复注册不得改变 TaskStore"
    );
    assert_eq!(
        format!("{:?}", registry.get(&child.task_id)),
        plan_state_before,
        "重复注册不得改变执行注册表"
    );
    assert_eq!(
        format!("{:?}", session_store.active_execution_chain(&session_id)),
        chain_state_before,
        "重复注册不得改变 session active chain"
    );
    assert_eq!(
        format!("{:?}", session_store.thread_registry_snapshot(&session_id)),
        threads_state_before,
        "重复注册不得改变 session thread registry"
    );

    task_store
        .remove_task(&child.task_id)
        .expect("移除测试任务应成功")
        .expect("测试任务应存在");
    let task_state_before = format!("{:?}", task_store.get_task(&child.task_id));
    let plan_state_before = format!("{:?}", registry.get(&child.task_id));
    let chain_state_before = format!("{:?}", session_store.active_execution_chain(&session_id));
    let threads_state_before = format!("{:?}", session_store.thread_registry_snapshot(&session_id));
    let registry_duplicate_error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect_err("仅执行注册表中已有的任务也必须被拒绝");
    assert!(matches!(
        registry_duplicate_error,
        SpawnedChildExecutionError::InvalidState(message)
            if message.contains("执行注册表")
    ));
    assert_eq!(
        format!("{:?}", task_store.get_task(&child.task_id)),
        task_state_before,
        "执行注册表重复注册不得写入 TaskStore"
    );
    assert_eq!(
        format!("{:?}", registry.get(&child.task_id)),
        plan_state_before,
        "执行注册表重复注册不得改变执行计划"
    );
    assert_eq!(
        format!("{:?}", session_store.active_execution_chain(&session_id)),
        chain_state_before,
        "执行注册表重复注册不得改变 session active chain"
    );
    assert_eq!(
        format!("{:?}", session_store.thread_registry_snapshot(&session_id)),
        threads_state_before,
        "执行注册表重复注册不得改变 session thread registry"
    );

    // 同一父任务下未结束的子代理达到上限时拒绝注册，且不写入任何状态。
    for index in 0..MAX_OPEN_CHILDREN_PER_PARENT {
        let open_child = test_task(
            &format!("task-child-open-{index}"),
            root_task_id.as_str(),
            &mission_id,
        );
        task_store
            .insert_task(open_child)
            .expect("未结束子任务应插入");
    }
    let fanout_child = test_task("task-child-fanout", root_task_id.as_str(), &mission_id);
    let chain_state_before = format!("{:?}", session_store.active_execution_chain(&session_id));
    let threads_state_before = format!("{:?}", session_store.thread_registry_snapshot(&session_id));
    let fanout_error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &fanout_child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect_err("未结束子代理达到上限时必须拒绝注册");
    assert!(matches!(
        fanout_error,
        SpawnedChildExecutionError::InvalidState(message)
            if message.contains("未结束的子代理")
    ));
    assert!(task_store.get_task(&fanout_child.task_id).is_none());
    assert!(registry.get(&fanout_child.task_id).is_none());
    assert_eq!(
        format!("{:?}", session_store.active_execution_chain(&session_id)),
        chain_state_before,
    );
    assert_eq!(
        format!("{:?}", session_store.thread_registry_snapshot(&session_id)),
        threads_state_before,
    );

    // 已结束的子代理不再占用名额，同一父任务可以持续派发新代理。
    for index in 0..MAX_OPEN_CHILDREN_PER_PARENT {
        let task_id = TaskId::new(format!("task-child-open-{index}"));
        let mut finished = task_store
            .remove_task(&task_id)
            .expect("移除测试任务应成功")
            .expect("测试任务应存在");
        finished.status = TaskStatus::Killed;
        task_store
            .insert_task(finished)
            .expect("已结束子任务应插入");
    }
    registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &fanout_child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect("已结束的子代理不应占用父任务名额");
    match registry.get(&fanout_child.task_id).expect("执行计划应注册") {
        TaskExecutionPlan::Dispatch { skill_name, .. } => assert_eq!(
            skill_name, None,
            "未声明 inherit_skill 时子代理不继承主线 Skill"
        ),
    }
}

#[test]
fn non_git_workspace_allows_only_one_unfinished_writable_agent() {
    let (
        task_store,
        session_store,
        registry,
        session_id,
        workspace_id,
        mission_id,
        root_task_id,
        now,
    ) = spawn_fixture("non-git-writer");
    let register = |child: &Task| {
        registry.register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
    };
    let first_writer = test_task("task-writer-a", root_task_id.as_str(), &mission_id);
    register(&first_writer).expect("第一个可写代理应注册");
    let mut reader = test_task("task-reader", root_task_id.as_str(), &mission_id);
    let mut read_only_policy =
        crate::agent_spawn_preflight::agent_spawn_child_policy_snapshot(None);
    read_only_policy.access_profile = magi_core::AccessProfile::ReadOnly;
    reader.policy_snapshot = Some(read_only_policy);
    register(&reader).expect("只读代理不受单写者限制");

    let second_writer = test_task("task-writer-b", root_task_id.as_str(), &mission_id);
    let error = register(&second_writer).expect_err("第二个可写代理必须被拒绝");
    assert_eq!(
        error,
        SpawnedChildExecutionError::SharedWorkspaceWriterActive
    );
    assert!(error.to_string().contains("等待它结束后再派发"));
    assert!(task_store.get_task(&second_writer.task_id).is_none());
}

#[test]
fn remove_session_drops_every_execution_plan_owned_by_session() {
    let registry = TaskExecutionRegistry::default();
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let mission_id = MissionId::new("mission-registry-cleanup");
    for (task_id, session_id) in [
        (TaskId::new("task-a-root"), session_a.clone()),
        (TaskId::new("task-a-child"), session_a.clone()),
        (TaskId::new("task-b-root"), session_b.clone()),
    ] {
        registry
            .insert(
                task_id.clone(),
                TaskExecutionPlan::Dispatch {
                    target: magi_core::TaskExecutionTarget {
                        mission_id: mission_id.clone(),
                        root_task_id: task_id.clone(),
                        task_id,
                        requested_worker_id: None,
                        recovery_id: None,
                        execution_chain_ref: None,
                    },
                    worker_id: WorkerId::new("worker-registry-cleanup"),
                    thread_id: ThreadId::new("thread-registry-cleanup"),
                    is_primary: true,
                    session_id,
                    turn_id: "turn-registry-cleanup".to_string(),
                    workspace_id: None,
                    execution_root: None,
                    writebacks: ExecutionWritebackPlans::default(),
                    use_tools: true,
                    skill_name: None,
                    images: Vec::new(),
                    execution_settings_snapshot: None,
                },
            )
            .expect("清理测试执行计划应严格插入注册表");
    }

    let removed = registry.remove_session(&session_a);

    assert_eq!(removed.len(), 2);
    assert!(registry.get(&TaskId::new("task-a-root")).is_none());
    assert!(registry.get(&TaskId::new("task-a-child")).is_none());
    assert!(registry.get(&TaskId::new("task-b-root")).is_some());
}

#[test]
fn spawned_local_agent_child_registration_allows_five_agents_per_role() {
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let registry = TaskExecutionRegistry::default();
    let session_id = SessionId::new("session-agent-capacity");
    let workspace_id = Some(WorkspaceId::new("workspace-agent-capacity"));
    let mission_id = MissionId::new("mission-agent-capacity");
    let root_task_id = TaskId::new("task-root-capacity");
    let parent_worker_id = WorkerId::new("worker-parent-capacity");
    let now = UtcMillis(20_000);
    let mut parent = test_task(root_task_id.as_str(), root_task_id.as_str(), &mission_id);
    parent.parent_task_id = None;
    parent.status = TaskStatus::Running;
    parent.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    task_store.insert_task(parent).expect("真实父任务应注册");
    session_store
        .create_session(session_id.clone(), "spawn fixture")
        .unwrap();
    let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
    let admitted_turn = ActiveExecutionTurn {
        turn_id: "turn-agent-capacity".to_string(),
        turn_seq: 1,
        accepted_at: now,
        completed_at: None,
        status: "running".to_string(),
        user_message: Some("spawn children".to_string()),
        items: Vec::new(),
    };
    session_store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            magi_session_store::TimelineEntryInput::new(
                format!("timeline-chain-{}", admitted_turn.turn_id),
                magi_session_store::TimelineEntryKind::UserMessage,
                admitted_turn.user_message.clone().unwrap_or_default(),
                admitted_turn.accepted_at,
            ),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-agent-capacity".to_string(),
                workspace_id: workspace_id.clone(),
                branches: vec![ActiveExecutionBranch {
                    task_id: root_task_id.clone(),
                    worker_id: parent_worker_id,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-agent-capacity-parent"),
                }],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "timeline-agent-capacity".to_string(),
                    trimmed_text: Some("spawn children".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
        )
        .expect("active chain should be accepted");

    for (role_index, role) in ["executor", "reviewer"].into_iter().enumerate() {
        for instance_index in 0..5 {
            let child = read_only_test_task(
                &format!("task-child-capacity-{role}-{instance_index}"),
                root_task_id.as_str(),
                &mission_id,
            );
            registry
                .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                    task_store: &task_store,
                    session_store: &session_store,
                    child_task: &child,
                    session_id: &session_id,
                    workspace_id: &workspace_id,
                    role,
                    delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
                    role_parallelism_limit: Some(DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE as u32),
                    plan_store: None,
                    plan_item_id: None,
                    execution_root: None,
                    inherit_skill: false,
                    now: UtcMillis(now.0 + (role_index * 5 + instance_index) as u64 + 1),
                })
                .expect("默认容量应允许每个角色同时运行五个代理实例");
        }
    }

    let overflow_child = read_only_test_task(
        "task-child-capacity-executor-overflow",
        root_task_id.as_str(),
        &mission_id,
    );
    let error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &overflow_child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: Some(DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE as u32),
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now: UtcMillis(now.0 + 10),
        })
        .expect_err("同一角色的第六个并发代理应被角色实例上限拒绝");

    assert_eq!(
        error,
        SpawnedChildExecutionError::RoleCapacityExceeded {
            role: "executor".to_string(),
            active: DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE,
            limit: DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE,
        }
    );
    assert!(
        task_store.get_task(&overflow_child.task_id).is_none(),
        "被容量拒绝的子代理不能写入 task_store"
    );

    let completed_task_id = TaskId::new("task-child-capacity-executor-0");
    task_store
        .update_status_checked(&completed_task_id, TaskStatus::Running)
        .expect("executor 代理应进入运行态");
    task_store
        .complete_task(
            &completed_task_id,
            magi_core::TaskCompletionAttempt {
                output_refs: vec!["executor done".to_string()],
                final_response: Some("executor done".to_string()),
                evidence: Vec::new(),
            },
        )
        .expect("完成一个 executor 代理后应释放角色容量");
    registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &overflow_child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: Some(DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE as u32),
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now: UtcMillis(now.0 + 11),
        })
        .expect("同角色已有实例完成后，第六个代理应能占用释放的名额");
}

#[test]
fn spawned_child_is_published_only_after_runtime_preparation_and_rolls_back_on_commit_failure() {
    for fail_commit in [false, true] {
        let (tasks, sessions, registry, session, workspace, mission, root, now) =
            spawn_fixture("publish-last");
        let sessions = Arc::new(sessions);
        sessions
            .upsert_plan(
                &session,
                SessionPlan {
                    plan_id: magi_core::PlanId::new("publication-plan"),
                    session_id: session.clone(),
                    goal_id: None,
                    revision: 1,
                    language: "zh-CN".into(),
                    state: magi_core::PlanState::Active,
                    items: vec![magi_core::PlanItem::new(
                        magi_core::PlanItemId::new("work"),
                        "独立工作",
                        magi_core::PlanItemStatus::InProgress,
                    )],
                    task_bindings: HashMap::new(),
                    updated_at: now,
                },
                Some(0),
            )
            .unwrap();
        let plans = magi_plan::PlanStore::from_store(&sessions, session.clone());
        let chain_before = sessions.active_execution_chain(&session).unwrap();
        let threads_before = sessions.thread_registry_snapshot(&session);
        let child = test_task("publication-child", root.as_str(), &mission);
        let child_id = child.task_id.clone();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_registry = registry.clone();
        let callback_sessions = sessions.clone();
        let callback_session = session.clone();
        let callback_observed = observed.clone();
        tasks.set_checkpoint_callback(Box::new(move |snapshot| {
            assert!(snapshot.tasks.iter().any(|task| task.task_id == child_id));
            let plan = callback_registry
                .get(&child_id)
                .expect("Task 的发布提交之前必须已有执行计划，Runner 不能观察半注册任务");
            let TaskExecutionPlan::Dispatch { thread_id, .. } = plan;
            assert!(
                callback_sessions
                    .thread_registry_snapshot(&callback_session)
                    .iter()
                    .any(|thread| thread.thread_id == thread_id
                        && thread.handled_task_ids.contains(&child_id))
            );
            assert!(
                callback_sessions
                    .plan(&callback_session)
                    .unwrap()
                    .task_bindings
                    .contains_key(&child_id)
            );
            let admission = callback_registry.execution_admission.snapshot();
            assert!(
                admission.active_task_ids.contains(&child_id.to_string())
                    || admission
                        .queued
                        .iter()
                        .any(|entry| entry.task_id == child_id.as_str())
            );
            callback_observed.store(true, std::sync::atomic::Ordering::SeqCst);
            if fail_commit {
                Err(magi_core::DomainError::Persistence {
                    message: "injected publication failure".into(),
                })
            } else {
                Ok(())
            }
        }));
        let result = registry.register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &tasks,
            session_store: &sessions,
            child_task: &child,
            session_id: &session,
            workspace_id: &workspace,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: Some(&plans),
            plan_item_id: Some(magi_core::PlanItemId::new("work")),
            execution_root: None,
            inherit_skill: false,
            now,
        });
        assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(result.is_err(), fail_commit, "{result:?}");
        if fail_commit {
            assert!(tasks.get_task(&child.task_id).is_none());
            assert!(registry.get(&child.task_id).is_none());
            assert_eq!(sessions.thread_registry_snapshot(&session), threads_before);
            assert_eq!(
                format!("{:?}", sessions.active_execution_chain(&session).unwrap()),
                format!("{chain_before:?}")
            );
            assert!(plans.snapshot().unwrap().task_bindings.is_empty());
            let admission = registry.execution_admission.snapshot();
            assert!(admission.active_task_ids.is_empty() && admission.queued.is_empty());
        }
    }
}

#[test]
fn spawned_local_agent_child_registration_rolls_back_when_plan_binding_fails() {
    let (
        task_store,
        session_store,
        registry,
        session_id,
        workspace_id,
        mission_id,
        root_task_id,
        now,
    ) = spawn_fixture("plan-binding-rollback");
    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    session_store
        .upsert_plan(
            &session_id,
            magi_session_store::SessionPlan {
                plan_id: magi_core::PlanId::new("plan-binding-rollback"),
                session_id: session_id.clone(),
                goal_id: None,
                revision: 1,
                language: "zh-CN".to_string(),
                state: magi_core::PlanState::Active,
                items: vec![magi_core::PlanItem::new(
                    magi_core::PlanItemId::new("active-item"),
                    "绑定代理",
                    magi_core::PlanItemStatus::InProgress,
                )],
                task_bindings: HashMap::new(),
                updated_at: now,
            },
            Some(0),
        )
        .expect("测试计划应创建");

    let child = test_task(
        "task-child-plan-binding-rollback",
        root_task_id.as_str(),
        &mission_id,
    );
    let chain_before = session_store
        .active_execution_chain(&session_id)
        .expect("测试执行链应存在");
    let threads_before = session_store.thread_registry_snapshot(&session_id);
    let plan_before = plan_store.snapshot().expect("测试计划应存在");

    let error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &child,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: Some(&plan_store),
            plan_item_id: Some(magi_core::PlanItemId::new("missing-item")),
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect_err("计划绑定失败时必须回滚整次注册");

    assert!(
        matches!(error, SpawnedChildExecutionError::InvalidState(message) if message.contains("计划绑定失败"))
    );
    assert!(task_store.get_task(&child.task_id).is_none());
    assert!(registry.get(&child.task_id).is_none());
    assert_eq!(
        format!(
            "{:?}",
            session_store
                .active_execution_chain(&session_id)
                .expect("执行链应保留")
        ),
        format!("{:?}", chain_before),
        "计划绑定失败后必须恢复完整 active execution chain"
    );
    assert_eq!(
        session_store.thread_registry_snapshot(&session_id),
        threads_before,
        "计划绑定失败后不得残留执行 thread"
    );
    let plan_after = plan_store.snapshot().expect("计划应保留");
    assert_eq!(plan_after.task_bindings, plan_before.task_bindings);
}

#[test]
fn spawned_local_agent_child_registration_rejects_duplicate_canonical_name() {
    let (
        task_store,
        session_store,
        registry,
        session_id,
        workspace_id,
        mission_id,
        root_task_id,
        now,
    ) = spawn_fixture("canonical-name");
    let canonical_name = "/root/canonical-name";
    let mut first = test_task(
        "task-child-canonical-first",
        root_task_id.as_str(),
        &mission_id,
    );
    first.executor_binding = Some(
        magi_core::TaskExecutorBinding::for_role("executor")
            .with_canonical_task_name(canonical_name),
    );
    registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &first,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now,
        })
        .expect("第一个 canonical task name 应注册成功");

    let mut duplicate = test_task(
        "task-child-canonical-duplicate",
        root_task_id.as_str(),
        &mission_id,
    );
    duplicate.executor_binding = Some(
        magi_core::TaskExecutorBinding::for_role("executor")
            .with_canonical_task_name(canonical_name),
    );
    let error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &duplicate,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "executor",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: None,
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now: UtcMillis(now.0 + 1),
        })
        .expect_err("重复 canonical task name 必须被拒绝");

    assert!(
        matches!(error, SpawnedChildExecutionError::DuplicateTaskName { existing_task_id } if existing_task_id == first.task_id)
    );
    assert!(task_store.get_task(&duplicate.task_id).is_none());
    assert!(registry.get(&duplicate.task_id).is_none());
}

#[test]
fn autonomous_spawn_rejects_renamed_active_work_but_preserves_deliberate_reviews() {
    use magi_tool_runtime::AgentDelegationPurpose::*;
    for purpose in [
        ParallelWork,
        ContextIsolation,
        UserRequested,
        IndependentReview,
    ] {
        let (tasks, sessions, registry, session, workspace, mission, root, now) =
            spawn_fixture("duplicate-work");
        let mut first = read_only_test_task("first", root.as_str(), &mission);
        first.executor_binding = Some(
            magi_core::TaskExecutorBinding::for_role("explorer")
                .with_canonical_task_name("/root/first"),
        );
        first.runtime_payload = magi_core::TaskRuntimePayload::AgentContext {
                package: Box::new(crate::agent_spawn_preflight::parse_agent_context_package(
                    &serde_json::json!({"context_package": {
                        "summary":"隔离指定日志的探索输出", "constraints":["只读"],
                        "expected_output":"错误证据与结论", "references":[{"kind":"file", "source_ref":"server.log"}]
                    }}), &root, now, 1).unwrap()),
                accesses: vec![],
            };
        let register = |child_task: &Task, delegation_purpose, execution_root| {
            registry.register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &tasks,
                session_store: &sessions,
                child_task,
                session_id: &session,
                workspace_id: &workspace,
                role: "explorer",
                delegation_purpose,
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root,
                inherit_skill: false,
                now,
            })
        };
        register(&first, ContextIsolation, None).unwrap();
        let mut renamed = first.clone();
        renamed.task_id = TaskId::new("renamed");
        renamed
            .executor_binding
            .as_mut()
            .unwrap()
            .canonical_task_name = Some("/root/renamed".into());
        if let magi_core::TaskRuntimePayload::AgentContext { package, .. } =
            &mut renamed.runtime_payload
        {
            package.package_id = "different-package-id".into();
            package.references[0].reference_id = "different-reference-id".into();
            package.created_at = UtcMillis(now.0 + 1);
        }
        let threads_before = sessions.thread_registry_snapshot(&session);
        let result = register(&renamed, purpose, None);
        if matches!(purpose, ParallelWork | ContextIsolation) {
            assert!(
                matches!(result, Err(SpawnedChildExecutionError::DuplicateAgentWork { existing_task_id }) if existing_task_id == first.task_id)
            );
            assert!(tasks.get_task(&renamed.task_id).is_none());
            assert!(registry.get(&renamed.task_id).is_none());
            assert_eq!(sessions.thread_registry_snapshot(&session), threads_before);
            assert_eq!(tasks.get_children(&root).len(), 1);
            // 相同目标在不同目录、不同输入或不同验收范围下可以是独立工作。
            register(&renamed, purpose, Some(PathBuf::from("/other-scope"))).unwrap();
            assert_eq!(tasks.get_children(&root).len(), 2);
        } else {
            result.expect("明确要求的多个代理与独立复核不被相同输入误伤");
        }
    }
}

#[test]
fn work_package_comparison_preserves_different_inputs_and_acceptance() {
    let mission = MissionId::new("work-package");
    let mut task = test_task("candidate", "root", &mission);
    task.runtime_payload = magi_core::TaskRuntimePayload::AgentContext {
            package: Box::new(crate::agent_spawn_preflight::parse_agent_context_package(
                &serde_json::json!({"context_package": {"summary":"核对日志", "constraints":["只读"],
                    "expected_output":"证据", "references":[{"kind":"file", "source_ref":"alpha.log"}]}}),
                &TaskId::new("root"), UtcMillis(1), 0).unwrap()),
            accesses: vec![],
        };
    for change in 0..4 {
        let mut different = task.clone();
        if let magi_core::TaskRuntimePayload::AgentContext { package, .. } =
            &mut different.runtime_payload
        {
            match change {
                0 => package.references[0].source_ref = "beta.log".into(),
                1 => package.constraints.push("只检查重试错误".into()),
                2 => package.expected_output = "修复补丁".into(),
                _ => package.summary = "独立检查网络日志".into(),
            }
        }
        assert!(
            !same_agent_work_package(&task, &different),
            "change={change}"
        );
    }
}

/// 只读代理不受非 Git 工作区单写者限制，用于只关心容量语义的测试。
fn read_only_test_task(task_id: &str, root_task_id: &str, mission_id: &MissionId) -> Task {
    let mut task = test_task(task_id, root_task_id, mission_id);
    let mut policy = crate::agent_spawn_preflight::agent_spawn_child_policy_snapshot(None);
    policy.access_profile = magi_core::AccessProfile::ReadOnly;
    task.policy_snapshot = Some(policy);
    task
}

#[test]
fn mainline_skill_activation_rejects_stale_or_closed_turn_without_changing_the_chain() {
    let (_, sessions, _, session, _, _, root, _) = spawn_fixture("skill-source");
    let snapshot = || {
        (
            serde_json::to_value(sessions.durable_state()).unwrap(),
            serde_json::to_value(sessions.execution_sidecar_store_state()).unwrap(),
            sessions.execution_sidecar_flush_metadata(),
        )
    };
    let before = snapshot();
    for (task, turn) in [
        (root.clone(), "stale-turn"),
        (TaskId::new("foreign-task"), "turn-skill-source"),
    ] {
        assert!(
            sessions
                .update_execution_branch_active_skill(&session, &task, turn, "closed-skill")
                .is_err()
        );
        assert_eq!(snapshot(), before);
    }
    sessions
        .update_execution_branch_active_skill(&session, &root, "turn-skill-source", "closed-skill")
        .unwrap();
    let chain = sessions.active_execution_chain(&session).unwrap();
    assert_eq!(
        chain.dispatch_context.skill_name.as_deref(),
        Some("closed-skill")
    );
    assert_eq!(
        chain.branches[0].skill_name.as_deref(),
        Some("closed-skill")
    );
    sessions
        .update_current_turn_status_for_turn_with_change(
            &session,
            Some("turn-skill-source"),
            "completed",
        )
        .unwrap();
    let closed = snapshot();
    assert!(
        sessions
            .update_execution_branch_active_skill(
                &session,
                &root,
                "turn-skill-source",
                "another-skill"
            )
            .is_err()
    );
    assert_eq!(snapshot(), closed, "已关闭 Turn 的迟到激活不能更新恢复状态");
}

fn spawn_fixture(
    label: &str,
) -> (
    TaskStore,
    SessionStore,
    TaskExecutionRegistry,
    SessionId,
    Option<WorkspaceId>,
    MissionId,
    TaskId,
    UtcMillis,
) {
    let task_store = TaskStore::new();
    let session_store = SessionStore::new();
    let registry = TaskExecutionRegistry::default();
    let session_id = SessionId::new(format!("session-{label}"));
    let workspace_id = Some(WorkspaceId::new(format!("workspace-{label}")));
    let mission_id = MissionId::new(format!("mission-{label}"));
    let root_task_id = TaskId::new(format!("task-root-{label}"));
    let parent_worker_id = WorkerId::new(format!("worker-parent-{label}"));
    let now = UtcMillis(30_000);
    let mut parent = test_task(root_task_id.as_str(), root_task_id.as_str(), &mission_id);
    parent.parent_task_id = None;
    parent.status = TaskStatus::Running;
    parent.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    task_store.insert_task(parent).expect("真实父任务应注册");
    session_store
        .create_session(session_id.clone(), "spawn fixture")
        .unwrap();
    let _ = session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
    let admitted_turn = ActiveExecutionTurn {
        turn_id: format!("turn-{label}"),
        turn_seq: 1,
        accepted_at: now,
        completed_at: None,
        status: "running".to_string(),
        user_message: Some("spawn children".to_string()),
        items: Vec::new(),
    };
    session_store
        .accept_active_execution_chain_with_timeline_entry(
            session_id.clone(),
            magi_session_store::TimelineEntryInput::new(
                format!("timeline-chain-{}", admitted_turn.turn_id),
                magi_session_store::TimelineEntryKind::UserMessage,
                admitted_turn.user_message.clone().unwrap_or_default(),
                admitted_turn.accepted_at,
            ),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: format!("chain-{label}"),
                workspace_id: workspace_id.clone(),
                branches: vec![ActiveExecutionBranch {
                    task_id: root_task_id.clone(),
                    worker_id: parent_worker_id,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new(format!("thread-{label}-parent")),
                }],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: format!("timeline-{label}"),
                    trimmed_text: Some("spawn children".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
        )
        .expect("测试执行链应创建");
    (
        task_store,
        session_store,
        registry,
        session_id,
        workspace_id,
        mission_id,
        root_task_id,
        now,
    )
}

#[test]
fn spawned_local_agent_child_registration_uses_role_parallelism_limit() {
    let (
        task_store,
        session_store,
        registry,
        session_id,
        workspace_id,
        mission_id,
        root_task_id,
        now,
    ) = spawn_fixture("role-limit");

    let limited_first = read_only_test_task(
        "task-child-role-limit-0",
        root_task_id.as_str(),
        &mission_id,
    );
    registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &limited_first,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "limited-role",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: Some(1),
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now: UtcMillis(now.0 + 1),
        })
        .expect("并发上限为 1 时应允许第一个实例");

    let limited_second = read_only_test_task(
        "task-child-role-limit-1",
        root_task_id.as_str(),
        &mission_id,
    );
    let limited_error = registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &limited_second,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "limited-role",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: Some(1),
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now: UtcMillis(now.0 + 2),
        })
        .expect_err("并发上限为 1 时不应允许第二个活跃实例");
    assert_eq!(
        limited_error,
        SpawnedChildExecutionError::RoleCapacityExceeded {
            role: "limited-role".to_string(),
            active: 1,
            limit: 1,
        }
    );

    task_store
        .update_status_checked(&limited_first.task_id, TaskStatus::Running)
        .expect("受限角色实例应进入运行态");
    task_store
        .complete_task(
            &limited_first.task_id,
            magi_core::TaskCompletionAttempt {
                output_refs: vec!["limited done".to_string()],
                final_response: Some("limited done".to_string()),
                evidence: Vec::new(),
            },
        )
        .expect("完成受限角色实例后应释放容量");
    registry
        .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
            task_store: &task_store,
            session_store: &session_store,
            child_task: &limited_second,
            session_id: &session_id,
            workspace_id: &workspace_id,
            role: "limited-role",
            delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
            role_parallelism_limit: Some(1),
            plan_store: None,
            plan_item_id: None,
            execution_root: None,
            inherit_skill: false,
            now: UtcMillis(now.0 + 3),
        })
        .expect("前一个受限实例完成后应允许下一个实例");

    for index in 0..=DEFAULT_MAX_ACTIVE_AGENTS_PER_ROLE {
        let unlimited_child = read_only_test_task(
            &format!("task-child-role-unlimited-{index}"),
            root_task_id.as_str(),
            &mission_id,
        );
        registry
            .register_spawned_local_agent_child(SpawnedChildExecutionRequest {
                task_store: &task_store,
                session_store: &session_store,
                child_task: &unlimited_child,
                session_id: &session_id,
                workspace_id: &workspace_id,
                role: "unlimited-role",
                delegation_purpose: magi_tool_runtime::AgentDelegationPurpose::UserRequested,
                role_parallelism_limit: None,
                plan_store: None,
                plan_item_id: None,
                execution_root: None,
                inherit_skill: false,
                now: UtcMillis(now.0 + 4 + index as u64),
            })
            .expect("None 应表示不设置角色级并发上限");
    }
}
