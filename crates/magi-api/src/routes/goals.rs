use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{DomainError, GoalId, SessionId};
use magi_core::{TodoItem, TodoStatus};
use magi_session_store::{GoalStatus, SessionGoal};
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
        .route("/goals/current/todos/clear", post(clear_current_todos))
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
    todo_items: Vec<GoalTodoItemDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalTodoItemDto {
    content: String,
    active_form: String,
    status: String,
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
    let todo_items = current_goal_todo_items(state, &scope.session_id);
    CurrentGoalResponseDto {
        session_id: scope.session_id.to_string(),
        workspace_id: scope.workspace_id.to_string(),
        workspace_path: scope.workspace_path,
        goal,
        todo_items,
    }
}

fn current_goal_todo_items(state: &ApiState, session_id: &SessionId) -> Vec<GoalTodoItemDto> {
    state
        .session_store
        .todo_items(session_id)
        .into_iter()
        .map(todo_item_to_goal_dto)
        .collect()
}

fn todo_item_to_goal_dto(item: TodoItem) -> GoalTodoItemDto {
    GoalTodoItemDto {
        content: item.content,
        active_form: item.active_form,
        status: todo_status_label(item.status).to_string(),
    }
}

fn todo_status_label(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
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
    state
        .session_store
        .replace_todo_items(&scope.session_id, Vec::new())
        .map_err(map_goal_domain_error)?;
    state.persist_session_state_checkpoint("goal_cleared")?;
    Ok(Json(goal_mutation_response(scope, None)))
}

async fn clear_current_todos(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<CurrentGoalResponseDto>, ApiError> {
    let scope = require_session_workspace_scope(
        &state,
        request.session_id.as_deref(),
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        "清除当前任务清单",
    )?;
    state
        .session_store
        .replace_todo_items(&scope.session_id, Vec::new())
        .map_err(map_goal_domain_error)?;
    state.persist_session_state_checkpoint("goal_todos_cleared")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use magi_conversation_runtime::{
        ConversationRegistry,
        task_execution_dispatcher::{
            ExecutionPipeline, LlmTaskDispatcher, LlmTaskDispatcherDependencies,
        },
        task_execution_registry::TaskExecutionRegistry,
        task_runner_bridge::EventBasedResultReceiver,
    };
    use magi_core::{AbsolutePath, SessionId, ThreadId, WorkspaceId};
    use magi_core::{TodoItem, TodoStatus};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::OrchestratorService;
    use magi_session_store::SessionStore;
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_dispatcher(
        event_bus: Arc<InMemoryEventBus>,
        session_store: Arc<SessionStore>,
    ) -> Arc<LlmTaskDispatcher> {
        let governance = Arc::new(GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let skill_runtime = magi_skill_runtime::SkillDispatchRuntime::new(
            tool_registry.clone(),
            magi_bridge_client::BridgeDispatchRuntime::new(),
        );
        let execution_runtime = orchestrator.execution_runtime(
            magi_worker_runtime::WorkerRuntime::new(Arc::clone(&event_bus)),
            tool_registry,
            skill_runtime,
        );
        Arc::new(LlmTaskDispatcher::new(
            event_bus,
            ExecutionPipeline {
                orchestrator,
                execution_runtime,
                memory_store: MemoryStore::new(),
            },
            LlmTaskDispatcherDependencies {
                session_store,
                execution_registry: TaskExecutionRegistry::default(),
                result_receiver: Arc::new(EventBasedResultReceiver::new()),
                spawn_graph: Arc::new(std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new())),
                conversation_registry: Arc::new(ConversationRegistry::new()),
                agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            },
            std::env::temp_dir().join("magi-goal-route-dispatcher"),
        ))
    }

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
        assert_eq!(
            payload["todoItems"]
                .as_array()
                .expect("todoItems should be array")
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn current_goal_route_exposes_goal_todo_ledger_snapshot() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let dispatcher = test_dispatcher(Arc::clone(&event_bus), Arc::clone(&session_store));
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_session_turn_dispatcher(Arc::clone(&dispatcher));
        let workspace_id = WorkspaceId::new("workspace-goal-todo-route");
        let workspace_path = std::env::temp_dir().join("magi-goal-todo-route");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-todo-route");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal todo route",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        state
            .session_store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-todo-route"),
                "完成目标任务清单展示",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("goal should be creatable");
        state
            .session_store
            .replace_todo_items(
                &session_id,
                vec![
                    TodoItem::new("梳理目标", "正在梳理目标", TodoStatus::Completed),
                    TodoItem::new("验证抽屉展示", "正在验证抽屉展示", TodoStatus::InProgress),
                ],
            )
            .expect("todo list should persist");

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
            payload["goal"]["objective"].as_str(),
            Some("完成目标任务清单展示")
        );
        assert_eq!(
            payload["todoItems"][0]["content"].as_str(),
            Some("梳理目标")
        );
        assert_eq!(
            payload["todoItems"][0]["status"].as_str(),
            Some("completed")
        );
        assert_eq!(
            payload["todoItems"][1]["activeForm"].as_str(),
            Some("正在验证抽屉展示")
        );
        assert_eq!(
            payload["todoItems"][1]["status"].as_str(),
            Some("in_progress")
        );
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

        let response = app
            .clone()
            .oneshot(json_post("/goals/current/resume", scope_body.clone()))
            .await
            .expect("resume should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));

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
        state
            .session_store
            .replace_todo_items(
                &session_id,
                vec![TodoItem::new(
                    "完成后待关闭的任务",
                    "正在关闭任务",
                    TodoStatus::Completed,
                )],
            )
            .expect("todo list should write before clear");

        let response = app
            .clone()
            .oneshot(json_post("/goals/current/clear", scope_body))
            .await
            .expect("clear should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["goal"].is_null());
        assert!(state.session_store.todo_items(&session_id).is_empty());

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
        assert_eq!(payload["todoItems"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn completed_todo_list_can_be_cleared_without_removing_goal() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-goal-todo-clear");
        let workspace_path = std::env::temp_dir().join("magi-goal-todo-clear");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-todo-clear");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal todo clear",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = state
            .session_store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-todo-clear"),
                "保留目标，仅关闭任务清单",
                magi_core::AccessProfile::Restricted,
                None,
            )
            .expect("goal should be creatable");
        state
            .session_store
            .set_goal_status(&session_id, &goal.goal_id, GoalStatus::Complete)
            .expect("goal should complete");
        state
            .session_store
            .replace_todo_items(
                &session_id,
                vec![TodoItem::new(
                    "已完成任务",
                    "正在完成任务",
                    TodoStatus::Completed,
                )],
            )
            .expect("todo list should write");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/todos/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "workspaceId": workspace_id.to_string(),
                }),
            ))
            .await
            .expect("todo clear should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(goal.goal_id.as_str())
        );
        assert_eq!(payload["goal"]["status"].as_str(), Some("complete"));
        assert_eq!(payload["todoItems"].as_array().map(Vec::len), Some(0));
        assert!(state.session_store.todo_items(&session_id).is_empty());
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
