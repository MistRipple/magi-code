//! Goal 应用服务：控制版本、持久化、执行和计划结算由 daemon 同一入口管理。
use crate::scope_binding::{SessionRequestScope, require_session_request_scope};
use crate::{
    dto::SessionScopeKindDto, errors::ApiError, session_activity::current_turn_status_is_active,
    state::ApiState,
};
use magi_conversation_runtime::TurnCommand;
use magi_core::{AccessProfile, DomainError, GoalId, SessionId, UtcMillis};
use magi_session_store::{GoalStatus, SessionGoal, SessionPlan};
use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GoalQuery {
    session_id: Option<String>,
    scope: SessionScopeKindDto,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoalSnapshot {
    session_id: String,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    observed_at: UtcMillis,
    goal: Option<SessionGoal>,
    plan: Option<SessionPlan>,
    allowed_actions: GoalAllowedActionsDto,
}

use magi_app_server_protocol::generated::GoalAllowedActions as GoalAllowedActionsDto;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GoalActionRequest {
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
    access_profile: Option<AccessProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GoalUpdateRequest {
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

pub(crate) async fn get_current_goal(
    state: &ApiState,
    query: GoalQuery,
) -> Result<GoalSnapshot, ApiError> {
    let scope = require_session_request_scope(
        state,
        query.session_id.as_deref(),
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    Ok(current_goal_response(state, scope))
}

fn current_goal_response(state: &ApiState, scope: SessionRequestScope) -> GoalSnapshot {
    let goal = state.session_store.current_visible_goal(&scope.session_id);
    let plan = goal.as_ref().and_then(|goal| {
        state
            .session_store
            .plan(&scope.session_id)
            .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id))
    });
    goal_mutation_response(state, scope, goal, plan)
}

pub(crate) fn allowed_actions(
    state: &ApiState,
    _session_id: &SessionId,
    goal: Option<&SessionGoal>,
    _plan: Option<&SessionPlan>,
) -> GoalAllowedActionsDto {
    let Some(goal) = goal else {
        return GoalAllowedActionsDto::default();
    };
    let resume_state_allowed = matches!(goal.status, GoalStatus::Paused | GoalStatus::Blocked);
    GoalAllowedActionsDto {
        can_edit: goal.status.is_unfinished(),
        can_pause: goal.status == GoalStatus::Active,
        can_resume: resume_state_allowed && state.session_turn_dispatcher().is_some(),
        can_clear: true,
    }
}

pub(crate) async fn update_current_goal(
    state: &ApiState,
    request: GoalUpdateRequest,
) -> Result<GoalSnapshot, ApiError> {
    let scope = require_session_request_scope(
        state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn_commit(&scope.session_id).await;
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
    state
        .settle_revoked_goal_agent_runs(&scope.session_id)
        .await?;
    let plan = state
        .session_store
        .plan(&scope.session_id)
        .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id));
    Ok(goal_mutation_response(state, scope, Some(goal), plan))
}

pub(crate) async fn pause_current_goal(
    state: &ApiState,
    request: GoalActionRequest,
) -> Result<GoalSnapshot, ApiError> {
    let scope = require_session_request_scope(
        state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn_commit(&scope.session_id).await;
    let execution_to_interrupt = goal_execution_to_interrupt(
        state,
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
    if let Some((root_task_id, turn_id)) = execution_to_interrupt {
        // 同会话可以正在处理 Goal 以外的显式输入。暂停 Goal 只结算其
        // 当前拥有的执行；不能按 session 无条件取消另一轮的工具/租约。
        if let Some(root_task_id) = root_task_id.as_ref() {
            state.cancel_task_execution_scope_resources(
                &scope.session_id,
                root_task_id,
                magi_browser_authority::BrowserLeaseEndReason::GoalPaused,
            );
        } else {
            state.cancel_conversation_execution_resources(
                &scope.session_id,
                magi_browser_authority::BrowserLeaseEndReason::GoalPaused,
            );
        }
        if let Some(root_task_id) = root_task_id
            && let Some(manager) = state.runner_manager()
            && let Err(error) = manager.kill_execution_scope(root_task_id.as_str())
        {
            tracing::warn!(
                session_id = %scope.session_id,
                task_id = %root_task_id,
                ?error,
                "暂停 Goal 时终止执行树失败，继续通过 Turn 取消信号收口"
            );
        }
        state
            .turn_event_sink()
            .interrupt_turn_by_user(&scope.session_id)
            .map_err(|error| {
                ApiError::internal_assembly("暂停 Goal 时中断当前 Turn 失败", error)
            })?;
        state
            .turn_coordinator()
            .close_session_turn_input(&scope.session_id, &turn_id);
        if let Ok(attempt) = state
            .turn_coordinator()
            .current_attempt(&scope.session_id, &turn_id)
            && let Err(error) = state.turn_coordinator().execute_command(
                &scope.session_id,
                TurnCommand::Cancel {
                    attempt: attempt.clone(),
                },
            )
        {
            tracing::error!(
                session_id = %scope.session_id,
                turn_id = %turn_id,
                %error,
                "暂停 Goal 时 Coordinator 取消收口失败"
            );
        }
    }
    publish_goal_plan_if_present(state, &scope, plan.as_ref());
    state.persist_session_state_checkpoint("goal_paused")?;
    state
        .settle_revoked_goal_agent_runs(&scope.session_id)
        .await?;
    Ok(goal_mutation_response(state, scope, Some(goal), plan))
}

fn goal_execution_to_interrupt(
    state: &ApiState,
    session_id: &SessionId,
    goal_id: &GoalId,
) -> Option<(Option<magi_core::TaskId>, String)> {
    let sidecar = state.session_store.runtime_sidecar(session_id)?;
    let current_turn = sidecar.current_turn.as_ref()?;
    if !current_turn_status_is_active(&current_turn.status) {
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

pub(crate) async fn resume_current_goal(
    state: &ApiState,
    request: GoalActionRequest,
) -> Result<GoalSnapshot, ApiError> {
    let scope = require_session_request_scope(
        state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn_commit(&scope.session_id).await;
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
    if !matches!(current.status, GoalStatus::Paused | GoalStatus::Blocked) {
        return Err(ApiError::InvalidInput(
            "只有暂停或阻塞目标可以继续执行".to_string(),
        ));
    }
    crate::turn_service::ensure_goal_continuation_runtime_available(state, &scope.session_id)?;
    let (_goal, _, resume_checkpoint) = state
        .session_store
        .resume_goal_with_plan(
            &scope.session_id,
            &GoalId::new(request.goal_id),
            request.expected_revision,
            request.expected_plan_revision,
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
        && let Err(error) =
            crate::turn_service::resume_active_goal_continuation_turn_after_turn_commit(
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
    publish_goal_plan_if_present(state, &scope, plan.as_ref());
    state.persist_session_state_checkpoint("goal_resumed")?;
    Ok(goal_mutation_response(state, scope, Some(goal), plan))
}

pub(crate) async fn clear_current_goal(
    state: &ApiState,
    request: GoalActionRequest,
) -> Result<GoalSnapshot, ApiError> {
    let scope = require_session_request_scope(
        state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn_commit(&scope.session_id).await;
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
    state
        .settle_revoked_goal_agent_runs(&scope.session_id)
        .await?;
    Ok(goal_mutation_response(state, scope, None, None))
}

pub(crate) async fn clear_current_plan(
    state: &ApiState,
    request: GoalActionRequest,
) -> Result<GoalSnapshot, ApiError> {
    let scope = require_session_request_scope(
        state,
        request.session_id.as_deref(),
        request.scope,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _session_turn_guard = state.lock_session_turn_commit(&scope.session_id).await;
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
    Ok(current_goal_response(state, scope))
}

fn goal_mutation_response(
    state: &ApiState,
    scope: SessionRequestScope,
    goal: Option<SessionGoal>,
    plan: Option<SessionPlan>,
) -> GoalSnapshot {
    let allowed_actions = allowed_actions(state, &scope.session_id, goal.as_ref(), plan.as_ref());
    GoalSnapshot {
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

/// 单一 wire 投影边界供 HTTP 与 App Server 共用；领域值不在客户端重新解释。
pub(crate) fn snapshot_to_wire(
    snapshot: GoalSnapshot,
) -> Result<magi_app_server_protocol::GoalSnapshot, ApiError> {
    serde_json::to_value(snapshot)
        .and_then(serde_json::from_value)
        .map_err(|error| ApiError::internal_assembly("Goal 快照不符合协议", error))
}
