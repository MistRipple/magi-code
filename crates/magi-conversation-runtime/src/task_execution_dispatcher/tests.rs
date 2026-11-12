#[test]
fn a_title_that_only_truncates_the_goal_is_not_repeated_in_the_prompt() {
    let goal = "只读任务,不要修改任何文件:请读取 docs/README.md 并总结仓库结构。";
    assert!(task_title_is_derived_from_goal(
        "执行: 只读任务,不要修改任何文件:请读取 docs/README.md",
        goal
    ));
    // 标题携带目标之外的信息时保留。
    assert!(!task_title_is_derived_from_goal("执行: 目标自动推进", goal));
    assert!(!task_title_is_derived_from_goal("", goal));
}

use super::*;
use crate::CanonicalTurnEventSink;
use crate::model_config::{merge_orchestrator_session_override, resolve_orchestrator_model_config};
use magi_core::{MissionId, Task, TaskCompletionAttempt, TaskPolicy, TaskRuntimePayload, TaskTier};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn retired_root_detection_decodes_windows_paths_and_allows_the_allocated_subdirectory() {
    let old = std::path::Path::new(r"C:\project");
    let new = std::path::Path::new(r"C:\project\.magi\isolated");
    assert!(arguments_reference_retired_root(
        "file_read",
        &serde_json::json!({"paths": [r"C:\project\src\main.rs"]}).to_string(),
        old,
        Some(new),
    ));
    assert!(!arguments_reference_retired_root(
        "shell_exec",
        &serde_json::json!({"command": r"cat C:\project\.magi\isolated\README.md"}).to_string(),
        old,
        Some(new),
    ));
    assert!(arguments_reference_retired_root(
        "shell_exec",
        &serde_json::json!({"command": r"cp C:\project\a.txt C:\project\.magi\isolated\a.txt"})
            .to_string(),
        old,
        Some(new),
    ));
}

#[cfg(unix)]
#[test]
fn retired_root_detection_covers_path_aliases_in_file_and_shell_tools() {
    let fixture = tempfile::tempdir().unwrap();
    let original = fixture.path().join("original");
    let alias = fixture.path().join("alias");
    let current = original.join("isolated");
    std::fs::create_dir_all(&current).unwrap();
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    assert!(
        !arguments_reference_retired_root(
            "file_write",
            &serde_json::json!({"path": alias.join("new.txt"), "content":"a"}).to_string(),
            &original,
            Some(&alias),
        ),
        "相同目录的别名不属于执行根切换"
    );
    assert_eq!(
        project_policy_paths_to_execution_root(
            &[alias.join("private").to_string_lossy().into_owned()],
            &original,
            &current,
        ),
        vec![current.join("private").to_string_lossy().into_owned()],
        "通过别名声明的禁止/只读子目录也必须投影到隔离目录"
    );
    for (tool, arguments) in [
        (
            "file_write",
            serde_json::json!({"path": alias.join("new.txt"), "content":"a"}),
        ),
        (
            "shell_exec",
            serde_json::json!({"command": format!("cat '{}'", alias.join("old.txt").display())}),
        ),
    ] {
        assert!(arguments_reference_retired_root(
            tool,
            &arguments.to_string(),
            &original,
            Some(&current)
        ));
    }
    assert!(!arguments_reference_retired_root(
        "file_write",
        &serde_json::json!({
            "path": alias.join("isolated/new.txt"), "content":"a"
        })
        .to_string(),
        &original,
        Some(&current)
    ));
}

#[test]
fn http_engine_binding_does_not_require_a_plugin_factory() {
    let session_id = SessionId::new("session-http");
    let store = Arc::new(SettingsStore::new());
    store
        .set_section(
            "engines",
            serde_json::json!([
                {
                    "id": "sonnet-4-5",
                    "displayName": "Sonnet 4.5",
                    "llm": {
                        "baseUrl": "https://api.example.com/v1",
                        "apiKey": "sk-engine",
                        "model": "claude-sonnet-4-5",
                        "apiProtocol": "openai_chat",
                    },
                }
            ]),
        )
        .expect("写入 engines");
    store
        .set_section(
            "agents",
            serde_json::json!([{"templateId": "reviewer", "engineId": "sonnet-4-5"}]),
        )
        .expect("写入角色绑定");
    let resolved = resolve_target_for_role(
        Some(&store),
        None,
        RoleTarget::Agent {
            role_id: "reviewer",
        },
        Some(&session_id),
    )
    .expect("HTTP 引擎绑定应当可用");
    assert!(resolved.is_some());
}

struct FailingAuxiliaryClient;

impl ModelBridgeClient for FailingAuxiliaryClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(
        &self,
        _request: ModelInvocationRequest,
    ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError> {
        Err(magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::Transport,
            code: None,
            message: "test failure".to_string(),
        })
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
    ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError> {
        self.invoke(request)
    }
}

fn task_with_role(role: &str, task_tier: TaskTier) -> Task {
    let now = UtcMillis(1_000);
    let background_allowed = false;
    let task_id = TaskId::new(format!("task-{role}"));
    Task {
        task_id: task_id.clone(),
        mission_id: MissionId::new("mission-tool-scope"),
        root_task_id: task_id,
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: format!("task {role}"),
        goal: format!("run as {role}"),
        status: magi_core::TaskStatus::Pending,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: Some(TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            access_profile: magi_core::AccessProfile::Restricted,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            read_only_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: None,
            checkpoint_mode: "turn".to_string(),
            task_tier,
            background_allowed,
            escalation_conditions: Vec::new(),
        }),
        executor_binding: Some(magi_core::TaskExecutorBinding::for_role(role)),
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

fn dispatcher_with_default_tool_surface() -> LlmTaskDispatcher {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let governance = Arc::new(magi_governance::GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    dispatcher_with_tool_surface(event_bus, tool_registry, "dispatcher-default-tool-surface")
}

fn dispatcher_with_ready_browser_tool_surface() -> LlmTaskDispatcher {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let governance = Arc::new(magi_governance::GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus))
        .with_browser_automation(
            Arc::new(|_, tool, _, _| {
                (
                    serde_json::json!({ "tool": tool, "status": "succeeded" }).to_string(),
                    magi_core::ExecutionResultStatus::Succeeded,
                )
            }),
            Arc::new(|| magi_browser_authority::BrowserCapabilitySnapshot {
                revision: 11,
                in_app_browser_enabled: true,
                browser_use_enabled: true,
                devtools_enabled: true,
                lan_access_enabled: false,
                host_status: magi_browser_authority::BrowserHostStatus::Ready,
                host_protocol_compatible: true,
            }),
        );
    tool_registry.register_default_builtins();
    dispatcher_with_tool_surface(event_bus, tool_registry, "dispatcher-browser-tool-surface")
}

fn dispatcher_with_tool_surface(
    event_bus: Arc<InMemoryEventBus>,
    tool_registry: ToolRegistry,
    state_label: &str,
) -> LlmTaskDispatcher {
    let task_store = Arc::new(TaskStore::new());
    let pipeline = ExecutionPipeline {
        task_store,
        memory_store: MemoryStore::new(),
    };

    LlmTaskDispatcher::new(
        Arc::clone(&event_bus),
        pipeline,
        LlmTaskDispatcherDependencies {
            session_store: Arc::new(SessionStore::new()),
            execution_registry: TaskExecutionRegistry::default(),
            completion_sink: Arc::new(|_| panic!("此夹具不应派发 Task 终态")),
            conversation_registry: Arc::new(ConversationRegistry::new()),
            agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
        },
        test_mission_state_root(state_label),
    )
    .with_tool_registry(tool_registry)
}

#[test]
fn post_completion_enrichment_does_not_block_terminal_result() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auxiliary test server");
    let address = listener.local_addr().expect("auxiliary test address");
    let (request_started_tx, request_started_rx) = mpsc::channel();
    let (release_response_tx, release_response_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept auxiliary request");
        request_started_tx
            .send(())
            .expect("signal auxiliary request start");
        release_response_rx
            .recv()
            .expect("wait for auxiliary response release");
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"[]"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write auxiliary response");
    });

    let settings = Arc::new(SettingsStore::new());
    settings
        .set_section(
            "auxiliary",
            serde_json::json!({
                "baseUrl": format!("http://{address}/v1"),
                "model": "auxiliary-enrichment-test",
                "apiProtocol": "openai_chat"
            }),
        )
        .expect("auxiliary settings should save");
    let dispatcher = dispatcher_with_default_tool_surface()
        .with_settings_store(settings)
        .with_knowledge_store(Arc::new(KnowledgeStore::new()));
    let (scheduled_tx, scheduled_rx) = mpsc::channel();
    let schedule_call = std::thread::spawn(move || {
        dispatcher.schedule_post_completion_enrichment(
            SessionId::new("session-nonblocking-enrichment"),
            "turn-nonblocking-enrichment".to_string(),
            Some(WorkspaceId::new("workspace-nonblocking-enrichment")),
            TaskOutcome::Completed {
                attempt: TaskCompletionAttempt {
                    output_refs: vec!["已完成真实任务输出，等待后台知识抽取".to_string()],
                    final_response: Some("任务已完成".to_string()),
                    evidence: Vec::new(),
                },
            },
            None,
        );
        scheduled_tx.send(()).expect("signal scheduling return");
    });

    let returned_before_auxiliary_release =
        scheduled_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let auxiliary_request_started = request_started_rx
        .recv_timeout(Duration::from_secs(2))
        .is_ok();
    if !auxiliary_request_started {
        let _ = TcpStream::connect(address);
    }
    release_response_tx
        .send(())
        .expect("release auxiliary response");
    schedule_call
        .join()
        .expect("join enrichment scheduling call");
    server.join().expect("join auxiliary test server");

    assert!(
        returned_before_auxiliary_release,
        "任务终态不能等待后台知识/记忆抽取返回"
    );
    assert!(
        auxiliary_request_started,
        "后台知识抽取必须真实启动，测试不能通过跳过辅助模型伪造非阻塞"
    );
}

fn git_fixture(path: &std::path::Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn sidechain_projects_workspace_policy_paths_to_agent_execution_root() {
    let identity_root = PathBuf::from("/workspace/project");
    let execution_root = PathBuf::from("/runtime/agent-worktree");
    let mut task = task_with_role("explorer", TaskTier::ExecutionChain);
    let policy = task.policy_snapshot.as_mut().expect("task policy");
    policy.allowed_paths = vec![
        identity_root.to_string_lossy().into_owned(),
        "/shared/reference".to_string(),
    ];
    policy.denied_paths = vec![identity_root.join("private").to_string_lossy().into_owned()];
    policy.read_only_paths = vec![identity_root.join("vendor").to_string_lossy().into_owned()];

    let projected = LlmTaskDispatcher::task_for_execution_root(
        &task,
        Some(&identity_root),
        Some(&execution_root),
    );
    let projected_policy = projected.policy_snapshot.expect("projected policy");

    assert_eq!(
        projected_policy.allowed_paths,
        vec![
            execution_root.to_string_lossy().into_owned(),
            "/shared/reference".to_string(),
        ]
    );
    assert_eq!(
        projected_policy.denied_paths,
        vec![
            execution_root
                .join("private")
                .to_string_lossy()
                .into_owned()
        ]
    );
    assert_eq!(
        projected_policy.read_only_paths,
        vec![execution_root.join("vendor").to_string_lossy().into_owned()]
    );
    assert_eq!(
        task.policy_snapshot
            .as_ref()
            .expect("source policy")
            .allowed_paths[0],
        identity_root.to_string_lossy()
    );
    let decision = crate::tool_batch::access_profile_tool_decision(
        crate::tool_batch::AccessProfileToolDecisionInput {
            access_profile: projected_policy.access_profile,
            command_mode: &projected_policy.command_mode,
            allowed_tools: &projected_policy.allowed_tools,
            denied_tools: &projected_policy.denied_tools,
            allowed_paths: &projected_policy.allowed_paths,
            denied_paths: &projected_policy.denied_paths,
            read_only_paths: &projected_policy.read_only_paths,
            requested_tool_name: BuiltinToolName::FileRead.as_str(),
            arguments: r#"{"path":"crates/magi-api/src/routes/sessions.rs"}"#,
            workspace_root_path: Some(&execution_root),
        },
    );
    assert!(
        decision.is_none(),
        "子代理相对执行根目录读取工作区文件不应被继承的逻辑路径策略拒绝"
    );
}

struct ScopeProbeExecutor {
    scopes: Arc<Mutex<Vec<magi_bridge_client::ModelExecutionScope>>>,
}

impl ModelBridgeClient for ScopeProbeExecutor {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        let mut caps = magi_bridge_client::text_executor_capabilities();
        caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
        caps
    }

    fn bind_execution_scope(
        &self,
        scope: magi_bridge_client::ModelExecutionScope,
    ) -> Result<Option<Arc<dyn ModelBridgeClient>>, BridgeClientError> {
        self.scopes.lock().unwrap().push(scope);
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: "executor_policy_unsupported: probe stops before external execution".into(),
        })
    }

    fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        panic!("scope probe must stop before executing")
    }

    fn invoke_streaming(
        &self,
        _: ModelInvocationRequest,
        _: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        panic!("scope probe must stop before executing")
    }
}

#[test]
fn executor_owned_child_prepares_isolated_directory_before_scope_binding() {
    let fixture = tempfile::tempdir().unwrap();
    let repository = fixture.path().join("repo");
    std::fs::create_dir(&repository).unwrap();
    git_fixture(&repository, &["init", "-b", "main"]);
    git_fixture(&repository, &["config", "user.name", "Magi Test"]);
    git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
    std::fs::write(repository.join("README.md"), "base\n").unwrap();
    git_fixture(&repository, &["add", "README.md"]);
    git_fixture(&repository, &["commit", "-m", "base"]);
    std::fs::write(repository.join("README.md"), "parent uncommitted\n").unwrap();
    let service = Arc::new(magi_git::GitService::new());
    let observation = block_on_git(service.observe(&repository)).unwrap();
    let contexts = magi_git::SessionCodeContextRegistry::default();
    let session = SessionId::new("session-external-scope");
    let workspace = Some(WorkspaceId::new("workspace-external-scope"));
    contexts.accept(
        session.as_str(),
        workspace.as_ref().unwrap().as_str(),
        vec![repository.clone()],
        &observation,
    );
    let scopes = Arc::new(Mutex::new(Vec::new()));
    let mut dispatcher = dispatcher_with_default_tool_surface()
        .with_git_context_runtime(service, contexts.clone())
        .with_model_bridge_client(Arc::new(ScopeProbeExecutor {
            scopes: scopes.clone(),
        }));
    dispatcher.agent_worktree_root = fixture.path().join("agent-worktrees");
    let mut parent = task_with_role("coordinator", TaskTier::ExecutionChain);
    parent.status = magi_core::TaskStatus::Running;
    let mut child = task_with_role("explorer", TaskTier::ExecutionChain);
    child.status = magi_core::TaskStatus::Running;
    child.root_task_id = parent.task_id.clone();
    child.parent_task_id = Some(parent.task_id.clone());
    let policy = child.policy_snapshot.as_mut().unwrap();
    policy.access_profile = AccessProfile::ReadOnly;
    policy.allowed_paths = vec![repository.to_string_lossy().into_owned()];
    let store = dispatcher.pipeline.task_store.as_ref();
    store.insert_task(parent).unwrap();
    store.insert_task(child.clone()).unwrap();
    let worker = WorkerId::new("worker-external-scope");
    let lease = store
        .grant_lease(
            &child.task_id,
            &child.root_task_id,
            &worker,
            "explorer",
            60_000,
        )
        .unwrap();
    dispatcher
        .session_store
        .create_session(session.clone(), "scope probe")
        .unwrap();
    dispatcher
        .session_store
        .ensure_session_mission(&session, UtcMillis::now(), || child.mission_id.clone());
    let thread = crate::session_thread::ensure_thread_for_role(
        &dispatcher.session_store,
        &session,
        &child.mission_id,
        "explorer",
        &worker,
        &child.task_id,
        UtcMillis::now(),
    )
    .unwrap();
    let turn_id = "turn-external-scope";
    dispatcher
        .session_store
        .accept_current_turn_with_timeline_entry(
            session.clone(),
            magi_session_store::TimelineEntryInput::new(
                "scope-probe-input",
                TimelineEntryKind::UserMessage,
                "inspect repository",
                UtcMillis::now(),
            ),
            magi_session_store::ActiveExecutionTurn {
                turn_id: turn_id.into(),
                turn_seq: 1,
                accepted_at: UtcMillis::now(),
                completed_at: None,
                status: "running".into(),
                user_message: Some("inspect repository".into()),
                items: vec![],
            },
        )
        .unwrap();
    dispatcher
        .execution_registry
        .insert(
            child.task_id.clone(),
            TaskExecutionPlan::Dispatch {
                target: magi_core::TaskExecutionTarget {
                    mission_id: child.mission_id.clone(),
                    root_task_id: child.root_task_id.clone(),
                    task_id: child.task_id.clone(),
                    requested_worker_id: Some(worker.clone()),
                    recovery_id: None,
                    execution_chain_ref: None,
                },
                worker_id: worker.clone(),
                thread_id: thread.clone(),
                is_primary: false,
                session_id: session.clone(),
                turn_id: turn_id.into(),
                workspace_id: workspace.clone(),
                execution_root: Some(repository.clone()),
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: None,
                images: vec![],
                execution_settings_snapshot: None,
            },
        )
        .unwrap();
    let usage = crate::usage_recording::session_turn_model_usage_binding(true);
    let invoke = |use_tools| {
        dispatcher.invoke_llm_with_tools(TaskInvocationInput {
            task: &child,
            lease_id: &lease.lease_id,
            session_id: &session,
            workspace_id: &workspace,
            execution_root: Some(&repository),
            use_tools,
            skill_name: None,
            images: vec![],
            usage_binding: &usage,
            streaming_entry_id: None,
            is_sidechain: true,
            worker_id: Some(&worker),
            execution_role_id: Some("explorer"),
            thread_id: &thread,
            system_prompt: None,
            execution_settings_snapshot: None,
        })
    };
    let (outcome, _) = invoke(false);
    assert!(matches!(outcome, TaskOutcome::Failed { .. }));
    assert!(scopes.lock().unwrap().is_empty());
    assert!(
        contexts
            .get(session.as_str())
            .unwrap()
            .agent_worktrees
            .is_empty()
    );
    let (outcome, _) = invoke(true);
    assert!(
        matches!(outcome, TaskOutcome::Failed { .. }),
        "probe rejects binding: {outcome:?}"
    );
    let observed = scopes.lock().unwrap();
    assert_eq!(
        observed.len(),
        1,
        "must reach scope binding after preparation: {outcome:?}"
    );
    let root = observed[0].working_dir.as_ref().unwrap();
    assert_ne!(root, &repository);
    assert_eq!(
        std::fs::read_to_string(root.join("README.md")).unwrap(),
        "parent uncommitted\n"
    );
    assert_eq!(
        observed[0].policy.allowed_paths,
        vec![root.to_string_lossy()]
    );
    let context = contexts.get(session.as_str()).unwrap();
    assert_eq!(context.agent_worktrees.len(), 1);
    assert_eq!(
        context.agent_worktrees[0].mode,
        magi_git::AgentWorktreeMode::ReadOnly
    );
    drop(observed);
    dispatcher.finalize_agent_worktree(&child, &lease.lease_id, &session, &workspace, true);
    assert!(!context.agent_worktrees[0].path.exists());
}

#[test]
fn recovered_agent_worktree_keeps_original_files_across_new_runtime_workers_and_cleanup() {
    for failed_checkpoint in [
        None,
        Some("agent_worktree_reattached"),
        Some("agent_worktree_result_saved"),
    ] {
        let fail_persist = failed_checkpoint == Some("agent_worktree_reattached");
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repo");
        std::fs::create_dir(&repository).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Magi Test"],
            vec!["config", "user.email", "test@magi.invalid"],
        ] {
            git_fixture(&repository, &args);
        }
        std::fs::write(repository.join("README.md"), "original baseline").unwrap();
        git_fixture(&repository, &["add", "."]);
        git_fixture(&repository, &["commit", "-m", "base"]);
        let git = Arc::new(magi_git::GitService::new());
        let observation = block_on_git(git.observe(&repository)).unwrap();
        let contexts = magi_git::SessionCodeContextRegistry::default();
        let session = SessionId::new("resource-recovery");
        let workspace = Some(WorkspaceId::new("resource-recovery"));
        contexts.accept(
            session.as_str(),
            workspace.as_ref().unwrap().as_str(),
            vec![],
            &observation,
        );
        let mut dispatcher =
            dispatcher_with_default_tool_surface().with_git_context_runtime(git, contexts.clone());
        dispatcher.agent_worktree_root = fixture.path().join("worktrees");
        let mut parent = task_with_role("coordinator", TaskTier::ExecutionChain);
        parent.status = magi_core::TaskStatus::Running;
        let mut child = task_with_role("executor", TaskTier::ExecutionChain);
        child.status = magi_core::TaskStatus::Running;
        child.parent_task_id = Some(parent.task_id.clone());
        child.root_task_id = parent.task_id.clone();
        child.executor_binding.as_mut().unwrap().agent_activation =
            Some(magi_core::AgentActivation {
                agent_id: child.task_id.clone(),
                session_id: session.clone(),
                lifetime: magi_core::AgentLifetime::Session,
                generation: 1,
                previous_task_id: None,
            });
        let store = dispatcher.pipeline.task_store.as_ref();
        store.insert_task(parent).unwrap();
        store.insert_task(child.clone()).unwrap();
        let logical_worker = WorkerId::new("persistent-thread-worker");
        let first = store
            .grant_lease(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("old-runtime-worker"),
                "executor",
                60_000,
            )
            .unwrap();
        let path = dispatcher
            .resolve_task_execution_root(
                &child,
                &first.lease_id,
                &session,
                &workspace,
                None,
                true,
                Some(&logical_worker),
            )
            .unwrap()
            .unwrap();
        store
            .transfer_agent_subtree(
                &child.root_task_id,
                &child.task_id,
                magi_core::AgentRunTransfer {
                    operation_id: "resource-recovery-transfer".into(),
                    session_id: session.clone(),
                    source_turn_id: "original-parent-turn".into(),
                    transferred_by: child.root_task_id.clone(),
                    activation_generation: 1,
                    owner: magi_core::AgentRunOwner::Session,
                    transferred_at: UtcMillis::now(),
                },
            )
            .unwrap();
        child = store.get_task(&child.task_id).unwrap();
        std::fs::write(path.join("retained.txt"), "original agent output").unwrap();
        assert!(store.revoke_lease(&child.task_id, &first.lease_id));
        contexts.restore_after_restart(contexts.all());
        std::fs::write(repository.join("README.md"), "new mainline baseline").unwrap();
        let second = store
            .grant_lease(
                &child.task_id,
                &child.root_task_id,
                &WorkerId::new("new-runtime-worker"),
                "executor",
                60_000,
            )
            .unwrap();
        dispatcher.session_state_persist_callback = Some(Arc::new(move |checkpoint| {
            if Some(checkpoint) == failed_checkpoint {
                Err("injected resource flush failure".into())
            } else {
                Ok(())
            }
        }));
        let resumed = dispatcher.resolve_task_execution_root(
            &child,
            &second.lease_id,
            &session,
            &workspace,
            None,
            true,
            Some(&logical_worker),
        );
        assert_eq!(resumed.is_err(), fail_persist, "{resumed:?}");
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "original baseline"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("retained.txt")).unwrap(),
            "original agent output"
        );
        assert_eq!(
            contexts
                .get(session.as_str())
                .unwrap()
                .agent_worktrees
                .len(),
            1
        );
        if fail_persist {
            assert!(!contexts.get(session.as_str()).unwrap().agent_worktrees[0].active);
            assert!(
                path.exists(),
                "保存失败仍保留原文件供核对，不能执行清理或换新副本"
            );
        } else {
            assert_eq!(
                std::fs::canonicalize(resumed.unwrap().unwrap()).unwrap(),
                std::fs::canonicalize(&path).unwrap()
            );
            assert!(
                dispatcher
                    .finalize_agent_worktree(&child, &first.lease_id, &session, &workspace, true)
                    .is_none()
            );
            assert!(
                contexts.get(session.as_str()).unwrap().agent_worktrees[0].active,
                "旧 lease 的迟到清理不能释放新执行"
            );
            dispatcher
                .finalize_agent_worktree(&child, &second.lease_id, &session, &workspace, true)
                .unwrap();
            assert_eq!(
                path.exists(),
                failed_checkpoint == Some("agent_worktree_result_saved"),
                "清理前保存失败须保留原目录；正常清理之后从持久结果恢复"
            );
            assert!(store.revoke_lease(&child.task_id, &second.lease_id));
            let third = store
                .grant_lease(
                    &child.task_id,
                    &child.root_task_id,
                    &WorkerId::new("third-runtime-worker"),
                    "executor",
                    60_000,
                )
                .unwrap();
            assert_eq!(
                std::fs::canonicalize(
                    dispatcher
                        .resolve_task_execution_root(
                            &child,
                            &third.lease_id,
                            &session,
                            &workspace,
                            None,
                            true,
                            Some(&logical_worker)
                        )
                        .unwrap()
                        .unwrap()
                )
                .unwrap(),
                std::fs::canonicalize(&path).unwrap()
            );
            assert_eq!(
                std::fs::read_to_string(path.join("retained.txt")).unwrap(),
                "original agent output",
                "已清理目录从原结果重建，不能从当前主线建立新基线"
            );
        }
        contexts.remove(session.as_str());
        assert!(
            dispatcher
                .resolve_task_execution_root(
                    &child,
                    &second.lease_id,
                    &session,
                    &workspace,
                    None,
                    true,
                    Some(&logical_worker)
                )
                .is_err(),
            "丢失原目录上下文时，不能以当前主线作为转交代理的恢复目录"
        );
        assert!(!repository.join("retained.txt").exists());
    }
}

#[test]
fn sidechain_inherits_session_base_in_independent_worktree() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let repository = fixture.path().join("repo");
    std::fs::create_dir_all(&repository).expect("repo directory");
    git_fixture(&repository, &["init", "-b", "main"]);
    git_fixture(&repository, &["config", "user.name", "Magi Test"]);
    git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
    std::fs::write(repository.join("README.md"), "base\n").expect("fixture file");
    git_fixture(&repository, &["add", "README.md"]);
    git_fixture(&repository, &["commit", "-m", "base"]);

    let git_service = Arc::new(magi_git::GitService::new());
    let observation = block_on_git(git_service.observe(&repository)).expect("observe repo");
    let contexts = magi_git::SessionCodeContextRegistry::default();
    contexts.accept(
        "session-git-agent",
        "workspace-git-agent",
        vec![repository.clone()],
        &observation,
    );
    let mut dispatcher = dispatcher_with_default_tool_surface()
        .with_git_context_runtime(git_service, contexts.clone());
    dispatcher.agent_worktree_root = fixture.path().join("agent-worktrees");
    let task = task_with_role("executor", TaskTier::ExecutionChain);
    let lease_id = LeaseId::new("lease-git-agent");
    let execution_root = dispatcher
        .resolve_task_execution_root(
            &task,
            &lease_id,
            &SessionId::new("session-git-agent"),
            &Some(WorkspaceId::new("workspace-git-agent")),
            None,
            true,
            Some(&WorkerId::new("worker-git-agent")),
        )
        .expect("agent root")
        .expect("agent path");

    assert_ne!(execution_root, repository);
    let agent_head = magi_process::std_command("git")
        .arg("-C")
        .arg(&execution_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("agent head");
    assert!(agent_head.status.success());
    assert_eq!(
        String::from_utf8_lossy(&agent_head.stdout).trim(),
        observation.head.as_deref().expect("base head")
    );
    let context = contexts.get("session-git-agent").expect("session context");
    assert_eq!(context.agent_worktrees.len(), 1);
    assert_eq!(
        context.agent_worktrees[0].base_head,
        observation.head.expect("head")
    );
    assert_eq!(
        context.agent_worktrees[0].mode,
        magi_git::AgentWorktreeMode::Writable
    );
    let agent_branch = context.agent_worktrees[0]
        .branch
        .clone()
        .expect("writable agent branch");

    dispatcher.finalize_agent_worktree(
        &task,
        &lease_id,
        &SessionId::new("session-git-agent"),
        &Some(WorkspaceId::new("workspace-git-agent")),
        true,
    );

    assert!(
        !execution_root.exists(),
        "clean agent worktree should be removed"
    );
    let context = contexts.get("session-git-agent").expect("released context");
    assert!(!context.agent_worktrees[0].active);
    assert!(
        !context
            .runtime_workspace_roots
            .iter()
            .any(|root| root == &execution_root)
    );
    git_fixture(
        &repository,
        &[
            "show-ref",
            "--verify",
            &format!("refs/heads/{agent_branch}"),
        ],
    );
}

#[test]
fn writable_sidechain_starts_from_main_worktree_and_commits_its_output() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let repository = fixture.path().join("repo");
    std::fs::create_dir_all(&repository).expect("repo directory");
    git_fixture(&repository, &["init", "-b", "main"]);
    git_fixture(&repository, &["config", "user.name", "Magi Test"]);
    git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
    std::fs::write(repository.join("README.md"), "base\n").expect("fixture file");
    git_fixture(&repository, &["add", "README.md"]);
    git_fixture(&repository, &["commit", "-m", "base"]);

    // 主线有未提交改动：子代理必须从包含它的快照出发。
    std::fs::write(repository.join("README.md"), "main uncommitted\n").expect("main edit");
    let git_service = Arc::new(magi_git::GitService::new());
    let observation = block_on_git(git_service.observe(&repository)).expect("observe repo");
    let contexts = magi_git::SessionCodeContextRegistry::default();
    contexts.accept(
        "session-git-agent-dirty",
        "workspace-git-agent-dirty",
        vec![repository.clone()],
        &observation,
    );
    let mut dispatcher = dispatcher_with_default_tool_surface()
        .with_git_context_runtime(git_service, contexts.clone());
    dispatcher.agent_worktree_root = fixture.path().join("agent-worktrees");
    let task = task_with_role("executor", TaskTier::ExecutionChain);
    let lease_id = LeaseId::new("lease-git-agent-dirty");
    let execution_root = dispatcher
        .resolve_task_execution_root(
            &task,
            &lease_id,
            &SessionId::new("session-git-agent-dirty"),
            &Some(WorkspaceId::new("workspace-git-agent-dirty")),
            None,
            true,
            Some(&WorkerId::new("worker-git-agent-dirty")),
        )
        .expect("agent root")
        .expect("agent path");
    assert_eq!(
        std::fs::read_to_string(execution_root.join("README.md")).unwrap(),
        "main uncommitted\n",
        "子代理应看到主线未提交的改动"
    );
    std::fs::write(execution_root.join("uncommitted.txt"), "agent output\n")
        .expect("dirty agent output");

    let workspace_output = dispatcher.finalize_agent_worktree(
        &task,
        &lease_id,
        &SessionId::new("session-git-agent-dirty"),
        &Some(WorkspaceId::new("workspace-git-agent-dirty")),
        true,
    );

    let workspace_output = workspace_output.expect("可写代理有改动时应返回产出");
    assert_eq!(
        workspace_output["agent_workspace"]["changed_paths"],
        serde_json::json!(["uncommitted.txt"]),
        "主线快照里的改动不算代理产出"
    );
    assert!(!execution_root.exists(), "提交后的干净 worktree 应移除");
    let context = contexts
        .get("session-git-agent-dirty")
        .expect("released context");
    let allocation = &context.agent_worktrees[0];
    assert!(!allocation.active);
    let head = allocation.result_head.clone().expect("应记录代理分支 HEAD");
    assert_eq!(
        allocation.changed_paths,
        vec!["uncommitted.txt".to_string()]
    );
    assert_eq!(
        git_fixture(&repository, &["show", &format!("{head}:uncommitted.txt")]),
        "agent output"
    );
    assert!(
        !context
            .runtime_workspace_roots
            .iter()
            .any(|root| root == &execution_root)
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("README.md")).unwrap(),
        "main uncommitted\n",
        "主线工作树不受影响"
    );
}

#[test]
fn sidechain_repository_allocations_finalize_and_restore_independently() {
    let fixture = tempfile::tempdir().unwrap();
    let service = Arc::new(magi_git::GitService::new());
    let contexts = magi_git::SessionCodeContextRegistry::default();
    let session = SessionId::new("multi-repository-agent");
    let workspace = Some(WorkspaceId::new("project"));
    let lease = LeaseId::new("agent-lease");
    let worker = WorkerId::new("agent-worker");
    let task = task_with_role("executor", TaskTier::ExecutionChain);
    let mut dispatcher = dispatcher_with_default_tool_surface()
        .with_git_context_runtime(service.clone(), contexts.clone());
    dispatcher.agent_worktree_root = fixture.path().join("agents");
    let roots = ["a", "b"].map(|name| {
        let root = fixture.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        git_fixture(&root, &["init", "-b", "main"]);
        git_fixture(&root, &["config", "user.name", "Magi Test"]);
        git_fixture(&root, &["config", "user.email", "magi@example.test"]);
        std::fs::write(root.join("same.txt"), name).unwrap();
        git_fixture(&root, &["add", "."]);
        git_fixture(&root, &["commit", "-m", "base"]);
        let observation = block_on_git(service.observe(&root)).unwrap();
        contexts.accept(session.as_str(), "project", vec![], &observation);
        observation.worktree_path
    });
    let copies = roots
        .iter()
        .map(|root| {
            let copy = dispatcher
                .resolve_task_execution_root(
                    &task,
                    &lease,
                    &session,
                    &workspace,
                    Some(root),
                    true,
                    Some(&worker),
                )
                .unwrap()
                .unwrap();
            std::fs::write(copy.join("same.txt"), format!("agent {}", root.display())).unwrap();
            copy
        })
        .collect::<Vec<_>>();
    assert_ne!(copies[0], copies[1]);
    let outputs = dispatcher
        .finalize_agent_worktree(&task, &lease, &session, &workspace, true)
        .unwrap();
    assert_eq!(outputs["agent_workspaces"].as_array().unwrap().len(), 2);
    let restored = magi_git::SessionCodeContextRegistry::default();
    restored.restore_after_restart(
        serde_json::from_str(&serde_json::to_string(&contexts.all()).unwrap()).unwrap(),
    );
    for (index, root) in roots.iter().enumerate() {
        assert!(!copies[index].exists());
        let context = restored.get_repository(session.as_str(), root).unwrap();
        let allocation = &context.agent_worktrees[0];
        assert!(!allocation.active);
        assert_eq!(allocation.changed_paths, vec!["same.txt"]);
        let result_head = allocation.result_head.as_deref().unwrap();
        assert_eq!(
            git_fixture(root, &["show", &format!("{result_head}:same.txt")]),
            format!("agent {}", root.display())
        );
        assert_eq!(
            std::fs::read_to_string(root.join("same.txt")).unwrap(),
            ["a", "b"][index]
        );
    }
}

#[test]
fn session_turn_model_configuration_failure_pauses_plan() {
    let dispatcher = dispatcher_with_default_tool_surface();
    let session_id = SessionId::new("session-model-config-failure");
    let accepted_at = UtcMillis(2_000);
    dispatcher
        .session_store
        .create_session(session_id.clone(), "model config failure")
        .expect("session should create");
    let (_mission_id, orchestrator_thread_id) =
        dispatcher
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || {
                MissionId::new("mission-model-config-failure")
            });
    let turn_id = "turn-model-config-failure";
    let attempt = match dispatcher
        .conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::Start(crate::TurnAdmission {
                turn_id: turn_id.to_string(),
                request_id: "request-model-config-failure".to_string(),
                request_fingerprint: "fingerprint-model-config-failure".to_string(),
                profile: crate::ExecutionProfile::Conversation,
            }),
        )
        .expect("model config failure Turn should start")
    {
        crate::CoordinatorCommandResult::Admission(crate::CoordinatorAdmission::Accepted(
            attempt,
        )) => attempt,
        other => panic!("unexpected model config failure admission: {other:?}"),
    };
    CanonicalTurnEventSink::for_store(&dispatcher.session_store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            None,
            magi_session_store::TimelineEntryInput::new(
                "timeline-model-config-failure",
                magi_session_store::TimelineEntryKind::UserMessage,
                "继续执行",
                accepted_at,
            ),
            magi_session_store::ActiveExecutionTurn {
                turn_id: turn_id.to_string(),
                turn_seq: accepted_at.0,
                accepted_at,
                completed_at: None,
                status: "accepted".to_string(),
                user_message: Some("继续执行".to_string()),
                items: vec![magi_session_store::ActiveExecutionTurnItem {
                    item_id: "user-model-config-failure".to_string(),
                    item_seq: 1,
                    kind: "user_message".to_string(),
                    status: "completed".to_string(),
                    source: "orchestrator".to_string(),
                    title: None,
                    content: Some("继续执行".to_string()),
                    task_id: None,
                    worker_id: None,
                    role_id: None,
                    tool_call_id: None,
                    tool_name: None,
                    tool_status: None,
                    tool_arguments: None,
                    tool_result: None,
                    tool_error: None,
                    request_id: Some("request-model-config-failure".to_string()),
                    user_message_id: None,
                    placeholder_message_id: None,
                    metadata: Default::default(),
                    timeline_entry_id: Some("timeline-model-config-failure".to_string()),
                    source_thread_id: orchestrator_thread_id,
                }],
            },
        )
        .expect("current Turn should persist through sink");
    dispatcher
        .conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt: attempt.clone(),
                status: crate::CoordinatorTurnStatus::Preparing,
            },
        )
        .expect("model config failure Turn should prepare");
    dispatcher
        .conversation_registry
        .turn_coordinator()
        .execute_command(
            &session_id,
            crate::TurnCommand::SetStatus {
                attempt,
                status: crate::CoordinatorTurnStatus::Running,
            },
        )
        .expect("model config failure Turn should run");
    CanonicalTurnEventSink::for_store(&dispatcher.session_store, None)
        .set_status_domain(&session_id, Some(turn_id), "running")
        .expect("current Turn running status should persist through sink");
    let plan_store =
        magi_plan::PlanStore::new(dispatcher.session_store.clone(), session_id.clone());
    plan_store
        .update(magi_plan::UpdatePlanInput {
            plan_id: None,
            expected_revision: Some(0),
            expected_goal_id: None,
            expected_goal_control_revision: None,
            language: "zh-CN".to_string(),
            explanation: None,
            plan: vec![magi_plan::UpdatePlanItemInput {
                item_id: Some("execute-current-step".to_string()),
                step: "执行当前步骤".to_string(),
                status: magi_core::PlanItemStatus::InProgress,
            }],
        })
        .expect("plan should persist");

    let result = dispatcher.execute_session_turn_for_test(SessionTurnExecutionRequest {
        project_directories: None,
        session_id,
        turn_id: "turn-model-config-failure".to_string(),
        workspace_id: None,
        prompt: "继续执行".to_string(),
        images: Vec::new(),
        context_references: Vec::new(),
        access_profile: magi_core::AccessProfile::Restricted,
        skill_name: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        command: None,
    });

    assert!(result.is_err());
    let plan = plan_store.snapshot().expect("plan should remain visible");
    assert_eq!(plan.state, magi_core::PlanState::Paused);
    assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
}

#[tokio::test]
async fn dispatch_thread_panic_is_reported_as_task_failure() {
    let mut dispatcher = dispatcher_with_default_tool_surface();
    let (sender, receiver) = std::sync::mpsc::channel();
    dispatcher.completion_sink = Arc::new(move |result| sender.send(result).unwrap());
    let task_id = TaskId::new("task-dispatch-thread-panic");
    let lease_id = LeaseId::new("lease-dispatch-thread-panic");
    let join = tokio::task::spawn_blocking(|| panic!("模拟模型执行线程 panic"));

    record_dispatch_join_outcome(dispatcher, task_id.clone(), lease_id.clone(), join).await;

    let results = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].task_id, task_id);
    assert_eq!(results[0].lease_id, lease_id);
    match &results[0].outcome {
        TaskOutcome::Failed { error } => {
            assert!(error.contains("模型执行线程异常退出"));
            assert!(error.contains("模拟模型执行线程 panic"));
        }
        TaskOutcome::Completed { .. } => panic!("panic 不得被记录为成功"),
        TaskOutcome::Yielded => panic!("panic 不得被记录为等待用户"),
        TaskOutcome::Cancelled => panic!("panic 不得被记录为用户取消"),
    }
}

fn test_mission_state_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("magi-{label}-{}", UtcMillis::now().0));
    std::fs::create_dir_all(&root).expect("test mission state root should create");
    root
}

#[test]
fn subagent_task_has_own_streaming_entry_without_writebacks() {
    let worker_task = task_with_role("executor", TaskTier::ExecutionChain);

    assert_eq!(
        task_streaming_entry_id(&worker_task),
        format!("timeline-streaming-{}", worker_task.task_id)
    );
}

#[test]
fn assemble_prompt_for_subagent_uses_package_and_current_file_facts_only() {
    let session_id = SessionId::new("session-context-prompt");
    let workspace_id = WorkspaceId::new("workspace-context-prompt");
    let session_store = SessionStore::from_state_for_test(magi_session_store::SessionStoreState {
        current_session_id: Some(session_id.clone()),
        sessions: vec![magi_session_store::SessionRecord {
            session_id: session_id.clone(),
            title: "Context prompt session".to_string(),
            status: magi_core::SessionLifecycleStatus::Active,
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
            message_count: None,
            workspace_id: Some(workspace_id.to_string()),
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: vec![magi_session_store::TimelineEntry {
            entry_id: "timeline-context-prompt".to_string(),
            session_id: session_id.clone(),
            kind: TimelineEntryKind::SystemNote,
            message: "prior session fact for runtime context".to_string(),
            occurred_at: UtcMillis(10),
        }],
        notifications: vec![],
        canonical_turns: vec![],
        inputs: vec![],
        goals: vec![],
        plans: vec![],
        thread_context_checkpoints: vec![],
        thread_registry: vec![],
        execution_sidecar_store: magi_session_store::SessionExecutionSidecarStoreState {
            runtime_sidecars: vec![],
        },
    });

    let file_summary_store = magi_context_runtime::FileSummaryStore::default();

    let context_runtime = ContextRuntime::with_runtime_sources(
        KnowledgeStore::new(),
        MemoryStore::new(),
        session_store,
        magi_context_runtime::SharedContextPool::default(),
        file_summary_store,
        magi_context_runtime::ProjectRecentTurnStore::default(),
    );
    let mut dispatcher = dispatcher_with_default_tool_surface()
        .with_context_runtime(Arc::new(context_runtime))
        .with_context_budget(ContextBudget {
            max_turns: 1,
            max_knowledge: 0,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 1,
        });
    dispatcher
        .session_store
        .create_session(session_id.clone(), "Context prompt session")
        .expect("dispatcher session should create");
    let (_, thread_id) =
        dispatcher
            .session_store
            .ensure_session_mission(&session_id, UtcMillis(15), || {
                MissionId::new("mission-context-prompt")
            });
    let directory = tempfile::tempdir().expect("tempdir");
    let file_path = directory.path().join("src-lib.rs");
    std::fs::write(&file_path, "Important file summary from current workspace.")
        .expect("write file fact");
    dispatcher.session_store = Arc::new(crate::test_history::with_messages(
        &dispatcher.session_store,
        &thread_id,
        vec![magi_session_store::ThreadChatMessage {
            input_request_id: None,
            role: "tool".to_string(),
            content: Some(
                serde_json::json!({
                    "tool": "file_read",
                    "status": "succeeded",
                    "path": file_path,
                    "content_hash": magi_snapshot::path_content_hash(&file_path)
                        .expect("hash file fact"),
                    "content": "Important file summary from current workspace."
                })
                .to_string(),
            ),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-file-summary".to_string()),
            provider_context: Vec::new(),
        }],
        UtcMillis(20),
    ));
    let mut task = task_with_role("executor", TaskTier::ExecutionChain);
    task.runtime_payload = magi_core::TaskRuntimePayload::AgentContext {
        package: Box::new(magi_core::AgentContextPackage {
            package_id: "agent-context-prompt".to_string(),
            revision: 1,
            parent_task_id: TaskId::new("task-parent-context-prompt"),
            summary: "只检查当前任务包".to_string(),
            constraints: vec!["不得自动读取主对话".to_string()],
            expected_output: "输出检查结论".to_string(),
            references: Vec::new(),
            fork: None,
            supplements: Vec::new(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }),
        accesses: Vec::new(),
    };

    let (prompt, context_messages, summary) =
        dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id.clone()), false);
    assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
    let context = context_messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(context.contains("[agent-context-package]"));
    assert!(context.contains("只检查当前任务包"));
    assert!(!context.contains("prior session fact for runtime context"));
    assert!(context.contains("Important file summary from current workspace"));
    let summary = summary.expect("context summary");
    assert_eq!(summary.used_turns, 0);
    assert_eq!(summary.used_file_summaries, 1);

    // 主会话线程的请求本来就带完整线程历史，文件读取摘要只是它的投影副本，不再装配。
    let (_, main_thread_context, main_thread_summary) =
        dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id.clone()), true);
    assert!(
        main_thread_context
            .iter()
            .filter_map(|message| message.content.as_deref())
            .all(
                |content| !content.contains("Important file summary from current workspace")
                    && !content.contains("[reference:file-summary]")
            ),
    );
    assert_eq!(main_thread_summary.expect("summary").used_file_summaries, 0);

    std::fs::write(&file_path, "Externally changed content.").expect("change projected file fact");
    let (prompt, context_messages, summary) =
        dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id), false);
    assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
    let context = context_messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!context.contains("Important file summary from current workspace"));
    assert_eq!(summary.expect("context summary").used_file_summaries, 0);
}

#[test]
fn assemble_prompt_without_workspace_does_not_query_default_workspace_context() {
    let session_id = SessionId::new("session-no-workspace-context");
    let knowledge_store = KnowledgeStore::new();
    knowledge_store.upsert(KnowledgeRecord {
        knowledge_id: "kb-default-workspace".to_string(),
        kind: KnowledgeKind::Learning,
        title: "Default workspace note".to_string(),
        content: "This note must not leak into workspace-less task prompts.".to_string(),
        tags: vec!["leak-check".to_string()],
        workspace_id: Some(WorkspaceId::new("default")),
        source_ref: Some("memory/default.md".to_string()),
        created_at: UtcMillis(1),
        updated_at: UtcMillis(1),
    });
    let dispatcher = dispatcher_with_default_tool_surface()
        .with_context_runtime(Arc::new(ContextRuntime::new(
            knowledge_store,
            MemoryStore::new(),
        )))
        .with_context_budget(ContextBudget {
            max_turns: 0,
            max_knowledge: 4,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        });
    let task = task_with_role("executor", TaskTier::ExecutionChain);

    let (prompt, context_messages, summary) =
        dispatcher.assemble_prompt(None, &task, &session_id, &None, false);
    assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
    let context = context_messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !context.contains("Default workspace note"),
        "缺少 workspace 时不得伪造 default workspace 并注入知识库内容"
    );
    assert!(summary.is_none());
}

#[test]
fn assemble_prompt_skips_matching_knowledge_when_task_has_no_knowledge_intent() {
    let session_id = SessionId::new("session-task-no-knowledge-intent");
    let workspace_id = WorkspaceId::new("workspace-task-no-knowledge-intent");
    let knowledge_store = KnowledgeStore::new();
    knowledge_store.upsert(KnowledgeRecord {
        knowledge_id: "learning-read-readme".to_string(),
        kind: KnowledgeKind::Learning,
        title: "读取 README 文件".to_string(),
        content: "读取 README 文件时使用 file_read。".to_string(),
        tags: vec!["readme".to_string()],
        workspace_id: Some(workspace_id.clone()),
        source_ref: Some("session:test".to_string()),
        created_at: UtcMillis(1),
        updated_at: UtcMillis(1),
    });
    let dispatcher = dispatcher_with_default_tool_surface()
        .with_context_runtime(Arc::new(ContextRuntime::new(
            knowledge_store,
            MemoryStore::new(),
        )))
        .with_context_budget(ContextBudget {
            max_turns: 0,
            max_knowledge: 4,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        });
    let mut task = task_with_role("executor", TaskTier::ExecutionChain);
    task.title = "读取 README 文件".to_string();
    task.goal = "读取 README 文件".to_string();

    let (prompt, context_messages, summary) =
        dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id), false);
    // 标题与目标相同（或只是其截断副本）时只发送一份目标。
    assert_eq!(prompt, task.goal);
    let context = context_messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!context.contains("读取 README 文件时使用 file_read"));
    assert_eq!(summary.expect("context summary").used_knowledge, 0);
    let events = dispatcher.event_bus.snapshot().recent_events;
    let diagnostic = events
        .iter()
        .find(|event| event.event_type == "knowledge.context.selected")
        .expect("knowledge diagnostic event");
    assert_eq!(diagnostic.payload["consumer"], "task_execution");
    assert_eq!(diagnostic.payload["decision"], "not_needed");
    assert_eq!(diagnostic.payload["injected_count"], 0);
}

#[test]
fn assemble_prompt_injects_full_relevant_adr_and_records_knowledge_id() {
    let session_id = SessionId::new("session-task-adr-context");
    let workspace_id = WorkspaceId::new("workspace-task-adr-context");
    let knowledge_store = KnowledgeStore::new();
    let adr_content = format!(
        "运行态采用单一事实源，事件事实负责写入，只读投影负责展示。{}最终约束是禁止多个状态源互相覆盖。",
        "背景信息。".repeat(24)
    );
    knowledge_store.upsert(KnowledgeRecord {
        knowledge_id: "adr-single-source".to_string(),
        kind: KnowledgeKind::Adr,
        title: "为什么运行态采用单一事实源架构".to_string(),
        content: adr_content,
        tags: vec!["架构".to_string(), "单一事实源".to_string()],
        workspace_id: Some(workspace_id.clone()),
        source_ref: Some("adr:runtime-state".to_string()),
        created_at: UtcMillis(1),
        updated_at: UtcMillis(1),
    });
    let dispatcher = dispatcher_with_default_tool_surface()
        .with_context_runtime(Arc::new(ContextRuntime::new(
            knowledge_store,
            MemoryStore::new(),
        )))
        .with_context_budget(ContextBudget {
            max_turns: 0,
            max_knowledge: 4,
            max_memory: 0,
            max_shared_items: 0,
            max_file_summaries: 0,
        });
    let mut task = task_with_role("executor", TaskTier::ExecutionChain);
    task.title = "分析运行态架构决策".to_string();
    task.goal = "说明为什么运行态采用单一事实源架构".to_string();

    let (prompt, context_messages, summary) =
        dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id), false);
    assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
    let context = context_messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(context.contains("[reference:knowledge:adr]"));
    assert!(context.contains("最终约束是禁止多个状态源互相覆盖"));
    let summary = summary.expect("context summary");
    assert_eq!(summary.used_knowledge, 1);
    assert_eq!(summary.knowledge_ids, vec!["adr-single-source".to_string()]);
    let events = dispatcher.event_bus.snapshot().recent_events;
    let diagnostic = events
        .iter()
        .find(|event| event.event_type == "knowledge.context.selected")
        .expect("knowledge diagnostic event");
    assert_eq!(diagnostic.payload["decision"], "injected");
    assert_eq!(
        diagnostic.payload["knowledge_ids"],
        serde_json::json!(["adr-single-source"])
    );
    assert_eq!(
        diagnostic.payload["result_kinds"],
        serde_json::json!(["adr"])
    );
    assert!(diagnostic.payload.get("content").is_none());
}

#[test]
fn tool_visibility_is_filtered_by_role_and_task_tier() {
    let registry = magi_agent_role::AgentRoleRegistry::load_default();
    let worker_task = task_with_role("executor", TaskTier::ExecutionChain);
    let coordinator_task = task_with_role("coordinator", TaskTier::ExecutionChain);

    assert!(!task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::AgentSpawn
    ));
    assert!(!task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::AgentWait
    ));
    assert!(task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::ContextSearch
    ));
    assert!(task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::ContextRead
    ));
    assert!(task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::ContextRequest
    ));
    assert!(task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::AgentSpawn
    ));
    assert!(task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::AgentWait
    ));
    assert!(task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::AgentSend
    ));
    assert!(!task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::ContextSearch
    ));
    assert!(task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::MemoryWrite
    ));
    assert!(task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::CreateGoal
    ));
    let mut nested_coordinator = coordinator_task.clone();
    nested_coordinator.parent_task_id = Some(TaskId::new("task-parent"));
    nested_coordinator.root_task_id = coordinator_task.task_id.clone();
    assert!(!task_can_see_builtin_tool(
        Some(&nested_coordinator),
        Some(&registry),
        BuiltinToolName::AgentSpawn
    ));
    let mut detached_coordinator = coordinator_task.clone();
    detached_coordinator.root_task_id = TaskId::new("task-other-root");
    assert!(!task_can_see_builtin_tool(
        Some(&detached_coordinator),
        Some(&registry),
        BuiltinToolName::AgentWait
    ));
    assert!(!task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::CreateGoal
    ));
    assert!(!task_can_see_builtin_tool(
        None,
        Some(&registry),
        BuiltinToolName::AgentSpawn
    ));
    assert!(!task_can_see_builtin_tool(
        None,
        Some(&registry),
        BuiltinToolName::AgentWait
    ));
    assert!(
        task_can_see_builtin_tool(None, Some(&registry), BuiltinToolName::CreateGoal),
        "主线 session turn 没有 Task 包装时也必须能调用 Goal 工具"
    );
    assert!(task_can_see_builtin_tool(
        None,
        Some(&registry),
        BuiltinToolName::UpdateGoal
    ));
    assert!(task_can_see_builtin_tool(
        None,
        Some(&registry),
        BuiltinToolName::GitBranchSwitch
    ));
    assert!(task_can_see_builtin_tool(
        Some(&coordinator_task),
        Some(&registry),
        BuiltinToolName::GitMerge
    ));
    assert!(!task_can_see_builtin_tool(
        Some(&worker_task),
        Some(&registry),
        BuiltinToolName::GitBranchSwitch
    ));
    assert!(
        task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::AskUserQuestion,
        ),
        "子代理可以请求用户澄清，但不能因此取得协调权限"
    );
}

#[tokio::test]
async fn dispatch_quiesce_waits_for_every_in_flight_dispatch_of_root() {
    let state = Arc::new(DispatchQuiesceState::default());
    let root_task_id = TaskId::new("root-dispatch-quiesce");
    let first = state.begin(root_task_id.clone());
    let second = state.begin(root_task_id.clone());

    let waiting = tokio::spawn({
        let state = Arc::clone(&state);
        let root_task_id = root_task_id.clone();
        async move { state.wait_for_root(&root_task_id).await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), waiting)
            .await
            .is_err(),
        "仍有 dispatch 时 quiesce 不应提前返回"
    );

    drop(first);
    let waiting = tokio::spawn({
        let state = Arc::clone(&state);
        let root_task_id = root_task_id.clone();
        async move { state.wait_for_root(&root_task_id).await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), waiting)
            .await
            .is_err(),
        "仍有第二个 dispatch 时 quiesce 不应提前返回"
    );

    drop(second);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state.wait_for_root(&root_task_id),
    )
    .await
    .expect("全部 dispatch 退出后 quiesce 应返回");
}

#[test]
fn dispatch_quiesce_task_scope_does_not_wait_for_its_caller_or_siblings() {
    let state = Arc::new(DispatchQuiesceState::default());
    let root = TaskId::new("scope-root");
    let child = TaskId::new("scope-child");
    let leaf = TaskId::new("scope-leaf");
    let _caller = state.begin_task(root.clone(), root.clone());
    let _sibling = state.begin_task(root.clone(), TaskId::new("scope-sibling"));
    let first = state.begin_task(root.clone(), child.clone());
    let second = state.begin_task(root.clone(), child.clone());
    let descendant = state.begin_task(root, leaf.clone());
    let ids = vec![child, leaf];
    assert!(!state.wait_for_tasks_blocking(&ids, std::time::Duration::ZERO));
    drop(first);
    assert!(!state.wait_for_tasks_blocking(&ids, std::time::Duration::ZERO));
    drop(second);
    assert!(!state.wait_for_tasks_blocking(&ids, std::time::Duration::ZERO));
    let (ready, waiting) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn({
        let state = state.clone();
        move || {
            ready.send(()).unwrap();
            state.wait_for_tasks_blocking(&ids, std::time::Duration::from_secs(2))
        }
    });
    waiting.recv().unwrap();
    drop(descendant);
    assert!(
        waiter.join().unwrap(),
        "子树退出后不能再等仍在运行的父任务或兄弟"
    );
}

#[tokio::test]
async fn dispatch_quiesce_uses_current_transfer_owner_without_retagging_in_flight_calls() {
    let store = magi_orchestrator::task_store::TaskStore::new();
    let mut root = task_with_role("coordinator", TaskTier::ExecutionChain);
    root.status = magi_core::TaskStatus::Running;
    let mut child = task_with_role("executor", TaskTier::ExecutionChain);
    child.root_task_id = root.task_id.clone();
    child.parent_task_id = Some(root.task_id.clone());
    child.executor_binding.as_mut().unwrap().agent_activation = Some(magi_core::AgentActivation {
        agent_id: child.task_id.clone(),
        session_id: SessionId::new("quiesce-transfer"),
        lifetime: magi_core::AgentLifetime::Session,
        generation: 1,
        previous_task_id: None,
    });
    store.insert_task(root.clone()).unwrap();
    store.insert_task(child.clone()).unwrap();
    let state = Arc::new(DispatchQuiesceState::default());
    let source = state.begin_task(root.task_id.clone(), root.task_id.clone());
    let work = state.begin_task(root.task_id.clone(), child.task_id.clone());
    drop(source);
    let waiting = state.wait_for_execution_scope(&store, &root.task_id);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiting)
            .await
            .is_err()
    );
    store
        .transfer_agent_subtree(
            &root.task_id,
            &child.task_id,
            magi_core::AgentRunTransfer {
                operation_id: "quiesce-handoff".into(),
                session_id: SessionId::new("quiesce-transfer"),
                source_turn_id: "source-turn".into(),
                transferred_by: root.task_id.clone(),
                activation_generation: 1,
                owner: magi_core::AgentRunOwner::Session,
                transferred_at: UtcMillis::now(),
            },
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), &mut waiting)
        .await
        .expect("原拥有方不能等待已转交执行");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            state.wait_for_execution_scope(&store, &child.task_id)
        )
        .await
        .is_err()
    );
    drop(work);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state.wait_for_execution_scope(&store, &child.task_id),
    )
    .await
    .expect("新拥有方须等待原线程真实退出");
}

#[test]
fn agent_mode_prompt_matches_role_and_current_tool_surface() {
    let dispatcher = dispatcher_with_default_tool_surface();
    let session_id = SessionId::new("session-multi-agent-mode");
    let workspace_id = None;

    let coordinator_task = task_with_role("coordinator", TaskTier::ExecutionChain);
    let prompt_for_task = |task: &Task, skill_name: Option<&str>| {
        let tools = dispatcher.build_tool_definitions(
            task,
            skill_name,
            AccessProfile::FullAccess,
            workspace_id.as_ref(),
        );
        crate::prompt_utils::current_agent_mode_prompt(
            task,
            dispatcher.execution_registry.execution_admission().limits(),
            &dispatcher.agent_role_registry,
            &tools,
        )
        .unwrap()
    };
    let (_, coordinator_context, _) =
        dispatcher.assemble_prompt(None, &coordinator_task, &session_id, &workspace_id, false);
    assert!(
        coordinator_context.iter().all(|message| {
            !message
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("多代理协作（root coordinator 必须遵守）")
        }),
        "运行时派发规则不能混入任务事实"
    );
    let coordinator_prompt = prompt_for_task(&coordinator_task, None);
    assert!(
        coordinator_prompt.contains("多代理协作（root coordinator 必须遵守）"),
        "root coordinator prompt 必须包含多代理协作规则: {coordinator_prompt}"
    );
    assert!(
        coordinator_prompt.contains("明确要求真实代理执行具体工作")
            && coordinator_prompt.contains("必须通过 agent_spawn 创建真实代理"),
        "明确 subagent 请求必须被约束为真实 agent_spawn"
    );
    assert!(
        coordinator_prompt.contains("runtime_internal=true")
            && coordinator_prompt.contains("可派发角色"),
        "root coordinator 必须知道代理工具可直接调用以及可派发角色"
    );

    let worker_task = task_with_role("executor", TaskTier::ExecutionChain);
    let worker_prompt = prompt_for_task(&worker_task, None);
    assert!(
        worker_prompt.contains("子代理模式（当前模式：worker；worker 必须遵守）")
            && worker_prompt.contains("不要继续创建代理"),
        "worker prompt 必须说明自身不能继续分派"
    );
    assert!(
        !worker_prompt.contains("多代理协作（root coordinator 必须遵守）"),
        "worker prompt 不能收到 root coordinator 派发策略"
    );

    for tool_policy in ["no_tools", "deny_spawn", "allow_file_read"] {
        let mut restricted = coordinator_task.clone();
        let policy = restricted.policy_snapshot.as_mut().unwrap();
        match tool_policy {
            "no_tools" => policy.command_mode = "no_tools".into(),
            "deny_spawn" => policy.denied_tools = vec!["agent_spawn".into()],
            _ => policy.allowed_tools = vec!["file_read".into()],
        }
        let prompt = prompt_for_task(&restricted, None);
        assert!(
            prompt.contains("本轮 tools 未提供 agent_spawn"),
            "{tool_policy}: {prompt}"
        );
        assert!(!prompt.contains("必须通过 agent_spawn 创建真实代理"));
        assert!(!prompt.contains("可派发角色"));
    }

    // 使用真实内置角色，检查角色提示、运行时提示和工具面不会相互矛盾。
    for role_id in ["architect", "executor", "reviewer", "explorer", "tester"] {
        let mut delegated = task_with_role(role_id, TaskTier::ExecutionChain);
        delegated.parent_task_id = Some(coordinator_task.task_id.clone());
        delegated.root_task_id = coordinator_task.task_id.clone();
        delegated
            .executor_binding
            .as_mut()
            .unwrap()
            .delegation_allowed = true;
        let prompt = prompt_for_task(&delegated, None);
        let role = dispatcher.agent_role_registry.get(role_id).unwrap();
        assert!(prompt.contains("受委派代理模式"), "{role_id}: {prompt}");
        assert!(prompt.contains("获授委派能力不要求创建子代理"));
        assert!(!prompt.contains("不要继续创建代理"));
        assert!(!role.system_prompt.contains("不要继续创建代理"));
        let names = dispatcher
            .build_tool_definitions(
                &delegated,
                None,
                magi_core::AccessProfile::Restricted,
                Some(&WorkspaceId::new("delegation-prompt-workspace")),
            )
            .into_iter()
            .map(|tool| tool.function.name)
            .collect::<Vec<_>>();
        for tool in [
            "agent_spawn",
            "agent_wait",
            "agent_cancel",
            "agent_send",
            "agent_apply",
            "ask_user_question",
        ] {
            assert!(
                names.iter().any(|name| name == tool),
                "{role_id} 缺少 {tool}"
            );
        }
        for tool in [
            "create_goal",
            "update_plan",
            "memory_write",
            "git_branch_switch",
        ] {
            assert!(
                !names.iter().any(|name| name == tool),
                "{role_id} 不应拥有 {tool}"
            );
        }
    }
}

#[test]
fn read_only_coordinator_surface_keeps_internal_coordination_tools() {
    let dispatcher = dispatcher_with_default_tool_surface();
    let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
    task.policy_snapshot
        .as_mut()
        .expect("policy")
        .access_profile = magi_core::AccessProfile::ReadOnly;

    let names = dispatcher
        .build_tool_definitions(
            &task,
            None,
            magi_core::AccessProfile::Restricted,
            Some(&WorkspaceId::new("test-workspace")),
        )
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<Vec<_>>();

    for hidden in [
        "file_write",
        "file_patch",
        "apply_patch",
        "file_remove",
        "file_mkdir",
        "file_copy",
        "file_move",
        "memory_write",
    ] {
        assert!(
            !names.iter().any(|name| name == hidden),
            "read-only 工具面不应暴露写工具 {hidden}: {names:?}"
        );
    }
    assert!(names.iter().any(|name| name == "file_read"));
    assert!(names.iter().any(|name| name == "search_text"));
    assert!(names.iter().any(|name| name == "shell_exec"));
    assert!(names.iter().any(|name| name == "agent_spawn"));
    assert!(names.iter().any(|name| name == "agent_wait"));
    for internal in ["get_goal", "create_goal", "update_goal", "update_plan"] {
        assert!(
            names.iter().any(|name| name == internal),
            "只读访问只限制外部副作用，不能屏蔽内部协调工具 {internal}: {names:?}"
        );
    }
}

#[test]
fn full_access_with_empty_allowed_tools_keeps_shell_exec_visible() {
    let dispatcher = dispatcher_with_default_tool_surface();
    let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
    let policy = task.policy_snapshot.as_mut().expect("policy");
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy.command_mode = "full".to_string();
    policy.allowed_tools.clear();
    policy.denied_tools.clear();

    let names = dispatcher
        .build_tool_definitions(
            &task,
            None,
            magi_core::AccessProfile::FullAccess,
            Some(&WorkspaceId::new("test-workspace")),
        )
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "shell_exec"));
    assert!(names.iter().any(|name| name == "file_write"));
    assert!(names.iter().any(|name| name == "apply_patch"));
}

#[test]
fn tool_surface_separates_coordinator_worker_and_delegation_grant() {
    let root = task_with_role("coordinator", TaskTier::ExecutionChain);
    let mut worker = root.clone();
    worker.parent_task_id = Some(root.task_id.clone());
    worker.task_id = TaskId::new("worker-surface");
    let mut delegator = worker.clone();
    delegator
        .executor_binding
        .as_mut()
        .unwrap()
        .delegation_allowed = true;
    let dispatcher = dispatcher_with_default_tool_surface();
    for (task, can_spawn, can_plan) in [
        (&root, true, true),
        (&worker, false, false),
        (&delegator, true, false),
        (&worker, false, false),
    ] {
        let definitions =
            dispatcher.build_tool_definitions(task, None, AccessProfile::FullAccess, None);
        assert_eq!(
            definitions
                .iter()
                .any(|tool| tool.function.name == "agent_spawn"),
            can_spawn
        );
        assert_eq!(
            definitions
                .iter()
                .any(|tool| tool.function.name == "update_plan"),
            can_plan
        );
    }
}

#[test]
fn coordinator_model_tool_surface_contains_agent_contracts() {
    let dispatcher = dispatcher_with_default_tool_surface();
    let task = task_with_role("coordinator", TaskTier::ExecutionChain);
    let definitions = dispatcher.build_tool_definitions(
        &task,
        None,
        magi_core::AccessProfile::Restricted,
        Some(&WorkspaceId::new("test-workspace")),
    );

    for name in ["agent_spawn", "agent_send", "agent_wait"] {
        let definition = definitions
            .iter()
            .find(|definition| definition.function.name == name)
            .unwrap_or_else(|| panic!("coordinator model request must contain {name}"));
        assert_eq!(definition.kind, "function");
        assert!(
            !definition.function.description.trim().is_empty(),
            "{name} definition must be model-facing"
        );
        assert_eq!(definition.function.parameters["type"], "object");
    }
}

#[test]
fn access_profile_tool_surface_distinguishes_coordinator_and_subagent_roles() {
    let dispatcher = dispatcher_with_default_tool_surface();

    for access_profile in [
        magi_core::AccessProfile::ReadOnly,
        magi_core::AccessProfile::Restricted,
        magi_core::AccessProfile::FullAccess,
    ] {
        let mut coordinator = task_with_role("coordinator", TaskTier::ExecutionChain);
        coordinator
            .policy_snapshot
            .as_mut()
            .expect("coordinator policy")
            .access_profile = access_profile;
        let coordinator_names = dispatcher
            .build_tool_definitions(
                &coordinator,
                None,
                access_profile,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();
        assert!(
            coordinator_names.iter().any(|name| name == "agent_spawn"),
            "主线在 {access_profile:?} 模式下都必须能够创建继承同模式的子代理"
        );

        let mut worker = task_with_role("executor", TaskTier::ExecutionChain);
        worker
            .policy_snapshot
            .as_mut()
            .expect("worker policy")
            .access_profile = access_profile;
        let worker_names = dispatcher
            .build_tool_definitions(
                &worker,
                None,
                access_profile,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();
        assert!(worker_names.iter().any(|name| name == "file_read"));
        assert!(worker_names.iter().any(|name| name == "shell_exec"));
        assert!(
            worker_names.iter().any(|name| name == "ask_user_question"),
            "子代理应在 {access_profile:?} 模式下看到自己的提问入口"
        );
        assert!(
            !worker_names.iter().any(|name| name == "agent_spawn"),
            "子代理在 {access_profile:?} 模式下都不能递归创建代理"
        );
        assert_eq!(
            worker_names.iter().any(|name| name == "file_write"),
            access_profile != magi_core::AccessProfile::ReadOnly,
            "子代理文件写入工具可见性必须与访问模式一致"
        );
    }
}

#[test]
fn ready_browser_host_is_exposed_to_regular_execution_worker() {
    let dispatcher = dispatcher_with_ready_browser_tool_surface();
    let worker = task_with_role("executor", TaskTier::ExecutionChain);
    let names = dispatcher
        .build_tool_definitions(
            &worker,
            None,
            magi_core::AccessProfile::Restricted,
            Some(&WorkspaceId::new("test-workspace")),
        )
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "shell_exec"));
    for browser_tool in [
        "browser_navigate",
        "browser_snapshot",
        "browser_click",
        "browser_type",
        "browser_press",
        "browser_scroll",
        "browser_screenshot",
        "browser_tabs",
        "browser_viewport",
    ] {
        assert!(
            names.iter().any(|name| name == browser_tool),
            "普通执行 Worker 未发现 {browser_tool}: {names:?}"
        );
    }
}

#[test]
fn personal_transferred_agent_keeps_execution_root_without_git_context_registry() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("retained.txt"), "original output").unwrap();
    let session = SessionId::new("personal-agent-root");
    let mut task = task_with_role("executor", TaskTier::ExecutionChain);
    task.executor_binding.as_mut().unwrap().agent_run_transfer =
        Some(magi_core::AgentRunTransfer {
            operation_id: "personal-transfer".into(),
            session_id: session.clone(),
            source_turn_id: "original-turn".into(),
            transferred_by: task.root_task_id.clone(),
            activation_generation: 1,
            owner: magi_core::AgentRunOwner::Session,
            transferred_at: UtcMillis::now(),
        });
    let mut dispatcher = dispatcher_with_default_tool_surface();
    dispatcher.session_code_contexts = None;
    let inherited = dispatcher
        .resolve_task_execution_root(
            &task,
            &LeaseId::new("personal-resumed-lease"),
            &session,
            &None,
            Some(&root.path().to_path_buf()),
            true,
            Some(&WorkerId::new("personal-worker")),
        )
        .unwrap()
        .unwrap();
    assert_eq!(inherited, root.path());
    assert_eq!(
        std::fs::read_to_string(inherited.join("retained.txt")).unwrap(),
        "original output"
    );
}

#[test]
fn task_tool_surface_separates_project_tools_from_personal_execution() {
    let dispatcher = dispatcher_with_ready_browser_tool_surface();
    let task = task_with_role("executor", TaskTier::ExecutionChain);
    let names = dispatcher
        .build_tool_definitions(&task, None, magi_core::AccessProfile::Restricted, None)
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<Vec<_>>();

    for hidden in [
        "memory_write",
        "knowledge_query",
        "search_semantic",
        "code_symbols",
    ] {
        assert!(
            !names.iter().any(|name| name == hidden),
            "无 workspace 的任务工具目录不能暴露 {hidden}: {names:?}"
        );
    }
    assert!(
        names.iter().any(|name| name == "browser_navigate"),
        "无 workspace 的任务工具目录必须保留浏览器工具: {names:?}"
    );
    for local in [
        "file_read",
        "file_write",
        "shell_exec",
        "search_text",
        "view_image",
    ] {
        assert!(
            names.iter().any(|name| name == local),
            "个人会话需要通用工具 {local}"
        );
    }
}

#[test]
fn personal_session_catalog_checks_every_builtin_for_mainline_and_worker() {
    let dispatcher = dispatcher_with_ready_browser_tool_surface();
    let root = task_with_role("coordinator", TaskTier::ExecutionChain);
    let mut worker = root.clone();
    worker.parent_task_id = Some(root.task_id.clone());
    worker.task_id = TaskId::new("personal-catalog-worker");
    for (task, is_worker) in [(&root, false), (&worker, true)] {
        let names = dispatcher
            .build_tool_definitions(task, None, AccessProfile::FullAccess, None)
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<std::collections::HashSet<_>>();
        for tool in BuiltinToolName::ALL {
            let expected = match tool.as_str() {
                    "search_semantic"
                    | "knowledge_query"
                    | "knowledge_graph_query"
                    | "code_symbols"
                    | "memory_write"
                    // 角色管理工具只在声明它们的 Skill 激活时开放，默认工具面里没有。
                    | "agent_role_get"
                    | "agent_role_save"
                    | "agent_apply"
                    | "process_launch"
                    | "process_read"
                    | "process_write"
                    | "process_kill"
                    | "process_list" => false,
                    "context_search" | "context_read" | "context_request" => is_worker,
                    "agent_spawn"
                    | "agent_send"
                    | "agent_cancel"
                    | "agent_wait"
                    | "get_goal"
                    | "create_goal"
                    | "update_goal"
                    | "update_plan"
                    | "git_branch_create"
                    | "git_branch_switch"
                    | "git_pull"
                    | "git_push"
                    | "git_merge_preview"
                    | "git_merge"
                    | "git_branch_delete"
                    | "git_worktree_create"
                    | "git_worktree_remove" => !is_worker,
                    _ => true,
                };
            assert_eq!(
                names.contains(tool.as_str()),
                expected,
                "personal worker={is_worker}: {}",
                tool.as_str()
            );
        }
    }
}

#[test]
fn read_only_command_mode_hides_write_tools_even_with_full_access_profile() {
    let dispatcher = dispatcher_with_default_tool_surface();
    let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
    let policy = task.policy_snapshot.as_mut().expect("policy");
    policy.access_profile = magi_core::AccessProfile::FullAccess;
    policy.command_mode = "read_only".to_string();

    let names = dispatcher
        .build_tool_definitions(
            &task,
            None,
            magi_core::AccessProfile::FullAccess,
            Some(&WorkspaceId::new("test-workspace")),
        )
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "file_read"));
    assert!(!names.iter().any(|name| name == "file_write"));
    assert!(names.iter().any(|name| name == "agent_spawn"));
}

#[test]
fn active_skill_custom_bindings_are_exposed_in_tool_surface() {
    let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性风险。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["review".to_string()],
            },
            restrict_standard_tools: true,
            allowed_tools: vec![],
            custom_tool_bindings: vec![magi_skill_runtime::CustomToolBinding {
                binding_id: "review-mcp".to_string(),
                tool_name: "echo.describe".to_string(),
                description: "回显描述".to_string(),
                bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp,
                dispatch_action: magi_bridge_client::BridgeDispatchAction::McpToolCall,
                bridge_target: "loopback-mcp".to_string(),
            }],
            prompt_priority: 50,
        });
        magi_skill_runtime::SkillRuntime::new(registry)
    }));
    let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
    let workspace = WorkspaceId::new("test-workspace");
    let definitions = dispatcher.build_tool_definitions(
        &task,
        Some("code-review"),
        magi_core::AccessProfile::Restricted,
        Some(&workspace),
    );
    let names = definitions
        .iter()
        .map(|definition| definition.function.name.clone())
        .collect::<Vec<_>>();

    assert!(
        names
            .iter()
            .any(|name| name == "skill__code-review__review-mcp"),
        "active skill custom binding should surface as callable tool"
    );
    assert!(names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
    let reference = definitions
        .iter()
        .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
        .unwrap();
    assert_eq!(
        reference.function.parameters["properties"]["mode"]["enum"],
        serde_json::json!(["reference"])
    );
    assert!(
        !names.iter().any(|name| name == "file_read"),
        "active skill 没有声明 allowed_tools 时不应暴露普通内置工具"
    );
    assert!(
        !names.iter().any(|name| name == "shell_exec"),
        "active skill 没有声明 allowed_tools 时不应暴露 shell_exec"
    );
    let skills = dispatcher.skill_runtime.as_ref().unwrap().registry();
    let original = skills.get("code-review").unwrap();
    let mut tightened = original.clone();
    tightened.custom_tool_bindings.clear();
    skills.register(tightened);
    let tightened_tools = dispatcher.build_tool_definitions(
        &task,
        Some("code-review"),
        AccessProfile::Restricted,
        Some(&workspace),
    );
    assert_eq!(
        tightened_tools
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>(),
        ["update_execution_steps", "skill_apply"],
        "Skill 收紧后只保留执行步骤和只读说明加载，不能复活原自定义工具"
    );
    skills.register(original);
    for denied in [false, true] {
        let policy = task.policy_snapshot.as_mut().unwrap();
        policy.allowed_tools = if denied {
            vec![]
        } else {
            vec!["file_read".into()]
        };
        policy.denied_tools = if denied {
            vec!["skill__code-review__review-mcp".into()]
        } else {
            vec![]
        };
        let tools = dispatcher.build_tool_definitions(
            &task,
            Some("code-review"),
            AccessProfile::Restricted,
            None,
        );
        assert!(
            !tools
                .iter()
                .any(|tool| tool.function.name == "skill__code-review__review-mcp"),
            "Task 白名单和明确禁止必须约束 Skill 自定义工具，不能只在执行时拒绝"
        );
    }
}

#[test]
fn active_skill_is_injected_directly_and_only_allows_reference_loading() {
    let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "direct-skill".to_string(),
            title: "直接 Skill".to_string(),
            instruction: "直接注入。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "workflow".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: false,
            allowed_tools: vec![],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        magi_skill_runtime::SkillRuntime::new(registry)
    }));

    let definitions = dispatcher.build_tool_definitions(
        &task_with_role("coordinator", TaskTier::ExecutionChain),
        Some("direct-skill"),
        magi_core::AccessProfile::Restricted,
        Some(&WorkspaceId::new("test-workspace")),
    );
    let names = definitions
        .iter()
        .map(|definition| definition.function.name.clone())
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
    let reference = definitions
        .iter()
        .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
        .unwrap();
    assert_eq!(
        reference.function.parameters["properties"]["mode"]["enum"],
        serde_json::json!(["reference"])
    );
    assert!(
        names.iter().any(|name| name == "file_read"),
        "未声明 allowed_tools 的 prompt-only Skill 应继承标准只读工具"
    );
}

#[test]
fn live_mcp_tools_are_exposed_on_the_model_tool_surface() {
    let mut dispatcher = dispatcher_with_default_tool_surface();
    let registry = dispatcher
        .tool_registry
        .take()
        .expect("test dispatcher should own a tool registry")
        .with_external_tool_catalog_provider(Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                instruction_skill_count: 0,
                mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                    server_id: "repo-tools".to_string(),
                    server_name: "Repository Tools".to_string(),
                    model_tool_name: "mcp__repo-tools__inspect".to_string(),
                    tool_name: "inspect".to_string(),
                    description: "Inspect repository".to_string(),
                    read_only: false,
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } }
                    }),
                }],
                ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
            }
        }));
    dispatcher.tool_registry = Some(registry);

    let definitions = dispatcher.build_tool_definitions(
        &task_with_role("coordinator", TaskTier::ExecutionChain),
        None,
        magi_core::AccessProfile::Restricted,
        Some(&WorkspaceId::new("test-workspace")),
    );
    let mcp = definitions
        .iter()
        .find(|definition| definition.function.name == "mcp__repo-tools__inspect");
    assert!(
        mcp.is_none(),
        "MCP schema 默认延迟加载，首轮不能占用完整工具面"
    );
}

#[test]
fn prompt_only_skill_keeps_live_mcp_tools() {
    let mut dispatcher = dispatcher_with_default_tool_surface();
    let registry = dispatcher
        .tool_registry
        .take()
        .expect("test dispatcher should own a tool registry")
        .with_external_tool_catalog_provider(Arc::new(|| {
            magi_tool_runtime::ExternalToolCatalogSnapshot {
                instruction_skill_count: 0,
                mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                    server_id: "repo-tools".to_string(),
                    server_name: "Repository Tools".to_string(),
                    model_tool_name: "mcp__repo-tools__inspect".to_string(),
                    tool_name: "inspect".to_string(),
                    description: "Inspect repository".to_string(),
                    read_only: false,
                    input_schema: serde_json::json!({ "type": "object" }),
                }],
                ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
            }
        }));
    dispatcher.tool_registry = Some(registry);
    dispatcher = dispatcher.with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "prompt-only".to_string(),
            title: "Prompt Only".to_string(),
            instruction: "先取证，再回答。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "workflow".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: false,
            allowed_tools: vec![],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        magi_skill_runtime::SkillRuntime::new(registry)
    }));

    let definitions = dispatcher.build_tool_definitions(
        &task_with_role("coordinator", TaskTier::ExecutionChain),
        Some("prompt-only"),
        magi_core::AccessProfile::Restricted,
        Some(&WorkspaceId::new("test-workspace")),
    );
    let names = definitions
        .iter()
        .map(|definition| definition.function.name.clone())
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "file_read"));
    assert!(!names.iter().any(|name| name == "mcp__repo-tools__inspect"));
    assert!(names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
    let reference = definitions
        .iter()
        .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
        .unwrap();
    assert_eq!(
        reference.function.parameters["properties"]["mode"]["enum"],
        serde_json::json!(["reference"])
    );
}

#[test]
fn selected_skill_short_name_resolves_before_prompt_and_tool_surface_build() {
    let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "owner/repo/skills/code-review".to_string(),
            title: "代码审查".to_string(),
            instruction: "检查稳定性。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "quality".to_string(),
                tags: vec![],
            },
            restrict_standard_tools: true,
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        magi_skill_runtime::SkillRuntime::new(registry)
    }));

    assert_eq!(
        dispatcher.resolve_registered_skill_id(Some("code-review")),
        Ok(Some("owner/repo/skills/code-review".to_string()))
    );
    let registry = dispatcher.skill_runtime.as_ref().unwrap().registry();
    let mut duplicate = registry.get("owner/repo/skills/code-review").unwrap();
    duplicate.skill_id = "another/repo/code-review".into();
    registry.register(duplicate);
    assert!(
        dispatcher
            .resolve_registered_skill_id(Some("code-review"))
            .unwrap_err()
            .contains("skill_ambiguous")
    );
    registry.replace([]);
    assert!(
        dispatcher
            .resolve_registered_skill_id(Some("owner/repo/skills/code-review"))
            .unwrap_err()
            .contains("skill_unavailable")
    );
    let dispatcher = dispatcher_with_default_tool_surface();
    assert!(
        dispatcher
            .resolve_registered_skill_id(Some("code-review"))
            .unwrap_err()
            .contains("skill_unavailable")
    );
    for absent in [None, Some(" ")] {
        assert_eq!(dispatcher.resolve_registered_skill_id(absent), Ok(None));
    }
}

#[test]
fn read_only_tool_surface_hides_mcp_skill_bindings() {
    let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "mixed-skill".to_string(),
            title: "混合 Skill".to_string(),
            instruction: "提供只读模型辅助和 MCP 外接工具。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["mixed".to_string()],
            },
            restrict_standard_tools: true,
            allowed_tools: vec![],
            custom_tool_bindings: vec![
                magi_skill_runtime::CustomToolBinding {
                    binding_id: "mcp-tool".to_string(),
                    tool_name: "echo.inspect".to_string(),
                    description: "MCP 检查".to_string(),
                    bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp,
                    dispatch_action: magi_bridge_client::BridgeDispatchAction::McpToolCall,
                    bridge_target: "loopback-mcp".to_string(),
                },
                magi_skill_runtime::CustomToolBinding {
                    binding_id: "model-tool".to_string(),
                    tool_name: "model.summarize".to_string(),
                    description: "模型摘要".to_string(),
                    bridge_kind: magi_bridge_client::BridgeBindingKind::Model,
                    dispatch_action: magi_bridge_client::BridgeDispatchAction::ModelPrompt,
                    bridge_target: "loopback-model".to_string(),
                },
            ],
            prompt_priority: 50,
        });
        magi_skill_runtime::SkillRuntime::new(registry)
    }));
    let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
    task.policy_snapshot
        .as_mut()
        .expect("policy")
        .access_profile = magi_core::AccessProfile::ReadOnly;

    let names = dispatcher
        .build_tool_definitions(
            &task,
            Some("mixed-skill"),
            magi_core::AccessProfile::ReadOnly,
            Some(&WorkspaceId::new("test-workspace")),
        )
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<Vec<_>>();

    assert!(
        !names
            .iter()
            .any(|name| name == "skill__mixed-skill__mcp-tool"),
        "read-only 工具面不应暴露 MCP Skill 工具: {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|name| name == "skill__mixed-skill__model-tool"),
        "read-only 工具面仍可保留模型型 Skill 工具: {names:?}"
    );
}

#[test]
fn agent_spawn_requires_wait_and_cancel_in_the_effective_tool_surface() {
    for missing in ["agent_wait", "agent_cancel"] {
        let dispatcher = dispatcher_with_default_tool_surface();
        let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
        task.policy_snapshot.as_mut().unwrap().denied_tools = vec![missing.into()];
        let tools = dispatcher.build_tool_definitions(&task, None, AccessProfile::Restricted, None);
        assert!(
            !tools.iter().any(|tool| tool.function.name == "agent_spawn"),
            "missing {missing} must prevent creation of an unmanageable child"
        );
        assert!(tools.iter().any(|tool| tool.function.name == "agent_send"));
        let prompt = crate::prompt_utils::current_agent_mode_prompt(
            &task,
            dispatcher.execution_registry.execution_admission().limits(),
            &dispatcher.agent_role_registry,
            &tools,
        )
        .unwrap();
        assert!(prompt.contains("本轮 tools 未提供 agent_spawn"));
        assert!(!prompt.contains("可派发角色"));
    }
}

#[test]
fn role_tools_enter_the_tool_surface_only_when_an_active_skill_declares_them() {
    let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        let skill =
            |id: &str, restrict: bool, tools: Vec<String>| magi_skill_runtime::SkillDefinition {
                description: String::new(),
                skill_id: id.to_string(),
                title: id.to_string(),
                instruction: "管理角色。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "agent".to_string(),
                    tags: vec![],
                },
                restrict_standard_tools: restrict,
                allowed_tools: tools,
                custom_tool_bindings: vec![],
                prompt_priority: 50,
            };
        registry.register(skill(
            "role-skill",
            true,
            vec![
                "agent_role_get".to_string(),
                "agent_role_save".to_string(),
                "ask_user_question".to_string(),
            ],
        ));
        registry.register(skill("plain-skill", false, vec![]));
        magi_skill_runtime::SkillRuntime::new(registry)
    }));
    let names = |task: &Task, skill: Option<&str>, profile: AccessProfile| {
        dispatcher
            .build_tool_definitions(
                task,
                skill,
                profile,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>()
    };
    let coordinator = task_with_role("coordinator", TaskTier::ExecutionChain);

    let declared = names(&coordinator, Some("role-skill"), AccessProfile::Restricted);
    assert!(declared.iter().any(|name| name == "agent_role_get"));
    assert!(declared.iter().any(|name| name == "agent_role_save"));
    // 只读模式隐藏写入工具，读取仍可用。
    let mut read_only_task = coordinator.clone();
    read_only_task
        .policy_snapshot
        .as_mut()
        .unwrap()
        .access_profile = AccessProfile::ReadOnly;
    let read_only = names(&read_only_task, Some("role-skill"), AccessProfile::ReadOnly);
    assert!(read_only.iter().any(|name| name == "agent_role_get"));
    assert!(!read_only.iter().any(|name| name == "agent_role_save"));
    // 没有 Skill、或 Skill 没有声明时，默认工具面里不出现，不能靠“没有限制”放行。
    for skill in [None, Some("plain-skill")] {
        let undeclared = names(&coordinator, skill, AccessProfile::FullAccess);
        assert!(
            !undeclared
                .iter()
                .any(|name| name.starts_with("agent_role_")),
            "{skill:?} 不应开放角色工具"
        );
    }
    // 子代理即使在声明它的 Skill 下也不能管理角色。
    let worker = task_with_role("executor", TaskTier::ExecutionChain);
    let worker_names = names(&worker, Some("role-skill"), AccessProfile::Restricted);
    assert!(
        !worker_names
            .iter()
            .any(|name| name.starts_with("agent_role_"))
    );
}

#[test]
fn active_skill_allowed_tools_filter_builtin_tool_surface() {
    let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
        let registry = magi_skill_runtime::SkillRegistry::new();
        registry.register(magi_skill_runtime::SkillDefinition {
            description: String::new(),
            skill_id: "read-only-skill".to_string(),
            title: "只读 Skill".to_string(),
            instruction: "只能读取文件。".to_string(),
            metadata: magi_skill_runtime::SkillMetadata {
                category: "quality".to_string(),
                tags: vec!["read".to_string()],
            },
            restrict_standard_tools: true,
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 50,
        });
        magi_skill_runtime::SkillRuntime::new(registry)
    }));
    let task = task_with_role("coordinator", TaskTier::ExecutionChain);

    let definitions = dispatcher.build_tool_definitions(
        &task,
        Some("read-only-skill"),
        magi_core::AccessProfile::Restricted,
        Some(&WorkspaceId::new("test-workspace")),
    );
    let names = definitions
        .iter()
        .map(|definition| definition.function.name.clone())
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "file_read"));
    assert!(names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
    let reference = definitions
        .iter()
        .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
        .unwrap();
    assert_eq!(
        reference.function.parameters["properties"]["mode"]["enum"],
        serde_json::json!(["reference"])
    );
    assert!(!names.iter().any(|name| name == "search_text"));
    assert!(!names.iter().any(|name| name == "shell_exec"));
    assert!(!names.iter().any(|name| name == "agent_spawn"));
    // Goal 生命周期工具继续由宿主拥有；重建目录不能被 Skill 隐藏，
    // Task 的显式禁止仍须生效。
    let mut goal_task = task.clone();
    goal_task.executor_binding.as_mut().unwrap().goal_mode = true;
    goal_task
        .policy_snapshot
        .as_mut()
        .unwrap()
        .denied_tools
        .push("update_plan".into());
    let goal_tools = dispatcher.build_tool_definitions(
        &goal_task,
        Some("read-only-skill"),
        AccessProfile::Restricted,
        Some(&WorkspaceId::new("test-workspace")),
    );
    for name in ["get_goal", "create_goal", "update_goal", "file_read"] {
        assert!(goal_tools.iter().any(|tool| tool.function.name == name));
    }
    assert!(
        !goal_tools
            .iter()
            .any(|tool| tool.function.name == "update_plan")
    );
    let prompt = crate::prompt_utils::current_agent_mode_prompt(
        &task,
        dispatcher.execution_registry.execution_admission().limits(),
        &dispatcher.agent_role_registry,
        &dispatcher.build_tool_definitions(
            &task,
            Some("read-only-skill"),
            AccessProfile::Restricted,
            Some(&WorkspaceId::new("test-workspace")),
        ),
    )
    .unwrap();
    assert!(prompt.contains("本轮 tools 未提供 agent_spawn"));
    assert!(!prompt.contains("必须通过 agent_spawn 创建真实代理"));

    // 同名配置重载及移除必须重新解析范围，不能因名称相同复用旧目录。
    let runtime = dispatcher.skill_runtime.as_ref().unwrap();
    let registry = runtime.registry();
    let mut skill = registry.get("read-only-skill").unwrap();
    skill.allowed_tools = vec!["search_text".into()];
    registry.register(skill);
    let tools = || {
        dispatcher
            .build_tool_definitions(
                &task,
                Some("read-only-skill"),
                AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        tools(),
        ["search_text", "update_execution_steps", "skill_apply"]
    );
    let available = dispatcher.build_tool_definitions(&task, None, AccessProfile::Restricted, None);
    assert!(
        available
            .iter()
            .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
            .unwrap()
            .function
            .description
            .contains("read-only-skill")
    );
    registry.replace([]);
    assert_eq!(tools(), ["update_execution_steps", "skill_apply"]);
    let available = dispatcher.build_tool_definitions(&task, None, AccessProfile::Restricted, None);
    assert!(
        !available
            .iter()
            .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
            .unwrap()
            .function
            .description
            .contains("read-only-skill")
    );
}

#[test]
fn parse_learning_candidates_accepts_well_formed_array() {
    let raw = r#"[
            {"content": "在 magi 中辅助模型必须配置 base_url 才会启用", "tags": ["config"]},
            {"content": "extract_learnings_via_auxiliary 失败时直接跳过，不退化", "tags": []}
        ]"#;
    let result = parse_learning_candidates(raw).expect("应解析成功");
    assert_eq!(result.len(), 2);
    assert!(result[0].tags.iter().any(|t| t == "auto"));
    assert!(result[0].tags.iter().any(|t| t == "learning"));
}

#[test]
fn auxiliary_learning_extraction_exposes_model_invocation_failure() {
    let event_bus = InMemoryEventBus::new(8);
    let session_store = SessionStore::new();
    let settings_store = Arc::new(SettingsStore::new());
    settings_store
        .set_section(
            "auxiliary",
            serde_json::json!({
                "baseUrl": "https://example.test",
                "apiProtocol": "openai_chat",
                "apiKey": "secret-auxiliary-test-key",
                "model": "auxiliary-test-model"
            }),
        )
        .expect("auxiliary config should save");
    let session_id = SessionId::new("session-learning-extraction-failure");
    let workspace_id = Some(WorkspaceId::new("workspace-learning-extraction-failure"));
    let result = extract_learnings_via_auxiliary(
        AuxiliaryModelExtractionContext {
            client: Arc::new(FailingAuxiliaryClient),
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: Some(&settings_store),
            session_id: &session_id,
            workspace_id: &workspace_id,
            expected_turn_id: None,
        },
        "这是一段用于验证辅助模型失败诊断的会话内容",
    );

    assert!(matches!(
        result,
        Err(LearningExtractionFailure::InvocationFailed)
    ));
    let usage_event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .expect("auxiliary failure should be recorded");
    assert_eq!(usage_event.payload["executionBinding"]["role"], "auxiliary");
    assert_eq!(
        usage_event.payload["modelConfig"]["model"],
        "auxiliary-test-model"
    );

    let dispatcher = dispatcher_with_default_tool_surface();
    let workspace_id = WorkspaceId::new("workspace-learning-extraction-failure");
    dispatcher.publish_learning_extraction_diagnostic(
        &session_id,
        Some(&workspace_id),
        "failed",
        Some(LearningExtractionFailure::InvocationFailed.as_str()),
        0,
        0,
    );
    let event = dispatcher
        .event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "knowledge.learning.extraction")
        .expect("learning extraction diagnostic");
    assert_eq!(event.payload["status"], "failed");
    assert_eq!(event.payload["failure_reason"], "model_invocation_failed");
    assert!(event.payload.get("content").is_none());
}

#[test]
fn parse_learning_candidates_drops_out_of_range_content() {
    let raw = r#"[
            {"content": "太短", "tags": []},
            {"content": "在 magi 中辅助模型必须配置 base_url 才会启用", "tags": []}
        ]"#;
    let result = parse_learning_candidates(raw).expect("仍有一项命中");
    assert_eq!(result.len(), 1);
}

#[test]
fn parse_learning_candidates_returns_none_when_all_filtered() {
    let raw = r#"[{"content": "短", "tags": []}]"#;
    assert!(parse_learning_candidates(raw).is_none());
}

#[test]
fn parse_learning_candidates_rejects_non_json_payload() {
    let raw = "这不是 JSON，模型胡乱输出";
    assert!(parse_learning_candidates(raw).is_none());
}

#[test]
fn parse_learning_candidates_caps_at_three_items() {
    let mut parts = Vec::new();
    for i in 0..8 {
        parts.push(format!(
            r#"{{"content": "条目编号 {i}：这是一条长度足够的占位内容用于通过过滤", "tags": []}}"#
        ));
    }
    let raw = format!("[{}]", parts.join(","));
    let result = parse_learning_candidates(&raw).expect("应解析成功");
    assert_eq!(result.len(), 3);
}

#[test]
fn parse_learning_candidates_filters_pure_tool_sequences() {
    let raw = r#"[
            {"content": "先调用 file_read，然后调用 apply_patch，最后运行 cargo test", "tags": []},
            {"content": "修改状态逻辑后必须同时验证事件事实和只读投影，避免多个状态源互相覆盖", "tags": []}
        ]"#;

    let result = parse_learning_candidates(raw).expect("应保留可复用经验");

    assert_eq!(result.len(), 1);
    assert!(result[0].content.contains("事件事实"));
}

#[test]
fn knowledge_duplicate_recognizes_chinese_paraphrases() {
    let workspace_id = WorkspaceId::new("workspace-learning-duplicate");
    let existing = vec![KnowledgeRecord {
        knowledge_id: "learning-existing".to_string(),
        kind: KnowledgeKind::Learning,
        title: "状态逻辑验证".to_string(),
        content: "修改状态逻辑后必须验证事件投影".to_string(),
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        source_ref: Some("session:existing".to_string()),
        created_at: UtcMillis(1),
        updated_at: UtcMillis(1),
    }];

    assert!(knowledge_duplicate(
        &existing,
        KnowledgeKind::Learning,
        Some(&workspace_id),
        "状态逻辑改动后需要检查事件投影"
    ));
}

#[test]
fn parse_session_memory_slices_accepts_full_object() {
    let raw = r#"{
            "currentWork": "正在收敛辅助模型介入路径，把会话记忆抽取并入主流程",
            "decisions": "决定复用现有 MemoryStore.apply_extraction 落库，不新增 schema",
            "importantContext": "用户强调 cn-engineering-standard，不允许并行实现或兜底",
            "pendingIssues": "Prompt 增强和配置错位修复尚未启动，会在后续两步中处理",
            "nextSteps": "先跑 cargo check 验证，再切入 B 步骤的 Prompt 增强路径"
        }"#;
    let result = parse_session_memory_slices(raw).expect("应解析成功");
    let categories: Vec<&str> = result.iter().map(|s| s.category).collect();
    assert_eq!(
        categories,
        vec![
            "currentWork",
            "decisions",
            "importantContext",
            "pendingIssues",
            "nextSteps"
        ]
    );
}

#[test]
fn parse_session_memory_slices_skips_empty_or_short_categories() {
    let raw = r#"{
            "currentWork": "正在调试会话记忆水位线触发的核心条件，避免每轮都跑",
            "decisions": "",
            "importantContext": "短",
            "pendingIssues": "保留与 TS 版 5 段契约一致，便于未来跨端共用提示词",
            "nextSteps": ""
        }"#;
    let result = parse_session_memory_slices(raw).expect("仍有命中");
    let categories: Vec<&str> = result.iter().map(|s| s.category).collect();
    assert_eq!(categories, vec!["currentWork", "pendingIssues"]);
}

#[test]
fn parse_session_memory_slices_rejects_non_json_payload() {
    assert!(parse_session_memory_slices("not json at all").is_none());
}

#[test]
fn parse_session_memory_slices_returns_none_when_all_filtered() {
    let raw = r#"{
            "currentWork": "",
            "decisions": "短",
            "importantContext": "",
            "pendingIssues": "",
            "nextSteps": ""
        }"#;
    assert!(parse_session_memory_slices(raw).is_none());
}

#[test]
fn resolve_target_for_role_rejects_obsolete_flat_orchestrator_config() {
    use magi_settings_store::SettingsStore;

    let store = Arc::new(SettingsStore::new());
    store
        .set_section(
            "orchestrator",
            serde_json::json!({
                "baseUrl": "https://api.example.com/v1",
                "apiKey": "sk-orch",
                "apiProtocol": "openai_chat",
            }),
        )
        .unwrap();

    assert!(
        resolve_target_for_role(Some(&store), None, RoleTarget::Orchestrator, None).is_err(),
        "旧连接结构必须拒绝，不能视为未配置后改用其他模型"
    );
}

#[test]
fn merge_orchestrator_session_override_applies_model_and_effort() {
    // 全局 base 只提供连接凭据，会话覆盖提供 model + reasoningEffort。
    let mut base = serde_json::json!({
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-orch",
        "model": "global-default-model",
        "apiProtocol": "openai_chat",
        "reasoningEffort": "medium",
    });
    let override_section = serde_json::json!({
        "model": "session-only-model",
        "reasoningEffort": "xhigh",
    });
    merge_orchestrator_session_override(&mut base, &override_section);

    let normalized =
        NormalizedModelConfig::from_settings_value(&base).expect("合并后的模型配置应符合当前协议");
    assert_eq!(
        normalized.require_model().expect("model 必须存在"),
        "session-only-model",
        "会话覆盖的 model 必须生效"
    );
    assert_eq!(
        normalized.require_base_url().expect("baseUrl 必须存在"),
        "https://api.example.com/v1",
        "凭据仍来自全局 base"
    );
    assert_eq!(
        base.get("reasoningEffort")
            .and_then(serde_json::Value::as_str),
        Some("xhigh"),
        "会话覆盖的 reasoningEffort 必须生效"
    );
}

#[test]
fn merge_orchestrator_session_override_ignores_credentials_and_empty() {
    // 会话覆盖即便误带 baseUrl/apiKey 也不得替换全局连接凭据。
    let mut base = serde_json::json!({
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-orch",
        "model": "global-default-model",
        "apiProtocol": "openai_chat",
    });
    let override_section = serde_json::json!({
        "baseUrl": "https://evil.example.com/v1",
        "apiKey": "sk-evil",
        "model": "session-only-model",
    });
    merge_orchestrator_session_override(&mut base, &override_section);
    assert_eq!(
        base.get("baseUrl").and_then(serde_json::Value::as_str),
        Some("https://api.example.com/v1"),
        "会话覆盖不得替换全局 baseUrl"
    );
    assert_eq!(
        base.get("apiKey").and_then(serde_json::Value::as_str),
        Some("sk-orch"),
        "会话覆盖不得替换全局 apiKey"
    );
    assert_eq!(
        base.get("model").and_then(serde_json::Value::as_str),
        Some("session-only-model"),
    );

    // 空覆盖：base 完全不变。
    let mut base2 = serde_json::json!({ "model": "keep-me" });
    merge_orchestrator_session_override(&mut base2, &serde_json::json!({}));
    assert_eq!(
        base2.get("model").and_then(serde_json::Value::as_str),
        Some("keep-me"),
    );
}

#[test]
fn merge_orchestrator_session_override_null_effort_restores_provider_default() {
    // null 保留服务商默认语义，与显式 none 档位不同。
    let mut base = serde_json::json!({
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-orch",
        "model": "global-default-model",
        "reasoningEffort": "high",
    });
    merge_orchestrator_session_override(
        &mut base,
        &serde_json::json!({ "reasoningEffort": serde_json::Value::Null }),
    );
    assert_eq!(
        base.get("reasoningEffort")
            .and_then(serde_json::Value::as_str),
        None,
        "null 覆盖保留模型默认值"
    );
}

#[test]
fn unknown_model_does_not_claim_a_default_reasoning_effort() {
    use magi_settings_store::SettingsStore;

    let store = SettingsStore::new();
    store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://api.example.com/v1",
                "sk-orch",
                &["session-model"],
            ),
        )
        .unwrap();
    let session_id = SessionId::new("session-default-reasoning-effort");
    store
        .set_session_section(
            &session_id,
            "orchestrator",
            serde_json::json!({ "sourceId": "custom/test", "model": "session-model" }),
        )
        .unwrap();

    let config = resolve_orchestrator_model_config(&store, Some(&session_id))
        .expect("主线模型配置应完成默认值归一化");
    assert_eq!(
        config
            .to_usage_llm_config()
            .and_then(|config| config.reasoning_effort),
        None,
    );
}

#[test]
fn resolve_orchestrator_model_config_uses_user_defaults_without_session_override() {
    use magi_settings_store::{ORCHESTRATOR_SESSION_DEFAULTS_SECTION, SettingsStore};

    let store = SettingsStore::new();
    store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://api.example.com/v1",
                "sk-orch",
                &["model-last-used"],
            ),
        )
        .unwrap();
    store
        .set_section(
            ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            serde_json::json!({
                "sourceId": "custom/test",
                "model": "model-last-used",
                "reasoningEffort": "high"
            }),
        )
        .unwrap();
    let session_without_model_override = SessionId::new("session-without-model-override");

    let config = resolve_orchestrator_model_config(&store, Some(&session_without_model_override))
        .expect("没有模型覆盖的会话应继承权威的用户默认模型");
    assert_eq!(
        config.require_model().expect("默认模型必须可执行"),
        "model-last-used"
    );
    assert_eq!(
        config
            .to_usage_llm_config()
            .and_then(|config| config.reasoning_effort),
        None,
    );
}

#[test]
fn resolve_target_for_role_orchestrator_threads_session_override() {
    use magi_settings_store::SettingsStore;

    let store = Arc::new(SettingsStore::new());
    store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://api.example.com/v1",
                "sk-orch",
                &["session-only-model"],
            ),
        )
        .unwrap();
    let session_id = SessionId::new("session-model-scope");
    store
        .set_session_section(
            &session_id,
            "orchestrator",
            serde_json::json!({
                "sourceId": "custom/test",
                "model": "session-only-model",
                "reasoningEffort": "xhigh",
            }),
        )
        .unwrap();

    // 会话级覆盖存在时，主路径必须返回业务模型 client。
    let resolved = resolve_target_for_role(
        Some(&store),
        None,
        RoleTarget::Orchestrator,
        Some(&session_id),
    )
    .expect("orchestrator 段解析不应失败");
    assert!(
        resolved.is_some(),
        "会话级覆盖存在时必须返回业务模型 client"
    );

    // 不带 session_id 时只能使用用户默认选择；没有任何数据源则交给宿主默认客户端。
    let global = resolve_target_for_role(Some(&store), None, RoleTarget::Orchestrator, None)
        .expect("已配置数据源时按目录默认选择解析");
    assert!(global.is_some(), "没有会话覆盖时继承目录第一项");
    let empty = Arc::new(SettingsStore::new());
    let unconfigured = resolve_target_for_role(Some(&empty), None, RoleTarget::Orchestrator, None)
        .expect("未配置数据源不是错误");
    assert!(
        unconfigured.is_none(),
        "未配置数据源不得生成隐藏默认模型 client"
    );
}

#[test]
fn build_safety_gate_honors_settings_override_for_builtin_rules() {
    use magi_settings_store::SettingsStore;

    let dispatcher = dispatcher_with_default_tool_surface();
    let store = Arc::new(SettingsStore::new());
    store
        .set_section(
            "safeguardConfig",
            serde_json::json!({
                "rules": [
                    {
                        "pattern": "rm -rf",
                        "enabled": false,
                        "category": "bulk_delete",
                        "action": "require_approval_in_restricted"
                    }
                ]
            }),
        )
        .unwrap();

    let gate = dispatcher
        .build_safety_gate(Some(&store))
        .expect("settings override 后仍应构造 SafetyGate");
    let args = serde_json::json!({ "command": "rm -rf /tmp/demo" }).to_string();

    assert_eq!(
        gate.evaluate("shell_exec", &args),
        magi_safety_gate::SafetyDecision::Allow,
        "用户在设置页禁用内置规则后，运行期必须尊重同一份 settings"
    );
    let force_push = serde_json::json!({ "command": "git push --force origin main" }).to_string();
    assert!(
        gate.evaluate("shell_exec", &force_push)
            .is_require_approval(),
        "settings 未覆盖的内置规则仍应自动补齐"
    );
}

#[test]
fn resolve_safeguard_prompt_renders_runtime_rule_actions() {
    use magi_settings_store::SettingsStore;

    let dispatcher = dispatcher_with_default_tool_surface();
    let store = Arc::new(SettingsStore::new());
    store
        .set_section(
            "safeguardConfig",
            serde_json::json!({
                "rules": [
                    {
                        "pattern": "custom hard block",
                        "enabled": true,
                        "category": "custom",
                        "action": "hard_block"
                    },
                    {
                        "pattern": "custom approval command",
                        "enabled": true,
                        "category": "custom",
                        "action": "require_approval_in_restricted"
                    },
                    {
                        "pattern": "custom audit command",
                        "enabled": true,
                        "category": "custom",
                        "action": "audit_only"
                    },
                    {
                        "pattern": "disabled hard block",
                        "enabled": false,
                        "category": "custom",
                        "action": "hard_block"
                    }
                ]
            }),
        )
        .unwrap();

    let prompt = dispatcher
        .resolve_safeguard_prompt(Some(&store))
        .expect("安全防护基线必须始终注入");

    assert!(
        prompt.contains(
            "- [硬阻断] custom hard block：任何访问模式下都不得执行，也不得请求授权绕过。"
        ),
        "HardBlock 必须明确表达为不可审批绕过的阻断"
    );
    assert!(
            prompt.contains(
                "- [需要授权] custom approval command：受限访问下允许模型发起调用，运行时暂停并创建用户授权请求；批准后继续执行，拒绝后不要重复同一调用；完全访问下按当前授权执行并保留风险说明。"
            ),
            "RequireApprovalInRestricted 必须表达访问模式差异"
        );
    assert!(
        prompt.contains("- [审计] custom audit command：允许执行，但必须如实说明影响。"),
        "AuditOnly 必须表达为审计而非审批"
    );
    assert!(
        !prompt.contains("disabled hard block"),
        "禁用规则不能进入模型可见提示"
    );
    assert!(
        !prompt.contains("如果命中以下危险模式，必须先向用户确认"),
        "提示不能再把所有 SafetyGate 动作统一描述为审批"
    );
}

#[test]
fn agent_model_selection_freezes_binding_capability_and_usage_without_credentials() {
    use crate::model_config::{agent_target_model_config, frozen_agent_model_config};
    use magi_core::ReasoningEffort::{High, Low, Max, Ultra};
    let store = Arc::new(SettingsStore::new());
    let session = SessionId::new("reasoning-selection");
    store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://example.test/v1",
                "private-key",
                &["gpt-5.6-sol"],
            ),
        )
        .unwrap();
    store
            .set_session_section(
                &session,
                "orchestrator",
                serde_json::json!({"sourceId":"custom/test", "model":"gpt-5.6-sol", "reasoningEffort":"high"}),
            )
            .unwrap();
    let parent = task_with_role("coordinator", TaskTier::ExecutionChain);
    let (inherited, engine, revision) =
        agent_target_model_config(&store, &parent, "executor", &session).unwrap();
    let inherited_selection = inherited
        .freeze_agent_selection(engine, revision, Some(Low))
        .unwrap();
    assert_eq!(inherited_selection.effective_effort, Some(Low));
    assert_eq!(
        inherited
            .freeze_agent_selection(None, 0, None)
            .unwrap()
            .effective_effort,
        Some(High)
    );
    assert!(
        inherited
            .freeze_agent_selection(None, 0, Some(Ultra))
            .is_err()
    );
    let mut bound = serde_json::json!({"baseUrl":"https://example.test/v1", "apiKey":"private-key", "apiProtocol":"openai_chat"});
    bound["model"] = serde_json::json!("o3-mini");
    bound["reasoningEffort"] = serde_json::json!("low");
    store
        .set_section("engines", serde_json::json!([{"id":"bound", "llm":bound}]))
        .unwrap();
    store
        .set_section(
            "agents",
            serde_json::json!([{"templateId":"executor", "engineId":"bound", "bindingRevision":7}]),
        )
        .unwrap();
    let (config, engine, revision) =
        agent_target_model_config(&store, &parent, "executor", &session).unwrap();
    assert!(
        config
            .freeze_agent_selection(engine.clone(), revision, Some(Max))
            .is_err()
    );
    let selection = config
        .freeze_agent_selection(engine, revision, Some(High))
        .unwrap();
    assert_eq!(selection.binding_revision, 7);
    assert_eq!(selection.effective_effort, Some(High));
    let encoded = serde_json::to_string(&selection).unwrap();
    assert!(!encoded.contains("private-key"));
    let mut child = task_with_role("executor", TaskTier::ExecutionChain);
    child.executor_binding.as_mut().unwrap().model_selection =
        Some(serde_json::from_str(&encoded).unwrap());
    let child: Task = serde_json::from_str(&serde_json::to_string(&child).unwrap()).unwrap();
    // 模拟排队期间修改角色绑定、引擎模型与强度；原任务恢复仍使用原模型与能力。
    store.set_section("agents", serde_json::json!([])).unwrap();
    let mut edited = bound.clone();
    edited["model"] = serde_json::json!("unknown-alias");
    edited["reasoningEffort"] = serde_json::Value::Null;
    store
        .set_section("engines", serde_json::json!([{"id":"bound", "llm":edited}]))
        .unwrap();
    let restored = frozen_agent_model_config(&store, &selection).unwrap();
    assert_eq!(restored.require_model().unwrap(), "o3-mini");
    assert_eq!(restored.reasoning_effort_label(), Some("high"));
    assert_eq!(
        restored.reasoning_capability("o3-mini"),
        selection.capability
    );
    assert!(
        dispatcher_with_default_tool_surface()
            .resolve_model_client_for_task(
                Some(&store),
                Some(&child),
                Some("executor"),
                Some(&session)
            )
            .is_ok()
    );
    let usage = crate::usage_recording::session_turn_model_usage_binding(true)
        .with_task_model(&child, Some(&store))
        .unwrap();
    let event_bus = InMemoryEventBus::new(8);
    let sessions = SessionStore::new();
    crate::usage_recording::publish_model_usage_record(
        &event_bus,
        &sessions,
        Some(&store),
        crate::usage_recording::ModelUsageRecordInput {
            session_id: &session,
            workspace_id: &None,
            binding: &usage,
            call_id: "selection-usage".into(),
            usage: Some(&serde_json::json!({"prompt_tokens":3,"completion_tokens":7})),
            context_breakdown: None,
            status: magi_usage_authority::UsageCallStatus::Success,
            assignment_id: Some(child.task_id.to_string()),
            error_code: None,
        },
    );
    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "model.usage.recorded")
        .unwrap();
    assert_eq!(event.payload["modelConfig"]["model"], "o3-mini");
    assert_eq!(event.payload["modelConfig"]["reasoningEffort"], "high");
    assert_eq!(event.payload["executionBinding"]["bindingRevision"], 7);
    let (nested, _, _) = agent_target_model_config(&store, &child, "reviewer", &session).unwrap();
    assert_eq!(nested.require_model().unwrap(), "o3-mini");
    assert_eq!(
        nested
            .freeze_agent_selection(selection.engine_id.clone(), 7, Some(Low))
            .unwrap()
            .effective_effort,
        Some(Low)
    );
    let mut replaced = bound;
    replaced["apiKey"] = serde_json::json!("different-account");
    store
        .set_section(
            "engines",
            serde_json::json!([{"id":"bound", "llm":replaced}]),
        )
        .unwrap();
    assert!(frozen_agent_model_config(&store, &selection).is_err());
    store.set_section("engines", serde_json::json!([])).unwrap();
    assert!(frozen_agent_model_config(&store, &selection).is_err());
}

#[test]
fn agent_model_unknown_capability_does_not_accept_explicit_effort() {
    use magi_core::ReasoningEffort::High;
    let config = NormalizedModelConfig::from_settings_value(&serde_json::json!({"baseUrl":"https://example.test", "apiProtocol":"openai_chat", "model":"opaque-alias", "reasoningEffort":"high"})).unwrap();
    assert!(config.freeze_agent_selection(None, 0, Some(High)).is_err());
    assert_eq!(
        config
            .freeze_agent_selection(None, 0, None)
            .unwrap()
            .effective_effort,
        None
    );
    let config = config.with_model("gpt-4o");
    assert!(config.freeze_agent_selection(None, 0, Some(High)).is_err());
    assert_eq!(
        config
            .freeze_agent_selection(None, 0, None)
            .unwrap()
            .effective_effort,
        None
    );
}

/// 插件贡献的主对话数据源：主对话可用；子代理、扩展调用、辅助模型都不会使用它；
/// 来源消失后不静默回退到宿主默认客户端。
#[test]
fn plugin_model_source_serves_only_the_main_conversation() {
    use crate::main_model_sources::set_plugin_source;
    use magi_settings_store::SettingsStore;

    let store = Arc::new(SettingsStore::new());
    let session = SessionId::new("session-plugin-source");
    set_plugin_source(
        &store,
        "acme.proxy",
        "gateway",
        "Gateway",
        &serde_json::json!({
            "baseUrl": "https://plugin.example.test/v1", "apiKey": "k",
            "apiProtocol": "openai_chat", "models": [{"id": "p1"}],
        }),
    )
    .unwrap();
    store
        .set_session_section(
            &session,
            "orchestrator",
            serde_json::json!({"sourceId": "plugin/acme.proxy/gateway", "model": "p1"}),
        )
        .unwrap();

    // 主对话：即使用户没有配置任何自有数据源，选择了插件来源也按目录解析。
    let main =
        resolve_target_for_role(Some(&store), None, RoleTarget::Orchestrator, Some(&session))
            .expect("插件来源应可解析");
    assert!(main.is_some());

    // 子代理：未绑定独立引擎时不得继承插件来源。
    let parent = task_with_role("coordinator", TaskTier::ExecutionChain);
    let error =
        crate::model_config::agent_target_model_config(&store, &parent, "executor", &session)
            .expect_err("子代理不得继承插件来源");
    assert!(error.contains("只能用于主对话"), "{error}");

    // 扩展 model.invoke / credential.ref：默认连接或未绑定引擎的角色同样拒绝。
    for role in [None, Some("executor")] {
        let error = crate::model_config::ensure_extension_model_source(&store, role, &session)
            .expect_err("扩展调用不得使用插件来源");
        assert!(error.contains("只能用于主对话"), "{error}");
    }
    // 角色绑定了独立引擎时，扩展调用走该引擎，与插件来源无关。
    store
        .set_section(
            "engines",
            serde_json::json!([{"id": "bound", "llm": {
                "baseUrl": "https://engine.example.test/v1", "apiKey": "k",
                "model": "m", "apiProtocol": "openai_chat",
            }}]),
        )
        .unwrap();
    store
        .set_section(
            "agents",
            serde_json::json!([{"templateId": "executor", "engineId": "bound"}]),
        )
        .unwrap();
    crate::model_config::ensure_extension_model_source(&store, Some("executor"), &session)
        .expect("独立引擎不受插件来源影响");

    // 辅助模型只读自己的设置段，不会落到主对话的插件来源。
    let auxiliary =
        resolve_target_for_role(Some(&store), None, RoleTarget::Auxiliary, Some(&session))
            .expect("辅助模型未配置不是错误");
    assert!(auxiliary.is_none());

    // 插件撤销后：主对话报来源不可用，而不是回退到默认客户端。
    crate::main_model_sources::clear_plugin_sources(&store, "acme.proxy");
    let default_client: Arc<dyn ModelBridgeClient> = Arc::new(FailingAuxiliaryClient);
    let error = match resolve_target_for_role(
        Some(&store),
        Some(default_client),
        RoleTarget::Orchestrator,
        Some(&session),
    ) {
        Err(error) => error,
        Ok(_) => panic!("来源撤销后不得回退到默认客户端"),
    };
    assert!(error.contains("插件数据源当前不可用"), "{error}");
}

#[test]
fn resolve_target_for_role_reads_execution_snapshot_not_live_settings() {
    use magi_settings_store::SettingsStore;

    let live_store = Arc::new(SettingsStore::new());
    live_store
        .set_section(
            "orchestrator",
            crate::main_model_sources::test_sources(
                "https://api.example.com/v1",
                "sk-old",
                &["model-snapshot"],
            ),
        )
        .unwrap();
    let session_id = SessionId::new("session-snapshot-model");
    live_store
        .set_session_section(
            &session_id,
            "orchestrator",
            serde_json::json!({
                "sourceId": "custom/test",
                "model": "model-snapshot",
            }),
        )
        .unwrap();
    let snapshot = Arc::new(live_store.execution_snapshot());

    live_store.remove_section("orchestrator").unwrap();
    live_store
        .remove_session_section(&session_id, "orchestrator")
        .unwrap();

    let snapshot_client = resolve_target_for_role(
        Some(&snapshot),
        None,
        RoleTarget::Orchestrator,
        Some(&session_id),
    )
    .expect("快照内的 orchestrator 配置应可解析");
    assert!(
        snapshot_client.is_some(),
        "执行快照必须保留任务接受时的会话模型配置"
    );

    let live_client = resolve_target_for_role(
        Some(&live_store),
        None,
        RoleTarget::Orchestrator,
        Some(&session_id),
    )
    .expect("实时 settings 查询不应失败");
    assert!(
        live_client.is_none(),
        "实时 settings 已删除 orchestrator，不应影响既有执行快照"
    );
}
fn extension_services_fixture() -> (LlmTaskDispatcher, SessionId, serde_json::Value) {
    let dispatcher = dispatcher_with_default_tool_surface()
        .with_session_state_persist_callback(Arc::new(|_| Ok(())));
    let session = SessionId::new("extension-service-session");
    dispatcher
        .session_store
        .create_session(session.clone(), "扩展服务")
        .unwrap();
    dispatcher
        .session_store
        .ensure_session_mission(&session, UtcMillis(1), || {
            MissionId::new("extension-service-mission")
        });
    dispatcher
        .session_store
        .accept_current_turn_with_timeline_entry(
            session.clone(),
            magi_session_store::TimelineEntryInput::new(
                "extension-test",
                TimelineEntryKind::UserMessage,
                "work",
                UtcMillis(1),
            ),
            magi_session_store::ActiveExecutionTurn {
                turn_id: "extension-service-turn".into(),
                turn_seq: 1,
                accepted_at: UtcMillis(1),
                completed_at: None,
                status: "running".into(),
                user_message: Some("work".into()),
                items: vec![],
            },
        )
        .unwrap();
    let context = serde_json::json!({"owner":{"sessionId":session,"turnId":"extension-service-turn"},"invocationId":"signed-by-daemon"});
    (dispatcher, session, context)
}

#[test]
fn plugin_page_model_selection_uses_native_reasoning_capability() {
    let settings = Arc::new(SettingsStore::new());
    let capability = magi_core::ReasoningCapability {
        supported_efforts: vec![
            magi_core::ReasoningEffort::None,
            magi_core::ReasoningEffort::High,
        ],
        default_effort: Some(magi_core::ReasoningEffort::None),
        transport: magi_core::ReasoningTransport::Openai,
        source: "user".into(),
    };
    settings.set_section("orchestrator", serde_json::json!({
            "sources":[{
                "id":"one","name":"one","baseUrl":"https://model.example.test/v1","apiKey":"private-key",
                "apiProtocol":"openai_chat","models":[{"id":"test-model","reasoning":capability}]
            }]
        })).unwrap();
    settings
        .set_section(
            magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
            serde_json::json!({
                "sourceId":"custom/one","model":"test-model","reasoningEffort":"none"
            }),
        )
        .unwrap();
    let dispatcher = dispatcher_with_default_tool_surface().with_settings_store(settings);
    let selection = dispatcher.extension_service("acme.native", "model.selection", serde_json::json!({
            "_context":{"activity":"route"},
            "_modelOwner":{"pluginId":"acme.native","digest":"a".repeat(64),"instanceId":"native-instance"}
        }), Arc::default(), None).unwrap();
    assert_eq!(
        selection["reasoning"],
        serde_json::to_value(capability).unwrap()
    );
    assert!(!selection.to_string().contains("private-key"));
    assert!(dispatcher.session_store.sessions().is_empty());
}

#[test]
fn plugin_page_model_uses_durable_plugin_ownership_without_creating_a_session_or_replaying() {
    use std::io::Read;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut raw = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            let size = stream.read(&mut bytes).unwrap();
            assert!(size > 0);
            raw.extend_from_slice(&bytes[..size]);
            if let Some(offset) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&raw[..offset]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if raw.len() >= offset + 4 + length {
                    break;
                }
            }
        }
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"PLUGIN RESULT\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n";
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        String::from_utf8(raw).unwrap()
    });
    let settings = Arc::new(SettingsStore::new());
    settings.set_section("orchestrator", serde_json::json!({"sources":[{
            "id":"one","name":"one","baseUrl":format!("http://{address}/v1"),"apiKey":"private-test-key",
            "apiProtocol":"openai_chat","models":[{"id":"test-model"}]
        }]})).unwrap();
    let dispatcher = dispatcher_with_default_tool_surface().with_settings_store(settings);
    let directory = tempfile::tempdir().unwrap();
    dispatcher
        .event_bus
        .set_audit_usage_ledger_persistence(directory.path());
    let identity = serde_json::json!({"pluginId":"acme.page","digest":"a".repeat(64),"instanceId":"instance-one"});
    let base = serde_json::json!({"_context":{"activity":"route"},"_modelOwner":identity});
    let selection = dispatcher
        .extension_service(
            "acme.page",
            "model.selection",
            base.clone(),
            Arc::default(),
            None,
        )
        .unwrap();
    assert!(!selection.to_string().contains("private-test-key"));
    let mut input = base.clone();
    input["operationId"] = serde_json::json!("operation-one");
    input["selection"] = selection;
    input["messages"] = serde_json::json!([{"role":"system","content":"ORIGINAL SYSTEM"},{"role":"user","content":"ORIGINAL USER"}]);
    let mut precancel = base.clone();
    precancel["operationId"] = serde_json::json!("precancelled");
    dispatcher
        .extension_service("acme.page", "model.cancel", precancel, Arc::default(), None)
        .unwrap();
    let mut cancelled_input = input.clone();
    cancelled_input["operationId"] = serde_json::json!("precancelled");
    assert!(
        dispatcher
            .extension_service(
                "acme.page",
                "model.invoke",
                cancelled_input,
                Arc::default(),
                None
            )
            .unwrap_err()
            .contains("Cancelled")
    );
    let result = dispatcher
        .extension_service(
            "acme.page",
            "model.invoke",
            input.clone(),
            Arc::default(),
            None,
        )
        .unwrap();
    assert_eq!(result["content"], "PLUGIN RESULT");
    dispatcher
        .settings_store
        .as_ref()
        .unwrap()
        .set_section("orchestrator", serde_json::json!({"sources":[]}))
        .unwrap();
    assert_eq!(
        dispatcher
            .extension_service(
                "acme.page",
                "model.invoke",
                input.clone(),
                Arc::default(),
                None
            )
            .unwrap(),
        result
    );
    let raw = server.join().unwrap();
    assert!(raw.contains("ORIGINAL SYSTEM") && raw.contains("ORIGINAL USER"));
    let restored = InMemoryEventBus::new(16);
    restored.restore_persisted_audit_usage_ledger(
        magi_event_bus::AuditUsageLedgerSnapshot::load_from_dir(directory.path(), UtcMillis::now())
            .unwrap(),
    );
    crate::auxiliary_usage::recover(&restored).unwrap();
    let ledger = restored.audit_usage_ledger_snapshot();
    let usage = ledger
        .usage_entries
        .iter()
        .find(|entry| entry.event_type == "model.usage.recorded")
        .unwrap();
    assert_eq!(usage.payload["plugin"]["pluginId"], "acme.page");
    assert!(usage.context.session_id.is_none() && usage.payload.get("sessionId").is_none());
    assert_eq!(usage.payload["usage"]["inputTokens"], 12);
    assert!(dispatcher.session_store.sessions().is_empty());
    input["messages"][1]["content"] = serde_json::json!("CHANGED");
    assert!(
        dispatcher
            .extension_service("acme.page", "model.invoke", input, Arc::default(), None)
            .unwrap_err()
            .contains("发生变化")
    );
    // 重启恢复只重建账本投影；上游服务器已退出，没有第二次模型请求。
    assert!(
        ledger
            .audit_entries
            .iter()
            .any(|entry| entry.payload["stage"] == "completed")
    );
}

#[test]
fn plugin_page_model_streams_native_http_tools_images_and_controls_on_one_ledger() {
    use std::io::{BufRead, Read};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ack, ready) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let name = body["tools"][0]["function"]["name"].as_str().unwrap();
        let mut stream = reader.into_inner();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
        let mut send = |value: serde_json::Value| {
            let data = format!("data: {value}\n\n");
            write!(stream, "{:x}\r\n{}\r\n", data.len(), data).unwrap();
            stream.flush().unwrap();
        };
        send(serde_json::json!({"choices":[{"delta":{"content":"first"}}]}));
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("进度必须在最终结果之前到达");
        send(
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"real-call","type":"function","function":{"name":name,"arguments":"{\"q\":\"value\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":30,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":0}}}),
        );
        let data = "data: [DONE]\n\n";
        write!(stream, "{:x}\r\n{}\r\n0\r\n\r\n", data.len(), data).unwrap();
        body
    });
    let settings = Arc::new(SettingsStore::new());
    settings.set_section("orchestrator",serde_json::json!({"sources":[{"id":"one","name":"one","baseUrl":format!("http://{address}/v1"),"apiKey":"private-key","apiProtocol":"openai_chat","models":[{"id":"model"}]}]})).unwrap();
    let dispatcher = dispatcher_with_default_tool_surface().with_settings_store(settings);
    let base = serde_json::json!({"_context":{"activity":"route"},"_modelOwner":{"pluginId":"acme.page","digest":"a".repeat(64),"instanceId":"live-one"}});
    let selection = dispatcher
        .extension_service(
            "acme.page",
            "model.selection",
            base.clone(),
            Arc::default(),
            None,
        )
        .unwrap();
    let mut input = base;
    input["selection"] = selection;
    input["operationId"] = serde_json::json!("rich-one");
    input["messages"] = serde_json::json!([{"role":"user","content":"Inspect","images":[{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aRZkAAAAASUVORK5CYII="}]}]);
    input["tools"] = serde_json::json!([{"type":"function","origin":"plugin","function":{"name":"business.lookup","description":"Lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}}]);
    input["temperature"] = serde_json::json!(0.25);
    input["stop"] = serde_json::json!(["END"]);
    let progress = Arc::new(move |delta: serde_json::Value| {
        if delta["content"] == "first" {
            ack.send(()).unwrap();
        }
        Ok(())
    });
    let result = dispatcher
        .extension_service(
            "acme.page",
            "model.invoke",
            input.clone(),
            Arc::default(),
            Some(progress),
        )
        .unwrap();
    assert_eq!(result["status"], "requires_tool_execution");
    assert_eq!(result["content"], "first");
    assert_eq!(
        result["toolCalls"][0]["function"]["name"],
        "business.lookup"
    );
    assert_eq!(result["usage"]["reasoningTokens"], 0);
    let request = server.join().unwrap();
    assert_eq!(request["temperature"], 0.25);
    assert_eq!(request["stop"], serde_json::json!(["END"]));
    assert!(
        request["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "image_url")
    );
    assert_eq!(
        dispatcher
            .extension_service("acme.page", "model.invoke", input, Arc::default(), None)
            .unwrap(),
        result,
        "读取同一结果不能重放模型"
    );
    assert!(dispatcher.session_store.sessions().is_empty());
    let ledger = dispatcher.event_bus.audit_usage_ledger_snapshot();
    assert_eq!(
        ledger
            .usage_entries
            .iter()
            .filter(|entry| entry.event_type == "model.usage.recorded")
            .count(),
        1
    );
}

#[test]
fn extension_snapshot_is_bounded_and_actions_require_a_live_owner() {
    let (dispatcher, session, context) = extension_services_fixture();
    let snapshot = dispatcher
        .extension_service(
            "acme.test",
            "session.snapshot",
            serde_json::json!({"sessionId":session}),
            Arc::default(),
            None,
        )
        .unwrap();
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 1);
    assert!(
        dispatcher
            .extension_service(
                "acme.test",
                "notify.user",
                serde_json::json!({"message":"hello"}),
                Arc::default(),
                None
            )
            .is_err()
    );
    let notification = dispatcher
        .extension_service(
            "acme.test",
            "notify.user",
            serde_json::json!({"_context":context,"message":"hello"}),
            Arc::default(),
            None,
        )
        .unwrap();
    let records = dispatcher.session_store.notifications_for_context(
        &magi_session_store::NotificationContext::personal(Some(session)),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(notification["notificationId"], records[0].notification_id);
}

#[test]
fn extension_session_directory_projects_current_metadata_with_bounded_stable_pages() {
    use serde_json::{Value, json};
    let (dispatcher, _, _) = extension_services_fixture();
    for (id, title, workspace) in [
        ("a", "First", Some("w".into())),
        ("b", "Second", None),
        ("c", "Third", Some("w".into())),
    ] {
        dispatcher
            .session_store
            .create_session_for_workspace(SessionId::new(id), title, workspace)
            .unwrap();
    }
    dispatcher
        .session_store
        .create_external_tool_session(SessionId::new("external"), "Private", "w".into())
        .unwrap();
    let directory = |params| {
        dispatcher.extension_service(
            "acme.test",
            "session.directory",
            params,
            Arc::default(),
            None,
        )
    };
    let first = directory(json!({"limit":1})).unwrap();
    assert_eq!(first["sessions"][0]["sessionId"], "a");
    assert_eq!(first["nextAfterSessionId"], "a");
    assert_eq!(first["hasMore"], true);
    assert_eq!(
        first["sessions"][0].as_object().unwrap().len(),
        5,
        "只有标题、身份、归属与时间，不含消息或运行配置"
    );
    let second = directory(json!({"afterSessionId":"a","limit":1})).unwrap();
    assert_eq!(second["sessions"][0]["sessionId"], "b");
    assert_eq!(second["sessions"][0]["workspaceId"], Value::Null);
    dispatcher
        .session_store
        .rename_session(&SessionId::new("c"), "Changed")
        .unwrap();
    let filtered = directory(json!({"workspaceId":"w","afterSessionId":"a"})).unwrap();
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["sessions"][0]["title"], "Changed");
    assert_eq!(filtered["hasMore"], false);
    assert_eq!(filtered["nextAfterSessionId"], Value::Null);
    for params in [
        json!({"limit":0}),
        json!({"limit":201}),
        json!({"workspaceId":" "}),
        json!({"afterSessionId":""}),
        json!({"messages":true}),
    ] {
        assert!(directory(params).is_err());
    }
    assert!(
        directory(json!({"workspaceId":"absent"})).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn extension_questions_use_canonical_interactions_and_only_user_answers_complete_them() {
    let (dispatcher, session, context) = extension_services_fixture();
    let worker = dispatcher.clone();
    let thread = std::thread::spawn(move || {
        worker.extension_service("acme.test", "ask.user", serde_json::json!({"_context":context,"questions":[{"header":"方式","question":"选择方式","options":[{"label":"A"},{"label":"B"}]}]}), Arc::default(), None)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let question = loop {
        if let Some(question) = dispatcher
            .conversation_registry
            .user_questions()
            .pending_for_session(&session)
            .into_iter()
            .next()
        {
            break question;
        }
        if thread.is_finished() {
            panic!("提问在登记前失败：{:?}", thread.join().unwrap());
        }
        assert!(std::time::Instant::now() < deadline, "提问登记超时");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(
        dispatcher
            .session_store
            .pending_user_questions(&session)
            .unwrap()
            .len(),
        1
    );
    dispatcher
        .conversation_registry
        .user_questions()
        .resolve(
            &session,
            &question.question_id,
            crate::UserQuestionResponse::Answered {
                answers: vec![crate::UserQuestionAnswer {
                    selected: vec!["A".into()],
                    other: None,
                }],
            },
        )
        .unwrap();
    let result = thread.join().unwrap().unwrap();
    assert_eq!(result["response"]["kind"], "answered");
    assert_eq!(
        dispatcher
            .session_store
            .pending_user_questions(&session)
            .unwrap()
            .len(),
        0
    );
}
