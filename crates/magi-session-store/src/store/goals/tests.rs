use super::*;
use crate::models::{
    ActiveExecutionChain, ActiveExecutionDispatchContext, SessionExecutionSidecarStoreState,
};
use magi_core::MissionId;

fn create_test_goal(
    store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
    objective: &str,
    access_profile: AccessProfile,
) -> SessionGoal {
    let (_, thread_id) = store.ensure_session_mission(session_id, UtcMillis::now(), || {
        MissionId::new(format!("mission-{session_id}"))
    });
    store
        .create_goal(
            session_id.clone(),
            thread_id,
            turn_id,
            objective,
            access_profile,
        )
        .expect("goal should be created")
}

fn set_test_continuation_owner(
    store: &SessionStore,
    session_id: &SessionId,
    goal_id: &GoalId,
    turn_id: &str,
) {
    let mut state = store
        .state
        .write()
        .expect("session state write lock should hold");
    let goal = state
        .goals
        .iter_mut()
        .find(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
        .expect("goal should exist");
    goal.continuation = GoalContinuationState {
        phase: GoalContinuationPhase::Running,
        turn_id: Some(turn_id.to_string()),
        reason: None,
    };
}

#[test]
fn active_goal_access_profile_change_survives_durable_restore() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-access-profile");
    store
        .create_session(session_id.clone(), "goal access profile")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-access-profile",
        "验证访问模式持久化",
        AccessProfile::ReadOnly,
    );

    let updated = store
        .set_active_goal_access_profile(&session_id, AccessProfile::FullAccess)
        .expect("active goal access profile should update")
        .expect("active goal should exist");
    assert_eq!(updated.goal_id, goal.goal_id);
    assert_eq!(updated.access_profile, AccessProfile::FullAccess);

    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        SessionExecutionSidecarStoreState::default(),
    )
    .expect("目标访问模式的持久化恢复应成功");
    let restored_goal = restored
        .active_goal(&session_id)
        .expect("active goal should survive durable restore");
    assert_eq!(restored_goal.goal_id, goal.goal_id);
    assert_eq!(restored_goal.access_profile, AccessProfile::FullAccess);
}

#[test]
fn goal_creation_requires_registered_orchestrator_thread() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-thread-owner");
    store
        .create_session(session_id.clone(), "goal thread owner")
        .expect("session should be created");

    let missing_thread_error = store
        .create_goal(
            session_id.clone(),
            ThreadId::new("thread-unregistered"),
            "turn-goal-thread-owner",
            "验证 thread 归属",
            AccessProfile::Restricted,
        )
        .expect_err("goal must not exist before orchestrator thread registration");
    assert!(matches!(
        missing_thread_error,
        DomainError::GoalRejected {
            reason: GoalRejection::NoOrchestratorThread,
            ..
        }
    ));

    store.ensure_session_mission(&session_id, UtcMillis::now(), || {
        MissionId::new("mission-goal-thread-owner")
    });
    let wrong_thread_error = store
        .create_goal(
            session_id,
            ThreadId::new("thread-worker"),
            "turn-goal-thread-owner",
            "验证 thread 归属",
            AccessProfile::Restricted,
        )
        .expect_err("goal must belong to the registered orchestrator thread");
    assert!(matches!(wrong_thread_error, DomainError::Validation { .. }));
}

#[test]
fn goal_objective_length_is_bounded_for_create_and_update() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-objective-length");
    store
        .create_session(session_id.clone(), "goal objective length")
        .expect("session should be created");
    let (_, orchestrator_thread_id) =
        store.ensure_session_mission(&session_id, UtcMillis::now(), || {
            MissionId::new("mission-goal-objective-length")
        });
    let oversized = "目".repeat(MAX_GOAL_OBJECTIVE_CHARS + 1);
    assert!(
        store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id.clone(),
                "turn-goal-objective-length",
                &oversized,
                AccessProfile::Restricted,
            )
            .is_err()
    );
    let goal = store
        .create_goal(
            session_id.clone(),
            orchestrator_thread_id,
            "turn-goal-objective-length",
            "有效目标",
            AccessProfile::Restricted,
        )
        .expect("bounded goal should create");
    assert!(
        store
            .update_goal_objective_if_revision(
                &session_id,
                &goal.goal_id,
                oversized,
                Some(goal.control_revision),
            )
            .is_err()
    );
    assert_eq!(
        store
            .current_goal(&session_id)
            .expect("goal should remain")
            .objective,
        "有效目标"
    );
}

#[test]
fn clearing_latest_goal_does_not_reveal_older_completed_goal() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-history");
    store
        .create_session(session_id.clone(), "goal history")
        .expect("session should be created");
    let first = create_test_goal(
        &store,
        &session_id,
        "turn-goal-history-1",
        "第一个目标",
        AccessProfile::Restricted,
    );
    store
        .complete_goal(
            &session_id,
            &first.goal_id,
            GoalRevisionExpectation::new(first.control_revision, None),
            "turn-goal-history-1",
            "第一个目标已完成",
            Vec::new(),
        )
        .expect("first goal should complete");
    let second = create_test_goal(
        &store,
        &session_id,
        "turn-goal-history-2",
        "第二个目标",
        AccessProfile::Restricted,
    );
    store
        .clear_goal_with_plan(&session_id, &second.goal_id, second.control_revision, None)
        .expect("second goal should clear");

    assert!(store.current_visible_goal(&session_id).is_none());
    assert!(store.goals_for_session(&session_id).is_empty());
}

#[test]
fn completed_goal_is_terminal() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-terminal");
    store
        .create_session(session_id.clone(), "goal terminal")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-terminal",
        "终态目标",
        AccessProfile::Restricted,
    );
    let completed = store
        .complete_goal(
            &session_id,
            &goal.goal_id,
            GoalRevisionExpectation::new(goal.control_revision, None),
            "turn-goal-terminal",
            "终态目标已完成",
            Vec::new(),
        )
        .expect("goal should complete");

    let error = store
        .resume_goal_with_plan(
            &session_id,
            &goal.goal_id,
            completed.control_revision,
            None,
            None,
        )
        .expect_err("completed goal must not reopen");
    assert!(matches!(
        error,
        DomainError::GoalRejected {
            reason: GoalRejection::IllegalTransition,
            ..
        }
    ));
}

#[test]
fn blocker_requires_three_consecutive_goal_turns() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-failure-streak");
    store
        .create_session(session_id.clone(), "goal failure streak")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-failure-streak",
        "完成目标",
        AccessProfile::Restricted,
    );

    let mut current = goal;
    for expected in 1..=2 {
        let turn_id = format!("turn-{expected}");
        set_test_continuation_owner(&store, &session_id, &current.goal_id, &turn_id);
        let updated = store
            .observe_goal_blocker(
                &session_id,
                &current.goal_id,
                current.control_revision,
                None,
                &turn_id,
                "missing-input",
                "等待用户输入",
            )
            .expect("blocker should be observed");
        assert_eq!(updated.status, GoalStatus::Active);
        assert_eq!(
            updated
                .blocker
                .as_ref()
                .expect("blocker should exist")
                .consecutive_turns,
            expected
        );
        current = updated;
    }
    set_test_continuation_owner(&store, &session_id, &current.goal_id, "turn-3");
    let blocked = store
        .observe_goal_blocker(
            &session_id,
            &current.goal_id,
            current.control_revision,
            None,
            "turn-3",
            "missing-input",
            "等待用户输入",
        )
        .expect("third observation should block");
    assert_eq!(blocked.status, GoalStatus::Blocked);
    assert_eq!(
        blocked
            .blocker
            .expect("blocker should exist")
            .consecutive_turns,
        3
    );
}

#[test]
fn stale_turn_cannot_complete_or_block_goal() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-stale-terminal-update");
    store
        .create_session(session_id.clone(), "stale terminal update")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-owner",
        "只允许归属 Turn 更新终态",
        AccessProfile::Restricted,
    );

    assert!(
        store
            .observe_goal_blocker(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                None,
                "turn-stale",
                "stale-blocker",
                "旧 Turn 不应阻塞目标",
            )
            .is_err()
    );
    assert!(
        store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                GoalRevisionExpectation::new(goal.control_revision, None),
                "turn-stale",
                "旧 Turn 不应完成目标",
                Vec::new(),
            )
            .is_err()
    );
    let unchanged = store.current_goal(&session_id).expect("goal should remain");
    assert_eq!(unchanged.status, GoalStatus::Active);
    assert!(unchanged.blocker.is_none());
    assert!(unchanged.completion.is_none());

    set_test_continuation_owner(
        &store,
        &session_id,
        &goal.goal_id,
        "turn-current-continuation",
    );
    let completed = store
        .complete_goal(
            &session_id,
            &goal.goal_id,
            GoalRevisionExpectation::new(goal.control_revision, None),
            "turn-current-continuation",
            "当前续跑 Turn 完成目标",
            Vec::new(),
        )
        .expect("current continuation should own terminal update");
    assert_eq!(completed.status, GoalStatus::Complete);
}

#[test]
fn runtime_failure_blocks_immediately() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-runtime-failure");
    store
        .create_session(session_id.clone(), "goal runtime failure")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-runtime-error",
        "完成目标",
        AccessProfile::Restricted,
    );
    let blocked = store
        .stop_goal_for_runtime_failure(
            &session_id,
            &goal.goal_id,
            None,
            "turn-runtime-error",
            "provider failed",
        )
        .expect("runtime failure should stop goal");
    assert_eq!(blocked.status, GoalStatus::Blocked);
    assert_eq!(
        blocked
            .blocker
            .expect("runtime blocker should exist")
            .blocker_key,
        "runtime_error"
    );
}

#[test]
fn transient_runtime_failures_retry_before_the_goal_is_blocked() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-runtime-retry");
    store
        .create_session(session_id.clone(), "goal runtime retry")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-1",
        "完成目标",
        AccessProfile::Restricted,
    );

    for (turn, expected_attempt) in [("turn-1", 1), ("turn-2", 2)] {
        // 每一轮的失败都由「拥有目标的轮次」报告；这里模拟续跑轮次接管目标归属。
        set_test_continuation_owner(&store, &session_id, &goal.goal_id, turn);
        let (after, disposition) = store
            .observe_goal_runtime_failure(&session_id, &goal.goal_id, turn, "timeout")
            .expect("failure should be observed");
        assert_eq!(
            after.status,
            GoalStatus::Active,
            "第 {expected_attempt} 次失败不应受阻"
        );
        assert_eq!(
            disposition,
            GoalRuntimeFailureDisposition::Retrying {
                attempt: expected_attempt
            }
        );
    }
    // 同一轮重复报告不重复计数。
    set_test_continuation_owner(&store, &session_id, &goal.goal_id, "turn-2");
    let (_, again) = store
        .observe_goal_runtime_failure(&session_id, &goal.goal_id, "turn-2", "timeout")
        .expect("duplicate report");
    assert_eq!(
        again,
        GoalRuntimeFailureDisposition::Retrying { attempt: 2 }
    );

    set_test_continuation_owner(&store, &session_id, &goal.goal_id, "turn-3");
    let (blocked, disposition) = store
        .observe_goal_runtime_failure(&session_id, &goal.goal_id, "turn-3", "timeout")
        .expect("third failure");
    assert_eq!(disposition, GoalRuntimeFailureDisposition::Blocked);
    assert_eq!(blocked.status, GoalStatus::Blocked);
}

#[test]
fn stale_turn_cannot_block_replacement_goal() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-stale-accounting");
    store
        .create_session(session_id.clone(), "stale accounting")
        .expect("session should be created");
    let first = create_test_goal(
        &store,
        &session_id,
        "turn-stale-accounting-1",
        "第一个目标",
        AccessProfile::Restricted,
    );
    store
        .complete_goal(
            &session_id,
            &first.goal_id,
            GoalRevisionExpectation::new(first.control_revision, None),
            "turn-stale-accounting-1",
            "第一个目标已完成",
            Vec::new(),
        )
        .expect("first goal should complete");
    let second = create_test_goal(
        &store,
        &session_id,
        "turn-stale-accounting-2",
        "第二个目标",
        AccessProfile::Restricted,
    );

    // 旧 Goal 的结算水位不能改写已经替换的新 Goal。
    super::reconcile_goal_token_usage(&mut store.state.write().unwrap());
    let current = store
        .current_goal(&session_id)
        .expect("current goal should exist");
    assert_eq!(current.goal_id, second.goal_id);
    assert_eq!(current.tokens_used, 0);
    assert_eq!(current.time_used_seconds, 0);

    let after_stale_failure = store
        .stop_goal_for_runtime_failure(
            &session_id,
            &second.goal_id,
            None,
            "turn-stale-accounting-1",
            "旧 Turn 延迟失败",
        )
        .expect("stale failure should be ignored");
    assert_eq!(after_stale_failure.status, GoalStatus::Active);
    assert!(after_stale_failure.blocker.is_none());
}

#[test]
fn goal_completion_requires_bound_plan_to_finish() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-completion-gate");
    store
        .create_session(session_id.clone(), "completion gate")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-completion-gate",
        "完成计划",
        AccessProfile::Restricted,
    );
    let plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-goal-completion-gate"),
        session_id: session_id.clone(),
        goal_id: Some(goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Active,
        items: vec![magi_core::PlanItem::new(
            magi_core::PlanItemId::new("finish"),
            "完成实现",
            PlanItemStatus::InProgress,
        )],
        task_bindings: std::collections::HashMap::new(),
        updated_at: UtcMillis::now(),
    };
    store
        .upsert_plan(&session_id, plan.clone(), Some(0))
        .expect("plan should create");
    assert!(
        store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                GoalRevisionExpectation::new(goal.control_revision, Some(1)),
                "turn-goal-completion-gate",
                "尚未完成",
                Vec::new(),
            )
            .is_err()
    );

    let mut completed_plan = plan;
    completed_plan.items[0].status = PlanItemStatus::Completed;
    completed_plan.state = PlanState::Completed;
    store
        .upsert_plan(&session_id, completed_plan, Some(1))
        .expect("plan should complete");
    let completed = store
        .complete_goal(
            &session_id,
            &goal.goal_id,
            GoalRevisionExpectation::new(goal.control_revision, Some(2)),
            "turn-goal-completion-gate",
            "计划和验证均已完成",
            vec!["test:goal-completion-gate".to_string()],
        )
        .expect("finished plan should allow completion");
    assert_eq!(completed.status, GoalStatus::Complete);
    assert_eq!(
        completed
            .completion
            .expect("completion record should exist")
            .plan_revision,
        Some(2)
    );
}

#[test]
fn goal_completion_uses_authoritative_task_query_without_removing_historical_branches() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-owning-turn-completion");
    let owning_turn_id = "turn-goal-owning-turn-completion";
    store
        .create_session(session_id.clone(), "owning turn completion")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        owning_turn_id,
        "完成当前 Goal Turn",
        AccessProfile::Restricted,
    );
    let own_task_id = magi_core::TaskId::new(owning_turn_id);
    let other_task_id = magi_core::TaskId::new("task-other-active-binding");
    let item_id = magi_core::PlanItemId::new("completed");
    let plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-goal-owning-turn-completion"),
        session_id: session_id.clone(),
        goal_id: Some(goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Completed,
        items: vec![magi_core::PlanItem::new(
            item_id.clone(),
            "完成目标",
            PlanItemStatus::Completed,
        )],
        task_bindings: std::collections::HashMap::from([
            (own_task_id.clone(), item_id.clone()),
            (other_task_id.clone(), item_id),
        ]),
        updated_at: UtcMillis::now(),
    };
    let plan = store
        .upsert_plan(&session_id, plan.clone(), Some(0))
        .expect("completed plan should create");
    let other_branch: crate::ActiveExecutionBranch = serde_json::from_value(serde_json::json!({
        "taskId": other_task_id,
        "workerId": "worker-other-active-binding",
        "useTools": true,
        "threadId": "thread-other-active-binding"
    }))
    .unwrap();
    let active_chain = ActiveExecutionChain {
        session_id: session_id.clone(),
        mission_id: MissionId::new(format!("mission-{session_id}")),
        root_task_id: own_task_id,
        execution_chain_ref: "chain-goal-owning-turn-completion".to_string(),
        workspace_id: None,
        branches: vec![other_branch.clone()],
        dispatch_context: ActiveExecutionDispatchContext {
            accepted_at: UtcMillis::now(),
            entry_id: "timeline-goal-owning-turn-completion".to_string(),
            trimmed_text: None,
            skill_name: None,
        },
        current_turn_id: None,
    };
    store
        .seed_execution_chain_for_test(session_id.clone(), active_chain.clone())
        .expect("active execution chain should persist");

    // 未装配 TaskStore 查询时拒绝完成，不用分支成员关系猜测运行状态。
    assert!(
        store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                GoalRevisionExpectation::new(goal.control_revision, Some(plan.revision)),
                owning_turn_id,
                "missing authority",
                vec!["evidence".into()]
            )
            .is_err()
    );
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let queried_active = active.clone();
    let expected_root = active_chain.root_task_id.clone();
    let expected_session = session_id.clone();
    let expected_goal = goal.goal_id.clone();
    let expected_other = other_task_id.clone();
    store.set_goal_task_activity_resolver(std::sync::Arc::new(
        move |root, session, goal, tasks| {
            assert_eq!(root, &expected_root);
            assert_eq!(session, &expected_session);
            assert_eq!(goal, &expected_goal);
            assert!(tasks.contains(&expected_other));
            Ok(queried_active.load(std::sync::atomic::Ordering::SeqCst))
        },
    ));
    assert!(
        store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                GoalRevisionExpectation::new(goal.control_revision, Some(plan.revision)),
                owning_turn_id,
                "仍有其他任务运行",
                vec!["test:other-task-running".to_string()],
            )
            .is_err(),
        "an unrelated active task must still block Goal completion"
    );

    assert_eq!(
        store.current_goal(&session_id).unwrap().status,
        GoalStatus::Active
    );
    active.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        store
            .active_execution_chain(&session_id)
            .unwrap()
            .contains_task(&other_task_id)
    );
    let completed = store
        .complete_goal(
            &session_id,
            &goal.goal_id,
            GoalRevisionExpectation::new(goal.control_revision, Some(plan.revision)),
            owning_turn_id,
            "当前 Goal Turn 已完成计划和证据验证",
            vec!["test:owning-turn-completion".to_string()],
        )
        .expect("stale task projection must not block Goal completion");
    assert_eq!(completed.status, GoalStatus::Complete);
}

#[test]
fn goal_terminal_updates_require_current_bound_plan_revision() {
    let store = SessionStore::new();
    let completion_session_id = SessionId::new("session-goal-completion-plan-cas");
    store
        .create_session(completion_session_id.clone(), "completion plan cas")
        .expect("session should be created");
    let completion_goal = create_test_goal(
        &store,
        &completion_session_id,
        "turn-goal-completion-plan-cas",
        "验证完成状态的计划版本",
        AccessProfile::Restricted,
    );
    let completion_plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-goal-completion-plan-cas"),
        session_id: completion_session_id.clone(),
        goal_id: Some(completion_goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Completed,
        items: vec![magi_core::PlanItem::new(
            magi_core::PlanItemId::new("completed"),
            "完成验证",
            PlanItemStatus::Completed,
        )],
        task_bindings: std::collections::HashMap::new(),
        updated_at: UtcMillis::now(),
    };
    store
        .upsert_plan(&completion_session_id, completion_plan, Some(0))
        .expect("completion plan should create");

    for expected_plan_revision in [None, Some(0)] {
        assert!(
            store
                .complete_goal(
                    &completion_session_id,
                    &completion_goal.goal_id,
                    GoalRevisionExpectation::new(
                        completion_goal.control_revision,
                        expected_plan_revision
                    ),
                    "turn-goal-completion-plan-cas",
                    "旧计划快照不能完成目标",
                    vec!["test:goal-completion-plan-cas".to_string()],
                )
                .is_err()
        );
    }
    assert_eq!(
        store
            .current_goal(&completion_session_id)
            .expect("goal should remain active")
            .status,
        GoalStatus::Active
    );
    let completed = store
        .complete_goal(
            &completion_session_id,
            &completion_goal.goal_id,
            GoalRevisionExpectation::new(completion_goal.control_revision, Some(1)),
            "turn-goal-completion-plan-cas",
            "当前计划快照允许完成目标",
            vec!["test:goal-completion-plan-cas".to_string()],
        )
        .expect("current plan revision should allow completion");
    assert_eq!(completed.status, GoalStatus::Complete);

    let blocker_session_id = SessionId::new("session-goal-blocker-plan-cas");
    store
        .create_session(blocker_session_id.clone(), "blocker plan cas")
        .expect("session should be created");
    let blocker_goal = create_test_goal(
        &store,
        &blocker_session_id,
        "turn-goal-blocker-plan-cas",
        "验证阻塞状态的计划版本",
        AccessProfile::Restricted,
    );
    let blocker_plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-goal-blocker-plan-cas"),
        session_id: blocker_session_id.clone(),
        goal_id: Some(blocker_goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Active,
        items: vec![magi_core::PlanItem::new(
            magi_core::PlanItemId::new("blocked"),
            "等待外部条件",
            PlanItemStatus::InProgress,
        )],
        task_bindings: std::collections::HashMap::new(),
        updated_at: UtcMillis::now(),
    };
    store
        .upsert_plan(&blocker_session_id, blocker_plan.clone(), Some(0))
        .expect("blocker plan should create");
    let mut current = blocker_goal;
    for turn_id in ["turn-blocker-plan-cas-1", "turn-blocker-plan-cas-2"] {
        set_test_continuation_owner(&store, &blocker_session_id, &current.goal_id, turn_id);
        current = store
            .observe_goal_blocker(
                &blocker_session_id,
                &current.goal_id,
                current.control_revision,
                Some(1),
                turn_id,
                "external-state",
                "等待外部状态变化",
            )
            .expect("current plan revision should observe blocker");
    }
    let mut changed_plan = blocker_plan;
    changed_plan.items[0].title = "外部条件仍未满足".to_string();
    let changed_plan = store
        .upsert_plan(&blocker_session_id, changed_plan, Some(1))
        .expect("concurrent plan update should succeed");
    set_test_continuation_owner(
        &store,
        &blocker_session_id,
        &current.goal_id,
        "turn-blocker-plan-cas-3",
    );
    assert!(
        store
            .observe_goal_blocker(
                &blocker_session_id,
                &current.goal_id,
                current.control_revision,
                Some(1),
                "turn-blocker-plan-cas-3",
                "external-state",
                "等待外部状态变化",
            )
            .is_err()
    );
    let blocked = store
        .observe_goal_blocker(
            &blocker_session_id,
            &current.goal_id,
            current.control_revision,
            Some(changed_plan.revision),
            "turn-blocker-plan-cas-3",
            "external-state",
            "等待外部状态变化",
        )
        .expect("current plan revision should permit terminal blocker update");
    assert_eq!(blocked.status, GoalStatus::Blocked);
    let paused_plan = store
        .plan(&blocker_session_id)
        .expect("bound plan should remain");
    assert_eq!(paused_plan.state, PlanState::Paused);
    assert_eq!(paused_plan.revision, changed_plan.revision + 1);
}

#[test]
fn goal_plan_controls_reject_stale_plan_revision_atomically() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-plan-control-revision");
    store
        .create_session(session_id.clone(), "goal plan control revision")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-plan-control-revision",
        "验证 Goal 与 Plan 控制版本",
        AccessProfile::Restricted,
    );
    store
        .upsert_plan(
            &session_id,
            SessionPlan {
                plan_id: magi_core::PlanId::new("plan-goal-plan-control-revision"),
                session_id: session_id.clone(),
                goal_id: Some(goal.goal_id.clone()),
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![magi_core::PlanItem::new(
                    magi_core::PlanItemId::new("execute"),
                    "执行目标",
                    PlanItemStatus::InProgress,
                )],
                task_bindings: std::collections::HashMap::new(),
                updated_at: UtcMillis::now(),
            },
            Some(0),
        )
        .expect("plan should create");

    assert!(
        store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(0),)
            .is_err()
    );
    assert_eq!(
        store
            .current_goal(&session_id)
            .expect("goal should remain")
            .status,
        GoalStatus::Active
    );
    assert_eq!(
        store.plan(&session_id).expect("plan should remain").state,
        PlanState::Active
    );

    let (paused, paused_plan) = store
        .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(1))
        .expect("matching revisions should pause goal and plan");
    let paused_plan = paused_plan.expect("bound plan should pause");
    assert_eq!(paused.status, GoalStatus::Paused);
    assert_eq!(paused_plan.state, PlanState::Paused);

    assert!(
        store
            .resume_goal_with_plan(
                &session_id,
                &goal.goal_id,
                paused.control_revision,
                Some(1),
                None,
            )
            .is_err()
    );
    assert_eq!(
        store
            .current_goal(&session_id)
            .expect("goal should remain")
            .status,
        GoalStatus::Paused
    );
    assert!(
        store
            .clear_goal_with_plan(&session_id, &goal.goal_id, paused.control_revision, Some(1),)
            .is_err()
    );
    assert!(store.current_goal(&session_id).is_some());

    let (resumed, resumed_plan, _) = store
        .resume_goal_with_plan(
            &session_id,
            &goal.goal_id,
            paused.control_revision,
            Some(paused_plan.revision),
            None,
        )
        .expect("matching revisions should resume goal and plan");
    assert_eq!(resumed.status, GoalStatus::Active);
    assert_eq!(
        resumed_plan.expect("bound plan should resume").state,
        PlanState::Active
    );
}

#[test]
fn paused_goal_rejects_late_plan_progress_atomically() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-paused-goal-late-plan-progress");
    store
        .create_session(session_id.clone(), "paused goal late plan progress")
        .expect("session should be created");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-paused-goal-late-plan-progress",
        "验证暂停后的迟到计划更新",
        AccessProfile::Restricted,
    );
    let active_plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-paused-goal-late-plan-progress"),
        session_id: session_id.clone(),
        goal_id: Some(goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Active,
        items: vec![magi_core::PlanItem::new(
            magi_core::PlanItemId::new("execute"),
            "执行目标",
            PlanItemStatus::InProgress,
        )],
        task_bindings: std::collections::HashMap::new(),
        updated_at: UtcMillis::now(),
    };
    let active_plan = store
        .upsert_plan_for_goal_progress(
            &session_id,
            active_plan,
            Some(0),
            Some(goal.goal_id.clone()),
            Some(goal.control_revision),
        )
        .expect("active goal should accept matching plan progress");
    let (paused_goal, paused_plan) = store
        .pause_goal_with_plan(
            &session_id,
            &goal.goal_id,
            goal.control_revision,
            Some(active_plan.revision),
        )
        .expect("goal and plan should pause atomically");
    let paused_plan = paused_plan.expect("bound plan should pause");

    let mut late_active_plan = paused_plan.clone();
    late_active_plan.state = PlanState::Active;

    assert!(
        store
            .upsert_plan_for_goal_progress(
                &session_id,
                late_active_plan.clone(),
                Some(active_plan.revision),
                Some(goal.goal_id.clone()),
                Some(goal.control_revision),
            )
            .is_err(),
        "stale goal and plan revisions must be rejected"
    );
    assert!(
        store
            .upsert_plan_for_goal_progress(
                &session_id,
                late_active_plan.clone(),
                Some(paused_plan.revision),
                Some(goal.goal_id.clone()),
                Some(goal.control_revision),
            )
            .is_err(),
        "current plan revision cannot bypass a stale goal revision"
    );
    assert!(
        store
            .upsert_plan_for_goal_progress(
                &session_id,
                late_active_plan.clone(),
                Some(paused_plan.revision),
                Some(goal.goal_id.clone()),
                Some(paused_goal.control_revision),
            )
            .is_err(),
        "a current control revision cannot advance a paused goal"
    );
    assert!(
        store
            .upsert_plan(&session_id, late_active_plan, Some(paused_plan.revision))
            .is_err(),
        "internal writes cannot reactivate a non-active goal plan"
    );

    assert_eq!(
        store.current_goal(&session_id).expect("goal should remain"),
        paused_goal
    );
    assert_eq!(
        store.plan(&session_id).expect("plan should remain"),
        paused_plan
    );

    store
        .clear_goal_with_plan(
            &session_id,
            &paused_goal.goal_id,
            paused_goal.control_revision,
            Some(paused_plan.revision),
        )
        .expect("paused goal should clear with matching revisions");
    let replacement_goal = create_test_goal(
        &store,
        &session_id,
        "turn-replacement-goal",
        "替换后的新目标",
        AccessProfile::Restricted,
    );
    assert_eq!(replacement_goal.control_revision, goal.control_revision);
    let late_initial_plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-late-initial-after-goal-replacement"),
        session_id: session_id.clone(),
        goal_id: Some(goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Active,
        items: vec![magi_core::PlanItem::new(
            magi_core::PlanItemId::new("late-execute"),
            "迟到执行",
            PlanItemStatus::InProgress,
        )],
        task_bindings: std::collections::HashMap::new(),
        updated_at: UtcMillis::now(),
    };
    assert!(
        store
            .upsert_plan_for_goal_progress(
                &session_id,
                late_initial_plan,
                Some(0),
                Some(goal.goal_id),
                Some(goal.control_revision),
            )
            .is_err(),
        "matching revisions must not let an old Goal identity bind a replacement Goal"
    );
    assert!(store.plan(&session_id).is_none());
    assert_eq!(
        store
            .current_goal(&session_id)
            .expect("replacement goal should remain"),
        replacement_goal
    );
}

#[test]
fn diversion_pauses_goal_and_bound_plan_atomically() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-diversion");
    store
        .create_session(session_id.clone(), "goal diversion")
        .expect("session should create");
    let goal = create_test_goal(
        &store,
        &session_id,
        "task-goal-diversion-owner",
        "验证任务切换原子暂停",
        AccessProfile::Restricted,
    );
    let plan = SessionPlan {
        plan_id: magi_core::PlanId::new("plan-goal-diversion"),
        session_id: session_id.clone(),
        goal_id: Some(goal.goal_id.clone()),
        revision: 1,
        language: "zh-CN".to_string(),
        state: PlanState::Active,
        items: vec![magi_core::PlanItem::new(
            magi_core::PlanItemId::new("execute"),
            "执行目标",
            PlanItemStatus::InProgress,
        )],
        task_bindings: std::collections::HashMap::new(),
        updated_at: UtcMillis::now(),
    };
    store
        .upsert_plan_for_goal_progress(
            &session_id,
            plan,
            Some(0),
            Some(goal.goal_id.clone()),
            Some(goal.control_revision),
        )
        .expect("plan should create");

    let (paused_goal, paused_plan) = store
        .pause_active_goal_for_diversion(&session_id)
        .expect("diversion should succeed")
        .expect("active goal should pause");

    assert_eq!(paused_goal.status, GoalStatus::Paused);
    assert_eq!(paused_goal.continuation, GoalContinuationState::default());
    assert_eq!(
        paused_plan.expect("bound plan should pause").state,
        PlanState::Paused
    );
    assert!(
        store
            .pause_active_goal_for_diversion(&session_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn execution_owner_and_resume_rollback_are_revision_safe() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-owner-rollback");
    store
        .create_session(session_id.clone(), "goal owner rollback")
        .expect("session should create");
    let goal = create_test_goal(
        &store,
        &session_id,
        "task-goal-owner",
        "验证执行归属和恢复回滚",
        AccessProfile::Restricted,
    );
    store
        .upsert_plan_for_goal_progress(
            &session_id,
            SessionPlan {
                plan_id: magi_core::PlanId::new("plan-goal-owner"),
                session_id: session_id.clone(),
                goal_id: Some(goal.goal_id.clone()),
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![magi_core::PlanItem::new(
                    magi_core::PlanItemId::new("execute"),
                    "执行目标",
                    PlanItemStatus::InProgress,
                )],
                task_bindings: std::collections::HashMap::new(),
                updated_at: UtcMillis::now(),
            },
            Some(0),
            Some(goal.goal_id.clone()),
            Some(goal.control_revision),
        )
        .expect("goal plan should create");
    assert_eq!(
        store
            .active_goal_for_execution_owner(&session_id, "task-goal-owner")
            .map(|owned| owned.goal_id),
        Some(goal.goal_id.clone())
    );
    assert!(
        store
            .active_plan_for_execution_owner(&session_id, "task-goal-owner")
            .is_some()
    );
    assert!(
        store
            .active_goal_for_execution_owner(&session_id, "task-unrelated")
            .is_none()
    );
    assert!(
        store
            .active_plan_for_execution_owner(&session_id, "task-unrelated")
            .is_none()
    );
    let paused = store
        .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(1))
        .expect("goal should pause")
        .0;
    let (resumed, resumed_plan, checkpoint) = store
        .resume_goal_with_plan(
            &session_id,
            &goal.goal_id,
            paused.control_revision,
            Some(2),
            Some(AccessProfile::FullAccess),
        )
        .expect("resume request should succeed");
    assert_eq!(resumed.status, GoalStatus::Active);
    assert_eq!(resumed.continuation.phase, GoalContinuationPhase::Waiting);
    assert_eq!(resumed.access_profile, AccessProfile::FullAccess);
    assert_eq!(
        resumed_plan.expect("plan should resume").state,
        PlanState::Active
    );
    assert_eq!(store.waiting_goal_session_ids(), vec![session_id.clone()]);
    assert!(
        store
            .active_goal_for_execution_owner(&session_id, "task-goal-owner")
            .is_none(),
        "waiting 阶段不能继续归属于旧 Goal Turn"
    );
    assert!(
        store
            .active_plan_for_execution_owner(&session_id, "task-goal-owner")
            .is_none(),
        "waiting 阶段没有执行拥有者"
    );

    store
        .rollback_goal_resume(checkpoint)
        .expect("unchanged resume request should roll back");
    assert_eq!(
        store.current_goal(&session_id).expect("goal should remain"),
        paused
    );
    assert!(store.waiting_goal_session_ids().is_empty());
}

#[test]
fn current_goal_turn_interruption_pauses_goal_without_execution_chain() {
    let store = SessionStore::new();
    let session_id = SessionId::new("session-goal-interrupt-without-chain");
    store
        .create_session(session_id.clone(), "goal interrupt without chain")
        .expect("session should create");
    let goal = create_test_goal(
        &store,
        &session_id,
        "turn-goal-interrupt-owner",
        "验证直接 Goal Turn 中断",
        AccessProfile::Restricted,
    );
    store
        .accept_current_turn_with_timeline_entry(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-goal-interrupt-owner",
                TimelineEntryKind::UserMessage,
                "推进目标",
                UtcMillis(1),
            ),
            ActiveExecutionTurn {
                turn_id: "turn-goal-interrupt-owner".to_string(),
                turn_seq: 1,
                accepted_at: UtcMillis(1),
                status: "running".to_string(),
                completed_at: None,
                user_message: Some("推进目标".to_string()),
                items: Vec::new(),
            },
        )
        .expect("goal turn should persist");

    store
        .interrupt_current_turn_by_user(&session_id)
        .expect("interruption should succeed");
    let paused = store
        .current_goal(&session_id)
        .expect("owned goal should remain");
    assert_eq!(paused.goal_id, goal.goal_id);
    assert_eq!(paused.status, GoalStatus::Paused);
    assert_eq!(paused.continuation, GoalContinuationState::default());
}
