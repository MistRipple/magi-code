use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces/register", post(register_workspace))
        .route("/workspaces/remove", post(remove_workspace))
        .route("/workspaces/pick", get(pick_workspace))
        .route("/workspaces/sessions", get(workspace_sessions))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceDto {
    workspace_id: String,
    path: String,
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
    title: String,
    status: String,
    created_at: u64,
    updated_at: u64,
    message_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSessionsResponse {
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
                path: w.root_path.to_string(),
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
    let workspace_id =
        magi_core::WorkspaceId::new(format!("workspace-{}", magi_core::UtcMillis::now().0));
    let path = magi_core::AbsolutePath::new(&request.path);
    state
        .workspace_registry
        .register(workspace_id.clone(), path)
        .map_err(|e| ApiError::internal_assembly("工作区注册失败", e))?;
    state.persist_workspace_durable_state()?;
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.to_string(),
        "registered": true
    })))
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
    state.persist_workspace_durable_state()?;
    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn pick_workspace(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let workspaces = state.workspace_registry.workspaces();
    let active_id = state.workspace_registry.active_workspace_id();
    Json(serde_json::json!({
        "workspaces": workspaces.iter().map(|w| serde_json::json!({
            "workspaceId": w.workspace_id.to_string(),
            "path": w.root_path.to_string(),
            "name": w.name.clone(),
            "isActive": active_id.as_ref() == Some(&w.workspace_id),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
struct WorkspaceSessionsQuery {
    #[serde(rename = "workspaceId", alias = "workspace_id")]
    workspace_id: Option<String>,
}

async fn workspace_sessions(
    State(state): State<ApiState>,
    Query(query): Query<WorkspaceSessionsQuery>,
) -> Json<WorkspaceSessionsResponse> {
    let sessions = state.session_store.sessions();
    let workspace_id = query
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let session_matches_workspace = |session: &magi_session_store::SessionRecord| {
        let Some(workspace_id) = workspace_id else {
            return true;
        };
        session.workspace_id.as_deref() == Some(workspace_id)
    };

    let mut scoped_sessions = sessions
        .iter()
        .filter(|session| session_matches_workspace(session))
        .map(|session| WorkspaceSessionDto {
            session_id: session.session_id.to_string(),
            title: session.title.clone(),
            status: format!("{:?}", session.status),
            created_at: session.created_at.0,
            updated_at: session.updated_at.0,
            message_count: session.message_count.unwrap_or(0),
        })
        .collect::<Vec<_>>();
    scoped_sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| right.session_id.cmp(&left.session_id))
    });

    let current_session_id = scoped_sessions
        .first()
        .map(|session| session.session_id.clone())
        .unwrap_or_default();

    Json(WorkspaceSessionsResponse {
        session_id: current_session_id,
        sessions: scoped_sessions,
    })
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
        SessionDurableState, SessionExecutionSidecarStatus, SessionExecutionSidecarStoreState,
        SessionRecord, SessionRuntimeSidecar, SessionStore, TimelineEntryKind,
    };
    use magi_workspace::WorkspaceStore;
    use std::{sync::Arc, time::Duration};
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
                    .uri("/workspaces/sessions?workspaceId=workspace-count")
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
        assert_eq!(payload["sessionId"], "session-counted");
        let sessions = payload["sessions"]
            .as_array()
            .expect("sessions should be an array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionId"], "session-counted");
        assert_eq!(sessions[0]["messageCount"], 2);
        assert!(sessions[0]["updatedAt"].as_u64().unwrap_or_default() > 0);
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
            }],
            timeline: Vec::new(),
            notifications: Vec::new(),
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
        state
            .session_store
            .switch_session(&newer_session_id)
            .expect("current session should switch");
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
        assert_eq!(payload["sessionId"], "session-newer");
        let sessions = payload["sessions"]
            .as_array()
            .expect("sessions should be an array");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0]["sessionId"], "session-newer");
        assert_eq!(sessions[1]["sessionId"], "session-older");
    }
}
