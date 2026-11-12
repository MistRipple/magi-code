use super::*;
use crate::state::ApiState;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_core::{AbsolutePath, ExecutionOwnership, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_session_store::{
    SessionDurableState, SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState,
    SessionRecord, SessionRuntimeSidecarState, SessionStore, TimelineEntryKind,
};
use magi_workspace::WorkspaceStore;
use std::{fs, sync::Arc, time::Duration};
use tower::ServiceExt;

fn test_state() -> ApiState {
    ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
}

#[tokio::test]
async fn workspace_registration_write_failure_does_not_publish_registration() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(&source).unwrap();
    let blocked = root.path().join("blocked");
    fs::write(&blocked, b"file blocks registry directory").unwrap();
    let state = test_state().with_runtime_persistence(Arc::new(
        crate::state::RuntimeStatePersistence::new(
            root.path().join("state"),
            blocked.join("workspaces.json"),
            root.path().join("knowledge.json"),
        ),
    ));
    let events_before = state.event_bus.snapshot().next_sequence;
    assert!(
        register_workspace(
            State(state.clone()),
            Json(RegisterWorkspaceRequest {
                path: source.to_string_lossy().into(),
            })
        )
        .await
        .is_err()
    );
    assert!(state.workspace_registry.is_empty());
    assert!(state.workspace_registry.active_workspace_id().is_none());
    assert_eq!(state.event_bus.snapshot().next_sequence, events_before);
    // 失败后的重试复用原目录稳定身份，不能分配另一个项目并遗失原历史。
    let identity =
        magi_workspace::resolve_or_create_workspace_identity(&source, WorkspaceId::new("unused"))
            .unwrap();
    fs::remove_file(&blocked).unwrap();
    let _ = register_workspace(
        State(state.clone()),
        Json(RegisterWorkspaceRequest {
            path: source.to_string_lossy().into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        state.workspace_registry.workspaces()[0].workspace_id,
        identity
    );
}

#[tokio::test]
async fn legacy_registration_cannot_reinterpret_a_multi_directory_project() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let state = test_state();
    let id = WorkspaceId::new("multi-project");
    let project = state
        .workspace_registry
        .register_native_path(id.clone(), a)
        .unwrap();
    let mut second = project.primary_source_folder().clone();
    let host = magi_core::HostPath::from_path(b.clone());
    second.source_folder_id = "second".into();
    second.path = AbsolutePath::new(host.display_string());
    second.path_ref = host.to_path_ref().as_str().into();
    let mut folders = project.source_folders;
    folders.push(second);
    state
        .workspace_registry
        .edit_project(
            &id,
            1,
            magi_workspace::WorkspaceProjectEdit {
                name: None,
                source_folders: folders,
                primary_source_folder_id: project.primary_source_folder_id,
            },
            |_| Ok(()),
        )
        .unwrap();
    let events_before = state.event_bus.snapshot().next_sequence;
    let result = register_workspace(
        State(state.clone()),
        Json(RegisterWorkspaceRequest {
            path: b.to_string_lossy().into(),
        }),
    )
    .await;
    assert!(matches!(result, Err(ApiError::Conflict(_))));
    assert_eq!(state.workspace_registry.workspaces().len(), 1);
    assert_eq!(state.event_bus.snapshot().next_sequence, events_before);
}

#[tokio::test]
async fn remove_workspace_checkpoints_before_deregister_and_clears_only_its_selection() {
    for fail in [false, true] {
        for selected_here in [false, true] {
            let mut state = test_state();
            let workspace = WorkspaceId::new("remove-checkpoint");
            state
                .workspace_registry
                .register(
                    workspace.clone(),
                    AbsolutePath::new("/tmp/magi-remove-checkpoint"),
                )
                .unwrap();
            let session = SessionId::new("remove-session");
            state
                .session_store
                .create_session_for_workspace(
                    session.clone(),
                    "history",
                    Some(workspace.to_string()),
                )
                .unwrap();
            if !selected_here {
                state
                    .session_store
                    .create_session(SessionId::new("other-current"), "other")
                    .unwrap();
            }
            let previous = state.session_store.current_session_id();
            let registry = state.workspace_registry.clone();
            let expected = workspace.clone();
            state = state.with_session_state_checkpoint_persist(Arc::new(move |_| {
                assert!(
                    registry
                        .workspaces()
                        .iter()
                        .any(|entry| entry.workspace_id == expected)
                );
                if fail {
                    Err(ApiError::Conflict("checkpoint failed".into()))
                } else {
                    Ok(true)
                }
            }));
            let result = remove_workspace(
                State(state.clone()),
                Json(RemoveWorkspaceRequest {
                    workspace_id: workspace.to_string(),
                }),
            )
            .await;
            assert_eq!(result.is_err(), fail);
            assert_eq!(state.workspace_registry.workspaces().is_empty(), !fail);
            assert_eq!(
                state.session_store.current_session_id(),
                if !fail && selected_here {
                    None
                } else {
                    previous
                }
            );
            assert!(
                state.session_store.session(&session).is_some(),
                "移除工作区不能删除会话历史"
            );
        }
    }
}

async fn read_json_response(response: axum::response::Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    serde_json::from_slice(&body).expect("payload should deserialize")
}

#[test]
fn new_workspace_id_keeps_same_millisecond_registrations_unique() {
    let first = new_workspace_id();
    let second = new_workspace_id();

    assert_ne!(first, second);
    assert!(first.as_str().starts_with("workspace-"));
    assert!(second.as_str().starts_with("workspace-"));
}

#[test]
fn canonical_workspace_path_uses_public_error_messages() {
    let missing =
        std::env::temp_dir().join(format!("magi-workspace-missing-{}", UtcMillis::now().0));
    let error =
        canonical_workspace_path(&missing.to_string_lossy()).expect_err("missing path should fail");
    assert_eq!(error.message(), "工作区路径不可访问");
    assert!(!error.message().contains("os error"));
    assert!(!error.message().contains("No such"));

    let file_path =
        std::env::temp_dir().join(format!("magi-workspace-file-{}", UtcMillis::now().0));
    fs::write(&file_path, "not a directory").expect("test file should write");
    let error =
        canonical_workspace_path(&file_path.to_string_lossy()).expect_err("file path should fail");
    assert_eq!(error.message(), "工作区路径必须是目录");
    let file_path_text = file_path.to_string_lossy().to_string();
    assert!(!error.message().contains(&file_path_text));
    let _ = fs::remove_file(file_path);
}

#[test]
fn canonical_workspace_path_accepts_host_path_ref() {
    let root = std::env::temp_dir().join(format!("magi-workspace-path-ref-{}", UtcMillis::now().0));
    fs::create_dir_all(&root).expect("workspace dir should create");
    let path_ref = magi_core::HostPath::from_path(root.clone())
        .to_path_ref()
        .as_str()
        .to_string();

    let resolved = canonical_workspace_path(&path_ref).expect("workspace path ref should resolve");

    assert_eq!(
        resolved,
        magi_core::HostPath::canonicalize(&root)
            .unwrap()
            .into_path_buf()
    );
}

#[tokio::test]
async fn register_workspace_ingests_workspace_code_index() {
    let state = test_state();
    let root = std::env::temp_dir().join(format!(
        "magi-register-workspace-index-{}",
        UtcMillis::now().0
    ));
    fs::create_dir_all(root.join("src")).expect("workspace dir should create");
    fs::write(root.join("src/index.ts"), "export const value = 1;\n")
        .expect("source file should write");

    let body = serde_json::json!({ "path": root.to_string_lossy() }).to_string();
    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspaces/register")
                .header("content-type", "application/json")
                .body(Body::from(body))
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
    let workspace_id = WorkspaceId::new(
        payload["workspaceId"]
            .as_str()
            .expect("workspaceId should exist"),
    );
    // 索引在 spawn_blocking 中异步构建，轮询等待完成。
    let code_index = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(summary) = state
                .knowledge_store
                .code_index_summary_for_workspace(&workspace_id)
            {
                return summary;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("code index should be built within timeout");

    assert!(
        code_index
            .files
            .iter()
            .any(|file| file.path == "src/index.ts"),
        "workspace code index should include the registered workspace files"
    );
}

#[tokio::test]
async fn register_workspace_reuses_existing_workspace_for_same_canonical_path() {
    let state = test_state();
    let root = std::env::temp_dir().join(format!(
        "magi-register-workspace-reuse-{}",
        UtcMillis::now().0
    ));
    fs::create_dir_all(root.join("src")).expect("workspace dir should create");
    fs::write(root.join("src/lib.rs"), "pub fn value() -> i32 { 1 }\n")
        .expect("source file should write");

    let first_body = serde_json::json!({ "path": root.to_string_lossy() }).to_string();
    let first_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspaces/register")
                .header("content-type", "application/json")
                .body(Body::from(first_body))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = to_bytes(first_response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let first_payload: serde_json::Value =
        serde_json::from_slice(&first_body).expect("payload should deserialize");
    assert_eq!(first_payload["registered"], true);

    let second_body = serde_json::json!({ "path": root.join(".").to_string_lossy() }).to_string();
    let second_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspaces/register")
                .header("content-type", "application/json")
                .body(Body::from(second_body))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = to_bytes(second_response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let second_payload: serde_json::Value =
        serde_json::from_slice(&second_body).expect("payload should deserialize");

    assert_eq!(second_payload["registered"], false);
    assert_eq!(second_payload["reused"], true);
    assert_eq!(second_payload["workspaceId"], first_payload["workspaceId"]);
    assert_eq!(state.workspace_registry.workspaces().len(), 1);
}

#[tokio::test]
async fn register_workspace_adopts_existing_project_identity() {
    let state = test_state();
    let root = tempfile::tempdir().expect("workspace dir");
    let projection_root = root.path().join(".magi").join("session-projections");
    fs::create_dir_all(&projection_root).expect("projection root");
    fs::write(
        projection_root.join("session-existing.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "canonical_event_seq": 0,
            "durable": {
                "sessions": [{
                    "sessionId": "session-existing",
                    "workspaceId": "workspace-existing"
                }]
            }
        }))
        .expect("projection json"),
    )
    .expect("projection write");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspaces/register")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "path": root.path().to_string_lossy() }).to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["workspaceId"], "workspace-existing");
    assert_eq!(
        state.workspace_registry.workspaces()[0]
            .workspace_id
            .as_str(),
        "workspace-existing"
    );
    let identity: serde_json::Value = serde_json::from_slice(
        &fs::read(root.path().join(".magi/workspace-identity.json")).expect("identity manifest"),
    )
    .expect("identity json");
    assert_eq!(identity["workspaceId"], "workspace-existing");
}

#[tokio::test]
async fn workspace_sessions_returns_user_message_count_and_updated_at() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-count");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-count"),
        )
        .expect("workspace should register");

    let session_id = SessionId::new("session-counted");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "会话计数",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.bind_execution_ownership_for_test(
        session_id.clone(),
        ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            ..ExecutionOwnership::default()
        },
    );
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "第一条用户消息",
    );
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::AssistantMessage,
        "助手回复不应计入会话消息数",
    );
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "第二条用户消息",
    );

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-count")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert!(
        payload["runtimeEpoch"]
            .as_str()
            .is_some_and(|runtime_epoch| !runtime_epoch.is_empty())
    );
    assert!(
        payload["eventStreamNextSequence"]
            .as_u64()
            .is_some_and(|sequence| sequence >= 1)
    );
    assert_eq!(payload["workspace"]["workspaceId"], "workspace-count");
    assert!(payload.get("sessionId").is_none());
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["sessionId"], "session-counted");
    assert_eq!(sessions[0]["workspaceId"], "workspace-count");
    assert_eq!(sessions[0]["messageCount"], 2);
    assert_eq!(sessions[0]["isRunning"], false);
    assert_eq!(sessions[0]["runningTaskCount"], 0);
    assert!(sessions[0]["updatedAt"].as_u64().unwrap_or_default() > 0);
}

#[tokio::test]
async fn workspace_sessions_reports_unread_completion_without_clearing_it() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-unread-completion");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-unread-completion"),
        )
        .expect("workspace should register");
    let session_id = SessionId::new("session-unread-completion");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "未读完成会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "触发未读完成",
    );
    let coordinator = state.turn_coordinator().clone();
    crate::routes::test_turn_fixtures::seed_conversation_turn(
        &state.session_store,
        &coordinator,
        &session_id,
        "turn-unread-completion",
        1,
        UtcMillis(10),
        "completed",
        "触发未读完成",
    );

    for _ in 0..2 {
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/workspaces/sessions?workspaceId=workspace-unread-completion")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["sessions"][0]["hasUnreadCompletion"], true);
    }

    assert!(
        state
            .session_store
            .session(&session_id)
            .expect("session should exist")
            .has_unread_completion()
    );
}

#[tokio::test]
async fn mark_session_viewed_clears_unread_completion() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-mark-viewed");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-mark-viewed"),
        )
        .expect("workspace should register");
    let session_id = SessionId::new("session-mark-viewed");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "已查看完成会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    let coordinator = state.turn_coordinator().clone();
    crate::routes::test_turn_fixtures::seed_conversation_turn(
        &state.session_store,
        &coordinator,
        &session_id,
        "turn-mark-viewed",
        1,
        UtcMillis(10),
        "completed",
        "触发已查看",
    );

    let response = crate::routes::build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/viewed")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceId": workspace_id.as_str(),
                        "sessionId": session_id.as_str(),
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["sessionId"], session_id.as_str());
    assert_eq!(payload["hasUnreadCompletion"], false);
    assert!(
        !state
            .session_store
            .session(&session_id)
            .expect("session should exist")
            .has_unread_completion()
    );
    assert!(
        state
            .event_bus
            .snapshot()
            .recent_events
            .iter()
            .any(|event| event.event_type == "session.viewed"),
        "标记已查看必须广播事件，让其他客户端同步清除未读状态"
    );
}

#[tokio::test]
async fn mark_session_viewed_rejects_workspace_mismatch() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-viewed-owner");
    let foreign_workspace_id = WorkspaceId::new("workspace-viewed-foreign");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-viewed-owner"),
        )
        .expect("workspace should register");
    state
        .workspace_registry
        .register(
            foreign_workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-viewed-foreign"),
        )
        .expect("foreign workspace should register");
    let session_id = SessionId::new("session-viewed-owner");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "归属会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");

    let response = crate::routes::build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/viewed")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "workspaceId": foreign_workspace_id.as_str(),
                        "sessionId": session_id.as_str(),
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn workspace_sessions_reports_active_and_interrupted_turn_states() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-running-session");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-running-session"),
        )
        .expect("workspace should register");

    let session_id = SessionId::new("session-running");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "运行中的会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "触发运行态",
    );
    let coordinator = state.turn_coordinator().clone();
    crate::routes::test_turn_fixtures::seed_conversation_turn(
        &state.session_store,
        &coordinator,
        &session_id,
        "turn-running",
        1,
        UtcMillis::now(),
        "running",
        "触发运行态",
    );

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-running-session")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["sessionId"], "session-running");
    assert_eq!(sessions[0]["isRunning"], true);
    assert_eq!(sessions[0]["runningTaskCount"], 1);

    state
        .turn_event_sink()
        .interrupt_turn_by_user(&session_id)
        .expect("current turn should be interruptible");
    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-running-session")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions[0]["isRunning"], false);
    assert_eq!(sessions[0]["runningTaskCount"], 0);
}

#[tokio::test]
async fn workspace_sessions_resolves_workspace_from_registered_path_when_query_id_is_stale() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-session-path-binding");
    let root = std::env::temp_dir().join(format!(
        "magi-workspace-session-path-binding-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    fs::create_dir_all(&root).expect("workspace root should create");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new(root.to_string_lossy().as_ref()),
        )
        .expect("workspace should register");

    let session_id = SessionId::new("session-path-bound");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "路径绑定会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "真实用户消息",
    );

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/workspaces/sessions?workspaceId=workspace-stale-query&workspacePath={}",
                    urlencoding::encode(root.to_string_lossy().as_ref())
                ))
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json_response(response).await;
    assert_eq!(payload["workspace"]["workspaceId"], workspace_id.as_str());
    assert_eq!(
        payload["workspace"]["rootPath"],
        root.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert!(payload.get("sessionId").is_none());
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["sessionId"], "session-path-bound");
}

#[tokio::test]
async fn workspace_sessions_requires_registered_workspace_scope() {
    let state = test_state();
    state
        .workspace_registry
        .register(
            WorkspaceId::new("workspace-required-scope"),
            AbsolutePath::new("/tmp/magi-workspace-required-scope"),
        )
        .expect("workspace should register");
    state
        .session_store
        .create_session_for_workspace(
            SessionId::new("session-required-scope"),
            "不应无 scope 出现在列表里",
            Some("workspace-required-scope".to_string()),
        )
        .expect("session should create");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions")
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

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-missing")
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
            .contains("workspace 不存在")
    );
}

#[tokio::test]
async fn workspace_sessions_includes_workspace_bound_session_without_runtime_sidecar() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-session-list");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-session-list"),
        )
        .expect("workspace should register");

    let session_id = SessionId::new("session-workspace-bound");
    state
        .session_store
        .create_session_for_workspace(
            session_id.clone(),
            "仅绑定工作区的会话",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        "真实用户消息",
    );

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-session-list")
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
    assert!(payload.get("sessionId").is_none());
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["sessionId"], "session-workspace-bound");
}

#[tokio::test]
async fn workspace_sessions_excludes_sidecar_only_workspace_binding() {
    let workspace_a_id = WorkspaceId::new("workspace-strict-a");
    let workspace_b_id = WorkspaceId::new("workspace-strict-b");
    let session_id = SessionId::new("session-workspace-a");
    let now = UtcMillis::now();
    let durable_state = SessionDurableState {
        current_session_id: Some(session_id.clone()),
        sessions: vec![SessionRecord {
            session_id: session_id.clone(),
            title: "归属工作区 A 的会话".to_string(),
            status: magi_core::SessionLifecycleStatus::Active,
            created_at: now,
            updated_at: now,
            message_count: None,
            workspace_id: Some(workspace_a_id.to_string()),
            last_completed_at: None,
            last_viewed_at: None,
            kind: Default::default(),
        }],
        timeline: Vec::new(),
        canonical_turns: Vec::new(),
        inputs: Vec::new(),
        notifications: Vec::new(),
        goals: Vec::new(),
        plans: Vec::new(),
        thread_context_checkpoints: Vec::new(),
        thread_registry: Vec::new(),
    };
    let sidecar_state = SessionExecutionSidecarStoreState {
        runtime_sidecars: vec![SessionRuntimeSidecarState {
            session_id: session_id.clone(),
            ownership: ExecutionOwnership {
                workspace_id: Some(workspace_b_id.clone()),
                ..ExecutionOwnership::default()
            },
            recovery_id: None,
            current_turn_id: None,
            active_execution_chain: None,
            status: SessionExecutionSidecarStatus::Bound,
            updated_at: now,
        }],
    };
    let state = ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(
            SessionStore::from_persisted_parts(durable_state, sidecar_state)
                .expect("workspace binding persisted state should restore"),
        ),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    );
    state
        .workspace_registry
        .register(
            workspace_a_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-strict-a"),
        )
        .expect("workspace a should register");
    state
        .workspace_registry
        .register(
            workspace_b_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-strict-b"),
        )
        .expect("workspace b should register");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-strict-b")
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
    assert!(payload.get("sessionId").is_none());
    assert!(
        payload["sessions"]
            .as_array()
            .expect("sessions should be an array")
            .is_empty()
    );
}

#[tokio::test]
async fn workspace_sessions_returns_current_session_and_sorts_by_updated_at_desc() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-session-order");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-session-order"),
        )
        .expect("workspace should register");

    let older_session_id = SessionId::new("session-older");
    state
        .session_store
        .create_session_for_workspace(
            older_session_id.clone(),
            "较早会话",
            Some(workspace_id.to_string()),
        )
        .expect("older session should create");
    state.session_store.append_timeline_entry(
        older_session_id.clone(),
        TimelineEntryKind::UserMessage,
        "较早会话用户消息",
    );
    tokio::time::sleep(Duration::from_millis(2)).await;

    let newer_session_id = SessionId::new("session-newer");
    state
        .session_store
        .create_session_for_workspace(
            newer_session_id.clone(),
            "较新会话",
            Some(workspace_id.to_string()),
        )
        .expect("newer session should create");
    tokio::time::sleep(Duration::from_millis(2)).await;
    state.session_store.append_timeline_entry(
        newer_session_id.clone(),
        TimelineEntryKind::UserMessage,
        "刷新较新会话更新时间",
    );

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-session-order")
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
    assert!(payload.get("sessionId").is_none());
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["sessionId"], "session-newer");
    assert_eq!(sessions[1]["sessionId"], "session-older");
}

#[tokio::test]
async fn workspace_sessions_does_not_select_session_without_explicit_navigation() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-default-latest");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-default-latest"),
        )
        .expect("workspace should register");
    let older_session_id = SessionId::new("session-default-older");
    state
        .session_store
        .create_session_for_workspace(
            older_session_id.clone(),
            "较早历史",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.append_timeline_entry(
        older_session_id,
        TimelineEntryKind::UserMessage,
        "较早消息",
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    let newer_session_id = SessionId::new("session-default-newer");
    state
        .session_store
        .create_session_for_workspace(
            newer_session_id.clone(),
            "较新历史",
            Some(workspace_id.to_string()),
        )
        .expect("session should create");
    state.session_store.append_timeline_entry(
        newer_session_id,
        TimelineEntryKind::UserMessage,
        "较新消息",
    );

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-default-latest")
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
    assert!(payload.get("sessionId").is_none());
    assert_eq!(
        payload["sessions"]
            .as_array()
            .expect("sessions should be an array")
            .len(),
        2
    );
}

#[tokio::test]
async fn workspace_sessions_hides_empty_sessions_from_history() {
    let state = test_state();
    let workspace_id = WorkspaceId::new("workspace-empty-session-history");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new("/tmp/magi-workspace-empty-session-history"),
        )
        .expect("workspace should register");
    state
        .session_store
        .create_session_for_workspace(
            SessionId::new("session-empty-history"),
            "空白会话",
            Some(workspace_id.to_string()),
        )
        .expect("empty session should create");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/workspaces/sessions?workspaceId=workspace-empty-session-history")
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
    assert!(payload.get("sessionId").is_none());
    assert!(
        payload["sessions"]
            .as_array()
            .expect("sessions should be an array")
            .is_empty()
    );
}
