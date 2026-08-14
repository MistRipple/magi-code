use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{AccessProfile, DomainError, GoalId, SessionId, UtcMillis};
use magi_session_store::{GoalStatus, SessionGoal, SessionPlan};
use serde::{Deserialize, Serialize};

use super::session_scope::{SessionRequestScope, require_session_request_scope};
use crate::{dto::SessionScopeKindDto, errors::ApiError, state::ApiState};

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
    scope: SessionScopeKindDto,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentGoalResponseDto {
    session_id: String,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    observed_at: UtcMillis,
    goal: Option<SessionGoal>,
    plan: Option<SessionPlan>,
    allowed_actions: GoalAllowedActionsDto,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalAllowedActionsDto {
    can_edit: bool,
    can_pause: bool,
    can_resume: bool,
    can_clear: bool,
    requires_budget_increase: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalActionRequest {
    session_id: Option<String>,
    scope: SessionScopeKindDto,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    goal_id: String,
    expected_revision: u64,
    #[serde(default)]
    expected_plan_revision: Option<u64>,
    #[serde(default)]
    new_token_budget: Option<u64>,
    #[serde(default)]
    access_profile: Option<AccessProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalUpdateRequest {
    session_id: Option<String>,
    scope: SessionScopeKindDto,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    goal_id: String,
    expected_revision: u64,
    #[serde(default, rename = "expectedPlanRevision")]
    _expected_plan_revision: Option<u64>,
    objective: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalMutationResponseDto {
    session_id: String,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    observed_at: UtcMillis,
    goal: Option<SessionGoal>,
    plan: Option<SessionPlan>,
    allowed_actions: GoalAllowedActionsDto,
}

async fn get_current_goal(
    State(state): State<ApiState>,
    Query(query): Query<GoalQuery>,
) -> Result<Json<CurrentGoalResponseDto>, ApiError> {
    let scope = require_session_request_scope(
        &state,
        query.session_id.as_deref(),
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    Ok(Json(current_goal_response(&state, scope)))
}

fn current_goal_response(state: &ApiState, scope: SessionRequestScope) -> CurrentGoalResponseDto {
    let goal = state.session_store.current_visible_goal(&scope.session_id);
    let plan = goal.as_ref().and_then(|goal| {
        state
            .session_store
            .plan(&scope.session_id)
            .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id))
    });
    let allowed_actions = allowed_actions(state, &scope.session_id, goal.as_ref(), plan.as_ref());
    CurrentGoalResponseDto {
        session_id: scope.session_id.to_string(),
        workspace_id: scope.workspace_id().map(|id| id.to_string()),
        workspace_path: scope.workspace_path(),
        observed_at: UtcMillis::now(),
        goal,
        plan,
        allowed_actions,
    }
}

fn allowed_actions(
    state: &ApiState,
    _session_id: &SessionId,
    goal: Option<&SessionGoal>,
    _plan: Option<&SessionPlan>,
) -> GoalAllowedActionsDto {
    let Some(goal) = goal else {
        return GoalAllowedActionsDto::default();
    };
    let resume_state_allowed = matches!(
        goal.status,
        GoalStatus::Paused
            | GoalStatus::Blocked
            | GoalStatus::UsageLimited
            | GoalStatus::BudgetLimited
    );
    GoalAllowedActionsDto {
        can_edit: goal.status.is_unfinished(),
        can_pause: goal.status == GoalStatus::Active,
        can_resume: resume_state_allowed && state.session_turn_dispatcher().is_some(),
        can_clear: true,
        requires_budget_increase: goal.status == GoalStatus::BudgetLimited,
    }
}

async fn update_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalUpdateRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_request_scope(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn(&scope.session_id).await;
    let goal = state
        .session_store
        .update_goal_objective_if_revision(
            &scope.session_id,
            &GoalId::new(request.goal_id),
            request.objective,
            Some(request.expected_revision),
        )
        .map_err(map_goal_domain_error)?;
    state.persist_session_state_checkpoint("goal_updated")?;
    let plan = state
        .session_store
        .plan(&scope.session_id)
        .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id));
    Ok(Json(goal_mutation_response(
        &state,
        scope,
        Some(goal),
        plan,
    )))
}

async fn pause_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_request_scope(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn(&scope.session_id).await;
    let execution_to_interrupt = goal_execution_to_interrupt(
        &state,
        &scope.session_id,
        &GoalId::new(request.goal_id.clone()),
    );
    let (goal, plan) = state
        .session_store
        .pause_goal_with_plan(
            &scope.session_id,
            &GoalId::new(request.goal_id),
            request.expected_revision,
            request.expected_plan_revision,
        )
        .map_err(map_goal_domain_error)?;
    state.cancel_execution_resources(
        Some(&scope.session_id),
        None,
        None,
        magi_browser_authority::BrowserLeaseEndReason::GoalPaused,
    );
    if let Some((root_task_id, turn_id)) = execution_to_interrupt {
        if let Some(root_task_id) = root_task_id
            && let Some(manager) = state.runner_manager()
            && let Err(error) = manager.kill_tree(root_task_id.as_str())
        {
            tracing::warn!(
                session_id = %scope.session_id,
                task_id = %root_task_id,
                ?error,
                "暂停 Goal 时终止执行树失败，继续通过 Turn 取消信号收口"
            );
        }
        state
            .session_store
            .interrupt_current_turn_by_user(&scope.session_id)
            .map_err(|error| {
                ApiError::internal_assembly("暂停 Goal 时中断当前 Turn 失败", error)
            })?;
        state
            .conversation_registry
            .close_session_turn_input(&scope.session_id, &turn_id);
        let _ = super::finalize_session_turn(&state, &scope.session_id, false);
    }
    publish_goal_plan_if_present(&state, &scope, plan.as_ref());
    state.persist_session_state_checkpoint("goal_paused")?;
    Ok(Json(goal_mutation_response(
        &state,
        scope,
        Some(goal),
        plan,
    )))
}

fn goal_execution_to_interrupt(
    state: &ApiState,
    session_id: &SessionId,
    goal_id: &GoalId,
) -> Option<(Option<magi_core::TaskId>, String)> {
    let sidecar = state.session_store.runtime_sidecar(session_id)?;
    let current_turn = sidecar.current_turn.as_ref()?;
    if current_turn.status.is_empty()
        || matches!(
            current_turn.status.trim().to_ascii_lowercase().as_str(),
            "completed"
                | "complete"
                | "succeeded"
                | "success"
                | "failed"
                | "error"
                | "interrupted"
                | "cancelled"
                | "canceled"
                | "superseded"
        )
    {
        return None;
    }
    let root_task_id = sidecar
        .active_execution_chain
        .as_ref()
        .map(|chain| chain.root_task_id.clone());
    let owner_matches = std::iter::once(current_turn.turn_id.as_str())
        .chain(root_task_id.as_ref().map(|task_id| task_id.as_str()))
        .any(|owner_id| {
            state
                .session_store
                .active_goal_for_execution_owner(session_id, owner_id)
                .is_some_and(|goal| &goal.goal_id == goal_id)
        });
    owner_matches.then(|| (root_task_id, current_turn.turn_id.clone()))
}

async fn resume_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_request_scope(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn(&scope.session_id).await;
    let current = state
        .session_store
        .current_goal(&scope.session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可操作目标".to_string()))?;
    if current.goal_id.as_str() != request.goal_id
        || current.control_revision != request.expected_revision
    {
        return Err(ApiError::InvalidInput(
            "目标状态已变化，请刷新后重试".to_string(),
        ));
    }
    if !matches!(
        current.status,
        GoalStatus::Paused
            | GoalStatus::Blocked
            | GoalStatus::UsageLimited
            | GoalStatus::BudgetLimited
    ) {
        return Err(ApiError::InvalidInput(
            "只有暂停、阻塞、用量受限或预算受限目标可以继续执行".to_string(),
        ));
    }
    if current.status == GoalStatus::BudgetLimited
        && request
            .new_token_budget
            .is_none_or(|budget| budget <= current.tokens_used)
    {
        return Err(ApiError::InvalidInput(
            "预算受限目标必须设置高于已用 Token 的新预算".to_string(),
        ));
    }
    super::sessions::ensure_goal_continuation_runtime_available(&state, &scope.session_id)?;
    let (_goal, _, resume_checkpoint) = state
        .session_store
        .resume_goal_with_plan(
            &scope.session_id,
            &GoalId::new(request.goal_id),
            request.expected_revision,
            request.expected_plan_revision,
            request.new_token_budget,
            request.access_profile,
        )
        .map_err(map_goal_domain_error)?;
    state.persist_session_state_checkpoint("goal_resume_requested")?;
    let can_start_now = state.queued_regular_session_turn_count(&scope.session_id) == 0
        && state
            .session_store
            .ensure_current_turn_acceptance_available(&scope.session_id)
            .is_ok();
    if can_start_now
        && let Err(error) = super::sessions::resume_active_goal_continuation_turn(
            state.clone(),
            scope.session_id.clone(),
            scope.workspace_id(),
        )
        .await
    {
        state
            .session_store
            .rollback_goal_resume(resume_checkpoint)
            .map_err(|rollback_error| {
                ApiError::internal_assembly("恢复目标失败后的状态回滚失败", rollback_error)
            })?;
        state.persist_session_state_checkpoint("goal_resume_rolled_back")?;
        return Err(error);
    }
    let goal = state
        .session_store
        .current_goal(&scope.session_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可操作目标".to_string()))?;
    let plan = state
        .session_store
        .plan(&scope.session_id)
        .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id));
    publish_goal_plan_if_present(&state, &scope, plan.as_ref());
    state.persist_session_state_checkpoint("goal_resumed")?;
    Ok(Json(goal_mutation_response(
        &state,
        scope,
        Some(goal),
        plan,
    )))
}

async fn clear_current_goal(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<GoalMutationResponseDto>, ApiError> {
    let scope = require_session_request_scope(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn(&scope.session_id).await;
    let (_cleared_goal, cleared_plan) = state
        .session_store
        .clear_goal_with_plan(
            &scope.session_id,
            &GoalId::new(request.goal_id),
            request.expected_revision,
            request.expected_plan_revision,
        )
        .map_err(map_goal_domain_error)?;
    if let Some(plan) = cleared_plan.as_ref() {
        let workspace_id = scope.workspace_id();
        magi_plan::publish_plan_cleared_event(&state.event_bus, plan, workspace_id.as_ref());
    }
    state.persist_session_state_checkpoint("goal_cleared")?;
    Ok(Json(goal_mutation_response(&state, scope, None, None)))
}

async fn clear_current_plan(
    State(state): State<ApiState>,
    Json(request): Json<GoalActionRequest>,
) -> Result<Json<CurrentGoalResponseDto>, ApiError> {
    let scope = require_session_request_scope(
        &state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn(&scope.session_id).await;
    let plan_store =
        magi_plan::PlanStore::new(state.session_store.clone(), scope.session_id.clone());
    let cleared_plan = plan_store.snapshot();
    if cleared_plan.as_ref().is_some_and(|plan| {
        plan.goal_id.as_ref().is_some_and(|goal_id| {
            state
                .session_store
                .current_unfinished_goal(&scope.session_id)
                .is_some_and(|goal| &goal.goal_id == goal_id)
        })
    }) {
        return Err(ApiError::InvalidInput(
            "未完成目标的绑定计划不能单独清除；请清除目标或先完成目标".to_string(),
        ));
    }
    plan_store
        .clear(request.expected_plan_revision)
        .map_err(map_plan_error)?;
    if let Some(plan) = cleared_plan.as_ref() {
        let workspace_id = scope.workspace_id();
        magi_plan::publish_plan_cleared_event(&state.event_bus, plan, workspace_id.as_ref());
    }
    state.persist_session_state_checkpoint("session_plan_cleared")?;
    Ok(Json(current_goal_response(&state, scope)))
}

fn goal_mutation_response(
    state: &ApiState,
    scope: SessionRequestScope,
    goal: Option<SessionGoal>,
    plan: Option<SessionPlan>,
) -> GoalMutationResponseDto {
    let allowed_actions = allowed_actions(state, &scope.session_id, goal.as_ref(), plan.as_ref());
    GoalMutationResponseDto {
        session_id: scope.session_id.to_string(),
        workspace_id: scope.workspace_id().map(|id| id.to_string()),
        workspace_path: scope.workspace_path(),
        observed_at: UtcMillis::now(),
        goal,
        plan,
        allowed_actions,
    }
}

fn publish_goal_plan_if_present(
    state: &ApiState,
    scope: &SessionRequestScope,
    plan: Option<&SessionPlan>,
) {
    if let Some(plan) = plan {
        let workspace_id = scope.workspace_id();
        magi_plan::publish_plan_event(
            &state.event_bus,
            magi_plan::plan_event_type(plan),
            plan,
            workspace_id.as_ref(),
            None,
            None,
        );
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
    use magi_bridge_client::ModelResponse;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, ModelBridgeClient, ModelInvocationRequest,
        ModelStreamingDelta,
    };
    use magi_conversation_runtime::{
        task_execution_dispatcher::{
            ExecutionPipeline, LlmTaskDispatcher, LlmTaskDispatcherDependencies,
        },
        task_runner_bridge::{EventBasedResultReceiver, TaskDispatcher},
    };
    use magi_core::PlanItemStatus;
    use magi_core::{AbsolutePath, MissionId, SessionId, UtcMillis, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
    use magi_session_store::{ActiveExecutionTurn, GoalRevisionExpectation, SessionStore};
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;

    fn create_test_goal(
        state: &ApiState,
        session_id: &SessionId,
        turn_id: &str,
        objective: &str,
        token_budget: Option<u64>,
    ) -> SessionGoal {
        let (_, thread_id) =
            state
                .session_store
                .ensure_session_mission(session_id, UtcMillis::now(), || {
                    MissionId::new(format!("mission-{session_id}"))
                });
        state
            .session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                turn_id,
                objective,
                magi_core::AccessProfile::Restricted,
                token_budget,
            )
            .expect("test goal should be creatable")
    }

    struct RecordingFailingModelClient {
        calls: Arc<AtomicUsize>,
    }

    impl ModelBridgeClient for RecordingFailingModelClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32007),
                message: "目标续跑执行测试".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    fn state_with_recording_goal_dispatcher(calls: Arc<AtomicUsize>) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let task_store = Arc::new(TaskStore::new());
        let state = ApiState::new(
            "magi-test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            Arc::clone(&governance),
        )
        .with_task_store(Arc::clone(&task_store));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let skill_dispatch_runtime = magi_skill_runtime::SkillDispatchRuntime::new(
            tool_registry.clone(),
            magi_bridge_client::BridgeDispatchRuntime::new(),
        );
        let execution_runtime = orchestrator
            .execution_runtime(
                WorkerRuntime::new(Arc::clone(&event_bus)),
                tool_registry.clone(),
                skill_dispatch_runtime,
            )
            .with_task_store(Arc::clone(&task_store));
        let result_receiver = Arc::new(EventBasedResultReceiver::new());
        let dispatcher = Arc::new(
            LlmTaskDispatcher::new(
                event_bus,
                ExecutionPipeline {
                    orchestrator,
                    execution_runtime,
                    memory_store: magi_memory_store::MemoryStore::new(),
                },
                LlmTaskDispatcherDependencies {
                    session_store: Arc::clone(&session_store),
                    execution_registry: state.task_execution_registry().clone(),
                    result_receiver: Arc::clone(&result_receiver),
                    spawn_graph: Arc::clone(&state.spawn_graph),
                    conversation_registry: Arc::clone(&state.conversation_registry),
                    agent_role_registry: Arc::clone(&state.agent_role_registry),
                },
                std::env::temp_dir().join("magi-goal-resume-dispatcher"),
            )
            .with_model_bridge_client(Arc::new(RecordingFailingModelClient { calls }))
            .with_workspace_registry(workspace_store)
            .with_tool_registry(tool_registry),
        );
        let state_for_workers = state.clone();
        let runner_dispatcher: Arc<dyn TaskDispatcher> = dispatcher.clone();
        let runner_manager = crate::state::RunnerManager::with_dispatcher_and_worker_catalog(
            task_store,
            session_store,
            Arc::new(move || state_for_workers.task_worker_catalog()),
            runner_dispatcher,
            result_receiver,
        )
        .with_agent_role_registry(Arc::clone(&state.agent_role_registry));
        state
            .with_runner_manager(runner_manager)
            .with_session_turn_dispatcher(dispatcher)
    }

    #[test]
    fn paused_goal_resume_action_remains_available_while_session_turn_is_running() {
        let state = state_with_recording_goal_dispatcher(Arc::new(AtomicUsize::new(0)));
        let session_id = SessionId::new("session-goal-resume-action-running");
        state
            .session_store
            .create_session(session_id.clone(), "goal resume action running")
            .expect("session should create");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-resume-action-owner",
            "验证运行中不展示 Goal 恢复操作",
            None,
        );
        let paused_goal = state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("goal should pause")
            .0;
        assert!(
            allowed_actions(&state, &session_id, Some(&paused_goal), None).can_resume,
            "空闲会话里的暂停 Goal 应允许恢复"
        );
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-goal-resume-action-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    status: "running".to_string(),
                    completed_at: None,
                    user_message: Some("会话正在执行".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("running turn should persist");

        assert!(
            allowed_actions(&state, &session_id, Some(&paused_goal), None).can_resume,
            "已有运行 Turn 时恢复请求应进入 waiting，而不是禁用操作"
        );
    }

    #[tokio::test]
    async fn busy_session_accepts_goal_resume_as_waiting() {
        let state = state_with_recording_goal_dispatcher(Arc::new(AtomicUsize::new(0)));
        let workspace_id = WorkspaceId::new("workspace-goal-resume-waiting");
        let workspace_path = std::env::temp_dir().join("magi-goal-resume-waiting");
        std::fs::create_dir_all(&workspace_path).expect("workspace directory should create");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-resume-waiting");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal resume waiting",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-resume-waiting-owner",
            "验证忙碌会话恢复等待",
            None,
        );
        let paused = state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("goal should pause")
            .0;
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-unrelated-running".to_string(),
                    turn_seq: 2,
                    accepted_at: UtcMillis(2),
                    status: "running".to_string(),
                    completed_at: None,
                    user_message: Some("执行其他任务".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("running turn should persist");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal.goal_id,
                    "expectedRevision": paused.control_revision,
                }),
            ))
            .await
            .expect("resume should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert_eq!(
            payload["goal"]["continuation"]["phase"].as_str(),
            Some("waiting")
        );
        assert!(payload["goal"]["timingStartedAt"].is_null());
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .all(|turn| !turn.turn_id.starts_with("turn-goal-continuation-")),
            "busy resume must not fabricate a continuation turn"
        );
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
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-route",
            "完成 Goal API",
            Some(2048),
        );

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
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
    async fn personal_goal_routes_use_personal_scope_without_workspace_binding() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let session_id = SessionId::new("session-personal-goal-routes");
        state
            .session_store
            .create_session(session_id.clone(), "个人目标会话")
            .expect("personal session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-personal-goal-routes",
            "完成个人会话目标",
            Some(4096),
        );
        let app = Router::new().merge(routes()).with_state(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=personal&sessionId={}",
                        session_id
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("personal goal should read");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert!(payload["workspacePath"].is_null());
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(goal.goal_id.as_str())
        );

        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/update",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": goal.control_revision,
                    "objective": "更新个人会话目标",
                }),
            ))
            .await
            .expect("personal goal should update");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert_eq!(
            payload["goal"]["objective"].as_str(),
            Some("更新个人会话目标")
        );
        let updated_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("updated revision should exist");

        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/pause",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": updated_revision,
                }),
            ))
            .await
            .expect("personal goal should pause");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("paused"));
        let paused_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("paused revision should exist");

        let response = app
            .oneshot(json_post(
                "/goals/current/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": paused_revision,
                }),
            ))
            .await
            .expect("personal goal should clear");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert!(payload["goal"].is_null());
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
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-plan-route",
            "完成稳定计划展示",
            None,
        );
        let plan_store = magi_plan::PlanStore::new(Arc::clone(&session_store), session_id.clone());
        let created = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
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
        assert_eq!(created.goal_id.as_ref(), Some(&goal.goal_id));
        let updated = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
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
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
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
    async fn current_goal_actions_edit_pause_and_reject_resume_without_runtime() {
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
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-actions",
            "原目标",
            Some(4096),
        );
        let goal_id = goal.goal_id.to_string();
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
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
        let update_body = serde_json::json!({
            "sessionId": session_id.to_string(),
            "scope": "workspace",
            "workspaceId": workspace_id.to_string(),
            "goalId": goal_id,
            "expectedRevision": goal.control_revision,
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
        let updated_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("updated goal revision");
        let pause_body = serde_json::json!({
            "sessionId": session_id.to_string(),
            "scope": "workspace",
            "workspaceId": workspace_id.to_string(),
            "goalId": goal_id,
            "expectedRevision": updated_revision,
            "expectedPlanRevision": 1,
        });
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-goal-actions".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    status: "running".to_string(),
                    completed_at: None,
                    user_message: Some("执行当前目标".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("running goal turn should persist");

        let response = app
            .clone()
            .oneshot(json_post("/goals/current/pause", pause_body))
            .await
            .expect("pause should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("paused"));
        assert_eq!(
            state
                .session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .map(|turn| turn.status),
            Some("cancelled".to_string()),
            "手动暂停 Goal 必须同时终止所属运行 Turn"
        );
        let paused_revision = payload["goal"]["controlRevision"]
            .as_u64()
            .expect("paused goal revision");
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
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal_id,
                    "expectedRevision": paused_revision,
                    "expectedPlanRevision": 2,
                }),
            ))
            .await
            .expect("resume should complete");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let payload = response_json(response).await;
        assert_eq!(payload["error_code"].as_str(), Some("CONFLICT"));
        assert_eq!(
            state
                .session_store
                .current_visible_goal(&session_id)
                .expect("goal should remain visible")
                .status,
            GoalStatus::Paused
        );
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Paused
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .all(|turn| !turn.turn_id.starts_with("turn-goal-continuation-")),
            "执行器不可用时不能留下伪造的目标续跑 Turn"
        );

        let paused_plan_revision = plan_store
            .snapshot()
            .expect("paused plan should remain")
            .revision;
        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/plan/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal_id,
                    "expectedRevision": paused_revision,
                    "expectedPlanRevision": paused_plan_revision,
                }),
            ))
            .await
            .expect("unfinished goal plan clear should complete");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(plan_store.snapshot().is_some());

        let paused_goal = state
            .session_store
            .current_visible_goal(&session_id)
            .expect("goal should remain available after rejected resume");
        let paused_goal_id = paused_goal.goal_id.to_string();
        let paused_plan = plan_store.snapshot().expect("paused plan should exist");
        let (active_goal, resumed_plan, _) = state
            .session_store
            .resume_goal_with_plan(
                &session_id,
                &paused_goal.goal_id,
                paused_goal.control_revision,
                Some(paused_plan.revision),
                None,
                None,
            )
            .expect("matching Goal and Plan revisions should permit activation");
        let resumed_plan = resumed_plan.expect("bound plan should resume");
        let completed_plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(resumed_plan.plan_id.to_string()),
                expected_revision: Some(resumed_plan.revision),
                expected_goal_id: Some(active_goal.goal_id.to_string()),
                expected_goal_control_revision: Some(active_goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("execute".to_string()),
                    step: "执行当前目标".to_string(),
                    status: PlanItemStatus::Completed,
                }],
            })
            .expect("plan should complete");
        let completed_goal = state
            .session_store
            .complete_goal(
                &session_id,
                &active_goal.goal_id,
                GoalRevisionExpectation::new(
                    active_goal.control_revision,
                    Some(completed_plan.revision),
                ),
                "turn-goal-actions",
                "目标已完成",
                vec!["test:goal-actions".to_string()],
            )
            .expect("goal should be markable complete");
        let response = app
            .clone()
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": paused_goal_id,
                    "expectedRevision": completed_goal.control_revision,
                    "expectedPlanRevision": completed_plan.revision,
                }),
            ))
            .await
            .expect("completed goal resume should complete");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_eq!(payload["error_code"].as_str(), Some("INPUT_INVALID"));
        assert_eq!(
            payload["message"].as_str(),
            Some("只有暂停、阻塞、用量受限或预算受限目标可以继续执行")
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .all(|turn| !turn.turn_id.starts_with("turn-goal-continuation-")),
            "已完成目标不能创建续跑 Turn"
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
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
            .oneshot(json_post(
                "/goals/current/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": paused_goal_id,
                    "expectedRevision": completed_goal.control_revision,
                    "expectedPlanRevision": completed_plan.revision,
                }),
            ))
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
                        "/goals/current?scope=workspace&sessionId={}&workspaceId={}",
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
    async fn resuming_paused_goal_starts_real_continuation_turn() {
        let model_calls = Arc::new(AtomicUsize::new(0));
        let state = state_with_recording_goal_dispatcher(Arc::clone(&model_calls));
        let workspace_id = WorkspaceId::new("workspace-goal-real-resume");
        let workspace_path = std::env::temp_dir().join("magi-goal-real-resume");
        std::fs::create_dir_all(&workspace_path).expect("workspace directory should create");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_path.display().to_string()),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-goal-real-resume");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "goal real resume",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-real-resume",
            "验证目标恢复会启动续跑任务",
            Some(4096),
        );
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("resume".to_string()),
                    step: "恢复目标续跑".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should be creatable");
        let goal = state
            .session_store
            .active_goal(&session_id)
            .expect("goal should be active initially");
        state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(1))
            .expect("goal and plan should pause");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal.goal_id,
                    "expectedRevision": goal.control_revision + 1,
                    "expectedPlanRevision": 2,
                    "accessProfile": "full_access",
                }),
            ))
            .await
            .expect("resume should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        assert_eq!(
            payload["goal"]["accessProfile"].as_str(),
            Some("full_access")
        );
        assert_eq!(
            plan_store.snapshot().expect("plan should exist").state,
            magi_core::PlanState::Active
        );
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .iter()
                .any(|turn| turn.turn_id.starts_with("turn-goal-continuation-")),
            "恢复后必须接受一轮目标续跑 Turn"
        );
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.turn.task.accepted"
                        && event.payload["session_id"].as_str() == Some(session_id.as_str())
                        && event.payload["goal_continuation"] == true
                }),
            "恢复后必须发布 Turn 已接受事件"
        );
        for _ in 0..20 {
            if model_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            model_calls.load(Ordering::SeqCst) > 0,
            "恢复目标后执行器必须实际发起模型调用"
        );
    }

    #[tokio::test]
    async fn personal_goal_resume_starts_continuation_without_workspace_binding() {
        let model_calls = Arc::new(AtomicUsize::new(0));
        let state = state_with_recording_goal_dispatcher(Arc::clone(&model_calls));
        let session_id = SessionId::new("session-personal-goal-resume");
        state
            .session_store
            .create_session(session_id.clone(), "个人目标恢复")
            .expect("personal session should be creatable");
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-personal-goal-resume",
            "验证个人目标恢复",
            Some(4096),
        );
        let paused = state
            .session_store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, None)
            .expect("personal goal should pause")
            .0;

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/resume",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "personal",
                    "goalId": goal.goal_id,
                    "expectedRevision": paused.control_revision,
                    "accessProfile": "full_access",
                }),
            ))
            .await
            .expect("personal goal resume should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert!(payload["workspaceId"].is_null());
        assert!(payload["workspacePath"].is_null());
        assert_eq!(payload["goal"]["status"].as_str(), Some("active"));
        for _ in 0..20 {
            if model_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(model_calls.load(Ordering::SeqCst) > 0);
        assert!(
            state
                .event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| {
                    event.event_type == "session.turn.task.accepted"
                        && event.session_id.as_ref() == Some(&session_id)
                        && event.workspace_id.is_none()
                })
        );
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
        let goal = create_test_goal(
            &state,
            &session_id,
            "turn-goal-plan-clear",
            "保留目标，仅清除计划",
            None,
        );
        let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
        let created = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("done".to_string()),
                    step: "已完成任务".to_string(),
                    status: PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should create");
        let completed_plan = plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: Some(created.plan_id.to_string()),
                expected_revision: Some(created.revision),
                expected_goal_id: Some(goal.goal_id.to_string()),
                expected_goal_control_revision: Some(goal.control_revision),
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("done".to_string()),
                    step: "已完成任务".to_string(),
                    status: PlanItemStatus::Completed,
                }],
            })
            .expect("plan should complete");
        let completed_goal = state
            .session_store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                GoalRevisionExpectation::new(goal.control_revision, Some(completed_plan.revision)),
                "turn-goal-plan-clear",
                "计划已完成",
                vec!["test:completed-plan-clear".to_string()],
            )
            .expect("goal should complete");

        let app = Router::new().merge(routes()).with_state(state.clone());
        let response = app
            .oneshot(json_post(
                "/goals/current/plan/clear",
                serde_json::json!({
                    "sessionId": session_id.to_string(),
                    "scope": "workspace",
                    "workspaceId": workspace_id.to_string(),
                    "goalId": goal.goal_id,
                    "expectedRevision": completed_goal.control_revision,
                    "expectedPlanRevision": completed_plan.revision,
                }),
            ))
            .await
            .expect("plan clear should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(
            payload["goal"]["goalId"].as_str(),
            Some(completed_goal.goal_id.as_str())
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
