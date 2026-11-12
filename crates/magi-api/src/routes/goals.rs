use crate::{
    errors::ApiError,
    goal_service::{GoalActionRequest, GoalQuery, GoalUpdateRequest},
    state::ApiState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/goals/current", get(get_current_goal))
        .route("/goals/current/update", post(update_current_goal))
        .route("/goals/current/pause", post(pause_current_goal))
        .route("/goals/current/resume", post(resume_current_goal))
        .route("/goals/current/clear", post(clear_current_goal))
        .route("/goals/current/plan/clear", post(clear_current_plan))
}

async fn get_current_goal(
    State(state): State<ApiState>,
    Query(query): Query<GoalQuery>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::get_current_goal(&state, query)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn update_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalUpdateRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::update_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn pause_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::pause_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn resume_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::resume_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn clear_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::clear_current_goal(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

async fn clear_current_plan(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<magi_app_server_protocol::GoalSnapshot>, ApiError> {
    crate::goal_service::clear_current_plan(&state, request)
        .await
        .and_then(crate::goal_service::snapshot_to_wire)
        .map(Json)
}

#[cfg(test)]
mod tests;
