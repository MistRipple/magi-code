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
mod workspace_vcs;
mod workspaces;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use magi_core::{SessionId, UtcMillis, WorkspaceId};
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
    finalize_session_task_dispatch, resolve_dispatch_session,
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

impl BootstrapQuery {
    fn requested_session_id(&self) -> Option<SessionId> {
        self.session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
            .map(SessionId::new)
    }

    fn requested_workspace_id(&self) -> Option<String> {
        self.workspace_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(String::from)
    }
}

async fn bootstrap(
    State(state): State<ApiState>,
    Query(query): Query<BootstrapQuery>,
) -> Result<Json<BootstrapDto>, ApiError> {
    let requested_session_id = query.requested_session_id();
    let workspace_id = resolve_bootstrap_workspace_id(
        &state,
        query.requested_workspace_id(),
        query.workspace_path.clone(),
        requested_session_id.as_ref(),
    );
    Ok(Json(state.bootstrap_dto_for_workspace_session(
        workspace_id.as_deref(),
        requested_session_id.as_ref(),
    )?))
}

fn resolve_bootstrap_workspace_id(
    state: &ApiState,
    requested_workspace_id: Option<String>,
    requested_workspace_path: Option<String>,
    requested_session_id: Option<&SessionId>,
) -> Option<String> {
    if let Some(workspace_id) = state.resolve_workspace_id_from_request(
        requested_workspace_id.map(WorkspaceId::new),
        requested_workspace_path.as_deref(),
    ) {
        return Some(workspace_id.to_string());
    }
    if let Some(session_id) = requested_session_id
        && let Some(session) = state.session_store.session(session_id)
        && let Some(workspace_id) = session_scope::session_workspace_id(state, &session)
    {
        return Some(workspace_id.to_string());
    }
    state
        .workspace_registry
        .active_workspace_id()
        .map(|workspace_id| workspace_id.to_string())
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
}

async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<EventStreamQuery>,
) -> impl axum::response::IntoResponse {
    sse::events(state, query.workspace_id).await
}
