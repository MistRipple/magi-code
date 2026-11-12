use super::{
    DaemonRuntime, SettingsBackedMcpBridgeClient, build_agent_role_catalog_provider,
    external_mcp_server_catalog_entry, fail_orphan_session_root_tasks,
    publish_task_status_changed_event, reconcile_terminal_task_execution_threads,
    test_fixture_sibling_root,
};
use crate::daemon::{config::DaemonConfig, persistence::StateRepository};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_bridge_client::{
    BridgeClientError, BridgeResponse, McpBridgeClient, McpServerClient, McpServerConfig,
    McpToolCallRequest, ModelInvocationRequest,
};
use magi_conversation_runtime::task_execution_registry::TaskExecutionRegistry;
use magi_core::{
    AbsolutePath, MissionId, SessionId, Task, TaskId, TaskKind, TaskRuntimePayload, TaskStatus,
    ThreadId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, SessionStore,
};
use magi_settings_store::SettingsStore;
use magi_workspace::WorkspaceStore;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, RwLock},
    time::{Duration, Instant},
};
use tower::util::ServiceExt;

const BACKGROUND_TEST_TIMEOUT: Duration = Duration::from_secs(30);
const OPENAI_COMPAT_ENV_KEYS: [&str; 3] = [
    "MAGI_OPENAI_COMPAT_BASE_URL",
    "MAGI_OPENAI_COMPAT_API_KEY",
    "MAGI_OPENAI_COMPAT_MODEL",
];
static OPENAI_COMPAT_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn router_rejects_corrupt_settings_file() {
    let state_root =
        std::env::temp_dir().join(format!("magi-corrupt-settings-{}", UtcMillis::now().0));
    fs::create_dir_all(&state_root).unwrap();
    fs::write(state_root.join("settings.json"), b"{not-json").unwrap();
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let runtime = DaemonRuntime::restore(&config).unwrap();

    let error = runtime
        .router("daemon-test".to_string())
        .expect_err("损坏设置文件必须阻止路由启动");

    assert!(error.to_string().contains("加载设置文件失败"));
    fs::remove_dir_all(state_root).unwrap();
}

#[test]
fn restore_rejects_state_root_that_is_workspace_magi_directory() {
    let root = temp_state_root("overlapping-state-root");
    let workspace_root = root.join("workspace");
    fs::create_dir_all(workspace_root.join(".magi")).unwrap();
    let workspace_id = "workspace-overlapping".to_string();

    let error = DaemonRuntime::validate_workspace_state_separation(
        &workspace_root.join(".magi"),
        &[(workspace_id.clone(), workspace_root.clone())],
    )
    .expect_err("global and workspace state roots must not overlap");

    assert!(error.to_string().contains("状态根与 workspace 状态根重合"));
    assert!(error.to_string().contains(&workspace_id));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn external_mcp_catalog_uses_live_tool_list_instead_of_saved_count() {
    let script = r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock-mcp","version":"0.1.0"}}}\n' "$id"
            ;;
        notifications/initialized)
            ;;
        tools/list)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"repo.inspect","description":"Inspect repository","inputSchema":{"type":"object","properties":{}}}]}}\n' "$id"
            ;;
    esac
done
"#;
    let client = Arc::new(McpServerClient::from_stdio(McpServerConfig::new(
        "sh",
        vec!["-c".to_string(), script.to_string()],
    )));
    let connections = Arc::new(RwLock::new(HashMap::from([(
        "mock-mcp".to_string(),
        client,
    )])));

    let entry = external_mcp_server_catalog_entry(
        &json!({
            "id": "mock-mcp",
            "name": "Mock MCP",
            "enabled": true,
            "toolCount": 0
        }),
        &connections,
    )
    .expect("catalog entry should exist");

    assert_eq!(entry.tool_count, Some(1));
}

struct OpenAiCompatEnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
    _guard: MutexGuard<'static, ()>,
}

impl OpenAiCompatEnvGuard {
    fn unset() -> Self {
        let guard = OPENAI_COMPAT_ENV_LOCK
            .lock()
            .expect("openai compat env mutex should lock");
        let saved = OPENAI_COMPAT_ENV_KEYS
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in OPENAI_COMPAT_ENV_KEYS {
            unsafe { std::env::remove_var(key) };
        }
        Self {
            saved,
            _guard: guard,
        }
    }
}

impl Drop for OpenAiCompatEnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            if let Some(value) = value {
                unsafe { std::env::set_var(key, value) };
            } else {
                unsafe { std::env::remove_var(key) };
            }
        }
    }
}

#[derive(Default)]
struct RecordingMcpClient {
    calls: Arc<Mutex<Vec<McpToolCallRequest>>>,
}

impl McpBridgeClient for RecordingMcpClient {
    fn call_tool(
        &self,
        request: McpToolCallRequest,
    ) -> Result<BridgeResponse, magi_bridge_client::BridgeClientError> {
        self.calls
            .lock()
            .expect("recording mcp client mutex should lock")
            .push(request);
        Ok(BridgeResponse {
            ok: true,
            payload: "default-client-ok".to_string(),
        })
    }
}

fn temp_state_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "magi-daemon-runtime-test-{name}-{}",
        magi_core::UtcMillis::now().0
    ));
    fs::create_dir_all(&root).expect("temp state root should be creatable");
    root
}

fn test_workspace_root(config: &DaemonConfig) -> PathBuf {
    super::test_fixture_workspace_root(&config.state_root)
}

fn seed_registered_test_workspace(config: &DaemonConfig, session_id: Option<&str>) {
    let workspace_root = test_workspace_root(config);
    fs::create_dir_all(&workspace_root).expect("test workspace should be creatable");

    let workspace_store = WorkspaceStore::new();
    let workspace_id = WorkspaceId::new("test-workspace-001");
    workspace_store
        .register(
            workspace_id.clone(),
            AbsolutePath::new(workspace_root.to_string_lossy().to_string()),
        )
        .expect("test workspace should be registrable");
    workspace_store
        .activate(&workspace_id)
        .expect("test workspace should be activatable");

    let repository = StateRepository::new(config.state_root.clone());
    repository
        .save_workspace_durable_state(&workspace_store.durable_state())
        .expect("test workspace state should persist");

    if let Some(session_id) = session_id {
        let session_store = SessionStore::new();
        session_store
            .create_session_for_workspace(
                SessionId::new(session_id),
                "runtime test session".to_string(),
                Some(workspace_id.to_string()),
            )
            .expect("test session should be creatable");
        repository
            .save_session_projection_state(
                &session_store.durable_state(),
                &session_store.execution_sidecar_store_state(),
            )
            .expect("test session state should persist");
    }
}

fn restore_task_fixture(
    task_id: &str,
    root_task_id: &str,
    parent_task_id: Option<&str>,
    status: TaskStatus,
    created_at: u64,
) -> Task {
    Task {
        task_id: TaskId::new(task_id),
        mission_id: MissionId::new("mission-spawn-graph-restore"),
        root_task_id: TaskId::new(root_task_id),
        parent_task_id: parent_task_id.map(TaskId::new),
        kind: TaskKind::LocalAgent,
        title: format!("task {task_id}"),
        goal: format!("run task {task_id}"),
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
        created_at: UtcMillis(created_at),
        updated_at: UtcMillis(created_at + 1),
    }
}

#[tokio::test]
async fn daemon_restart_recovers_detached_agent_question_without_replaying_execution() {
    use magi_conversation_runtime::session_writeback::session_turn_item;
    use magi_session_store::user_question::{PendingUserQuestion, UserQuestionResponse};
    use magi_session_store::{CanonicalTurnStatus, ExecutionThread, ExecutionThreadStatus};
    // 两个独立测试进程分别执行首次恢复/回答和再次重启，不能复用父进程的
    // SessionStore、TaskStore、resolver、线程或审批/问题注册表。
    const CHILD_STATE: &str = "MAGI_INDEPENDENT_AGENT_RESTART_TEST_STATE";
    const CHILD_STAGE: &str = "MAGI_INDEPENDENT_AGENT_RESTART_TEST_STAGE";
    if let Ok(root) = std::env::var(CHILD_STATE) {
        let config = DaemonConfig::new("127.0.0.1", 0, "independent-restart", root);
        let runtime = DaemonRuntime::restore(&config).unwrap();
        let (_, state) = runtime.router_with_state_for_tests("independent-restart".into());
        let session = SessionId::new("detached-agent-restart");
        let child = TaskId::new("restart-independent");
        let store = state.task_store().unwrap();
        assert_eq!(
            store
                .get_task(&TaskId::new("restart-source"))
                .unwrap()
                .status,
            TaskStatus::Completed
        );
        assert_eq!(store.get_task(&child).unwrap().status, TaskStatus::Failed);
        let turn = state
            .session_store
            .agent_turn_for_task(&session, &child)
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CanonicalTurnStatus::Interrupted);
        assert!(turn.model_steps.is_empty(), "恢复不能自动向 Provider 重发");
        assert!(
            !state
                .session_store
                .turn_execution_is_active(&session, &turn.turn_id)
        );
        assert!(
            state
                .session_store
                .runtime_sidecar(&session)
                .unwrap()
                .active_execution_chain
                .is_none()
        );
        assert!(store.collect_all_active_leases().is_empty());
        assert_eq!(store.all_tasks().len(), 2);
        assert!(state.task_execution_registry().get(&child).is_none());
        if std::env::var(CHILD_STAGE).unwrap() == "answer" {
            let pending = state
                .session_store
                .pending_user_questions(&session)
                .unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].question_id, "detached-question");
            assert_eq!(pending[0].task_id.as_ref(), Some(&child));
            assert_eq!(pending[0].turn_id, turn.turn_id);
            state
                .session_store
                .resolve_user_question(&session, "detached-question", UserQuestionResponse::Skipped)
                .unwrap();
            assert!(
                !state
                    .session_store
                    .turn_execution_is_active(&session, &turn.turn_id)
            );
        } else {
            assert!(
                state
                    .session_store
                    .pending_user_questions(&session)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                state
                    .session_store
                    .user_question_state(&session, "detached-question")
                    .unwrap()
                    .unwrap()
                    .response,
                Some(UserQuestionResponse::Skipped)
            );
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let config = DaemonConfig::new("127.0.0.1", 0, "independent-restart", directory.path());
    let repository = StateRepository::new(config.state_root.clone());
    let store = SessionStore::new();
    store.install_session_event_writer(Arc::new(repository.clone()));
    let session = SessionId::new("detached-agent-restart");
    store
        .create_session(session.clone(), "detached agent")
        .unwrap();
    let now = UtcMillis::now();
    let mut root = restore_task_fixture(
        "restart-source",
        "restart-source",
        None,
        TaskStatus::Running,
        now.0,
    );
    root.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("coordinator"));
    let (_, root_thread) = store.ensure_session_mission(&session, now, || root.mission_id.clone());
    let mut user = session_turn_item(
        "user_message",
        "completed",
        None,
        Some("run independent work".into()),
        Some("source-user".into()),
        root_thread,
    );
    user.task_id = Some(root.task_id.clone());
    user.item_seq = 1;
    let source_turn = "detached-agent-source-turn";
    let admitted_turn = ActiveExecutionTurn {
        turn_id: source_turn.into(),
        turn_seq: 1,
        accepted_at: now,
        completed_at: None,
        status: "running".into(),
        user_message: Some("run independent work".into()),
        items: vec![user],
    };
    store
        .accept_active_execution_chain_with_timeline_entry(
            session.clone(),
            magi_session_store::TimelineEntryInput::new(
                format!("timeline-chain-{}", admitted_turn.turn_id),
                magi_session_store::TimelineEntryKind::UserMessage,
                admitted_turn.user_message.clone().unwrap_or_default(),
                admitted_turn.accepted_at,
            ),
            ActiveExecutionChain {
                session_id: session.clone(),
                mission_id: root.mission_id.clone(),
                root_task_id: root.task_id.clone(),
                execution_chain_ref: "detached-agent-chain".into(),
                workspace_id: None,
                branches: vec![],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "source-user".into(),
                    trimmed_text: Some("run independent work".into()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
        )
        .unwrap();
    let tasks = TaskStore::new();
    tasks.insert_task(root.clone()).unwrap();
    let mut child = restore_task_fixture(
        "restart-independent",
        root.task_id.as_str(),
        Some(root.task_id.as_str()),
        TaskStatus::Pending,
        now.0,
    );
    let mut executor = magi_core::TaskExecutorBinding::for_role("executor");
    executor.agent_activation = Some(magi_core::AgentActivation {
        agent_id: child.task_id.clone(),
        session_id: session.clone(),
        lifetime: magi_core::AgentLifetime::Session,
        generation: 1,
        previous_task_id: None,
    });
    child.executor_binding = Some(executor);
    tasks.insert_task(child.clone()).unwrap();
    let thread = ThreadId::new("detached-agent-thread");
    store
        .register_thread(ExecutionThread {
            thread_id: thread.clone(),
            session_id: session.clone(),
            mission_id: child.mission_id.clone(),
            role_id: "executor".into(),
            worker_instance_id: WorkerId::new("detached-agent-worker"),
            status: ExecutionThreadStatus::Active,
            created_at: now,
            last_used_at: now,
            observed_context_window_tokens: None,
            handled_task_ids: vec![child.task_id.clone()],
        })
        .unwrap();
    let owned = store
        .create_agent_turn(&child, source_turn, &thread)
        .unwrap();
    // 模拟 TaskStore 回调先于显式 Worker 结果到达。即使计划尚指向父轮，
    // 或 Dispatcher 已移除计划，通知也必须收口子代理自己的 Turn。
    let registry = TaskExecutionRegistry::default();
    registry
        .insert(
            child.task_id.clone(),
            magi_conversation_runtime::task_execution_registry::TaskExecutionPlan::Dispatch {
                target: magi_core::TaskExecutionTarget {
                    mission_id: child.mission_id.clone(),
                    root_task_id: child.root_task_id.clone(),
                    task_id: child.task_id.clone(),
                    requested_worker_id: None,
                    recovery_id: None,
                    execution_chain_ref: None,
                },
                worker_id: WorkerId::new("detached-agent-worker"),
                thread_id: thread.clone(),
                is_primary: false,
                session_id: session.clone(),
                turn_id: source_turn.into(),
                workspace_id: None,
                execution_root: None,
                writebacks: Default::default(),
                use_tools: true,
                skill_name: None,
                images: Vec::new(),
                execution_settings_snapshot: None,
            },
        )
        .unwrap();
    for has_plan in [true, false] {
        if !has_plan {
            registry.remove(&child.task_id);
        }
        assert_eq!(
            super::terminal_task_context(&store, &registry, &child).unwrap(),
            Some((session.clone(), owned.turn_id.clone())),
            "Agent 终态来源不能被同 root 的当前主线覆盖"
        );
    }
    assert_eq!(
        super::terminal_task_context(&store, &registry, &root).unwrap(),
        Some((session.clone(), source_turn.into())),
        "主线仍从原执行链解析自己的 Turn"
    );
    let mut question_item = session_turn_item(
        "tool_call_started",
        "running",
        None,
        None,
        Some("detached-question-item".into()),
        thread,
    );
    question_item.task_id = Some(child.task_id.clone());
    question_item.role_id = Some("executor".into());
    question_item.tool_call_id = Some("detached-question-call".into());
    question_item.tool_name = Some("ask_user_question".into());
    question_item.tool_arguments = Some("{}".into());
    store
        .upsert_agent_turn_item(&session, &child.task_id, &owned.turn_id, question_item)
        .unwrap();
    let question = PendingUserQuestion {
            question_id: "detached-question".into(), session_id: session.clone(), task_id: Some(child.task_id.clone()), turn_id: owned.turn_id.clone(), tool_call_id: "detached-question-call".into(),
            requested_at: UtcMillis::now(), can_skip: true,
            questions: magi_session_store::user_question::parse_questions(&json!({"questions":[{"header":"选择","question":"继续？","options":[{"label":"A"},{"label":"B"}]}]}).to_string()).unwrap(),
        };
    store.register_user_question(question.clone()).unwrap();
    tasks
        .transfer_agent_subtree(
            &root.task_id,
            &child.task_id,
            magi_core::AgentRunTransfer {
                operation_id: "detached-transfer".into(),
                session_id: session.clone(),
                source_turn_id: source_turn.into(),
                transferred_by: root.task_id.clone(),
                activation_generation: 1,
                owner: magi_core::AgentRunOwner::Session,
                transferred_at: UtcMillis::now(),
            },
        )
        .unwrap();
    tasks
        .complete_task(
            &root.task_id,
            magi_core::TaskCompletionAttempt {
                final_response: Some("source done".into()),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .update_current_turn_status_for_turn_with_change(&session, Some(source_turn), "completed")
        .unwrap();
    store
        .archive_active_execution_chain(&session, &root.task_id, Some(source_turn))
        .unwrap();
    assert_eq!(
        super::task_status_event_scope(&store, &child),
        Some((session.clone(), None)),
        "原主线归档后，子代理状态事件仍属于原会话"
    );
    assert_eq!(
        super::terminal_task_context(&store, &registry, &child).unwrap(),
        Some((session.clone(), owned.turn_id.clone()))
    );
    repository
        .save_session_projection_state(
            &store.durable_state(),
            &store.execution_sidecar_store_state(),
        )
        .unwrap();
    tasks
        .checkpoint_to_projection_directory(&repository.task_store_projection_path())
        .unwrap();
    for stage in ["answer", "verify"] {
        let output = magi_process::std_command(std::env::current_exe().unwrap())
                .args(["--exact", "daemon::runtime::tests::daemon_restart_recovers_detached_agent_question_without_replaying_execution", "--nocapture"])
                .env(CHILD_STATE, directory.path())
                .env(CHILD_STAGE, stage)
                .output().unwrap();
        assert!(
            output.status.success(),
            "{stage}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn agent_role_catalog_provider_exports_spawnable_roles() {
    let registry = std::sync::Arc::new(magi_agent_role::AgentRoleRegistry::load_default());
    let provider =
        build_agent_role_catalog_provider(registry, Arc::new(SettingsStore::new()), false);
    let roles = provider();

    assert!(
        roles
            .iter()
            .any(|role| role.role_id == "executor" && role.spawnable),
        "executor 应作为 agent_spawn 可派发角色暴露"
    );
    assert!(
        roles
            .iter()
            .any(|role| role.role_id == "coordinator" && !role.spawnable),
        "coordinator 是主线编排身份，不应作为可派发代理暴露"
    );
    assert!(
        roles.iter().any(|role| role
            .supported_kinds
            .iter()
            .any(|kind| kind == "local_agent")),
        "代理角色目录应暴露 supported_kinds，便于 tool_catalog 诊断"
    );
    assert!(
        roles.iter().any(|role| role
            .capability_ids
            .iter()
            .any(|capability| capability == "general_engineering")),
        "代理角色目录应暴露 capability_ids，供 agent_spawn 选择默认能力"
    );
    assert!(
        roles
            .iter()
            .all(|role| !role.display_name.trim().is_empty()),
        "代理角色目录必须返回用户可见 display_name"
    );
}

#[test]
fn external_tool_catalog_uses_stable_bridge_labels() {
    let registry = magi_skill_runtime::SkillRegistry::new();
    registry.register(magi_skill_runtime::SkillDefinition {
        description: String::new(),
        skill_id: "catalog-skill".to_string(),
        title: "Catalog Skill".to_string(),
        instruction: "test".to_string(),
        metadata: magi_skill_runtime::SkillMetadata {
            category: "test".to_string(),
            tags: Vec::new(),
        },
        restrict_standard_tools: true,
        allowed_tools: Vec::new(),
        custom_tool_bindings: vec![magi_skill_runtime::CustomToolBinding {
            binding_id: "catalog-binding".to_string(),
            tool_name: "catalog.inspect".to_string(),
            description: "Inspect catalog".to_string(),
            bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp,
            dispatch_action: magi_bridge_client::BridgeDispatchAction::McpToolCall,
            bridge_target: "local-mcp".to_string(),
        }],
        prompt_priority: 50,
    });
    let provider = super::build_external_tool_catalog_provider(
        std::sync::Arc::new(SettingsStore::default()),
        std::sync::Arc::new(magi_skill_runtime::SkillRuntime::new(registry)),
        std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
    );

    let snapshot = provider();
    let tool = snapshot
        .skill_tools
        .iter()
        .find(|tool| tool.name == "catalog.inspect")
        .expect("skill custom tool should be exported");

    assert_eq!(tool.bridge_kind, "mcp");
    assert_eq!(tool.dispatch_action, "mcp_tool_call");
    assert_eq!(tool.access_profile_behavior, "restricted_blocks_high_risk");
    assert_eq!(tool.approval_requirement, "required");
}

#[test]
fn failed_task_status_event_keeps_scope_and_sanitized_direct_error() {
    let event_bus = magi_event_bus::InMemoryEventBus::new(16);
    let session_store = SessionStore::new();
    let session_id = SessionId::new("session-task-status-scope");
    let workspace_id = WorkspaceId::new("workspace-task-status-scope");
    let mission_id = MissionId::new("mission-task-status-scope");
    let task_id = TaskId::new("task-status-scope");
    let worker_id = WorkerId::new("worker-task-status-scope");
    let now = UtcMillis::now();
    session_store
        .create_session_for_workspace(
            session_id.clone(),
            "task status scope",
            Some(workspace_id.to_string()),
        )
        .expect("session should be created");
    let admitted_turn = ActiveExecutionTurn {
        turn_id: "turn-task-status-scope".to_string(),
        turn_seq: now.0,
        accepted_at: now,
        completed_at: None,
        status: "running".to_string(),
        user_message: Some("run scoped task".to_string()),
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
                root_task_id: task_id.clone(),
                execution_chain_ref: "chain-task-status-scope".to_string(),
                workspace_id: Some(workspace_id.clone()),
                branches: vec![ActiveExecutionBranch {
                    task_id: task_id.clone(),
                    worker_id,
                    use_tools: true,
                    skill_name: None,
                    is_primary: true,
                    thread_id: ThreadId::new("thread-task-status-scope"),
                }],
                dispatch_context: ActiveExecutionDispatchContext {
                    accepted_at: now,
                    entry_id: "timeline-task-status-scope".to_string(),
                    trimmed_text: Some("run scoped task".to_string()),
                    skill_name: None,
                },
                current_turn_id: Some(admitted_turn.turn_id.clone()),
            },
            admitted_turn,
        )
        .expect("active execution chain should be stored");
    let mut task = restore_task_fixture(
        task_id.as_str(),
        task_id.as_str(),
        None,
        TaskStatus::Running,
        now.0,
    );
    task.mission_id = mission_id;
    task.output_refs =
        vec!["provider timeout at /Users/xie/code/model.rs with sk-test-secret-value".to_string()];

    publish_task_status_changed_event(
        &event_bus,
        &session_store,
        &task_id,
        TaskStatus::Running,
        TaskStatus::Failed,
        &task,
    );

    let event = event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .find(|event| event.event_type == "task.status.changed")
        .expect("task status event should be published");
    assert_eq!(event.session_id.as_ref(), Some(&session_id));
    assert_eq!(event.workspace_id.as_ref(), Some(&workspace_id));
    assert_eq!(event.payload["session_id"], json!(session_id.as_str()));
    assert_eq!(event.payload["workspace_id"], json!(workspace_id.as_str()));
    assert_eq!(event.payload["root_task_id"], json!(task_id.as_str()));
    assert_eq!(event.payload["title"], json!(task.title));
    assert_eq!(event.payload["failure_stage"], json!("task_execution"));
    let failure_detail = event.payload["failure_detail"]
        .as_str()
        .expect("failed event should include direct error");
    assert!(failure_detail.contains("provider timeout"));
    assert!(failure_detail.contains("[path]"));
    assert!(failure_detail.contains("sk-[redacted]"));
    assert!(!failure_detail.contains("/Users/xie"));
    assert!(!failure_detail.contains("test-secret-value"));
}

#[test]
fn terminal_task_reconciliation_clears_only_stale_active_threads() {
    let session_store = SessionStore::new();
    let task_store = TaskStore::new();
    let session_id = SessionId::new("session-terminal-thread-reconcile");
    let mission_id = MissionId::new("mission-terminal-thread-reconcile");
    let completed_task_id = TaskId::new("task-terminal-thread-completed");
    let running_task_id = TaskId::new("task-terminal-thread-running");

    let mut completed_task = restore_task_fixture(
        completed_task_id.as_str(),
        completed_task_id.as_str(),
        None,
        TaskStatus::Completed,
        1_000,
    );
    completed_task.mission_id = mission_id.clone();
    task_store
        .insert_task(completed_task)
        .expect("已完成任务应插入");
    let mut running_task = restore_task_fixture(
        running_task_id.as_str(),
        running_task_id.as_str(),
        None,
        TaskStatus::Running,
        2_000,
    );
    running_task.mission_id = mission_id.clone();
    task_store
        .insert_task(running_task)
        .expect("运行中任务应插入");

    for (thread_id, task_id) in [
        ("thread-terminal-completed", &completed_task_id),
        ("thread-terminal-running", &running_task_id),
    ] {
        session_store
            .register_thread(magi_session_store::ExecutionThread {
                thread_id: ThreadId::new(thread_id),
                session_id: session_id.clone(),
                mission_id: mission_id.clone(),
                role_id: "reviewer".to_string(),
                worker_instance_id: WorkerId::new(format!("worker-{thread_id}")),
                status: magi_session_store::ExecutionThreadStatus::Active,
                created_at: UtcMillis(1_000),
                last_used_at: UtcMillis(2_000),
                observed_context_window_tokens: None,
                handled_task_ids: vec![task_id.clone()],
            })
            .expect("终态 thread 测试数据应注册成功");
    }

    assert_eq!(
        reconcile_terminal_task_execution_threads(&session_store, &task_store),
        1
    );

    let threads = session_store.thread_registry_snapshot(&session_id);
    let status = |thread_id: &str| {
        threads
            .iter()
            .find(|thread| thread.thread_id.as_str() == thread_id)
            .map(|thread| thread.status)
            .expect("thread should exist")
    };
    assert_eq!(
        status("thread-terminal-completed"),
        magi_session_store::ExecutionThreadStatus::Idle
    );
    assert_eq!(
        status("thread-terminal-running"),
        magi_session_store::ExecutionThreadStatus::Active
    );
}

#[test]
fn daemon_restore_fails_only_session_owned_root_tasks_without_execution_chain() {
    let session_store = Arc::new(SessionStore::new());
    let session_store_for_status = Arc::clone(&session_store);
    let task_store =
        TaskStore::with_status_change_callback(Box::new(move |task_id, _, new_status, _| {
            if matches!(
                new_status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
            ) {
                session_store_for_status.mark_task_threads_idle(task_id, UtcMillis::now());
            }
        }));
    let session_id = SessionId::new("session-orphan-root-reconcile");
    let mission_id = MissionId::new("mission-orphan-root-reconcile");
    let orphan_task_id = TaskId::new("task-orphan-root-reconcile");
    let unrelated_task_id = TaskId::new("task-unrelated-root-reconcile");
    let now = UtcMillis(5_000);

    session_store
        .create_session(session_id.clone(), "orphan root reconcile")
        .expect("session should be created");
    session_store
        .register_thread(magi_session_store::ExecutionThread {
            thread_id: ThreadId::new("thread-orphan-root-reconcile"),
            session_id,
            mission_id: mission_id.clone(),
            role_id: "coordinator".to_string(),
            worker_instance_id: WorkerId::new("worker-orphan-root-reconcile"),
            status: magi_session_store::ExecutionThreadStatus::Active,
            created_at: now,
            last_used_at: now,
            observed_context_window_tokens: None,
            handled_task_ids: vec![orphan_task_id.clone()],
        })
        .expect("孤儿 root thread 测试数据应注册成功");
    let mut orphan_task = restore_task_fixture(
        orphan_task_id.as_str(),
        orphan_task_id.as_str(),
        None,
        TaskStatus::Pending,
        now.0,
    );
    orphan_task.mission_id = mission_id.clone();
    task_store.insert_task(orphan_task).expect("孤立任务应插入");
    let mut unrelated_task = restore_task_fixture(
        unrelated_task_id.as_str(),
        unrelated_task_id.as_str(),
        None,
        TaskStatus::Pending,
        now.0,
    );
    unrelated_task.mission_id = mission_id;
    task_store
        .insert_task(unrelated_task)
        .expect("无关任务应插入");

    assert_eq!(
        fail_orphan_session_root_tasks(session_store.as_ref(), &task_store),
        1
    );
    let orphan_task = task_store
        .get_task(&orphan_task_id)
        .expect("orphan task should remain as terminal history");
    assert_eq!(orphan_task.status, TaskStatus::Failed);
    assert!(
        orphan_task
            .output_refs
            .iter()
            .any(|output| output.contains("未绑定 execution chain"))
    );
    assert_eq!(
        task_store
            .get_task(&unrelated_task_id)
            .expect("unrelated task should remain")
            .status,
        TaskStatus::Pending,
        "没有会话归属的任务不能被 session 恢复逻辑处理",
    );
    assert_eq!(
        session_store.thread_registry_snapshot(&SessionId::new("session-orphan-root-reconcile"))[0]
            .status,
        magi_session_store::ExecutionThreadStatus::Idle,
        "孤立任务进入终态时必须同步收口对应 thread",
    );
}

#[test]
fn external_mcp_catalog_redacts_raw_error_text() {
    let connections = std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    let raw = json!({
        "id": "local-mcp",
        "name": "Local MCP",
        "enabled": true,
        "error": "/Users/xie/.mcp/server failed: ENOENT",
    });

    let entry = super::external_mcp_server_catalog_entry(&raw, &connections)
        .expect("mcp catalog entry should be built");

    assert_eq!(entry.error.as_deref(), Some("mcp_connection_failed"));
}

#[test]
fn external_mcp_catalog_keeps_disabled_servers_non_error() {
    let connections = std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    let raw = json!({
        "id": "disabled-mcp",
        "name": "Disabled MCP",
        "enabled": false,
        "toolCount": 3,
        "error": "/private/raw error"
    });

    let entry = super::external_mcp_server_catalog_entry(&raw, &connections)
        .expect("mcp catalog entry should be built");

    assert!(!entry.enabled);
    assert!(!entry.connected);
    assert_eq!(entry.health, "disabled");
    assert_eq!(entry.tool_count, None);
    assert_eq!(entry.error, None);
}

#[test]
fn settings_backed_mcp_bridge_rejects_missing_targets_without_environment_client() {
    let client = SettingsBackedMcpBridgeClient::new(
        Arc::new(SettingsStore::new()),
        Arc::new(RwLock::new(HashMap::new())),
        None,
    );
    for target in ["", "missing", "loopback-mcp"] {
        let error = client
            .call_tool(McpToolCallRequest {
                server_name: target.into(),
                tool_name: "echo.inspect".into(),
                input: "{}".into(),
            })
            .expect_err("unconfigured targets must not produce simulated success");
        assert!(matches!(
            error,
            BridgeClientError::MissingClient {
                bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp
            }
        ));
    }
}

#[test]
fn settings_backed_mcp_bridge_uses_explicit_environment_client() {
    let settings_store = Arc::new(SettingsStore::new());
    let default_client = Arc::new(RecordingMcpClient::default());
    let calls = default_client.calls.clone();
    let client = SettingsBackedMcpBridgeClient::new(
        settings_store,
        Arc::new(RwLock::new(HashMap::new())),
        Some(default_client),
    );

    let response = client
        .call_tool(McpToolCallRequest {
            server_name: "environment-mcp".to_string(),
            tool_name: "echo.inspect".to_string(),
            input: "{}".to_string(),
        })
        .expect("explicit environment MCP client should receive the target");

    assert!(response.ok);
    let calls = calls
        .lock()
        .expect("recording mcp client mutex should lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].server_name, "environment-mcp");
}

#[test]
fn settings_backed_mcp_bridge_rejects_disabled_configured_target_before_default_client() {
    let settings_store = Arc::new(SettingsStore::new());
    settings_store
        .set_section(
            "mcpServers",
            json!([
                {
                    "id": "disabled-mcp",
                    "name": "disabled-mcp",
                    "command": "node",
                    "enabled": false
                }
            ]),
        )
        .unwrap();
    let default_client = Arc::new(RecordingMcpClient::default());
    let calls = default_client.calls.clone();
    let connections = Arc::new(RwLock::new(HashMap::new()));
    connections
        .write()
        .expect("mcp connection pool should lock")
        .insert(
            "disabled-mcp".to_string(),
            Arc::new(McpServerClient::from_stdio(McpServerConfig {
                command: "node".to_string(),
                args: Vec::new(),
                working_directory: None,
                env: BTreeMap::new(),
                request_timeout: std::time::Duration::from_secs(30),
            })),
        );
    let client = SettingsBackedMcpBridgeClient::new(
        settings_store,
        connections.clone(),
        Some(default_client),
    );

    let error = client
        .call_tool(McpToolCallRequest {
            server_name: "disabled-mcp".to_string(),
            tool_name: "echo.inspect".to_string(),
            input: "{}".to_string(),
        })
        .expect_err("disabled configured target should not use default bridge client");

    assert!(matches!(
        error,
        magi_bridge_client::BridgeClientError::MissingClient { .. }
    ));
    assert!(
        calls
            .lock()
            .expect("recording mcp client mutex should lock")
            .is_empty(),
        "configured disabled target must not fall through to the default bridge client"
    );
    assert!(
        !connections
            .read()
            .expect("mcp connection pool should lock")
            .contains_key("disabled-mcp"),
        "disabled configured target must clear stale settings-backed connection"
    );
}

#[tokio::test]
async fn daemon_tools_catalog_route_exposes_runtime_catalog() {
    let state_root = temp_state_root("tools-catalog-route");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let workspace_root = test_workspace_root(&config);
    fs::create_dir_all(workspace_root.join("src"))
        .expect("bootstrap workspace source directory should be creatable");
    fs::write(
        workspace_root.join("src/lib.rs"),
        "pub fn tool_catalog_route_probe() -> bool { true }\n",
    )
    .expect("bootstrap workspace source file should be writable");
    seed_registered_test_workspace(&config, Some("test-session-001"));
    let runtime = DaemonRuntime::restore(&config).expect("runtime restore should load state");
    let app = runtime
        .router("daemon-test".to_string())
        .expect("测试路由应成功构造");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let _ = get_json(app.clone(), "/api/knowledge?workspaceId=test-workspace-001").await;
            if runtime
                .knowledge_store
                .workspace_index_ready(&WorkspaceId::new("test-workspace-001"))
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("tool catalog test workspace index should become ready");
    let catalog = get_json(
            app,
            "/api/tools/catalog?scope=workspace&workspaceId=test-workspace-001&sessionId=test-session-001&includeInternal=true&includeSchema=true",
        )
        .await;

    assert_eq!(catalog["tool"], "tool_catalog");
    assert_eq!(catalog["status"], "succeeded");
    assert_eq!(catalog["externalCatalogStatus"], "available");
    assert_eq!(catalog["agentRoleCatalogStatus"], "available");
    assert!(
        catalog["spawnableAgentRoleCount"]
            .as_u64()
            .expect("spawnable agent role count should serialize")
            > 0,
        "daemon tool catalog should expose agent_spawn-capable roles"
    );
    let workspace_code_index = catalog["runtimeDependencies"]
        .as_array()
        .expect("runtime dependencies should be an array")
        .iter()
        .find(|dependency| dependency["name"] == "workspace_code_index")
        .expect("workspace_code_index dependency should be exposed");
    assert_eq!(workspace_code_index["status"], "ready");
    assert!(workspace_code_index.get("workspaceId").is_none());
    let context_runtime = catalog["runtimeDependencies"]
        .as_array()
        .expect("runtime dependencies should be an array")
        .iter()
        .find(|dependency| dependency["name"] == "context_runtime")
        .expect("context_runtime dependency should be exposed");
    assert_eq!(context_runtime["status"], "ready");
    assert!(context_runtime.get("workspaceId").is_none());
    assert!(context_runtime.get("sessionId").is_none());
    let process_launch = catalog["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .find(|tool| tool["name"] == "process_launch")
        .expect("includeInternal=true should expose process_launch");
    assert_eq!(process_launch["public"], false);
    assert_eq!(process_launch["parametersSchema"]["type"], "object");
}

#[test]
fn restore_keeps_empty_state_and_persists_runtime_files() {
    let state_root = temp_state_root("bootstrap");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());

    let runtime = DaemonRuntime::restore(&config).expect("runtime restore should keep empty state");

    assert!(runtime.session_store.current_session().is_none());
    assert!(runtime.workspace_store.workspaces().is_empty());
    assert!(runtime.workspace_store.snapshots().is_empty());
    assert!(state_root.join("workspaces.json").exists());
    assert!(!state_root.join("workspace-recovery-sidecars.json").exists());
    assert!(state_root.join("audit-usage-ledger").is_dir());

    let ledger = AuditUsageLedgerSnapshot::load_from_dir(
        &state_root.join("audit-usage-ledger"),
        magi_core::UtcMillis::now(),
    )
    .expect("audit usage ledger should load");
    assert!(ledger.audit_entries.is_empty());
    assert!(ledger.usage_entries.is_empty());
}

#[tokio::test]
async fn portable_project_registration_restores_history_without_daemon_restart() {
    let root = tempfile::tempdir().unwrap();
    let source_config = DaemonConfig::new(
        "127.0.0.1",
        0,
        "daemon-test",
        root.path().join("source-state"),
    );
    seed_registered_test_workspace(&source_config, Some("portable-hot-session"));
    let source = test_workspace_root(&source_config);
    let repository = StateRepository::new(source_config.state_root.clone());
    let projection_dir = source.join(".magi/session-projections");
    fs::create_dir_all(&projection_dir).unwrap();
    let name = "portable-hot-session.json";
    fs::copy(
        repository
            .project_storage_root("test-workspace-001")
            .join(".magi/session-projections")
            .join(name),
        projection_dir.join(name),
    )
    .unwrap();
    let projection_path = projection_dir.join(name);
    let mut projection: serde_json::Value =
        serde_json::from_slice(&fs::read(&projection_path).unwrap()).unwrap();
    projection["durable"]["timeline"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "entryId":"portable-user-message", "sessionId":"portable-hot-session",
            "kind":"UserMessage", "message":"retained portable history", "occurredAt":1
        }));
    fs::write(
        &projection_path,
        serde_json::to_vec_pretty(&projection).unwrap(),
    )
    .unwrap();
    let other = root.path().join("other-source");
    fs::create_dir_all(&other).unwrap();
    for use_project_create in [false, true] {
        let target = DaemonConfig::new(
            "127.0.0.1",
            0,
            "daemon-test",
            root.path().join(if use_project_create {
                "target-project-state"
            } else {
                "target-register-state"
            }),
        );
        let runtime = DaemonRuntime::restore(&target).unwrap();
        let app = runtime.router("daemon-test".into()).unwrap();
        let (status, registered) = post_json(
            app.clone(),
            if use_project_create {
                "/api/workspaces/create"
            } else {
                "/api/workspaces/register"
            },
            if use_project_create {
                json!({"name":"portable multi-source", "sourceFolders":[
                {"sourceFolderId":"old", "path":source}, {"sourceFolderId":"new", "path":other}
            ], "primarySourceFolderId":"new"})
            } else {
                json!({"path":source})
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{registered}");
        assert_eq!(registered["workspaceId"], "test-workspace-001");
        if use_project_create {
            let project = runtime
                .workspace_store
                .project(&WorkspaceId::new("test-workspace-001"))
                .unwrap();
            assert_eq!(
                project.primary_source_folder().native_path(),
                other.canonicalize().unwrap()
            );
            assert_eq!(project.source_folders.len(), 2);
        }
        let history = get_json(
            app.clone(),
            "/api/workspaces/sessions?workspaceId=test-workspace-001",
        )
        .await;
        assert_eq!(
            history["sessions"][0]["sessionId"],
            "portable-hot-session",
            "{history}; {:?}",
            runtime
                .session_store
                .unavailable_sessions(Some("test-workspace-001"))
        );
        assert_eq!(history["sessions"].as_array().unwrap().len(), 1);
        let (status, _) = post_json(
            app,
            "/api/session/materialize",
            json!({"scope":"workspace","workspaceId":"test-workspace-001"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            runtime
                .session_store
                .sessions_for_workspace("test-workspace-001")
                .len(),
            2
        );
        drop(runtime);
        let restarted = DaemonRuntime::restore(&target).unwrap();
        assert_eq!(
            restarted
                .session_store
                .sessions_for_workspace("test-workspace-001")
                .len(),
            2
        );
        assert!(projection_dir.is_dir());
    }
}

#[test]
fn restore_does_not_rewrite_existing_session_projection_or_ledger() {
    let state_root = temp_state_root("restore-without-rewrite");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());

    let fixture = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("test fixture should create a persisted session");
    drop(fixture);

    let projection_path = fs::read_dir(
        StateRepository::new(state_root.clone())
            .project_storage_root("test-workspace-001")
            .join(".magi")
            .join("session-projections"),
    )
    .expect("session projection directory should exist")
    .map(|entry| {
        entry
            .expect("session projection entry should be readable")
            .path()
    })
    .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
    .expect("fixture should persist a session projection");
    let ledger_path = state_root.join("audit-usage-ledger");
    let projection_modified = fs::metadata(&projection_path)
        .expect("session projection metadata should exist")
        .modified()
        .expect("session projection mtime should exist");
    let ledger_modified = fs::metadata(&ledger_path)
        .expect("ledger metadata should exist")
        .modified()
        .expect("ledger mtime should exist");

    std::thread::sleep(Duration::from_millis(30));
    DaemonRuntime::restore(&config).expect("persisted runtime should restore");

    assert_eq!(
        fs::metadata(&projection_path)
            .expect("session projection should remain")
            .modified()
            .expect("session projection mtime should remain"),
        projection_modified,
        "恢复已有 session 不应重写 projection"
    );
    assert_eq!(
        fs::metadata(&ledger_path)
            .expect("ledger should remain")
            .modified()
            .expect("ledger mtime should remain"),
        ledger_modified,
        "恢复已有账本不应重复重写"
    );
}

#[test]
fn restart_repairs_missing_model_usage_from_canonical_receipts_once() {
    use magi_session_store::{
        ActiveExecutionTurn, ModelStep, ModelTransportOutcome, ModelTransportRecord,
        ModelTransportUsage, TimelineEntryInput, TimelineEntryKind,
    };
    for settled in [false, true] {
        let state_root = temp_state_root(if settled {
            "usage-after-settlement"
        } else {
            "usage-before-settlement"
        });
        let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
        let fixture = DaemonRuntime::restore_with_test_fixture(&config).unwrap();
        let session = SessionId::new("test-session-001");
        let at = UtcMillis::now();
        fixture
            .session_store
            .accept_current_turn_with_timeline_entry(
                session.clone(),
                TimelineEntryInput::new(
                    "usage-timeline",
                    TimelineEntryKind::UserMessage,
                    "work",
                    at,
                ),
                ActiveExecutionTurn {
                    turn_id: "usage-turn".into(),
                    turn_seq: 1,
                    accepted_at: at,
                    completed_at: None,
                    status: "running".into(),
                    user_message: Some("work".into()),
                    items: vec![],
                },
            )
            .unwrap();
        let step: ModelStep = serde_json::from_value(serde_json::json!({
                "stepId":"usage-step", "attempts":[{"attemptId":"usage-step:attempt:1", "startedAt":at.0,"settledAt":null,"status":"started"}],
                "threadId":"usage-thread", "checkpointGeneration":0,"requestFingerprint":"a".repeat(64),"provider":"configured",
                "messageCount":1,"toolCount":0,"inputRequestIds":[],"startedAt":at.0,"settledAt":null,"status":"started","responseFingerprint":null,
                "binding":{"model":"frozen-model","provider":"configured","bindingRevision":1,"settingsRevision":1,
                    "usageContext":{"templateId":"orchestrator","engineId":"orchestrator","bindingRevision":1,"role":"orchestrator","phase":"execution",
                        "modelConfig":{"provider":"configured","model":"frozen-model","baseUrl":"https://frozen.test/v1"}}}
            })).unwrap();
        fixture
            .session_store
            .begin_model_step(&session, "usage-turn", step)
            .unwrap();
        fixture
            .session_store
            .record_model_step_transport(
                &session,
                "usage-turn",
                "usage-step",
                ModelTransportRecord {
                    usage_observed: None,
                    response_timing: None,
                    observed_at: Some(at.0),
                    request_fingerprint: "b".repeat(64),
                    http_status: Some(200),
                    usage: Some(ModelTransportUsage {
                        input_tokens: 100,
                        output_tokens: 5,
                        cache_read_tokens: Some(40),
                        cache_write_tokens: None,
                        cache_read_included_in_input: true,
                    }),
                    outcome: Some(ModelTransportOutcome::Succeeded),
                },
            )
            .unwrap();
        if settled {
            fixture
                .session_store
                .settle_model_step(
                    &session,
                    "usage-turn",
                    "usage-step",
                    Some("c".repeat(64)),
                    None,
                    None,
                )
                .unwrap();
        }
        assert_eq!(fixture.event_bus.audit_usage_ledger_status().usage_count, 0);
        drop(fixture);
        for _ in 0..2 {
            let restored = DaemonRuntime::restore(&config).unwrap();
            let ledger = restored.event_bus.audit_usage_ledger_snapshot();
            assert_eq!(ledger.usage_entries.len(), 1);
            assert_eq!(
                ledger.usage_entries[0].payload["modelConfig"]["model"],
                "frozen-model"
            );
            assert_eq!(ledger.usage_entries[0].payload["usage"]["inputTokens"], 100);
            assert_eq!(
                ledger.usage_entries[0].payload["callIdentity"]["callId"],
                "usage-step:attempt:1"
            );
            assert_eq!(
                restored
                    .state_repository
                    .load_audit_usage_ledger()
                    .unwrap()
                    .usage_entries
                    .len(),
                1
            );
            let step = restored
                .session_store
                .canonical_turn_for_session_turn_id(&session, "usage-turn")
                .unwrap()
                .model_steps
                .remove(0);
            assert_eq!(step.usage_tokens, Some(105));
            assert_eq!(step.attempts.len(), 1, "恢复不得触发请求重放");
            assert_eq!(
                step.status,
                if settled {
                    magi_session_store::ModelStepStatus::ResponseReceived
                } else {
                    magi_session_store::ModelStepStatus::OutcomeUnknown
                }
            );
        }
    }
}

#[tokio::test]
async fn knowledge_endpoint_returns_code_index_after_bootstrap_scan() {
    let state_root = temp_state_root("knowledge-code-index");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let workspace_root = test_workspace_root(&config);

    // 在引导工作区中创建模拟源文件，供代码扫描器发现
    fs::create_dir_all(workspace_root.join("src")).unwrap();
    fs::write(
        workspace_root.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    fs::write(
        workspace_root.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    fs::write(
        workspace_root.join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();

    seed_registered_test_workspace(&config, None);
    let runtime = DaemonRuntime::restore(&config)
        .expect("runtime restore should load the registered workspace");

    let app = runtime
        .router("daemon-test".to_string())
        .expect("测试路由应成功构造");
    let knowledge = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let knowledge =
                get_json(app.clone(), "/api/knowledge?workspaceId=test-workspace-001").await;
            if knowledge["codeIndexStatus"]["status"] == "indexed" {
                return knowledge;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("knowledge endpoint should finish lazy index rebuild");

    // 知识接口按需建立索引，完成后存储中应存在同一份摘要。
    let summary = runtime
        .knowledge_store
        .code_index_summary_for_workspace(&WorkspaceId::new("test-workspace-001"))
        .expect("code index summary should exist after bootstrap scan");
    assert!(
        !summary.files.is_empty(),
        "code index should contain scanned files"
    );
    assert!(
        summary.tech_stack.iter().any(|t| t == "Rust"),
        "tech stack should detect Rust"
    );
    assert!(
        summary.entry_points.iter().any(|e| e.ends_with("main.rs")),
        "entry points should detect main.rs"
    );

    assert!(
        knowledge.get("codeIndex").is_some_and(|v| !v.is_null()),
        "API should return non-null codeIndex"
    );
    let code_index = knowledge["codeIndex"]
        .as_object()
        .expect("codeIndex should be an object");
    let files = code_index["files"]
        .as_array()
        .expect("files should be an array");
    assert!(!files.is_empty(), "codeIndex.files should not be empty");
    assert!(
        code_index["techStack"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "codeIndex.techStack should not be empty"
    );
}

#[tokio::test]
async fn restore_defers_code_index_rebuild_for_registered_workspaces() {
    let state_root = temp_state_root("multi-workspace-code-index");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root.clone());
    let secondary_root = test_fixture_sibling_root(&state_root, "secondary-workspace");
    fs::create_dir_all(secondary_root.join("src")).unwrap();
    fs::write(
        secondary_root.join("src/lib.rs"),
        "pub fn secondary_workspace_probe() -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        secondary_root.join("Cargo.toml"),
        "[package]\nname = \"secondary-workspace\"\n",
    )
    .unwrap();

    let secondary_workspace_id = {
        let runtime = DaemonRuntime::restore(&config)
            .expect("initial runtime restore should bootstrap state");
        let (status, body) = post_json(
            runtime
                .router("daemon-test".to_string())
                .expect("测试路由应成功构造"),
            "/api/workspaces/register",
            json!({ "path": secondary_root.to_string_lossy() }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let workspace_id = body["workspaceId"]
            .as_str()
            .expect("registered workspace id")
            .to_string();
        // register 把索引构建放到后台 spawn_blocking（见 workspaces.rs，
        // 修复大工作区注册阻塞服务），故注册返回后轮询等待索引就绪。
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if runtime
                    .knowledge_store
                    .workspace_index_ready(&WorkspaceId::new(&workspace_id))
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("newly registered workspace should build an in-process search index");
        workspace_id
    };

    let restored = DaemonRuntime::restore(&config)
        .expect("runtime restore should not block on code index rebuild");
    assert!(
        !restored
            .knowledge_store
            .workspace_index_ready(&WorkspaceId::new(&secondary_workspace_id)),
        "daemon restore must not synchronously rebuild every registered workspace index"
    );

    let bootstrap = get_json(
            restored
                .router("daemon-test".to_string())
                .expect("测试路由应成功构造"),
            &format!("/api/settings/bootstrap?scope=workspace&bootstrapScope=core&workspaceId={secondary_workspace_id}"),
        )
        .await;
    let workspace_code_index = bootstrap["capabilityDependencies"]
        .as_array()
        .expect("capability dependencies should be array")
        .iter()
        .find(|dependency| dependency["name"] == "workspace_code_index")
        .expect("workspace_code_index dependency should be exposed");
    assert_eq!(workspace_code_index["status"], "ready");
    assert!(workspace_code_index.get("workspaceId").is_none());

    let app = restored
        .router("daemon-test".to_string())
        .expect("测试路由应成功构造");
    let knowledge = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let knowledge = get_json(
                app.clone(),
                &format!("/api/knowledge?workspaceId={secondary_workspace_id}"),
            )
            .await;
            if knowledge["codeIndexStatus"]["status"] == "indexed" {
                return knowledge;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("knowledge endpoint should finish lazy index rebuild");
    assert_eq!(knowledge["codeIndexStatus"]["status"], "indexed");
    assert!(
        restored
            .knowledge_store
            .workspace_index_ready(&WorkspaceId::new(&secondary_workspace_id)),
        "knowledge endpoint should lazily rebuild the requested workspace index"
    );
}

async fn post_json(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    let status = response.status();
    let body = serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read"),
    )
    .expect("response should be valid json");
    (status, body)
}

async fn get_json(app: axum::Router, path: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read"),
    )
    .expect("response should be valid json")
}

async fn get_agent_run_projection(
    app: axum::Router,
    root_task_id: &str,
    session_id: &str,
    workspace_id: &str,
) -> Value {
    get_json(
            app,
            &format!(
                "/api/agent-runs/projection/{root_task_id}?scope=workspace&workspaceId={workspace_id}&sessionId={session_id}"
            ),
        )
        .await
}

async fn wait_for_agent_run_projection_completed(
    app: axum::Router,
    root_task_id: &str,
    session_id: &str,
    workspace_id: &str,
) -> Value {
    let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
    loop {
        let projection =
            get_agent_run_projection(app.clone(), root_task_id, session_id, workspace_id).await;
        let total_tasks = projection["progress_summary"]["total_tasks"]
            .as_u64()
            .unwrap_or(0);
        let completed_tasks = projection["progress_summary"]["completed_tasks"]
            .as_u64()
            .unwrap_or(0);
        if total_tasks >= 1
            && completed_tasks == total_tasks
            && projection["root_task"]["status"] == "completed"
        {
            // TaskStore 的 durable terminal 与 canonical Turn finalizer 由不同回调
            // 顺序完成；只有 Turn 也已经进入终态，后续同一 session 的请求才不会
            // 在这个等待窗口内被错误地判定为“已有活动 Turn”。
            let canonical_terminal = get_json(app.clone(), "/runtime/read-model").await["details"]
                ["sessions"]
                .as_array()
                .and_then(|sessions| {
                    sessions
                        .iter()
                        .find(|entry| entry["session_id"] == session_id)
                })
                .and_then(|entry| entry["current_turn"]["status"].as_str())
                .is_some_and(|status| {
                    matches!(
                        status,
                        "completed"
                            | "complete"
                            | "succeeded"
                            | "success"
                            | "failed"
                            | "error"
                            | "interrupted"
                            | "cancelled"
                            | "canceled"
                            | "blocked"
                            | "killed"
                            | "superseded"
                    )
                });
            if canonical_terminal {
                return projection;
            }
        }
        if Instant::now() >= deadline {
            return projection;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_execution_group(
    app: axum::Router,
    mission_id: &str,
    mut is_ready: impl FnMut(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
    loop {
        let read_model = get_json(app.clone(), "/runtime/read-model").await;
        if let Some(group) = read_model["details"]["execution_groups"]
            .as_array()
            .and_then(|groups| {
                groups
                    .iter()
                    .find(|entry| entry["mission_id"] == mission_id)
            })
            && (is_ready(group) || Instant::now() >= deadline)
        {
            return group.clone();
        }
        if Instant::now() >= deadline {
            panic!("execution group {mission_id} did not appear before timeout");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_completed_agent_run_projection(projection: &Value) {
    let total_tasks = projection["progress_summary"]["total_tasks"]
        .as_u64()
        .expect("total_tasks should serialize as integer");
    let completed_tasks = projection["progress_summary"]["completed_tasks"]
        .as_u64()
        .expect("completed_tasks should serialize as integer");
    // ExecutionChain：单 worker 任务只产出 1 个 root task；coordinator 才会扩展为多任务。
    assert!(
        total_tasks >= 1,
        "agent run projection should include the root task"
    );
    assert_eq!(completed_tasks, total_tasks);
    assert_eq!(projection["progress_summary"]["failed_tasks"], 0);
    assert_eq!(projection["root_task"]["status"], "completed");
}

#[tokio::test]
async fn router_session_action_auto_extraction_is_consumed_on_followup_dispatch() {
    let state_root = temp_state_root("router-session-action");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    seed_registered_test_workspace(&config, None);
    let runtime = DaemonRuntime::restore(&config).expect("runtime restore should load state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let active_workspace_id = state
        .workspace_registry
        .active_workspace_id()
        .expect("bootstrap workspace should exist");
    if state
        .session_store
        .session(&magi_core::SessionId::new("test-session-001"))
        .is_none()
    {
        state
            .session_store
            .create_session_for_workspace(
                magi_core::SessionId::new("test-session-001"),
                "runtime session".to_string(),
                Some(active_workspace_id.to_string()),
            )
            .expect("runtime session should be creatable");
    }

    let (status, first_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": "test-session-001",
            "text": "修复 parser constraint 记忆问题，完成后运行测试并汇总验证结果",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {first_body:?}");

    let first_accepted_at = first_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let first_mission_id = format!("mission-session-action-{first_accepted_at}");
    let first_root_task_id = first_body["rootTaskId"]
        .as_str()
        .expect("root_task_id should serialize as string");
    let read_model = get_json(app.clone(), "/runtime/read-model").await;
    let first_execution_group = read_model["details"]["execution_groups"]
        .as_array()
        .expect("execution groups should be an array")
        .iter()
        .find(|entry| entry["mission_id"] == first_mission_id)
        .expect("first execution group should exist");
    assert_eq!(first_execution_group["context_used_memory_count"], 0);
    assert_eq!(
        first_execution_group["context_memory_extraction_refs"]
            .as_array()
            .expect("refs should be array")
            .len(),
        0
    );
    let first_projection = wait_for_agent_run_projection_completed(
        app.clone(),
        first_root_task_id,
        "test-session-001",
        active_workspace_id.as_str(),
    )
    .await;
    assert_completed_agent_run_projection(&first_projection);

    let second_request_id = "request-router-session-action-followup";
    let (status, second_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": "test-session-001",
            "text": "修复 parser 后续工作问题，完成后运行测试并汇总验证结果",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
            "requestId": second_request_id,
            "userMessageId": "user-router-session-action-followup",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {second_body:?}");

    let _second_accepted_at = second_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    // session 一生一 mission：第二次派发复用第一次派发创建的 mission_id
    let second_mission_id = format!("mission-session-action-{first_accepted_at}");
    let second_root_task_id = if let Some(root_task_id) = second_body["rootTaskId"].as_str() {
        root_task_id.to_string()
    } else {
        assert_eq!(
            second_body["queued"], true,
            "忙碌 session 的 followup 应进入队列: {second_body:?}"
        );
        let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
        loop {
            if let Some(turn) = state
                .session_store
                .canonical_turn_for_request_id(second_request_id)
                && let Some(task_id) = turn
                    .items
                    .iter()
                    .find(|item| {
                        item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                    })
                    .and_then(|item| item.worker.as_ref())
                    .and_then(|worker| worker.task_id.as_ref())
            {
                break task_id.to_string();
            }
            if Instant::now() >= deadline {
                panic!("queued followup was not accepted into canonical Turn before timeout");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let expected_extraction_id = format!(
        "extract-session-action-test-session-001-{first_accepted_at}-timeline-test-session-001-{first_accepted_at}"
    );

    let second_execution_group =
        wait_for_execution_group(app.clone(), &second_mission_id, |entry| {
            entry["context_memory_extraction_refs"] == json!([expected_extraction_id])
                && entry["context_used_memory_count"] == 1
                && entry["context_extracted_memory_count"] == 1
        })
        .await;
    assert_eq!(second_execution_group["context_used_memory_count"], 1);
    assert_eq!(second_execution_group["context_extracted_memory_count"], 1);
    assert_eq!(
        second_execution_group["context_memory_extraction_refs"],
        json!([expected_extraction_id])
    );
    let second_projection = wait_for_agent_run_projection_completed(
        app,
        &second_root_task_id,
        "test-session-001",
        active_workspace_id.as_str(),
    )
    .await;
    assert_completed_agent_run_projection(&second_projection);
}

#[tokio::test]
async fn router_regular_session_turn_uses_daemon_session_turn_dispatcher() {
    let state_root = temp_state_root("router-regular-session-turn");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    seed_registered_test_workspace(&config, None);
    let runtime = DaemonRuntime::restore(&config).expect("runtime restore should load state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let active_workspace_id = state
        .workspace_registry
        .active_workspace_id()
        .expect("bootstrap workspace should exist");
    let session_id = magi_core::SessionId::new("test-session-chat");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "runtime chat session".to_string(),
            Some(active_workspace_id.to_string()),
        )
        .expect("runtime session should be creatable");

    let (status, body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": session_id.to_string(),
            "text": "这是一条普通对话",
            "skillName": null,
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body:?}");
    assert_eq!(body["route"], "execute");
    // 普通消息现在统一进入 task 主线，由 daemon dispatcher 建立 root/action task。
    assert!(body["rootTaskId"].is_string());
    assert!(body["actionTaskId"].is_string());

    let deadline = Instant::now() + BACKGROUND_TEST_TIMEOUT;
    loop {
        let read_model = get_json(app.clone(), "/runtime/read-model").await;
        let session = read_model["details"]["sessions"]
            .as_array()
            .and_then(|sessions| {
                sessions
                    .iter()
                    .find(|entry| entry["session_id"] == session_id.to_string())
            })
            .expect("session should appear in read model");
        let status = session["current_turn"]["status"].as_str().unwrap_or("");
        if status == "completed" {
            assert!(
                session["turn_items"]
                    .as_array()
                    .expect("turn items should be an array")
                    .iter()
                    .any(|item| item["kind"] == "assistant_final"),
                "regular chat turn should produce assistant_final item: {session:?}"
            );
            break;
        }
        assert_ne!(
            status, "failed",
            "regular chat turn should not fail: {session:?}"
        );
        if Instant::now() >= deadline {
            panic!("regular chat turn did not complete before timeout: {session:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn router_recovery_resume_writeback_is_consumed_on_followup_dispatch() {
    let state_root = temp_state_root("router-recovery-resume");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    seed_registered_test_workspace(&config, None);
    let runtime = DaemonRuntime::restore(&config).expect("runtime restore should load state");
    let (app, state) = runtime.router_with_state_for_tests("daemon-test".to_string());
    let active_workspace_id = state
        .workspace_registry
        .active_workspace_id()
        .expect("bootstrap workspace should exist");
    if state
        .session_store
        .session(&magi_core::SessionId::new("test-session-001"))
        .is_none()
    {
        state
            .session_store
            .create_session_for_workspace(
                magi_core::SessionId::new("test-session-001"),
                "runtime recovery session".to_string(),
                Some(active_workspace_id.to_string()),
            )
            .expect("runtime recovery session should be creatable");
    }

    let (status, seed_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": "test-session-001",
            "text": "修复 recovery route 初始状态问题，完成后运行测试并汇总验证结果",
            "images": [],
            "workspaceId": active_workspace_id.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected seed body: {seed_body:?}"
    );
    // session 一生一 mission：后续 followup dispatch 复用 seed 的 mission_id
    let seed_accepted_at = seed_body["acceptedAt"]
        .as_u64()
        .expect("seed accepted_at should serialize as integer");

    let session_id = magi_core::SessionId::new("test-session-001");
    let ownership = runtime
        .session_store
        .execution_ownership(&session_id)
        .expect("seed session action should bind execution ownership");
    let workspace_id = ownership
        .workspace_id
        .clone()
        .expect("seed execution ownership should include workspace");
    let recovery_task_id = ownership
        .task_id
        .clone()
        .expect("seed execution ownership should include task");
    state
        .task_store()
        .expect("task store should be configured")
        .revoke_lease_and_set_task_terminal(
            &recovery_task_id,
            &recovery_task_id,
            None,
            TaskStatus::Failed,
            Vec::new(),
        )
        .expect("seed task should become recoverable")
        .then_some(())
        .expect("seed task should become recoverable");
    state
        .turn_event_sink()
        .cancel_turn(&session_id)
        .expect("seed current turn should cancel for recovery");
    let snapshot = runtime.workspace_store.append_execution_snapshot(
        workspace_id.clone(),
        ownership.clone(),
        "snapshot-daemon-recovery-route",
        "Daemon recovery snapshot",
    );
    let recovery = runtime.workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        ownership,
        snapshot.snapshot_id,
        "recovery-daemon-route",
        Some("resume daemon route followup".to_string()),
    );
    runtime
        .workspace_store
        .mark_recovery_ready(&recovery.recovery_id)
        .expect("recovery should become ready");
    runtime
        .session_store
        .set_recovery_id(&session_id, None, Some(recovery.recovery_id.clone()))
        .expect("recovery ref should attach to session");

    let (status, recovery_body) = post_json(
        app.clone(),
        "/api/session/continue",
        json!({
            "sessionId": session_id.to_string(),
            "workspaceId": workspace_id.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected recovery body: {recovery_body:?}"
    );
    assert_eq!(recovery_body["sessionId"], session_id.to_string());
    assert_eq!(recovery_body["status"], "continued");

    let (status, followup_body) = post_json(
        app.clone(),
        "/api/session/turn",
        json!({
            "scope": "workspace",
            "sessionId": "test-session-001",
            "text": "修复 recovery memory 消费问题，完成后运行测试并汇总验证结果",
            "images": [],
            "workspaceId": workspace_id.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected followup body: {followup_body:?}"
    );

    let _followup_accepted_at = followup_body["acceptedAt"]
        .as_u64()
        .expect("accepted_at should serialize as integer");
    let followup_mission_id = format!("mission-session-action-{seed_accepted_at}");
    let expected_extraction_id = "extract-session-continue-recovery-daemon-route";
    let followup_execution_group = wait_for_execution_group(app, &followup_mission_id, |entry| {
        entry["context_memory_extraction_refs"]
            .as_array()
            .is_some_and(|refs| refs.iter().any(|value| value == expected_extraction_id))
    })
    .await;
    let extraction_refs = followup_execution_group["context_memory_extraction_refs"]
        .as_array()
        .expect("refs should be array");
    assert!(
        extraction_refs
            .iter()
            .any(|value| value == expected_extraction_id),
        "followup execution group should consume recovery extraction, got {extraction_refs:?}"
    );
}

#[tokio::test]
async fn daemon_business_model_environment_fallback_exposes_the_actual_http_cache_ir() {
    let _env = OpenAiCompatEnvGuard::unset();
    let state_root = temp_state_root("business-model-cache-ir");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config).unwrap();
    let (_app, state) = runtime.router_with_bridge_env_for_tests(
        "daemon-test".into(),
        &[
            ("MAGI_OPENAI_COMPAT_BASE_URL", "https://relay.example/v1"),
            ("MAGI_OPENAI_COMPAT_MODEL", "environment-model"),
        ],
    );
    let client = state.model_bridge_client().unwrap();
    let identity = client.cache_identity().expect("actual HTTP identity");
    assert_eq!(identity.model, "environment-model");
    assert_eq!(identity.protocol, "openai_chat");
    let ir = client.cache_prompt_ir(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: Some("not-supported-by-unknown-chat".into()),
        provider: "fallback".into(),
        prompt: "inspect".into(),
        messages: None,
        tools: Some(vec![magi_bridge_client::ChatToolDefinition {
            kind: "function".into(),
            origin: magi_bridge_client::ChatToolOrigin::ExternalMcp,
            function: magi_bridge_client::ChatToolFunctionDefinition {
                name: "mcp__repo__inspect".into(),
                description: "inspect".into(),
                parameters: json!({"type":"object"}),
            },
        }]),
        tool_choice: None,
    });
    assert!(ir.prompt_cache_key.is_none());
    assert_eq!(ir.tools.unwrap()[0].name, "magi_mcp_mcp__repo__inspect");
}

#[tokio::test]
async fn daemon_business_model_without_real_config_reports_configuration_error() {
    let _env = OpenAiCompatEnvGuard::unset();
    let state_root = temp_state_root("business-model-unavailable");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (_app, state) = runtime.router_with_bridge_env_for_tests("daemon-test".to_string(), &[]);
    let client = state
        .model_bridge_client()
        .expect("daemon should always wire a business model client");

    let err = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "magi-business-model".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect_err("unconfigured business model should return a configuration error");

    let BridgeClientError::CallFailed { layer, message, .. } = err else {
        panic!("unexpected business model error shape: {err:?}");
    };
    assert_eq!(layer, magi_bridge_client::BridgeErrorLayer::RemoteBusiness);
    assert!(
        message.contains("业务模型桥未配置"),
        "error should guide model configuration, got: {message}"
    );
}

#[tokio::test]
async fn daemon_router_settings_bootstrap_exposes_env_seeded_orchestrator_config() {
    let state_root = temp_state_root("router-settings-bootstrap-env-model");
    let config = DaemonConfig::new("127.0.0.1", 0, "daemon-test", state_root);
    let runtime = DaemonRuntime::restore_with_test_fixture(&config)
        .expect("runtime restore should load explicit test fixture");
    let (app, state) = runtime.router_with_bridge_env_for_tests(
        "daemon-test".to_string(),
        &[
            ("MAGI_OPENAI_COMPAT_BASE_URL", "http://127.0.0.1:8317/v1"),
            ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
            ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
        ],
    );

    let bootstrap = get_json(
        app,
        "/api/settings/bootstrap?scope=personal&bootstrapScope=core",
    )
    .await;

    let source = &bootstrap["orchestratorConfig"]["sources"][0];
    assert_eq!(source["id"], json!("environment"));
    assert_eq!(source["baseUrl"], json!("http://127.0.0.1:8317/v1"));
    assert_eq!(source["apiKey"], json!("test-key"));
    assert_eq!(source["models"], json!([{"id": "gpt-test"}]));
    assert_eq!(
        bootstrap["orchestratorSessionDefaults"],
        json!({"sourceId": "custom/environment", "model": "gpt-test", "reasoningEffort": null})
    );
    assert!(
        state
            .settings_store
            .get_section("orchestrator")
            .get("model")
            .is_none(),
        "环境变量种子写入数据源目录，具体模型选择归会话默认值"
    );
}
