mod changes_files_tunnel;
mod conversation_bridge;
mod dispatch_flow;
mod knowledge;
mod mcp_skills_repos;
mod messages;
mod session_scope;
mod sessions;
pub(crate) mod settings;
mod tasks_interaction;
mod tasks_projection;
mod tools;
mod workspace_vcs;
mod workspaces;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::{SessionId, UtcMillis};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};

static ACCEPTED_AT_COUNTER: AtomicU64 = AtomicU64::new(0);
static SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn monotonic_accepted_at() -> UtcMillis {
    let now = UtcMillis::now().0;
    let prev = ACCEPTED_AT_COUNTER.fetch_max(now, Ordering::SeqCst);
    let ts = if now <= prev { prev + 1 } else { now };
    ACCEPTED_AT_COUNTER.store(ts, Ordering::SeqCst);
    UtcMillis(ts)
}

pub(super) fn new_session_id() -> SessionId {
    let nonce = SESSION_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    SessionId::new(format!("session-{}-{}", UtcMillis::now().0, nonce))
}

use crate::{
    dto::{
        AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeSnapshotDto,
        BridgePreflightSnapshotDto, BridgeServicesSnapshotDto, HealthDto, RuntimeReadModelDto,
        VersionHandshakeDto,
    },
    errors::ApiError,
    sse,
    state::ApiState,
};

use conversation_bridge::{
    begin_session_turn, finalize_session_turn, ingest_user_input_to_conversation,
};
use dispatch_flow::{
    accept_session_task_submission, append_dispatch_assistant_message,
    dispatch_accepted_canonical_event, finalize_session_task_dispatch, resolve_dispatch_session,
};

pub fn build_router(state: ApiState) -> Router {
    let api_routes = Router::new()
        .merge(workspaces::routes())
        .merge(workspace_vcs::routes())
        .merge(sessions::routes())
        .merge(knowledge::routes())
        .merge(settings::routes())
        .merge(mcp_skills_repos::routes())
        .merge(changes_files_tunnel::routes())
        .merge(tasks_interaction::routes())
        .merge(tasks_projection::routes())
        .merge(tools::routes())
        .merge(messages::routes());

    Router::new()
        .route("/health", get(health))
        .route("/bootstrap", get(bootstrap))
        .route("/runtime/read-model", get(runtime_read_model))
        .route("/ledger", get(ledger))
        .route("/bridges/services", get(bridge_services))
        .route("/bridges/preflight", get(bridge_preflight))
        .route("/bridges/cutover-smoke", get(bridge_cutover_smoke))
        .route("/events", get(stream_events))
        .route("/version", get(version))
        .nest("/api", api_routes)
        .with_state(state)
}

async fn health(State(state): State<ApiState>) -> Json<HealthDto> {
    Json(state.health_dto())
}

#[derive(Debug, Deserialize)]
struct BootstrapQuery {
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "workspaceId", alias = "workspace_id")]
    workspace_id: Option<String>,
    #[serde(rename = "workspacePath", alias = "workspace_path")]
    workspace_path: Option<String>,
}

async fn bootstrap(
    State(state): State<ApiState>,
    Query(query): Query<BootstrapQuery>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let scope = resolve_bootstrap_scope(&state, &query)?;
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        scope.workspace_id.as_deref(),
        scope.session_id.as_ref(),
    )?))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BootstrapScope {
    workspace_id: Option<String>,
    session_id: Option<SessionId>,
}

fn resolve_bootstrap_scope(
    state: &ApiState,
    query: &BootstrapQuery,
) -> Result<BootstrapScope, ApiError> {
    let scope = session_scope::resolve_optional_session_workspace_scope(
        state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let workspace_id = scope.workspace_id().map(ToString::to_string).or_else(|| {
        if scope.session_id().is_none() {
            state
                .workspace_registry
                .active_workspace_id()
                .map(|workspace_id| workspace_id.to_string())
        } else {
            None
        }
    });
    Ok(BootstrapScope {
        workspace_id,
        session_id: scope.session_id().cloned(),
    })
}

async fn runtime_read_model(State(state): State<ApiState>) -> Json<RuntimeReadModelDto> {
    Json(state.runtime_read_model_dto())
}

async fn ledger(State(state): State<ApiState>) -> Json<AuditUsageLedgerDto> {
    Json(state.audit_usage_ledger_dto())
}

async fn bridge_services(State(state): State<ApiState>) -> Json<BridgeServicesSnapshotDto> {
    Json(state.bridge_services_dto())
}

async fn bridge_preflight(State(state): State<ApiState>) -> Json<BridgePreflightSnapshotDto> {
    Json(state.bridge_preflight_dto())
}

async fn bridge_cutover_smoke(
    State(state): State<ApiState>,
) -> Json<BridgeCutoverSmokeSnapshotDto> {
    Json(state.bridge_cutover_smoke_dto())
}

async fn version(State(state): State<ApiState>) -> Json<VersionHandshakeDto> {
    Json(state.version_handshake_dto())
}

#[derive(Debug, Deserialize)]
struct EventStreamQuery {
    #[serde(rename = "workspaceId", alias = "workspace_id")]
    workspace_id: Option<String>,
    #[serde(rename = "workspacePath", alias = "workspace_path")]
    workspace_path: Option<String>,
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
}

async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<EventStreamQuery>,
) -> impl axum::response::IntoResponse {
    sse::events(
        state,
        query.workspace_id,
        query.workspace_path,
        query.session_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{AbsolutePath, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
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

    async fn get_json(app: Router, path: &str) -> (StatusCode, serde_json::Value) {
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
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "response should be json: {error}; body={}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, body)
    }

    #[test]
    fn bootstrap_workspace_resolution_rejects_workspace_mismatched_session() {
        let state = test_state();
        let workspace_a = WorkspaceId::new("workspace-bootstrap-url-a");
        let workspace_b = WorkspaceId::new("workspace-bootstrap-url-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-url-a"),
            )
            .expect("workspace A should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-url-b"),
            )
            .expect("workspace B should register");
        let session_b = SessionId::new("session-bootstrap-url-b");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session B should create");

        let result = resolve_bootstrap_scope(
            &state,
            &BootstrapQuery {
                workspace_id: Some(workspace_a.to_string()),
                workspace_path: None,
                session_id: Some(session_b.to_string()),
            },
        );

        match result {
            Err(ApiError::InvalidInput(message)) => {
                assert!(
                    message.contains("不属于 workspace"),
                    "bootstrap should reject mismatched session binding: {message}"
                );
            }
            other => panic!("unexpected bootstrap scope result: {:?}", other),
        }
    }

    #[test]
    fn bootstrap_workspace_resolution_uses_session_workspace_without_explicit_workspace() {
        let state = test_state();
        let workspace_b = WorkspaceId::new("workspace-bootstrap-session-deeplink");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-session-deeplink"),
            )
            .expect("workspace should register");
        let session_b = SessionId::new("session-bootstrap-session-deeplink");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session should create");

        let resolved = resolve_bootstrap_scope(
            &state,
            &BootstrapQuery {
                workspace_id: None,
                workspace_path: None,
                session_id: Some(session_b.to_string()),
            },
        )
        .expect("session workspace should resolve");

        assert_eq!(resolved.workspace_id.as_deref(), Some(workspace_b.as_str()));
    }

    #[tokio::test]
    async fn bootstrap_route_rejects_workspace_mismatched_session_scope() {
        let state = test_state();
        let workspace_a = WorkspaceId::new("workspace-bootstrap-route-a");
        let workspace_b = WorkspaceId::new("workspace-bootstrap-route-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-route-a"),
            )
            .expect("workspace A should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-route-b"),
            )
            .expect("workspace B should register");
        let session_b = SessionId::new("session-bootstrap-route-b");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session B should create");
        let app = build_router(state);

        let (status, body) = get_json(
            app,
            &format!(
                "/bootstrap?workspaceId={}&sessionId={}",
                workspace_a, session_b
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], serde_json::json!("INPUT_INVALID"));
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("不属于 workspace")),
            "bootstrap mismatch should be actionable for bridge recovery: {body}"
        );
    }
}
