use super::*;
use magi_core::{
    AbsolutePath, ExecutionOwnership, MissionId, PlanId, PlanItem, PlanItemId, PlanItemStatus,
    PlanState, SessionId, TaskId, TaskKind, TaskStatus, ThreadId, UtcMillis, WorkspaceId,
};
use magi_session_store::{
    ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
    ActiveExecutionTurnItem, CanonicalTurnMutation, NotificationRecord, NotificationScope,
    SessionDurableState, SessionEventWriter, SessionPlan, ThreadContextCheckpoint,
    TimelineEntryKind,
};
use std::collections::HashMap;

#[test]
fn semantic_json_comparison_accepts_numeric_representation_changes() {
    let cached = serde_json::json!({
        "clip": {"x": 0, "width": 1, "height": 1},
        "items": [1, 2]
    });
    let replayed = serde_json::json!({
        "clip": {"x": 0.0, "width": 1.0, "height": 1.0},
        "items": [1.0, 2.0]
    });
    assert!(json_values_semantically_equal(&cached, &replayed));
    assert!(!json_values_semantically_equal(
        &cached,
        &serde_json::json!({
            "clip": {"x": 0, "width": 1, "height": 2},
            "items": [1, 2]
        })
    ));
}

#[test]
fn cached_projection_ignores_goal_derived_turn_metadata() {
    // 事件日志在目标仍活跃时记录了 goal_progress；目标之后阻塞于该 Turn，
    // 恢复推导会去掉该标注。两者不能被判成“事实被改写”。
    let turn = |scope: Option<&str>| {
        let mut turn: magi_session_store::CanonicalTurn =
            serde_json::from_value(serde_json::json!({
                "sessionId": "s", "turnId": "t", "turnSeq": 1, "acceptedAt": 1,
                "completedAt": 2, "status": "failed", "responseDurationMs": 1,
                "items": [], "metadata": {"goalId": "g"}
            }))
            .expect("turn fixture should deserialize");
        if let Some(scope) = scope {
            turn.metadata.insert(
                "responseDurationScope".to_string(),
                serde_json::Value::String(scope.to_string()),
            );
        }
        turn
    };
    let with_scope = StateRepository::event_fact_turns(&[turn(Some("goal_progress"))]).unwrap();
    let without_scope = StateRepository::event_fact_turns(&[turn(None)]).unwrap();
    assert!(json_values_semantically_equal(&with_scope, &without_scope));

    let mut changed = turn(None);
    changed.status = magi_session_store::CanonicalTurnStatus::Completed;
    let changed = StateRepository::event_fact_turns(&[changed]).unwrap();
    assert!(!json_values_semantically_equal(&without_scope, &changed));
}

fn accepted_session_store(
    session_id: &str,
    workspace_id: Option<&str>,
    accepted_at: u64,
) -> (SessionStore, String, TaskId) {
    let session_store = SessionStore::new();
    let session_id = SessionId::new(session_id);
    session_store
        .create_session_for_workspace(
            session_id.clone(),
            "accepted journal test",
            workspace_id.map(str::to_string),
        )
        .expect("accepted journal test session should be created");
    let turn_id = format!("turn-{session_id}-{accepted_at}");
    let entry_id = format!("timeline-{session_id}-{accepted_at}");
    let task_id = TaskId::new(format!("task-{session_id}-{accepted_at}"));
    let chain = ActiveExecutionChain {
        session_id: session_id.clone(),
        mission_id: MissionId::new(format!("mission-{session_id}")),
        root_task_id: task_id.clone(),
        execution_chain_ref: format!("chain-{session_id}-{accepted_at}"),
        workspace_id: workspace_id.map(WorkspaceId::new),
        branches: Vec::new(),
        dispatch_context: ActiveExecutionDispatchContext {
            accepted_at: UtcMillis(accepted_at),
            entry_id: entry_id.clone(),
            trimmed_text: Some("test accepted journal".to_string()),
            skill_name: None,
        },
        current_turn_id: Some(turn_id.clone()),
    };
    session_store
        .accept_active_execution_chain_with_timeline_entry(
            session_id,
            magi_session_store::TimelineEntryInput::new(
                entry_id,
                TimelineEntryKind::UserMessage,
                "test accepted journal",
                UtcMillis(accepted_at),
            ),
            chain,
            ActiveExecutionTurn {
                turn_id: turn_id.clone(),
                turn_seq: accepted_at,
                accepted_at: UtcMillis(accepted_at),
                completed_at: None,
                status: "accepted".to_string(),
                user_message: Some("test accepted journal".to_string()),
                items: Vec::new(),
            },
        )
        .expect("accepted journal test turn should be accepted");
    (session_store, turn_id, task_id)
}

/// 测试夹具：把内存 SessionStore 已有的 canonical turn 写成事件日志。
fn initialize_test_session_events(repository: &StateRepository, durable: &SessionDurableState) {
    let mut turns_by_session = HashMap::<SessionId, Vec<_>>::new();
    for turn in &durable.canonical_turns {
        turns_by_session
            .entry(turn.session_id.clone())
            .or_default()
            .push(turn.clone());
    }
    for (session_id, turns) in turns_by_session {
        let mutations = StateRepository::initial_canonical_mutations(turns);
        repository
            .append_session_transaction(&session_id, &mutations, &[], None)
            .expect("test canonical events should initialize");
    }
}

fn install_test_event_authority(repository: &StateRepository, session_store: &SessionStore) {
    initialize_test_session_events(repository, &session_store.durable_state());
    session_store.install_session_event_writer(Arc::new(repository.clone()));
}

fn accepted_task(task_id: TaskId, accepted_at: u64) -> Task {
    let mission_id = MissionId::new(format!("mission-{task_id}"));
    Task {
        task_id: task_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: task_id,
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: "accepted journal task".to_string(),
        goal: "accepted journal task".to_string(),
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
        runtime_payload: magi_core::TaskRuntimePayload::default(),
        created_at: UtcMillis(accepted_at),
        updated_at: UtcMillis(accepted_at),
    }
}

#[test]
fn discarding_an_unavailable_session_removes_only_its_files() {
    let state_root = unique_temp_dir("magi-discard-unavailable-session");
    let repository = StateRepository::new(state_root.clone());
    let store = SessionStore::new();
    let healthy_id = SessionId::new("session-healthy");
    let corrupted_id = SessionId::new("session-corrupted");
    for (id, title) in [(&healthy_id, "healthy"), (&corrupted_id, "corrupted")] {
        store
            .create_session(id.clone(), title)
            .expect("session should create");
    }
    store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect("projections should save");
    let corrupted_path = repository
        .session_projection_root()
        .join(StateRepository::session_projection_file_name(&corrupted_id));
    fs::write(&corrupted_path, b"{\"version\":2,<<<<<<< HEAD").expect("corrupt projection");
    let event_root = repository.session_event_root(&corrupted_id);
    fs::create_dir_all(&event_root).expect("event root");
    fs::write(
        event_root.join("00000000000000000001-00000000000000000001.json"),
        b"{}",
    )
    .expect("event file");

    let restarted = StateRepository::new(state_root.clone());
    restarted
        .load_session_projections(&[])
        .expect("单个会话损坏不能阻止其余会话启动");
    assert!(
        restarted
            .discard_unavailable_session(&SessionId::new("session-healthy"))
            .is_err(),
        "只有记录在案的不可用会话可以被清理"
    );

    let discarded = restarted
        .discard_unavailable_session(&corrupted_id)
        .expect("unavailable session should be discarded");
    assert_eq!(discarded.session_id, corrupted_id);
    assert!(!corrupted_path.exists(), "投影文件必须被删除");
    assert!(!event_root.exists(), "事件目录必须被删除");
    assert!(restarted.unavailable_sessions().is_empty());
    assert!(
        repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&healthy_id))
            .exists(),
        "健康会话文件不能被牵连"
    );

    let reloaded = StateRepository::new(state_root.clone());
    let (restored, _) = reloaded
        .load_session_projections(&[])
        .expect("清理后应能正常启动");
    assert_eq!(restored.sessions.len(), 1);
    assert!(reloaded.unavailable_sessions().is_empty());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn corrupted_session_projection_only_makes_that_session_unavailable() {
    let state_root = unique_temp_dir("magi-session-projection-corrupted");
    let repository = StateRepository::new(state_root.clone());
    let store = SessionStore::new();
    let healthy_id = SessionId::new("session-healthy");
    let corrupted_id = SessionId::new("session-corrupted");
    store
        .create_session(healthy_id.clone(), "healthy")
        .expect("healthy session should create");
    store
        .create_session(corrupted_id.clone(), "corrupted")
        .expect("corrupted session should create");
    store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect("projections should save");
    // 模拟合并冲突或写入截断留下的损坏文件。
    let corrupted_path = repository
        .session_projection_root()
        .join(StateRepository::session_projection_file_name(&corrupted_id));
    fs::write(&corrupted_path, b"{\"version\":2,<<<<<<< HEAD").expect("corrupt projection");

    let restarted = StateRepository::new(state_root.clone());
    let (restored, _) = restarted
        .load_session_projections(&[])
        .expect("单个会话损坏不能阻止其余会话启动");

    assert_eq!(
        restored
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>(),
        vec![healthy_id]
    );
    let unavailable = restarted.unavailable_sessions();
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0].session_id, corrupted_id);
    assert_eq!(unavailable[0].workspace_id, None);
    assert_eq!(
        unavailable[0].source_path,
        corrupted_path.display().to_string()
    );
    assert!(
        unavailable[0]
            .reason
            .contains("解析 session projection 失败")
    );
    assert!(
        restored.current_session_id.as_ref() != Some(&corrupted_id),
        "current 不能指向不可用会话"
    );
    restarted
        .validate_session_event_log_coverage(&restored)
        .expect("不可用会话不影响事件日志校验");
    assert_eq!(
        fs::read(&corrupted_path).expect("损坏文件必须原地保留"),
        b"{\"version\":2,<<<<<<< HEAD"
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn corrupted_durable_state_is_rejected_without_empty_state_recovery() {
    let state_root = unique_temp_dir("magi-durable-state-corrupted");
    let repository = StateRepository::new(state_root.clone());
    let path = state_root.join("workspaces.json");
    fs::write(&path, b"{not-json").expect("corrupted durable state should write");

    let error = repository
        .load_workspace_durable_state()
        .expect_err("corrupted durable state must reject recovery");
    assert!(error.to_string().contains("拒绝以空状态继续"));
    assert!(path.exists(), "损坏状态必须原地保留以便诊断和恢复");
    assert!(!path.with_file_name("workspaces.json.stale").exists());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn duplicate_session_projection_does_not_block_recovery_or_delete_either_copy() {
    let state_root = unique_temp_dir("magi-session-projection-duplicate");
    let duplicate_root = unique_temp_dir("magi-session-projection-duplicate-root");
    let repository = StateRepository::new(state_root.clone());
    let workspace_store = WorkspaceStore::new();
    workspace_store
        .register(
            WorkspaceId::new("duplicate-root"),
            AbsolutePath::new(duplicate_root.to_string_lossy().to_string()),
        )
        .expect("duplicate workspace should register");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("duplicate workspace state should save");
    let session_store = SessionStore::new();
    let session_id = SessionId::new("session-projection-duplicate");
    session_store
        .create_session_for_workspace(
            session_id.clone(),
            "duplicate projection",
            Some("duplicate-root".to_string()),
        )
        .expect("session should create");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("initial projection should save");

    let file_name = StateRepository::session_projection_file_name(&session_id);
    let canonical_path = repository
        .project_storage_root("duplicate-root")
        .join(".magi")
        .join("session-projections")
        .join(&file_name);
    let duplicate_path = repository.session_projection_root().join(&file_name);
    fs::create_dir_all(repository.session_projection_root())
        .expect("duplicate projection directory should create");
    fs::copy(&canonical_path, &duplicate_path).expect("matching duplicate should copy");
    let canonical_content = fs::read(&canonical_path).expect("canonical projection bytes");
    let duplicate_content = fs::read(&duplicate_path).expect("duplicate projection bytes");

    // 恢复时多余副本只让它自己被忽略：会话从规范副本恢复，两份文件都原样保留。
    let (restored, _) = repository
        .load_session_projections(&[("duplicate-root".to_string(), duplicate_root.clone())])
        .expect("duplicate projection must not block recovery");
    assert!(
        restored
            .sessions
            .iter()
            .any(|session| session.session_id == session_id)
    );
    assert!(repository.unavailable_sessions().is_empty());
    assert_eq!(
        fs::read(&canonical_path).expect("canonical projection remains"),
        canonical_content
    );
    assert_eq!(
        fs::read(&duplicate_path).expect("duplicate projection remains"),
        duplicate_content
    );

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(duplicate_root);
}

#[test]
fn changing_session_workspace_never_moves_projection_implicitly() {
    let state_root = unique_temp_dir("magi-session-projection-move");
    let workspace_root = unique_temp_dir("magi-session-projection-move-workspace");
    let repository = StateRepository::new(state_root.clone());
    let session_store = SessionStore::new();
    let session_id = SessionId::new("session-projection-move");
    session_store
        .create_session(session_id.clone(), "moving projection")
        .expect("session should create");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("personal projection should save");
    let file_name = StateRepository::session_projection_file_name(&session_id);
    let old_path = repository.session_projection_root().join(&file_name);
    assert!(old_path.exists());

    let workspace_id = WorkspaceId::new("workspace-projection-move");
    let workspace_store = WorkspaceStore::new();
    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace registry should save");
    session_store.bind_execution_ownership_for_test(
        session_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            ..magi_core::ExecutionOwnership::default()
        },
    );
    let error = session_store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect_err("workspace ownership change must use an explicit migration transaction");
    assert!(error.to_string().contains("归属路径发生变化"));

    let new_path = workspace_root
        .join(".magi")
        .join("session-projections")
        .join(file_name);
    assert!(old_path.exists(), "原 projection 必须保留");
    assert!(!new_path.exists(), "失败事务不能写入新 projection");

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(workspace_root);
}

#[test]
fn partial_session_projection_checkpoint_only_rewrites_target_session() {
    let state_root = unique_temp_dir("magi-session-projection-partial");
    let repository = StateRepository::new(state_root.clone());
    let session_store = SessionStore::new();
    let first = SessionId::new("session-partial-first");
    let second = SessionId::new("session-partial-second");
    session_store
        .create_session(first.clone(), "partial first")
        .expect("first session should create");
    session_store
        .create_session(second.clone(), "partial second")
        .expect("second session should create");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("initial full projection should save");

    let first_path = state_root
        .join("session-projections")
        .join(StateRepository::session_projection_file_name(&first));
    let second_path = state_root
        .join("session-projections")
        .join(StateRepository::session_projection_file_name(&second));
    let second_before = fs::read(&second_path).expect("second projection should exist");
    session_store
        .select_current_session(&first)
        .expect("first session should be selected before checkpoint snapshot");
    session_store.append_timeline_entry(
        first.clone(),
        TimelineEntryKind::AssistantMessage,
        "partial update",
    );
    // 模拟后台 checkpoint 在释放 SessionStore 快照锁后才开始写盘：此时它携带
    // 的 current 指针仍是 first，但用户已经切换到了 second 并完成了导航提交。
    let stale = session_store.durable_state();
    session_store
        .select_current_session(&second)
        .expect("user navigation should update current session");
    repository
        .save_session_navigation_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
            Some(&second),
        )
        .expect("new current session should persist before stale checkpoint");
    let sidecars = session_store.execution_sidecar_store_state();
    repository
        .save_session_projection_partial_state_with_roots(
            &stale,
            &sidecars,
            std::slice::from_ref(&first),
            &HashMap::new(),
        )
        .expect("partial projection should save");

    let first_after = fs::read_to_string(&first_path).expect("first projection should exist");
    assert!(first_after.contains("partial update"));
    assert_eq!(
        fs::read(&second_path).expect("second projection should remain"),
        second_before,
        "未变化 session 的 projection 不应被重写"
    );
    let current: Option<SessionId> = serde_json::from_str(
        &fs::read_to_string(state_root.join("session-current.json"))
            .expect("current pointer should remain readable"),
    )
    .expect("current pointer should parse");
    assert_eq!(
        current,
        Some(second),
        "旧的增量 checkpoint 不得覆盖用户刚提交的 current session"
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn session_current_never_points_to_a_deleted_projection_after_checkpoint() {
    let state_root = unique_temp_dir("magi-session-current-delete-order");
    let repository = StateRepository::new(state_root.clone());
    let store = SessionStore::new();
    let retained_id = SessionId::new("session-current-retained");
    let deleted_id = SessionId::new("session-current-deleted");
    store
        .create_session(retained_id.clone(), "retained")
        .expect("retained session should create");
    store
        .create_session(deleted_id.clone(), "deleted")
        .expect("deleted session should create");
    store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect("initial projection should save");
    store
        .delete_session(&deleted_id)
        .expect("current session should delete");
    store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect("deletion checkpoint should save");

    let current: Option<SessionId> = serde_json::from_str(
        &fs::read_to_string(state_root.join("session-current.json"))
            .expect("current pointer should read"),
    )
    .expect("current pointer should parse");
    assert_eq!(current, Some(retained_id));
    assert!(
        !repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&deleted_id))
            .exists()
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn background_checkpoint_cannot_overwrite_a_session_rename_or_deletion() {
    use std::{sync::mpsc, thread, time::Duration};

    for delete in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let repository = StateRepository::new(root.path().to_path_buf());
        let (store, turn_id, _) = accepted_session_store("checkpoint-race", None, 45);
        let session = SessionId::new("checkpoint-race");
        install_test_event_authority(&repository, &store);
        store
            .persist_projection_with(|durable, sidecars| {
                repository.save_session_projection_state(durable, sidecars)
            })
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let checkpoint_store = store.clone();
        let checkpoint_repository = repository.clone();
        let checkpoint = thread::spawn(move || {
            checkpoint_store
                .flush_execution_sidecar_snapshot_with(|durable, sidecars, ids| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    checkpoint_repository.save_session_projection_partial_state_with_roots(
                        durable,
                        sidecars,
                        ids,
                        &HashMap::new(),
                    )?;
                    Ok::<_, DaemonError>(ids.to_vec())
                })
                .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // checkpoint 的磁盘 IO 不得持有 canonical 或 state 锁：新工具结果仍可
        // 提交事件，投影 builder 会从日志补齐它，而不是覆盖成旧快照。
        append_test_tool_item(&store, &session, &turn_id, "tool-during-checkpoint", 2);
        assert_eq!(store.canonical_turns_for_session(&session).len(), 1);
        let (finished_tx, finished_rx) = mpsc::channel();
        let mutation_store = store.clone();
        let mutation_session = session.clone();
        let mutation_repository = repository.clone();
        let mutation = thread::spawn(move || {
            let persist = |durable: &SessionDurableState,
                           sidecars: &SessionExecutionSidecarStoreState| {
                mutation_repository.save_session_projection_state_for_sessions(
                    durable,
                    sidecars,
                    std::slice::from_ref(&mutation_session),
                )
            };
            if delete {
                mutation_store
                    .delete_session_with_persistence(&mutation_session, None, persist)
                    .unwrap();
            } else {
                mutation_store
                    .rename_session_with_persistence(&mutation_session, "renamed", persist)
                    .unwrap();
            }
            finished_tx.send(()).unwrap();
        });
        let overtook_checkpoint = finished_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        release_tx.send(()).unwrap();
        assert!(checkpoint.join().unwrap());
        mutation.join().unwrap();

        let (restored, _) = StateRepository::new(root.path().to_path_buf())
            .load_session_projections(&[])
            .unwrap();
        if delete {
            assert!(
                restored.sessions.is_empty(),
                "旧 checkpoint 不得恢复已删除会话"
            );
            assert!(!repository.session_event_root(&session).exists());
        } else {
            assert_eq!(
                restored.sessions[0].title, "renamed",
                "旧 checkpoint 不得撤销重命名"
            );
            assert!(
                restored.canonical_turns[0]
                    .items
                    .iter()
                    .any(|item| { item.item_id == "tool-during-checkpoint" })
            );
        }
        assert!(!overtook_checkpoint, "会话投影事务必须按快照捕获顺序提交");
    }
}

#[test]
fn session_deletion_removes_projection_and_event_log_in_one_checkpoint() {
    let state_root = unique_temp_dir("magi-session-delete-events");
    let repository = StateRepository::new(state_root.clone());
    let (store, _, _) = accepted_session_store("session-delete-events", None, 45);
    let session_id = SessionId::new("session-delete-events");
    install_test_event_authority(&repository, &store);
    store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect("session with event log should persist");
    assert!(repository.session_event_root(&session_id).exists());

    store
        .delete_session(&session_id)
        .expect("session should delete");
    store
        .persist_projection_with(|durable, sidecars| {
            repository.save_session_projection_state(durable, sidecars)
        })
        .expect("session deletion should persist");

    assert!(
        !repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id))
            .exists()
    );
    assert!(!repository.session_event_root(&session_id).exists());
    let (restored, _) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .expect("deleted session state should remain valid");
    assert!(restored.sessions.is_empty());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn session_navigation_only_writes_pointer_and_missing_target_projection() {
    let state_root = unique_temp_dir("magi-session-navigation-targeted");
    let repository = StateRepository::new(state_root.clone());
    let store = SessionStore::new();
    let first_session_id = SessionId::new("session-navigation-first");
    let second_session_id = SessionId::new("session-navigation-second");
    store
        .create_session(first_session_id.clone(), "first")
        .expect("first session should create");
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .expect("first session should persist");
    let first_projection_path =
        state_root
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(
                &first_session_id,
            ));
    let first_projection_before =
        fs::read(&first_projection_path).expect("first projection should exist");

    store
        .create_session(second_session_id.clone(), "second")
        .expect("second session should create");
    repository
        .save_session_navigation_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
            Some(&second_session_id),
        )
        .expect("new target navigation should persist");
    let second_projection_path =
        state_root
            .join("session-projections")
            .join(StateRepository::session_projection_file_name(
                &second_session_id,
            ));
    assert!(second_projection_path.exists());
    let second_projection_before =
        fs::read(&second_projection_path).expect("second projection should exist");
    assert_eq!(
        fs::read(&first_projection_path).expect("first projection should remain readable"),
        first_projection_before
    );

    store
        .select_current_session(&first_session_id)
        .expect("first session should select");
    repository
        .save_session_navigation_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
            Some(&first_session_id),
        )
        .expect("existing target navigation should persist");
    assert_eq!(
        fs::read(&second_projection_path).expect("second projection should remain readable"),
        second_projection_before
    );

    store.clear_current_session();
    repository
        .save_session_navigation_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
            None,
        )
        .expect("draft navigation should persist");
    let current: Option<SessionId> = serde_json::from_str(
        &fs::read_to_string(state_root.join("session-current.json"))
            .expect("current pointer should exist"),
    )
    .expect("current pointer should parse");
    assert_eq!(current, None);
    assert_eq!(
        fs::read(&first_projection_path).expect("first projection should remain stable"),
        first_projection_before
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn dangling_session_current_pointer_rejects_recovery() {
    let state_root = unique_temp_dir("magi-session-current-dangling");
    let repository = StateRepository::new(state_root.clone());
    repository
        .write_json_atomically(
            state_root.join("session-current.json"),
            &Some(SessionId::new("session-missing")),
        )
        .expect("dangling current pointer should write");

    let error = repository
        .load_session_projections(&[])
        .expect_err("dangling current pointer must reject recovery");
    assert!(error.to_string().contains("指向不存在的 session"));

    let _ = fs::remove_dir_all(state_root);
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    fs::create_dir_all(&path).expect("temp dir should create");
    path
}

#[test]
fn removed_workspace_does_not_block_other_session_checkpoints_and_readd_flushes_pending() {
    let root = unique_temp_dir("magi-removed-workspace-checkpoint");
    let repository = StateRepository::new(root.join("state"));
    let workspace_root = root.join("workspace");
    fs::create_dir_all(&workspace_root).unwrap();
    let workspace = WorkspaceId::new("removed-workspace");
    let workspaces = Arc::new(WorkspaceStore::new());
    workspaces
        .register(
            workspace.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy()),
        )
        .unwrap();
    repository
        .save_workspace_durable_state(&workspaces.durable_state())
        .unwrap();
    let store = Arc::new(SessionStore::new());
    let offline = SessionId::new("checkpoint-offline");
    let healthy = SessionId::new("checkpoint-healthy");
    store
        .create_session_for_workspace(offline.clone(), "offline", Some(workspace.to_string()))
        .unwrap();
    store.create_session(healthy.clone(), "healthy").unwrap();
    for id in [&offline, &healthy] {
        store.bind_execution_ownership_for_test(id.clone(), ExecutionOwnership::default());
    }
    let persistence =
        RuntimeSidecarPersistence::new(repository.clone(), store.clone(), workspaces.clone());
    assert!(
        persistence
            .flush_runtime_sidecars()
            .unwrap()
            .session_sidecars_flushed
    );
    let offline_path = repository
        .project_storage_root(workspace.as_str())
        .join(".magi/session-projections")
        .join(StateRepository::session_projection_file_name(&offline));
    let healthy_path = repository
        .session_projection_root()
        .join(StateRepository::session_projection_file_name(&healthy));
    let old_offline = fs::read(&offline_path).unwrap();
    let old_healthy = fs::read(&healthy_path).unwrap();
    workspaces.deregister(&workspace).unwrap();
    repository
        .save_workspace_durable_state(&workspaces.durable_state())
        .unwrap();
    for id in [&offline, &healthy] {
        store
            .set_recovery_id(id, None, Some(format!("recovery-{id}")))
            .unwrap();
    }
    // 原来的全批提交会在离线工作区失败，健康会话的文件也无法更新。
    assert!(
        store
            .persist_projection_with(
                |durable, sidecars| repository.save_session_projection_state(durable, sidecars)
            )
            .is_err()
    );
    assert!(
        persistence
            .flush_runtime_sidecars()
            .unwrap()
            .session_sidecars_flushed
    );
    assert_eq!(fs::read(&offline_path).unwrap(), old_offline);
    assert_ne!(fs::read(&healthy_path).unwrap(), old_healthy);
    assert_eq!(
        store.pending_execution_sidecar_session_ids(),
        vec![offline.clone()]
    );
    assert!(
        !persistence
            .flush_runtime_sidecars()
            .unwrap()
            .session_sidecars_flushed
    );
    assert!(
        !repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&offline))
            .exists()
    );
    workspaces
        .register(
            workspace.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy()),
        )
        .unwrap();
    repository
        .save_workspace_durable_state(&workspaces.durable_state())
        .unwrap();
    assert!(
        persistence
            .flush_runtime_sidecars()
            .unwrap()
            .session_sidecars_flushed
    );
    assert!(store.pending_execution_sidecar_session_ids().is_empty());
    assert_ne!(fs::read(&offline_path).unwrap(), old_offline);
    let restarted = StateRepository::new(root.join("state"));
    let workspace_roots = workspaces
        .workspaces()
        .into_iter()
        .map(|workspace| {
            (
                workspace.workspace_id.to_string(),
                workspace.native_root_path(),
            )
        })
        .collect::<Vec<_>>();
    let (durable, sidecars) = restarted
        .load_session_projections(&workspace_roots)
        .unwrap();
    let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
    assert_eq!(
        serde_json::to_value(restored.runtime_sidecar(&offline)).unwrap(),
        serde_json::to_value(store.runtime_sidecar(&offline)).unwrap()
    );
    assert_eq!(
        serde_json::to_value(restored.runtime_sidecar(&healthy)).unwrap(),
        serde_json::to_value(store.runtime_sidecar(&healthy)).unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

fn session_incident(
    notification_id: &str,
    workspace_id: &str,
    session_id: &SessionId,
    message: &str,
    created_at: UtcMillis,
) -> NotificationRecord {
    NotificationRecord {
        notification_id: notification_id.to_string(),
        scope: NotificationScope::Session,
        workspace_id: Some(workspace_id.to_string()),
        session_id: Some(session_id.clone()),
        kind: "incident".to_string(),
        level: Some("error".to_string()),
        title: None,
        message: message.to_string(),
        detail: None,
        error_code: None,
        failure_stage: None,
        task_id: None,
        request_id: None,
        source: Some("test".to_string()),
        created_at,
        handled: false,
        action_required: true,
        count_unread: true,
        fingerprint: notification_id.to_string(),
        occurrence_count: 1,
        resolved: false,
    }
}

#[test]
fn session_projection_restores_canonical_history_and_checkpoint() {
    let state_root = unique_temp_dir("magi-persistence-state");
    let workspace_root = unique_temp_dir("magi-persistence-workspace");
    let repository = StateRepository::new(state_root.clone());
    let workspace_store = WorkspaceStore::new();
    workspace_store
        .register(
            WorkspaceId::new("workspace-persisted"),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("workspace registry should persist");
    let session_id = SessionId::new("session-persisted-timeline");
    let now = UtcMillis::now();
    let (store, turn_id, _) =
        accepted_session_store(session_id.as_str(), Some("workspace-persisted"), now.0);
    install_test_event_authority(&repository, &store);
    let (_, thread_id) =
        store.ensure_session_mission(&session_id, now, || MissionId::new("mission-persisted"));
    store.record_thread_context_window_tokens(&thread_id, 32_000, now);
    let raw_result = serde_json::json!({
        "tool": "shell_exec", "status": "failed", "error": "persisted tool error",
    })
    .to_string();
    store
        .upsert_current_turn_item_for_turn(
            &session_id,
            Some(&turn_id),
            serde_json::from_value(serde_json::json!({
                "itemId": "tool-persisted", "itemSeq": 2,
                "kind": "tool_call_result", "status": "failed", "source": "tool",
                "sourceThreadId": thread_id, "toolCallId": "call-persisted",
                "toolName": "shell_exec", "toolArguments": "{}",
                "toolStatus": "failed", "toolResult": raw_result,
            }))
            .unwrap(),
        )
        .unwrap();
    let snapshot = store.thread_context_snapshot(&thread_id).unwrap();
    assert!(snapshot.messages().iter().any(|message| {
        message.role == "tool" && message.content.as_deref() == Some(raw_result.as_str())
    }));
    let checkpoint = ThreadContextCheckpoint {
        thread_id: thread_id.clone(),
        checkpoint_id: "checkpoint-persisted".into(),
        source_message_count: snapshot.messages().len(),
        summary_message: serde_json::from_value(serde_json::json!({
            "role": "user", "content": "[context_compaction] persisted summary",
        }))
        .unwrap(),
        reason: "context_window_pressure".into(),
        original_token_estimate: 32_000,
        checkpoint_token_estimate: 4_000,
        created_at: now,
        generation: 1,
        source_fingerprint: magi_session_store::thread_history_fingerprint(snapshot.messages()),
        tool_result_projection_fingerprint: Some("tool-projection-v1".into()),
        model_provider: Some("test-provider".into()),
        model: Some("test-model".into()),
        binding_revision: Some(1),
        projected_request_tokens: 4_000,
        context_window_limit_tokens: Some(32_000),
        preserved_tail_message_count: 0,
        file_fact_versions: vec![],
    };
    assert!(store.install_thread_context_checkpoint_if_current(&snapshot, checkpoint.clone(), now));
    let app_incident = NotificationRecord {
        notification_id: "notification-persisted-app".to_string(),
        scope: NotificationScope::App,
        workspace_id: None,
        session_id: None,
        kind: "incident".to_string(),
        level: Some("error".to_string()),
        title: None,
        message: "恢复后的应用异常".to_string(),
        detail: None,
        error_code: None,
        failure_stage: None,
        task_id: None,
        request_id: None,
        source: Some("test".to_string()),
        created_at: now,
        handled: false,
        action_required: true,
        count_unread: true,
        fingerprint: "notification-persisted-app".to_string(),
        occurrence_count: 1,
        resolved: false,
    };
    let mut workspace_state = store.durable_state();
    workspace_state.notifications = vec![
        app_incident,
        session_incident(
            "notification-persisted",
            "workspace-persisted",
            &session_id,
            "恢复后的异常",
            now,
        ),
    ];
    workspace_state.plans = vec![SessionPlan {
        plan_id: PlanId::new("plan-persisted"),
        session_id: session_id.clone(),
        goal_id: None,
        revision: 1,
        language: "zh-CN".into(),
        state: PlanState::Active,
        items: vec![PlanItem::new(
            PlanItemId::new("restore-plan"),
            "恢复目标任务清单",
            PlanItemStatus::InProgress,
        )],
        task_bindings: HashMap::new(),
        updated_at: now,
    }];
    repository
        .save_session_projection_state(&workspace_state, &store.execution_sidecar_store_state())
        .expect("workspace session state should save");
    drop(store);
    drop(repository);

    let restarted = StateRepository::new(state_root.clone());
    let (merged, sidecars) = restarted
        .load_session_projections(&[("workspace-persisted".into(), workspace_root.clone())])
        .expect("workspace session state should load");
    assert_eq!(merged.sessions.len(), 1);
    assert!(merged.timeline.iter().any(|entry| {
        matches!(entry.kind, TimelineEntryKind::UserMessage)
            && entry.message == "test accepted journal"
    }));
    assert_eq!(merged.notifications.len(), 2);
    assert!(state_root.join("session-app-meta.json").exists());
    assert_eq!(merged.plans.len(), 1);
    assert_eq!(merged.plans[0].items[0].title, "恢复目标任务清单");
    assert_eq!(merged.thread_registry.len(), 1);
    assert_eq!(merged.thread_registry[0].thread_id, thread_id);
    assert_eq!(
        merged.thread_registry[0].observed_context_window_tokens,
        Some(32_000)
    );
    assert_eq!(merged.thread_context_checkpoints, vec![checkpoint.clone()]);
    assert_eq!(merged.current_session_id, Some(session_id));
    let restored = Arc::new(SessionStore::from_persisted_parts(merged, sidecars).unwrap());
    let restored_snapshot = restored.thread_context_snapshot(&thread_id).unwrap();
    assert_eq!(restored_snapshot.messages(), snapshot.messages());
    assert_eq!(
        restored
            .thread_context_snapshot(&thread_id)
            .unwrap()
            .messages()
            .to_vec(),
        snapshot.messages()
    );
    assert_eq!(restored_snapshot.checkpoint(), Some(&checkpoint));
    assert_eq!(
        restored_snapshot.model_history(),
        vec![checkpoint.summary_message]
    );
    let path = StateRepository::new(state_root.clone())
        .session_projection_path(
            &restored.durable_state().sessions[0].session_id,
            Some("workspace-persisted"),
            &HashMap::from([("workspace-persisted".into(), workspace_root.clone())]),
        )
        .unwrap();
    let encoded: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(encoded["durable"]["thread_registry"][0].is_object());
    assert!(
        encoded["durable"]["thread_registry"][0]
            .get("messageHistory")
            .is_none(),
        "落盘只保存 canonical 原文和摘要，不保存重复的线程历史"
    );

    fs::remove_dir_all(state_root).unwrap();
    fs::remove_dir_all(workspace_root).unwrap();
}

#[test]
fn input_only_transaction_restores_from_events_over_a_stale_session_projection() {
    let root = unique_temp_dir("magi-input-event-restore");
    let repository = StateRepository::new(root.clone());
    repository.verify_state_layout(&[]).unwrap();
    let store = SessionStore::new();
    let session = SessionId::new("session-pending-input-restore");
    store.create_session(session.clone(), "pending").unwrap();
    store.install_session_event_writer(Arc::new(repository.clone()));
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .unwrap();
    let input = magi_session_store::SessionInput {
        input_id: "input-restore".into(),
        session_id: session.clone(),
        request_id: "request-restore".into(),
        user_message_id: "user-restore".into(),
        request_fingerprint: "fingerprint-restore".into(),
        accepted_at: UtcMillis(50),
        revision: 1,
        state: magi_session_store::SessionInputState::Pending,
        attached_turn_id: None,
        consumption: None,
        interruption: None,
        payload: serde_json::json!({"request":"frozen"}),
    };
    store.commit_input(input.clone(), None).unwrap();
    // 只保留写入前的 Session projection，销毁内存并从新 repository 重放。
    let restarted = StateRepository::new(root.clone());
    let (restored, _) = restarted.load_session_projections(&[]).unwrap();
    assert_eq!(restored.inputs, vec![input]);
    assert!(restored.canonical_turns.is_empty());
    assert!(restarted.unavailable_sessions().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn replacement_barrier_and_settlement_replay_from_canonical_log_without_checkpoint() {
    let root = unique_temp_dir("magi-replacement-barrier-replay");
    let repository = StateRepository::new(root.clone());
    repository.verify_state_layout(&[]).unwrap();
    let (store, turn, task) = accepted_session_store("replacement-replay", None, 10);
    let session = SessionId::new("replacement-replay");
    install_test_event_authority(&repository, &store);
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .unwrap();
    let pending = magi_session_store::SessionInput {
        input_id: "replacement-replay-input".into(),
        session_id: session.clone(),
        request_id: "replacement-replay-request".into(),
        user_message_id: "replacement-replay-user".into(),
        request_fingerprint: "replacement-fingerprint".into(),
        accepted_at: UtcMillis(20),
        revision: 1,
        state: magi_session_store::SessionInputState::Pending,
        attached_turn_id: None,
        consumption: None,
        interruption: Some(magi_session_store::InputInterruption {
            turn_id: turn,
            root_task_id: Some(task),
            settled_at: None,
            error: None,
        }),
        payload: serde_json::json!({"request":"frozen"}),
    };
    store.commit_input(pending.clone(), None).unwrap();
    // 输入提交后未做快照：重启必须从同一个 Session 日志恢复屏障。
    let restarted = StateRepository::new(root.clone());
    let (durable, sidecar) = restarted.load_session_projections(&[]).unwrap();
    let store = SessionStore::from_persisted_parts(durable, sidecar).unwrap();
    assert_eq!(
        store.input_for_request(&pending.request_id),
        Some(pending.clone())
    );
    store.install_session_event_writer(Arc::new(restarted));
    store
        .interrupt_current_turn_by_daemon_restart(&session)
        .unwrap();
    let mut settled = pending;
    settled.revision = 2;
    settled.interruption.as_mut().unwrap().settled_at = Some(UtcMillis(30));
    store.commit_input(settled.clone(), Some(1)).unwrap();
    let final_repository = StateRepository::new(root.clone());
    let (durable, sidecar) = final_repository.load_session_projections(&[]).unwrap();
    let store = SessionStore::from_persisted_parts(durable, sidecar).unwrap();
    assert_eq!(store.input_for_request(&settled.request_id), Some(settled));
    assert!(
        store.canonical_turns_for_session(&session)[0]
            .status
            .is_terminal()
    );
    assert!(final_repository.unavailable_sessions().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_event_log_overrides_stale_projection_cache() {
    let state_root = unique_temp_dir("magi-canonical-event-authority");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, turn_id, _) = accepted_session_store("canonical-event-authority", None, 50);
    let session_id = SessionId::new("canonical-event-authority");
    install_test_event_authority(&repository, &session_store);

    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("pending turn should persist");
    let projection_path = repository
        .session_projection_root()
        .join(StateRepository::session_projection_file_name(&session_id));
    let stale_projection = fs::read(&projection_path).expect("projection should exist");

    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
        .expect("turn should complete");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("terminal turn should persist");
    magi_core::fs_atomic::write_atomic(&projection_path, stale_projection)
        .expect("test should restore stale projection cache");

    let restored_repository = StateRepository::new(state_root.clone());
    let (restored, _) = restored_repository
        .load_session_projections(&[])
        .expect("canonical events should advance stale cache");
    assert_eq!(restored.canonical_turns.len(), 1);
    assert_eq!(
        restored.canonical_turns[0].status,
        magi_session_store::CanonicalTurnStatus::Completed
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn canonical_event_log_projects_sidecar_identity_even_when_event_cursor_matches() {
    let state_root = unique_temp_dir("magi-canonical-sidecar-authority");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, turn_id, _) =
        accepted_session_store("canonical-sidecar-authority", None, 51);
    let session_id = SessionId::new("canonical-sidecar-authority");
    install_test_event_authority(&repository, &session_store);

    // 保留接纳时的 sidecar 身份，随后 canonical 新增 item 并收口。
    // 即使 sidecar 未再次 flush，也应直接从相同游标的 canonical 读取完整结果。
    let stale_sidecar = session_store
        .execution_sidecar_store_state()
        .runtime_sidecar(&session_id)
        .expect("accepted turn should have a sidecar");
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .upsert_item_sidecar(
            &session_id,
            Some(turn_id.as_str()),
            ActiveExecutionTurnItem {
                item_id: "turn-item-after-sidecar-checkpoint".to_string(),
                item_seq: 1,
                kind: "assistant_stream".to_string(),
                status: "running".to_string(),
                source: "orchestrator".to_string(),
                title: None,
                content: Some("canonical event is authoritative".to_string()),
                task_id: None,
                worker_id: None,
                role_id: None,
                tool_call_id: None,
                tool_name: None,
                tool_status: None,
                tool_arguments: None,
                tool_result: None,
                tool_error: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                metadata: HashMap::new(),
                timeline_entry_id: None,
                source_thread_id: ThreadId::new(format!("thread-orchestrator-{session_id}")),
            },
        )
        .expect("canonical item should append");
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
        .expect("turn should complete");
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("terminal canonical projection should persist");

    let projection_path = repository
        .session_projection_root()
        .join(StateRepository::session_projection_file_name(&session_id));
    let mut snapshot: SessionProjectionSnapshot =
        serde_json::from_slice(&fs::read(&projection_path).expect("projection should read"))
            .expect("projection should parse");
    assert_eq!(
        snapshot.canonical_event_seq,
        SessionConversationProjection::load(
            &repository.session_event_root(&session_id),
            &session_id
        )
        .expect("event projection should load")
        .last_event_seq(),
        "fixture must prove that the durable event cursor is already current"
    );
    snapshot.sidecar = Some(stale_sidecar);
    repository
        .write_json_atomically(projection_path, &snapshot)
        .expect("stale sidecar fixture should persist");

    let (durable, sidecars) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .expect(
            "sidecar identity must project current canonical facts regardless of cursor equality",
        );
    let canonical = durable
        .canonical_turns
        .iter()
        .find(|turn| turn.session_id == session_id && turn.turn_id == turn_id)
        .expect("canonical turn should restore");
    assert_eq!(
        canonical.status,
        magi_session_store::CanonicalTurnStatus::Completed
    );
    let recovered_turn = sidecars
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| {
            sidecar
                .project(&durable.canonical_turns)
                .unwrap()
                .current_turn
        })
        .expect("canonical turn should project through sidecar identity");
    assert_eq!(recovered_turn.status, "completed");
    assert_eq!(recovered_turn.items.len(), canonical.items.len());
    assert_eq!(
        recovered_turn.items[0].item_id,
        "turn-item-after-sidecar-checkpoint"
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn canonical_projection_cannot_recreate_a_missing_event_log() {
    let state_root = unique_temp_dir("magi-canonical-event-required");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, _, _) = accepted_session_store("canonical-event-required", None, 55);
    let session_id = SessionId::new("canonical-event-required");
    install_test_event_authority(&repository, &session_store);
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("session checkpoint should persist");
    fs::remove_dir_all(repository.session_event_root(&session_id))
        .expect("event log should be removed for corruption test");

    let restarted = StateRepository::new(state_root.clone());
    let (restored, _) = restarted
        .load_session_projections(&[])
        .expect("缺失事件日志只让该会话不可用");
    assert!(
        restored.sessions.is_empty(),
        "v2 projection must not recreate missing canonical facts"
    );
    let unavailable = restarted.unavailable_sessions();
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0].session_id, session_id);
    assert!(unavailable[0].reason.contains("canonical event 游标超前"));
    assert!(!repository.session_event_root(&session_id).exists());

    let _ = fs::remove_dir_all(state_root);
}

fn migrate_legacy_project_fixture(repository: &StateRepository, id: &str, root: &Path) {
    let host = magi_core::HostPath::from_path(root.to_path_buf());
    let legacy = serde_json::json!({
        "active_workspace_id": id, "workspaces": [{
            "workspaceId": id, "name": "legacy", "rootPath": host.display_string(),
            "rootPathRef": host.to_path_ref().as_str(), "worktreeRoot": null,
            "status": "Registered", "createdAt": 1, "updatedAt": 1
        }], "worktree_allocations": [], "snapshots": []
    });
    repository
        .write_json_atomically(repository.state_root.join("workspaces.json"), &legacy)
        .unwrap();
    repository
        .verify_state_layout(&[(id.into(), root.to_path_buf())])
        .unwrap();
    repository.migrate_project_storage().unwrap();
}

#[test]
fn detached_workspace_projection_imports_into_an_empty_event_state_root() {
    let source_state_root = unique_temp_dir("magi-workspace-import-source");
    let target_state_root = unique_temp_dir("magi-workspace-import-target");
    let workspace_root = unique_temp_dir("magi-workspace-import-project");
    let source_repository = StateRepository::new(source_state_root.clone());
    let (session_store, turn_id, _) =
        accepted_session_store("workspace-import-session", Some("workspace-import"), 56);
    let session_id = SessionId::new("workspace-import-session");
    install_test_event_authority(&source_repository, &session_store);
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
        .expect("workspace import turn should complete");
    let source_events = SessionConversationProjection::load(
        &source_repository.session_event_root(&session_id),
        &session_id,
    )
    .expect("source events should load");
    let durable = session_store
        .durable_state()
        .durable_state_for_session(&session_id);
    let sidecar = session_store
        .execution_sidecar_store_state()
        .runtime_sidecar(&session_id);
    let projection_path = workspace_root
        .join(".magi")
        .join("session-projections")
        .join(StateRepository::session_projection_file_name(&session_id));
    source_repository
        .write_json_atomically(
            projection_path.clone(),
            &SessionProjectionSnapshot {
                canonical_event_seq: source_events.last_event_seq(),
                durable: durable.clone(),
                sidecar,
            },
        )
        .expect("portable workspace projection should persist");
    let original_projection_content =
        fs::read(&projection_path).expect("portable projection bytes");

    let target_repository = StateRepository::new(target_state_root.clone());
    let identity = magi_workspace::resolve_or_create_workspace_identity(
        &workspace_root,
        WorkspaceId::new("unused-new-identity"),
    )
    .unwrap();
    assert_eq!(identity.as_str(), "workspace-import");
    let projects = WorkspaceStore::new();
    projects
        .register_native_path(identity, workspace_root.clone())
        .unwrap();
    target_repository
        .save_workspace_durable_state(&projects.durable_state())
        .unwrap();
    target_repository
        .verify_state_layout(&[("workspace-import".into(), workspace_root.clone())])
        .unwrap();
    target_repository.migrate_project_storage().unwrap();
    let (restored, _) = target_repository
        .load_session_projections(&[("workspace-import".to_string(), workspace_root.clone())])
        .expect("detached workspace projection should import into an empty event root");
    assert_eq!(restored.canonical_turns, durable.canonical_turns);
    let imported_events = SessionConversationProjection::load(
        &target_repository.session_event_root(&session_id),
        &session_id,
    )
    .expect("imported events should load");
    assert_eq!(imported_events.canonical_turns(), durable.canonical_turns);
    let rewritten: SessionProjectionSnapshot = serde_json::from_slice(
        &fs::read(&projection_path).expect("rewritten projection should read"),
    )
    .expect("rewritten projection should parse");
    assert_eq!(
        rewritten.canonical_event_seq,
        imported_events.last_event_seq()
    );
    let imported_projection_content = fs::read(&projection_path).expect("projection bytes");
    assert_eq!(
        imported_projection_content, original_projection_content,
        "匹配的 canonical 游标不应重写 project projection"
    );
    let imported_event_root = target_repository.session_event_root(&session_id);
    let imported_event_content = fs::read(
        fs::read_dir(&imported_event_root)
            .expect("event root")
            .next()
            .expect("event segment")
            .expect("event entry")
            .path(),
    )
    .expect("event bytes");
    assert!(
        !target_repository
            .session_projection_transaction_path()
            .exists()
    );

    let restarted_repository = StateRepository::new(target_state_root.clone());
    let (restarted, _) = restarted_repository
        .load_session_projections(&[("workspace-import".to_string(), workspace_root.clone())])
        .expect("workspace import should be idempotent after restart");
    assert_eq!(restarted.canonical_turns, durable.canonical_turns);
    assert_eq!(
        fs::read(&projection_path).expect("restarted projection bytes"),
        imported_projection_content
    );
    assert_eq!(
        fs::read(
            fs::read_dir(&imported_event_root)
                .expect("restarted event root")
                .next()
                .expect("restarted event segment")
                .expect("restarted event entry")
                .path()
        )
        .expect("restarted event bytes"),
        imported_event_content
    );

    let _ = fs::remove_dir_all(source_state_root);
    let _ = fs::remove_dir_all(target_state_root);
    let _ = fs::remove_dir_all(workspace_root);
}

#[test]
fn workspace_projection_identity_mismatch_is_unavailable_and_never_moves_source() {
    let source_state_root = unique_temp_dir("magi-workspace-mismatch-source");
    let target_state_root = unique_temp_dir("magi-workspace-mismatch-target");
    let workspace_root = unique_temp_dir("magi-workspace-mismatch-project");
    let source_repository = StateRepository::new(source_state_root.clone());
    let (session_store, _, _) =
        accepted_session_store("workspace-mismatch-session", Some("workspace-original"), 57);
    let session_id = SessionId::new("workspace-mismatch-session");
    install_test_event_authority(&source_repository, &session_store);
    let source_events = SessionConversationProjection::load(
        &source_repository.session_event_root(&session_id),
        &session_id,
    )
    .expect("source events should load");
    let projection_path = workspace_root
        .join(".magi")
        .join("session-projections")
        .join(StateRepository::session_projection_file_name(&session_id));
    source_repository
        .write_json_atomically(
            projection_path.clone(),
            &SessionProjectionSnapshot {
                canonical_event_seq: source_events.last_event_seq(),
                durable: session_store
                    .durable_state()
                    .durable_state_for_session(&session_id),
                sidecar: session_store
                    .execution_sidecar_store_state()
                    .runtime_sidecar(&session_id),
            },
        )
        .expect("mismatched projection fixture should persist");
    let original_content = fs::read(&projection_path).expect("source projection bytes");

    let target_repository = StateRepository::new(target_state_root.clone());
    migrate_legacy_project_fixture(&target_repository, "workspace-registered", &workspace_root);
    let (restored, _) = target_repository
        .load_session_projections(&[("workspace-registered".to_string(), workspace_root.clone())])
        .expect("workspace identity mismatch only makes that session unavailable");
    assert!(restored.sessions.is_empty());
    let unavailable = target_repository.unavailable_sessions();
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0].session_id, session_id);
    assert_eq!(
        unavailable[0].workspace_id.as_deref(),
        Some("workspace-registered")
    );
    assert!(unavailable[0].reason.contains("归属与扫描根不一致"));
    assert_eq!(
        fs::read(&projection_path).expect("source projection must remain"),
        original_content
    );
    assert!(!target_repository.session_event_root(&session_id).exists());
    assert!(
        !target_repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id))
            .exists()
    );

    let _ = fs::remove_dir_all(source_state_root);
    let _ = fs::remove_dir_all(target_state_root);
    let _ = fs::remove_dir_all(workspace_root);
}

#[test]
fn interrupted_workspace_event_import_recovers_from_durable_transaction() {
    let source_state_root = unique_temp_dir("magi-workspace-import-journal-source");
    let target_state_root = unique_temp_dir("magi-workspace-import-journal-target");
    let workspace_root = unique_temp_dir("magi-workspace-import-journal-project");
    let source_repository = StateRepository::new(source_state_root.clone());
    let (session_store, turn_id, _) = accepted_session_store(
        "workspace-import-journal-session",
        Some("workspace-import-journal"),
        59,
    );
    let session_id = SessionId::new("workspace-import-journal-session");
    install_test_event_authority(&source_repository, &session_store);
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
        .expect("journal import turn should complete");
    let source_events = SessionConversationProjection::load(
        &source_repository.session_event_root(&session_id),
        &session_id,
    )
    .expect("source events should load");
    let mut snapshot = SessionProjectionSnapshot {
        canonical_event_seq: source_events.last_event_seq(),
        durable: session_store
            .durable_state()
            .durable_state_for_session(&session_id),
        sidecar: session_store
            .execution_sidecar_store_state()
            .runtime_sidecar(&session_id),
    };
    let projection_path = workspace_root
        .join(".magi")
        .join("session-projections")
        .join(StateRepository::session_projection_file_name(&session_id));
    source_repository
        .write_json_atomically(projection_path.clone(), &snapshot)
        .expect("portable projection should persist");

    let target_repository = StateRepository::new(target_state_root.clone());
    let event_root = target_repository.session_event_root(&session_id);
    let mutations =
        StateRepository::initial_canonical_mutations(snapshot.durable.canonical_turns.clone());
    let prepared = SessionConversationProjection::load(&event_root, &session_id)
        .expect("empty target event projection")
        .prepare_transaction_write(&event_root, &session_id, &mutations, &[])
        .expect("event import transaction should prepare");
    snapshot.canonical_event_seq = prepared.projection.last_event_seq();
    let projection_content =
        serde_json::to_string_pretty(&snapshot).expect("projection transaction content");
    let transaction = SessionProjectionTransaction {
        schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
        transaction_id: "workspace-import-interrupted".to_string(),
        writes: vec![
            SessionProjectionWrite {
                path: prepared.path,
                content: prepared.content,
            },
            SessionProjectionWrite {
                path: projection_path.clone(),
                content: projection_content,
            },
        ],
        removals: Vec::new(),
    };
    target_repository
        .write_json_atomically(
            target_repository.session_projection_transaction_path(),
            &transaction,
        )
        .expect("durable transaction journal should persist");
    assert!(!event_root.exists());

    let restarted_repository = StateRepository::new(target_state_root.clone());
    migrate_legacy_project_fixture(
        &restarted_repository,
        "workspace-import-journal",
        &workspace_root,
    );
    let (restored, _) = restarted_repository
        .load_session_projections(&[(
            "workspace-import-journal".to_string(),
            workspace_root.clone(),
        )])
        .expect("startup should finish interrupted import transaction");
    assert_eq!(
        restored.canonical_turns,
        session_store.durable_state().canonical_turns
    );
    assert!(event_root.exists());
    assert!(
        !restarted_repository
            .session_projection_transaction_path()
            .exists()
    );

    let _ = fs::remove_dir_all(source_state_root);
    let _ = fs::remove_dir_all(target_state_root);
    let _ = fs::remove_dir_all(workspace_root);
}

#[test]
fn unknown_workspace_projection_never_falls_back_to_global_storage() {
    let state_root = unique_temp_dir("magi-unknown-workspace-projection");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, _, _) =
        accepted_session_store("unknown-workspace-session", Some("workspace-unknown"), 58);
    let session_id = SessionId::new("unknown-workspace-session");
    install_test_event_authority(&repository, &session_store);

    let error = repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect_err("unknown workspace must not persist as personal session");
    assert!(error.to_string().contains("未注册 workspace"));
    assert!(
        !repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id))
            .exists()
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn accepted_event_transaction_recovers_without_session_projection() {
    let state_root = unique_temp_dir("magi-accepted-event-only-recovery");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, turn_id, task_id) =
        accepted_session_store("accepted-event-only-recovery", None, 57);
    let session_id = SessionId::new("accepted-event-only-recovery");
    let acceptance = session_store
        .session_acceptance_record(&session_id, &turn_id)
        .expect("accepted session should provide a recovery record");
    let task = accepted_task(task_id.clone(), 57);

    repository
        .append_session_transaction(
            &session_id,
            &[CanonicalTurnMutation {
                previous: None,
                next: acceptance.canonical_turn.clone(),
            }],
            &[],
            Some((&acceptance, Some(&task))),
        )
        .expect("accepted event transaction should commit");
    // accepted 事务之后继续追加 canonical item 并完成 Turn，模拟 projection 尚未
    // checkpoint 时 daemon 退出。恢复必须从 event-only canonical 结果重建旧 sidecar。
    session_store.install_session_event_writer(Arc::new(repository.clone()));
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .upsert_item_sidecar(
            &session_id,
            Some(turn_id.as_str()),
            ActiveExecutionTurnItem {
                item_id: "event-only-recovered-item".to_string(),
                item_seq: 1,
                kind: "assistant_stream".to_string(),
                status: "running".to_string(),
                source: "orchestrator".to_string(),
                title: None,
                content: Some("event-only canonical item".to_string()),
                task_id: None,
                worker_id: None,
                role_id: None,
                tool_call_id: None,
                tool_name: None,
                tool_status: None,
                tool_arguments: None,
                tool_result: None,
                tool_error: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                metadata: HashMap::new(),
                timeline_entry_id: None,
                source_thread_id: ThreadId::new(format!("thread-orchestrator-{session_id}")),
            },
        )
        .expect("event-only canonical item should append");
    magi_conversation_runtime::CanonicalTurnEventSink::for_store(&session_store, None)
        .set_status_domain(&session_id, Some(turn_id.as_str()), "completed")
        .expect("event-only canonical turn should complete");
    assert!(
        !repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id))
            .exists()
    );
    let restored_repository = StateRepository::new(state_root.clone());
    let (durable, sidecars) = restored_repository
        .load_session_projections(&[])
        .expect("event-only accepted state should recover");
    assert_eq!(durable.sessions.len(), 1);
    assert_eq!(durable.timeline.len(), 1);
    assert_eq!(durable.canonical_turns.len(), 1);
    assert_eq!(durable.canonical_turns[0].turn_id, turn_id);
    let recovered_sidecar = sidecars
        .runtime_sidecar(&session_id)
        .expect("event-only sidecar should be rebuilt");
    let recovered_turn = recovered_sidecar
        .project(&durable.canonical_turns)
        .unwrap()
        .current_turn
        .expect("event-only current turn should be rebuilt");
    assert_eq!(recovered_turn.status, "completed");
    assert_eq!(
        recovered_turn.items.len(),
        durable.canonical_turns[0].items.len()
    );
    let recovered_item = recovered_turn
        .items
        .iter()
        .find(|item| item.item_id == "event-only-recovered-item")
        .expect("event-only canonical item should be restored");
    assert_eq!(
        recovered_item.content.as_deref(),
        Some("event-only canonical item")
    );
    SessionStore::from_persisted_parts(durable.clone(), sidecars.clone())
        .expect("event-only canonical and rebuilt sidecar should pass strict recovery");
    restored_repository
        .validate_session_event_log_coverage(&durable)
        .expect("recovered acceptance should own its event log");

    let recovered_acceptance = restored_repository
        .load_accepted_submissions()
        .expect("accepted event should remain available until task checkpoint");
    assert_eq!(recovered_acceptance.len(), 1);
    assert!(recovered_acceptance[0].session_checkpointed);
    assert!(!recovered_acceptance[0].task_checkpointed);
    assert_eq!(
        recovered_acceptance[0]
            .task
            .as_ref()
            .expect("task acceptance should include task")
            .task_id,
        task_id
    );

    restored_repository
        .save_session_projection_state(&durable, &sidecars)
        .expect("recovered session projection should checkpoint");
    let task_store = TaskStore::new();
    task_store
        .insert_task_without_checkpoint(task)
        .expect("recovered task should insert");
    restored_repository
        .checkpoint_task_store(&task_store)
        .expect("recovered task should checkpoint");
    assert!(
        restored_repository
            .load_accepted_submissions()
            .expect("accepted event should converge after task checkpoint")
            .is_empty()
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn unowned_event_log_is_quarantined_instead_of_blocking_startup() {
    let state_root = unique_temp_dir("magi-canonical-event-orphan");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, _, _) = accepted_session_store("canonical-event-orphan", None, 56);
    let session_id = SessionId::new("canonical-event-orphan");
    install_test_event_authority(&repository, &session_store);
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("session checkpoint should persist");
    fs::remove_file(
        repository
            .session_projection_root()
            .join(StateRepository::session_projection_file_name(&session_id)),
    )
    .expect("projection should be removed for interrupted-write test");

    repository
        .validate_session_event_log_coverage(&session_store.durable_state())
        .expect("accepted WAL-restored session should own its event log");
    // 会话数据（例如其工作区被用户删除）已经不在时，事件日志不能阻止启动：
    // 整目录移到隔离区保留，不删除。
    let event_root = repository.session_event_root(&session_id);
    repository
        .validate_session_event_log_coverage(&SessionDurableState::default())
        .expect("unowned event log must be quarantined, not fail startup");
    assert!(!event_root.exists());
    let quarantined = state_root
        .join("orphaned-session-events")
        .join(event_root.file_name().expect("event root has a name"));
    assert!(quarantined.is_dir(), "event data must be preserved");

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn renaming_one_session_persists_only_that_session_and_survives_a_reload() {
    let state_root = unique_temp_dir("magi-rename-incremental");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, _, _) = accepted_session_store("rename-target", None, 61);
    let session_id = SessionId::new("rename-target");
    install_test_event_authority(&repository, &session_store);
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("initial projection should persist");

    // 与 daemon 装配里的 Rename 持久化回调相同：只提交被重命名的会话。
    let renamed = session_store
        .rename_session_with_persistence(
            &session_id,
            "咖啡店创意命名",
            |durable, sidecars| {
                repository.save_session_projection_state_for_sessions(
                    durable,
                    sidecars,
                    std::slice::from_ref(&session_id),
                )
            },
        )
        .expect("rename should persist");
    assert_eq!(renamed.title, "咖啡店创意命名");

    let reloaded = StateRepository::new(state_root.clone());
    let (restored, _) = reloaded
        .load_session_projections(&[])
        .expect("renamed projection must load");
    let title = restored
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .map(|session| session.title.clone());
    assert_eq!(title.as_deref(), Some("咖啡店创意命名"));

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn restored_session_data_brings_its_quarantined_event_log_back() {
    let state_root = unique_temp_dir("magi-quarantine-restore");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, _, _) = accepted_session_store("quarantine-restore", None, 57);
    let session_id = SessionId::new("quarantine-restore");
    install_test_event_authority(&repository, &session_store);
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("session checkpoint should persist");
    let projection_path = repository
        .session_projection_root()
        .join(StateRepository::session_projection_file_name(&session_id));
    let parked = state_root.join("parked-projection.json");

    // 工作区被删除：投影消失，事件日志被隔离。
    fs::rename(&projection_path, &parked).expect("projection should be parked");
    repository
        .validate_session_event_log_coverage(&SessionDurableState::default())
        .expect("unowned event log is quarantined");
    let event_root = repository.session_event_root(&session_id);
    assert!(!event_root.exists());

    // 目录复原：投影回来，下一次加载前事件日志自动归位，会话完整恢复。
    fs::rename(&parked, &projection_path).expect("projection should be restored");
    let reloaded = StateRepository::new(state_root.clone());
    let (restored, _) = reloaded
        .load_session_projections(&[])
        .expect("restored projection must load with its event log");
    assert_eq!(restored.sessions.len(), 1);
    assert!(event_root.is_dir());
    assert!(
        !state_root
            .join("orphaned-session-events")
            .join(event_root.file_name().expect("event root has a name"))
            .exists()
    );
    reloaded
        .validate_session_event_log_coverage(&restored)
        .expect("restored session owns its event log");

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn model_step_journal_replays_after_checkpoint_and_layout_upgrade() {
    let state_root = unique_temp_dir("magi-step-journal");
    let repository = StateRepository::new(state_root.clone());
    fs::write(state_root.join("state-layout.json"), r#"{"version":4}"#).unwrap();
    repository.verify_state_layout(&[]).unwrap();
    let marker: StateLayoutMarker = repository
        .read_json_strict(&state_root.join("state-layout.json"))
        .unwrap();
    assert_eq!(marker.version, STATE_LAYOUT_VERSION);
    assert!(state_root.join("state-layout.v4.json").exists());
    let (store, turn_id, task_id) = accepted_session_store("step-journal", None, 10);
    let session = SessionId::new("step-journal");
    let (_, main_thread) = store.ensure_session_mission(&session, UtcMillis::now(), || {
        MissionId::new("mission-metered-steps")
    });
    store
        .create_goal(
            session.clone(),
            main_thread,
            turn_id.clone(),
            "metered steps",
            magi_core::AccessProfile::Restricted,
        )
        .unwrap();
    install_test_event_authority(&repository, &store);
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .unwrap();
    for index in 0..70 {
        let step_id = format!("model-call-{index}");
        let now = UtcMillis::now();
        store
            .begin_model_step(
                &session,
                &turn_id,
                magi_session_store::ModelStep {
                    attempts: vec![magi_session_store::ModelAttempt {
                        transport: None,
                        attempt_id: format!("{step_id}:attempt:1"),
                        started_at: now,
                        settled_at: None,
                        status: magi_session_store::ModelStepStatus::Started,
                    }],
                    step_id: step_id.clone(),
                    task_id: Some(task_id.clone()),
                    thread_id: ThreadId::new("thread-step"),
                    goal_id: None,
                    usage_tokens: None,
                    goal_tokens_after: None,
                    checkpoint_generation: 0,
                    request_fingerprint: "a".repeat(64),
                    binding: None,
                    executor_capabilities: Some(magi_bridge_client::native_executor_capabilities()),
                    purpose: Some(magi_session_store::ModelStepPurpose::Execution),
                    provider: "test".into(),
                    message_count: 1,
                    tool_count: 0,
                    max_output_tokens: None,
                    input_request_ids: vec![],
                    started_at: now,
                    settled_at: None,
                    status: magi_session_store::ModelStepStatus::Started,
                    response_fingerprint: None,
                    prompt_frame: (index == 0).then(|| {
                        serde_json::from_value(serde_json::json!({
                            "irVersion": 2,
                            "stablePrefixHash": "a".repeat(64),
                            "appendOnlyHistoryHash": "b".repeat(64),
                            "toolSurfaceHash": "c".repeat(64),
                            "volatileTailHash": "d".repeat(64),
                            "providerContextHash": "e".repeat(64),
                            "postBoundaryHash": "f".repeat(64),
                            "historyMessageCount": 1,
                            "historyChainHash": "1".repeat(64),
                            "cacheBinding": {
                                "lineageId": "2".repeat(64),
                                "cacheKey": "3".repeat(64),
                                "providerHash": "4".repeat(64),
                                "modelHash": "5".repeat(64),
                                "reasoningHash": "6".repeat(64),
                                "executionScopeHash": "7".repeat(64),
                                "skillScopeHash": "8".repeat(64),
                                "invalidationReason": "initial"
                            }
                        }))
                        .unwrap()
                    }),
                    cache_epoch: if index == 0 { 42 } else { 0 },
                },
            )
            .unwrap();
        if index == 0 {
            store
                .start_model_step_retry(&session, &turn_id, &step_id)
                .unwrap();
        }
        // The last request simulates a crash after send, before receiving a result.
        if index < 69 {
            store
                .settle_model_step(
                    &session,
                    &turn_id,
                    &step_id,
                    Some("b".repeat(64)),
                    Some(2),
                    None,
                )
                .unwrap();
        }
    }
    for entry in fs::read_dir(repository.session_event_root(&session)).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("checkpoint-")
        {
            continue;
        }
        let transaction: serde_json::Value =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        for event in transaction["events"].as_array().unwrap() {
            assert_eq!(event["kind"], "model_step_updated");
            assert!(
                event.get("turn").is_none(),
                "普通 Step 更新不能重写完整 Turn/Step 历史"
            );
            assert!(event.get("modelStep").is_some());
        }
    }
    // 模拟旧版 projection、checkpoint 和增量 Step 事件；读取迁移不能丢失 journal。
    fn add_retired_budget_fields(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if object.contains_key("stepId") && object.contains_key("requestFingerprint") {
                    object.insert("reservationTokens".into(), serde_json::json!(4096));
                }
                if object.contains_key("goalId") && object.contains_key("objective") {
                    object.insert("tokenBudget".into(), serde_json::json!(1));
                    object.insert("tokensReserved".into(), serde_json::json!(4096));
                    object.insert("budgetRequiredTokens".into(), serde_json::json!(4096));
                    object.insert("budgetActiveMillis".into(), serde_json::json!(1));
                    object.insert(
                        "runBudget".into(),
                        serde_json::json!({"maxModelAttempts":1}),
                    );
                    object.insert("status".into(), serde_json::json!("budget_limited"));
                }
                for child in object.values_mut() {
                    add_retired_budget_fields(child);
                }
            }
            serde_json::Value::Array(values) => {
                for child in values {
                    add_retired_budget_fields(child);
                }
            }
            _ => {}
        }
    }
    for directory in [
        repository.session_projection_root(),
        repository.session_event_root(&session),
    ] {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                add_retired_budget_fields(&mut value);
                fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
        }
    }
    let expected = store
        .canonical_turn_for_session_turn_id(&session, &turn_id)
        .unwrap();
    let (restored, _) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .unwrap();
    assert_eq!(
        restored.canonical_turns[0].model_steps,
        expected.model_steps
    );
    assert_eq!(
        restored.canonical_turns[0]
            .model_steps
            .last()
            .unwrap()
            .status,
        magi_session_store::ModelStepStatus::Started
    );
    assert_eq!(restored.canonical_turns[0].model_steps[0].attempts.len(), 2);
    let recovered =
        SessionStore::from_persisted_parts(restored, store.execution_sidecar_store_state())
            .unwrap();
    let goal = recovered.current_goal(&session).unwrap();
    assert_eq!(goal.status, magi_session_store::GoalStatus::Paused);
    let resumed = recovered
        .resume_goal_with_plan(&session, &goal.goal_id, goal.control_revision, None, None)
        .unwrap()
        .0;
    assert_eq!(resumed.tokens_used, goal.tokens_used);
    assert_eq!(
        goal.model_attempts_used, 71,
        "重放必须保留压缩/主子代理和重试的累计次数"
    );
    assert_eq!(
        goal.tokens_used, 138,
        "旧 projection 不得漏掉 checkpoint/事件中已结算的用量"
    );
    assert!(
        expected
            .model_steps
            .iter()
            .all(|step| step.goal_id.as_ref() == Some(&goal.goal_id))
    );

    assert!(
        fs::read_dir(repository.session_event_root(&session))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("checkpoint-"))
    );
    // 模拟取消后才收到最后一个请求的响应；只追加结算事件，不重开 Turn。
    store
        .update_current_turn_status_for_turn_with_change(&session, Some(&turn_id), "cancelled")
        .unwrap();
    store
        .settle_model_step(
            &session,
            &turn_id,
            "model-call-69",
            Some("d".repeat(64)),
            Some(3),
            None,
        )
        .unwrap();
    let (late_durable, late_sidecars) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .unwrap();
    let late_restored = SessionStore::from_persisted_parts(late_durable, late_sidecars).unwrap();
    assert_eq!(
        late_restored.current_goal(&session).unwrap().tokens_used,
        141
    );
    let closed = late_restored
        .canonical_turn_for_session_turn_id(&session, &turn_id)
        .unwrap();
    assert_eq!(
        closed.status,
        magi_session_store::CanonicalTurnStatus::Cancelled
    );
    assert_eq!(
        closed.model_steps.last().unwrap().status,
        magi_session_store::ModelStepStatus::ResponseReceived
    );
    fs::remove_dir_all(state_root).unwrap();
}

#[test]
fn user_question_answer_replays_from_event_log_with_a_stale_projection() {
    let state_root = unique_temp_dir("magi-question-journal");
    let repository = StateRepository::new(state_root.clone());
    repository.verify_state_layout(&[]).unwrap();
    let (store, turn_id, task_id) = accepted_session_store("question-journal", None, 10);
    let session = SessionId::new("question-journal");
    install_test_event_authority(&repository, &store);
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .unwrap();
    store.upsert_current_turn_item_for_turn(&session,Some(&turn_id),serde_json::from_value(serde_json::json!({
            "itemId":"question-journal-item", "itemSeq":1, "kind":"tool_call_started", "status":"running",
            "source":"orchestrator", "sourceThreadId":"thread-question-journal", "taskId":task_id,
            "toolName":"ask_user_question", "toolCallId":"question-journal-call"
        })).unwrap()).unwrap();
    let request = magi_session_store::user_question::PendingUserQuestion {
        question_id: "question-journal-id".into(),
        session_id: session.clone(),
        task_id: Some(task_id),
        turn_id: turn_id.clone(),
        tool_call_id: "question-journal-call".into(),
        requested_at: UtcMillis(11),
        can_skip: true,
        questions: magi_session_store::user_question::parse_questions(
            &serde_json::json!({"questions":[{
                "header":"方案","question":"选哪个？","options":[{"label":"A"},{"label":"B"}]
            }]})
            .to_string(),
        )
        .unwrap(),
    };
    store.register_user_question(request.clone()).unwrap();
    // 没有刷新 projection，进程在等待时退出，新的 repository 只能沿日志恢复。
    let restored_repository = StateRepository::new(state_root.clone());
    let (durable, sidecars) = restored_repository.load_session_projections(&[]).unwrap();
    let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
    restored.install_session_event_writer(Arc::new(restored_repository.clone()));
    restored
        .interrupt_current_turn_by_daemon_restart(&session)
        .unwrap();
    assert_eq!(
        restored.pending_user_questions(&session).unwrap(),
        vec![request.clone()]
    );
    let response = magi_session_store::user_question::UserQuestionResponse::Answered {
        answers: vec![magi_session_store::user_question::UserQuestionAnswer {
            selected: vec!["B".into()],
            other: None,
        }],
    };
    restored
        .resolve_user_question(&session, &request.question_id, response.clone())
        .unwrap();
    let (durable, sidecars) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .unwrap();
    let reloaded = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
    assert!(
        reloaded
            .pending_user_questions(&session)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        reloaded
            .user_question_state(&session, &request.question_id)
            .unwrap()
            .unwrap()
            .response,
        Some(response)
    );
    fs::remove_dir_all(state_root).unwrap();
}

#[test]
fn approval_decision_and_cross_turn_claim_replay_without_relaunching_a_tool() {
    use magi_session_store::tool_approval::{PendingToolApproval, ToolApprovalDecision};
    let state_root = unique_temp_dir("magi-approval-journal");
    let repository = StateRepository::new(state_root.clone());
    repository.verify_state_layout(&[]).unwrap();
    let (store, turn_id, task_id) = accepted_session_store("approval-journal", None, 10);
    let session = SessionId::new("approval-journal");
    install_test_event_authority(&repository, &store);
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .unwrap();
    let tool = |id: &str, call: &str| {
        serde_json::from_value(serde_json::json!({
            "itemId":id,"itemSeq":1,"kind":"tool_call_started","status":"running","source":"orchestrator", "sourceThreadId":"thread-approval-journal",
            "taskId":task_id,"toolName":"shell_exec","toolCallId":call,"toolArguments":"{\"command\":\"publish\"}"
        })).unwrap()
    };
    store
        .upsert_current_turn_item_for_turn(
            &session,
            Some(&turn_id),
            tool("approval-origin-item", "approval-origin-call"),
        )
        .unwrap();
    let request = PendingToolApproval {
        approval_id: "approval-journal-id".into(),
        session_id: session.clone(),
        task_id: task_id.clone(),
        turn_id: turn_id.clone(),
        tool_call_id: "approval-origin-call".into(),
        tool_name: "shell_exec".into(),
        reason: "publish".into(),
        requested_at: UtcMillis::now(),
        agent: None,
    };
    store
        .register_tool_approval(request.clone(), None, serde_json::Value::Null, None)
        .unwrap();
    let restored_repository = StateRepository::new(state_root.clone());
    let (durable, sidecars) = restored_repository.load_session_projections(&[]).unwrap();
    let restored = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
    restored.install_session_event_writer(Arc::new(restored_repository));
    restored
        .interrupt_current_turn_by_daemon_restart(&session)
        .unwrap();
    assert_eq!(
        restored.pending_tool_approvals(&session).unwrap(),
        vec![request.clone()]
    );
    restored
        .resolve_tool_approvals(
            &session,
            std::slice::from_ref(&request.approval_id),
            ToolApprovalDecision::AllowOnce,
        )
        .unwrap();
    let now = UtcMillis::now();
    restored
        .accept_conversation_turn_with_timeline_entry(
            session.clone(),
            None,
            magi_session_store::TimelineEntryInput::new(
                "approval-resume-input",
                TimelineEntryKind::UserMessage,
                "继续",
                now,
            ),
            ActiveExecutionTurn {
                turn_id: "approval-resume-turn".into(),
                turn_seq: now.0,
                accepted_at: now,
                status: "running".into(),
                completed_at: None,
                user_message: Some("继续".into()),
                items: vec![tool("approval-target-item", "approval-target-call")],
            },
        )
        .unwrap();
    let receipt = restored
        .claim_tool_approval(
            &session,
            &task_id,
            "approval-target-call",
            "shell_exec",
            r#"{"command":"publish"}"#,
            None,
            &serde_json::Value::Null,
            std::slice::from_ref(&task_id),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(receipt.approval_id, request.approval_id);
    let (durable, sidecars) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .unwrap();
    let replayed = SessionStore::from_persisted_parts(durable, sidecars).unwrap();
    replayed
        .validate_approval_execution_links(&session)
        .unwrap();
    assert!(
        replayed
            .claim_tool_approval(
                &session,
                &task_id,
                "approval-target-call",
                "shell_exec",
                r#"{"command":"publish"}"#,
                None,
                &serde_json::Value::Null,
                std::slice::from_ref(&task_id),
                None
            )
            .unwrap()
            .is_none()
    );
    let target = replayed
        .canonical_turn_for_session_turn_id(&session, "approval-resume-turn")
        .unwrap();
    assert_eq!(
        target.items[0]
            .tool
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["status"],
        "running"
    );
    fs::remove_dir_all(state_root).unwrap();
}

#[test]
fn prior_state_layouts_upgrade_write_barrier_once() {
    for version in 2..=20 {
        let state_root = unique_temp_dir("magi-state-layout-upgrade");
        let repository = StateRepository::new(state_root.clone());
        fs::write(
            state_root.join("state-layout.json"),
            format!("{{\"version\":{version}}}"),
        )
        .unwrap();
        repository.verify_state_layout(&[]).unwrap();
        repository.verify_state_layout(&[]).unwrap();
        let old: StateLayoutMarker = repository
            .read_json_strict(&state_root.join(format!("state-layout.v{version}.json")))
            .unwrap();
        let current: StateLayoutMarker = repository
            .read_json_strict(&state_root.join("state-layout.json"))
            .unwrap();
        assert_eq!(old.version, version);
        assert_eq!(current.version, STATE_LAYOUT_VERSION);
        fs::remove_dir_all(state_root).unwrap();
    }
}

#[test]
fn state_layout_only_accepts_the_current_marked_layout() {
    let fresh_root = unique_temp_dir("magi-layout-fresh");
    let fresh = StateRepository::new(fresh_root.clone());
    fresh
        .verify_state_layout(&[])
        .expect("fresh state root should accept the current layout");
    assert!(fresh_root.join("state-layout.json").exists());
    fresh
        .verify_state_layout(&[])
        .expect("marked current layout should verify again");

    let old_version_root = unique_temp_dir("magi-layout-old-version");
    fs::write(
        old_version_root.join("state-layout.json"),
        r#"{"version":1}"#,
    )
    .expect("old marker should write");
    StateRepository::new(old_version_root.clone())
        .verify_state_layout(&[])
        .expect_err("unsupported layout version must be rejected");

    for root in [fresh_root, old_version_root] {
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn committed_v2_layout_preserves_accepted_event_only_recovery() {
    let state_root = unique_temp_dir("magi-layout-accepted-event-recovery");
    let repository = StateRepository::new(state_root.clone());
    let (session_store, turn_id, task_id) =
        accepted_session_store("accepted-event-recovery", None, 60);
    let session_id = SessionId::new("accepted-event-recovery");
    let acceptance = session_store
        .session_acceptance_record(&session_id, &turn_id)
        .expect("accepted session should provide recovery record");
    repository
        .append_session_transaction(
            &session_id,
            &[CanonicalTurnMutation {
                previous: None,
                next: acceptance.canonical_turn.clone(),
            }],
            &[],
            Some((&acceptance, Some(&accepted_task(task_id, 60)))),
        )
        .expect("accepted event transaction should persist");
    repository
        .write_json_atomically(
            state_root.join("state-layout.json"),
            &StateLayoutMarker {
                version: STATE_LAYOUT_VERSION,
            },
        )
        .expect("committed v2 marker should persist");

    repository
        .verify_state_layout(&[])
        .expect("accepted event-only crash window must remain recoverable");
    assert!(repository.session_event_root(&session_id).exists());
    let (restored, _) = StateRepository::new(state_root.clone())
        .load_session_projections(&[])
        .expect("accepted event should restore the missing session projection");
    assert_eq!(restored.sessions.len(), 1);
    assert_eq!(restored.sessions[0].session_id, session_id);

    let _ = fs::remove_dir_all(state_root);
}

fn resident_event_projections(repository: &StateRepository) -> usize {
    repository
        .session_event_cache
        .lock()
        .expect("event cache")
        .values()
        .filter(|projection| projection.is_resident())
        .count()
}

fn append_test_tool_item(
    session_store: &SessionStore,
    session_id: &SessionId,
    turn_id: &str,
    item_id: &str,
    item_seq: usize,
) {
    session_store
        .upsert_current_turn_item_for_turn(
            session_id,
            Some(turn_id),
            magi_session_store::ActiveExecutionTurnItem {
                item_id: item_id.to_string(),
                item_seq,
                kind: "tool_call_result".to_string(),
                status: "completed".to_string(),
                source: "worker".to_string(),
                title: Some("工具".to_string()),
                content: Some("工具卡片".to_string()),
                task_id: None,
                worker_id: None,
                role_id: None,
                tool_call_id: Some(format!("call-{item_id}")),
                tool_name: Some("file_read".to_string()),
                tool_status: Some("completed".to_string()),
                tool_arguments: Some("{}".to_string()),
                tool_result: Some("{\"ok\":true}".to_string()),
                tool_error: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                metadata: Default::default(),
                timeline_entry_id: None,
                source_thread_id: magi_core::ThreadId::new("thread-main-default"),
            },
        )
        .expect("tool item should append through the canonical event writer");
}

#[test]
fn restored_event_projections_release_turns_and_reload_on_the_next_write() {
    let state_root = unique_temp_dir("magi-event-projection-release");
    let session_id = SessionId::new("event-projection-release");
    {
        let repository = StateRepository::new(state_root.clone());
        let (session_store, turn_id, _task_id) =
            accepted_session_store("event-projection-release", None, 10);
        install_test_event_authority(&repository, &session_store);
        append_test_tool_item(&session_store, &session_id, &turn_id, "item-before", 5);
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("initial projection should save");
    }

    // 重启：所有会话的事件回放结果只保留游标，不常驻完整回合。
    let repository = StateRepository::new(state_root.clone());
    let (durable, sidecars) = repository
        .load_session_projections(&[])
        .expect("projections should restore");
    assert_eq!(resident_event_projections(&repository), 0);
    assert!(
        repository
            .canonical_event_next_sequence(&session_id)
            .expect("cursor is available without resident turns")
            > 1
    );
    let session_store =
        SessionStore::from_persisted_parts(durable, sidecars).expect("store should restore");
    session_store.install_session_event_writer(Arc::new(repository.clone()));
    let restored_turn = session_store.canonical_turns_for_session(&session_id);
    let turn_id = restored_turn[0].turn_id.clone();

    // 完整保存直接使用 SessionStore 的回合，不为冷会话重放事件目录。
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("full save with released projections");
    assert_eq!(resident_event_projections(&repository), 0);

    // 下一次写入才从事件目录重放常驻，并且事实不丢。
    append_test_tool_item(&session_store, &session_id, &turn_id, "item-after", 6);
    assert_eq!(resident_event_projections(&repository), 1);
    repository
        .save_session_projection_state(
            &session_store.durable_state(),
            &session_store.execution_sidecar_store_state(),
        )
        .expect("save after resident append");

    let reloaded = StateRepository::new(state_root.clone());
    let (durable, _) = reloaded
        .load_session_projections(&[])
        .expect("reload after append");
    let item_ids = durable
        .canonical_turns
        .iter()
        .flat_map(|turn| turn.items.iter().map(|item| item.item_id.as_str()))
        .collect::<Vec<_>>();
    assert!(item_ids.contains(&"item-before"), "{item_ids:?}");
    assert!(item_ids.contains(&"item-after"), "{item_ids:?}");
    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn resident_event_projections_are_capped_to_the_most_recently_used() {
    let mut cache = HashMap::new();
    for index in 0..(MAX_RESIDENT_EVENT_PROJECTIONS + 4) {
        let session_id = SessionId::new(format!("resident-cap-{index}"));
        let mut projection = SessionConversationProjection::default();
        projection.touch();
        cache.insert(session_id, projection);
    }
    trim_resident_event_projections(&mut cache);
    let resident = cache
        .iter()
        .filter(|(_, projection)| projection.is_resident())
        .map(|(session_id, _)| session_id.as_str().to_string())
        .collect::<HashSet<_>>();
    assert_eq!(resident.len(), MAX_RESIDENT_EVENT_PROJECTIONS);
    // 释放的是最早使用的那几个。
    for index in 4..(MAX_RESIDENT_EVENT_PROJECTIONS + 4) {
        assert!(resident.contains(&format!("resident-cap-{index}")));
    }
}

#[test]
fn content_digest_distinguishes_content_and_length() {
    assert_eq!(
        ContentDigest::of("同一份内容"),
        ContentDigest::of("同一份内容")
    );
    assert_ne!(ContentDigest::of("a"), ContentDigest::of("b"));
    assert_ne!(ContentDigest::of("a"), ContentDigest::of("a "));
}
