use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{EventId, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::session_scope::{require_registered_workspace_binding, require_session_workspace_scope};
use crate::{errors::ApiError, state::ApiState};

static WORKSPACE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
static SESSION_VIEW_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces/register", post(register_workspace))
        .route("/workspaces/remove", post(remove_workspace))
        .route("/workspaces/pick", get(pick_workspace))
        .route("/workspaces/sessions", get(workspace_sessions))
        .route(
            "/workspaces/sessions/viewed",
            post(mark_workspace_session_viewed),
        )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceDto {
    workspace_id: String,
    root_path: String,
    root_path_ref: Option<String>,
    name: Option<String>,
    is_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceListResponse {
    workspaces: Vec<WorkspaceDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSessionDto {
    session_id: String,
    workspace_id: String,
    title: String,
    status: String,
    created_at: u64,
    updated_at: u64,
    message_count: usize,
    is_running: bool,
    running_task_count: usize,
    has_unread_completion: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSessionsResponse {
    workspace: WorkspaceDto,
    session_id: String,
    sessions: Vec<WorkspaceSessionDto>,
}

async fn list_workspaces(State(state): State<ApiState>) -> Json<WorkspaceListResponse> {
    let workspaces = state
        .workspace_registry
        .workspaces()
        .into_iter()
        .map(|w| {
            let active_id = state.workspace_registry.active_workspace_id();
            WorkspaceDto {
                workspace_id: w.workspace_id.to_string(),
                root_path: w.root_path.to_string(),
                root_path_ref: w.root_path_ref.clone(),
                name: w.name.clone(),
                is_active: active_id.as_ref() == Some(&w.workspace_id),
            }
        })
        .collect();
    Json(WorkspaceListResponse { workspaces })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterWorkspaceRequest {
    path: String,
}

async fn register_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RegisterWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let canonical_path = canonical_workspace_path(&request.path)?;

    // 已注册过的 workspace：复用已有记录，仍异步刷新索引。
    if let Some(workspace) = registered_workspace_for_path(&state, &canonical_path) {
        let workspace_id = workspace.workspace_id.clone();
        super::knowledge::schedule_workspace_code_index(
            state.clone(),
            workspace_id,
            canonical_path.clone(),
        );
        return Ok(Json(serde_json::json!({
            "workspaceId": workspace.workspace_id.to_string(),
            "registered": false,
            "reused": true
        })));
    }

    // 新 workspace：先同步完成注册（快），再异步构建索引。
    let workspace_id = new_workspace_id();
    match state
        .workspace_registry
        .register_native_path(workspace_id.clone(), canonical_path.clone())
    {
        Ok(_) => {}
        Err(error) => {
            // 并发竞态：另一个请求先注册了同一路径。
            if let Some(workspace) = registered_workspace_for_path(&state, &canonical_path) {
                let wid = workspace.workspace_id.clone();
                super::knowledge::schedule_workspace_code_index(
                    state.clone(),
                    wid,
                    canonical_path.clone(),
                );
                return Ok(Json(serde_json::json!({
                    "workspaceId": workspace.workspace_id.to_string(),
                    "registered": false,
                    "reused": true
                })));
            }
            return Err(ApiError::internal_assembly("工作区注册失败", error));
        }
    }

    // 先持久化 workspace 注册状态（快），索引放到后台不阻塞响应。
    state.persist_workspace_durable_state_for_api()?;
    super::knowledge::schedule_workspace_code_index(
        state.clone(),
        workspace_id.clone(),
        canonical_path.clone(),
    );
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.to_string(),
        "registered": true
    })))
}

fn new_workspace_id() -> magi_core::WorkspaceId {
    let nonce = WORKSPACE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    magi_core::WorkspaceId::new(format!(
        "workspace-{}-{nonce}",
        magi_core::UtcMillis::now().0
    ))
}

pub(crate) fn canonical_workspace_path(raw_path: &str) -> Result<PathBuf, ApiError> {
    let trimmed_path = raw_path.trim();
    if trimmed_path.is_empty() {
        return Err(ApiError::InvalidInput("工作区路径不能为空".to_string()));
    }
    let path = magi_core::HostPath::resolve_native_input(
        trimmed_path,
        std::env::current_dir().ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
    .map(magi_core::HostPath::into_path_buf)
    .map_err(|_| ApiError::InvalidInput("工作区路径不可访问".to_string()))?;
    let canonical_path = magi_core::HostPath::canonicalize(&path)
        .map(magi_core::HostPath::into_path_buf)
        .map_err(|_| ApiError::InvalidInput("工作区路径不可访问".to_string()))?;
    if !canonical_path.is_dir() {
        return Err(ApiError::InvalidInput("工作区路径必须是目录".to_string()));
    }
    Ok(canonical_path)
}

pub(crate) fn registered_workspace_for_path(
    state: &ApiState,
    canonical_path: &Path,
) -> Option<magi_workspace::WorkspaceRecord> {
    state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace_root_matches(workspace, canonical_path))
}

fn workspace_root_matches(
    workspace: &magi_workspace::WorkspaceRecord,
    canonical_path: &Path,
) -> bool {
    let stored_path = workspace.native_root_path();
    magi_core::HostPath::canonicalize(&stored_path)
        .map(magi_core::HostPath::into_path_buf)
        .map(|path| path == canonical_path)
        .unwrap_or_else(|_| stored_path == canonical_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveWorkspaceRequest {
    workspace_id: String,
}

async fn remove_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RemoveWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = magi_core::WorkspaceId::new(&request.workspace_id);
    state
        .workspace_registry
        .deregister(&workspace_id)
        .map_err(|e| ApiError::internal_assembly("工作区移除失败", e))?;
    state.persist_workspace_durable_state_for_api()?;
    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn pick_workspace(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let workspaces = state.workspace_registry.workspaces();
    let active_id = state.workspace_registry.active_workspace_id();
    Json(serde_json::json!({
        "workspaces": workspaces.iter().map(|w| serde_json::json!({
            "workspaceId": w.workspace_id.to_string(),
            "rootPath": w.root_path.to_string(),
            "rootPathRef": w.root_path_ref.clone(),
            "name": w.name.clone(),
            "isActive": active_id.as_ref() == Some(&w.workspace_id),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceSessionsQuery {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MarkWorkspaceSessionViewedRequest {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    session_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarkWorkspaceSessionViewedResponse {
    session_id: String,
    has_unread_completion: bool,
}

async fn mark_workspace_session_viewed(
    State(state): State<ApiState>,
    Json(request): Json<MarkWorkspaceSessionViewedRequest>,
) -> Result<Json<MarkWorkspaceSessionViewedResponse>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        Some(request.session_id.as_str()),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        "标记为已查看",
    )?;
    let session = state
        .session_store
        .mark_session_viewed(&scope.session_id)
        .map_err(|error| ApiError::internal_assembly("标记会话已查看失败", error))?;
    state.persist_session_state_checkpoint("session_viewed")?;
    let event_suffix = SESSION_VIEW_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-session-viewed-{}-{}-{event_suffix}",
                scope.session_id,
                UtcMillis::now().0,
            )),
            "session.viewed",
            serde_json::json!({
                "sessionId": scope.session_id.as_str(),
                "workspaceId": scope.workspace_id.as_str(),
                "hasUnreadCompletion": session.has_unread_completion(),
            }),
        )
        .with_context(EventContext {
            session_id: Some(scope.session_id.clone()),
            workspace_id: Some(scope.workspace_id.clone()),
            ..EventContext::default()
        }),
    );
    Ok(Json(MarkWorkspaceSessionViewedResponse {
        session_id: scope.session_id.to_string(),
        has_unread_completion: session.has_unread_completion(),
    }))
}

async fn workspace_sessions(
    State(state): State<ApiState>,
    Query(query): Query<WorkspaceSessionsQuery>,
) -> Result<Json<WorkspaceSessionsResponse>, ApiError> {
    let requested_workspace_id = match query
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|workspace_path| !workspace_path.is_empty())
    {
        Some(_) => {
            require_registered_workspace_binding(
                &state,
                query.workspace_id.as_deref(),
                query.workspace_path.as_deref(),
            )?
            .workspace_id
        }
        None => {
            let workspace_id = query
                .workspace_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))?;
            magi_core::WorkspaceId::new(workspace_id)
        }
    };
    let workspace = state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace.workspace_id == requested_workspace_id)
        .ok_or_else(|| {
            ApiError::InvalidInput(format!("workspace 不存在: {requested_workspace_id}"))
        })?;
    let active_id = state.workspace_registry.active_workspace_id();
    let workspace_dto = WorkspaceDto {
        workspace_id: workspace.workspace_id.to_string(),
        root_path: workspace.root_path.to_string(),
        root_path_ref: workspace.root_path_ref.clone(),
        name: workspace.name.clone(),
        is_active: active_id.as_ref() == Some(&workspace.workspace_id),
    };
    let scoped_workspace_id = workspace.workspace_id.to_string();

    let scoped_sessions = state
        .session_records_for_workspace(Some(scoped_workspace_id.as_str()))
        .iter()
        .map(|session| {
            let sidecar = state.session_store.runtime_sidecar(&session.session_id);
            let running_task_count = workspace_session_running_task_count(sidecar.as_ref());
            WorkspaceSessionDto {
                session_id: session.session_id.to_string(),
                workspace_id: scoped_workspace_id.clone(),
                title: session.title.clone(),
                status: format!("{:?}", session.status),
                created_at: session.created_at.0,
                updated_at: session.updated_at.0,
                message_count: session.message_count.unwrap_or(0),
                is_running: running_task_count > 0,
                running_task_count,
                has_unread_completion: session.has_unread_completion(),
            }
        })
        .collect::<Vec<_>>();

    let requested_session_id = query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let current_session_id = requested_session_id
        .filter(|session_id| {
            scoped_sessions
                .iter()
                .any(|session| session.session_id == *session_id)
        })
        .or_else(|| {
            scoped_sessions
                .first()
                .map(|session| session.session_id.as_str())
        })
        .unwrap_or_default()
        .to_string();

    Ok(Json(WorkspaceSessionsResponse {
        workspace: workspace_dto,
        session_id: current_session_id,
        sessions: scoped_sessions,
    }))
}

fn workspace_session_running_task_count(
    sidecar: Option<&magi_session_store::SessionRuntimeSidecar>,
) -> usize {
    let Some(turn) = sidecar.and_then(workspace_session_current_turn) else {
        return 0;
    };
    if current_turn_status_is_terminal(&turn.status) {
        return 0;
    }
    turn.items
        .iter()
        .filter(|item| {
            current_turn_item_status_is_active(&item.status)
                || item
                    .tool_status
                    .as_deref()
                    .is_some_and(current_turn_item_status_is_active)
        })
        .count()
        .max(1)
}

fn workspace_session_current_turn(
    sidecar: &magi_session_store::SessionRuntimeSidecar,
) -> Option<&magi_session_store::ActiveExecutionTurn> {
    sidecar.current_turn.as_ref().or_else(|| {
        sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
    })
}

fn current_turn_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "blocked"
            | "cancelled"
            | "canceled"
            | "killed"
    )
}

fn current_turn_item_status_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pending"
            | "queued"
            | "running"
            | "started"
            | "streaming"
            | "blocked"
            | "awaiting_approval"
            | "review_required"
            | "repairing"
            | "verifying"
    )
}

#[cfg(test)]
mod tests {
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
        ActiveExecutionTurn, SessionDurableState, SessionExecutionSidecarStatus,
        SessionExecutionSidecarStoreState, SessionRecord, SessionRuntimeSidecar, SessionStore,
        TimelineEntryKind,
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
        let error = canonical_workspace_path(&missing.to_string_lossy())
            .expect_err("missing path should fail");
        assert_eq!(error.message(), "工作区路径不可访问");
        assert!(!error.message().contains("os error"));
        assert!(!error.message().contains("No such"));

        let file_path =
            std::env::temp_dir().join(format!("magi-workspace-file-{}", UtcMillis::now().0));
        fs::write(&file_path, "not a directory").expect("test file should write");
        let error = canonical_workspace_path(&file_path.to_string_lossy())
            .expect_err("file path should fail");
        assert_eq!(error.message(), "工作区路径必须是目录");
        let file_path_text = file_path.to_string_lossy().to_string();
        assert!(!error.message().contains(&file_path_text));
        let _ = fs::remove_file(file_path);
    }

    #[test]
    fn canonical_workspace_path_accepts_host_path_ref() {
        let root =
            std::env::temp_dir().join(format!("magi-workspace-path-ref-{}", UtcMillis::now().0));
        fs::create_dir_all(&root).expect("workspace dir should create");
        let path_ref = magi_core::HostPath::from_path(root.clone())
            .to_path_ref()
            .as_str()
            .to_string();

        let resolved =
            canonical_workspace_path(&path_ref).expect("workspace path ref should resolve");

        assert_eq!(resolved, root.canonicalize().unwrap());
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

        let second_body =
            serde_json::json!({ "path": root.join(".").to_string_lossy() }).to_string();
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
        state.session_store.bind_execution_ownership(
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
                    .uri("/workspaces/sessions?workspaceId=workspace-count&sessionId=session-counted")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["workspace"]["workspaceId"], "workspace-count");
        assert_eq!(payload["sessionId"], "session-counted");
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
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-unread-completion".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(10),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("触发未读完成".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should upsert");
        state
            .session_store
            .update_current_turn_status(&session_id, "completed")
            .expect("turn should complete");

        for _ in 0..2 {
            let response = routes()
                .with_state(state.clone())
                .oneshot(
                    Request::builder()
                        .uri("/workspaces/sessions?workspaceId=workspace-unread-completion&sessionId=session-unread-completion")
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
    async fn mark_workspace_session_viewed_clears_unread_completion() {
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
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-mark-viewed".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(10),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("触发已查看".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should upsert");
        state
            .session_store
            .update_current_turn_status(&session_id, "completed")
            .expect("turn should complete");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/workspaces/sessions/viewed")
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
    async fn mark_workspace_session_viewed_rejects_workspace_mismatch() {
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

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/workspaces/sessions/viewed")
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
    async fn workspace_sessions_marks_non_terminal_current_turn_running() {
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
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("触发运行态".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("current turn should upsert");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/workspaces/sessions?workspaceId=workspace-running-session&sessionId=session-running")
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
                        "/workspaces/sessions?workspaceId=workspace-stale-query&workspacePath={}&sessionId=session-path-bound",
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
            root.to_string_lossy().as_ref()
        );
        assert_eq!(payload["sessionId"], "session-path-bound");
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
                    .uri("/workspaces/sessions?workspaceId=workspace-session-list&sessionId=session-workspace-bound")
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
        assert_eq!(payload["sessionId"], "session-workspace-bound");
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
            }],
            timeline: Vec::new(),
            canonical_turns: Vec::new(),
            notifications: Vec::new(),
            goals: Vec::new(),
            todo_lists: Vec::new(),
        };
        let sidecar_state = SessionExecutionSidecarStoreState {
            runtime_sidecars: vec![SessionRuntimeSidecar {
                session_id: session_id.clone(),
                ownership: ExecutionOwnership {
                    workspace_id: Some(workspace_b_id.clone()),
                    ..ExecutionOwnership::default()
                },
                recovery_id: None,
                current_turn: None,
                active_execution_chain: None,
                status: SessionExecutionSidecarStatus::Bound,
                updated_at: now,
            }],
        };
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::from_persisted_parts(
                durable_state,
                sidecar_state,
            )),
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
        assert_eq!(payload["sessionId"], "");
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
                    .uri("/workspaces/sessions?workspaceId=workspace-session-order&sessionId=session-newer")
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
        assert_eq!(payload["sessionId"], "session-newer");
        let sessions = payload["sessions"]
            .as_array()
            .expect("sessions should be an array");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0]["sessionId"], "session-newer");
        assert_eq!(sessions[1]["sessionId"], "session-older");
    }

    #[tokio::test]
    async fn workspace_sessions_selects_latest_visible_session_without_explicit_session() {
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
        assert_eq!(payload["sessionId"], "session-default-newer");
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
        assert_eq!(payload["sessionId"], "");
        assert!(
            payload["sessions"]
                .as_array()
                .expect("sessions should be an array")
                .is_empty()
        );
    }
}
