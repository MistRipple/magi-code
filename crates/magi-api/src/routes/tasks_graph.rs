use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use magi_core::{TaskId, TaskProjection};
use serde::Deserialize;

use super::session_scope::parse_session_id;
use crate::{errors::ApiError, shadow_execution::replan_deep_task_graph, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/tasks/graph/{root_task_id}", get(get_task_projection))
        .route("/tasks/{task_id}", get(get_task))
        .route("/tasks/{task_id}/decision", post(resolve_decision))
        .route("/tasks/{root_task_id}/replan", post(replan_task_graph))
        .route(
            "/tasks/{root_task_id}/delivery-package",
            get(get_delivery_package),
        )
}

fn require_task_store(
    state: &ApiState,
) -> Result<&magi_orchestrator::task_store::TaskStore, ApiError> {
    state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("任务存储未配置", "task_store is not configured")
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionScopedTaskQuery {
    session_id: Option<String>,
}

fn require_session_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    task_id: &TaskId,
) -> Result<(), ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务链".to_string()))?;
    let store = require_task_store(state)?;
    let task = store
        .get_task(task_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id.as_str()))?;
    if task.mission_id != mission_id {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话 {}",
            task_id, session_id
        )));
    }
    Ok(())
}

async fn get_task_projection(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    require_session_task(&state, query.session_id.as_deref(), &root_id)?;
    let mut projection = store
        .build_projection(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    apply_authoritative_runner_status(&state, &root_id, &mut projection);
    let value = serde_json::to_value(&projection)
        .map_err(|err| ApiError::internal_assembly("序列化任务投影失败", err))?;
    Ok(Json(value))
}

fn apply_authoritative_runner_status(
    state: &ApiState,
    root_task_id: &TaskId,
    projection: &mut TaskProjection,
) {
    let Some(snapshot) = state
        .runner_manager()
        .and_then(|manager| manager.status(root_task_id.as_str()))
    else {
        return;
    };
    if let Some(status) = normalize_runner_status(&snapshot.status) {
        projection.runner_status = status.to_string();
    }
}

fn normalize_runner_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => Some("running"),
        "blocked" => Some("blocked"),
        "completed" => Some("completed"),
        "error" => Some("error"),
        "idle" | "stopped" => Some("idle"),
        _ => None,
    }
}

async fn get_task(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    require_session_task(&state, query.session_id.as_deref(), &id)?;
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;
    let value = serde_json::to_value(&task)
        .map_err(|err| ApiError::internal_assembly("序列化任务失败", err))?;
    Ok(Json(value))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveDecisionRequest {
    chosen_option: String,
    evidence: Option<serde_json::Value>,
}

async fn resolve_decision(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
    Json(request): Json<ResolveDecisionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    require_session_task(&state, query.session_id.as_deref(), &id)?;
    store
        .resolve_decision(&id, &request.chosen_option, request.evidence)
        .map_err(|error| ApiError::InvalidInput(error))?;
    Ok(Json(serde_json::json!({
        "taskId": task_id,
        "resolved": true,
        "chosenOption": request.chosen_option,
    })))
}

async fn replan_task_graph(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root_id = TaskId::new(&root_task_id);
    require_session_task(&state, query.session_id.as_deref(), &root_id)?;
    let store = require_task_store(&state)?;
    let root_task = store
        .get_task(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    let root_goal = root_task.goal.trim();
    let objective_text = if root_goal.is_empty() {
        root_task.title.as_str()
    } else {
        root_goal
    };
    let prompt = format!(
        "当前任务目标：{}\n请基于当前已完成任务重规划剩余任务图，保留已完成节点，不重写已完成工作。",
        objective_text
    );
    let replan =
        replan_deep_task_graph(&state, &root_id, &prompt, None, "manual task graph replan")?;

    Ok(Json(serde_json::json!({
        "rootTaskId": root_task_id,
        "replan": true,
        "cancelledTaskIds": replan.cancelled_task_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
    })))
}

async fn get_delivery_package(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    require_session_task(&state, query.session_id.as_deref(), &root_id)?;

    let package = store
        .build_delivery_package(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;

    Ok(Json(package))
}
