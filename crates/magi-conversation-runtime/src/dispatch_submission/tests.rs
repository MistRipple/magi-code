use super::*;
use magi_session_store::ThreadContextCheckpoint;
use magi_session_store::{
    ExecutionThread, ExecutionThreadStatus, GoalContinuationPhase, GoalStatus, ThreadChatMessage,
    ThreadChatToolCall, ThreadChatToolFunction,
};

fn rollback_test_request(
    session_id: SessionId,
    accepted_at: UtcMillis,
    target_role: &str,
) -> DispatchSubmissionRequest {
    DispatchSubmissionRequest {
        project_directories: None,
        accepted_at,
        session_id,
        workspace_id: Some(WorkspaceId::new("workspace-materialize-rollback")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: format!("timeline-materialize-rollback-{}", accepted_at.0),
        timeline_message: "验证执行装配回滚".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "执行装配回滚".to_string(),
        task_title: "执行: 装配回滚".to_string(),
        trimmed_text: Some("验证执行装配回滚".to_string()),
        execution_goal: Some("验证执行装配失败时不留下任何运行资源".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: Some(target_role.to_string()),
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    }
}

#[test]
fn disabled_tools_survive_acceptance_and_store_reload() {
    let sessions = SessionStore::new();
    let tasks = TaskStore::new();
    let registry = TaskExecutionRegistry::default();
    let events = InMemoryEventBus::new(16);
    let roles = AgentRoleRegistry::load_default();
    let session = SessionId::new("disabled-tools-dispatch");
    sessions
        .create_session(session.clone(), "disabled tools")
        .unwrap();
    let mut request = rollback_test_request(session.clone(), UtcMillis(3500), "coordinator");
    request.use_tools = false;
    let graph = run_dispatch_submission_for_test(
        &DispatchSubmissionRuntime {
            session_store: &sessions,
            task_store: &tasks,
            execution_registry: &registry,
            event_bus: &events,
            agent_role_registry: &roles,
            model_bridge_client: None,
            settings_store: None,
            workspace_root_path: None,
        },
        &request,
    )
    .unwrap();
    let TaskExecutionPlan::Dispatch { use_tools, .. } =
        registry.get(&graph.action_task_id).unwrap();
    assert!(!use_tools);
    accept_dispatch_submission(&sessions, &tasks, &registry, request, graph).unwrap();
    let restored = SessionStore::from_persisted_parts(
        sessions.durable_state(),
        sessions.execution_sidecar_store_state(),
    )
    .unwrap();
    let chain = restored.active_execution_chain(&session).unwrap();
    assert!(
        !chain.branches[0].use_tools,
        "恢复不能把原先关闭的工具重新打开"
    );
}

fn rollback_test_execution_plan(
    session_id: &SessionId,
    mission_id: &MissionId,
    task_id: &TaskId,
    worker_id: &WorkerId,
    thread_id: &ThreadId,
    turn_id: &str,
) -> TaskExecutionPlan {
    TaskExecutionPlan::Dispatch {
        target: TaskExecutionTarget {
            mission_id: mission_id.clone(),
            root_task_id: task_id.clone(),
            task_id: task_id.clone(),
            requested_worker_id: Some(worker_id.clone()),
            recovery_id: None,
            execution_chain_ref: Some("chain-materialize-rollback".to_string()),
        },
        worker_id: worker_id.clone(),
        thread_id: thread_id.clone(),
        is_primary: true,
        session_id: session_id.clone(),
        turn_id: turn_id.to_string(),
        workspace_id: None,
        execution_root: None,
        writebacks: ExecutionWritebackPlans::default(),
        use_tools: true,
        skill_name: None,
        images: Vec::new(),
        execution_settings_snapshot: None,
    }
}

#[test]
fn plugin_context_is_claimed_only_by_model_admission_and_rebuilt_with_provenance() {
    use magi_session_store::{SessionInput, SessionInputState};
    let session = SessionId::new("plugin-context-admission");
    let session_store = SessionStore::default();
    session_store
        .create_session(session.clone(), "plugin context")
        .unwrap();
    let origin = serde_json::json!({"pluginId":"acme.result","pluginName":"Result",
            "messageId":"stable","sourceKind":"result","sourceTurnId":"previous",
            "sourceTaskId":"previous-task","delivery":"context"});
    session_store.commit_input(SessionInput {
            input_id:"context-input".into(),session_id:session.clone(),request_id:"context-request".into(),
            user_message_id:"context-user".into(),request_fingerprint:"context-fingerprint".into(),
            accepted_at:UtcMillis(1),revision:1,state:SessionInputState::Pending,attached_turn_id:None,
            consumption:None,interruption:None,
            payload:serde_json::json!({"plugin_message":origin,"request":{"text":"ORIGINAL_RESULT"}}),
        },None).unwrap();
    let task_store = TaskStore::default();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(8);
    let roles = AgentRoleRegistry::load_default();
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &roles,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let mut request = rollback_test_request(session.clone(), UtcMillis(3000), "coordinator");
    request.workspace_id = None;
    request.user_message_metadata.insert(
        "pluginCommandId".into(),
        serde_json::json!("plugin/acme.result/run"),
    );
    let command = prepare_pending_dispatch_submission(&runtime, &request).unwrap();
    assert_eq!(command.turn.items.len(), 1);
    task_store.remove_task(&command.task.task_id).unwrap();
    assert_eq!(
        session_store
            .input_for_request("context-request")
            .unwrap()
            .state,
        SessionInputState::Pending
    );
    request.user_message_metadata.clear();
    let pending = prepare_pending_dispatch_submission(&runtime, &request).unwrap();
    assert_eq!(pending.turn.items.len(), 2);
    assert_eq!(
        session_store
            .input_for_request("context-request")
            .unwrap()
            .state,
        SessionInputState::Pending,
        "执行准备不能提前领取 Input"
    );
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        pending.active_execution_chain,
        pending.turn,
    );
    let accepted = accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request.clone(),
        graph,
    )
    .unwrap();
    let input = session_store.input_for_request("context-request").unwrap();
    assert_eq!(input.state, SessionInputState::Attached);
    assert_eq!(
        input.attached_turn_id.as_deref(),
        Some(accepted.turn_id.as_str())
    );
    let turn = session_store
        .canonical_turn_for_session_turn_id(&session, &accepted.turn_id)
        .unwrap();
    let item = turn
        .items
        .iter()
        .find(|item| item.metadata.contains_key("pluginMessage"))
        .unwrap();
    assert_eq!(item.content.as_deref(), Some("ORIGINAL_RESULT"));
    assert_eq!(item.metadata["pluginMessage"], origin);
    materialize_dispatch_submission(&runtime, &request, &accepted.turn_id).unwrap();
    let thread = session_store
        .orchestrator_thread_for_session(&session)
        .unwrap()
        .thread_id;
    let projected = session_store
        .thread_context_snapshot(&thread)
        .unwrap()
        .messages()
        .to_vec();
    let message = projected
        .iter()
        .find(|message| message.input_request_id.as_deref() == Some("context-request"))
        .unwrap();
    assert_eq!(message.content, item.model_input_content());
    assert!(
        message
            .content
            .as_ref()
            .unwrap()
            .contains("Plugin-provided message")
    );
}

#[test]
fn pending_dispatch_does_not_duplicate_images_and_recovery_restores_them_from_metadata() {
    let session_id = SessionId::new("session-pending-dispatch-images");
    let mut request = rollback_test_request(session_id.clone(), UtcMillis(3_000), "executor");
    request.images = vec![
        crate::session_images::SessionTurnImage::from_data_url(
            "paste.png",
            "data:image/png;base64,AAAA",
        )
        .expect("image should parse"),
    ];
    let pending_dispatch = serde_json::to_value(&request).expect("request should serialize");
    assert!(
        pending_dispatch.get("images").is_none(),
        "pendingDispatch 不能再保存一份 base64 图片"
    );
    let mut metadata = crate::session_images::session_turn_images_metadata(&request.images);
    metadata.insert("pendingDispatch".to_string(), pending_dispatch);
    let mut item = crate::session_writeback::session_turn_item(
        "user_message",
        "completed",
        None,
        Some("带图片的消息".to_string()),
        Some("turn-item-user-images".to_string()),
        magi_core::ThreadId::new("thread-pending-dispatch-images"),
    );
    item.metadata = metadata;
    let sidecar = SessionRuntimeSidecar {
        session_id,
        ownership: Default::default(),
        recovery_id: None,
        current_turn: Some(ActiveExecutionTurn {
            turn_id: "turn-pending-dispatch-images".to_string(),
            turn_seq: 3_000,
            accepted_at: UtcMillis(3_000),
            completed_at: None,
            status: "accepted".to_string(),
            user_message: Some("带图片的消息".to_string()),
            items: vec![item],
        }),
        active_execution_chain: None,
        status: Default::default(),
        updated_at: UtcMillis(3_000),
    };

    let recovered =
        recover_dispatch_submission_request(&sidecar).expect("pending dispatch should recover");
    assert_eq!(
        recovered.images, request.images,
        "恢复时必须从唯一的图片元数据重建"
    );
}

#[test]
fn dispatch_submission_creates_fresh_worker_thread_even_when_role_has_idle_history() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-fresh-thread");
    let mission_id = MissionId::new("mission-dispatch-fresh-thread");
    let old_thread_id = magi_core::ThreadId::new("thread-executor-old");

    session_store
        .create_session(session_id.clone(), "dispatch fresh thread")
        .expect("session should be creatable");
    session_store
        .register_thread(ExecutionThread {
            thread_id: old_thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: "executor".to_string(),
            worker_instance_id: WorkerId::new("worker-old"),
            status: ExecutionThreadStatus::Idle,
            created_at: UtcMillis(1_000),
            last_used_at: UtcMillis(1_000),
            observed_context_window_tokens: None,
            handled_task_ids: vec![TaskId::new("task-old")],
        })
        .expect("旧 thread 测试数据应注册成功");
    let session_store = crate::test_history::with_messages(&session_store, &old_thread_id,
            serde_json::from_value(serde_json::json!([
                {"role": "user", "content": "历史验收任务：写 validation_auto_save_marker.txt / COMPLEX_WORKER_LANE_OK"},
            ])).unwrap(), UtcMillis(1000));

    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(2_000),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-dispatch-fresh-thread")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-dispatch-fresh-thread".to_string(),
        timeline_message: "创建当前任务文件".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "当前任务推进".to_string(),
        task_title: "当前任务推进".to_string(),
        trimmed_text: Some("创建 task-system-e2e.md".to_string()),
        execution_goal: Some("创建 task-system-e2e.md 并写入当前 marker".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: Some("executor".to_string()),
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("dispatch submission should build graph");
    let chain = graph.active_execution_chain;
    let lane_thread_id = chain.branches[0].thread_id.clone();

    assert_ne!(lane_thread_id, old_thread_id);
    assert!(
        session_store
            .thread_context_snapshot(&lane_thread_id)
            .unwrap()
            .messages()
            .is_empty(),
        "新的 worker thread 不能继承旧 role thread 的历史"
    );
    assert_eq!(
        session_store
            .thread_context_snapshot(&old_thread_id)
            .unwrap()
            .messages()[0]
            .content
            .as_deref(),
        Some("历史验收任务：写 validation_auto_save_marker.txt / COMPLEX_WORKER_LANE_OK")
    );
}

#[test]
fn coordinator_dispatch_reuses_session_orchestrator_thread_for_continuous_context() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-orchestrator-thread");
    session_store
        .create_session(session_id.clone(), "coordinator thread")
        .expect("session should be creatable");

    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let make_request = |accepted_at| DispatchSubmissionRequest {
        project_directories: None,
        accepted_at,
        session_id: session_id.clone(),
        workspace_id: None,
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: format!("timeline-{}", accepted_at.0),
        timeline_message: format!("主线回合 {}", accepted_at.0),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "连续上下文".to_string(),
        task_title: "连续上下文".to_string(),
        trimmed_text: Some("继续处理".to_string()),
        execution_goal: Some("继续处理当前任务".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };

    let first = run_dispatch_submission_for_test(&runtime, &make_request(UtcMillis(1_000)))
        .expect("first coordinator dispatch should build");
    let second = run_dispatch_submission_for_test(&runtime, &make_request(UtcMillis(2_000)))
        .expect("second coordinator dispatch should build");
    let orchestrator_thread_id = session_store
        .orchestrator_thread_for_session(&session_id)
        .expect("session should have orchestrator thread")
        .thread_id;
    let first_thread_id = first
        .active_execution_chain
        .branches
        .first()
        .expect("first branch should exist")
        .thread_id
        .clone();
    let second_thread_id = second
        .active_execution_chain
        .branches
        .first()
        .expect("second branch should exist")
        .thread_id
        .clone();

    assert_eq!(first_thread_id, orchestrator_thread_id);
    assert_eq!(second_thread_id, orchestrator_thread_id);
    let thread = session_store
        .thread_registry_snapshot(&session_id)
        .into_iter()
        .find(|thread| thread.thread_id == orchestrator_thread_id)
        .expect("orchestrator thread should be registered");
    assert_eq!(thread.handled_task_ids.len(), 2);
    assert_eq!(thread.status, ExecutionThreadStatus::Active);
}

#[test]
fn dispatch_submission_persists_authoritative_browser_annotations_on_turn_and_task() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-browser-annotation");
    session_store
        .create_session(session_id.clone(), "dispatch browser annotation")
        .expect("session should be creatable");
    let annotation = serde_json::json!({
        "annotationId": "browser-annotation-dispatch",
        "browserSessionId": "browser-session-dispatch",
        "tabId": "browser-tab-dispatch",
        "comment": "检查保存按钮",
        "anchor": {
            "kind": "region",
            "url": "https://example.com/settings",
            "snapshotRevision": 4
        },
        "screenshotArtifactId": "session-dispatch/annotation.png",
        "screenshotPath": "/tmp/browser-artifacts/session-dispatch/annotation.png",
        "status": "active"
    });
    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(2_100),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-dispatch-browser-annotation")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-dispatch-browser-annotation".to_string(),
        timeline_message: "根据浏览器标记检查页面".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: vec![annotation.clone()],
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "浏览器标记检查".to_string(),
        task_title: "浏览器标记检查".to_string(),
        trimmed_text: Some("根据浏览器标记检查页面".to_string()),
        execution_goal: Some("核对标记位置并报告问题".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: std::collections::HashMap::from([(
            "route".to_string(),
            serde_json::json!("chat"),
        )]),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: Some(Path::new("/tmp/workspace-dispatch-browser-annotation")),
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("browser annotation dispatch should build graph");
    let task = task_store
        .get_task(&graph.root_task_id)
        .expect("root task should persist");
    assert_eq!(
        task.browser_annotation_references(),
        std::slice::from_ref(&annotation)
    );
    let policy = task
        .policy_snapshot
        .as_ref()
        .expect("browser annotation task should have policy");
    assert!(
        policy.allowed_paths.is_empty(),
        "派发不得把默认工作区范围写成显式 allowed_paths"
    );
    assert_eq!(
        policy.read_only_paths,
        vec!["/tmp/browser-artifacts/session-dispatch/annotation.png"]
    );
    assert!(task.input_refs.iter().any(|reference| {
        reference.contains("/tmp/browser-artifacts/session-dispatch/annotation.png")
    }));
    let user_item = graph
        .turn
        .items
        .iter()
        .find(|item| item.kind == "user_message")
        .expect("canonical user item should persist");
    assert_eq!(
        user_item.metadata.get("browserAnnotationRefs"),
        Some(&serde_json::Value::Array(vec![annotation]))
    );
    accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request,
        graph,
    )
    .expect("dispatch acceptance should persist the complete user item atomically");
    let canonical_user_item = session_store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .flat_map(|turn| turn.items)
        .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
        .expect("canonical user item should be restored");
    assert_eq!(
        canonical_user_item.metadata.get("route"),
        Some(&serde_json::json!("chat"))
    );
}

#[test]
fn interrupted_turn_resume_uses_new_thread_with_explicit_checkpoint_history() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-resume-checkpoint");
    let source_task_id = TaskId::new("task-dispatch-resume-source");
    let now = UtcMillis(2_500);

    session_store
        .create_session(session_id.clone(), "dispatch resume checkpoint")
        .expect("session should be creatable");
    let (mission_id, orchestrator_thread_id) =
        session_store.ensure_session_mission(&session_id, now, || {
            MissionId::new("mission-dispatch-resume-checkpoint")
        });
    let source_thread_id = magi_core::ThreadId::new("thread-dispatch-resume-source");
    let source_worker_id = WorkerId::new("worker-dispatch-resume-source");
    let source_history = [
        ThreadChatMessage {
            input_request_id: None,
            role: "user".to_string(),
            content: Some("画当前项目流程图".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: "call-resume-file-read".to_string(),
                kind: "function".to_string(),
                function: ThreadChatToolFunction {
                    name: "file_read".to_string(),
                    arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        ThreadChatMessage {
            input_request_id: None,
            role: "tool".to_string(),
            content: Some(r#"{"status":"succeeded","content":"workspace"}"#.to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-resume-file-read".to_string()),
            provider_context: Vec::new(),
        },
    ];
    session_store
        .register_thread(ExecutionThread {
            thread_id: source_thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: "coordinator".to_string(),
            worker_instance_id: source_worker_id.clone(),
            status: ExecutionThreadStatus::Idle,
            created_at: now,
            last_used_at: now,
            observed_context_window_tokens: None,
            handled_task_ids: vec![source_task_id.clone()],
        })
        .expect("source thread 测试数据应注册成功");
    let coordinator = crate::SessionTurnCoordinator::new();
    let attempt = match coordinator
        .execute_command(
            &session_id,
            crate::TurnCommand::Start(crate::TurnAdmission {
                turn_id: "turn-dispatch-resume-source".to_string(),
                request_id: "request-dispatch-resume-source".to_string(),
                request_fingerprint: "fingerprint-dispatch-resume-source".to_string(),
                profile: crate::ExecutionProfile::Task,
            }),
        )
        .expect("source Turn should start")
    {
        crate::CoordinatorCommandResult::Admission(crate::CoordinatorAdmission::Accepted(
            attempt,
        )) => attempt,
        other => panic!("unexpected source Turn admission: {other:?}"),
    };
    let turn = ActiveExecutionTurn {
        turn_id: "turn-dispatch-resume-source".to_string(),
        turn_seq: now.0,
        accepted_at: now,
        completed_at: None,
        status: "accepted".to_string(),
        user_message: Some("画当前项目流程图".to_string()),
        items: vec![ActiveExecutionTurnItem {
            item_id: "user-dispatch-resume-source".to_string(),
            item_seq: 1,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: Some("画当前项目流程图".to_string()),
            task_id: Some(source_task_id.clone()),
            worker_id: None,
            role_id: None,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            request_id: None,
            user_message_id: Some("user-dispatch-resume-source".to_string()),
            placeholder_message_id: None,
            metadata: Default::default(),
            timeline_entry_id: None,
            source_thread_id: orchestrator_thread_id,
        }],
    };
    let mut turn = turn;
    let mut task_input = turn.items[0].clone();
    task_input.item_id = "source-task-input".into();
    task_input.item_seq = 2;
    task_input.source_thread_id = source_thread_id.clone();
    let mut tool = task_input.clone();
    tool.item_id = "source-tool".into();
    tool.item_seq = 3;
    tool.kind = "tool_call_result".into();
    tool.source = "tool".into();
    tool.tool_call_id = Some("call-resume-file-read".into());
    tool.tool_name = Some("file_read".into());
    tool.tool_status = Some("completed".into());
    tool.tool_arguments = Some(r#"{"path":"Cargo.toml"}"#.into());
    tool.tool_result = Some(r#"{"status":"succeeded","content":"workspace"}"#.into());
    turn.items.extend([task_input, tool]);
    let source_task = make_dispatch_task(DispatchTaskInput {
        task_id: source_task_id.clone(),
        mission_id: mission_id.clone(),
        title: "恢复来源任务".to_string(),
        goal: "验证中断任务恢复".to_string(),
        now,
        target_role: "coordinator",
        active_skill_id: None,
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        context_references: &[],
        required_tool_chain: vec!["file_read".to_string()],
        goal_mode: false,
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        plan_item_id: None,
        browser_annotation_refs: &[],
        browser_node_selections: &[],
    });
    let admitted_turn = turn;
    CanonicalTurnEventSink::for_store(&session_store, None)
        .accept_active_execution_chain_with_timeline_entry_and_task(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-dispatch-resume-source",
                TimelineEntryKind::UserMessage,
                "画当前项目流程图",
                now,
            ),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: source_task_id.clone(),
                execution_chain_ref: "chain-dispatch-resume-source".to_string(),
                workspace_id: None,
                branches: vec![ActiveExecutionBranch {
                    task_id: source_task_id.clone(),
                    worker_id: source_worker_id,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: source_thread_id.clone(),
                }],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "timeline-dispatch-resume-source".to_string(),
                    trimmed_text: Some("画当前项目流程图".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
            &source_task,
        )
        .expect("source Turn should persist through sink");
    coordinator
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: crate::CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("source Turn should prepare");
    coordinator
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt,
                status: crate::CoordinatorTurnStatus::Running,
            },
        )
        .expect("source Turn should run");
    CanonicalTurnEventSink::for_store(&session_store, None)
        .interrupt_turn_by_user(&session_id)
        .expect("source Turn should be interrupted by user");

    session_store
        .install_thread_context_checkpoint_checked(
            &source_thread_id,
            ThreadContextCheckpoint {
                thread_id: source_thread_id.clone(),
                checkpoint_id: "source-summary".into(),
                source_message_count: 1,
                summary_message: ThreadChatMessage {
                    content: Some("来源任务摘要".into()),
                    ..source_history[0].clone()
                },
                reason: "context_window_pressure".into(),
                original_token_estimate: 100,
                checkpoint_token_estimate: 10,
                created_at: UtcMillis(2_500),
                generation: 1,
                tool_result_projection_fingerprint: None,
                source_fingerprint: magi_session_store::thread_history_fingerprint(
                    &session_store
                        .thread_context_snapshot(&source_thread_id)
                        .unwrap()
                        .messages()[..1],
                ),
                model_provider: None,
                model: None,
                binding_revision: None,
                projected_request_tokens: 0,
                context_window_limit_tokens: None,
                preserved_tail_message_count: 2,
                file_fact_versions: vec![],
            },
            UtcMillis(2_500),
        )
        .unwrap();

    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(3_000),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-dispatch-resume-checkpoint")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-dispatch-resume-checkpoint".to_string(),
        timeline_message: "继续".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "继续中断任务".to_string(),
        task_title: "继续: 画当前项目流程图".to_string(),
        trimmed_text: Some("继续".to_string()),
        execution_goal: Some("继续原始流程图任务".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["file_read".to_string()],
        completion_contract: TaskCompletionContract::default().with_evidence_requirements(vec![
            magi_core::TaskEvidenceRequirement::successful_tool_call("diagram_render"),
        ]),
        recovery_checkpoint: Some(TaskRecoveryCheckpoint {
            source_session_id: session_id.clone(),
            source_task_id: source_task_id.clone(),
            source_turn_id: "turn-dispatch-resume-source".to_string(),
            source_thread_id: source_thread_id.clone(),
        }),
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("resume dispatch should build graph");
    let destination_thread_id = graph
        .active_execution_chain
        .branches
        .first()
        .map(|branch| branch.thread_id.clone())
        .expect("resume task should own a thread");
    assert_ne!(destination_thread_id, source_thread_id);
    let resumed_task_id = graph.action_task_id.clone();
    accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request,
        graph,
    )
    .unwrap();
    restore_interrupted_turn_context(
        &session_store,
        &source_thread_id,
        &destination_thread_id,
        UtcMillis(3_000),
    )
    .unwrap();
    let destination_history = session_store
        .thread_context_snapshot(&destination_thread_id)
        .unwrap()
        .messages()
        .to_vec();
    assert_eq!(
        session_store
            .thread_context_checkpoint(&destination_thread_id)
            .unwrap()
            .checkpoint_id,
        "source-summary"
    );
    assert_eq!(destination_history.len(), source_history.len());
    assert_eq!(
        destination_history[0].content.as_deref(),
        Some("画当前项目流程图")
    );
    assert_eq!(
        destination_history[1].tool_calls[0].function.name,
        "file_read"
    );
    assert_eq!(
        destination_history[2].tool_call_id.as_deref(),
        Some("call-resume-file-read")
    );
    let persisted = serde_json::to_vec(&session_store.durable_state()).unwrap();
    let restored = SessionStore::from_persisted_parts(
        serde_json::from_slice(&persisted).unwrap(),
        session_store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .thread_context_snapshot(&destination_thread_id)
            .unwrap()
            .messages(),
        destination_history
    );
    let resumed_task = task_store
        .get_task(&resumed_task_id)
        .expect("resume action task should persist");
    assert_eq!(
        resumed_task
            .recovery_checkpoint()
            .map(|checkpoint| checkpoint.source_turn_id.as_str()),
        Some("turn-dispatch-resume-source")
    );
    assert_eq!(resumed_task.required_tool_chain(), ["file_read"]);
    assert_eq!(
        resumed_task.completion_contract().evidence_requirements,
        [magi_core::TaskEvidenceRequirement::successful_tool_call(
            "diagram_render"
        )]
    );
}

#[test]
fn invalid_interrupted_turn_checkpoint_is_rejected_before_dispatch_side_effects() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-invalid-resume-checkpoint");
    let source_task_id = TaskId::new("task-invalid-resume-source");
    let now = UtcMillis(3_500);

    session_store
        .create_session(session_id.clone(), "invalid resume checkpoint")
        .expect("session should be creatable");
    let (mission_id, orchestrator_thread_id) =
        session_store.ensure_session_mission(&session_id, now, || {
            MissionId::new("mission-invalid-resume-checkpoint")
        });
    let source_worker_id = WorkerId::new("worker-invalid-resume-source");
    let coordinator = crate::SessionTurnCoordinator::new();
    let attempt = match coordinator
        .execute_command(
            &session_id,
            crate::TurnCommand::Start(crate::TurnAdmission {
                turn_id: "turn-invalid-resume-source".to_string(),
                request_id: "request-invalid-resume-source".to_string(),
                request_fingerprint: "fingerprint-invalid-resume-source".to_string(),
                profile: crate::ExecutionProfile::Task,
            }),
        )
        .expect("invalid source Turn should start")
    {
        crate::CoordinatorCommandResult::Admission(crate::CoordinatorAdmission::Accepted(
            attempt,
        )) => attempt,
        other => panic!("unexpected invalid source admission: {other:?}"),
    };
    let source_task = make_dispatch_task(DispatchTaskInput {
        task_id: source_task_id.clone(),
        mission_id: mission_id.clone(),
        title: "无效恢复来源任务".to_string(),
        goal: "验证恢复校验".to_string(),
        now,
        target_role: "coordinator",
        active_skill_id: None,
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        context_references: &[],
        required_tool_chain: vec!["shell_exec".to_string()],
        goal_mode: false,
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        plan_item_id: None,
        browser_annotation_refs: &[],
        browser_node_selections: &[],
    });
    let admitted_turn = ActiveExecutionTurn {
        turn_id: "turn-invalid-resume-source".to_string(),
        turn_seq: now.0,
        accepted_at: now,
        completed_at: None,
        status: "accepted".to_string(),
        user_message: Some("继续验证".to_string()),
        items: vec![ActiveExecutionTurnItem {
            item_id: "user-invalid-resume-source".to_string(),
            item_seq: 1,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: Some("继续验证".to_string()),
            task_id: Some(source_task_id.clone()),
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
            metadata: Default::default(),
            timeline_entry_id: None,
            source_thread_id: orchestrator_thread_id.clone(),
        }],
    };
    CanonicalTurnEventSink::for_store(&session_store, None)
        .accept_active_execution_chain_with_timeline_entry_and_task(
            session_id.clone(),
            TimelineEntryInput::new(
                "timeline-invalid-resume-source",
                TimelineEntryKind::UserMessage,
                "继续验证",
                now,
            ),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id,
                root_task_id: source_task_id.clone(),
                execution_chain_ref: "chain-invalid-resume-source".to_string(),
                workspace_id: None,
                branches: vec![ActiveExecutionBranch {
                    task_id: source_task_id.clone(),
                    worker_id: source_worker_id,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: orchestrator_thread_id.clone(),
                }],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "timeline-invalid-resume-source".to_string(),
                    trimmed_text: Some("继续验证".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
            &source_task,
        )
        .expect("invalid source Turn should persist");
    coordinator
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: crate::CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("invalid source Turn should prepare");
    coordinator
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt,
                status: crate::CoordinatorTurnStatus::Running,
            },
        )
        .expect("invalid source Turn should run");
    CanonicalTurnEventSink::for_store(&session_store, None)
        .interrupt_turn_by_user(&session_id)
        .expect("invalid source Turn should be interrupted");
    let thread_count_before = session_store.thread_registry_snapshot(&session_id).len();
    let event_count_before = event_bus.snapshot().recent_events.len();
    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(4_000),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-invalid-resume-checkpoint")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-invalid-resume-checkpoint".to_string(),
        timeline_message: "继续".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "继续中断任务".to_string(),
        task_title: "继续: 验证恢复原子性".to_string(),
        trimmed_text: Some("继续".to_string()),
        execution_goal: Some("继续验证恢复原子性".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["shell_exec".to_string()],
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: Some(TaskRecoveryCheckpoint {
            source_session_id: session_id.clone(),
            source_task_id,
            source_turn_id: "turn-invalid-resume-source".to_string(),
            // 主线 thread 不处理 action task，正是本次真实故障中的错误参数。
            source_thread_id: orchestrator_thread_id,
        }),
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let error = match run_dispatch_submission_for_test(&runtime, &request) {
        Ok(_) => panic!("mismatched source thread should reject dispatch"),
        Err(error) => error,
    };

    assert!(
        error
            .into_message()
            .contains("续接来源 Thread 与任务不匹配")
    );
    assert!(task_store.all_tasks().is_empty());
    assert!(
        execution_registry
            .get(&TaskId::new("task-local-agent-4000"))
            .is_none()
    );
    assert_eq!(
        session_store.thread_registry_snapshot(&session_id).len(),
        thread_count_before,
        "拒绝恢复不得创建空的 task thread",
    );
    assert_eq!(event_bus.snapshot().recent_events.len(), event_count_before);
    assert_eq!(
        session_store
            .canonical_turns_for_session(&session_id)
            .last()
            .map(|turn| turn.status),
        Some(magi_session_store::CanonicalTurnStatus::Cancelled),
    );
}

/// 任务系统验收：所有 action task 统一走 ExecutionChain 路径。
///
/// 验收点：
/// - route 已是 task（由 classifier 决定，本处不重测）；
/// - dispatch 创建 action task 并落入 TaskStore；
/// - `policy_snapshot.task_tier == ExecutionChain`；
/// - 同步产生 ActiveExecutionChain，让运行期具备可观察的执行链。
///
#[test]
fn execution_chain_dispatch_creates_action_task_with_chain_tier() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-exec-chain-tier");

    session_store
        .create_session(session_id.clone(), "execution chain tier")
        .expect("session should be creatable");

    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(3_000),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-exec-chain-tier")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-exec-chain-tier".to_string(),
        timeline_message: "修复明确 bug 并跑相关验证".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "修复 bug + 验证".to_string(),
        task_title: "修复 bug + 验证".to_string(),
        trimmed_text: Some("修复明确 bug 并跑相关验证".to_string()),
        execution_goal: Some("定位并修复 bug、再跑相关验证命令".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::FullAccess,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: Some("executor".to_string()),
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("execution chain dispatch should build graph");

    let action_task = task_store
        .get_task(&graph.action_task_id)
        .expect("action task should be persisted in TaskStore");
    let policy = action_task
        .policy_snapshot
        .as_ref()
        .expect("dispatch 必须给 action task 写入 policy_snapshot");
    assert_eq!(
        policy.task_tier,
        TaskTier::ExecutionChain,
        "action task tier 必须统一为 ExecutionChain",
    );
    assert_eq!(
        policy.access_profile,
        AccessProfile::FullAccess,
        "用户选择的访问模式必须写入 action task policy_snapshot",
    );

    let chain = &graph.active_execution_chain;
    assert!(
        chain.current_turn_id.is_some(),
        "ActiveExecutionChain 必须指向当前 Turn",
    );
}

#[test]
fn dispatch_binds_root_task_to_current_plan_stage() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-plan-root-binding");
    session_store
        .create_session(session_id.clone(), "plan root binding")
        .expect("session should be creatable");
    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    let plan = plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![
                magi_plan::UpdatePlanItemInput {
                    item_id: Some("implement".to_string()),
                    step: "完成实现".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                },
                magi_plan::UpdatePlanItemInput {
                    item_id: Some("verify".to_string()),
                    step: "完成验证".to_string(),
                    status: magi_core::PlanItemStatus::Pending,
                },
            ],
        })
        .expect("plan should create");
    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(3_100),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-plan-root-binding")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-plan-root-binding".to_string(),
        timeline_message: "继续计划".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "继续计划".to_string(),
        task_title: "继续计划".to_string(),
        trimmed_text: Some("继续计划".to_string()),
        execution_goal: Some("完成当前计划全部阶段".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::FullAccess,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph =
        run_dispatch_submission_for_test(&runtime, &request).expect("dispatch should build");
    let root_task = task_store
        .get_task(&graph.root_task_id)
        .expect("root task should exist");
    assert_eq!(root_task.plan_item_id(), Some(&plan.items[0].item_id));
    let bound_plan = plan_store.snapshot().expect("plan should remain");
    assert_eq!(
        bound_plan.task_bindings.get(&graph.root_task_id),
        Some(&plan.items[0].item_id)
    );
}

#[test]
fn selected_skill_does_not_reassign_mainline_coordinator_role() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-skill-mainline-role");

    session_store
        .create_session(session_id.clone(), "dispatch skill mainline role")
        .expect("session should be creatable");

    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(3_250),
        session_id,
        workspace_id: Some(WorkspaceId::new("workspace-dispatch-skill-mainline-role")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-dispatch-skill-mainline-role".to_string(),
        timeline_message: "使用 browser Skill 创建 explorer 子代理".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "Skill 子代理继承".to_string(),
        task_title: "Skill 子代理继承".to_string(),
        trimmed_text: Some("使用 browser Skill 创建 explorer 子代理".to_string()),
        execution_goal: Some("创建 explorer 子代理并等待结果".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::FullAccess,
        skill_name: Some("stellarlinkco/myclaude/skills/browser".to_string()),
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("dispatch submission should build graph");
    let action_task = task_store
        .get_task(&graph.action_task_id)
        .expect("action task should be persisted in TaskStore");

    assert_eq!(
        action_task.executor_binding_target_role(),
        Some("coordinator"),
        "Skill 只决定本轮执行方法，不能把主线入口降级为不能创建子代理的 worker role"
    );
    assert_eq!(
        action_task.executor_binding_active_skill_id(),
        Some("stellarlinkco/myclaude/skills/browser"),
        "主线保持 coordinator 时仍必须保留完整 Skill ID"
    );
}

#[test]
fn dispatch_submission_persists_active_skill_id_on_action_task() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-active-skill");

    session_store
        .create_session(session_id.clone(), "dispatch active skill")
        .expect("session should be creatable");

    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(3_500),
        session_id,
        workspace_id: Some(WorkspaceId::new("workspace-dispatch-active-skill")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-dispatch-active-skill".to_string(),
        timeline_message: "使用代码审查 skill 检查当前改动".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "代码审查".to_string(),
        task_title: "代码审查".to_string(),
        trimmed_text: Some("使用代码审查 skill 检查当前改动".to_string()),
        execution_goal: Some("检查当前改动并给出问题列表".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: Some("code-review".to_string()),
        goal_mode: false,
        use_tools: true,
        target_role: Some("reviewer".to_string()),
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("dispatch submission should build graph");
    let action_task = task_store
        .get_task(&graph.action_task_id)
        .expect("action task should be persisted in TaskStore");

    assert_eq!(
        action_task.executor_binding_active_skill_id(),
        Some("code-review"),
        "active skill 必须进入 Task executor_binding，任务重跑才能恢复同一 skill 上下文"
    );
    assert_eq!(
        action_task
            .executor_binding
            .as_ref()
            .and_then(|binding| binding.active_skill_id.as_deref()),
        Some("code-review"),
        "Task executor_binding 已类型化，不能再写入旧 skill_name 字段"
    );
}

#[test]
fn dispatch_submission_propagates_context_references_to_task_policy_and_turn_metadata() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-dispatch-context-reference");
    session_store
        .create_session(session_id.clone(), "dispatch context reference")
        .expect("session should be creatable");

    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(4_000),
        session_id,
        workspace_id: Some(WorkspaceId::new("workspace-dispatch-context-reference")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-dispatch-context-reference".to_string(),
        timeline_message: "检查引用文件".to_string(),
        images: Vec::new(),
        context_references: vec![SessionContextReference {
            kind: crate::context_reference::SessionContextReferenceKind::File,
            path: std::path::PathBuf::from("/tmp/external/reference.md"),
            source_folder_id: None,
            name: "reference.md".to_string(),
        }],
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "检查引用文件".to_string(),
        task_title: "检查引用文件".to_string(),
        trimmed_text: Some("检查引用文件".to_string()),
        execution_goal: Some("读取并分析引用文件".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: false,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: Vec::new(),
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::User,
    };
    let workspace_root = std::path::PathBuf::from("/tmp/workspace");
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: Some(&workspace_root),
    };

    let graph = run_dispatch_submission_for_test(&runtime, &request)
        .expect("dispatch submission should propagate context reference");
    let task = task_store
        .get_task(&graph.action_task_id)
        .expect("action task should exist");
    let policy = task
        .policy_snapshot
        .as_ref()
        .expect("task policy should exist");
    assert!(
        policy.allowed_paths.is_empty(),
        "派发不得把默认工作区范围写成显式 allowed_paths"
    );
    assert_eq!(
        policy.read_only_paths,
        vec!["/tmp/external/reference.md".to_string()]
    );
    assert!(
        task.input_refs
            .iter()
            .any(|value| value.contains("/tmp/external/reference.md"))
    );
    let user_item = graph
        .turn
        .items
        .first()
        .expect("canonical user item should exist");
    assert!(user_item.metadata.contains_key("contextReferences"));
}

#[test]
fn goal_continuation_uses_root_execution_chain_without_faking_user_message() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-goal-continuation-dispatch");
    session_store
        .create_session(session_id.clone(), "goal continuation dispatch")
        .expect("session should be creatable");
    let (_, orchestrator_thread_id) =
        session_store.ensure_session_mission(&session_id, UtcMillis(4_999), || {
            MissionId::new("mission-goal-continuation-dispatch")
        });
    let goal = session_store
        .create_goal(
            session_id.clone(),
            orchestrator_thread_id,
            "task-goal-creator",
            "完成验收",
            AccessProfile::Restricted,
        )
        .expect("goal should be creatable");
    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(5_000),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-goal-continuation-dispatch")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-goal-continuation-dispatch".to_string(),
        timeline_message: "目标自动推进: 完成验收".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "目标自动推进".to_string(),
        task_title: "执行: 目标自动推进".to_string(),
        trimmed_text: None,
        execution_goal: Some("先读取当前目标，再继续推进".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: true,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["get_goal".to_string()],
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::GoalContinuation(goal.goal_id.clone()),
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph =
        run_dispatch_submission_for_test(&runtime, &request).expect("dispatch should build");
    let turn = &graph.turn;
    assert!(turn.user_message.is_none());
    assert_eq!(turn.items.len(), 1);
    assert_eq!(turn.items[0].kind, "assistant_phase");
    assert_eq!(
        turn.items[0]
            .metadata
            .get("renderable")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    let root_task = task_store
        .get_task(&graph.root_task_id)
        .expect("root task should exist");
    assert_eq!(
        root_task.executor_binding_target_role(),
        Some("coordinator")
    );
    assert_eq!(root_task.required_tool_chain(), ["get_goal"]);
    let root_task_id = graph.root_task_id.clone();
    accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request,
        graph,
    )
    .expect("continuation should be accepted");
    let accepted_goal = session_store
        .current_goal(&session_id)
        .expect("goal should remain visible");
    assert_eq!(
        accepted_goal.continuation.phase,
        GoalContinuationPhase::Running
    );
    assert_eq!(
        accepted_goal.continuation.turn_id.as_deref(),
        Some(root_task_id.as_str())
    );
    assert!(
        session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .is_some()
    );
}

#[test]
fn paused_goal_rejects_built_continuation_without_leaving_pending_turn() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-goal-continuation-pause-race");
    session_store
        .create_session(session_id.clone(), "goal continuation pause race")
        .expect("session should be creatable");
    let (_, orchestrator_thread_id) =
        session_store.ensure_session_mission(&session_id, UtcMillis(5_999), || {
            MissionId::new("mission-goal-continuation-pause-race")
        });
    let goal = session_store
        .create_goal(
            session_id.clone(),
            orchestrator_thread_id,
            "task-goal-creator",
            "验证暂停竞态",
            AccessProfile::Restricted,
        )
        .expect("goal should be creatable");
    let request = DispatchSubmissionRequest {
        project_directories: None,
        accepted_at: UtcMillis(6_000),
        session_id: session_id.clone(),
        workspace_id: Some(WorkspaceId::new("workspace-goal-continuation-pause-race")),
        execution_root: None,
        orchestrator_session_config: None,
        entry_id: "timeline-goal-continuation-pause-race".to_string(),
        timeline_message: "目标自动推进: 验证暂停竞态".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        created_session: false,
        mission_title: "目标自动推进".to_string(),
        task_title: "执行: 目标自动推进".to_string(),
        trimmed_text: None,
        execution_goal: Some("先读取当前目标，再继续推进".to_string()),
        task_tier: TaskTier::ExecutionChain,
        access_profile: AccessProfile::Restricted,
        skill_name: None,
        goal_mode: true,
        use_tools: true,
        target_role: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["get_goal".to_string()],
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::GoalContinuation(goal.goal_id.clone()),
    };
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };

    let graph =
        run_dispatch_submission_for_test(&runtime, &request).expect("dispatch should build");
    let root_task_id = graph.root_task_id.clone();
    let mut clear_request = request.clone();
    clear_request.accepted_at = UtcMillis(6_001);
    clear_request.entry_id = "timeline-goal-continuation-clear-race".to_string();
    clear_request.timeline_message = "目标自动推进: 验证清除竞态".to_string();
    session_store
        .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
        .expect("goal should pause before acceptance");

    assert!(
        accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            request,
            graph,
        )
        .is_err()
    );
    assert!(task_store.get_task(&root_task_id).is_none());
    assert!(execution_registry.get(&root_task_id).is_none());
    assert!(
        session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .is_none()
    );
    assert!(
        session_store
            .timeline_for_session(&session_id)
            .iter()
            .all(|entry| entry.entry_id != "timeline-goal-continuation-pause-race")
    );
    let paused = session_store
        .current_goal(&session_id)
        .expect("paused goal should remain");
    assert_eq!(paused.status, GoalStatus::Paused);
    assert_eq!(paused.continuation.phase, GoalContinuationPhase::Idle);

    let clear_graph = run_dispatch_submission_for_test(&runtime, &clear_request)
        .expect("dispatch should build before goal clear");
    let clear_root_task_id = clear_graph.root_task_id.clone();
    session_store
        .clear_goal_with_plan(&session_id, &goal.goal_id, paused.control_revision, None)
        .expect("goal should clear before acceptance");
    assert!(
        accept_dispatch_submission(
            &session_store,
            &task_store,
            &execution_registry,
            clear_request,
            clear_graph,
        )
        .is_err()
    );
    assert!(task_store.get_task(&clear_root_task_id).is_none());
    assert!(execution_registry.get(&clear_root_task_id).is_none());
    assert!(session_store.current_goal(&session_id).is_none());
    assert!(
        session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .is_none()
    );
    assert!(
        session_store
            .timeline_for_session(&session_id)
            .iter()
            .all(|entry| entry.entry_id != "timeline-goal-continuation-clear-race")
    );
}

#[test]
fn worker_thread_conflict_rolls_back_new_coordinator_task_and_registry() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-worker-thread-conflict-rollback");
    session_store
        .create_session(session_id.clone(), "worker thread conflict rollback")
        .expect("session should be creatable");
    let request = rollback_test_request(session_id.clone(), UtcMillis(7_100), "executor");
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let prepared = validate_dispatch_request(&runtime, &request, true)
        .expect("request should validate before conflict injection");
    session_store
        .register_thread(ExecutionThread {
            thread_id: prepared.worker_thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: prepared.mission_id.clone(),
            role_id: "executor".to_string(),
            worker_instance_id: WorkerId::new("worker-existing-conflict"),
            status: ExecutionThreadStatus::Idle,
            created_at: UtcMillis(7_000),
            last_used_at: UtcMillis(7_000),
            observed_context_window_tokens: None,
            handled_task_ids: vec![TaskId::new("task-existing-conflict")],
        })
        .expect("conflicting thread should register");

    let error = match run_dispatch_submission_for_test(&runtime, &request) {
        Ok(_) => panic!("duplicate worker thread id must reject materialization"),
        Err(error) => error,
    };

    assert!(error.into_message().contains("拒绝重复注册"));
    assert!(task_store.get_task(&prepared.action_task_id).is_none());
    assert!(execution_registry.get(&prepared.action_task_id).is_none());
    assert!(
        session_store
            .orchestrator_thread_for_session(&session_id)
            .is_none(),
        "materialize 创建的 coordinator 必须随失败一起回收"
    );
    let threads = session_store.thread_registry_snapshot(&session_id);
    assert_eq!(threads.len(), 1);
    assert_eq!(
        threads[0].worker_instance_id.as_str(),
        "worker-existing-conflict"
    );
}

#[test]
fn materialize_rollback_restores_full_resources_and_keeps_plan_revision_monotonic() {
    let session_store = SessionStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let session_id = SessionId::new("session-materialize-owned-resource-rollback");
    let mission_id = MissionId::new("mission-materialize-owned-resource-rollback");
    let task_id = TaskId::new("task-materialize-owned-resource-rollback");
    let worker_id = WorkerId::new("worker-materialize-owned-resource-rollback");
    let turn_id = "turn-materialize-owned-resource-rollback";
    session_store
        .create_session(session_id.clone(), "materialize owned resource rollback")
        .expect("session should be creatable");
    let (_, coordinator_thread_id) =
        session_store.ensure_session_mission(&session_id, UtcMillis(8_000), || mission_id.clone());
    let coordinator_before = session_store
        .orchestrator_thread_for_session(&session_id)
        .expect("coordinator should exist");
    let coordinator_after = session_store
        .activate_thread_checked(&coordinator_thread_id, &task_id, UtcMillis(8_100))
        .expect("coordinator should activate");

    let worker_thread_id = ThreadId::new("thread-materialize-owned-resource-rollback");
    let worker_thread = ExecutionThread {
        thread_id: worker_thread_id.clone(),
        session_id: session_id.clone(),
        mission_id: mission_id.clone(),
        role_id: "executor".to_string(),
        worker_instance_id: worker_id.clone(),
        status: ExecutionThreadStatus::Active,
        created_at: UtcMillis(8_000),
        last_used_at: UtcMillis(8_100),
        observed_context_window_tokens: None,
        handled_task_ids: vec![task_id.clone()],
    };
    session_store
        .register_thread(worker_thread.clone())
        .expect("worker thread should register");
    let mut persisted = session_store.export_state();
    persisted
        .thread_context_checkpoints
        .push(ThreadContextCheckpoint {
            thread_id: worker_thread_id.clone(),
            checkpoint_id: "checkpoint-materialize-rollback".to_string(),
            source_message_count: 1,
            summary_message: ThreadChatMessage {
                input_request_id: None,
                role: "system".to_string(),
                content: Some("恢复摘要".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            reason: "test".to_string(),
            original_token_estimate: 100,
            checkpoint_token_estimate: 20,
            created_at: UtcMillis(8_100),
            generation: 1,
            tool_result_projection_fingerprint: None,
            source_fingerprint: "fingerprint-materialize-rollback".to_string(),
            model_provider: None,
            model: None,
            binding_revision: None,
            projected_request_tokens: 20,
            context_window_limit_tokens: None,
            preserved_tail_message_count: 0,
            file_fact_versions: Vec::new(),
        });
    let session_store = SessionStore::from_state_for_test(persisted);

    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    let original_plan = plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("materialize-step".to_string()),
                step: "验证装配回滚".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("plan should create");
    let (plan_before_binding, plan_after_binding) = plan_store
        .bind_task_for_materialization(task_id.clone(), original_plan.items[0].item_id.clone())
        .expect("plan binding should succeed")
        .expect("new task binding should mutate plan");
    execution_registry
        .insert(
            task_id.clone(),
            rollback_test_execution_plan(
                &session_id,
                &mission_id,
                &task_id,
                &worker_id,
                &worker_thread_id,
                turn_id,
            ),
        )
        .expect("execution plan should register");

    let mut rollback = MaterializeRollback::new(
        &session_store,
        &execution_registry,
        session_id.clone(),
        task_id.clone(),
        turn_id.to_string(),
    );
    rollback.record_created_thread(worker_thread);
    rollback.record_coordinator_before(coordinator_before.clone());
    rollback.record_coordinator_after(coordinator_after);
    rollback.record_plan_materialization(PlanMaterialization {
        original_plan: Some(plan_before_binding.clone()),
        current_revision: plan_after_binding.revision,
    });
    rollback.record_registry_inserted();

    assert!(rollback.rollback_now().is_empty());
    assert!(execution_registry.get(&task_id).is_none());
    assert_eq!(
        session_store
            .orchestrator_thread_for_session(&session_id)
            .expect("coordinator should remain"),
        coordinator_before
    );
    assert!(
        session_store
            .thread_registry_snapshot(&session_id)
            .iter()
            .all(|thread| thread.thread_id != worker_thread_id)
    );
    assert!(
        session_store
            .thread_context_checkpoint(&worker_thread_id)
            .is_none(),
        "回收恢复 thread 时必须同步删除 context checkpoint"
    );
    let restored_plan = plan_store.snapshot().expect("plan should remain");
    assert_eq!(restored_plan.items, plan_before_binding.items);
    assert_eq!(restored_plan.state, plan_before_binding.state);
    assert_eq!(
        restored_plan.task_bindings,
        plan_before_binding.task_bindings
    );
    assert_eq!(restored_plan.revision, plan_after_binding.revision);
    assert!(restored_plan.revision > plan_before_binding.revision);
}

#[test]
fn accepted_runner_start_cleanup_is_idempotent_for_registry_worker_and_plan_binding() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-accepted-runner-cleanup");
    session_store
        .create_session(session_id.clone(), "accepted runner cleanup")
        .expect("session should be creatable");
    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("accepted-runner-step".to_string()),
                step: "验证 runner 启动失败清理".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("plan should create");
    let request = rollback_test_request(session_id.clone(), UtcMillis(9_100), "executor");
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let pending = prepare_pending_dispatch_submission(&runtime, &request)
        .expect("pending dispatch should prepare");
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        pending.active_execution_chain,
        pending.turn,
    );
    let accepted = accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request.clone(),
        graph,
    )
    .expect("dispatch should be accepted");
    session_store
        .update_current_turn_status_for_turn_with_change(
            &session_id,
            Some(&accepted.turn_id),
            "preparing",
        )
        .expect("preparation status should commit");
    assert_eq!(
        session_store
            .runtime_sidecar(&session_id)
            .unwrap()
            .current_turn
            .unwrap()
            .status,
        "running",
        "preparing follows the canonical Running projection"
    );
    materialize_dispatch_submission(&runtime, &request, &accepted.turn_id)
        .expect("accepted dispatch should materialize");
    let worker_thread_id = session_store
        .active_execution_chain(&session_id)
        .and_then(|chain| {
            chain
                .branches
                .first()
                .map(|branch| branch.thread_id.clone())
        })
        .expect("accepted chain should contain worker thread");
    assert!(execution_registry.get(&accepted.root_task_id).is_some());
    assert!(
        plan_store
            .snapshot()
            .expect("plan should remain")
            .task_bindings
            .contains_key(&accepted.root_task_id)
    );

    for _ in 0..2 {
        cleanup_materialized_dispatch_submission_if_not_started(
            &session_store,
            &execution_registry,
            &session_id,
            &accepted.root_task_id,
            &accepted.turn_id,
        )
        .expect("runner start cleanup should be idempotent");
    }

    assert!(execution_registry.get(&accepted.root_task_id).is_none());
    assert!(
        session_store
            .thread_registry_snapshot(&session_id)
            .iter()
            .all(|thread| thread.thread_id != worker_thread_id)
    );
    assert!(
        !plan_store
            .snapshot()
            .expect("plan should remain")
            .task_bindings
            .contains_key(&accepted.root_task_id)
    );
    assert_eq!(
        task_store
            .get_task(&accepted.root_task_id)
            .expect("accepted task fact must remain")
            .status,
        TaskStatus::Pending
    );
    assert_eq!(
        session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("accepted turn fact must remain")
            .turn_id,
        accepted.turn_id
    );
}

#[test]
fn accepted_materialize_rejects_registry_owned_by_another_execution_identity() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-stale-materialize-registry");
    session_store
        .create_session(session_id.clone(), "stale materialize registry")
        .expect("session should be creatable");
    let request = rollback_test_request(session_id.clone(), UtcMillis(9_200), "executor");
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let pending = prepare_pending_dispatch_submission(&runtime, &request)
        .expect("pending dispatch should prepare");
    let prepared = validate_dispatch_request(&runtime, &request, false)
        .expect("accepted request should validate");
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        pending.active_execution_chain,
        pending.turn,
    );
    let accepted = accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request.clone(),
        graph,
    )
    .expect("dispatch should be accepted");
    execution_registry
        .insert(
            accepted.root_task_id.clone(),
            rollback_test_execution_plan(
                &session_id,
                &prepared.mission_id,
                &accepted.root_task_id,
                &prepared.worker_id,
                &prepared.worker_thread_id,
                &accepted.turn_id,
            ),
        )
        .expect("conflicting execution plan should register");

    let error = materialize_dispatch_submission(&runtime, &request, &accepted.turn_id)
        .expect_err("different Turn registry entry must not count as idempotent success");

    assert!(error.into_message().contains("已被其他执行身份装配"));
    assert_eq!(
        execution_registry
            .turn_id(&accepted.root_task_id)
            .as_deref(),
        Some(accepted.turn_id.as_str())
    );
    assert!(
        session_store
            .thread_registry_snapshot(&session_id)
            .is_empty(),
        "身份冲突必须在创建执行 thread 前被拒绝"
    );
}

#[test]
fn active_chain_writeback_failure_rolls_back_materialized_execution_resources() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-chain-writeback-rollback");
    session_store
        .create_session(session_id.clone(), "chain writeback rollback")
        .expect("session should be creatable");
    let plan_store = magi_plan::PlanStore::from_store(&session_store, session_id.clone());
    let original_plan = plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("chain-writeback-step".to_string()),
                step: "验证 chain 写回失败回滚".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("plan should create");
    let request = rollback_test_request(session_id.clone(), UtcMillis(9_300), "executor");
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let pending = prepare_pending_dispatch_submission(&runtime, &request)
        .expect("pending dispatch should prepare");
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        pending.active_execution_chain,
        pending.turn,
    );
    let accepted = accept_dispatch_submission(
        &session_store,
        &task_store,
        &execution_registry,
        request.clone(),
        graph,
    )
    .expect("dispatch should be accepted");
    let prepared = validate_dispatch_request(&runtime, &request, false)
        .expect("accepted request should validate");
    let mut changed_chain = session_store
        .active_execution_chain(&session_id)
        .expect("accepted chain should exist");
    changed_chain.current_turn_id = Some("stale-materialization-turn".into());

    let error = match materialize_execution(&runtime, &request, &prepared, Some(changed_chain)) {
        Ok(_) => panic!("stale Turn binding must fail materialization"),
        Err(error) => error,
    };

    assert!(
        error
            .into_message()
            .contains("校验 materialize execution chain 失败")
    );
    assert!(execution_registry.get(&accepted.root_task_id).is_none());
    assert!(
        session_store
            .thread_registry_snapshot(&session_id)
            .is_empty(),
        "写回失败后 coordinator 和 worker thread 都必须回收"
    );
    let restored_plan = plan_store.snapshot().expect("plan should remain");
    assert_eq!(restored_plan.items, original_plan.items);
    assert!(
        !restored_plan
            .task_bindings
            .contains_key(&accepted.root_task_id)
    );
    assert!(restored_plan.revision > original_plan.revision);
    assert!(
        session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .flat_map(|turn| turn.items)
            .all(|item| !item.metadata.contains_key("forceWriteback")),
        "失败的 chain 写回不能污染 canonical projection"
    );
}

#[test]
fn rejected_dispatch_cleanup_does_not_remove_foreign_registry_generation() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let execution_registry = TaskExecutionRegistry::default();
    let event_bus = InMemoryEventBus::new(16);
    let agent_role_registry = AgentRoleRegistry::load_default();
    let session_id = SessionId::new("session-rejected-cleanup-generation");
    session_store
        .create_session(session_id.clone(), "rejected cleanup generation")
        .expect("session should be creatable");
    let request = rollback_test_request(session_id.clone(), UtcMillis(9_400), "executor");
    let runtime = DispatchSubmissionRuntime {
        session_store: &session_store,
        task_store: &task_store,
        execution_registry: &execution_registry,
        event_bus: &event_bus,
        agent_role_registry: &agent_role_registry,
        model_bridge_client: None,
        settings_store: None,
        workspace_root_path: None,
    };
    let pending = prepare_pending_dispatch_submission(&runtime, &request)
        .expect("pending dispatch should prepare");
    let prepared = validate_dispatch_request(&runtime, &request, false)
        .expect("pending request should validate");
    let task_id = pending.task.task_id.clone();
    execution_registry
        .insert(
            task_id.clone(),
            rollback_test_execution_plan(
                &session_id,
                &prepared.mission_id,
                &task_id,
                &prepared.worker_id,
                &prepared.worker_thread_id,
                "turn-foreign-generation",
            ),
        )
        .expect("foreign generation should register");
    let graph = DispatchSubmissionGraph::new(
        task_id.clone(),
        task_id.clone(),
        pending.active_execution_chain,
        pending.turn,
    );

    cleanup_rejected_dispatch(Some(&task_store), &execution_registry, graph)
        .expect("rejected dispatch facts should clean up");

    assert!(task_store.get_task(&task_id).is_none());
    assert_eq!(
        execution_registry.turn_id(&task_id).as_deref(),
        Some("turn-foreign-generation"),
        "拒绝旧 Turn 时不能删除同 task id 下的新执行代际"
    );
}
