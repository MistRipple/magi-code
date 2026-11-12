#[test]
fn dynamic_builtin_tools_share_the_namespace_shape_of_the_static_catalog() {
    assert_eq!(
        super::dynamic_public_name("git_branch_create"),
        "magi.git.branch_create"
    );
    assert_eq!(super::dynamic_public_name("web_search"), "magi.web.search");
    assert_eq!(
        super::dynamic_public_name("knowledge_graph_query"),
        "magi.knowledge.graph_query"
    );
}

use super::*;
use axum::body::Body;
use axum::http::{Request, header};
use magi_core::AbsolutePath;
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_mcp_server::http::{HttpConfig, HttpState, router};
use magi_mcp_server::{ApprovalMode, AttributionMode, IssueTokenRequest};
use magi_session_store::{SessionKind, SessionStore};
use magi_tool_runtime::ToolRegistry;
use magi_workspace::WorkspaceStore;
use tower::ServiceExt;

struct Fixture {
    state: ApiState,
    router: axum::Router,
    transport_session: tokio::sync::OnceCell<String>,
    tokens: Arc<TokenStore>,
    secret: String,
    token_id: String,
    root: PathBuf,
    mcp_calls: Arc<std::sync::atomic::AtomicUsize>,
    _dir: tempfile::TempDir,
}

fn fixture(approval_timeout: Duration) -> Fixture {
    fixture_with_approval(ApprovalMode::Ask, approval_timeout)
}

fn fixture_with_approval(approval_mode: ApprovalMode, approval_timeout: Duration) -> Fixture {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let governance = Arc::new(GovernanceService::default());
    let base = ApiState::new(
        "magi-test",
        event_bus.clone(),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        governance.clone(),
    );
    let state_dir = tempfile::tempdir().unwrap();
    let git_executor = crate::git_tool_runtime::build_git_tool_executor(
        crate::git_tool_runtime::GitToolRuntimeDependencies {
            task_store: None,
            git_service: base.git_service.clone(),
            session_code_contexts: base.session_code_contexts.clone(),
            workspace_git_coordinator: base.workspace_git_coordinator.clone(),
            event_bus: event_bus.clone(),
            knowledge_store: base.knowledge_store.clone(),
            snapshot_manager: base.snapshot_manager.clone(),
            runtime_persistence: Arc::new(crate::RuntimeStatePersistence::new(
                state_dir.path().join("state"),
                state_dir.path().join("state/workspaces.json"),
                state_dir.path().join("state/knowledge.json"),
            )),
            managed_worktree_root: state_dir.path().join("worktrees"),
        },
    );
    let mcp_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executor_calls = mcp_calls.clone();
    let mut registry = ToolRegistry::new(governance.clone(), event_bus.clone())
        .with_git_tool_executor(git_executor)
        .with_external_tool_catalog_provider(Arc::new(|| {
            use magi_tool_runtime::{
                ExternalMcpServerCatalogEntry, ExternalMcpToolCatalogEntry,
                ExternalToolCatalogSnapshot,
            };
            let tool = |name: &str, read_only: bool| ExternalMcpToolCatalogEntry {
                server_id: "docs".to_string(),
                server_name: "Docs".to_string(),
                model_tool_name: format!("mcp__docs__{name}"),
                tool_name: name.to_string(),
                description: format!("{name} 下游工具"),
                read_only,
                input_schema: serde_json::json!({"type": "object"}),
            };
            ExternalToolCatalogSnapshot {
                mcp_servers: vec![ExternalMcpServerCatalogEntry {
                    server_id: "docs".to_string(),
                    name: "Docs".to_string(),
                    enabled: true,
                    connected: true,
                    health: "ok".to_string(),
                    tool_count: Some(2),
                    error: None,
                }],
                mcp_tools: vec![tool("search", true), tool("publish", false)],
                ..Default::default()
            }
        }))
        .with_external_mcp_tool_executor(Arc::new(move |_server, tool, _args| {
            executor_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                serde_json::json!({"status": "succeeded", "ran": tool}).to_string(),
                magi_core::ExecutionResultStatus::Succeeded,
            )
        }));
    registry.register_default_builtins();
    std::mem::forget(state_dir);
    let state = base.with_tool_registry(registry);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    state
        .workspace_registry
        .register(
            WorkspaceId::new("ws-mcp"),
            AbsolutePath::new(root.display().to_string()),
        )
        .unwrap();
    let tokens = Arc::new(TokenStore::new());
    let issued = tokens
        .issue(
            IssueTokenRequest {
                project_directories: Some(
                    state
                        .workspace_registry
                        .capture_directory_binding(&WorkspaceId::new("ws-mcp"), None, None)
                        .unwrap(),
                ),
                client_name: "cursor".to_string(),
                workspace_id: "ws-mcp".to_string(),
                approval_mode,
                attribution: AttributionMode::External,
                ttl_ms: None,
                network: false,
            },
            UtcMillis::now().0,
        )
        .unwrap();
    let backend =
        ApiToolBackend::new(state.clone(), tokens.clone()).with_approval_timeout(approval_timeout);
    let server = Arc::new(
        magi_mcp_server::McpServer::new(
            "magi",
            "test",
            Arc::new(backend),
            Arc::new(RegistryWorkspaces {
                state: state.clone(),
            }),
        )
        .with_audit(Arc::new(EventBusAudit {
            state: state.clone(),
        })),
    );
    Fixture {
        state,
        transport_session: tokio::sync::OnceCell::new(),
        router: router(HttpState::new(
            server,
            tokens.clone(),
            HttpConfig::default(),
        )),
        tokens,
        secret: issued.secret,
        token_id: issued.record.token_id,
        root,
        mcp_calls,
        _dir: dir,
    }
}

async fn rpc(fixture: &Fixture, method: &str, params: Value) -> Value {
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    rpc_message(fixture, body).await
}

async fn http_message(
    fixture: &Fixture,
    body: Value,
    session: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "127.0.0.1")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {}", fixture.secret));
    if let Some(id) = session {
        request = request.header("mcp-session-id", id);
    }
    fixture
        .router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn rpc_message(fixture: &Fixture, body: Value) -> Value {
    let session = fixture
        .transport_session
        .get_or_init(|| async {
            let response = http_message(
                fixture,
                serde_json::json!({"jsonrpc":"2.0", "id":0, "method":"initialize"}),
                None,
            )
            .await;
            response.headers()["mcp-session-id"]
                .to_str()
                .unwrap()
                .to_string()
        })
        .await;
    let response = http_message(fixture, body, Some(session)).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn conversation_fixture() -> Fixture {
    let mut f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
    let issued = f
        .tokens
        .issue(
            IssueTokenRequest {
                client_name: "Web Agent".into(),
                workspace_id: String::new(),
                project_directories: None,
                attribution: AttributionMode::Conversation,
                approval_mode: ApprovalMode::Always,
                ttl_ms: None,
                network: false,
            },
            UtcMillis::now().0,
        )
        .unwrap();
    f.secret = issued.secret;
    f.token_id = issued.record.token_id;
    f
}

async fn start_chat(f: &Fixture, conversation_id: &str) -> Value {
    let result = rpc(f, "tools/call", call("magi.session.start", serde_json::json!({"conversation_id":conversation_id,"title":format!("Chat {conversation_id}")}))).await;
    assert!(!is_error(&result), "{result}");
    serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn approve_chat(f: &Fixture, id: &str, workspace: &str) {
    let binding = f
        .state
        .workspace_registry
        .capture_directory_binding(&WorkspaceId::new(workspace), None, None)
        .unwrap();
    f.state
        .mcp_service
        .conversations
        .decide(&f.state, id, Some(binding))
        .unwrap();
}

#[tokio::test]
async fn conversations_select_once_reconnect_and_share_a_directory_without_sharing_history() {
    let f = conversation_fixture();
    let a = start_chat(&f, "chat-a").await;
    assert_eq!(a["status"], "awaiting_directory");
    let a_id = a["magi_session_id"].as_str().unwrap();
    assert_eq!(start_chat(&f, "chat-a").await["magi_session_id"], a_id);
    let denied = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"magi_session_id":a_id,"path":"a.txt","content":"too early"}),
        ),
    )
    .await;
    assert!(is_error(&denied));
    assert!(!f.root.join("a.txt").exists());
    approve_chat(&f, a_id, "ws-mcp");
    let b = start_chat(&f, "chat-b").await;
    let b_id = b["magi_session_id"].as_str().unwrap();
    assert_ne!(a_id, b_id);
    approve_chat(&f, b_id, "ws-mcp");
    for (id, file, content) in [(a_id, "a.txt", "from-a"), (b_id, "b.txt", "from-b")] {
        for _ in 0..2 {
            let result = rpc(
                &f,
                "tools/call",
                call(
                    "magi.fs.write",
                    serde_json::json!({"magi_session_id":id,"path":file,"content":content}),
                ),
            )
            .await;
            assert!(!is_error(&result), "{result}");
        }
    }
    assert_eq!(
        std::fs::read_to_string(f.root.join("a.txt")).unwrap(),
        "from-a"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("b.txt")).unwrap(),
        "from-b"
    );
    for (id, own, other) in [(a_id, "a.txt", "b.txt"), (b_id, "b.txt", "a.txt")] {
        let history = serde_json::to_string(
            &f.state
                .session_store
                .canonical_turns_for_session(&SessionId::new(id)),
        )
        .unwrap();
        assert!(history.contains(own), "{history}");
        assert!(!history.contains(other), "{history}");
        assert_eq!(
            f.state
                .session_store
                .session(&SessionId::new(id))
                .unwrap()
                .workspace_id
                .as_deref(),
            Some("ws-mcp")
        );
    }
    // Re-initialize a transport: the explicit conversation ID keeps the same binding.
    let initialized = rpc(&f, "initialize", serde_json::json!({})).await;
    assert!(initialized.get("error").is_none());
    assert_eq!(start_chat(&f, "chat-a").await["status"], "ready");
    assert!(
        f.state
            .mcp_service
            .conversations
            .records()
            .iter()
            .all(|r| r.directories.is_some())
    );
    assert!(
        f.state
            .session_store
            .session(&external_session_id(&f.token_id))
            .is_none()
    );
}

#[tokio::test]
async fn conversations_with_different_directories_do_not_follow_the_active_project() {
    let f = conversation_fixture();
    let other = tempfile::tempdir().unwrap();
    f.state
        .workspace_registry
        .register(
            WorkspaceId::new("ws-other"),
            AbsolutePath::new(other.path().canonicalize().unwrap().display().to_string()),
        )
        .unwrap();
    let a = start_chat(&f, "first-project").await;
    let b = start_chat(&f, "second-project").await;
    let a_id = a["magi_session_id"].as_str().unwrap();
    let b_id = b["magi_session_id"].as_str().unwrap();
    approve_chat(&f, a_id, "ws-mcp");
    approve_chat(&f, b_id, "ws-other");
    for (id, content) in [(b_id, "B"), (a_id, "A"), (b_id, "B"), (a_id, "A")] {
        let result = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"magi_session_id":id,"path":"same.txt","content":content}),
            ),
        )
        .await;
        assert!(!is_error(&result), "{result}");
    }
    assert_eq!(
        std::fs::read_to_string(f.root.join("same.txt")).unwrap(),
        "A"
    );
    assert_eq!(
        std::fs::read_to_string(other.path().join("same.txt")).unwrap(),
        "B"
    );
    let outside = rpc(&f,"tools/call",call("magi.fs.write",serde_json::json!({"magi_session_id":a_id,"path":other.path().join("escape.txt"),"content":"no"}))).await;
    assert!(is_error(&outside));
    assert!(!other.path().join("escape.txt").exists());
}

#[tokio::test]
async fn conversation_identity_is_required_and_scoped_to_the_client() {
    let f = conversation_fixture();
    let missing = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path":"escape.txt","content":"no"}),
        ),
    )
    .await;
    assert!(is_error(&missing));
    let chat = start_chat(&f, "same-id").await;
    let id = chat["magi_session_id"].as_str().unwrap();
    approve_chat(&f, id, "ws-mcp");
    let another = f
        .tokens
        .issue(
            IssueTokenRequest {
                client_name: "Another Web Agent".into(),
                workspace_id: String::new(),
                project_directories: None,
                attribution: AttributionMode::Conversation,
                approval_mode: ApprovalMode::Always,
                ttl_ms: None,
                network: false,
            },
            UtcMillis::now().0,
        )
        .unwrap();
    let principal = magi_mcp_server::ClientPrincipal::from(&another.record);
    assert!(
        f.state
            .mcp_service
            .conversations
            .bind(&f.state, &principal, id)
            .is_err()
    );
    let other = f
        .state
        .mcp_service
        .conversations
        .start(&principal, "same-id", "")
        .unwrap();
    assert_ne!(other.session_id, id);
    assert!(other.directories.is_none());
    assert!(!f.root.join("escape.txt").exists());
    f.tokens.revoke(&f.token_id, UtcMillis::now().0);
    let response = http_message(&f,serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":call("magi.fs.write",serde_json::json!({"magi_session_id":id,"path":"escape.txt","content":"no"}))}),None).await;
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn conversations_sharing_a_client_and_directory_keep_tool_approvals_separate() {
    let f = Arc::new(conversation_fixture());
    f.tokens
        .update(
            &f.token_id,
            magi_mcp_server::TokenPatch {
                approval_mode: Some(ApprovalMode::Ask),
                ..Default::default()
            },
            UtcMillis::now().0,
        )
        .unwrap();
    let a = start_chat(&f, "approval-A").await;
    let b = start_chat(&f, "approval-B").await;
    let a_id = a["magi_session_id"].as_str().unwrap().to_string();
    let b_id = b["magi_session_id"].as_str().unwrap().to_string();
    approve_chat(&f, &a_id, "ws-mcp");
    approve_chat(&f, &b_id, "ws-mcp");
    let run = |session_id: String, file: &'static str| {
        let f = f.clone();
        tokio::spawn(async move {
            let response = http_message(&f,serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":call("magi.fs.write",serde_json::json!({"magi_session_id":session_id,"path":file,"content":"approved"}))}),None).await;
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        })
    };
    let a_call = run(a_id.clone(), "allowed-a.txt");
    let b_call = run(b_id.clone(), "denied-b.txt");
    let registry = f.state.conversation_registry.tool_approvals();
    let a_session = SessionId::new(a_id);
    let b_session = SessionId::new(b_id);
    tokio::time::timeout(Duration::from_secs(3), async {
        while registry.pending_for_session(&a_session).is_empty()
            || registry.pending_for_session(&b_session).is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let a_pending = registry.pending_for_session(&a_session).remove(0);
    registry
        .resolve(
            &a_session,
            &a_pending.approval_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        )
        .unwrap();
    assert_eq!(registry.pending_for_session(&b_session).len(), 1);
    let b_pending = registry.pending_for_session(&b_session).remove(0);
    assert_ne!(a_pending.approval_id, b_pending.approval_id);
    registry
        .resolve(
            &b_session,
            &b_pending.approval_id,
            magi_conversation_runtime::ToolApprovalDecision::Deny,
        )
        .unwrap();
    assert!(!is_error(&a_call.await.unwrap()));
    assert!(is_error(&b_call.await.unwrap()));
    assert!(f.root.join("allowed-a.txt").exists());
    assert!(!f.root.join("denied-b.txt").exists());
}

#[tokio::test]
async fn conversation_directory_grants_are_immutable_and_restore_after_restart() {
    let f = conversation_fixture();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mcp-conversations.json");
    let store = crate::mcp_sessions::McpConversations::load(path.clone());
    let record = f
        .tokens
        .records()
        .into_iter()
        .find(|record| record.token_id == f.token_id)
        .unwrap();
    let principal = magi_mcp_server::ClientPrincipal::from(&record);
    let chat = store
        .start(&principal, "persistent-chat", "Persistent")
        .unwrap();
    let binding = f
        .state
        .workspace_registry
        .capture_directory_binding(&WorkspaceId::new("ws-mcp"), None, None)
        .unwrap();
    store
        .decide(&f.state, &chat.session_id, Some(binding.clone()))
        .unwrap();
    assert!(store.decide(&f.state, &chat.session_id, None).is_err());
    let pending = store.start(&principal, "not-yet-selected", "").unwrap();
    let restored = crate::mcp_sessions::McpConversations::load(path);
    assert_eq!(
        restored
            .start(&principal, "persistent-chat", "")
            .unwrap()
            .session_id,
        chat.session_id
    );
    assert_eq!(
        restored
            .bind(&f.state, &principal, &chat.session_id)
            .unwrap()
            .project_directories,
        Some(binding)
    );
    assert!(
        !restored
            .records()
            .iter()
            .any(|record| record.session_id == pending.session_id)
    );
}

fn call(name: &str, arguments: Value) -> Value {
    serde_json::json!({ "name": name, "arguments": arguments })
}

fn is_error(response: &Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}

fn resolve_pending(
    state: &ApiState,
    token_id: &str,
    decision: magi_conversation_runtime::ToolApprovalDecision,
) -> std::thread::JoinHandle<()> {
    let state = state.clone();
    let session_id = external_session_id(token_id);
    std::thread::spawn(move || {
        let approvals = state.conversation_registry.tool_approvals().clone();
        for _ in 0..200 {
            if let Some(pending) = approvals.pending_for_session(&session_id).first() {
                approvals
                    .resolve(&session_id, &pending.approval_id, decision)
                    .unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("待审批没有出现");
    })
}

#[tokio::test]
async fn client_cancellation_and_disconnect_settle_approval_without_writing() {
    for disconnect in [false, true] {
        let f = Arc::new(fixture(Duration::from_secs(10)));
        let running = {
            let f = f.clone();
            tokio::spawn(async move {
                rpc(
                    &f,
                    "tools/call",
                    call(
                        "magi.fs.write",
                        serde_json::json!({"path":"cancelled.txt", "content":"must not write"}),
                    ),
                )
                .await
            })
        };
        let session = external_session_id(&f.token_id);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !f
                    .state
                    .conversation_registry
                    .tool_approvals()
                    .pending_for_session(&session)
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if disconnect {
            running.abort();
            assert!(running.await.unwrap_err().is_cancelled());
        } else {
            assert!(rpc_message(&f, serde_json::json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":1}})).await.is_null());
            let result = tokio::time::timeout(Duration::from_secs(5), running)
                .await
                .unwrap()
                .unwrap();
            assert!(result.to_string().contains("cancelled"), "{result}");
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if f.state.mcp_service.external_turns.in_flight(&session) == 0
                    && f.state
                        .event_bus
                        .snapshot()
                        .recent_events
                        .iter()
                        .any(|event| event.event_type == "mcp.tool.call")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&session)
                .is_empty()
        );
        assert!(!f.root.join("cancelled.txt").exists());
        assert!(
            f.state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "mcp.activity"
                        && event.session_id.as_ref() == Some(&session)
                }),
            "调用结算通知必须能通过 Desktop 的会话事件过滤"
        );
        assert!(
                external_tool_items(&f)
                    .iter()
                    .any(|(_, _, result, status)| result.contains("cancelled")
                        && status == "Cancelled")
            );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_stops_only_the_requested_shell_call_and_records_actual_status() {
    let f = Arc::new(fixture_with_approval(
        ApprovalMode::Always,
        Duration::from_secs(5),
    ));
    let running = {
        let f = f.clone();
        tokio::spawn(async move {
            rpc(&f, "tools/call", call("magi.shell.exec", serde_json::json!({"command":"printf started > started.txt; sleep 30; printf finished > finished.txt"}))).await
        })
    };
    tokio::time::timeout(Duration::from_secs(8), async {
        while !f.root.join("started.txt").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let read = rpc_message(&f, serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":call("magi.fs.read", serde_json::json!({"path":"started.txt"}))})).await;
    assert!(!is_error(&read), "{read}");
    rpc_message(&f, serde_json::json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":1}})).await;
    let result = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert!(result.to_string().contains("cancelled"), "{result}");
    assert!(!f.root.join("finished.txt").exists());
    assert_eq!(
        f.state
            .mcp_service
            .external_turns
            .in_flight(&external_session_id(&f.token_id)),
        0
    );
    let items = external_tool_items(&f);
    assert!(
        items
            .iter()
            .any(|(name, _, _, status)| name == "shell_exec" && status == "Cancelled"),
        "{items:?}"
    );
    assert!(
        items
            .iter()
            .any(|(name, _, _, status)| name == "file_read" && status == "Completed"),
        "{items:?}"
    );
}

#[tokio::test]
async fn tokens_list_the_full_tool_catalog_including_shell() {
    let f = fixture(Duration::from_secs(5));
    let response = rpc(&f, "tools/list", serde_json::json!({})).await;
    let names = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"magi.fs.read".to_string()));
    assert!(names.contains(&"magi.fs.write".to_string()));
    assert!(
        names.contains(&"magi.shell.exec".to_string()),
        "令牌不按工具档过滤，命令执行也在目录里（是否询问由授权方式决定）"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn invalidating_a_token_stops_its_running_shell_and_records_cancellation() {
    for invalidation in ["revoke", "expire", "rotate"] {
        let f = Arc::new(fixture_with_approval(
            ApprovalMode::Always,
            Duration::from_secs(5),
        ));
        let running = {
            let f = f.clone();
            tokio::spawn(async move {
                rpc(&f, "tools/call", call("magi.shell.exec", serde_json::json!({"command":"printf started > started.txt; sleep 2; printf finished > finished.txt"}))).await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.root.join("started.txt").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        match invalidation {
            "revoke" => assert!(f.tokens.revoke(&f.token_id, UtcMillis::now().0)),
            "expire" => {
                f.tokens
                    .update(
                        &f.token_id,
                        magi_mcp_server::TokenPatch {
                            expires_at_ms: Some(Some(UtcMillis::now().0)),
                            ..Default::default()
                        },
                        UtcMillis::now().0,
                    )
                    .unwrap();
            }
            "rotate" => {
                f.tokens.rotate(&f.token_id).unwrap();
            }
            _ => unreachable!(),
        }
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap();
        assert!(
            is_error(&result) && result.to_string().contains("cancelled"),
            "{result}"
        );
        assert!(!f.root.join("finished.txt").exists());
        assert_eq!(
            f.state
                .mcp_service
                .external_turns
                .in_flight(&external_session_id(&f.token_id)),
            0
        );
        assert!(
            external_tool_items(&f)
                .iter()
                .any(|(name, _, _, status)| name == "shell_exec" && status == "Cancelled")
        );
    }
}

#[tokio::test]
async fn open_mode_reads_without_prompt_and_paths_outside_the_workspace_are_denied() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
    std::fs::write(f.root.join("hello.txt"), "hi there").unwrap();

    let ok = rpc(
        &f,
        "tools/call",
        call("magi.fs.read", serde_json::json!({"path": "hello.txt"})),
    )
    .await;
    assert!(!is_error(&ok), "{ok}");
    assert!(ok.to_string().contains("hi there"));

    for path in ["../escape.txt", "/etc/passwd", ".magi/state.json"] {
        let denied = rpc(
            &f,
            "tools/call",
            call("magi.fs.read", serde_json::json!({"path": path})),
        )
        .await;
        assert!(is_error(&denied), "{path}: {denied}");
    }
}

/// 外部会话在 canonical 里的工具项：(工具名, 参数文本, 结果文本, 状态)。
fn external_tool_items(f: &Fixture) -> Vec<(String, String, String, String)> {
    f.state
        .session_store
        .canonical_turns_for_session(&external_session_id(&f.token_id))
        .into_iter()
        .flat_map(|turn| turn.items)
        .filter_map(|item| {
            let tool = item.tool?;
            Some((
                tool.name,
                tool.arguments
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                tool.result
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                format!("{:?}", item.status),
            ))
        })
        .collect()
}

#[tokio::test]
async fn external_calls_land_in_the_external_session_timeline_with_results_and_denials() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
    std::fs::write(f.root.join("hello.txt"), "hi there").unwrap();
    let ok = rpc(
        &f,
        "tools/call",
        call("magi.fs.read", serde_json::json!({"path": "hello.txt"})),
    )
    .await;
    assert!(!is_error(&ok), "{ok}");
    let denied = rpc(
        &f,
        "tools/call",
        call("magi.fs.read", serde_json::json!({"path": "../escape.txt"})),
    )
    .await;
    assert!(is_error(&denied), "{denied}");

    let session_id = external_session_id(&f.token_id);
    let turns = f
        .state
        .session_store
        .canonical_turns_for_session(&session_id);
    assert_eq!(turns.len(), 1, "连续的外部调用在同一个活动段 Turn 里");
    let items = external_tool_items(&f);
    let read = items
        .iter()
        .find(|item| item.2.contains("hi there"))
        .expect("成功的读取带着读到的内容");
    assert!(read.1.contains("hello.txt"));
    let refused = items
        .iter()
        .find(|item| item.1.contains("paths"))
        .expect("被拒绝的越界读取也是一条工具项");
    assert!(!refused.2.is_empty(), "结果文本是拒绝原因");
    assert_eq!(f.state.mcp_service.external_turns.in_flight(&session_id), 0);
}

#[tokio::test]
async fn turning_off_content_saving_keeps_actions_but_not_results() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
    f.state
        .mcp_service
        .set_retention(crate::mcp_runtime::ExternalRetention {
            save_content: false,
            retention_days: 30,
        })
        .await
        .unwrap();
    std::fs::write(f.root.join("secret.txt"), "top secret body").unwrap();
    let ok = rpc(
        &f,
        "tools/call",
        call("magi.fs.read", serde_json::json!({"path": "secret.txt"})),
    )
    .await;
    assert!(!is_error(&ok), "{ok}");
    let items = external_tool_items(&f);
    assert!(
        items.iter().any(|item| item.1.contains("secret.txt")),
        "动作与路径保留"
    );
    assert!(
        items.iter().all(|item| !item.2.contains("top secret body")),
        "关闭保存内容后读到的内容不落库: {items:?}"
    );
    assert!(items.iter().any(|item| item.2.contains("内容未保存")));
}

#[tokio::test]
async fn project_mcp_tokens_freeze_directory_grants_and_write_each_execution_copy() {
    let mut f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
    let second = tempfile::tempdir().unwrap();
    let second_root = second.path().canonicalize().unwrap();
    let project = crate::routes::test_project_fixtures::add_source(
        &f.state,
        "ws-mcp",
        "second",
        &second_root,
    );
    let first_id = project.primary_source_folder_id.clone();
    let denied = rpc(&f, "tools/call", call("magi.fs.write", serde_json::json!({"source_folder_id":"second","path":"same.txt","content":"must reject"}))).await;
    assert!(is_error(&denied), "{denied}");
    assert!(!second_root.join("same.txt").exists());
    f.state
        .workspace_registry
        .edit_project(
            &project.workspace_id,
            project.revision,
            magi_workspace::WorkspaceProjectEdit {
                name: Some("renamed project".into()),
                source_folders: project.source_folders,
                primary_source_folder_id: "second".into(),
            },
            |_| Ok(()),
        )
        .unwrap();
    let old_default = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path":"same.txt","content":"old default"}),
        ),
    )
    .await;
    assert!(!is_error(&old_default), "{old_default}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("same.txt")).unwrap(),
        "old default"
    );
    assert!(!second_root.join("same.txt").exists());

    let binding = f
        .state
        .workspace_registry
        .capture_directory_binding(&WorkspaceId::new("ws-mcp"), Some(&first_id), None)
        .unwrap();
    let issued = f
        .tokens
        .issue(
            IssueTokenRequest {
                project_directories: Some(binding),
                client_name: "multi root".into(),
                workspace_id: "ws-mcp".into(),
                approval_mode: ApprovalMode::Always,
                attribution: AttributionMode::External,
                ttl_ms: None,
                network: false,
            },
            UtcMillis::now().0,
        )
        .unwrap();
    f.secret = issued.secret;
    f.token_id = issued.record.token_id;
    f.transport_session = tokio::sync::OnceCell::new();
    std::fs::write(second_root.join("same.txt"), "second original").unwrap();
    let copies = tempfile::tempdir().unwrap();
    let copy_root = copies.path().canonicalize().unwrap();
    let execution_roots = [copy_root.join("first"), copy_root.join("second")];
    let session = ensure_external_session(&f.state, &f.token_id, "multi root", "ws-mcp").unwrap();
    for (source, execution) in [&f.root, &second_root].into_iter().zip(&execution_roots) {
        let outcome = magi_session_isolation::create_isolated_copy(source, execution).unwrap();
        f.state
            .session_isolations
            .insert(magi_session_isolation::SessionIsolation {
                session_id: session.to_string(),
                workspace_id: "ws-mcp".into(),
                source_root: source.clone(),
                root: execution.clone(),
                origin: magi_session_isolation::IsolationOrigin::Manual,
                strategy: outcome.strategy,
                linked_dirs: outcome.linked_dirs,
                git_available: outcome.git_available,
                created_at_ms: 1,
            });
    }
    for (index, id) in [first_id.as_str(), "second"].into_iter().enumerate() {
        let result = rpc(&f, "tools/call", call("magi.fs.write", serde_json::json!({"source_folder_id":id,"path":"same.txt","content":format!("copy {index}")}))).await;
        assert!(!is_error(&result), "{result}");
        assert_eq!(
            std::fs::read_to_string(execution_roots[index].join("same.txt")).unwrap(),
            format!("copy {index}")
        );
        let snapshot = f
            .state
            .snapshot_manager
            .get_session_for_workspace(session.as_str(), &execution_roots[index])
            .unwrap();
        let changes = snapshot.pending_changes().unwrap();
        let same = changes
            .iter()
            .find(|change| change.path == "same.txt")
            .unwrap();
        assert_eq!(same.source, magi_snapshot::SourceKind::Tool);
        assert!(same.tool_call_id.is_some());
    }
    let result = rpc(&f, "tools/call", call("magi.fs.copy", serde_json::json!({"source":execution_roots[0].join("same.txt"),"destination":execution_roots[1].join("copied.txt")}))).await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(
        std::fs::read_to_string(execution_roots[1].join("copied.txt")).unwrap(),
        "copy 0"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("same.txt")).unwrap(),
        "old default"
    );
    assert_eq!(
        std::fs::read_to_string(second_root.join("same.txt")).unwrap(),
        "second original"
    );
    let source_write = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path":second_root.join("same.txt"),"content":"must reject"}),
        ),
    )
    .await;
    assert!(is_error(&source_write), "{source_write}");

    f.tokens
        .update(
            &f.token_id,
            magi_mcp_server::TokenPatch {
                approval_mode: Some(ApprovalMode::Ask),
                ..Default::default()
            },
            UtcMillis::now().0,
        )
        .unwrap();
    let state = f.state.clone();
    let owner = session.clone();
    let keep_id = first_id.clone();
    let remover = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(approval) = state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&owner)
                .first()
            {
                crate::routes::test_project_fixtures::set_members(
                    &state,
                    "ws-mcp",
                    &[&keep_id],
                    &keep_id,
                );
                let _ = state.conversation_registry.tool_approvals().resolve(
                    &owner,
                    &approval.approval_id,
                    magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
                );
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let result = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.remove",
            serde_json::json!({"source_folder_id":"second","path":"same.txt"}),
        ),
    )
    .await;
    remover.join().unwrap();
    assert!(is_error(&result), "{result}");
    assert_eq!(
        std::fs::read_to_string(execution_roots[1].join("same.txt")).unwrap(),
        "copy 1"
    );
}

#[tokio::test]
async fn approved_write_lands_in_the_external_session_ledger() {
    let f = fixture(Duration::from_secs(10));
    let approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
    );
    let response = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "new.txt", "content": "from mcp"}),
        ),
    )
    .await;
    approver.join().unwrap();
    assert!(!is_error(&response), "{response}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("new.txt")).unwrap(),
        "from mcp"
    );

    let session = f
        .state
        .session_store
        .session(&external_session_id(&f.token_id))
        .expect("外部工具会话应已创建");
    assert_eq!(session.kind, SessionKind::ExternalTool);
    assert!(
        f.state.session_store.current_session().is_none(),
        "外部会话不得成为用户当前会话"
    );
    let snapshot = f
        .state
        .snapshot_session(&external_session_id(&f.token_id), &f.root)
        .expect("写入类调用应建立账本");
    snapshot.reconcile().unwrap();
    let pending = snapshot.pending_changes().unwrap();
    assert!(
        pending
            .iter()
            .any(|change| change.path.ends_with("new.txt")),
        "写入应出现在待处理变更里: {pending:?}"
    );
}

#[tokio::test]
async fn ask_mode_prompts_for_each_write_in_one_activity() {
    let f = fixture(Duration::from_millis(400));
    let approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
    );
    let first = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "activity-first.txt", "content": "first"}),
        ),
    )
    .await;
    approver.join().unwrap();
    assert!(!is_error(&first), "{first}");

    let second = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "activity-second.txt", "content": "second"}),
        ),
    )
    .await;
    assert!(
        is_error(&second),
        "同一活动段内的第二次写入仍需授权: {second}"
    );
    assert!(second.to_string().contains("tool_approval_timeout"));
    assert!(!f.root.join("activity-second.txt").exists());
    assert_eq!(
        f.state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&external_session_id(&f.token_id))
            .len(),
        0
    );
}

#[tokio::test]
async fn denied_or_timed_out_write_leaves_the_file_untouched() {
    let f = fixture(Duration::from_millis(400));
    let approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::Deny,
    );
    let denied = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "a.txt", "content": "x"}),
        ),
    )
    .await;
    approver.join().unwrap();
    assert!(is_error(&denied), "{denied}");
    assert!(!f.root.join("a.txt").exists());

    let timed_out = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "b.txt", "content": "x"}),
        ),
    )
    .await;
    assert!(is_error(&timed_out), "{timed_out}");
    assert!(timed_out.to_string().contains("tool_approval_timeout"));
    assert!(!f.root.join("b.txt").exists());
}

#[tokio::test]
async fn changes_list_shows_own_writes_and_revert_restores_after_approval() {
    let f = fixture(Duration::from_secs(10));
    std::fs::write(f.root.join("keep.txt"), "original").unwrap();

    // 先让 snapshot 建立基线：一次经批准的覆盖写入。
    let approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
    );
    let written = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "keep.txt", "content": "changed"}),
        ),
    )
    .await;
    approver.join().unwrap();
    assert!(!is_error(&written), "{written}");

    let list_approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
    );
    let listed = rpc(
        &f,
        "tools/call",
        call("magi.changes.list", serde_json::json!({})),
    )
    .await;
    list_approver.join().unwrap();
    assert!(!is_error(&listed), "{listed}");
    assert!(listed.to_string().contains("keep.txt"), "{listed}");

    // 回退是写入类：需要确认；批准后文件恢复。
    let approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
    );
    let reverted = rpc(
        &f,
        "tools/call",
        call(
            "magi.changes.revert",
            serde_json::json!({"paths": ["keep.txt"]}),
        ),
    )
    .await;
    approver.join().unwrap();
    assert!(!is_error(&reverted), "{reverted}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("keep.txt")).unwrap(),
        "original"
    );
}

#[tokio::test]
async fn revert_is_refused_when_someone_else_changed_the_file_after_this_client() {
    let f = fixture(Duration::from_secs(10));
    std::fs::write(f.root.join("shared.txt"), "original").unwrap();

    let approver = resolve_pending(
        &f.state,
        &f.token_id,
        magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
    );
    let written = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "shared.txt", "content": "client A was here"}),
        ),
    )
    .await;
    approver.join().unwrap();
    assert!(!is_error(&written), "{written}");

    // 另一个客户端（或用户）随后改了同一个文件。
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(f.root.join("shared.txt"), "client B overwrote this, longer").unwrap();

    // 在检查阶段就被拒绝，不需要审批：没有挂起的审批可供处理。
    let reverted = rpc(
        &f,
        "tools/call",
        call(
            "magi.changes.revert",
            serde_json::json!({"paths": ["shared.txt"]}),
        ),
    )
    .await;
    assert!(is_error(&reverted), "{reverted}");
    assert!(reverted.to_string().contains("拒绝回退"), "{reverted}");
    assert!(reverted.to_string().contains("shared.txt"), "{reverted}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("shared.txt")).unwrap(),
        "client B overwrote this, longer",
        "别人的修改不能被回退抹掉"
    );
}

#[tokio::test]
async fn revert_outside_the_workspace_and_approve_are_not_available() {
    let f = fixture(Duration::from_secs(5));
    let escaped = rpc(
        &f,
        "tools/call",
        call(
            "magi.changes.revert",
            serde_json::json!({"paths": ["../outside.txt"]}),
        ),
    )
    .await;
    assert!(is_error(&escaped), "{escaped}");
    let approve = rpc(
        &f,
        "tools/call",
        call("magi.changes.approve", serde_json::json!({"paths": ["a"]})),
    )
    .await;
    assert!(is_error(&approve));
}

#[tokio::test]
async fn open_mode_git_status_runs_without_approval() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
    let git = |args: &[&str]| {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(&f.root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Magi Test"]);
    git(&["config", "user.email", "magi@example.test"]);
    std::fs::write(f.root.join("README.md"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init"]);
    std::fs::write(f.root.join("dirty.txt"), "x").unwrap();

    let status = rpc(
        &f,
        "tools/call",
        call("magi.git.status", serde_json::json!({})),
    )
    .await;
    assert!(!is_error(&status), "{status}");
    assert!(
        status.to_string().contains(r#"\"branch\":\"main\""#),
        "{status}"
    );
    assert!(
        status.to_string().contains(r#"\"untracked\":1"#),
        "{status}"
    );
    // 兼容的 dirty 改动沿 GitService 的既有语义保留；MCP 不得被通用
    // `.git` 写保护误拒绝，也不能改变 GitService 的工作树保护语义。
    let dirty_branch = rpc(
        &f,
        "tools/call",
        call(
            "magi.git.branch_create",
            serde_json::json!({"branch":"mcp-test"}),
        ),
    )
    .await;
    assert!(!is_error(&dirty_branch), "{dirty_branch}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("dirty.txt")).unwrap(),
        "x"
    );
    std::fs::remove_file(f.root.join("dirty.txt")).unwrap();
    for (name, arguments) in [
        (
            "magi.git.branch_switch",
            serde_json::json!({"branch":"main"}),
        ),
        (
            "magi.git.branch_delete",
            serde_json::json!({"branch":"mcp-test"}),
        ),
    ] {
        let result = rpc(&f, "tools/call", call(name, arguments)).await;
        assert!(!is_error(&result), "{name}: {result}");
    }
    let raw = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path":".git/config","content":"must not replace"}),
        ),
    )
    .await;
    assert!(is_error(&raw), "原始元数据写入仍须拒绝: {raw}");
    let escaped = rpc(
        &f,
        "tools/call",
        call(
            "magi.git.branch_create",
            serde_json::json!({"branch":"escape","repositoryPath":"../outside"}),
        ),
    )
    .await;
    assert!(is_error(&escaped), "结构化 Git 不能越界: {escaped}");
    // 未配置远端仍须报错，完全访问不伪造 Git 操作成功。
    let push = rpc(
        &f,
        "tools/call",
        call("magi.git.push", serde_json::json!({})),
    )
    .await;
    assert!(is_error(&push));
}

// ── 威胁演练（WP15）：越权、逃逸、别名与泄露 ─────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn symlink_pointing_outside_the_workspace_cannot_be_read_or_written_through() {
    let f = fixture(Duration::from_secs(2));
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "TOP-SECRET").unwrap();
    std::os::unix::fs::symlink(outside.path(), f.root.join("link")).unwrap();

    let read = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.read",
            serde_json::json!({"path": "link/secret.txt"}),
        ),
    )
    .await;
    assert!(is_error(&read), "{read}");
    assert!(!read.to_string().contains("TOP-SECRET"));
    let write = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "link/new.txt", "content": "x"}),
        ),
    )
    .await;
    assert!(is_error(&write), "{write}");
    assert!(!outside.path().join("new.txt").exists());
}

#[tokio::test]
async fn patch_paths_git_dir_and_internal_or_cased_names_cannot_bypass_the_guards() {
    let f = fixture(Duration::from_secs(2));
    let patch = "*** Begin Patch\n*** Add File: ../escaped.txt\n+x\n*** End Patch\n";
    let escaped = rpc(
        &f,
        "tools/call",
        call("magi.fs.apply_patch", serde_json::json!({"patch": patch})),
    )
    .await;
    assert!(is_error(&escaped), "{escaped}");
    assert!(!f.root.parent().unwrap().join("escaped.txt").exists());

    std::fs::create_dir_all(f.root.join(".git")).unwrap();
    let git_write = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": ".git/config", "content": "x"}),
        ),
    )
    .await;
    assert!(is_error(&git_write), "{git_write}");

    for name in [
        "file_write",
        "FILE_WRITE",
        "MAGI.FS.WRITE",
        "magi.fs.write ",
        "magi.shell.exec",
    ] {
        let response = rpc(
            &f,
            "tools/call",
            call(
                name,
                serde_json::json!({"path": "b.txt", "content": "x", "command": "id"}),
            ),
        )
        .await;
        assert!(is_error(&response), "{name}: {response}");
    }
    assert!(!f.root.join("b.txt").exists());
}

#[tokio::test]
async fn audit_and_events_never_contain_file_content_or_the_token_secret() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(2));
    let response = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "c.txt", "content": "SUPER-PRIVATE-CONTENT"}),
        ),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let audit = serde_json::to_string(&f.state.mcp_service.audit().recent(50, None)).unwrap();
    assert!(audit.contains("magi.fs.write"));
    assert!(!audit.contains("SUPER-PRIVATE-CONTENT"));
    assert!(!audit.contains(&f.secret));
    let events = serde_json::to_string(&f.state.event_bus.snapshot()).unwrap();
    assert!(!events.contains(&f.secret), "事件流不得出现令牌原文");
}

#[tokio::test]
async fn open_mode_still_rejects_hard_blocked_commands_without_prompting() {
    // 开放权限只免掉人工确认；用户配置的 HardBlock 规则、路径与访问档限制仍然拒绝。
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(2));
    f.state
        .settings_store
        .set_section(
            "safeguardConfig",
            serde_json::json!({"rules": [{
                "pattern": "magi-hard-block-probe",
                "enabled": true,
                "category": "custom",
                "action": "hard_block"
            }]}),
        )
        .unwrap();
    let blocked = rpc(
            &f,
            "tools/call",
            call(
                "magi.shell.exec",
                serde_json::json!({"command": "touch hard-block-ran.txt && echo magi-hard-block-probe"}),
            ),
        )
        .await;
    assert!(
        is_error(&blocked),
        "开放模式也必须拒绝 HardBlock: {blocked}"
    );
    assert!(
        blocked.to_string().contains("tool_safety_rejected"),
        "拒绝原因必须是安全防护硬拦截: {blocked}"
    );
    assert!(
        !f.root.join("hard-block-ran.txt").exists(),
        "被硬拦截的命令不得产生任何副作用"
    );
    assert!(
        f.state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&external_session_id(&f.token_id))
            .is_empty(),
        "硬拦截是直接拒绝，不是转成待确认"
    );

    // 同一令牌的其它命令不受影响：只拦命中规则的那一条。
    let ok = rpc(
        &f,
        "tools/call",
        call(
            "magi.shell.exec",
            serde_json::json!({"command": "printf still-open"}),
        ),
    )
    .await;
    assert!(!is_error(&ok), "{ok}");
    assert!(ok.to_string().contains("still-open"));
}

#[tokio::test]
async fn read_write_mode_runs_normal_io_but_prompts_for_commands_and_opaque_effects() {
    let f = fixture_with_approval(ApprovalMode::ReadWrite, Duration::from_millis(150));
    let written = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({
                "path": "rw.txt", "content": "allowed"
            }),
        ),
    )
    .await;
    assert!(!is_error(&written), "{written}");
    let read = rpc(
        &f,
        "tools/call",
        call("magi.fs.read", serde_json::json!({"path": "rw.txt"})),
    )
    .await;
    assert!(!is_error(&read), "{read}");
    assert!(read.to_string().contains("allowed"));
    assert!(
        f.state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&external_session_id(&f.token_id))
            .is_empty()
    );

    for (name, arguments) in [
        ("magi.fs.remove", serde_json::json!({"path": "rw.txt"})),
        (
            "magi.shell.exec",
            serde_json::json!({"command": "touch command-must-not-run"}),
        ),
        ("mcp.mcp__docs__publish", serde_json::json!({})),
    ] {
        let result = rpc(&f, "tools/call", call(name, arguments)).await;
        assert!(is_error(&result), "未确认必须拒绝: {name}: {result}");
        assert!(
            result.to_string().contains("tool_approval_timeout"),
            "必须进入审批并超时: {result}"
        );
    }
    assert!(f.root.join("rw.txt").exists());
    assert!(!f.root.join("command-must-not-run").exists());
    assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let outside = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({
                "path": "../escape.txt", "content": "deny"
            }),
        ),
    )
    .await;
    assert!(is_error(&outside), "{outside}");
}

#[tokio::test]
async fn approval_mode_always_allows_all_calls_without_a_prompt() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_millis(300));
    let written = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.write",
            serde_json::json!({"path": "auto.txt", "content": "ok"}),
        ),
    )
    .await;
    assert!(!is_error(&written), "{written}");
    assert!(
        f.state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&external_session_id(&f.token_id))
            .is_empty(),
        "自动允许普通写入不应创建待确认项"
    );

    let created = rpc(
        &f,
        "tools/call",
        call("magi.fs.mkdir", serde_json::json!({"path": "auto-dir"})),
    )
    .await;
    assert!(!is_error(&created), "普通目录写入也不应再次询问: {created}");
    assert!(f.root.join("auto-dir").is_dir());
    assert!(
        f.state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&external_session_id(&f.token_id))
            .is_empty()
    );

    std::fs::write(f.root.join("remove-without-prompt.txt"), "remove me").unwrap();
    let removed = rpc(
        &f,
        "tools/call",
        call(
            "magi.fs.remove",
            serde_json::json!({"path": "remove-without-prompt.txt"}),
        ),
    )
    .await;
    assert!(!is_error(&removed), "开放权限应自动执行删除: {removed}");
    assert!(!f.root.join("remove-without-prompt.txt").exists());

    let command = rpc(
        &f,
        "tools/call",
        call(
            "magi.shell.exec",
            serde_json::json!({"command": "printf open-mode"}),
        ),
    )
    .await;
    assert!(!is_error(&command), "开放权限应自动执行命令: {command}");
    assert!(command.to_string().contains("open-mode"));
    let read = rpc(
        &f,
        "tools/call",
        call("magi.fs.read", serde_json::json!({"path": "auto.txt"})),
    )
    .await;
    assert!(!is_error(&read), "开放权限应自动读取: {read}");
    assert!(read.to_string().contains("ok"));
    assert!(
        f.state
            .conversation_registry
            .tool_approvals()
            .pending_for_session(&external_session_id(&f.token_id))
            .is_empty(),
        "开放权限调用不应留下待确认项"
    );
}

#[tokio::test]
async fn ask_mode_still_prompts_for_destructive_calls() {
    let f = fixture_with_approval(ApprovalMode::Ask, Duration::from_millis(250));
    std::fs::write(f.root.join("keep.txt"), "keep").unwrap();
    let removed = rpc(
        &f,
        "tools/call",
        call("magi.fs.remove", serde_json::json!({"path": "keep.txt"})),
    )
    .await;
    assert!(is_error(&removed), "删除仍需逐次授权: {removed}");
    assert!(f.root.join("keep.txt").exists(), "未授权时不得删除文件");
}

// ── 网关动态目录：项目允许的内置工具 / 下游 MCP / Skill handler ──────────────

async fn tool_names(f: &Fixture) -> Vec<String> {
    rpc(f, "tools/list", serde_json::json!({})).await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn gateway_lists_extra_builtins_and_downstream_mcp_but_never_context_bound_tools() {
    let f = fixture(Duration::from_secs(5));
    let names = tool_names(&f).await;
    assert!(
        names.contains(&"mcp.mcp__docs__search".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"mcp.mcp__docs__publish".to_string()));
    // 需要 Task / 会话 / 浏览器上下文的内置工具不对外开放。
    for hidden in [
        "agent_spawn",
        "update_plan",
        "memory_write",
        "get_goal",
        "context_read",
    ] {
        assert!(!names.contains(&format!("magi.{hidden}")), "{hidden}");
    }
    assert!(
        !names.iter().any(|name| name.starts_with("magi.browser_")),
        "浏览器工具不对外开放: {names:?}"
    );
    // 静态目录里的内置工具不重复出现。
    assert!(!names.contains(&"magi.file_read".to_string()));
    // 令牌统一开放完整目录：命令执行与下游写入类工具都在，授权方式只决定是否询问。
    assert!(names.contains(&"magi.shell.exec".to_string()));
    assert!(
        !names.contains(&"magi.shell_exec".to_string()),
        "内部名不得作为公开名重复出现"
    );
}

#[tokio::test]
async fn open_mode_runs_downstream_read_and_write_tools_without_approval() {
    let f = fixture_with_approval(ApprovalMode::Always, Duration::from_millis(400));
    let search = rpc(
        &f,
        "tools/call",
        call("mcp.mcp__docs__search", serde_json::json!({})),
    )
    .await;
    assert!(!is_error(&search), "{search}");
    assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    let publish = rpc(
        &f,
        "tools/call",
        call("mcp.mcp__docs__publish", serde_json::json!({})),
    )
    .await;
    assert!(!is_error(&publish), "{publish}");
    assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn extra_builtin_runs_through_the_same_policy_and_unknown_dynamic_names_are_refused() {
    let f = fixture(Duration::from_secs(5));
    for name in [
        "mcp.nope",
        "skill.nope",
        "magi.agent_spawn",
        "mcp.mcp__docs__missing",
    ] {
        let reply = rpc(&f, "tools/call", call(name, serde_json::json!({}))).await;
        assert!(is_error(&reply), "{name}: {reply}");
    }
}
