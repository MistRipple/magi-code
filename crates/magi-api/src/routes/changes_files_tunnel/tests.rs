use super::*;
use crate::change_projection::collect_session_pending_changes_with_state;
use crate::state::{ApiState, RuntimeStatePersistence};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_core::{AbsolutePath, ExecutionOwnership, MissionId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tower::ServiceExt;

static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(1);

#[test]
fn score_lan_candidate_172_private_range_is_octet_accurate() {
    // 172.16.0.0/12 内的地址按私网加分。
    assert!(score_lan_candidate("en0", "172.16.0.1") > 0);
    assert!(score_lan_candidate("en0", "172.31.255.254") > 0);
    // 区间外的 172. 公网地址不应被当作私网，应落入降权分支。
    assert!(score_lan_candidate("en0", "172.15.0.1") < score_lan_candidate("en0", "172.16.0.1"));
    assert!(score_lan_candidate("en0", "172.32.0.1") < score_lan_candidate("en0", "172.16.0.1"));
    // 直接验证段判定函数边界。
    assert!(addr_in_172_private_range("172.16.0.0"));
    assert!(addr_in_172_private_range("172.31.0.0"));
    assert!(!addr_in_172_private_range("172.15.0.0"));
    assert!(!addr_in_172_private_range("172.32.0.0"));
    assert!(!addr_in_172_private_range("172.abc"));
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let unique = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "{}-{}-{}-{}",
        prefix,
        std::process::id(),
        UtcMillis::now().0,
        unique
    ));
    fs::create_dir_all(&dir).expect("temp dir should create");
    dir
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = magi_process::std_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("git command should start");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn prompt_enhance_instruction_uses_requested_locale_and_data_boundaries() {
    let english = build_enhance_prompt_instruction(
        "Improve this task",
        Some("writing"),
        Some("Make the result concise"),
        Some("en-US"),
    );
    assert!(english.contains("Output only the rewritten prompt"));
    assert!(english.contains("<skill_context>"));
    assert!(english.contains("<user_prompt>"));
    assert!(english.contains("Improve this task"));

    let chinese = build_enhance_prompt_instruction("整理项目", None, None, Some("zh-CN"));
    assert!(chinese.contains("只输出优化后的 prompt"));
    assert!(chinese.contains("<user_prompt>"));
}

#[test]
fn prompt_enhance_normalizes_plain_text_fences_and_json() {
    assert_eq!(
        normalize_enhanced_prompt("```text\n整理项目\n```").as_deref(),
        Some("整理项目")
    );
    assert_eq!(
        normalize_enhanced_prompt(r#"{"enhancedPrompt":"整理项目"}"#).as_deref(),
        Some("整理项目")
    );
    assert_eq!(normalize_enhanced_prompt(" \n "), None);
}

#[tokio::test]
async fn prompt_enhance_draft_scope_does_not_require_session() {
    let root = unique_temp_dir("magi-prompt-enhance-draft-scope");
    let state = build_state_with_workspace_root(&root, "workspace-prompt-enhance-draft");
    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/prompt/enhance")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceId": "workspace-prompt-enhance-draft",
                        "prompt": "整理项目"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    assert_eq!(
        payload["message"],
        "辅助模型未配置，无法增强提示词；请在设置中配置 auxiliary 模型"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_enhance_supports_personal_and_workspace_drafts_and_sessions() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move {
        axum::serve(
                listener,
                Router::new().route("/v1/chat/completions", post(|| async {
                    Json(serde_json::json!({
                        "choices": [{"message": {"role": "assistant", "content": "请整理项目结构并说明各模块职责。"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18}
                    }))
                })),
            ).await.unwrap();
    });
    let root = unique_temp_dir("magi-prompt-enhance-scopes");
    let state = build_state_with_workspace_root(&root, "enhance-workspace");
    state
        .settings_store
        .set_section(
            "auxiliary",
            serde_json::json!({
                "baseUrl": format!("http://{address}/v1"),
                "apiProtocol": "openai_chat", "apiKey": "fixture", "model": "enhance-fixture"
            }),
        )
        .unwrap();
    state
        .session_store
        .create_session(SessionId::new("enhance-personal"), "个人会话")
        .unwrap();
    state
        .session_store
        .create_session_for_workspace(
            SessionId::new("enhance-project"),
            "项目会话",
            Some("enhance-workspace".into()),
        )
        .unwrap();
    for mut request in [
        serde_json::json!({}),
        serde_json::json!({"sessionId": "enhance-personal"}),
        serde_json::json!({"workspaceId": "enhance-workspace"}),
        serde_json::json!({"workspaceId": "enhance-workspace", "sessionId": "enhance-project"}),
    ] {
        request["prompt"] = serde_json::json!("整理项目");
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/prompt/enhance")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{request}");
        assert_eq!(
            read_json_response(response).await["enhancedPrompt"],
            "请整理项目结构并说明各模块职责。"
        );
    }
    let usage = state
        .event_bus
        .snapshot()
        .recent_events
        .into_iter()
        .filter(|event| event.event_type == "model.usage.recorded")
        .collect::<Vec<_>>();
    assert_eq!(usage.len(), 2, "仅已绑定会话的强化调用归入会话用量");
    assert!(usage.iter().any(
        |event| event.session_id == Some(SessionId::new("enhance-personal"))
            && event.workspace_id.is_none()
    ));
    assert!(usage.iter().any(
        |event| event.session_id == Some(SessionId::new("enhance-project"))
            && event.workspace_id == Some(WorkspaceId::new("enhance-workspace"))
    ));
    provider.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn prompt_enhance_personal_draft_reaches_auxiliary_configuration_validation() {
    let error = enhance_prompt(
        State(build_state()),
        Json(serde_json::from_value(serde_json::json!({"prompt": "整理想法"})).unwrap()),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ApiError::InvalidInput(message) if message.contains("辅助模型未配置")));
}

#[tokio::test]
async fn personal_delete_keeps_files_when_session_commit_fails() {
    let root = unique_temp_dir("magi-personal-delete-rollback");
    let state = build_state()
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            root.clone(),
            root.join("workspaces.json"),
            root.join("knowledge.json"),
        )))
        .with_session_projection_persist(Arc::new(|_, _, _| {
            Err(ApiError::internal_assembly(
                "测试删除持久化失败",
                "injected failure",
            ))
        }));
    let id = SessionId::new("personal-delete-rollback");
    state
        .session_store
        .create_session(id.clone(), "保留成果")
        .unwrap();
    let directory = state.personal_session_execution_root(&id).unwrap();
    fs::write(directory.join("report.md"), "keep").unwrap();
    assert!(state.delete_session_and_resources(&id).await.is_err());
    assert!(state.session_store.session(&id).is_some());
    assert_eq!(
        fs::read_to_string(directory.join("report.md")).unwrap(),
        "keep"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn personal_files_keep_session_boundary_and_do_not_require_project_snapshot() {
    let state_root = unique_temp_dir("magi-personal-files");
    let project_root = unique_temp_dir("magi-personal-files-project");
    let state = build_state_with_workspace_root(&project_root, "file-project")
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            state_root.clone(),
            state_root.join("workspaces.json"),
            state_root.join("knowledge.json"),
        )));
    for id in ["file-personal-a", "file-personal-b", "file-personal-empty"] {
        state
            .session_store
            .create_session(SessionId::new(id), id)
            .unwrap();
    }
    let a = state
        .personal_session_execution_root(&SessionId::new("file-personal-a"))
        .unwrap();
    let b = state
        .personal_session_execution_root(&SessionId::new("file-personal-b"))
        .unwrap();
    fs::write(a.join("report.md"), "A report").unwrap();
    fs::write(b.join("report.md"), "B report").unwrap();
    fs::write(a.join("image.png"), b"fixture-image").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(b.join("report.md"), a.join("escape.md")).unwrap();
    for (path, expected) in [
        (
            "/files/content?sessionId=file-personal-a&filePath=report.md",
            StatusCode::OK,
        ),
        (
            "/files/raw?sessionId=file-personal-a&filePath=image.png",
            StatusCode::OK,
        ),
        (
            "/files/download?sessionId=file-personal-a&filePath=report.md",
            StatusCode::OK,
        ),
        ("/filesystem/list?sessionId=file-personal-a", StatusCode::OK),
        (
            "/filesystem/list?sessionId=file-personal-empty",
            StatusCode::OK,
        ),
        (
            "/files/content?sessionId=file-personal-a&workspaceId=file-project&filePath=report.md",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/files/content?sessionId=file-personal-a&workspaceId=unknown&filePath=report.md",
            StatusCode::NOT_FOUND,
        ),
        (
            "/files/content?sessionId=file-personal-a&filePath=../file-personal-b/report.md",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = routes()
            .with_state(state.clone())
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
    }
    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/files/content?sessionId=file-personal-b&filePath=report.md")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let payload = read_json_response(response).await;
    assert_eq!(payload["content"], "B report");
    assert!(payload["workspaceId"].is_null());
    assert!(
        state
            .snapshot_manager
            .get_session("file-personal-b")
            .is_none()
    );
    #[cfg(unix)]
    {
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/files/content?sessionId=file-personal-a&filePath=escape.md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    state
        .delete_session_and_resources(&SessionId::new("file-personal-a"))
        .await
        .unwrap();
    assert!(!a.exists(), "删除提交后回收自有文件");
    assert!(b.join("report.md").exists(), "不得清理兄弟会话");
    fs::remove_dir_all(state_root).unwrap();
    fs::remove_dir_all(project_root).unwrap();
}

fn build_state() -> ApiState {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let session_store = Arc::new(SessionStore::default());
    let workspace_store = Arc::new(WorkspaceStore::default());
    let governance = Arc::new(GovernanceService::default());
    ApiState::new(
        "magi-test",
        event_bus,
        session_store,
        workspace_store,
        governance,
    )
    .with_task_store(Arc::new(TaskStore::new()))
}

#[tokio::test]
async fn project_files_keep_source_identity_across_primary_change_and_revocation() {
    use super::super::test_project_fixtures::{add_source, set_members};
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    let outside = root.path().join("outside");
    for (path, content) in [(&a, "A"), (&b, "B"), (&outside, "outside")] {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("report.md"), content).unwrap();
        fs::write(path.join("image.png"), content).unwrap();
    }
    let state = build_state_with_workspace_root(&a, "files-project");
    let project = add_source(&state, "files-project", "second", &b);
    let first = project.primary_source_folder_id;
    state
        .session_store
        .create_session_for_workspace(
            SessionId::new("files-session"),
            "files",
            Some("files-project".into()),
        )
        .unwrap();
    let app = routes().with_state(state.clone());
    let request = |path: String| Request::builder().uri(path).body(Body::empty()).unwrap();
    let binding = "sessionId=files-session&workspaceId=files-project";
    for (id, content) in [(&first, "A"), (&"second".to_string(), "B")] {
        let response = app
            .clone()
            .oneshot(request(format!(
                "/files/content?{binding}&sourceFolderId={id}&filePath=report.md"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = read_json_response(response).await;
        assert_eq!(value["content"], content);
        assert_eq!(value["sourceFolderId"], id.as_str());
        for endpoint in ["/files/download", "/files/raw", "/filesystem/list"] {
            let file = if endpoint == "/filesystem/list" {
                ""
            } else {
                "&filePath=image.png"
            };
            let response = app
                .clone()
                .oneshot(request(format!(
                    "{endpoint}?{binding}&sourceFolderId={id}{file}"
                )))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        }
        let draft = app
            .clone()
            .oneshot(request(format!(
                "/files/content?workspaceId=files-project&sourceFolderId={id}&filePath=report.md"
            )))
            .await
            .unwrap();
        assert_eq!(draft.status(), StatusCode::OK);
        let draft = read_json_response(draft).await;
        assert_eq!(draft["content"], content);
        assert_eq!(draft["sourceFolderId"], id.as_str());
        let diff = app
            .clone()
            .oneshot(request(format!(
                "/changes/diff?workspaceId=files-project&sourceFolderId={id}"
            )))
            .await
            .unwrap();
        assert_eq!(diff.status(), StatusCode::OK);
        let diff = read_json_response(diff).await;
        assert_eq!(diff["sourceFolderId"], id.as_str());
        assert_eq!(diff["pendingChangesState"]["reasonCode"], "session_unbound");
    }
    set_members(&state, "files-project", &[&first, "second"], "second");
    let original_uri =
        format!("/files/content?{binding}&sourceFolderId={first}&filePath=report.md");
    assert_eq!(
        read_json_response(
            app.clone()
                .oneshot(request(original_uri.clone()))
                .await
                .unwrap()
        )
        .await["content"],
        "A"
    );
    for path in [
        "/changes/diff?workspaceId=files-project".to_string(),
        "/files/content?workspaceId=files-project&filePath=report.md".to_string(),
        format!("/files/content?{binding}&filePath=report.md"),
        format!("/files/content?{binding}&sourceFolderId=ungranted&filePath=report.md"),
        format!("/files/content?{binding}&sourceFolderId={first}&filePath=../b/report.md"),
        format!("/filesystem/list?{binding}&sourceFolderId={first}&path=../outside"),
    ] {
        let response = app.clone().oneshot(request(path.clone())).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("report.md"), a.join("escape.md")).unwrap();
        let response = app
            .clone()
            .oneshot(request(format!(
                "/files/download?{binding}&sourceFolderId={first}&filePath=escape.md"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    set_members(&state, "files-project", &["second"], "second");
    let response = app.clone().oneshot(request(original_uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    fs::remove_dir_all(&b).unwrap();
    let response = app
        .oneshot(request(format!(
            "/filesystem/list?{binding}&sourceFolderId=second"
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!b.exists());
    assert!(a.join("report.md").exists());
}

fn build_state_with_workspace_root(root: &Path, workspace_id: &str) -> ApiState {
    let state = build_state();
    state
        .workspace_registry
        .register(
            WorkspaceId::new(workspace_id),
            AbsolutePath::new(root.to_string_lossy().as_ref()),
        )
        .expect("workspace should register");
    state
}

async fn read_json_response(response: axum::response::Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    serde_json::from_slice(&body).expect("payload should deserialize")
}

/// 注册 workspace + session（可选 mission）。session 创建后立即在 SnapshotManager
/// 中拉起 SnapshotSession，并完成首次 baseline 扫描；调用方随后做文件改动 + reconcile。
async fn register_workspace_and_snapshot(
    state: &ApiState,
    workspace_id: &str,
    session_id: &str,
    root: &Path,
    mission_id: Option<&str>,
) -> Arc<SnapshotSession> {
    let ws = WorkspaceId::new(workspace_id);
    let sid = SessionId::new(session_id);
    state
        .workspace_registry
        .register(
            ws.clone(),
            AbsolutePath::new(root.to_string_lossy().as_ref()),
        )
        .expect("workspace should register");
    state
        .session_store
        .create_session_for_workspace(sid.clone(), session_id, Some(workspace_id.to_string()))
        .expect("session should create");
    if let Some(mid) = mission_id {
        state.session_store.bind_execution_ownership_for_test(
            sid.clone(),
            ExecutionOwnership {
                session_id: Some(sid.clone()),
                workspace_id: Some(ws.clone()),
                mission_id: Some(MissionId::new(mid)),
                ..ExecutionOwnership::default()
            },
        );
    }
    state
        .snapshot_manager
        .start_session(session_id.to_string(), root.to_path_buf())
        .await
        .expect("snapshot session should start")
}

#[tokio::test]
async fn lan_access_uses_current_daemon_port() {
    let root = unique_temp_dir("magi-changes-route-lan-access");
    let state =
        build_state_with_workspace_root(&root, "workspace-lan-access").with_tunnel_port(39219);

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/lan-access?workspaceId=workspace-lan-access&workspacePath=%2Ftmp%2Fmagi%20test&sessionId=session-lan-access")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    assert_eq!(payload["port"], 39219);
    assert!(
        payload["url"]
            .as_str()
            .expect("url should be string")
            .contains(":39219/web.html?workspaceId=workspace-lan-access")
    );
    let url = payload["url"].as_str().expect("url should be string");
    assert!(url.contains("workspacePath=%2Ftmp%2Fmagi%20test"));
    assert!(url.contains("sessionId=session-lan-access"));
}

#[tokio::test]
async fn filesystem_list_is_workspace_bound_and_filters_hidden_entries() {
    let root = unique_temp_dir("magi-filesystem-list-bound");
    fs::write(root.join("visible.txt"), "visible\n").expect("visible file should write");
    fs::write(root.join(".hidden"), "hidden\n").expect("hidden file should write");
    fs::create_dir_all(root.join("src")).expect("src dir should create");
    let state = build_state_with_workspace_root(&root, "workspace-filesystem-list");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/filesystem/list?workspaceId=workspace-filesystem-list")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["workspaceId"], "workspace-filesystem-list");
    assert!(
        payload["workspacePath"]
            .as_str()
            .is_some_and(|path| path.contains("magi-filesystem-list-bound")),
        "filesystem payload must carry workspace path"
    );
    assert!(
        payload["path"]
            .as_str()
            .is_some_and(|path| path.contains("magi-filesystem-list-bound")),
        "filesystem payload must carry listed path"
    );
    let entries = payload["entries"].as_array().expect("entries array");
    assert!(
        entries.iter().any(|entry| entry["name"] == "visible.txt"),
        "visible file should be listed"
    );
    assert!(
        entries.iter().all(|entry| entry["name"] != ".hidden"),
        "hidden file should be filtered by default"
    );

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/filesystem/list?workspaceId=workspace-filesystem-list&showHidden=1")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let entries = payload["entries"].as_array().expect("entries array");
    assert!(
        entries.iter().any(|entry| entry["name"] == ".hidden"),
        "showHidden=1 should include hidden entries"
    );
}

#[tokio::test]
async fn filesystem_list_resolves_workspace_from_registered_path_when_query_id_is_stale() {
    let root = unique_temp_dir("magi-filesystem-list-path-binding");
    fs::write(root.join("from-path.txt"), "workspace path wins\n")
        .expect("workspace file should write");
    let state = build_state_with_workspace_root(&root, "workspace-filesystem-path");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/filesystem/list?workspaceId=workspace-stale-query&workspacePath={}",
                    urlencoding::encode(root.to_string_lossy().as_ref())
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["workspaceId"], "workspace-filesystem-path");
    assert_eq!(
        payload["workspacePath"],
        root.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let entries = payload["entries"].as_array().expect("entries array");
    assert!(
        entries.iter().any(|entry| entry["name"] == "from-path.txt"),
        "filesystem list must read the workspace resolved from workspacePath"
    );
}

#[tokio::test]
async fn filesystem_list_rejects_missing_workspace_and_outside_path() {
    let root = unique_temp_dir("magi-filesystem-list-secure");
    let outside = unique_temp_dir("magi-filesystem-list-outside");
    let state = build_state_with_workspace_root(&root, "workspace-filesystem-secure");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/filesystem/list")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/filesystem/list?workspaceId=workspace-filesystem-secure&path={}",
                    outside.to_string_lossy()
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn filesystem_browse_lists_picker_directory_without_workspace_scope() {
    let root = unique_temp_dir("magi-filesystem-browse-picker");
    fs::write(root.join("visible.txt"), "visible\n").expect("visible file should write");
    fs::create_dir_all(root.join(".hidden-z")).expect("hidden dir should create");
    fs::create_dir_all(root.join(".hidden-a")).expect("hidden dir should create");
    fs::create_dir_all(root.join("workspace-z")).expect("workspace dir should create");
    fs::create_dir_all(root.join("workspace-a")).expect("workspace dir should create");
    let state = build_state();

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/filesystem/browse?path={}",
                    root.to_string_lossy()
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert!(
        payload["workspaceId"].is_null(),
        "browse payload must not claim a workspace binding"
    );
    assert!(
        payload["displayPath"]
            .as_str()
            .is_some_and(|path| path.contains("magi-filesystem-browse-picker")),
        "browse payload must carry listed path"
    );
    assert!(
        payload["pathRef"]
            .as_str()
            .is_some_and(|value| value.starts_with("mhp1:"))
    );
    assert!(
        payload["breadcrumbs"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    let entries = payload["entries"].as_array().expect("entries array");
    assert!(
        entries.iter().any(|entry| entry["name"] == "workspace-a"),
        "directory candidates should be listed"
    );
    assert!(
        entries.iter().all(|entry| entry["name"] != ".hidden-a"),
        "hidden entries should be filtered by default"
    );
    assert!(entries.iter().all(|entry| entry["isDirectory"] == true));
    assert!(entries.iter().all(|entry| entry["name"] != "visible.txt"));

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/filesystem/browse?path={}&showHidden=1",
                    root.to_string_lossy()
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let entries = payload["entries"].as_array().expect("entries array");
    let names = entries
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![".hidden-a", ".hidden-z", "workspace-a", "workspace-z"],
        "隐藏目录必须优先，组内按名称排序"
    );
}

#[tokio::test]
async fn filesystem_browse_includes_files_only_when_requested() {
    let root = unique_temp_dir("magi-filesystem-browse-files");
    fs::write(root.join("visible.txt"), "visible\n").unwrap();
    fs::write(root.join(".hidden.txt"), "hidden\n").unwrap();
    fs::create_dir_all(root.join("folder")).unwrap();
    for (suffix, expected_names) in [
        ("", vec!["folder"]),
        ("&includeFiles=1", vec!["folder", "visible.txt"]),
        (
            "&includeFiles=1&showHidden=1",
            vec!["folder", ".hidden.txt", "visible.txt"],
        ),
    ] {
        let response = routes()
            .with_state(build_state())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/filesystem/browse?path={}{}",
                        root.to_string_lossy(),
                        suffix
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        let entries = payload["entries"].as_array().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected_names
        );
        for entry in entries {
            assert_eq!(entry["isDirectory"], entry["name"] == "folder");
            let resolved = decode_path_ref(entry["pathRef"].as_str().unwrap()).unwrap();
            assert!(resolved.as_path().exists());
        }
    }
}

#[tokio::test]
async fn filesystem_list_returns_entry_path_refs_for_lossless_navigation() {
    let root = unique_temp_dir("magi-filesystem-list-path-ref");
    let file = root.join("preview.txt");
    fs::write(&file, "preview\n").expect("preview file should write");
    let state = build_state_with_workspace_root(&root, "workspace-filesystem-path-ref");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/filesystem/list?workspaceId=workspace-filesystem-path-ref")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let entry = payload["entries"]
        .as_array()
        .and_then(|entries| entries.first())
        .expect("file entry should be returned");
    let decoded =
        magi_core::HostPath::from_path_ref(entry["pathRef"].as_str().expect("entry path ref"))
            .expect("entry path ref should decode");
    let canonical_file =
        magi_core::HostPath::canonicalize(&file).expect("file should canonicalize");
    assert_eq!(decoded.as_path(), canonical_file.as_path());
    assert_eq!(entry["displayPath"], canonical_file.display_string());
}

#[tokio::test]
async fn file_reveal_target_returns_only_existing_workspace_file() {
    let root = unique_temp_dir("magi-file-reveal-target");
    let file = root.join("src/main.rs");
    fs::create_dir_all(file.parent().unwrap()).expect("source directory should create");
    fs::write(&file, "fn main() {}\n").expect("source file should write");
    let state = build_state_with_workspace_root(&root, "workspace-file-reveal");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/reveal-target")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceId": "workspace-file-reveal",
                        "filePath": "src/main.rs"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert!(
        payload["targetPathRef"]
            .as_str()
            .is_some_and(|value| value.starts_with("mhp1:"))
    );
    assert!(
        payload["workspaceRootPathRef"]
            .as_str()
            .is_some_and(|value| value.starts_with("mhp1:"))
    );
    let ancestor_path_refs = payload["ancestorPathRefs"]
        .as_array()
        .expect("ancestor path refs array");
    assert_eq!(ancestor_path_refs.len(), 1);
    let ancestor_path = magi_core::HostPath::from_path_ref(
        ancestor_path_refs[0]
            .as_str()
            .expect("ancestor path ref should be a string"),
    )
    .expect("ancestor path ref should decode")
    .into_path_buf();
    assert_eq!(
        ancestor_path,
        magi_core::HostPath::canonicalize(file.parent().unwrap())
            .unwrap()
            .into_path_buf()
    );
    assert_eq!(
        payload["displayPath"],
        magi_core::HostPath::canonicalize(&file)
            .unwrap()
            .display_string()
    );
}

#[tokio::test]
async fn file_reveal_target_rejects_directory() {
    let root = unique_temp_dir("magi-file-reveal-directory");
    fs::create_dir_all(root.join("src")).expect("source directory should create");
    let state = build_state_with_workspace_root(&root, "workspace-file-reveal-directory");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/reveal-target")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceId": "workspace-file-reveal-directory",
                        "filePath": "src"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    assert_eq!(payload["message"], "路径不是可定位的文件");
}

#[cfg(unix)]
#[tokio::test]
async fn file_reveal_target_rejects_symlink_outside_workspace() {
    use std::os::unix::fs::symlink;

    let root = unique_temp_dir("magi-file-reveal-symlink");
    let outside = unique_temp_dir("magi-file-reveal-outside").join("outside.txt");
    fs::write(&outside, "outside\n").expect("outside file should write");
    symlink(&outside, root.join("outside-link.txt")).expect("symlink should create");
    let state = build_state_with_workspace_root(&root, "workspace-file-reveal-symlink");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/reveal-target")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceId": "workspace-file-reveal-symlink",
                        "filePath": "outside-link.txt"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    assert_eq!(payload["message"], "路径越出工作区边界");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn filesystem_list_preserves_non_utf8_entries_with_path_ref() {
    use std::os::unix::ffi::OsStringExt;

    let root = unique_temp_dir("magi-filesystem-list-non-utf8");
    let name = std::ffi::OsString::from_vec(vec![b'f', b'i', b'l', b'e', b'-', 0xff]);
    let file = root.join(&name);
    fs::write(&file, "lossless\n").expect("non utf8 file should write");
    let state = build_state_with_workspace_root(&root, "workspace-filesystem-non-utf8");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/filesystem/list?workspaceId=workspace-filesystem-non-utf8")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let entry = payload["entries"]
        .as_array()
        .and_then(|entries| entries.first())
        .expect("non utf8 entry should be returned");
    let decoded =
        magi_core::HostPath::from_path_ref(entry["pathRef"].as_str().expect("entry path ref"))
            .expect("entry path ref should decode");
    assert_eq!(decoded.as_path(), file.canonicalize().unwrap());
}

#[tokio::test]
async fn filesystem_resolve_returns_file_path_ref_without_using_directory_browse() {
    let root = unique_temp_dir("magi-filesystem-browse-file-selection");
    let file = root.join("reference.txt");
    fs::write(&file, "reference\n").expect("reference file should write");
    let state = build_state();

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/filesystem/resolve")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "input": file.to_string_lossy() }).to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let canonical_file =
        magi_core::HostPath::canonicalize(&file).expect("file should canonicalize");
    assert_eq!(payload["displayPath"], canonical_file.display_string());
    assert_eq!(payload["kind"], "file");
    assert!(
        payload["pathRef"]
            .as_str()
            .is_some_and(|value| value.starts_with("mhp1:"))
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn filesystem_browse_preserves_non_utf8_directory_with_path_ref() {
    use std::os::unix::ffi::OsStringExt;

    let root = unique_temp_dir("magi-filesystem-browse-non-utf8");
    let name = std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', b'-', 0xff]);
    fs::create_dir_all(root.join(&name)).expect("non utf8 dir should create");
    let state = build_state();

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/filesystem/browse?path={}",
                    urlencoding::encode(root.to_string_lossy().as_ref())
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let entry = payload["entries"]
        .as_array()
        .and_then(|entries| entries.first())
        .expect("non utf8 directory should be returned");
    let decoded =
        magi_core::HostPath::from_path_ref(entry["pathRef"].as_str().expect("entry path ref"))
            .expect("path ref should decode");
    assert_eq!(decoded.as_path(), root.join(name).canonicalize().unwrap());
}

#[tokio::test]
async fn filesystem_browse_uses_public_missing_directory_error() {
    let root = unique_temp_dir("magi-filesystem-browse-missing");
    let missing = root.join("missing");
    let state = build_state();

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/filesystem/browse?path={}",
                    missing.to_string_lossy()
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    assert_eq!(payload["message"], "目录不可读取或不存在");
}

#[tokio::test]
async fn get_diff_returns_empty_without_session_scope() {
    let root = unique_temp_dir("magi-changes-route-no-session-diff");
    fs::write(root.join("notes.txt"), "hello\n").expect("workspace file should write");
    let state = build_state_with_workspace_root(&root, "workspace-no-session");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/changes/diff?workspaceId=workspace-no-session")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    assert_eq!(payload["diff"], "");
    assert!(payload["filePath"].is_null());
}

#[tokio::test]
async fn get_diff_resolves_workspace_from_registered_path_when_query_id_is_stale() {
    let root = unique_temp_dir("magi-changes-route-path-diff");
    fs::write(root.join("notes.txt"), "hello\n").expect("workspace file should write");
    let state = build_state_with_workspace_root(&root, "workspace-path-diff");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/changes/diff?workspaceId=workspace-stale-query&workspacePath={}",
                    urlencoding::encode(root.to_string_lossy().as_ref())
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["workspaceId"], "workspace-path-diff");
    assert_eq!(
        payload["workspacePath"],
        root.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(payload["diff"], "");
    assert_eq!(
        payload["pendingChangesState"]["reasonCode"],
        "session_unbound"
    );
}

#[tokio::test]
async fn get_diff_rejects_missing_workspace_without_session() {
    let root = unique_temp_dir("magi-changes-route-no-session-missing-workspace");
    fs::write(root.join("notes.txt"), "hello\n").expect("workspace file should write");
    let state = build_state_with_workspace_root(&root, "workspace-no-session-missing");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/changes/diff")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    assert!(
        payload["message"]
            .as_str()
            .unwrap_or_default()
            .contains("workspaceId 不能为空")
    );
}

#[tokio::test]
async fn get_diff_returns_unified_diff_for_session_file() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-session-diff");
    fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
    let snap = register_workspace_and_snapshot(
        &state,
        "ws-session-diff",
        "sess-session-diff",
        &root,
        Some("mission-diff"),
    )
    .await;
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
    snap.reconcile().expect("reconcile should succeed");

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/diff?sessionId=sess-session-diff&workspaceId=ws-session-diff&filePath=alpha.txt")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    assert_eq!(payload["workspaceId"], "ws-session-diff");
    assert_eq!(payload["pendingChangesState"]["status"], "ready");
    assert_eq!(payload["pendingChangesState"]["pendingCount"], 1);
    assert!(
        payload["workspacePath"]
            .as_str()
            .is_some_and(|path| path.contains("magi-changes-route-session-diff")),
        "diff payload must carry canonical workspace path"
    );
    assert_eq!(payload["sessionId"], "sess-session-diff");
    assert_eq!(payload["executionGroupId"], "mission-diff");
    let diff = payload["diff"].as_str().unwrap_or_default();
    assert!(diff.contains("alpha"), "diff should mention path: {}", diff);
    assert!(
        diff.contains("-alpha"),
        "diff should contain old line marker: {}",
        diff
    );
    assert!(
        diff.contains("+alpha changed"),
        "diff should contain new line marker: {}",
        diff
    );
    assert_eq!(payload["originalContent"], serde_json::json!("alpha\n"));
    assert_eq!(
        payload["currentContent"],
        serde_json::json!("alpha changed\n")
    );
}

#[tokio::test]
async fn changes_refresh_tracks_external_restore_overwrite_and_partial_commit() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-external-refresh");
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Magi Test"]);
    git(&root, &["config", "user.email", "magi@example.test"]);
    fs::write(root.join("committed.txt"), "v0\n").expect("committed fixture");
    fs::write(root.join("pending.txt"), "v0\n").expect("pending fixture");
    git(&root, &["add", "committed.txt", "pending.txt"]);
    git(&root, &["commit", "-m", "initial"]);

    let snapshot = register_workspace_and_snapshot(
        &state,
        "ws-external-refresh",
        "sess-external-refresh",
        &root,
        None,
    )
    .await;
    let session_id = SessionId::new("sess-external-refresh");
    let workspace_id = WorkspaceId::new("ws-external-refresh");
    state
        .ensure_session_code_context(&session_id, &Some(workspace_id.clone()))
        .await
        .expect("session Git context")
        .expect("Git repository context");
    state.release_session_git_execution_lease(&session_id);

    let tool_context = magi_snapshot::ToolHookCtx {
        tool_call_id: "call-external-refresh".to_string(),
        worker_id: None,
        execution_group_id: Some("group-external-refresh".to_string()),
        declared_paths: vec![PathBuf::from("committed.txt"), PathBuf::from("pending.txt")],
    };
    snapshot.before_tool(&tool_context);
    fs::write(root.join("committed.txt"), "v1\n").expect("committed change");
    fs::write(root.join("pending.txt"), "v1\n").expect("pending change");
    snapshot.after_tool(&tool_context);

    git(&root, &["add", "committed.txt"]);
    git(&root, &["commit", "-m", "partial external commit"]);

    let app = routes().with_state(state.clone());
    let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/changes?sessionId=sess-external-refresh&workspaceId=ws-external-refresh&forceRefresh=true")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let pending = payload["pendingChanges"]
        .as_array()
        .expect("pending changes array");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["filePath"], "pending.txt");

    git(&root, &["restore", "pending.txt"]);
    let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/changes?sessionId=sess-external-refresh&workspaceId=ws-external-refresh&forceRefresh=true")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
    let payload = read_json_response(response).await;
    assert_eq!(payload["pendingChangesState"]["pendingCount"], 0);

    fs::write(root.join("pending.txt"), "external overwrite\n").expect("external overwrite");
    let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/changes?sessionId=sess-external-refresh&workspaceId=ws-external-refresh&forceRefresh=true")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
    let payload = read_json_response(response).await;
    let pending = payload["pendingChanges"]
        .as_array()
        .expect("pending changes array");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["filePath"], "pending.txt");
    // watcher 与显式 reconcile 竞速，二者都表示没有工具归属的外部覆盖。
    // 关键是不能沿用先前工具写入的身份或提供撤销外部修改的权限。
    assert!(
        matches!(
            pending[0]["sourceKind"].as_str(),
            Some("external" | "watcher")
        ),
        "{payload}"
    );
    assert_eq!(pending[0]["revertible"], false);
    assert!(pending[0]["toolCallId"].is_null());
    assert!(pending[0]["workerId"].is_null());

    let response = app
            .oneshot(
                Request::builder()
                    .uri("/changes/diff?sessionId=sess-external-refresh&workspaceId=ws-external-refresh&filePath=pending.txt")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("diff route should respond");
    let payload = read_json_response(response).await;
    assert_eq!(payload["currentContent"], "external overwrite\n");
}

#[tokio::test]
async fn changes_refresh_initial_git_binding_adopts_changes_already_committed_externally() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-initial-git-binding");
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Magi Test"]);
    git(&root, &["config", "user.email", "magi@example.test"]);
    fs::write(root.join("tracked.txt"), "v0\n").expect("tracked fixture");
    git(&root, &["add", "tracked.txt"]);
    git(&root, &["commit", "-m", "initial"]);
    register_workspace_and_snapshot(
        &state,
        "ws-initial-git-binding",
        "sess-initial-git-binding",
        &root,
        None,
    )
    .await;

    fs::write(root.join("tracked.txt"), "v1\n").expect("external committed change");
    git(&root, &["add", "tracked.txt"]);
    git(&root, &["commit", "-m", "external before context binding"]);

    let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/changes?sessionId=sess-initial-git-binding&workspaceId=ws-initial-git-binding&forceRefresh=true")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["pendingChangesState"]["pendingCount"], 0);
    let context = state
        .session_code_contexts
        .get("sess-initial-git-binding")
        .expect("Git context should bind");
    assert_eq!(context.git.base_head, context.git.observed_head);
}

#[tokio::test]
async fn list_changes_returns_current_session_projection() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-list");
    fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
    let snap = register_workspace_and_snapshot(
        &state,
        "ws-list-changes",
        "sess-list-changes",
        &root,
        None,
    )
    .await;
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
    snap.reconcile().expect("reconcile should succeed");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/changes?sessionId=sess-list-changes&workspaceId=ws-list-changes")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["sessionId"], "sess-list-changes");
    assert_eq!(payload["workspaceId"], "ws-list-changes");
    assert_eq!(payload["pendingChangesState"]["status"], "ready");
    assert_eq!(payload["pendingChangesState"]["pendingCount"], 1);
    assert_eq!(payload["pendingChanges"][0]["filePath"], "alpha.txt");
    assert_eq!(payload["pendingChanges"][0]["type"], "modify");
    assert!(payload["generatedAt"].as_u64().is_some());
}

#[tokio::test]
async fn get_diff_resolves_session_workspace_from_registered_path_when_query_id_is_stale() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-session-path-diff");
    fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
    let snap = register_workspace_and_snapshot(
        &state,
        "ws-session-path-diff",
        "sess-session-path-diff",
        &root,
        None,
    )
    .await;
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
    snap.reconcile().expect("reconcile should succeed");

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/changes/diff?sessionId=sess-session-path-diff&workspaceId=workspace-stale-query&workspacePath={}&filePath=alpha.txt",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["sessionId"], "sess-session-path-diff");
    assert_eq!(payload["workspaceId"], "ws-session-path-diff");
    assert_eq!(
        std::fs::canonicalize(payload["workspacePath"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&root).unwrap()
    );
    assert!(
        payload["diff"]
            .as_str()
            .is_some_and(|diff| diff.contains("+alpha changed")),
        "session diff must use the workspace resolved from workspacePath"
    );
}

#[tokio::test]
async fn get_diff_lazily_starts_snapshot_session_for_bound_session() {
    let root = unique_temp_dir("magi-changes-route-lazy-snapshot");
    fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
    let state = build_state_with_workspace_root(&root, "ws-lazy-snapshot");
    let session_id = SessionId::new("sess-lazy-snapshot");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "lazy snapshot",
            Some("ws-lazy-snapshot".to_string()),
        )
        .expect("session should create");
    assert!(
        state.snapshot_session(&session_id, &root).is_none(),
        "测试前不应手动启动快照账本"
    );

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/changes/diff?sessionId=sess-lazy-snapshot&workspaceId=ws-lazy-snapshot")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["pendingChangesState"]["status"], "ready");
    assert_eq!(payload["pendingChangesState"]["pendingCount"], 0);
    assert!(
        state.snapshot_session(&session_id, &root).is_some(),
        "显式变更路由必须在账本缺失时按会话/工作区启动快照账本"
    );
}

#[tokio::test]
async fn approve_all_clears_pending_changes() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-approve-all");
    fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
    let snap =
        register_workspace_and_snapshot(&state, "ws-approve-all", "sess-approve-all", &root, None)
            .await;
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
    snap.reconcile().expect("reconcile should succeed");

    let before_projection = collect_session_pending_changes_with_state(
        &state,
        &SessionId::new("sess-approve-all"),
        Some("ws-approve-all"),
    )
    .expect("pending changes should collect before approval");
    let before = before_projection.pending_changes;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].file_path, "alpha.txt");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/changes/approve-all")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "sessionId": "sess-approve-all",
                        "workspaceId": "ws-approve-all"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    assert_eq!(payload["workspaceId"], "ws-approve-all");
    assert!(
        payload["workspacePath"]
            .as_str()
            .is_some_and(|path| path.contains("magi-changes-route-approve-all")),
        "approve payload must carry canonical workspace path"
    );
    assert_eq!(payload["sessionId"], "sess-approve-all");
    assert_eq!(payload["executionGroupId"], "session:sess-approve-all");

    let after_projection = collect_session_pending_changes_with_state(
        &state,
        &SessionId::new("sess-approve-all"),
        Some("ws-approve-all"),
    )
    .expect("pending changes should collect after approval");
    let after = after_projection.pending_changes;
    assert!(
        after.is_empty(),
        "approved files should disappear from pending changes"
    );
}

#[tokio::test]
async fn revert_all_removes_added_file() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-revert-all");
    let snap =
        register_workspace_and_snapshot(&state, "ws-revert-all", "sess-revert-all", &root, None)
            .await;
    fs::create_dir_all(root.join("tmp")).expect("tmp dir should create");
    let ctx = ToolHookCtx {
        tool_call_id: "call-route-revert-all".into(),
        worker_id: None,
        execution_group_id: Some("route-revert-all".into()),
        declared_paths: vec![PathBuf::from("tmp/added.txt")],
    };
    snap.before_tool(&ctx);
    fs::write(root.join("tmp/added.txt"), "new file\n").expect("added file should write");
    snap.after_tool(&ctx);

    let before_projection = collect_session_pending_changes_with_state(
        &state,
        &SessionId::new("sess-revert-all"),
        Some("ws-revert-all"),
    )
    .expect("pending changes should collect");
    let before = before_projection.pending_changes;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].file_path, "tmp/added.txt");
    assert_eq!(before[0].r#type, "add");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/changes/revert-all")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "sessionId": "sess-revert-all",
                        "workspaceId": "ws-revert-all"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(!root.join("tmp/added.txt").exists());
}

#[tokio::test]
async fn revert_change_restores_modified_file_to_baseline() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-revert-single");
    fs::write(root.join("alpha.txt"), "original\n").expect("alpha should write");
    let snap = register_workspace_and_snapshot(
        &state,
        "ws-revert-single",
        "sess-revert-single",
        &root,
        None,
    )
    .await;
    let ctx = ToolHookCtx {
        tool_call_id: "call-route-revert-single".into(),
        worker_id: None,
        execution_group_id: Some("route-revert-single".into()),
        declared_paths: vec![PathBuf::from("alpha.txt")],
    };
    snap.before_tool(&ctx);
    fs::write(root.join("alpha.txt"), "modified\n").expect("alpha modify");
    snap.after_tool(&ctx);

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/changes/revert")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "sessionId": "sess-revert-single",
                        "workspaceId": "ws-revert-single",
                        "filePath": "alpha.txt"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let restored = fs::read_to_string(root.join("alpha.txt")).expect("alpha should read");
    assert_eq!(restored, "original\n");
}

#[tokio::test]
async fn revert_execution_group_rejects_cross_session_mission() {
    let state = build_state();
    let root = unique_temp_dir("magi-changes-route-revert-group");
    fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
    let _snap = register_workspace_and_snapshot(
        &state,
        "ws-revert-group",
        "sess-revert-group",
        &root,
        Some("mission-a"),
    )
    .await;

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/changes/revert-execution-group")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "sessionId": "sess-revert-group",
                        "workspaceId": "ws-revert-group",
                        "executionGroupId": "mission-b"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    let message = payload["message"].as_str().unwrap_or_default();
    assert!(message.contains("不属于当前会话"));
}

#[tokio::test]
async fn get_file_content_accepts_absolute_path_within_workspace() {
    let root = unique_temp_dir("magi-changes-route-content-inside");
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha should write");
    let absolute_path = root.join("alpha.txt").to_string_lossy().into_owned();
    let state = build_state_with_workspace_root(&root, "workspace-absolute-content");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/files/content?workspaceId=workspace-absolute-content&filePath={}",
                    absolute_path
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "absolute path should be accepted"
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    assert_eq!(payload["content"], "alpha changed\n");
}

#[tokio::test]
async fn first_file_preview_accepts_path_refs_without_session_bootstrap() {
    let root = unique_temp_dir("magi-first-path-ref-preview");
    let file = root.join("README.md");
    fs::write(&file, "first preview works\n").expect("preview file should write");
    let state = build_state();
    state
        .workspace_registry
        .register_native_path(
            WorkspaceId::new("workspace-first-path-ref-preview"),
            root.clone(),
        )
        .expect("native workspace should register");
    let workspace_path_ref = magi_core::HostPath::from_path(root.clone())
        .to_path_ref()
        .as_str()
        .to_string();
    let file_path_ref = magi_core::HostPath::from_path(file)
        .to_path_ref()
        .as_str()
        .to_string();

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/content?workspaceId=workspace-first-path-ref-preview&workspacePath={}&filePath={}",
                        urlencoding::encode(&workspace_path_ref),
                        urlencoding::encode(&file_path_ref),
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["content"], "first preview works\n");
    assert_eq!(payload["workspaceId"], "workspace-first-path-ref-preview");
}

#[tokio::test]
async fn get_file_content_accepts_absolute_path_inside_managed_skill_cache() {
    let workspace_root = unique_temp_dir("magi-changes-route-skill-preview-workspace");
    let state_root = unique_temp_dir("magi-changes-route-skill-preview-state");
    let skill_root = state_root
        .join("skills_cache")
        .join("owner%2Frepo")
        .join("skills")
        .join("browser");
    fs::create_dir_all(&skill_root).expect("skill root should create");
    let skill_file = skill_root.join("SKILL.md");
    fs::write(&skill_file, "# Browser Skill\n").expect("skill file should write");
    let state = build_state_with_workspace_root(&workspace_root, "workspace-managed-skill-content")
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            state_root.clone(),
            state_root.join("workspaces.json"),
            state_root.join("knowledge.json"),
        )));

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/files/content?workspaceId=workspace-managed-skill-content&filePath={}",
                    urlencoding::encode(skill_file.to_string_lossy().as_ref())
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["content"], "# Browser Skill\n");
}

#[tokio::test]
async fn get_file_content_resolves_workspace_from_registered_path_when_query_id_is_stale() {
    let root = unique_temp_dir("magi-changes-route-content-path-binding");
    fs::write(root.join("alpha.txt"), "alpha from bound path\n").expect("alpha should write");
    let state = build_state_with_workspace_root(&root, "workspace-content-path");

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/content?workspaceId=workspace-stale-query&workspacePath={}&filePath=alpha.txt",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["workspaceId"], "workspace-content-path");
    assert_eq!(
        payload["workspacePath"],
        root.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(payload["content"], "alpha from bound path\n");
}

#[tokio::test]
async fn get_file_content_rejects_workspace_mismatched_session_scope() {
    let root_a = unique_temp_dir("magi-changes-route-content-session-a");
    let root_b = unique_temp_dir("magi-changes-route-content-session-b");
    fs::write(root_a.join("alpha.txt"), "alpha from workspace a\n").expect("alpha should write");
    fs::write(root_b.join("alpha.txt"), "alpha from workspace b\n").expect("alpha should write");
    let state = build_state();
    state
        .workspace_registry
        .register(
            WorkspaceId::new("workspace-content-a"),
            AbsolutePath::new(root_a.to_string_lossy().as_ref()),
        )
        .expect("workspace a should register");
    state
        .workspace_registry
        .register(
            WorkspaceId::new("workspace-content-b"),
            AbsolutePath::new(root_b.to_string_lossy().as_ref()),
        )
        .expect("workspace b should register");
    state
        .session_store
        .create_session_for_workspace(
            SessionId::new("session-content-a"),
            "session-content-a",
            Some("workspace-content-a".to_string()),
        )
        .expect("session should create");

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/files/content?sessionId=session-content-a&workspaceId=workspace-content-b&filePath=alpha.txt")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    let message = payload["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("不属于 workspace workspace-content-b"),
        "file preview must reject mismatched session/workspace scope: {message}"
    );
}

#[tokio::test]
async fn get_file_content_rejects_absolute_path_outside_workspace() {
    let root = unique_temp_dir("magi-changes-route-content-inside-2");
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha should write");
    let outside_dir = unique_temp_dir("magi-changes-route-content-outside");
    let outside_file = outside_dir.join("secret.txt");
    fs::write(&outside_file, "off-limits\n").expect("outside file should write");
    let outside_path = outside_file.to_string_lossy().into_owned();
    let state = build_state_with_workspace_root(&root, "workspace-outside-content");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/files/content?workspaceId=workspace-outside-content&filePath={}",
                    outside_path
                ))
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should deserialize");
    let message = payload["message"].as_str().unwrap_or_default();
    assert!(message.contains("路径越出工作区边界"));
}

#[tokio::test]
async fn get_file_content_rejects_missing_workspace_without_session() {
    let root = unique_temp_dir("magi-changes-route-content-missing-workspace");
    fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha should write");
    let state = build_state_with_workspace_root(&root, "workspace-content-missing-scope");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/content?filePath=alpha.txt")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    let message = payload["message"].as_str().unwrap_or_default();
    assert!(message.contains("workspaceId 不能为空"));
}

#[tokio::test]
async fn get_file_content_uses_public_unreadable_file_error() {
    let root = unique_temp_dir("magi-changes-route-content-binary");
    fs::write(root.join("binary.bin"), [0xff, 0xfe, 0xfd]).expect("binary should write");
    let state = build_state_with_workspace_root(&root, "workspace-content-binary");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/content?workspaceId=workspace-content-binary&filePath=binary.bin")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = read_json_response(response).await;
    assert_eq!(payload["message"], "文件不可读取或不存在");
    let text = payload.to_string();
    assert!(
        !text.contains("invalid utf-8") && !text.contains("stream did not contain valid UTF-8"),
        "file read detail should stay out of response: {text}"
    );
}

#[tokio::test]
async fn get_file_raw_requires_workspace_and_serves_workspace_image() {
    let root = unique_temp_dir("magi-changes-route-raw-image");
    fs::write(root.join("image.png"), b"not-a-real-png").expect("image should write");
    let state = build_state_with_workspace_root(&root, "workspace-raw-image");

    let missing_scope = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/files/raw?filePath=image.png")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(missing_scope.status(), StatusCode::BAD_REQUEST);

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/raw?workspaceId=workspace-raw-image&filePath=image.png")
                .method("GET")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(axum::http::header::CONTENT_TYPE),
        Some(&axum::http::HeaderValue::from_static("image/png"))
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    assert_eq!(&body[..], b"not-a-real-png");
}

#[tokio::test]
async fn get_file_raw_resolves_workspace_from_registered_path_when_query_id_is_stale() {
    let root = unique_temp_dir("magi-changes-route-raw-path-image");
    fs::write(root.join("image.png"), b"not-a-real-png").expect("image should write");
    let state = build_state_with_workspace_root(&root, "workspace-raw-path-image");

    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/raw?workspaceId=workspace-stale-query&workspacePath={}&filePath=image.png",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(axum::http::header::CONTENT_TYPE),
        Some(&axum::http::HeaderValue::from_static("image/png"))
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    assert_eq!(&body[..], b"not-a-real-png");
}

#[tokio::test]
async fn svg_preview_preserves_vectors_and_isolates_direct_navigation() {
    let root = unique_temp_dir("magi-raw-svg");
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><style>rect{fill:blue}</style><rect width="32" height="32"/></svg>"##;
    fs::write(root.join("drawing.SVG"), svg).unwrap();
    let state = build_state_with_workspace_root(&root, "workspace-svg");
    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/files/raw?workspaceId=workspace-svg&filePath=drawing.SVG")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        magi_media::IMAGE_CONTENT_SECURITY_POLICY
    );
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        svg
    );
}
