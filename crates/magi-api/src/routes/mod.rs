mod agent_run_actions;
mod agent_runs;
mod changes_files_tunnel;
mod conversation_bridge;
mod dispatch_flow;
mod goals;
mod knowledge;
mod mcp_skills_repos;
mod messages;
mod session_scope;
pub(crate) mod sessions;
pub(crate) mod settings;
mod tools;
mod workspace_vcs;
mod workspaces;

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderName, StatusCode, Uri, header::HOST},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn};
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

pub(super) fn publish_superseded_turn_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    occurred_at: UtcMillis,
    turn: &CanonicalTurn,
) {
    state.event_bus.publish(
        EventEnvelope::domain(
            magi_core::EventId::new(format!(
                "event-session-turn-superseded-{}-{}",
                turn.turn_id, occurred_at.0
            )),
            "session.turn.superseded",
            serde_json::json!({
                "session_id": session_id,
                "workspace_id": workspace_id,
                "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
                "canonical_event_kind": "turn_superseded",
                "canonical_turn": turn,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.cloned(),
            ..EventContext::default()
        }),
    );
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
    SessionTaskSubmissionInput, accept_session_task_submission, accept_session_task_submission_at,
    append_dispatch_assistant_message, dispatch_accepted_canonical_event,
    finalize_session_task_dispatch, resolve_dispatch_session,
};

pub fn build_router(state: ApiState) -> Router {
    let tunnel_manager = state.tunnel_manager.clone();
    let api_routes = Router::new()
        .merge(workspaces::routes())
        .merge(workspace_vcs::routes())
        .merge(sessions::routes())
        .merge(goals::routes())
        .merge(knowledge::routes())
        .merge(settings::routes())
        .merge(mcp_skills_repos::routes())
        .merge(changes_files_tunnel::routes())
        .merge(agent_run_actions::routes())
        .merge(agent_runs::routes())
        .merge(tools::routes())
        .merge(messages::routes())
        .layer(middleware::from_fn(
            crate::errors::normalize_framework_rejection_response,
        ));

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
        .layer(middleware::from_fn_with_state(
            tunnel_manager,
            enforce_public_tunnel_auth,
        ))
        .with_state(state)
}

async fn enforce_public_tunnel_auth(
    State(tunnel_manager): State<crate::tunnel::TunnelManager>,
    mut request: Request,
    next: Next,
) -> Response {
    if !is_public_tunnel_request(request.headers())
        || !is_protected_remote_path(request.uri().path())
    {
        return next.run(request).await;
    }

    let token = query_parameter(request.uri().query(), "tunnel_token");
    if tunnel_manager
        .authorize_public_request(token.as_deref())
        .await
    {
        *request.uri_mut() = without_query_parameter(request.uri(), "tunnel_token");
        return next.run(request).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error_code": "TUNNEL_AUTH_REQUIRED",
            "message": "公网访问凭据无效，请重新扫描远程访问链接",
        })),
    )
        .into_response()
}

fn is_public_tunnel_request(headers: &HeaderMap) -> bool {
    headers.contains_key("cf-ray")
        || headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(|host| host.split(':').next())
            .is_some_and(|host| host.ends_with(".trycloudflare.com"))
}

fn is_protected_remote_path(path: &str) -> bool {
    path == "/bootstrap"
        || path == "/runtime/read-model"
        || path == "/ledger"
        || path == "/events"
        || path.starts_with("/bridges/")
        || path.starts_with("/api/")
}

fn query_parameter(query: Option<&str>, expected_key: &str) -> Option<String> {
    query?.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == expected_key)
            .then(|| {
                urlencoding::decode(value)
                    .ok()
                    .map(|value| value.into_owned())
            })
            .flatten()
    })
}

fn without_query_parameter(uri: &Uri, excluded_key: &str) -> Uri {
    let Some(query) = uri.query() else {
        return uri.clone();
    };
    let filtered_query = query
        .split('&')
        .filter(|part| {
            let key = part.split_once('=').map_or(*part, |(key, _value)| key);
            key != excluded_key
        })
        .collect::<Vec<_>>()
        .join("&");
    let path_and_query = if filtered_query.is_empty() {
        uri.path().to_owned()
    } else {
        format!("{}?{filtered_query}", uri.path())
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(
        path_and_query
            .parse()
            .expect("移除已存在查询参数后 URI 仍应保持合法"),
    );
    Uri::from_parts(parts).expect("替换合法 path-and-query 后 URI 仍应保持合法")
}

async fn health(State(state): State<ApiState>) -> Json<HealthDto> {
    Json(state.health_dto())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BootstrapQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
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
    let workspace_id_param = trimmed_query_param(query.workspace_id.as_deref());
    let workspace_path_param = trimmed_query_param(query.workspace_path.as_deref());
    let requested_workspace_id = state.resolve_workspace_id_from_request(
        workspace_id_param.map(WorkspaceId::new),
        workspace_path_param,
    );
    let requested_session_id = trimmed_query_param(query.session_id.as_deref()).map(SessionId::new);

    if let Some(workspace_id) = requested_workspace_id {
        let session_id = match requested_session_id {
            Some(session_id) => {
                let session = state
                    .session_store
                    .session(&session_id)
                    .ok_or_else(|| ApiError::not_found("session 不存在", session_id.as_str()))?;
                (state.session_workspace_id(&session).as_ref() == Some(&workspace_id))
                    .then_some(session_id)
            }
            None => None,
        };
        return Ok(BootstrapScope {
            workspace_id: Some(workspace_id.to_string()),
            session_id,
        });
    }

    if workspace_id_param.is_some() || workspace_path_param.is_some() {
        return Err(ApiError::not_found(
            "workspace 不存在",
            workspace_id_param
                .or(workspace_path_param)
                .unwrap_or_default(),
        ));
    }

    let scope = session_scope::resolve_optional_session_workspace_scope(
        state,
        query.session_id.as_deref(),
        None,
        None,
    )?;
    let workspace_id = scope.workspace_id().map(ToString::to_string).or_else(|| {
        state
            .workspace_registry
            .active_workspace_id()
            .map(|workspace_id| workspace_id.to_string())
    });
    Ok(BootstrapScope {
        workspace_id,
        session_id: scope.session_id().cloned(),
    })
}

fn trimmed_query_param(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventStreamQuery {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    session_id: Option<String>,
    after_sequence: Option<u64>,
}

async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<EventStreamQuery>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    let last_event_id = headers
        .get(HeaderName::from_static("last-event-id"))
        .and_then(|value| value.to_str().ok());
    let after_sequence = sse::resolve_after_sequence(query.after_sequence, last_event_id);
    sse::events(
        state,
        query.workspace_id,
        query.workspace_path,
        query.session_id,
        after_sequence,
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

    #[tokio::test]
    async fn public_tunnel_api_rejects_missing_or_invalid_token() {
        let mut state = test_state();
        state.tunnel_manager =
            crate::tunnel::TunnelManager::new_with_token_for_test(38123, "secret-token");
        let app = build_router(state);

        for uri in [
            "/api/tunnel/status",
            "/api/tunnel/status?tunnel_token=wrong",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("host", "example.trycloudflare.com")
                        .header("cf-ray", "test-ray")
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn public_tunnel_api_accepts_current_token_and_local_requests_need_none() {
        let mut state = test_state();
        state.tunnel_manager =
            crate::tunnel::TunnelManager::new_with_token_for_test(38123, "secret-token");
        let app = build_router(state);

        let public_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tunnel/status?tunnel_token=secret-token")
                    .header("host", "example.trycloudflare.com")
                    .header("cf-ray", "test-ray")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(public_response.status(), StatusCode::OK);

        let local_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tunnel/status")
                    .header("host", "127.0.0.1:38123")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(local_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_tunnel_auth_parameter_is_not_exposed_to_business_query_parsing() {
        let mut state = test_state();
        state.tunnel_manager =
            crate::tunnel::TunnelManager::new_with_token_for_test(38123, "secret-token");
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/bootstrap?tunnel_token=secret-token")
                    .header("host", "example.trycloudflare.com")
                    .header("cf-ray", "test-ray")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn public_tunnel_auth_removes_only_transport_query_parameter() {
        let uri = Uri::from_static(
            "/bootstrap?workspacePath=%2Ftmp%2Fmagi&tunnel_token=secret-token&sessionId=session-1&tunnel_token=duplicate",
        );

        assert_eq!(
            without_query_parameter(&uri, "tunnel_token"),
            Uri::from_static("/bootstrap?workspacePath=%2Ftmp%2Fmagi&sessionId=session-1")
        );
    }

    #[tokio::test]
    async fn public_tunnel_event_stream_accepts_token_without_leaking_it_to_query_parsing() {
        let mut state = test_state();
        state.tunnel_manager =
            crate::tunnel::TunnelManager::new_with_token_for_test(38123, "secret-token");
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/events?afterSequence=0&tunnel_token=secret-token")
                    .header("host", "example.trycloudflare.com")
                    .header("cf-ray", "test-ray")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
    }

    async fn post_json_body(
        app: Router,
        path: &str,
        body: &'static str,
    ) -> (StatusCode, serde_json::Value, String) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let raw_body = String::from_utf8_lossy(&bytes).to_string();
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!("response should be json: {error}; body={raw_body}");
        });
        (status, body, raw_body)
    }

    #[test]
    fn bootstrap_workspace_resolution_ignores_workspace_mismatched_session() {
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

        let resolved = result.expect("bootstrap should ignore stale foreign session");
        assert_eq!(resolved.workspace_id.as_deref(), Some(workspace_a.as_str()));
        assert_eq!(resolved.session_id, None);
    }

    #[test]
    fn bootstrap_workspace_resolution_rejects_unknown_explicit_session() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-bootstrap-unknown-session");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-unknown-session"),
            )
            .expect("workspace should register");

        let result = resolve_bootstrap_scope(
            &state,
            &BootstrapQuery {
                workspace_id: Some(workspace_id.to_string()),
                workspace_path: None,
                session_id: Some("session-bootstrap-missing".to_string()),
            },
        );

        match result {
            Err(ApiError::NotFound(message)) => {
                assert!(
                    message.contains("session 不存在")
                        && message.contains("session-bootstrap-missing"),
                    "unknown session error should stay explicit: {message}"
                );
            }
            other => panic!("unknown explicit session should not fall back: {other:?}"),
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

    #[test]
    fn bootstrap_workspace_resolution_rejects_unknown_explicit_workspace_before_session_fallback() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-bootstrap-valid-session");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-valid-session"),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-bootstrap-valid-session");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "有效 session",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");

        let result = resolve_bootstrap_scope(
            &state,
            &BootstrapQuery {
                workspace_id: Some("workspace-bootstrap-missing".to_string()),
                workspace_path: None,
                session_id: Some(session_id.to_string()),
            },
        );

        match result {
            Err(ApiError::NotFound(message)) => {
                assert!(
                    message.contains("workspace 不存在")
                        && message.contains("workspace-bootstrap-missing"),
                    "unknown workspace error should stay explicit: {message}"
                );
            }
            other => {
                panic!("unknown explicit workspace should not fall back to session: {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn bootstrap_route_ignores_workspace_mismatched_session_scope() {
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
        let session_a = SessionId::new("session-bootstrap-route-a");
        state
            .session_store
            .create_session_for_workspace(
                session_a.clone(),
                "A 会话",
                Some(workspace_a.to_string()),
            )
            .expect("session A should create");
        state.session_store.append_timeline_entry(
            session_a.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "A 消息",
        );
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

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["currentSession"]["sessionId"],
            serde_json::json!(session_a.as_str())
        );
        assert_eq!(
            body["sessions"]
                .as_array()
                .expect("sessions should be an array")
                .iter()
                .map(|session| session["workspaceId"].as_str())
                .collect::<Vec<_>>(),
            vec![Some(workspace_a.as_str())]
        );
    }

    #[tokio::test]
    async fn bootstrap_route_rejects_unknown_explicit_session_scope() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-bootstrap-route-unknown-session");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-route-unknown-session"),
            )
            .expect("workspace should register");
        let app = build_router(state);

        let (status, body) = get_json(
            app,
            &format!(
                "/bootstrap?workspaceId={}&sessionId=session-bootstrap-route-missing",
                workspace_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("session 不存在"))
        );
    }

    #[tokio::test]
    async fn bootstrap_route_rejects_unknown_explicit_workspace_before_session_fallback() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-bootstrap-route-valid-session");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-route-valid-session"),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-bootstrap-route-valid-session");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "有效 session",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let app = build_router(state);

        let (status, body) = get_json(
            app,
            &format!(
                "/bootstrap?workspaceId=workspace-bootstrap-route-missing&sessionId={}",
                session_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error_code"], serde_json::json!("NOT_FOUND"));
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("workspace-bootstrap-route-missing")),
            "unknown explicit workspace should stay authoritative: {body}"
        );
    }

    #[tokio::test]
    async fn api_malformed_json_uses_public_error_response() {
        let app = build_router(test_state());

        let (status, body, raw_body) = post_json_body(app, "/api/workspaces/register", "{").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error_code"],
            serde_json::json!("REQUEST_BODY_INVALID")
        );
        assert_eq!(
            body["message"],
            serde_json::json!("请求内容格式不正确，请检查后重试")
        );
        assert!(
            !raw_body.contains("line")
                && !raw_body.contains("column")
                && !raw_body.contains("expected"),
            "框架 JSON parser 细节不能出现在 API 响应中: {raw_body}"
        );
    }

    #[tokio::test]
    async fn api_business_invalid_input_keeps_actionable_message() {
        let app = build_router(test_state());

        let (status, body, _raw_body) =
            post_json_body(app, "/api/workspaces/register", r#"{"path":""}"#).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error_code"], serde_json::json!("INPUT_INVALID"));
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("工作区路径不能为空")),
            "业务校验错误应保留可操作文案: {body}"
        );
    }
}
