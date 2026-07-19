use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{DomainError, GoalId, SessionId};
use magi_session_store::{GoalStatus, SessionGoal, SessionPlan};
use serde::{Deserialize, Serialize};

use super::session_scope::{SessionWorkspaceScope, require_session_workspace_scope};
use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/goals/current", get(get_current_goal))
        .route("/goals/current/update", post(update_current_goal))
        .route("/goals/current/pause", post(pause_current_goal))
        .route("/goals/current/resume", post(resume_current_goal))
        .route("/goals/current/clear", post(clear_current_goal))
        .route("/goals/current/plan/clear", post(clear_current_plan))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentGoalResponseDto {
    session_id: String,
    workspace_id: String,
    workspace_path: String,
    goal: Option<SessionGoal>,
    plan: Option<SessionPlan>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalActionRequest {
    session_id: Option<String>,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalUpdateRequest {
    session_id: Option<String>,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    objective: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalMutationResponseDto {
    session_id: String,
    workspace_id: String,
    workspace_path: String,
    goal: Option<SessionGoal>,
}

async fn get_current_goal(
    State(state): State<ApiState>,
    Query(query): Query<GoalQuery>,
) -> Result<Json<CurrentGoalResponseDto>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
        "读取当前目标",
    )?;
    Ok(Json(current_goal_response(&state, scope)))
}

fn current_goal_response(state: &ApiState, scope: SessionWorkspaceScope) -> CurrentGoalResponseDto {
    let goal = state.session_store.current_visible_goal(&scope.session_id);
    let plan = state.session_store.plan(&scope.session_id);
    CurrentGoalResponseDto {
        session_id: scope.session_id.to_string(),
        workspace_id: scope.workspace_id.to_string(),
        workspace_path: scope.workspace_path,
        goal,
        plan,
    }
}

async fn update_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalUpdateRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        "编辑当前目标",
    )?;
    let current = require_current_visible_goal(&state, &scope.session_id)?;
    let goal = state
        .session_store
        .update_goal_objective(
            &scope.session_id,
            &GoalId::new(current.goal_id.as_str()),
            request.objective,
        )
        .map_err(map_goal_domain_error)?;
    state.persist_session_state_checkpoint("goal_updated")?;
    Ok(Json(goal_mutation_response(scope, Some(goal))))
}

async fn pause_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    mutate_current_goal_status(state, request, GoalStatus::Paused, "goal_paused").await
}

async fn resume_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    mutate_current_goal_status(state, request, GoalStatus::Active, "goal_resumed").await
}

async fn clear_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        "清除当前目标",
    )?;
    let current = require_current_visible_goal(&state, &scope.session_id)?;
    state
        .session_store
        .set_goal_status(
            &scope.session_id,
            &GoalId::new(current.goal_id.as_str()),
            GoalStatus::Cleared,
        )
        .map_err(map_goal_domain_error)?;
    let plan_store =
        magi_plan::PlanStore::new(state.session_store.clone(), scope.session_id.clone());
    let cleared_plan = plan_store.snapshot();
    plan_store.clear(None).map_err(map_plan_error)?;
    if let Some(plan) = cleared_plan.as_ref() {
        magi_plan::publish_plan_cleared_event(&state.event_bus, plan, Some(&scope.workspace_id));
    }
    state.persist_session_state_checkpoint("goal_cleared")?;
    Ok(Json(goal_mutation_response(scope, None)))
}

async fn clear_current_plan(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<CurrentGoalResponseDto>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        "清除当前计划",
    )?;
    let plan_store =
        magi_plan::PlanStore::new(state.session_store.clone(), scope.session_id.clone());
    let cleared_plan = plan_store.snapshot();
    plan_store.clear(None).map_err(map_plan_error)?;
    if let Some(plan) = cleared_plan.as_ref() {
        magi_plan::publish_plan_cleared_event(&state.event_bus, plan, Some(&scope.workspace_id));
    }
    state.persist_session_state_checkpoint("session_plan_cleared")?;
    Ok(Json(current_goal_response(&state, scope)))
}

async fn mutate_current_goal_status(
    state: ApiState,
    request: GoalActionRequest,
    status: GoalStatus,
    checkpoint: &'static str,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        "更新当前目标状态",
    )?;
    let current = require_current_visible_goal(&state, &scope.session_id)?;
    let goal = state
        .session_store
        .set_goal_status(
            &scope.session_id,
            &GoalId::new(current.goal_id.as_str()),
            status,
        )
        .map_err(map_goal_domain_error)?;
    let plan_store =
        magi_plan::PlanStore::new(state.session_store.clone(), scope.session_id.clone());
    let plan = match status {
        GoalStatus::Paused => plan_store.pause(),
        GoalStatus::Active => plan_store.resume(),
        _ => Ok(plan_store.snapshot()),
    }
    .map_err(map_plan_error)?;
    if let Some(plan) = plan.as_ref() {
        magi_plan::publish_plan_event(
            &state.event_bus,
            magi_plan::plan_event_type(plan),
            plan,
            Some(&scope.workspace_id),
            None,
            None,
        );
    }
    state.persist_session_state_checkpoint(checkpoint)?;
    Ok(Json(goal_mutation_response(scope, Some(goal))))
}

fn require_current_visible_goal(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<SessionGoal, ApiError> {
    state
        .session_store
        .current_visible_goal(session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可操作目标".to_string()))
}

fn goal_mutation_response(
    scope: SessionWorkspaceScope,
    goal: Option<SessionGoal>,
) -> GoalMutationResponseDto {
    GoalMutationResponseDto {
        session_id: scope.session_id.to_string(),
        workspace_id: scope.workspace_id.to_string(),
        workspace_path: scope.workspace_path,
        goal,
    }
}

fn map_goal_domain_error(error: DomainError) -> ApiError {
    match error {
        DomainError::NotFound { .. } => ApiError::not_found("目标不存在", "current"),
        DomainError::Validation { message } | DomainError::InvalidState { message } => {
            ApiError::InvalidInput(message)
        }
        other => ApiError::internal_assembly("目标状态更新失败", other),
    }
}

fn map_plan_error(error: magi_plan::PlanUpdateError) -> ApiError {
    ApiError::InvalidInput(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use magi_core::PlanItemStatus;
    use magi_core::{AbsolutePath, SessionId, ThreadId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn current_goal_route_reads_session_goal_without_task_projection() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-route");
        let workspace_path = std::env::temp_dir().join("magi-goal-route");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-route");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal route",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = state
            .session_store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-route"),
                "完成 Goal API",
                magi_core::AccessProfile::Restricted,
                Some(2048),
            )
            .expect("goal should be creatable");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("response should be json");
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(goal.goal_id.as_str())
        );
        assert_eq!(payload["goal"]["objective"].as_str(), Some("完成 Goal API"));
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert!(payload["plan"].is_null());
    }

    #[tokio::test]
    async fn current_goal_route_returns_stable_revisioned_plan() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-plan-route");
        let workspace_path = std::env::temp_dir().join("magi-goal-plan-route");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-plan-route");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal plan route",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        state
            .session_store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-plan-route"),
                "完成稳定计划展示",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("goal should be creatable");
        let plan_store = magi_plan::PlanStore::new(Arc::clone(&session_store), session_id.clone());
        let created = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("inspect".to_string()),
                        step: "检查现状".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "验证结果".to_string(),
                        status: PlanItemStatus::Pending,
                    },
                ],
            })
            .expect("plan should create");
        let updated = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("inspect".to_string()),
                        step: "检查现状".to_string(),
                        status: PlanItemStatus::Completed,
                    },
                    magi_plan::UpdatePlanItemInput {
                        item_id: Some("verify".to_string()),
                        step: "验证结果".to_string(),
                        status: PlanItemStatus::InProgress,
                    },
                ],
            })
            .expect("plan should update");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(
            payload["plan"]["planId"].as_str(),
            Some(updated.plan_id.as_str())
        );
        assert_eq!(payload["plan"]["revision"].as_u64(), Some(updated.revision));
        let items = payload["plan"]["items"].as_array().expect("plan items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["itemId"].as_str(), Some("inspect"));
        assert_eq!(items[0]["status"].as_str(), Some("completed"));
        assert_eq!(items[1]["itemId"].as_str(), Some("verify"));
        assert_eq!(items[1]["status"].as_str(), Some("in_progress"));
    }

    #[tokio::test]
    async fn current_goal_actions_edit_pause_resume_and_clear_session_goal() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-actions");
        let workspace_path = std::env::temp_dir().join("magi-goal-actions");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-actions");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal actions",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        state
            .session_store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-actions"),
                "原目标",
                magi_core::AccessProfile::Restricted,
                Some(4096),
            )
            .expect("goal should be creatable");
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("execute".to_string()),
                    step: "执行当前目标".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should be creatable");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let scope_body = serde_json::json!({
            "sessionId": session_id.to_string(),
            "workspaceId": workspace_id.to_string()
        });

        let update_body = serde_json::json!({
            "sessionId": session_id.to_string(),
            "workspaceId": workspace_id.to_string(),
            "objective": "更新后的目标"
        });
        let response = app
            .clone()
            .oneshot(json_post("/goals/current/update", update_body))
            .await
            .expect("update should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["objective"].as_str(), Some("更新后的目标"));

        let response = app
            .clone()
            .oneshot(json_post("/goals/current/pause", scope_body.clone()))
            .await
            .expect("pause should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("paused"));
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Paused
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.plan.paused"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                })
        );

        let response = app
            .clone()
            .oneshot(json_post("/goals/current/resume", scope_body.clone()))
            .await
            .expect("resume should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Active
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.plan.updated"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                })
        );

        let active_goal = state
            .session_store
            .active_goal(&session_id)
            .expect("goal should be active after resume");
        state
            .session_store
            .set_goal_status(&session_id, &active_goal.goal_id, GoalStatus::Complete)
            .expect("goal should be markable complete");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("complete"));
        let response = app
            .clone()
            .oneshot(json_post("/goals/current/clear", scope_body))
            .await
            .expect("clear should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["goal"].is_null());
        assert!(state.session_store.plan(&session_id).is_none());
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.plan.cleared"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                        && event.payload["plan"].is_null()
                })
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?sessionId={}&workspaceId={}",
                        session_id, workspace_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["goal"].is_null());
        assert!(payload["plan"].is_null());
    }

    #[tokio::test]
    async fn completed_plan_can_be_cleared_without_removing_goal() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-plan-clear");
        let workspace_path = std::env::temp_dir().join("magi-goal-plan-clear");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-plan-clear");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal plan clear",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = state
            .session_store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-plan-clear"),
                "保留目标，仅清除计划",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("goal should be creatable");
        state
            .session_store
            .set_goal_status(&session_id, &goal.goal_id, GoalStatus::Complete)
            .expect("goal should complete");
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        let created = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("done".to_string()),
                    step: "已完成任务".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("done".to_string()),
                    step: "已完成任务".to_string(),
                    status: PlanItemStatus::Completed,
                }],
            })
            .expect("plan should complete");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/plan/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "workspaceId": workspace_id.to_string(),
                }),
            ))
            .await
            .expect("plan clear should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(goal.goal_id.as_str())
        );
        assert_eq!(payload["goal"]["status"].as_str(), Some("complete"));
        assert!(payload["plan"].is_null());
        assert!(state.session_store.plan(&session_id).is_none());
    }

    fn json_post(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request should build")
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&body).expect("response should be json")
    }
}
