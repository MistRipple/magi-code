use super::*;
use magi_agent_role::{AgentRole, AgentRoleRegistry, TaskKindLabel};
use magi_core::{
    AbsolutePath, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
    ExecutionOwnership, MissionId, Task, TaskKind, TaskStatus, WorkerId,
};
use magi_orchestrator::task_store::TaskLease;
use magi_session_store::{ActiveExecutionChain, ActiveExecutionDispatchContext};
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn background_turn_followup_survives_its_parent_work_returning() {
    let (outer_done, outer_received) = std::sync::mpsc::channel();
    let (finished, received) = std::sync::mpsc::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        super::spawn_session_turn_work("test-terminal-parent", async move {
            super::spawn_session_turn_work("test-queued-followup", async move {
                if released.await.is_ok() {
                    let _ = finished.send(());
                }
            });
            outer_done.send(()).unwrap();
        });
    })
    .join()
    .unwrap();
    outer_received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    // 模拟 finalizer 已返回，但后续队列任务仍等待 Session 提交锁。
    assert!(matches!(
        received.recv_timeout(std::time::Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).expect("父工作结束不能取消派生任务");
    received
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
}

#[test]
fn browser_capability_settings_default_to_safe_values_and_round_trip() {
    let defaults = BrowserCapabilitySettings::from_section(&serde_json::json!({}));
    assert!(defaults.in_app_browser_enabled);
    assert!(defaults.browser_use_enabled);
    assert!(!defaults.devtools_enabled, "开发者工具集默认关闭");
    assert!(!defaults.lan_access_enabled, "局域网访问默认关闭");

    let custom = BrowserCapabilitySettings {
        in_app_browser_enabled: false,
        browser_use_enabled: true,
        devtools_enabled: true,
        lan_access_enabled: true,
    };
    assert_eq!(
        BrowserCapabilitySettings::from_section(&custom.to_section()),
        custom
    );
    // 旧设置文件只有前两项：缺失的新开关取默认值，不丢已有设置。
    let legacy = BrowserCapabilitySettings::from_section(&serde_json::json!({
        "inAppBrowserEnabled": false,
        "browserUseEnabled": false,
    }));
    assert!(!legacy.in_app_browser_enabled && !legacy.browser_use_enabled);
    assert!(!legacy.devtools_enabled && !legacy.lan_access_enabled);
}

#[test]
fn prepared_role_delete_transaction_restores_role_and_binding_snapshot() {
    let state_root = tempfile::tempdir().expect("状态根应创建");
    let role_path = state_root.path().join("roles/restore-me.md");
    let role_contents = "---\nid: restore-me\nsupported_kinds: [local_agent]\n---\n恢复角色\n";
    std::fs::create_dir_all(role_path.parent().expect("角色目录应存在")).expect("角色目录应创建");
    std::fs::write(&role_path, role_contents).expect("角色文件应写入");

    let settings = SettingsStore::new();
    let previous_agents = serde_json::json!([{
        "templateId": "restore-me",
        "engineId": "engine-a",
        "bindingRevision": 3,
    }]);
    settings
        .set_section("agents", previous_agents.clone())
        .expect("绑定快照应写入");
    prepare_role_delete_transaction(
        state_root.path(),
        "restore-me",
        Some(role_contents.to_string()),
        Some(previous_agents.clone()),
    )
    .expect("删除事务应准备");

    std::fs::remove_file(&role_path).expect("应模拟角色文件删除");
    settings
        .set_section("agents", serde_json::json!([]))
        .expect("应模拟绑定删除");

    recover_role_delete_transaction(state_root.path(), &settings).expect("事务应恢复");
    assert_eq!(
        std::fs::read_to_string(&role_path).expect("角色文件应恢复"),
        role_contents
    );
    assert_eq!(settings.get_section("agents"), previous_agents);
    assert!(!role_delete_journal_path(state_root.path()).exists());
}

#[test]
fn committed_role_delete_transaction_keeps_deletion_and_only_clears_journal() {
    let state_root = tempfile::tempdir().expect("状态根应创建");
    let role_path = state_root.path().join("roles/committed.md");
    let role_contents = "---\nid: committed\nsupported_kinds: [local_agent]\n---\n已提交\n";
    std::fs::create_dir_all(role_path.parent().expect("角色目录应存在")).expect("角色目录应创建");
    std::fs::write(&role_path, role_contents).expect("角色文件应写入");

    let settings = SettingsStore::new();
    settings
        .set_section("agents", serde_json::json!([{"templateId": "committed"}]))
        .expect("绑定快照应写入");
    prepare_role_delete_transaction(
        state_root.path(),
        "committed",
        Some(role_contents.to_string()),
        Some(settings.get_section("agents")),
    )
    .expect("删除事务应准备");
    std::fs::remove_file(&role_path).expect("应模拟角色文件删除");
    settings.remove_section("agents").expect("应模拟绑定删除");
    mark_role_delete_transaction_committed(state_root.path()).expect("事务应提交");

    recover_role_delete_transaction(state_root.path(), &settings).expect("提交事务应收敛");
    assert!(!role_path.exists());
    assert!(settings.get_section("agents").is_null());
    assert!(!role_delete_journal_path(state_root.path()).exists());
}

#[test]
fn role_delete_transaction_rejects_invalid_role_id_before_touching_files() {
    let state_root = tempfile::tempdir().expect("状态根应创建");
    let journal = serde_json::json!({
        "role_id": "../outside",
        "role_file_contents": null,
        "agents_section": null,
        "phase": "prepared",
    });
    std::fs::write(
        role_delete_journal_path(state_root.path()),
        serde_json::to_vec(&journal).expect("事务应序列化"),
    )
    .expect("非法事务应写入测试文件");

    let error = recover_role_delete_transaction(state_root.path(), &SettingsStore::new())
        .expect_err("非法 role_id 必须拒绝恢复");
    assert!(error.contains("role_id"));
    assert!(role_delete_journal_path(state_root.path()).exists());
    assert!(!state_root.path().join("outside.md").exists());
}

fn git_fixture(path: &Path, args: &[&str]) {
    let output = magi_process::std_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("git fixture command should start");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn queued_turn_fixture(
    session_id: &SessionId,
    workspace_id: &WorkspaceId,
    queue_id: &str,
    accepted_at: u64,
) -> QueuedRegularSessionTurn {
    QueuedRegularSessionTurn {
        plugin_message: None,
        project_directories: None,
        request: SessionTurnRequestDto {
            plugin_command_id: None,
            source_folder_id: None,
            project_directories: None,
            desktop_browser_tools_allowed: false,
            session_id: Some(session_id.to_string()),
            scope: crate::dto::SessionScopeKindDto::Workspace,
            workspace_id: Some(workspace_id.to_string()),
            workspace_path: None,
            text: Some(format!("queued {queue_id}")),
            skill_name: None,
            locale: None,
            goal_mode: false,
            resume: false,
            images: Vec::new(),
            context_references: Vec::new(),
            browser_annotation_refs: Vec::new(),
            browser_node_selections: Vec::new(),
            access_profile: None,
            orchestrator_session_config: None,
            request_id: Some(format!("request-{queue_id}")),
            user_message_id: Some(format!("user-{queue_id}")),
            placeholder_message_id: Some(format!("assistant-{queue_id}")),
            steer_current_turn: false,
            expected_turn_id: None,
            replace_turn_id: None,
            interrupt_turn_id: None,
            command: None,
        },
        request_fingerprint: None,
        requested_workspace_id: Some(workspace_id.clone()),
        accepted_at: UtcMillis(accepted_at),
        route: SessionTurnRouteDto::Chat,
        task_title: None,
        execution_goal: None,
        required_tool_chain: Vec::new(),
        completion_contract: magi_core::TaskCompletionContract::default(),
        recovery_checkpoint: None,
        session_id: session_id.clone(),
        workspace_id: Some(workspace_id.clone()),
        queue_id: queue_id.to_string(),
        retry_count: 0,
    }
}

#[test]
fn session_turn_identity_normalization_is_stable_and_trims_client_values() {
    assert_eq!(
        normalize_session_turn_identity(
            Some("  request-client  ".to_string()),
            None,
            "queue-identity-1",
        ),
        (
            "request-client".to_string(),
            "turn-item-user-queue-identity-1".to_string(),
        )
    );
    assert_eq!(
        normalize_session_turn_identity(None, None, "queue-identity-1"),
        (
            "request-queue-identity-1".to_string(),
            "turn-item-user-queue-identity-1".to_string(),
        )
    );
}

#[tokio::test]
async fn session_execution_lifecycle_does_not_block_global_navigation() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let session_id = SessionId::new("session-navigation-isolation");
    let _execution_guard = state.lock_session_lifecycle(&session_id).await;

    // 执行生命周期可以等待同一 session 的 Turn/Runner，但不能占用全局
    // current 指针锁；否则任一后台 Turn 都会阻塞其他会话的切换。
    let navigation_guard =
        tokio::time::timeout(Duration::from_millis(50), state.lock_session_navigation())
            .await
            .expect("session execution lifecycle must not hold navigation lock");
    drop(navigation_guard);
}

#[test]
fn enqueue_regular_session_turn_persists_complete_identity_and_fingerprint() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let session_id = SessionId::new("session-queue-identity");
    let workspace_id = WorkspaceId::new("workspace-queue-identity");
    state
        .session_store
        .create_session(session_id.clone(), "identity")
        .unwrap();
    let mut queued = queued_turn_fixture(&session_id, &workspace_id, "queue-identity", 100);
    queued.request.request_id = None;
    queued.request.user_message_id = None;

    state
        .enqueue_regular_session_turn(queued)
        .expect("queued turn should persist");
    let restored = state
        .peek_next_regular_session_turn(&session_id)
        .expect("queued turn should remain available");
    assert_eq!(
        restored.request.request_id().as_deref(),
        Some("request-queue-identity")
    );
    assert_eq!(
        restored.request.user_message_id().as_deref(),
        Some("turn-item-user-queue-identity")
    );
    assert!(
        restored.request_fingerprint.is_some(),
        "队列状态必须同时持有可重放的完整请求指纹"
    );
}

#[test]
fn queued_turn_request_id_lookup_returns_stable_queue_position() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let session_id = SessionId::new("session-queued-request-lookup");
    let workspace_id = WorkspaceId::new("workspace-queued-request-lookup");
    state
        .session_store
        .create_session(session_id.clone(), "lookup")
        .unwrap();
    state
        .enqueue_regular_session_turn(queued_turn_fixture(
            &session_id,
            &workspace_id,
            "queue-first",
            10,
        ))
        .expect("首条队列消息应入队");
    state
        .enqueue_regular_session_turn(queued_turn_fixture(
            &session_id,
            &workspace_id,
            "queue-second",
            20,
        ))
        .expect("第二条队列消息应入队");

    let (queued, position) = state
        .queued_regular_session_turn_for_request_id("request-queue-second")
        .expect("应按 requestId 找到队列消息");
    assert_eq!(queued.queue_id, "queue-second");
    assert_eq!(position, 2);
    assert!(
        state
            .queued_regular_session_turn_for_request_id("missing-request")
            .is_none()
    );
}

#[test]
fn terminal_task_releases_undispatched_input_without_closing_sibling() {
    let store = Arc::new(TaskStore::new());
    let task = task_with_status("queued-agent-cancelled", TaskStatus::Killed);
    store.insert_task(task.clone()).unwrap();
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_task_store(store);
    let session = SessionId::new("queued-agent-session");
    let sibling = TaskId::new("other-agent");
    let signal = magi_conversation_runtime::RuntimeSignal {
        author: magi_conversation_runtime::MailboxAuthor::System,
        kind: magi_conversation_runtime::MailboxKind::Message,
        trigger_turn: true,
        payload: serde_json::json!({"type":"agent_assignment"}),
        enqueued_at: UtcMillis(1),
    };
    for id in [&task.task_id, &sibling] {
        state
            .conversation_registry
            .open_task_signal_channel(&session, id);
        state
            .conversation_registry
            .enqueue_task_signal(&session, id, signal.clone())
            .unwrap();
    }
    state.release_finished_task_resources(&session, &task.task_id);
    state.release_finished_task_resources(&session, &task.task_id);
    assert!(
        state
            .conversation_registry
            .drain_task_signals(&session, &task.task_id)
            .is_empty()
    );
    assert!(
        state
            .conversation_registry
            .enqueue_task_signal(&session, &task.task_id, signal.clone())
            .is_err()
    );
    assert_eq!(
        state
            .conversation_registry
            .drain_task_signals(&session, &sibling),
        [signal]
    );
    assert_eq!(state.conversation_registry.len(), 1);
}

#[test]
fn execution_resource_coordinator_revokes_browser_leases_by_subtree_scope() {
    run_browser_resource_scope(false);
}

#[test]
fn execution_resource_coordinator_preserves_transferred_browser_lease() {
    run_browser_resource_scope(true);
}

fn run_browser_resource_scope(transferred: bool) {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let workspace_id = WorkspaceId::new("workspace-resource-coordinator");
    let session_id = SessionId::new("session-resource-coordinator");
    let browser_session_id = BrowserSessionId::new("browser-session-resource-coordinator");
    let profile_id = BrowserProfileId::new("browser-profile-resource-coordinator");
    let tab_id = BrowserTabId::new("browser-tab-resource-coordinator");
    let lease_id = BrowserLeaseId::new("browser-lease-resource-coordinator");
    let retained_tab_id = BrowserTabId::new("browser-tab-resource-coordinator-retained");
    let retained_lease_id = BrowserLeaseId::new("browser-lease-resource-coordinator-retained");
    let completed_task_id = TaskId::new("task-resource-coordinator-completed");
    let retained_task_id = TaskId::new("task-resource-coordinator-retained");
    let now = UtcMillis(100);
    let store = Arc::new(TaskStore::new());
    let mut root = task_with_status("task-resource-coordinator-root", TaskStatus::Running);
    root.root_task_id = root.task_id.clone();
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    let mut child = task_with_status(completed_task_id.as_str(), TaskStatus::Running);
    child.parent_task_id = Some(root.task_id.clone());
    child.root_task_id = root.task_id.clone();
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child).unwrap();
    if transferred {
        insert_transferred_agent_fixture(
            &store,
            &root,
            &session_id,
            retained_task_id.as_str(),
            magi_core::AgentRunOwner::Session,
        );
    }
    let state = state.with_task_store(store);

    state
        .mutate_browser_authority(|authority| {
            authority.register_profile(magi_browser_authority::BrowserProfile {
                profile_id: profile_id.clone(),
                kind: magi_browser_authority::BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("browser profile fixture should create")
                    .keep(),
                created_at: now,
                updated_at: now,
            })?;
            authority.create_session(magi_browser_authority::CreateBrowserSession {
                browser_session_id: browser_session_id.clone(),
                owner: magi_browser_authority::BrowserSessionOwner::Session {
                    session_id: session_id.clone(),
                    workspace_id: Some(workspace_id.clone()),
                },
                profile_id: profile_id.clone(),
                now,
            })?;
            authority.transition_session(
                &browser_session_id,
                magi_browser_authority::BrowserSessionLifecycle::Ready,
                now,
            )?;
            authority.create_tab(magi_browser_authority::CreateBrowserTab {
                tab_id: tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "about:blank".to_string(),
                now,
            })?;
            authority.transition_tab(
                &tab_id,
                magi_browser_authority::BrowserTabLifecycle::Ready,
                now,
            )?;
            authority.set_primary_surface(
                magi_browser_authority::BrowserSurfaceBinding {
                    desktop_epoch: "desktop-resource-coordinator".to_string(),
                    window_id: "window-resource-coordinator".to_string(),
                    surface_id: "surface-resource-coordinator".to_string(),
                    surface_revision: 1,
                    tab_id: tab_id.clone(),
                    web_contents_id: 23,
                    target_id: "target-resource-coordinator".to_string(),
                    browser_context_id: "context-resource-coordinator".to_string(),
                    navigation_revision: 0,
                },
                now,
            )?;
            authority.acquire_lease(magi_browser_authority::AcquireBrowserLease {
                lease_id: lease_id.clone(),
                tab_id: tab_id.clone(),
                surface_id: "surface-resource-coordinator".to_string(),
                owner: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    task_id: Some(completed_task_id.clone()),
                    ..ExecutionOwnership::default()
                },
                turn_id: "turn-resource-coordinator".to_string(),
                goal_binding: None,
                acquired_at: now,
                expires_at: UtcMillis(1_000),
            })?;
            authority.create_tab(magi_browser_authority::CreateBrowserTab {
                tab_id: retained_tab_id.clone(),
                browser_session_id: browser_session_id.clone(),
                url: "https://example.com/retained".to_string(),
                now,
            })?;
            authority.transition_tab(
                &retained_tab_id,
                magi_browser_authority::BrowserTabLifecycle::Ready,
                now,
            )?;
            authority.set_primary_surface(
                magi_browser_authority::BrowserSurfaceBinding {
                    desktop_epoch: "desktop-resource-coordinator".to_string(),
                    window_id: "window-resource-coordinator".to_string(),
                    surface_id: "surface-resource-coordinator-retained".to_string(),
                    surface_revision: 1,
                    tab_id: retained_tab_id.clone(),
                    web_contents_id: 24,
                    target_id: "target-resource-coordinator-retained".to_string(),
                    browser_context_id: "context-resource-coordinator".to_string(),
                    navigation_revision: 0,
                },
                now,
            )?;
            authority.acquire_lease(magi_browser_authority::AcquireBrowserLease {
                lease_id: retained_lease_id.clone(),
                tab_id: retained_tab_id.clone(),
                surface_id: "surface-resource-coordinator-retained".to_string(),
                owner: ExecutionOwnership {
                    session_id: Some(session_id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                    task_id: Some(retained_task_id.clone()),
                    ..ExecutionOwnership::default()
                },
                turn_id: "turn-resource-coordinator-retained".to_string(),
                goal_binding: None,
                acquired_at: now,
                expires_at: UtcMillis(1_000),
            })?;
            Ok(())
        })
        .expect("browser resource fixture should initialize");

    let report = state.cancel_task_execution_scope_resources(
        &session_id,
        &root.task_id,
        magi_browser_authority::BrowserLeaseEndReason::TaskFinished,
    );
    assert_eq!(report.browser_lease_count, 1);
    assert_eq!(report.total(), 1);
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock should not poison");
    assert_eq!(
        authority.lease(&lease_id).map(|lease| lease.lifecycle),
        Some(magi_browser_authority::BrowserLeaseLifecycle::Revoked)
    );
    assert_eq!(
        authority
            .lease(&lease_id)
            .and_then(|lease| lease.end_reason),
        Some(magi_browser_authority::BrowserLeaseEndReason::TaskFinished)
    );
    assert_eq!(
        authority
            .lease(&retained_lease_id)
            .map(|lease| lease.lifecycle),
        Some(magi_browser_authority::BrowserLeaseLifecycle::Held)
    );
    let browser_session = authority
        .session(&browser_session_id)
        .expect("completed task must retain its browser session");
    assert_eq!(
        browser_session.lifecycle,
        magi_browser_authority::BrowserSessionLifecycle::Ready
    );
    assert!(browser_session.tab_ids.contains(&tab_id));
    let tab = authority
        .tab(&tab_id)
        .expect("completed task must retain its browser tab");
    assert_eq!(
        tab.lifecycle,
        magi_browser_authority::BrowserTabLifecycle::Ready
    );
    assert_eq!(tab.url, "about:blank");
    let retained_tab = authority
        .tab(&retained_tab_id)
        .expect("并行任务的 Browser Tab 必须继续保留");
    assert_eq!(
        retained_tab.lifecycle,
        magi_browser_authority::BrowserTabLifecycle::Ready
    );
    assert_eq!(retained_tab.url, "https://example.com/retained");
}

#[tokio::test]
async fn session_cleanup_preserves_inactive_dirty_agent_worktree_context() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let repository = fixture.path().join("repo");
    std::fs::create_dir_all(&repository).expect("repo directory");
    git_fixture(&repository, &["init", "-b", "main"]);
    git_fixture(&repository, &["config", "user.name", "Magi Test"]);
    git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
    std::fs::write(repository.join("README.md"), "base\n").expect("fixture file");
    git_fixture(&repository, &["add", "README.md"]);
    git_fixture(&repository, &["commit", "-m", "base"]);

    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let session_id = SessionId::new("session-dirty-agent-cleanup");
    let observation = state
        .git_service
        .observe(&repository)
        .await
        .expect("observe repository");
    let context = state.session_code_contexts.accept(
        session_id.as_str(),
        "workspace-dirty-agent-cleanup",
        vec![repository.clone()],
        &observation,
    );
    let worktree_path = fixture.path().join("agent-worktree");
    let created = state
        .git_service
        .worktree_create(
            &repository,
            magi_git::WorktreeCreateOptions {
                path: worktree_path.clone(),
                base: observation.head.clone().expect("base head"),
                branch: Some("magi/agent/dirty-cleanup".to_string()),
                create_branch: true,
                detached: false,
                precondition: context.precondition(),
            },
        )
        .await
        .expect("create agent worktree");
    state
        .session_code_contexts
        .register_agent_worktree(
            session_id.as_str(),
            magi_git::AgentWorktreeContext {
                task_id: "task-dirty-agent-cleanup".to_string(),
                lease_id: None,
                worker_id: "worker-dirty-agent-cleanup".to_string(),
                path: created.path,
                mode: magi_git::AgentWorktreeMode::Writable,
                base_head: observation.head.expect("base head"),
                branch: created.branch,
                active: true,
                result_head: None,
                changed_paths: Vec::new(),
                applied: false,
            },
        )
        .expect("register agent worktree");
    std::fs::write(worktree_path.join("uncommitted.txt"), "preserve me\n")
        .expect("dirty agent output");
    state
        .session_code_contexts
        .release_agent_worktree(session_id.as_str(), "task-dirty-agent-cleanup")
        .expect("release agent worktree");

    state.cleanup_session_git_resources(&session_id).await;

    assert!(worktree_path.join("uncommitted.txt").is_file());
    let retained = state
        .session_code_contexts
        .get(session_id.as_str())
        .expect("dirty worktree context must remain recoverable");
    assert!(!retained.agent_worktrees[0].active);
}

#[tokio::test]
async fn personal_session_delete_reclaims_only_safe_owned_git_worktrees() {
    for mode in ["clean", "dirty", "busy"] {
        let fixture = tempfile::tempdir().unwrap();
        let state_root = fixture.path().join("state");
        let state = ApiState::new(
            "personal-git-cleanup",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(browser_persistence_for_test(&state_root));
        let session = SessionId::new("personal-cleanup");
        state
            .session_store
            .create_session(session.clone(), "cleanup")
            .unwrap();
        let personal_root = state.personal_session_execution_root(&session).unwrap();
        std::fs::write(
            personal_root.join("result.txt"),
            "keep if cleanup is unsafe",
        )
        .unwrap();
        let repository = fixture.path().join("external-repository");
        std::fs::create_dir_all(&repository).unwrap();
        git_fixture(&repository, &["init", "-b", "main"]);
        git_fixture(&repository, &["config", "user.name", "Magi Test"]);
        git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
        git_fixture(&repository, &["commit", "--allow-empty", "-m", "base"]);
        let observation = state.git_service.observe(&repository).await.unwrap();
        state
            .session_code_contexts
            .accept(session.as_str(), "", vec![], &observation);
        state.persist_session_git_contexts().unwrap();
        let owned = state_root
            .join("worktrees/personal")
            .join(session.as_str())
            .join("owned");
        let other = state_root.join("worktrees/personal/other-session/retained");
        let unmanaged = fixture.path().join("user-worktree");
        for path in [&owned, &other, &unmanaged] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            state
                .git_service
                .worktree_create(
                    &repository,
                    magi_git::WorktreeCreateOptions {
                        path: path.clone(),
                        base: observation.head.clone().unwrap(),
                        branch: None,
                        create_branch: false,
                        detached: true,
                        precondition: magi_git::GitPrecondition::default(),
                    },
                )
                .await
                .unwrap();
        }
        if mode == "dirty" {
            std::fs::write(owned.join("uncommitted.txt"), "preserve me").unwrap();
        }
        if mode == "busy" {
            state
                .workspace_git_coordinator
                .begin_execution("other-session", &observation.git_common_dir)
                .unwrap();
        }

        state.delete_session_and_resources(&session).await.unwrap();

        assert!(state.session_store.session(&session).is_none());
        assert!(
            repository.join(".git").is_dir(),
            "external repository: {mode}"
        );
        assert!(other.join(".git").is_file(), "other session: {mode}");
        assert!(unmanaged.join(".git").is_file(), "user worktree: {mode}");
        let retained = mode != "clean";
        assert_eq!(owned.exists(), retained, "owned worktree: {mode}");
        assert_eq!(
            personal_root.join("result.txt").exists(),
            retained,
            "personal files: {mode}"
        );
        assert_eq!(
            state
                .session_code_contexts
                .get_repository(session.as_str(), &repository)
                .is_some(),
            retained
        );
        let persisted: Vec<magi_git::SessionCodeContext> = serde_json::from_slice(
            &std::fs::read(state_root.join("session-git-contexts.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted.len(),
            usize::from(retained),
            "recoverable baseline: {mode}"
        );
        state
            .workspace_git_coordinator
            .end_execution("other-session");
    }
}

#[cfg(unix)]
#[test]
fn task_subtree_resource_cancel_keeps_other_tasks_in_the_same_session_running() {
    run_task_scope_resource_cancellation(false, false);
}

#[cfg(unix)]
#[test]
fn task_scope_resource_cancel_preserves_transferred_shell_until_session_close() {
    run_task_scope_resource_cancellation(true, true);
}

#[cfg(unix)]
fn run_task_scope_resource_cancellation(transferred: bool, close_session: bool) {
    use magi_core::{AccessProfile, ExecutionResultStatus, ToolCallId};
    use magi_tool_runtime::{
        BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy,
    };
    let store = Arc::new(TaskStore::new());
    let mut root = task_with_status("resource-tree-root", TaskStatus::Running);
    root.root_task_id = root.task_id.clone();
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    let mut child = task_with_status("resource-tree-child", TaskStatus::Running);
    child.parent_task_id = Some(root.task_id.clone());
    child.root_task_id = root.task_id.clone();
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child.clone()).unwrap();
    let session = SessionId::new(format!(
        "resource-tree-session-{transferred}-{close_session}"
    ));
    let outside = TaskId::new("resource-tree-independent");
    if transferred {
        insert_transferred_agent_fixture(
            &store,
            &root,
            &session,
            outside.as_str(),
            magi_core::AgentRunOwner::Session,
        );
    }
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let governance = Arc::new(GovernanceService::default());
    let mut registry = ToolRegistry::new(governance.clone(), event_bus.clone());
    registry.register_default_builtins();
    let state = ApiState::new(
        "resource-tree-test",
        event_bus,
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        governance,
    )
    .with_task_store(store)
    .with_tool_registry(registry.clone());
    let fixture = tempfile::tempdir().unwrap();
    let release = fixture.path().join("release-shells");
    let mut runs = Vec::new();
    for (index, task) in [&root.task_id, &child.task_id, &outside]
        .into_iter()
        .enumerate()
    {
        // 独立执行目录，避免本测试被正确的并发写保护先行拒绝。
        let cwd = fixture.path().join(index.to_string());
        std::fs::create_dir(&cwd).unwrap();
        let context = ToolExecutionContext {
            session_id: Some(session.clone()),
            task_id: Some(task.clone()),
            workspace_id: Some(WorkspaceId::new("resource-tree-workspace")),
            working_directory: Some(cwd),
            access_profile: AccessProfile::FullAccess,
            ..Default::default()
        };
        let registry = registry.clone();
        let call = ToolCallId::new(format!("resource-tree-{index}"));
        runs.push(std::thread::spawn(move || registry.execute_with_policy(
                ToolExecutionInput::for_builtin_invocation(call, BuiltinToolName::ShellExec.as_str(),
                    serde_json::json!({"command":"while [ ! -f ../release-shells ]; do sleep 0.02; done; printf survives", "timeout_ms":10000}).to_string()),
                context, &ToolExecutionPolicy { access_profile: AccessProfile::FullAccess, ..Default::default() },
            )));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for task in [&root.task_id, &child.task_id, &outside] {
        let query = ToolExecutionContextQuery {
            session_id: Some(session.clone()),
            task_id: Some(task.clone()),
            ..Default::default()
        };
        while !registry.has_active_executions(&query) {
            if std::time::Instant::now() >= deadline {
                std::fs::write(&release, "release").unwrap();
                let results = runs
                    .into_iter()
                    .map(|run| run.join().unwrap())
                    .collect::<Vec<_>>();
                panic!("资源未按预期启动: {task}; {results:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    state.cancel_task_execution_scope_resources(
        &session,
        &root.task_id,
        BrowserLeaseEndReason::GoalPaused,
    );
    let retained = registry.has_active_executions(&ToolExecutionContextQuery {
        session_id: Some(session.clone()),
        task_id: Some(outside.clone()),
        ..Default::default()
    });
    if close_session {
        state.cancel_execution_resources(
            Some(&session),
            None,
            None,
            BrowserLeaseEndReason::SessionClosed,
        );
    }
    std::fs::write(release, "release").unwrap();
    let results = runs
        .into_iter()
        .map(|run| run.join().unwrap())
        .collect::<Vec<_>>();
    assert!(retained, "子树外进程不能被取消");
    for result in &results[..2] {
        assert_eq!(
            result.status,
            ExecutionResultStatus::Cancelled,
            "{result:?}"
        );
    }
    assert_eq!(
        results[2].status,
        if close_session {
            ExecutionResultStatus::Cancelled
        } else {
            ExecutionResultStatus::Succeeded
        },
        "{:?}",
        results[2]
    );
}

fn insert_transferred_agent_fixture(
    store: &TaskStore,
    root: &Task,
    session: &SessionId,
    id: &str,
    owner: magi_core::AgentRunOwner,
) -> Task {
    let mut child = task_with_status(id, TaskStatus::Running);
    child.root_task_id = root.task_id.clone();
    child.parent_task_id = Some(root.task_id.clone());
    let lifetime = match &owner {
        magi_core::AgentRunOwner::Session => magi_core::AgentLifetime::Session,
        magi_core::AgentRunOwner::Goal { goal_id, .. } => magi_core::AgentLifetime::Goal {
            goal_id: goal_id.clone(),
        },
    };
    let mut binding = magi_core::TaskExecutorBinding::for_role("executor");
    binding.agent_activation = Some(magi_core::AgentActivation {
        agent_id: child.task_id.clone(),
        session_id: session.clone(),
        lifetime,
        generation: 1,
        previous_task_id: None,
    });
    child.executor_binding = Some(binding);
    store.insert_task(child.clone()).unwrap();
    store
        .transfer_agent_subtree(
            &root.task_id,
            &child.task_id,
            magi_core::AgentRunTransfer {
                operation_id: format!("transfer-{id}"),
                session_id: session.clone(),
                source_turn_id: "resource-source".into(),
                transferred_by: root.task_id.clone(),
                activation_generation: 1,
                owner,
                transferred_at: UtcMillis::now(),
            },
        )
        .unwrap();
    store.get_task(&child.task_id).unwrap()
}

fn task_with_status(task_id: &str, status: TaskStatus) -> Task {
    let now = UtcMillis::now();
    Task {
        task_id: TaskId::new(task_id),
        mission_id: MissionId::new("mission-stable-waiting-state"),
        root_task_id: TaskId::new("task-root-stable-waiting-state"),
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: "等待确认".to_string(),
        goal: "等待用户确认后继续".to_string(),
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
        runtime_payload: magi_core::TaskRuntimePayload::default(),
        created_at: now,
        updated_at: now,
    }
}

struct RecordingDispatcher {
    observed_role: Arc<Mutex<Option<String>>>,
}

impl TaskDispatcher for RecordingDispatcher {
    fn dispatch(
        &self,
        _task: &Task,
        worker: &WorkerInfo,
        _lease: &TaskLease,
        _admission_permit: magi_conversation_runtime::execution_admission::ExecutionAdmissionPermit,
    ) -> Result<(), String> {
        *self
            .observed_role
            .lock()
            .expect("observed role lock should not poison") = Some(worker.role.clone());
        Ok(())
    }
}

struct CompletingDispatcher {
    completion_notifier: Arc<TaskCompletionNotifier>,
}

impl TaskDispatcher for CompletingDispatcher {
    fn dispatch(
        &self,
        task: &Task,
        _worker: &WorkerInfo,
        lease: &TaskLease,
        _admission_permit: magi_conversation_runtime::execution_admission::ExecutionAdmissionPermit,
    ) -> Result<(), String> {
        self.completion_notifier.notify(
            magi_conversation_runtime::task_runner_bridge::TaskResult {
                task_id: task.task_id.clone(),
                lease_id: lease.lease_id.clone(),
                outcome: magi_conversation_runtime::task_runner_bridge::TaskOutcome::Completed {
                    attempt: magi_core::TaskCompletionAttempt {
                        output_refs: vec!["runner checkpoint deduplication verified".to_string()],
                        final_response: Some(
                            "runner checkpoint deduplication verified".to_string(),
                        ),
                        evidence: Vec::new(),
                    },
                },
            },
        );
        Ok(())
    }
}

struct PanickingDispatcher;

impl TaskDispatcher for PanickingDispatcher {
    fn dispatch(
        &self,
        _task: &Task,
        _worker: &WorkerInfo,
        _lease: &TaskLease,
        _admission_permit: magi_conversation_runtime::execution_admission::ExecutionAdmissionPermit,
    ) -> Result<(), String> {
        panic!("模拟 Runner 派发 panic");
    }
}

fn test_agent_role(id: &str) -> AgentRole {
    AgentRole {
        id: id.to_string(),
        system_prompt: format!("{id} prompt"),
        display_name: id.to_string(),
        description: String::new(),
        supported_kinds: vec![TaskKindLabel::LocalAgent],
        parallelism_limit: None,
        coordinator_mode: false,
        version: 1,
        role_revision: 1,
        role: id.to_string(),
        focus: Vec::new(),
        constraints: Vec::new(),
        output_preferences: Vec::new(),
        ownerships: Vec::new(),
        insight_preferences: Vec::new(),
        capabilities: vec!["general_engineering".to_string()],
        color_token: format!("agent-{id}"),
        icon: "bot".to_string(),
    }
}

#[test]
fn runner_manager_uses_injected_agent_role_registry_for_worker_matching() {
    let store = Arc::new(TaskStore::new());
    let mut task = task_with_status("task-custom-agent-role", TaskStatus::Pending);
    task.root_task_id = task.task_id.clone();
    task.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("auditor"));
    let root_task_id = task.root_task_id.clone();
    store.insert_task(task).expect("任务应插入");

    let observed_role = Arc::new(Mutex::new(None));
    let dispatcher = Arc::new(RecordingDispatcher {
        observed_role: observed_role.clone(),
    });
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        Arc::new(SessionStore::new()),
        Arc::new(|| {
            vec![
                WorkerInfo {
                    worker_id: WorkerId::new("worker-executor"),
                    role: "executor".to_string(),
                    supported_kinds: vec![TaskKind::LocalAgent],
                    parallelism_limit: None,
                    system_prompt_template: None,
                },
                WorkerInfo {
                    worker_id: WorkerId::new("worker-auditor"),
                    role: "auditor".to_string(),
                    supported_kinds: vec![TaskKind::LocalAgent],
                    parallelism_limit: None,
                    system_prompt_template: None,
                },
            ]
        }),
        dispatcher,
    )
    .with_agent_role_registry(Arc::new(AgentRoleRegistry::from_map(HashMap::from([
        ("executor".to_string(), test_agent_role("executor")),
        ("auditor".to_string(), test_agent_role("auditor")),
    ]))));

    let outcome = manager.build_task_runner(None).run_cycle(&root_task_id);

    assert_eq!(outcome, RunCycleOutcome::Continue);
    assert_eq!(
        observed_role
            .lock()
            .expect("observed role lock should not poison")
            .as_deref(),
        Some("auditor")
    );
}

#[test]
fn task_worker_catalog_includes_custom_roles_and_internal_coordinator() {
    let role_dir = tempfile::tempdir().expect("角色目录应创建");
    let custom_registry =
        Arc::new(AgentRoleRegistry::builtin().with_user_role_dir(role_dir.path()));
    let mut custom_role = test_agent_role("data-analyst");
    custom_role.display_name = "数据分析师".to_string();
    custom_registry
        .save_user_role(custom_role, None)
        .expect("自定义角色应保存");
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::new()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_agent_role_registry(custom_registry);

    let catalog = state.task_worker_catalog();
    assert!(catalog.iter().any(|worker| worker.role == "data-analyst"));
    assert!(catalog.iter().any(|worker| worker.role == "executor"));
    assert!(catalog.iter().any(|worker| worker.role == "coordinator"));
}

#[test]
fn kill_tree_signals_active_runner_before_updating_task_tree() {
    let store = Arc::new(TaskStore::new());
    let mut root_task = task_with_status("task-runner-interrupt", TaskStatus::Running);
    root_task.root_task_id = root_task.task_id.clone();
    store.insert_task(root_task).expect("根任务应插入");
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store.clone(),
        Arc::new(SessionStore::new()),
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    manager.runners.lock().expect("runners should lock").insert(
        "task-runner-interrupt".to_string(),
        Arc::new(RunnerHandle {
            cancel: cancel.clone(),
            active,
            cycle_count: Arc::new(AtomicU64::new(1)),
            status: Arc::new(Mutex::new("running".to_string())),
            last_error: Arc::new(Mutex::new(None)),
            join_handle: Mutex::new(None),
        }),
    );

    manager
        .kill_tree("task-runner-interrupt")
        .expect("kill tree should succeed");

    assert!(cancel.load(Ordering::Relaxed));
    assert_eq!(
        manager
            .status("task-runner-interrupt")
            .expect("runner status should remain observable")
            .status,
        "killed"
    );
    assert_eq!(
        store
            .get_task(&TaskId::new("task-runner-interrupt"))
            .expect("root task should remain")
            .status,
        TaskStatus::Killed
    );
}

#[tokio::test]
async fn unbind_session_keeps_ownership_on_checkpoint_failure_and_retries_full_tree() {
    let store = Arc::new(TaskStore::new());
    let mut root = task_with_status("unbind-source", TaskStatus::Running);
    root.root_task_id = root.task_id.clone();
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    store.insert_task(root.clone()).unwrap();
    let session = SessionId::new("unbind-transferred-session");
    let child = insert_transferred_agent_fixture(
        &store,
        &root,
        &session,
        "unbind-transferred",
        magi_core::AgentRunOwner::Session,
    );
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store.clone(),
        Arc::new(SessionStore::new()),
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );
    manager.bind_session(session.clone(), root.task_id.as_str());
    manager.bind_session(session.clone(), child.task_id.as_str());
    store.set_checkpoint_callback(Box::new(|_| {
        Err(magi_core::DomainError::InvalidState {
            message: "unbind checkpoint failed".into(),
        })
    }));
    assert!(manager.unbind_session(&session).await.is_err());
    assert_eq!(
        store.get_task(&root.task_id).unwrap().status,
        TaskStatus::Running
    );
    assert_eq!(
        store.get_task(&child.task_id).unwrap().status,
        TaskStatus::Running
    );
    assert_eq!(
        manager.session_runner_index.lock().unwrap()[&session].len(),
        2
    );
    store.set_checkpoint_callback(Box::new(|_| Ok(())));
    assert_eq!(manager.unbind_session(&session).await.unwrap(), 2);
    assert!(
        !manager
            .session_runner_index
            .lock()
            .unwrap()
            .contains_key(&session)
    );
    assert_eq!(
        store.get_task(&root.task_id).unwrap().status,
        TaskStatus::Killed
    );
    assert_eq!(
        store.get_task(&child.task_id).unwrap().status,
        TaskStatus::Killed
    );
}

#[tokio::test]
async fn goal_control_settles_only_revoked_goal_agent_scopes() {
    for action in ["pause", "update", "clear", "failure_retry"] {
        let session_store = Arc::new(SessionStore::new());
        let session = SessionId::new(format!("goal-agent-control-{action}"));
        session_store
            .create_session(session.clone(), "goal controls")
            .unwrap();
        let (_, thread) = session_store.ensure_session_mission(&session, UtcMillis::now(), || {
            MissionId::new("goal-control-mission")
        });
        let goal = session_store
            .create_goal(
                session.clone(),
                thread,
                "resource-source",
                "控制目标代理",
                magi_core::AccessProfile::FullAccess,
            )
            .unwrap();
        let store = Arc::new(TaskStore::new());
        let mut root = task_with_status("goal-control-root", TaskStatus::Running);
        root.root_task_id = root.task_id.clone();
        root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
        store.insert_task(root.clone()).unwrap();
        let retained = insert_transferred_agent_fixture(
            &store,
            &root,
            &session,
            "goal-control-session",
            magi_core::AgentRunOwner::Session,
        );
        let revoked = insert_transferred_agent_fixture(
            &store,
            &root,
            &session,
            "goal-control-goal",
            magi_core::AgentRunOwner::Goal {
                goal_id: goal.goal_id.clone(),
                control_revision: goal.control_revision,
            },
        );
        let state = ApiState::new(
            "goal-agent-control",
            Arc::new(InMemoryEventBus::new(64)),
            session_store,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(store.clone());
        assert_eq!(
            state
                .settle_revoked_goal_agent_runs(&session)
                .await
                .unwrap(),
            0
        );
        if action == "failure_retry" {
            store.set_checkpoint_callback(Box::new(|_| {
                Err(magi_core::DomainError::InvalidState {
                    message: "goal checkpoint failed".into(),
                })
            }));
        }
        let request = serde_json::json!({ "sessionId": session, "scope":"personal", "goalId":goal.goal_id, "expectedRevision":goal.control_revision, "objective":"新目标" });
        let result = match action {
                "pause" => crate::goal_service::pause_current_goal(&state, serde_json::from_value(serde_json::json!({
                    "sessionId":session, "scope":"personal", "goalId":goal.goal_id, "expectedRevision":goal.control_revision
                })).unwrap()).await.map(|_| ()),
                "clear" => crate::goal_service::clear_current_goal(&state, serde_json::from_value(serde_json::json!({
                    "sessionId":session, "scope":"personal", "goalId":goal.goal_id, "expectedRevision":goal.control_revision
                })).unwrap()).await.map(|_| ()),
                _ => crate::goal_service::update_current_goal(&state, serde_json::from_value(request).unwrap()).await.map(|_| ()),
            };
        if action == "failure_retry" {
            assert!(result.is_err());
            assert_eq!(
                store.get_task(&revoked.task_id).unwrap().status,
                TaskStatus::Running
            );
            store.set_checkpoint_callback(Box::new(|_| Ok(())));
            state
                .settle_revoked_goal_agent_runs(&session)
                .await
                .unwrap();
        } else {
            result.unwrap();
        }
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            store.get_task(&retained.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            store.get_task(&revoked.task_id).unwrap().status,
            TaskStatus::Killed
        );
    }
}

#[tokio::test]
async fn unbind_session_waits_for_blocked_runner_and_removes_handle() {
    let store = Arc::new(TaskStore::new());
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        Arc::new(SessionStore::new()),
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );
    let session_id = SessionId::new("session-blocked-runner-cleanup");
    let root_task_id = "task-blocked-runner-cleanup";
    let cancel = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let background_cancel = cancel.clone();
    let background_active = active.clone();
    let join_handle = tokio::spawn(async move {
        while !background_cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        background_active.store(false, Ordering::Relaxed);
    });
    manager.runners.lock().expect("runners should lock").insert(
        root_task_id.to_string(),
        Arc::new(RunnerHandle {
            cancel,
            active,
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new("blocked".to_string())),
            last_error: Arc::new(Mutex::new(Some("等待输入".to_string()))),
            join_handle: Mutex::new(Some(join_handle)),
        }),
    );
    manager.bind_session(session_id.clone(), root_task_id);

    assert_eq!(manager.unbind_session(&session_id).await.unwrap(), 1);
    assert!(manager.status(root_task_id).is_none());
}

#[tokio::test]
async fn unbind_session_stops_active_chain_runner_before_session_binding_exists() {
    let store = Arc::new(TaskStore::new());
    let session_store = Arc::new(SessionStore::new());
    let session_id = SessionId::new("session-active-chain-runner-cleanup");
    let root_task_id = TaskId::new("task-active-chain-runner-cleanup");
    session_store
        .create_session(session_id.clone(), "active chain cleanup")
        .expect("session should create");
    session_store
        .seed_execution_chain_for_test(
            session_id.clone(),
            ActiveExecutionChain {
                session_id: session_id.clone(),
                mission_id: MissionId::new("mission-active-chain-runner-cleanup"),
                root_task_id: root_task_id.clone(),
                execution_chain_ref: "chain-active-runner-cleanup".to_string(),
                workspace_id: None,
                branches: Vec::new(),
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: UtcMillis::now(),
                    entry_id: "entry-active-runner-cleanup".to_string(),
                    trimmed_text: None,
                    skill_name: None,
                },
                current_turn_id: None,
            },
        )
        .expect("active chain should persist");
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        session_store,
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let background_cancel = cancel.clone();
    let join_handle = tokio::spawn(async move {
        while !background_cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    manager.runners.lock().expect("runners should lock").insert(
        root_task_id.to_string(),
        Arc::new(RunnerHandle {
            cancel,
            active: Arc::new(AtomicBool::new(true)),
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new("running".to_string())),
            last_error: Arc::new(Mutex::new(None)),
            join_handle: Mutex::new(Some(join_handle)),
        }),
    );

    assert_eq!(manager.unbind_session(&session_id).await.unwrap(), 1);
    assert!(manager.status(root_task_id.as_str()).is_none());
}

#[tokio::test]
async fn status_callback_and_restart_share_the_execution_scope_lock() {
    let store = Arc::new(TaskStore::new());
    let mut task = task_with_status("status-restart-root", TaskStatus::Failed);
    task.root_task_id = task.task_id.clone();
    task.parent_task_id = None;
    store.insert_task(task.clone()).unwrap();
    let manager = Arc::new(RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        Arc::new(SessionStore::new()),
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    ));
    let callback_manager = manager.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let callback = tokio::task::spawn_blocking(move || {
        let _guard = callback_manager.lock_for_task_status_change(&task);
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    entered_rx.await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            manager.lock_for_restart("status-restart-root")
        )
        .await
        .is_err(),
        "已通过版本校验的回调结束前不能重开执行"
    );
    release_tx.send(()).unwrap();
    callback.await.unwrap();
    let restart = manager.lock_for_restart("status-restart-root").await;
    let callback_manager = manager.clone();
    let (entered_tx, mut entered_rx) = tokio::sync::oneshot::channel();
    let callback = tokio::task::spawn_blocking(move || {
        let task = callback_manager
            .task_store
            .get_task(&TaskId::new("status-restart-root"))
            .unwrap();
        let _guard = callback_manager.lock_for_task_status_change(&task);
        let _ = entered_tx.send(());
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut entered_rx)
            .await
            .is_err(),
        "恢复提交期间旧回调必须等待，之后才能核对最新代际"
    );
    drop(restart);
    entered_rx.await.unwrap();
    callback.await.unwrap();
}

#[tokio::test]
async fn quiesce_runner_waits_for_exit_and_removes_handle_before_restart() {
    let store = Arc::new(TaskStore::new());
    let root_task_id = "task-runner-restart-race";
    let mut root_task = task_with_status(root_task_id, TaskStatus::Running);
    root_task.root_task_id = root_task.task_id.clone();
    store.insert_task(root_task).expect("根任务应插入");
    let session_store = Arc::new(SessionStore::new());
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        session_store,
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let background_cancel = cancel.clone();
    let background_active = active.clone();
    let join_handle = tokio::spawn(async move {
        while !background_cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        background_active.store(false, Ordering::Relaxed);
    });
    manager.runners.lock().expect("runners should lock").insert(
        root_task_id.to_string(),
        Arc::new(RunnerHandle {
            cancel,
            active: active.clone(),
            cycle_count: Arc::new(AtomicU64::new(1)),
            status: Arc::new(Mutex::new("error".to_string())),
            last_error: Arc::new(Mutex::new(Some("旧执行轮即将退出".to_string()))),
            join_handle: Mutex::new(Some(join_handle)),
        }),
    );

    let _restart_guard = manager.lock_for_restart(root_task_id).await;
    manager.quiesce_for_restart(root_task_id).await;

    assert!(!active.load(Ordering::Relaxed));
    assert!(manager.status(root_task_id).is_none());
    assert!(manager.start_after_quiesce(root_task_id, None).is_ok());
}

#[tokio::test]
async fn quiesce_runner_cleans_handle_when_old_join_panics() {
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        Arc::new(TaskStore::new()),
        Arc::new(SessionStore::new()),
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );
    let root_task_id = "task-runner-join-panic";
    let join_handle = tokio::spawn(async move {
        panic!("模拟旧 runner panic");
    });
    manager.runners.lock().expect("runners should lock").insert(
        root_task_id.to_string(),
        Arc::new(RunnerHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(true)),
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new("running".to_string())),
            last_error: Arc::new(Mutex::new(None)),
            join_handle: Mutex::new(Some(join_handle)),
        }),
    );

    manager.quiesce_for_restart(root_task_id).await;

    assert!(manager.status(root_task_id).is_none());
}

#[tokio::test]
async fn runner_start_rejects_archived_session() {
    let store = Arc::new(TaskStore::new());
    let root_task_id = "task-archived-session-runner";
    let mut root_task = task_with_status(root_task_id, TaskStatus::Pending);
    root_task.root_task_id = root_task.task_id.clone();
    store.insert_task(root_task).expect("根任务应插入");
    let session_store = Arc::new(SessionStore::new());
    let session_id = SessionId::new("session-archived-runner");
    session_store
        .create_session(session_id.clone(), "archived")
        .expect("session should create");
    session_store
        .archive_session(&session_id)
        .expect("session should archive");
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        session_store,
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    );

    assert!(matches!(
        manager.start(root_task_id, Some(session_id)).await,
        Err(RunnerStartError::SessionUnavailable)
    ));
    assert!(manager.status(root_task_id).is_none());
}

#[tokio::test]
async fn runner_panic_fails_root_task_and_stops_with_error() {
    let store = Arc::new(TaskStore::new());
    let root_task_id = "task-runner-cycle-panic";
    let mut root_task = task_with_status(root_task_id, TaskStatus::Pending);
    root_task.root_task_id = root_task.task_id.clone();
    store.insert_task(root_task).expect("根任务应插入");
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store.clone(),
        Arc::new(SessionStore::new()),
        Arc::new(|| {
            vec![WorkerInfo {
                worker_id: WorkerId::new("worker-runner-cycle-panic"),
                role: "executor".to_string(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }]
        }),
        Arc::new(PanickingDispatcher),
    );

    let handle = manager
        .start_after_quiesce(root_task_id, None)
        .expect("runner should start");
    for _ in 0..20 {
        if !handle.active.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(!handle.active.load(Ordering::Relaxed));
    assert_eq!(
        store
            .get_task(&TaskId::new(root_task_id))
            .expect("root task should remain available")
            .status,
        TaskStatus::Failed
    );
    let failure_outputs = store
        .get_task(&TaskId::new(root_task_id))
        .expect("root task should remain available")
        .output_refs;
    assert_eq!(failure_outputs.len(), 1);
    assert!(failure_outputs[0].contains("任务 Runner 执行线程异常退出"));
    assert!(failure_outputs[0].contains("模拟 Runner 派发 panic"));
    assert_eq!(
        manager
            .status(root_task_id)
            .expect("runner status should remain inspectable")
            .status,
        "error"
    );
}

#[tokio::test]
async fn runner_does_not_repeat_full_checkpoint_after_task_store_checkpoint() {
    let store = Arc::new(TaskStore::new());
    let task_id = TaskId::new("task-runner-checkpoint-dedup");
    let mut task = task_with_status(task_id.as_str(), TaskStatus::Pending);
    task.root_task_id = task_id.clone();
    store.insert_task(task).expect("pending root should insert");

    let task_store_checkpoint_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let checkpoint_count_for_callback = task_store_checkpoint_count.clone();
    store.set_checkpoint_callback(Box::new(move |_| {
        checkpoint_count_for_callback.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    // 让 root 使用生产默认的 `turn` checkpoint 策略。
    store
        .update_status_checked(&task_id, TaskStatus::Pending)
        .expect("pending policy should freeze");

    let completion_notifier = Arc::new(TaskCompletionNotifier::new(store.clone()));
    let runner_checkpoint_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runner_checkpoint_count_for_callback = runner_checkpoint_count.clone();
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store.clone(),
        Arc::new(SessionStore::new()),
        Arc::new(|| {
            vec![WorkerInfo {
                worker_id: WorkerId::new("worker-runner-checkpoint-dedup"),
                role: "executor".to_string(),
                supported_kinds: vec![TaskKind::LocalAgent],
                parallelism_limit: None,
                system_prompt_template: None,
            }]
        }),
        Arc::new(CompletingDispatcher {
            completion_notifier,
        }),
    )
    .with_checkpoint_persist(Arc::new(move |_| {
        runner_checkpoint_count_for_callback.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));

    let handle = manager
        .start(task_id.as_str(), None)
        .await
        .expect("runner should start");
    for _ in 0..100 {
        if !handle.active.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(!handle.active.load(Ordering::SeqCst));
    assert_eq!(
        store
            .get_task(&task_id)
            .expect("completed root should remain")
            .status,
        TaskStatus::Completed
    );
    assert_eq!(
        runner_checkpoint_count.load(Ordering::SeqCst),
        0,
        "TaskStore callback 已完成 turn checkpoint，Runner 不应再写全量快照"
    );
    assert!(
        task_store_checkpoint_count.load(Ordering::SeqCst) >= 3,
        "租约和终态变更必须仍由 TaskStore callback 持久化"
    );
}

#[tokio::test]
async fn runner_checkpoint_failure_never_publishes_completed_terminal_status() {
    let store = Arc::new(TaskStore::new());
    let root_task_id = "task-runner-checkpoint-failure";
    let mut root_task = task_with_status(root_task_id, TaskStatus::Completed);
    root_task.root_task_id = root_task.task_id.clone();
    store
        .insert_task(root_task)
        .expect("completed root should insert before runner starts");
    let manager = RunnerManager::with_dispatcher_and_worker_catalog(
        store,
        Arc::new(SessionStore::new()),
        Arc::new(Vec::new),
        Arc::new(RecordingDispatcher {
            observed_role: Arc::new(Mutex::new(None)),
        }),
    )
    .with_checkpoint_persist(Arc::new(|_| {
        Err(DomainError::Persistence {
            message: "checkpoint unavailable".to_string(),
        })
    }));

    let handle = manager
        .start(root_task_id, None)
        .await
        .expect("runner should start");
    for _ in 0..50 {
        if !handle.active.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(!handle.active.load(Ordering::Relaxed));
    assert_eq!(
        handle.status.lock().expect("status should lock").as_str(),
        "error"
    );
    assert!(
        handle
            .last_error
            .lock()
            .expect("last error should lock")
            .as_deref()
            .is_some_and(|error| error.contains("checkpoint 持久化失败"))
    );
}

#[test]
fn builtin_tools_json_does_not_assume_missing_runtime_status_ready() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let catalog = serde_json::json!({
        "tools": [
            {
                "name": "file_read",
                "public": true,
                "runtime_status": "ready"
            },
            {
                "name": "tool_catalog",
                "public": true
            },
            {
                "name": "shell_exec",
                "public": true,
                "runtime_status": " "
            }
        ]
    });

    let tools = state.builtin_tools_json(&catalog);
    let tools = tools.as_array().expect("builtin tools should be an array");

    assert_eq!(tools[0]["runtimeStatus"], serde_json::json!("ready"));
    assert_eq!(tools[1]["runtimeStatus"], serde_json::json!("unknown"));
    assert_eq!(tools[2]["runtimeStatus"], serde_json::json!("unknown"));
}

fn browser_persistence_for_test(state_root: &Path) -> Arc<RuntimeStatePersistence> {
    Arc::new(RuntimeStatePersistence::new(
        state_root,
        state_root.join("workspaces.json"),
        state_root.join("knowledge.json"),
    ))
}

fn state_for_browser_persistence_test(state_root: &Path) -> ApiState {
    ApiState::new(
        "magi-browser-persistence-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(browser_persistence_for_test(state_root))
}

#[test]
fn corrupted_or_unknown_browser_durable_state_is_rebuilt_and_overwritten() {
    for (name, contents) in [
        (
            "corrupted",
            br##"{"schema_version":7,"profiles":["##.to_vec(),
        ),
        (
            "unknown-version",
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 999,
                "revision": 12,
                "profiles": [],
                "sessions": [],
                "tabs": [],
                "annotations": []
            }))
            .expect("unknown browser schema should serialize"),
        ),
        (
            "unknown-field",
            serde_json::to_vec(&serde_json::json!({
                "schema_version": magi_browser_authority::BROWSER_DURABLE_STATE_SCHEMA_VERSION,
                "revision": 12,
                "profiles": [],
                "sessions": [],
                "tabs": [],
                "annotations": [],
                "future_field": true
            }))
            .expect("unknown browser field should serialize"),
        ),
    ] {
        let root = tempfile::tempdir().expect("browser state root should create");
        let browser_path = root.path().join("browser/state.json");
        fs::create_dir_all(browser_path.parent().expect("browser parent should exist"))
            .expect("browser directory should create");
        fs::write(&browser_path, contents).expect("fixture should write");

        let state = state_for_browser_persistence_test(root.path());
        let persisted: BrowserDurableState = serde_json::from_slice(
            &fs::read(&browser_path).expect("rebuilt browser state should be written"),
        )
        .unwrap_or_else(|error| panic!("{name}: rebuilt browser state should parse: {error}"));

        assert_eq!(
            persisted.schema_version,
            magi_browser_authority::BROWSER_DURABLE_STATE_SCHEMA_VERSION,
            "{name}: rebuilt state must use the current schema"
        );
        assert!(
            persisted.sessions.is_empty(),
            "{name}: state must rebuild empty"
        );
        assert_ne!(
            state.browser_host_status().status,
            BrowserHostStatus::Failed,
            "{name}: browser recovery must not enter Failed"
        );
    }
}

#[tokio::test]
async fn app_browser_session_is_excluded_from_magi_close_and_reconciliation() {
    let state = ApiState::new(
        "magi-browser-owner-scope-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let now = UtcMillis(200);
    let profile_id = BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID);
    let app_session_id = magi_core::BrowserSessionId::new("browser-session-app-owner-scope");
    let orphan_session_id = SessionId::new("session-browser-owner-scope-orphan");
    let orphan_browser_session_id =
        magi_core::BrowserSessionId::new("browser-session-magi-owner-scope-orphan");

    state
        .mutate_browser_authority(|authority| {
            authority.register_profile(BrowserProfile {
                profile_id: profile_id.clone(),
                kind: BrowserProfileKind::ManagedDefault,
                data_path: tempfile::tempdir()
                    .expect("browser profile fixture should create")
                    .keep(),
                created_at: now,
                updated_at: now,
            })?;
            authority.create_session(magi_browser_authority::CreateBrowserSession {
                browser_session_id: app_session_id.clone(),
                owner: magi_browser_authority::BrowserSessionOwner::App,
                profile_id: profile_id.clone(),
                now,
            })?;
            authority.transition_session(&app_session_id, BrowserSessionLifecycle::Ready, now)?;
            authority.create_session(magi_browser_authority::CreateBrowserSession {
                browser_session_id: orphan_browser_session_id.clone(),
                owner: magi_browser_authority::BrowserSessionOwner::Session {
                    session_id: orphan_session_id.clone(),
                    workspace_id: None,
                },
                profile_id,
                now,
            })?;
            authority.transition_session(
                &orphan_browser_session_id,
                BrowserSessionLifecycle::Ready,
                now,
            )?;
            Ok(())
        })
        .expect("app and Magi browser sessions should create");

    assert_eq!(
        state
            .reconcile_browser_sessions_with_session_store()
            .expect("reconciliation should succeed"),
        1,
        "only the orphaned Magi-owned session should reconcile"
    );
    {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority
                .app_session()
                .expect("app session must survive reconciliation")
                .lifecycle,
            BrowserSessionLifecycle::Ready
        );
        assert_eq!(
            authority
                .session(&orphan_browser_session_id)
                .expect("orphan session should remain as a closed record")
                .lifecycle,
            BrowserSessionLifecycle::Closed
        );
    }

    assert!(
        state
            .close_browser_session_for_magi_session(&SessionId::new("session-app-owner-scope"))
            .await
            .expect("closing an unrelated Magi session should succeed")
            .is_none(),
        "App-owned browser session must not match Magi session close"
    );
    assert_eq!(
        state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold")
            .app_session()
            .expect("app session must survive close")
            .lifecycle,
        BrowserSessionLifecycle::Ready
    );
}

#[tokio::test]
async fn session_isolation_round_trips_and_preserves_missing_copy_binding() {
    let root = tempfile::tempdir().expect("state root");
    let alive_copy = tempfile::tempdir().expect("isolated copy");
    let persistence = || {
        Arc::new(RuntimeStatePersistence::new(
            root.path(),
            root.path().join("workspaces.json"),
            root.path().join("knowledge.json"),
        ))
    };
    let new_state = || {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence())
    };
    let isolation = |session: &str, copy: &Path| magi_session_isolation::SessionIsolation {
        session_id: session.to_string(),
        workspace_id: "workspace-isolation-persist".to_string(),
        source_root: PathBuf::from("/repo"),
        root: copy.to_path_buf(),
        origin: magi_session_isolation::IsolationOrigin::Manual,
        strategy: magi_session_isolation::CloneStrategy::Clone,
        linked_dirs: Vec::new(),
        git_available: true,
        created_at_ms: 1,
    };
    let state = new_state();
    state
        .session_isolations
        .insert(isolation("alive-session", alive_copy.path()));
    let gone_copy = root.path().join("gone-copy");
    state
        .session_isolations
        .insert(isolation("gone-session", &gone_copy));
    state
        .persist_session_isolations()
        .expect("persist session isolations");

    let reloaded = new_state();
    assert!(reloaded.session_isolations.contains("alive-session"));
    assert!(
        reloaded.session_isolations.contains("gone-session"),
        "副本目录已经不存在时必须保留绑定，阻止请求回落原目录"
    );
    assert_eq!(
        reloaded.snapshot_root_for_session(&SessionId::new("gone-session"), Path::new("/repo")),
        gone_copy
    );
    assert!(
        reloaded
            .ensure_snapshot_session(&SessionId::new("gone-session"), Path::new("/repo"))
            .await
            .is_err(),
        "快照准备不能为丢失副本新建空目录"
    );
    assert!(!gone_copy.exists());
    assert_eq!(
        reloaded.snapshot_root_for_session(&SessionId::new("alive-session"), Path::new("/repo")),
        alive_copy.path(),
        "隔离会话的变更账本取副本路径"
    );
    assert_eq!(
        reloaded.snapshot_root_for_session(&SessionId::new("other-session"), Path::new("/repo")),
        Path::new("/repo")
    );
}

#[test]
fn orphan_session_isolations_are_reclaimed_after_restart() {
    let root = tempfile::tempdir().expect("state root");
    let persistence = Arc::new(RuntimeStatePersistence::new(
        root.path(),
        root.path().join("workspaces.json"),
        root.path().join("knowledge.json"),
    ));
    let session_store = Arc::new(SessionStore::default());
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::clone(&session_store),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence);
    let isolations_dir = root.path().join("session-isolations");
    let make_copy = |session: &str| {
        let dir = isolations_dir.join(session).join("project");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), "x").unwrap();
        dir
    };
    let register = |session: &str, copy: &Path| {
        state
            .session_isolations
            .insert(magi_session_isolation::SessionIsolation {
                session_id: session.to_string(),
                workspace_id: "ws".to_string(),
                source_root: PathBuf::from("/repo"),
                root: copy.to_path_buf(),
                origin: magi_session_isolation::IsolationOrigin::Manual,
                strategy: magi_session_isolation::CloneStrategy::Clone,
                linked_dirs: Vec::new(),
                git_available: false,
                created_at_ms: 1,
            });
    };
    // 会话还在：保留。
    session_store
        .create_session(SessionId::new("alive-session"), "alive")
        .unwrap();
    let alive = make_copy("alive-session");
    register("alive-session", &alive);
    // 会话已经不存在：登记和目录都回收。
    let deleted = make_copy("deleted-session");
    register("deleted-session", &deleted);
    // 目录在、登记里没有：当作崩溃残留回收。
    let leftover = make_copy("leftover-session");

    assert_eq!(state.reclaim_orphan_session_isolations(), 2);
    assert!(state.session_isolations.contains("alive-session"));
    assert!(!state.session_isolations.contains("deleted-session"));
    // 目录在后台线程里删除。
    for _ in 0..100 {
        if !deleted.exists() && !leftover.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(alive.exists(), "仍在使用的副本不能被回收");
    assert!(!deleted.exists());
    assert!(!leftover.exists());
}

#[test]
fn session_git_context_round_trips_through_runtime_persistence() {
    let root = tempfile::tempdir().expect("state root");
    let persistence = || {
        Arc::new(RuntimeStatePersistence::new(
            root.path(),
            root.path().join("workspaces.json"),
            root.path().join("knowledge.json"),
        ))
    };
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    state.session_code_contexts.accept(
        "session-git-persist",
        "workspace-git-persist",
        vec![PathBuf::from("/repo")],
        &magi_git::GitObservation {
            repository_root: PathBuf::from("/repo"),
            git_common_dir: PathBuf::from("/repo/.git"),
            worktree_path: PathBuf::from("/repo"),
            worktree_git_dir: PathBuf::from("/repo/.git"),
            branch: Some("main".to_string()),
            head: Some("abc123".to_string()),
            upstream: Some("origin/main".to_string()),
            origin_url: Some("https://example.test/repo.git".to_string()),
            ahead: 0,
            behind: 0,
            dirty: magi_git::GitDirtySummary::default(),
        },
    );
    state
        .persist_session_git_contexts()
        .expect("persist Git context");

    let reloaded = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    let context = reloaded
        .session_code_contexts
        .get("session-git-persist")
        .expect("reloaded Git context");
    assert_eq!(context.git.desired_ref.as_deref(), Some("main"));
    assert_eq!(context.git.base_head.as_deref(), Some("abc123"));
    assert_eq!(context.context_revision, 1);
}

#[test]
fn session_input_projection_survives_api_recreation_and_retry_updates() {
    let root = tempfile::tempdir().expect("state root");
    let persistence = || {
        Arc::new(RuntimeStatePersistence::new(
            root.path(),
            root.path().join("workspaces.json"),
            root.path().join("knowledge.json"),
        ))
    };
    let session_store = Arc::new(SessionStore::default());
    let session_id = SessionId::new("session-turn-queue-persistence");
    let workspace_id = WorkspaceId::new("workspace-turn-queue-persistence");
    session_store
        .create_session(session_id.clone(), "queue persistence")
        .expect("session should create");
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::clone(&session_store),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    let source_root = root.path().join("source");
    fs::create_dir_all(&source_root).unwrap();
    state
        .workspace_registry
        .register_native_path(workspace_id.clone(), source_root)
        .unwrap();
    let binding = state
        .workspace_registry
        .capture_directory_binding(&workspace_id, None, None)
        .unwrap();
    let mut first = queued_turn_fixture(&session_id, &workspace_id, "queue-first", 11);
    first.project_directories = Some(binding.clone());
    first.request.project_directories = Some(binding.clone());
    state
        .enqueue_regular_session_turn(first)
        .expect("first turn should persist");
    state
        .enqueue_regular_session_turn(queued_turn_fixture(
            &session_id,
            &workspace_id,
            "queue-second",
            12,
        ))
        .expect("second turn should persist");

    let restored = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::clone(&session_store),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    assert_eq!(restored.restore_regular_session_turn_queues().unwrap(), 2);
    assert_eq!(restored.queued_regular_session_turns(&session_id).len(), 2);
    let restored_first = restored
        .peek_next_regular_session_turn(&session_id)
        .unwrap();
    assert_eq!(restored_first.project_directories, Some(binding));
    assert!(
        restored_first.request.project_directories.is_none(),
        "请求 JSON 不接受服务端授权；授权只在原 Input payload 保存"
    );
    assert!(
        restored
            .remove_regular_session_turn(&session_id, "queue-second")
            .expect("queued turn removal should persist")
    );
    assert_eq!(restored.queued_regular_session_turns(&session_id).len(), 1);
    restored
        .enqueue_regular_session_turn(queued_turn_fixture(
            &session_id,
            &workspace_id,
            "queue-third",
            13,
        ))
        .expect("new input should be enqueued for retry coverage");
    assert_eq!(
        restored
            .peek_next_regular_session_turn(&session_id)
            .expect("restored queue should have a head")
            .queue_id,
        "queue-first"
    );
    restored
        .remove_regular_session_turn(&session_id, "queue-first")
        .expect("acknowledgement should persist");
    assert_eq!(
        restored
            .record_regular_session_turn_retry(&session_id, "queue-third")
            .expect("retry should persist"),
        1
    );

    let reloaded = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        session_store,
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    assert_eq!(reloaded.restore_regular_session_turn_queues().unwrap(), 1);
    let remaining = reloaded
        .peek_next_regular_session_turn(&session_id)
        .expect("second turn should remain persisted");
    assert_eq!(remaining.queue_id, "queue-third");
    assert_eq!(remaining.retry_count, 1);
}

#[test]
fn session_input_restore_cancels_archived_session_pending_entries() {
    let root = tempfile::tempdir().expect("state root");
    let persistence = || {
        Arc::new(RuntimeStatePersistence::new(
            root.path(),
            root.path().join("workspaces.json"),
            root.path().join("knowledge.json"),
        ))
    };
    let session_store = Arc::new(SessionStore::default());
    let session_id = SessionId::new("session-turn-queue-archived");
    let workspace_id = WorkspaceId::new("workspace-turn-queue-archived");
    session_store
        .create_session(session_id.clone(), "archived queue")
        .expect("session should create");
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::clone(&session_store),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    state
        .enqueue_regular_session_turn(queued_turn_fixture(
            &session_id,
            &workspace_id,
            "queue-archived",
            21,
        ))
        .expect("queued turn should persist");
    session_store
        .archive_session(&session_id)
        .expect("session should archive");

    let restored = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        session_store,
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(persistence());
    assert_eq!(restored.restore_regular_session_turn_queues().unwrap(), 0);
    assert!(!root.path().join("session-turn-queue.json").exists());
    assert_eq!(
        restored.session_store.inputs_for_session(&session_id)[0].state,
        magi_session_store::SessionInputState::Cancelled
    );
}

#[test]
fn legacy_queue_migration_keeps_backup_and_does_not_recreate_cancelled_input() {
    let root = tempfile::tempdir().unwrap();
    let session = SessionId::new("session-legacy-input");
    let workspace = WorkspaceId::new("workspace-legacy-input");
    let store = Arc::new(SessionStore::new());
    store.create_session(session.clone(), "legacy").unwrap();
    let legacy = vec![queued_turn_fixture(
        &session,
        &workspace,
        "queue-legacy",
        50,
    )];
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(root.path().join("session-turn-queue.json"), &bytes).unwrap();
    let state = ApiState::new(
        "test",
        Arc::new(InMemoryEventBus::new(16)),
        store.clone(),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
        root.path(),
        root.path().join("workspaces.json"),
        root.path().join("knowledge.json"),
    )));
    assert_eq!(state.restore_regular_session_turn_queues().unwrap(), 1);
    assert_eq!(
        fs::read(root.path().join("session-turn-queue.migrated.json")).unwrap(),
        bytes
    );
    assert!(!root.path().join("session-turn-queue.json").exists());
    state
        .remove_regular_session_turn(&session, "queue-legacy")
        .unwrap();
    // 模拟 rename 前崩溃遗留旧文件；已迁移的终态输入不能被旧文件复活。
    fs::write(root.path().join("session-turn-queue.json"), bytes).unwrap();
    assert_eq!(state.restore_regular_session_turn_queues().unwrap(), 0);
    assert_eq!(store.inputs_for_session(&session).len(), 1);
    assert_eq!(
        store.inputs_for_session(&session)[0].state,
        magi_session_store::SessionInputState::Cancelled
    );
}

#[tokio::test]
async fn snapshot_lifecycle_install_defers_historical_sessions_until_access() {
    let session_store = Arc::new(SessionStore::default());
    let workspace_store = Arc::new(WorkspaceStore::default());
    let governance = Arc::new(GovernanceService::default());
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let workspace_root =
        std::env::temp_dir().join(format!("magi-api-snapshot-replay-{}", UtcMillis::now().0));
    std::fs::create_dir_all(&workspace_root).expect("workspace root should create");
    let registered_workspace_id = WorkspaceId::new("workspace-snapshot-replay-known");
    workspace_store
        .register(
            registered_workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().as_ref()),
        )
        .expect("workspace should register");
    let known_session_id = SessionId::new("session-snapshot-replay-known");
    let orphan_session_id = SessionId::new("session-snapshot-replay-orphan");
    session_store
        .create_session_for_workspace(
            known_session_id.clone(),
            "known",
            Some(registered_workspace_id.to_string()),
        )
        .expect("known session should create");
    session_store
        .create_session_for_workspace(
            orphan_session_id.clone(),
            "orphan",
            Some("workspace-snapshot-replay-missing".to_string()),
        )
        .expect("orphan session should create");
    let state = ApiState::new(
        "magi-test",
        event_bus,
        session_store,
        workspace_store,
        governance,
    );

    state.install_snapshot_lifecycle_observer();
    tokio::task::yield_now().await;

    assert!(
        state
            .snapshot_manager
            .get_session(known_session_id.as_str())
            .is_none(),
        "historical sessions must not trigger a startup-wide snapshot scan"
    );
    assert!(
        state
            .snapshot_manager
            .get_session(orphan_session_id.as_str())
            .is_none(),
        "unregistered workspace session should not start a stale snapshot lifecycle"
    );
    let new_session_id = SessionId::new("session-snapshot-new-lazy");
    state
        .session_store
        .create_session_for_workspace(
            new_session_id.clone(),
            "new",
            Some(registered_workspace_id.to_string()),
        )
        .unwrap();
    tokio::task::yield_now().await;
    assert!(
        state
            .snapshot_manager
            .get_session(new_session_id.as_str())
            .is_none(),
        "新建会话同样不得扫描工作区"
    );
    state
        .ensure_snapshot_session(&known_session_id, &workspace_root)
        .await
        .expect("accessing the session must lazily start its snapshot lifecycle");
    assert!(
        state
            .snapshot_manager
            .get_session(known_session_id.as_str())
            .is_some(),
        "lazy snapshot start must materialize the requested historical session"
    );
    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn public_skills_config_section_hides_directory_paths() {
    let temp = tempfile::tempdir().expect("temp skill dir should create");
    std::fs::write(
        temp.path().join("SKILL.md"),
        "# local-skill\n\n请输出 local-skill。\n",
    )
    .expect("skill markdown should write");
    let missing_dir = temp.path().join("missing-skill");
    let public = public_skills_config_section(serde_json::json!({
        "instructionSkills": [
            {
                "name": "local-skill",
                "skillId": "local-skill",
                "directoryPath": temp.path().to_string_lossy().to_string(),
                "description": "desc"
            },
            {
                "name": "missing-skill",
                "skillId": "missing-skill",
                "directoryPath": missing_dir.to_string_lossy().to_string(),
                "description": "stale"
            }
        ],
        "customTools": [
            {
                "name": "custom-tool"
            }
        ]
    }));

    let skill = public["instructionSkills"][0]
        .as_object()
        .expect("skill should stay object");
    assert_eq!(skill["name"], serde_json::json!("local-skill"));
    assert!(!skill.contains_key("directoryPath"));
    assert_eq!(
        public["instructionSkills"].as_array().map(Vec::len),
        Some(1),
        "unavailable local skills should not be exposed as selectable instructions"
    );
    assert_eq!(public["customTools"][0]["name"], "custom-tool");
}

#[test]
fn session_state_checkpoint_runs_callback_and_persists_durable_state() {
    let state_root = std::env::temp_dir().join(format!(
        "magi-api-session-checkpoint-{}",
        UtcMillis::now().0
    ));
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let session_store = Arc::new(SessionStore::default());
    let workspace_store = Arc::new(WorkspaceStore::default());
    let governance = Arc::new(GovernanceService::default());
    let session_id = SessionId::new("session-checkpoint-durable");
    session_store
        .create_session(session_id.clone(), "checkpoint durable")
        .expect("session should create");
    let observed_checkpoints = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed_for_callback = observed_checkpoints.clone();
    let projection_state_root = state_root.clone();

    let state = ApiState::new(
        "magi-test",
        event_bus,
        session_store,
        workspace_store,
        governance,
    )
    .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
        state_root.clone(),
        state_root.join("workspaces.json"),
        state_root.join("knowledge.json"),
    )))
    .with_session_projection_persist(Arc::new(move |durable, _sidecars, _mode| {
        let projection_dir = projection_state_root.join("session-projections");
        std::fs::create_dir_all(&projection_dir)
            .map_err(|error| ApiError::internal_assembly("创建 projection 目录失败", error))?;
        for session in &durable.sessions {
            let path = projection_dir.join(format!("{}.json", session.session_id));
            let content = serde_json::to_vec_pretty(session).map_err(|error| {
                ApiError::internal_assembly("序列化 session projection 失败", error)
            })?;
            std::fs::write(&path, content).map_err(|error| {
                ApiError::internal_assembly("写入 session projection 失败", error)
            })?;
        }
        Ok(())
    }))
    .with_session_state_checkpoint_persist(Arc::new({
        let observed_for_callback = observed_for_callback.clone();
        move |checkpoint| {
            observed_for_callback
                .lock()
                .expect("checkpoint observer lock should not poison")
                .push(checkpoint.to_string());
            Ok(false)
        }
    }));

    state
        .persist_session_state_checkpoint("checkpoint-test")
        .expect("checkpoint should persist");

    assert_eq!(
        observed_checkpoints
            .lock()
            .expect("checkpoint observer lock should not poison")
            .as_slice(),
        ["checkpoint-test"]
    );
    let persisted = std::fs::read_to_string(
        state_root
            .join("session-projections")
            .join(format!("{session_id}.json")),
    )
    .expect("session projection should be written");
    assert!(persisted.contains(session_id.as_str()));
}

#[test]
fn session_projection_persist_is_injected_at_api_boundary() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let session_store = Arc::new(SessionStore::default());
    let workspace_store = Arc::new(WorkspaceStore::default());
    let governance = Arc::new(GovernanceService::default());
    let session_id = SessionId::new("session-orphan-workspace-current");
    session_store
        .create_session_for_workspace(
            session_id.clone(),
            "orphan workspace",
            Some("workspace-missing-current".to_string()),
        )
        .expect("session should create");
    let observed_sessions = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed_for_callback = observed_sessions.clone();

    let state = ApiState::new(
        "magi-test",
        event_bus,
        session_store,
        workspace_store,
        governance,
    )
    .with_session_projection_persist(Arc::new(move |durable, sidecars, _mode| {
        observed_for_callback
            .lock()
            .expect("session projection observer lock should not poison")
            .extend(
                durable
                    .sessions
                    .iter()
                    .map(|session| session.session_id.to_string()),
            );
        assert_eq!(sidecars.runtime_sidecars.len(), 0);
        Ok(())
    }));

    state
        .persist_session_projection()
        .expect("session projection should persist");
    assert_eq!(
        observed_sessions
            .lock()
            .expect("session projection observer lock should not poison")
            .as_slice(),
        [session_id.to_string()]
    );
}

#[test]
fn api_persistence_wrappers_redact_runtime_errors() {
    let state_root = std::env::temp_dir().join(format!(
        "magi-api-redacted-persistence-{}",
        UtcMillis::now().0
    ));
    let session_persist_root = state_root.join("session-persist-conflict");
    let workspace_path = state_root.join("workspaces.json");
    let knowledge_path = state_root.join("knowledge.json");
    std::fs::create_dir_all(&session_persist_root).expect("session conflict dir should create");
    std::fs::create_dir_all(&workspace_path).expect("workspace conflict dir should create");
    std::fs::create_dir_all(&knowledge_path).expect("knowledge conflict dir should create");

    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
        session_persist_root,
        workspace_path,
        knowledge_path,
    )))
    .with_session_projection_persist(Arc::new(|_durable, _sidecars, _mode| {
        Err(ApiError::internal_assembly(
            "session projection 持久化失败",
            "projection conflict",
        ))
    }));

    assert_public_persistence_error(
        state
            .persist_session_projection_for_sessions_for_api(&[])
            .expect_err("session persistence should fail"),
        SESSION_PERSISTENCE_PUBLIC_ERROR,
    );
    assert_public_persistence_error(
        state
            .persist_workspace_durable_state_for_api()
            .expect_err("workspace persistence should fail"),
        WORKSPACE_PERSISTENCE_PUBLIC_ERROR,
    );
    assert_public_persistence_error(
        state
            .persist_knowledge_state_for_api()
            .expect_err("knowledge persistence should fail"),
        KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR,
    );

    let _ = std::fs::remove_dir_all(state_root);
}

fn assert_public_persistence_error(error: ApiError, expected_message: &str) {
    let ApiError::InternalAssemblyError(message) = error else {
        panic!("expected internal assembly error");
    };
    assert_eq!(message, expected_message);
    assert!(!message.contains("os error"));
    assert!(!message.contains("Is a directory"));
    assert!(!message.contains("Permission denied"));
    assert!(!message.contains(".json"));
}

#[tokio::test]
async fn bootstrap_workspace_session_preserves_explicit_draft_selection() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let session_store = Arc::new(SessionStore::default());
    let workspace_store = Arc::new(WorkspaceStore::default());
    let governance = Arc::new(GovernanceService::default());
    let workspace_root = std::env::temp_dir().join(format!(
        "magi-api-bootstrap-default-history-{}",
        UtcMillis::now().0
    ));
    std::fs::create_dir_all(&workspace_root).expect("workspace root should create");
    let state = ApiState::new(
        "magi-test",
        event_bus,
        session_store.clone(),
        workspace_store.clone(),
        governance,
    );
    let workspace_id = WorkspaceId::new("workspace-bootstrap-default-history");
    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().as_ref()),
        )
        .expect("workspace should register");
    session_store
        .create_session_for_workspace(
            SessionId::new("session-empty-bootstrap-history"),
            "空白会话",
            Some(workspace_id.to_string()),
        )
        .expect("empty session should create");

    let older_session_id = SessionId::new("session-bootstrap-older");
    session_store
        .create_session_for_workspace(
            older_session_id.clone(),
            "较早历史",
            Some(workspace_id.to_string()),
        )
        .expect("older session should create");
    session_store.append_timeline_entry(
        older_session_id,
        magi_session_store::TimelineEntryKind::UserMessage,
        "较早消息",
    );
    std::thread::sleep(Duration::from_millis(2));

    let newer_session_id = SessionId::new("session-bootstrap-newer");
    session_store
        .create_session_for_workspace(
            newer_session_id.clone(),
            "较新历史",
            Some(workspace_id.to_string()),
        )
        .expect("newer session should create");
    session_store.append_timeline_entry(
        newer_session_id.clone(),
        magi_session_store::TimelineEntryKind::UserMessage,
        "较新消息",
    );
    state
        .snapshot_manager
        .start_session(
            newer_session_id.as_str().to_string(),
            workspace_root.clone(),
        )
        .await
        .expect("selected session snapshot should start");
    session_store.clear_current_session();

    let bootstrap = state
        .bootstrap_dto_for_workspace_session(Some(workspace_id.as_str()), None)
        .expect("bootstrap should build");

    assert!(bootstrap.current_session.is_none());
    assert!(
        bootstrap
            .sessions
            .iter()
            .any(|session| session.session_id == newer_session_id)
    );
    assert_eq!(bootstrap.sessions.len(), 2);
    assert!(
        bootstrap
            .sessions
            .iter()
            .all(|session| session.message_count.unwrap_or(0) > 0)
    );
    let _ = std::fs::remove_dir_all(workspace_root);
}

#[tokio::test]
async fn bootstrap_workspace_session_ignores_foreign_requested_session() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let workspace_a = WorkspaceId::new("workspace-bootstrap-a");
    let workspace_b = WorkspaceId::new("workspace-bootstrap-b");
    state
        .workspace_registry
        .register(
            workspace_a.clone(),
            AbsolutePath::new("/tmp/magi-bootstrap-workspace-a"),
        )
        .expect("workspace A should register");
    state
        .workspace_registry
        .register(
            workspace_b.clone(),
            AbsolutePath::new("/tmp/magi-bootstrap-workspace-b"),
        )
        .expect("workspace B should register");

    let session_a = SessionId::new("session-bootstrap-workspace-a");
    state
        .session_store
        .create_session_for_workspace(session_a.clone(), "A 会话", Some(workspace_a.to_string()))
        .expect("session A should create");
    state.session_store.append_timeline_entry(
        session_a.clone(),
        magi_session_store::TimelineEntryKind::UserMessage,
        "A 消息",
    );
    let session_b = SessionId::new("session-bootstrap-workspace-b");
    state
        .session_store
        .create_session_for_workspace(session_b.clone(), "B 会话", Some(workspace_b.to_string()))
        .expect("session B should create");
    state.session_store.append_timeline_entry(
        session_b.clone(),
        magi_session_store::TimelineEntryKind::UserMessage,
        "B 消息",
    );

    let bootstrap = state
        .bootstrap_dto_for_workspace_session(Some(workspace_a.as_str()), Some(&session_b))
        .expect("bootstrap should build");

    assert!(bootstrap.current_session.is_none());
    assert_eq!(bootstrap.sessions.len(), 1);
    assert_eq!(bootstrap.sessions[0].session_id, session_a);
    assert!(bootstrap.timeline.is_empty());
}

#[tokio::test]
async fn bootstrap_workspace_session_prefers_stored_selection_over_list_order() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let workspace_id = WorkspaceId::new("workspace-bootstrap-stored-selection");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-bootstrap-stored-selection"),
        )
        .expect("workspace should register");
    let selected_session_id = SessionId::new("session-bootstrap-stored-selected");
    state
        .session_store
        .create_session_for_workspace(
            selected_session_id.clone(),
            "选择会话",
            Some(workspace_id.to_string()),
        )
        .expect("selected session should create");
    state.session_store.append_timeline_entry(
        selected_session_id.clone(),
        magi_session_store::TimelineEntryKind::UserMessage,
        "选择会话消息",
    );
    std::thread::sleep(Duration::from_millis(2));
    let newer_session_id = SessionId::new("session-bootstrap-stored-newer");
    state
        .session_store
        .create_session_for_workspace(
            newer_session_id.clone(),
            "更新会话",
            Some(workspace_id.to_string()),
        )
        .expect("newer session should create");
    state.session_store.append_timeline_entry(
        newer_session_id.clone(),
        magi_session_store::TimelineEntryKind::UserMessage,
        "更新会话消息",
    );
    state
        .session_store
        .select_current_session(&selected_session_id)
        .expect("older session should be selected");

    let bootstrap = state
        .bootstrap_dto_for_workspace_session(Some(workspace_id.as_str()), None)
        .expect("bootstrap should build");

    assert_eq!(
        bootstrap
            .current_session
            .as_ref()
            .map(|session| session.session_id.clone()),
        Some(selected_session_id)
    );
    assert_eq!(bootstrap.sessions[0].session_id, newer_session_id);
}

#[test]
fn resolve_workspace_id_from_request_rejects_unknown_stale_workspace_id() {
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let workspace_id = WorkspaceId::new("workspace-known-from-path");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-known-from-path"),
        )
        .expect("workspace should register");
    state
        .workspace_registry
        .register(
            WorkspaceId::new("workspace-stale-registered-url"),
            AbsolutePath::new("/tmp/magi-stale-registered-url"),
        )
        .expect("stale workspace should register");

    assert_eq!(
            state.resolve_workspace_id_from_request(
                Some(WorkspaceId::new("workspace-stale-url")),
                None,
            ),
            None
        );
    assert_eq!(
        state.resolve_workspace_id_from_request(
            Some(WorkspaceId::new("workspace-stale-url")),
            Some("/tmp/magi-known-from-path"),
        ),
        Some(workspace_id.clone())
    );
    assert_eq!(
        state.resolve_workspace_id_from_request(
            Some(WorkspaceId::new("workspace-stale-registered-url")),
            Some("/tmp/magi-known-from-path"),
        ),
        Some(workspace_id)
    );
}

#[test]
fn workspace_navigation_path_tracks_all_current_sources_after_primary_change() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    let id = WorkspaceId::new("navigation-multi-source");
    let project = state
        .workspace_registry
        .register_native_path(id.clone(), a.clone())
        .unwrap();
    let mut second = project.primary_source_folder().clone();
    let host = magi_core::HostPath::canonicalize(&b).unwrap();
    second.source_folder_id = "secondary".into();
    second.path = AbsolutePath::new(host.display_string());
    second.path_ref = host.to_path_ref().as_str().into();
    let project = state
        .workspace_registry
        .edit_project(
            &id,
            project.revision,
            magi_workspace::WorkspaceProjectEdit {
                name: Some("renamed".into()),
                source_folders: vec![project.primary_source_folder().clone(), second.clone()],
                primary_source_folder_id: second.source_folder_id.clone(),
            },
            |_| Ok(()),
        )
        .unwrap();
    for path in [&a, &b] {
        assert_eq!(
            state.resolve_workspace_id_from_request(None, path.to_str()),
            Some(id.clone())
        );
        let binding =
            crate::scope_binding::require_registered_workspace_binding(&state, None, path.to_str())
                .unwrap();
        assert_eq!(binding.workspace_id, id);
        assert_eq!(binding.workspace_path, host.display_string());
    }
    state
        .workspace_registry
        .edit_project(
            &id,
            project.revision,
            magi_workspace::WorkspaceProjectEdit {
                name: project.name,
                source_folders: vec![second],
                primary_source_folder_id: "secondary".into(),
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        state.resolve_workspace_id_from_request(None, a.to_str()),
        None
    );
    assert_eq!(
        state.resolve_workspace_id_from_request(None, b.to_str()),
        Some(id)
    );
}

#[test]
fn product_capability_dependencies_do_not_require_workspace_or_session_context() {
    let provider = build_runtime_capability_dependency_provider(true);

    let entries = provider();
    let context_runtime = entries
        .iter()
        .find(|entry| entry.name == "context_runtime")
        .expect("context runtime dependency should be listed");
    let file_snapshot = entries
        .iter()
        .find(|entry| entry.name == "file_snapshot")
        .expect("file snapshot dependency should be listed");

    assert_eq!(context_runtime.status, "ready");
    assert_eq!(file_snapshot.status, "ready");
}

#[test]
fn capability_dependency_json_preserves_mcp_tool_count_semantics() {
    let raw = serde_json::json!({
        "name": "mcp_servers",
        "status": "not_ready",
        "required_by": ["mcp custom tools"],
        "configured_count": 1,
        "enabled_count": 1,
        "ready_count": 0,
        "enabled_tool_count": 7,
        "ready_tool_count": 0,
        "tool_count": 0,
    });

    let normalized = normalize_capability_dependency_json(&raw);

    assert_eq!(normalized["enabledToolCount"], serde_json::json!(7));
    assert_eq!(normalized["readyToolCount"], serde_json::json!(0));
    assert_eq!(
        normalized["toolCount"],
        serde_json::json!(0),
        "toolCount must remain the ready/usable tool count in settings bootstrap"
    );
}

#[test]
fn public_tool_catalog_response_uses_camel_case_boundary() {
    let raw = serde_json::json!({
        "tool": "tool_catalog",
        "status": "succeeded",
        "catalog_access_mode": "read_only",
        "current_access_profile": "full_access",
        "runtime_dependencies": [{
            "name": "mcp_servers",
            "status": "ready",
            "required_by": ["mcp custom tools"],
            "enabled_tool_count": 2,
            "tool_count": 2
        }],
        "tools": [{
            "name": "shell_exec",
            "category": "builtin",
            "public": true,
            "runtime_internal": false,
            "model_call_scope": "session_or_task",
            "access_mode": "explicit_write",
            "policy_scope": "input_sensitive",
            "input_sensitive_policy": true,
            "policy_summary": "summary",
            "risk_level": "high",
            "approval_requirement": "required",
            "effective_approval_policy": "none",
            "access_profile_behavior": "full_access_allowed",
            "schema_status": "ok",
            "schema_warnings": [],
            "runtime_status": "ready",
            "runtime_warnings": [],
            "model_available": false,
            "availability": "denied",
            "reason_code": "tool_not_in_allowed_scope",
            "schema_included": true,
            "parameters_schema": {"type": "object", "properties": {"old_string": {"type": "string"}}}
        }],
        "skill_tools": [{
            "source": "skill",
            "skill_id": "skill-1",
            "binding_id": "binding-1",
            "name": "skill.tool",
            "description": "tool",
            "bridge_kind": "skill",
            "dispatch_action": "run",
            "bridge_target": "skill-1",
            "access_profile_behavior": "restricted_allowed",
            "risk_level": "low",
            "approval_requirement": "none",
            "status": "available"
        }],
        "mcp_servers": [{
            "server_id": "mcp-1",
            "name": "mcp",
            "enabled": true,
            "connected": true,
            "health": "connected",
            "tool_count": 3,
            "error": null
        }],
        "mcp_tools": [{
            "server_id": "mcp-1",
            "server_name": "mcp",
            "model_tool_name": "mcp__mcp-1__inspect",
            "tool_name": "inspect",
            "description": "inspect",
            "read_only": true,
            "model_available": false,
            "availability": "denied",
            "reason_code": "tool_explicitly_denied",
            "input_schema": {"type": "object", "properties": {}}
        }],
        "agent_roles": [{
            "role_id": "executor",
            "spawnable": true,
            "coordinator_mode": false,
            "supported_kinds": ["local_agent"],
            "parallelism_limit": 2,
            "status": "ready"
        }]
    });

    let public = public_tool_catalog_response_json(&raw);

    assert!(public.get("runtime_dependencies").is_none());
    assert!(public.get("catalog_access_mode").is_none());
    assert_eq!(public["catalogAccessMode"], serde_json::json!("read_only"));
    assert_eq!(
        public["currentAccessProfile"],
        serde_json::json!("full_access")
    );
    assert_eq!(
        public["runtimeDependencies"][0]["requiredBy"][0],
        "mcp custom tools"
    );
    assert_eq!(public["runtimeDependencies"][0]["enabledToolCount"], 2);
    assert_eq!(public["tools"][0]["effectiveApprovalPolicy"], "none");
    assert_eq!(public["tools"][0]["modelCallScope"], "session_or_task");
    assert_eq!(public["tools"][0]["modelAvailable"], false);
    assert_eq!(public["tools"][0]["availability"], "denied");
    assert_eq!(
        public["tools"][0]["reasonCode"],
        "tool_not_in_allowed_scope"
    );
    assert_eq!(public["tools"][0]["schemaIncluded"], true);
    assert_eq!(
        public["tools"][0]["parametersSchema"]["properties"]["old_string"]["type"],
        "string"
    );
    assert_eq!(public["skillTools"][0]["skillId"], "skill-1");
    assert_eq!(
        public["skillTools"][0]["accessProfileBehavior"],
        "restricted_allowed"
    );
    assert_eq!(public["mcpServers"][0]["serverId"], "mcp-1");
    assert_eq!(public["mcpServers"][0]["toolCount"], 3);
    assert_eq!(public["mcpTools"][0]["readOnly"], true);
    assert_eq!(public["mcpTools"][0]["modelAvailable"], false);
    assert_eq!(
        public["mcpTools"][0]["reasonCode"],
        "tool_explicitly_denied"
    );
    assert_eq!(public["agentRoles"][0]["roleId"], "executor");
    assert_eq!(public["agentRoles"][0]["parallelismLimit"], 2);
}
