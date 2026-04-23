use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use magi_core::SessionId;
use serde::{Deserialize, Serialize};

use crate::{
    errors::ApiError,
    state::{ApiState, RunnerStartError, RunnerStopError},
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/tasks/runner/start", post(start_runner))
        .route("/tasks/runner/stop", post(stop_runner))
        .route("/tasks/runner/status/{root_task_id}", get(runner_status))
        .route("/tasks/runner/cycle", post(run_cycle))
}

fn require_runner_manager(state: &ApiState) -> Result<&crate::state::RunnerManager, ApiError> {
    state.runner_manager().ok_or_else(|| {
        ApiError::internal_assembly("Runner 未配置", "runner_manager is not configured")
    })
}

fn parse_session_id(value: Option<&str>) -> Result<SessionId, ApiError> {
    let session_id = value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    Ok(SessionId::new(session_id))
}

fn require_session_root_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    root_task_id: &str,
) -> Result<SessionId, ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务链".to_string()))?;
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("runner session guard", "task_store 未配置"))?;
    let task = task_store
        .get_task(&magi_core::TaskId::new(root_task_id))
        .ok_or_else(|| ApiError::not_found("任务不存在", root_task_id))?;
    if task.mission_id != mission_id || task.parent_task_id.is_some() {
        return Err(ApiError::InvalidInput(format!(
            "根任务 {} 不属于当前会话 {}",
            root_task_id, session_id
        )));
    }
    Ok(session_id)
}

// ─── Request / Response DTOs ────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnerStartRequest {
    root_task_id: String,
    session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerStartResponse {
    root_task_id: String,
    started: bool,
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnerStopRequest {
    root_task_id: String,
    session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerStopResponse {
    root_task_id: String,
    stopped: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerStatusResponse {
    root_task_id: String,
    status: String,
    cycle_count: u64,
    last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnerCycleRequest {
    root_task_id: String,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnerStatusQuery {
    session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerCycleResponse {
    root_task_id: String,
    outcome: String,
    blocked_task_ids: Vec<String>,
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn start_runner(
    State(state): State<ApiState>,
    Json(request): Json<RunnerStartRequest>,
) -> Result<Json<RunnerStartResponse>, ApiError> {
    require_session_root_task(&state, request.session_id.as_deref(), &request.root_task_id)?;
    let manager = require_runner_manager(&state)?;
    match manager.start(&request.root_task_id) {
        Ok(_handle) => Ok(Json(RunnerStartResponse {
            root_task_id: request.root_task_id,
            started: true,
            status: "running".to_string(),
        })),
        Err(RunnerStartError::NotFound) => {
            Err(ApiError::not_found("任务不存在", &request.root_task_id))
        }
        Err(RunnerStartError::AlreadyRunning) => {
            Err(ApiError::conflict("Runner 已在运行", &request.root_task_id))
        }
    }
}

async fn stop_runner(
    State(state): State<ApiState>,
    Json(request): Json<RunnerStopRequest>,
) -> Result<Json<RunnerStopResponse>, ApiError> {
    require_session_root_task(&state, request.session_id.as_deref(), &request.root_task_id)?;
    let manager = require_runner_manager(&state)?;
    match manager.stop(&request.root_task_id) {
        Ok(()) => Ok(Json(RunnerStopResponse {
            root_task_id: request.root_task_id,
            stopped: true,
        })),
        Err(RunnerStopError::NotFound) => {
            Err(ApiError::not_found("Runner 不存在", &request.root_task_id))
        }
        Err(RunnerStopError::NotRunning) => Err(ApiError::not_found(
            "Runner 未在运行",
            &request.root_task_id,
        )),
    }
}

async fn runner_status(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<RunnerStatusQuery>,
) -> Result<Json<RunnerStatusResponse>, ApiError> {
    require_session_root_task(&state, query.session_id.as_deref(), &root_task_id)?;
    let manager = require_runner_manager(&state)?;
    let snapshot = manager
        .status(&root_task_id)
        .ok_or_else(|| ApiError::not_found("Runner 不存在", &root_task_id))?;
    Ok(Json(RunnerStatusResponse {
        root_task_id: snapshot.root_task_id,
        status: snapshot.status,
        cycle_count: snapshot.cycle_count,
        last_error: snapshot.last_error,
    }))
}

async fn run_cycle(
    State(state): State<ApiState>,
    Json(request): Json<RunnerCycleRequest>,
) -> Result<Json<RunnerCycleResponse>, ApiError> {
    require_session_root_task(&state, request.session_id.as_deref(), &request.root_task_id)?;
    let manager = require_runner_manager(&state)?;
    let outcome = manager
        .run_single_cycle(&request.root_task_id)
        .map_err(|err| ApiError::not_found("任务不存在", &err))?;

    let (outcome_str, blocked_ids) = match &outcome {
        magi_orchestrator::task_runner::RunCycleOutcome::Continue => {
            ("continue".to_string(), vec![])
        }
        magi_orchestrator::task_runner::RunCycleOutcome::AllComplete => {
            ("all_complete".to_string(), vec![])
        }
        magi_orchestrator::task_runner::RunCycleOutcome::Blocked(ids) => (
            "blocked".to_string(),
            ids.iter().map(|id| id.to_string()).collect(),
        ),
        magi_orchestrator::task_runner::RunCycleOutcome::Error(err) => {
            ("error".to_string(), vec![err.clone()])
        }
    };

    Ok(Json(RunnerCycleResponse {
        root_task_id: request.root_task_id,
        outcome: outcome_str,
        blocked_task_ids: blocked_ids,
    }))
}
