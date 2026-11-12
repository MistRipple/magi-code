//! Turn 业务服务。
//!
//! HTTP 和 Desktop App Server 只能在传输层做解析、鉴权和序列化，接纳与幂等
//! 必须经过同一个服务入口，避免两个协议维护不同的 Turn 生命周期。

use magi_browser_authority::BrowserToolKind;
use magi_conversation_runtime::session_writeback::publish_current_session_turn_item_event;
use magi_conversation_runtime::task_runner_bridge::TaskDispatcher;
use magi_conversation_runtime::{
    CoordinatorAdmission, CoordinatorCommandResult, CoordinatorTurnStatus, ExecutionProfile,
    SessionTurnCommand, SessionTurnExecutionRequest, TurnAdmission, TurnCommand,
};
use magi_conversation_runtime::{SessionTurnInputCommitError, SessionTurnInputError, UserSignal};
use magi_core::{
    DomainError, EventId, MissionId, SessionId, TaskCompletionContract, TaskRecoveryCheckpoint,
    TaskTier, UtcMillis, WorkerId, WorkspaceId, public_runtime_excerpt,
};
use magi_core::{SessionLifecycleStatus, TaskStatus};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::{
    ActiveExecutionTurn, ActiveExecutionTurnItem, CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn,
    CanonicalTurnItemKind, SessionGoal, TimelineEntryInput, TimelineEntryKind,
};
use serde_json::{Value, json};

use crate::scope_binding::{
    SessionScope, parse_session_id, require_session_record_in_scope, require_session_request_scope,
    resolve_explicit_session_scope,
};
use crate::task_dispatch::{
    SessionTaskSubmissionInput, accept_goal_continuation_task_submission,
    accept_session_task_submission_at, accepted_session_directory_entry,
    dispatch_accepted_canonical_event, schedule_session_task_dispatch,
};
use crate::{
    dto::{
        ContinueSessionRequest, ContinueSessionResponseDto, SessionDirectoryEntryDto,
        SessionInterruptResponseDto, SessionTurnRequestDto, SessionTurnResponseDto,
        SessionTurnResponseInput, SessionTurnRouteDto,
    },
    errors::ApiError,
    performance::PerformanceTrace,
    session_continue::{
        SessionContinueAccepted, active_execution_branch_is_continue_recoverable,
        continue_execution_chain_with_pre_resume,
    },
    state::{ApiState, QueuedRegularSessionTurn, normalize_session_turn_identity},
};

use std::sync::atomic::{AtomicU64, Ordering};

mod queued_turns;
pub(crate) use queued_turns::{QueuedRegularSessionTurnDrainOutcome, canonical_turn_matches_queued_regular_turn, drain_next_queued_regular_session_turn, schedule_next_queued_regular_session_turn, settle_queued_input_interruption_after_turn_commit};
use queued_turns::*;
mod steering;
pub(crate) use steering::{session_turn_guide_capability, session_turn_request_is_steerable_input, submit_steer_current_turn_after_turn_commit};
use steering::*;
mod replay;
use replay::*;
mod goal_turns;
#[cfg(test)]
pub(crate) use goal_turns::{goal_continuation_prompt, goal_runtime_retry_backoff, plan_allows_goal_continuation};
pub(crate) use goal_turns::{ensure_goal_continuation_runtime_available, goal_mode_tool_intent, record_active_goal_turn_failure, record_active_goal_turn_success, resume_active_goal_continuation_turn_after_turn_commit, settle_goal_start_turn};
use goal_turns::*;
mod interrupt_continue;
#[cfg(test)]
pub(crate) use interrupt_continue::continue_session_with_expected_root;
pub(crate) use interrupt_continue::{ContinueUserMessageInput, cancel_active_session_turn_for_lifecycle, continue_session, finalize_continue_session, finalize_terminal_root_current_turn, interrupt_session_turn, interrupt_session_turn_with_expected, turn_status_is_interruptible, write_continue_user_message};
use interrupt_continue::*;


static ACCEPTED_AT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn monotonic_accepted_at() -> UtcMillis {
    let now = UtcMillis::now().0;
    let previous = ACCEPTED_AT_COUNTER
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |previous| {
            Some(now.max(previous + 1))
        })
        .expect("acceptance clock update always succeeds");
    UtcMillis(now.max(previous + 1))
}

/// 接纳时记录展示名称，历史消息不随插件改名、升级或停用重新解释。
fn selected_skill_title(state: &ApiState, request: &SessionTurnRequestDto) -> Option<String> {
    let id = request.skill_name.as_deref()?.trim();
    state
        .skill_runtime
        .as_ref()?
        .registry()
        .get(id)
        .map(|skill| skill.title)
}

#[derive(Clone)]
pub(crate) struct TurnService {
    state: ApiState,
}

impl TurnService {
    pub(crate) fn new(state: ApiState) -> Self {
        Self { state }
    }
    pub(crate) async fn submit(
        &self,
        request: SessionTurnRequestDto,
    ) -> Result<SessionTurnResponseDto, ApiError> {
        submit_session_turn_internal(self.state.clone(), request, None).await
    }
}

pub(crate) async fn submit_plugin_job(
    state: ApiState,
    request: SessionTurnRequestDto,
    job: magi_app_server_protocol::generated::PluginJobContext,
) -> Result<SessionTurnResponseDto, ApiError> {
    submit_session_turn_internal(state, request, Some(job)).await
}

/// TurnService 的唯一业务实现。
///
/// 该函数不接触 Axum 类型，HTTP 与 App Server 均通过 `TurnService::submit` 调用。
async fn submit_session_turn_internal(
    state: ApiState,
    mut request: SessionTurnRequestDto,
    job: Option<magi_app_server_protocol::generated::PluginJobContext>,
) -> Result<SessionTurnResponseDto, ApiError> {
    request.skill_name = request
        .skill_name
        .as_deref()
        .and_then(|skill_name| trimmed_non_empty(Some(skill_name)).map(str::to_string));
    request.plugin_command_id = request
        .plugin_command_id
        .as_deref()
        .and_then(|command_id| trimmed_non_empty(Some(command_id)).map(str::to_string));
    validate_session_turn_input(&request)?;
    request.interrupt_turn_id = request
        .interrupt_turn_id
        .as_deref()
        .and_then(|value| trimmed_non_empty(Some(value)).map(str::to_string));
    request
        .validate_context_references()
        .map_err(ApiError::InvalidInput)?;
    request
        .validate_browser_node_selections()
        .map_err(ApiError::InvalidInput)?;
    let images = request
        .parsed_images()
        .map_err(|error| ApiError::InvalidInput(format!("图片输入无效: {error}")))?;
    let request_fingerprint = request
        .request_fingerprint()
        .map_err(ApiError::InvalidInput)?;
    let accepted_at = monotonic_accepted_at();
    let (request_id, user_message_id) = normalize_session_turn_identity(
        request.request_id.take(),
        request.user_message_id.take(),
        &format!("session-turn-{}", accepted_at.0),
    );
    request.request_id = Some(request_id);
    request.user_message_id = Some(user_message_id);
    let requested_workspace_id = request.requested_workspace_id();
    let requested_workspace_path = request.requested_workspace_path();
    let scope = if request.requested_session_id().is_some() {
        require_session_request_scope(
            &state,
            request.session_id.as_deref(),
            request.scope,
            requested_workspace_id.as_deref(),
            requested_workspace_path.as_deref(),
        )?
        .scope
    } else {
        resolve_explicit_session_scope(
            &state,
            request.scope,
            requested_workspace_id.as_deref(),
            requested_workspace_path.as_deref(),
        )?
    };
    let workspace_id = scope.workspace_id();
    let _project_admission = workspace_id
        .as_ref()
        .map(|id| state.try_project_directory_admission(id))
        .transpose()?;
    if request.steer_current_turn && (request.goal_mode || request.resume) {
        return Err(ApiError::InvalidInput(
            "目标模式和继续执行必须作为独立执行轮次提交，不能作为当前轮引导".to_string(),
        ));
    }
    if request.steer_current_turn {
        return submit_steer_current_turn(
            &state,
            &request,
            &scope,
            accepted_at,
            &request_fingerprint,
        )
        .await;
    }
    // 引导已在上面分流，这里的提交都会创建或继续一个 Turn。只有创建新 session 的
    // 提交需要保护全局 current 指针；已有 session 的 Turn 事实由 per-session Turn 锁
    // 保护，不能因为后台执行阻塞其他 session 导航。新 session 尚未有 session_id，因此
    // 在 resolve_dispatch_session 创建并提交首条消息的短控制面事务内持有导航锁，失败时
    // 由同一边界执行回滚。
    let _navigation_guard = if request.requested_session_id().is_none() {
        Some(state.lock_session_navigation().await)
    } else {
        None
    };
    let session_turn_guard = match request.requested_session_id() {
        Some(session_id) => Some(state.lock_session_turn(&session_id).await),
        None => None,
    };
    // requestId 判重必须在加锁后、路由决策前：已受理的请求原样返回原 Turn 或排队位置，
    // 不能因为会话状态变化后重新决策而得到不同结果或失败。
    if let Some(replay) = replay_existing_submission(&state, &request, &request_fingerprint, true)?
    {
        if request.interrupt_turn_id.is_some()
            && request.request_id().is_some_and(|id| {
                state
                    .session_store
                    .input_for_request(&id)
                    .is_some_and(|input| {
                        input.state == magi_session_store::SessionInputState::Pending
                    })
            })
        {
            schedule_next_queued_regular_session_turn(
                state.clone(),
                request.requested_session_id().expect("validated session"),
                workspace_id,
            );
        }
        return Ok(replay);
    }
    // 幂等重放必须先读取已提交事实，再检查当前插件准入：插件可能已停用或被升级，这些状态
    // 不能让客户端无法读取原 accepted 回执。新请求仍在此处经过当前能力校验。
    resolve_plugin_command_selection(&state, &request)?;
    // 扩展 `input` guard：新请求在任何状态被创建之前征询；幂等重放已在上面返回，不会再次调用。
    check_input_guard(&state, &request, "turn").await?;

    let decision = decide_session_turn(&state, &request)?;
    request.source_folder_id =
        trimmed_non_empty(request.source_folder_id.as_deref()).map(str::to_string);
    request.project_directories = if request.resume {
        let session_id = request
            .requested_session_id()
            .expect("validated resume session");
        let source_task_id = decision
            .recovery_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.source_task_id.clone())
            .or_else(|| {
                state
                    .session_store
                    .active_execution_chain(&session_id)
                    .map(|chain| chain.root_task_id)
            });
        let source_task = source_task_id
            .as_ref()
            .and_then(|id| state.task_store().and_then(|store| store.get_task(id)));
        let original = source_task
            .and_then(|task| task.executor_binding)
            .and_then(|binding| binding.project_directories);
        let binding = crate::task_dispatch::validate_or_migrate_directory_binding(
            &state,
            workspace_id.as_ref(),
            original,
        )?;
        if request.source_folder_id.as_ref().is_some_and(|id| {
            binding
                .as_ref()
                .is_none_or(|binding| &binding.default_source_folder_id != id)
        }) {
            return Err(ApiError::Conflict(
                "继续执行不能改变原任务的源文件夹".into(),
            ));
        }
        binding
    } else {
        match workspace_id.as_ref() {
            Some(workspace_id) => Some(
                state
                    .workspace_registry
                    .capture_directory_binding(
                        workspace_id,
                        request.source_folder_id.as_deref(),
                        None,
                    )
                    .map_err(|error| {
                        ApiError::InvalidInput(format!("项目源文件夹选择无效: {error}"))
                    })?,
            ),
            None if request.source_folder_id.is_some() => {
                return Err(ApiError::InvalidInput(
                    "个人会话不能选择项目源文件夹".into(),
                ));
            }
            None => None,
        }
    };
    crate::task_dispatch::validate_project_context_references(
        request.project_directories.as_ref(),
        &request.context_references(),
    )?;
    let canonical_goal_mode = request.goal_mode;
    if let Some(target) = request.interrupt_turn_id.as_deref() {
        let session_id = request.requested_session_id().expect("validated session");
        let current = state
            .session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn);
        if current.as_ref().is_none_or(|turn| {
            turn.turn_id != target || !turn_status_is_interruptible(&turn.status)
        }) {
            return Err(ApiError::turn_conflict(
                "interrupt_target_changed",
                current.map(|turn| turn.turn_id),
                "停止并改做的目标已结束或变化，请核对当前任务",
            ));
        }
        if state
            .session_store
            .inbox_snapshot(&session_id)
            .0
            .iter()
            .any(|input| input.interruption.is_some())
        {
            return Err(ApiError::turn_conflict(
                "interrupt_pending",
                Some(target.to_string()),
                "已有停止并改做输入待处理，请先处理原输入",
            ));
        }
        let response = enqueue_session_turn_response(EnqueueSessionTurnInput {
            state: &state,
            request,
            request_fingerprint,
            requested_workspace_id: workspace_id.clone(),
            accepted_at,
            decision,
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
        })?;
        drop(session_turn_guard);
        schedule_next_queued_regular_session_turn(state.clone(), session_id, workspace_id);
        return Ok(response);
    }
    if request.replace_turn_id().is_some()
        && matches!(
            decision.route,
            SessionTurnRouteDto::Continue | SessionTurnRouteDto::Steer
        )
    {
        return Err(ApiError::InvalidInput(
            "编辑上一条消息必须开始新的对话轮次".to_string(),
        ));
    }
    if job.is_some() {
        let session = request
            .requested_session_id()
            .ok_or_else(|| ApiError::InvalidInput("作业需要明确会话".into()))?;
        if state.queued_regular_session_turn_count(&session) > 0
            || state
                .session_store
                .ensure_current_turn_acceptance_available(&session)
                .is_err()
        {
            return Err(ApiError::Conflict("会话繁忙，作业等待下一次调度".into()));
        }
    }
    if matches!(
        decision.route,
        SessionTurnRouteDto::Chat | SessionTurnRouteDto::Execute
    ) && let Some(session_id) = request.requested_session_id()
    {
        let session = require_session_record_in_scope(&state, &session_id, &scope)?;
        let session_workspace_id = state.session_workspace_id(&session);
        if state.queued_regular_session_turn_count(&session_id) > 0 {
            let should_schedule = state
                .session_store
                .ensure_current_turn_acceptance_available(&session_id)
                .is_ok();
            let response = enqueue_session_turn_response(EnqueueSessionTurnInput {
                state: &state,
                request,
                request_fingerprint: request_fingerprint.clone(),
                requested_workspace_id: workspace_id.clone(),
                accepted_at,
                decision,
                session_id: session_id.clone(),
                workspace_id: session_workspace_id.clone(),
            })?;
            drop(session_turn_guard);
            if should_schedule {
                schedule_next_queued_regular_session_turn(
                    state.clone(),
                    session_id,
                    session_workspace_id,
                );
            }
            return Ok(response);
        }
        match state
            .session_store
            .ensure_current_turn_acceptance_available(&session_id)
        {
            Ok(()) => {}
            Err(error) if request.replace_turn_id().is_some() => {
                return Err(map_turn_replacement_error(&state, &session_id, error));
            }
            Err(DomainError::CurrentTurnConflict { .. }) => {
                // root task 已终态但异步收口尚未落地时，先按需收口再接受新消息，
                // 与 interrupt 路径的收口逻辑保持一致；否则新消息会排在一个
                // 实际上已经结束的轮次后面，响应里也拿不到 rootTaskId。
                let current_turn = state
                    .session_store
                    .runtime_sidecar(&session_id)
                    .and_then(|sidecar| sidecar.current_turn);
                let reconciled =
                    finalize_terminal_root_current_turn(&state, &session_id, current_turn.as_ref())
                        && state
                            .session_store
                            .ensure_current_turn_acceptance_available(&session_id)
                            .is_ok();
                if !reconciled {
                    return enqueue_session_turn_response(EnqueueSessionTurnInput {
                        state: &state,
                        request,
                        request_fingerprint: request_fingerprint.clone(),
                        requested_workspace_id: workspace_id.clone(),
                        accepted_at,
                        decision,
                        session_id,
                        workspace_id: session_workspace_id,
                    });
                }
            }
            Err(error) => {
                return Err(ApiError::internal_assembly(
                    "检查 session turn 队列接受条件失败",
                    error,
                ));
            }
        }
    }
    match decision.route {
        SessionTurnRouteDto::Chat if !canonical_goal_mode => {
            submit_conversation_session_turn(
                state.clone(),
                request,
                images,
                workspace_id,
                accepted_at,
                request_fingerprint,
            )
            .await
        }
        SessionTurnRouteDto::Chat | SessionTurnRouteDto::Execute => {
            submit_mainline_session_turn(MainlineSessionTurnInput {
                state: state.clone(),
                request,
                images,
                workspace_id,
                accepted_at,
                decision,
                creation_request_fingerprint: request_fingerprint,
                job,
                plugin_message: None,
            })
            .await
        }
        SessionTurnRouteDto::Steer => {
            unreachable!("steer route should be handled before classifier")
        }
        SessionTurnRouteDto::Continue => {
            let session_id = request
                .requested_session_id()
                .ok_or_else(|| ApiError::InvalidInput("继续会话需要明确的 session".to_string()))?;
            require_session_record_in_scope(&state, &session_id, &scope)?;
            let resumed_turn_id = format!("turn-session-continue-{}", accepted_at.0);
            let signal = user_signal_from_request(&request, &images, accepted_at);
            let (accepted, user_message) = continue_execution_chain_with_pre_resume(
                &state,
                &session_id,
                &[],
                &resumed_turn_id,
                accepted_at,
                request
                    .request_id()
                    .unwrap_or_else(|| format!("request-{resumed_turn_id}")),
                request_fingerprint.clone(),
                |chain, primary_branch, branches, coordinator_attempt| {
                    let (_, orchestrator_thread_id) = state.session_store.ensure_session_mission(
                        &session_id,
                        accepted_at,
                        || chain.mission_id.clone(),
                    );
                    let accepted_for_turn = SessionContinueAccepted {
                        session_id: session_id.clone(),
                        mission_id: chain.mission_id.clone(),
                        root_task_id: chain.root_task_id.clone(),
                        action_task_id: primary_branch.task_id.clone(),
                        turn_id: resumed_turn_id.clone(),
                        execution_chain_ref: chain.execution_chain_ref.clone(),
                        resumed_branch_count: 0,
                        runner_started: false,
                    };
                    write_continue_user_message(ContinueUserMessageInput {
                        resumed_thread_ids: branches
                            .iter()
                            .map(|branch| branch.thread_id.clone())
                            .collect(),
                        state: &state,
                        accepted: &accepted_for_turn,
                        prompt_text: signal.text.as_deref(),
                        control_only: request.resume,
                        continued_at: accepted_at,
                        request_id: signal.request_id.clone(),
                        user_message_id: signal.user_message_id.clone(),
                        placeholder_message_id: signal.placeholder_message_id.clone(),
                        request_fingerprint: Some(request_fingerprint.clone()),
                        attempt_id: Some(coordinator_attempt.attempt_id.clone()),
                        orchestrator_thread_id,
                    })
                },
            )
            .await?;
            let (entry_id, user_message_item_id) = user_message;
            finalize_continue_session(state.clone(), accepted.clone());
            state.persist_runtime_durable_state_for_sessions_for_api(std::slice::from_ref(
                &accepted.session_id,
            ))?;
            let event_id = publish_session_turn_continue_event(&state, &accepted)?;
            Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
                session_id: accepted.session_id,
                entry_id,
                event_id,
                accepted_at,
                runtime_epoch: state.runtime_epoch().to_string(),
                event_stream_next_sequence: state.event_bus.snapshot().next_sequence,
                created_session: false,
                route: SessionTurnRouteDto::Continue,
                root_task_id: Some(accepted.root_task_id),
                action_task_id: Some(accepted.action_task_id),
                execution_chain_ref: Some(accepted.execution_chain_ref),
                user_message_item_id,
            }))
        }
    }
}

#[derive(Debug)]
pub(crate) struct SessionTurnIntentDecision {
    pub(crate) route: SessionTurnRouteDto,
    /// 从中断检查点继续时沿用原任务的标题与目标；其余主线轮次以用户原文为目标。
    pub(crate) task_title: Option<String>,
    pub(crate) execution_goal: Option<String>,
    /// 只来自结构化契约：目标模式要求维护计划，续做沿用原任务的必调工具链。
    pub(crate) required_tool_chain: Vec<String>,
    pub(crate) completion_contract: TaskCompletionContract,
    pub(crate) recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
}

impl SessionTurnIntentDecision {
    fn new(route: SessionTurnRouteDto) -> Self {
        Self {
            route,
            task_title: None,
            execution_goal: None,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
        }
    }
}

pub(crate) fn validate_session_turn_input(request: &SessionTurnRequestDto) -> Result<(), ApiError> {
    if let Some(target) = &request.interrupt_turn_id
        && (target.trim().is_empty()
            || request.requested_session_id().is_none()
            || request.steer_current_turn
            || request.replace_turn_id().is_some()
            || request.expected_turn_id().is_some()
            || request.resume
            || request.command.is_some()
            || request.plugin_command_id.is_some())
    {
        return Err(ApiError::InvalidInput(
            "停止并改做需要明确 sessionId/interruptTurnId，不能混合引导、编辑重发、恢复或会话命令"
                .into(),
        ));
    }
    if request.command.is_some() || request.plugin_command_id.is_some() {
        return validate_session_turn_command(request);
    }
    if request.trimmed_text().is_none()
        && request
            .skill_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        && request.images.is_empty()
        && request.context_references.is_empty()
        && request.browser_annotation_refs.is_empty()
        && request.browser_node_selections.is_empty()
    {
        return Err(ApiError::InvalidInput("会话输入不能为空".to_string()));
    }
    if request.replace_turn_id().is_some() && request.requested_session_id().is_none() {
        return Err(ApiError::InvalidInput(
            "编辑上一条消息需要明确的 sessionId".to_string(),
        ));
    }
    if request.replace_turn_id().is_some() && request.steer_current_turn {
        return Err(ApiError::InvalidInput(
            "编辑上一条消息不能同时作为当前轮次引导".to_string(),
        ));
    }
    Ok(())
}

/// 会话命令只能作为已有会话中的独立轮次提交；文本是命令参数。
fn validate_session_turn_command(request: &SessionTurnRequestDto) -> Result<(), ApiError> {
    if request.command.is_some() && request.requested_session_id().is_none() {
        return Err(ApiError::InvalidInput(
            "会话命令需要明确的 sessionId".to_string(),
        ));
    }
    let combined_with_other_input = request
        .skill_name
        .as_deref()
        .is_some_and(|skill_name| !skill_name.trim().is_empty())
        || request.goal_mode
        || !request.images.is_empty()
        || !request.context_references.is_empty()
        || !request.browser_annotation_refs.is_empty()
        || !request.browser_node_selections.is_empty()
        || request.command.is_some() && request.plugin_command_id.is_some();
    if combined_with_other_input {
        return Err(ApiError::InvalidInput(
            "会话命令不能与技能、目标模式、图片或上下文引用同时提交".to_string(),
        ));
    }
    if request.steer_current_turn || request.replace_turn_id().is_some() {
        return Err(ApiError::InvalidInput(
            "会话命令必须作为新的独立轮次提交".to_string(),
        ));
    }
    Ok(())
}

/// 请求里选择的插件命令身份：长度受限，形态固定为 `plugin/{插件}/{贡献}`。
pub(crate) fn resolve_plugin_command_selection(
    state: &ApiState,
    request: &SessionTurnRequestDto,
) -> Result<Option<magi_app_server_protocol::generated::PluginNamedContribution>, ApiError> {
    let Some(command_id) = request.plugin_command_id.as_deref() else {
        return Ok(None);
    };
    if command_id.len() > 192 {
        return Err(ApiError::InvalidInput("插件命令身份无效".into()));
    }
    let selected = magi_plugin_system::ContributionRef::parse("命令", command_id)
        .map_err(ApiError::InvalidInput)?;
    let manager = state
        .plugin_manager
        .lock()
        .map_err(|_| ApiError::internal_assembly("校验插件命令失败", "插件锁已损坏"))?;
    let command = manager
        .command_contributions()
        .map_err(|error| ApiError::internal_assembly("读取插件命令失败", error.to_string()))?
        .into_iter()
        .find(|(manifest, command)| {
            magi_plugin_system::ContributionRef::new(manifest.id.clone(), command.id.clone())
                == selected
        })
        .map(|(_, command)| command)
        .ok_or_else(|| ApiError::Forbidden("所选插件命令未激活".into()))?;
    Ok(Some(command))
}

fn session_turn_command_decision() -> SessionTurnIntentDecision {
    SessionTurnIntentDecision::new(SessionTurnRouteDto::Chat)
}

pub(crate) fn decide_session_turn(
    state: &ApiState,
    request: &SessionTurnRequestDto,
) -> Result<SessionTurnIntentDecision, ApiError> {
    let requested_session_id = request.requested_session_id();
    if requested_session_id.as_ref().is_some_and(|session_id| {
        state
            .session_store
            .has_claimed_interrupted_recovery(session_id)
    }) {
        return Err(ApiError::conflict(
            "恢复异常中断会话失败",
            "当前会话的恢复正在启动，请等待本轮恢复完成",
        ));
    }
    if let Some(command) = resolve_plugin_command_selection(state, request)? {
        let mut decision = SessionTurnIntentDecision::new(SessionTurnRouteDto::Execute);
        decision.task_title = Some(format!("/{}", command.title));
        decision.execution_goal = Some(format!("执行插件命令 /{}", command.title));
        return Ok(decision);
    }
    if request.command.is_some() {
        return Ok(session_turn_command_decision());
    }

    if request.resume {
        return decide_resume_turn(state, request);
    }
    if request.goal_mode {
        let mut decision = SessionTurnIntentDecision::new(SessionTurnRouteDto::Chat);
        decision.required_tool_chain = vec!["update_plan".to_string()];
        return Ok(decision);
    }
    Ok(SessionTurnIntentDecision::new(SessionTurnRouteDto::Execute))
}

/// 用户点击“继续”。是否可继续、从哪里继续由 `continuation::decide_continuation` 决定，
/// 与读模型投影给客户端的结果是同一份判定；这里只负责把判定落成执行路径。
fn decide_resume_turn(
    state: &ApiState,
    request: &SessionTurnRequestDto,
) -> Result<SessionTurnIntentDecision, ApiError> {
    let Some(session_id) = request.requested_session_id() else {
        return Err(ApiError::InvalidInput("继续执行需要指定会话".to_string()));
    };
    let candidate = state.session_store.mainline_resume_candidate(&session_id);
    // 编辑替换某一轮时不沿用“最近停止的轮次”，只允许恢复执行链。
    let candidate_for_decision = candidate
        .as_ref()
        .filter(|_| request.replace_turn_id().is_none());
    let kind = crate::continuation::decide_continuation(
        session_recoverable_branch_count(state, &session_id),
        candidate_for_decision,
        state.task_store(),
    );
    match (kind, candidate) {
        (Some(crate::continuation::ContinuationKind::ResumeChain), _) => Ok(
            SessionTurnIntentDecision::new(SessionTurnRouteDto::Continue),
        ),
        (Some(crate::continuation::ContinuationKind::ResumeTurn), Some(candidate)) => {
            let resume = interrupted_turn_resume_from(state, &session_id, candidate)
                .ok_or_else(|| ApiError::InvalidInput("当前会话没有可继续的执行".to_string()))?;
            let mut decision = SessionTurnIntentDecision::new(SessionTurnRouteDto::Execute);
            decision.task_title = Some(format!("继续: {}", resume.original_user_message));
            decision.execution_goal = Some(interrupted_turn_resume_goal(
                &resume,
                request.trimmed_text().as_deref().unwrap_or("继续"),
            ));
            decision.required_tool_chain = resume.required_tool_chain;
            decision.completion_contract = resume.completion_contract;
            decision.recovery_checkpoint = Some(resume.recovery_checkpoint);
            Ok(decision)
        }
        _ => Err(ApiError::InvalidInput(
            "当前会话没有可继续的执行".to_string(),
        )),
    }
}

struct UserInterruptedTurnResume {
    turn_id: String,
    original_user_message: String,
    required_tool_chain: Vec<String>,
    completion_contract: TaskCompletionContract,
    recovery_checkpoint: TaskRecoveryCheckpoint,
}

/// 把共享的继续来源补全为执行所需的任务契约；来源任务缺失时与判定一致地视为不可继续。
fn interrupted_turn_resume_from(
    state: &ApiState,
    session_id: &SessionId,
    candidate: magi_session_store::MainlineResumeCandidate,
) -> Option<UserInterruptedTurnResume> {
    let source_task = state.task_store()?.get_task(&candidate.source_task_id)?;
    Some(UserInterruptedTurnResume {
        turn_id: candidate.turn_id.clone(),
        original_user_message: candidate.original_user_message,
        required_tool_chain: source_task.required_tool_chain().to_vec(),
        completion_contract: source_task.completion_contract().clone(),
        recovery_checkpoint: TaskRecoveryCheckpoint {
            source_session_id: session_id.clone(),
            source_task_id: candidate.source_task_id,
            source_turn_id: candidate.turn_id,
            source_thread_id: candidate.source_thread_id,
        },
    })
}

fn interrupted_turn_resume_goal(resume: &UserInterruptedTurnResume, current_input: &str) -> String {
    format!(
        "[interrupted-turn-resume]\nsource_turn_id: {}\n原始用户目标：{}\n当前用户输入：{}\n\n这是用户明确要求继续的中断任务。运行时已经把来源任务的持久化模型消息、工具调用和工具结果复制到当前独占 Thread。必须直接继承其中已经成功的工具证据，不要从头重复；没有结果的外部操作必须先检查真实状态。只有原始目标的交付物已经完整产生时才能结束当前任务。",
        resume.turn_id,
        resume.original_user_message,
        current_input.trim()
    )
}

pub(crate) fn session_recoverable_branch_count(state: &ApiState, session_id: &SessionId) -> usize {
    let Some(sidecar) = state.session_store.runtime_sidecar(session_id) else {
        return 0;
    };
    let Some(chain) = sidecar.active_execution_chain.as_ref() else {
        return 0;
    };
    chain
        .branches
        .iter()
        .filter(|branch| {
            active_execution_branch_is_continue_recoverable(
                state.task_store(),
                chain,
                sidecar.current_turn.as_ref(),
                branch,
            )
        })
        .count()
}

/// 主线工具面的结构化裁剪：Goal 工具只在目标模式开放；浏览器工具只在可信桌面连接可用。
pub(crate) fn session_turn_denied_tools(request: &SessionTurnRequestDto) -> Vec<String> {
    let mut denied = Vec::new();
    if !request.goal_mode {
        denied.extend(["get_goal", "create_goal", "update_goal"].map(str::to_string));
    }
    if !request.desktop_browser_tools_allowed {
        denied.extend(
            BrowserToolKind::ALL
                .into_iter()
                .map(|tool| tool.name().to_string()),
        );
    }
    denied
}

pub(crate) struct UserMessageTurnItemInput<'a> {
    pub(crate) accepted_at: UtcMillis,
    pub(crate) message: &'a str,
    pub(crate) entry_id: &'a str,
    pub(crate) request_id: Option<String>,
    pub(crate) user_message_id: Option<String>,
    pub(crate) placeholder_message_id: Option<String>,
    pub(crate) metadata: std::collections::HashMap<String, serde_json::Value>,
    pub(crate) task_id: Option<magi_core::TaskId>,
    pub(crate) source_thread_id: magi_core::ThreadId,
}

pub(crate) fn build_user_message_turn_item(
    input: UserMessageTurnItemInput<'_>,
) -> (String, ActiveExecutionTurnItem) {
    let UserMessageTurnItemInput {
        accepted_at,
        message,
        entry_id,
        request_id,
        user_message_id,
        placeholder_message_id,
        metadata,
        task_id,
        source_thread_id,
    } = input;
    let user_message_item_id = user_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-user-{}", accepted_at.0));
    (
        user_message_item_id.clone(),
        ActiveExecutionTurnItem {
            item_id: user_message_item_id,
            item_seq: 1,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: Some(message.to_string()),
            task_id,
            worker_id: None,
            role_id: None,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            request_id,
            user_message_id,
            placeholder_message_id,
            metadata,
            timeline_entry_id: Some(entry_id.to_string()),
            // P7：user_message 由前端用户发起，归属到 orchestrator thread，走主线可见性。
            source_thread_id,
        },
    )
}

/// 接受并启动普通 Conversation Turn。
///
/// 该路径只写 canonical/session projection，并把执行输入交给 Turn Coordinator；
/// 不创建 TaskStore root task、lease、Runner、Snapshot 或 Git execution context。
async fn submit_conversation_session_turn(
    state: ApiState,
    request: SessionTurnRequestDto,
    images: Vec<magi_conversation_runtime::session_images::SessionTurnImage>,
    workspace_id: Option<WorkspaceId>,
    accepted_at: UtcMillis,
    request_fingerprint: String,
) -> Result<SessionTurnResponseDto, ApiError> {
    if request.plugin_command_id.is_some() {
        return Err(ApiError::InvalidInput(
            "插件命令必须通过 Task 入口执行，请重新提交命令".into(),
        ));
    }
    let previous_current_session_id = state.session_store.current_session_id();
    let (session_id, created_session, workspace_id) = crate::scope_binding::resolve_turn_session(
        &state,
        request.requested_session_id(),
        workspace_id,
        crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE,
        accepted_at,
    )?;
    let request_id = request
        .request_id()
        .ok_or_else(|| ApiError::InvalidInput("conversation Turn 缺少 requestId".to_string()))?;
    let user_message_id = request.user_message_id().ok_or_else(|| {
        ApiError::InvalidInput("conversation Turn 缺少 userMessageId".to_string())
    })?;
    let turn_id = format!("turn-session-conversation-{}", accepted_at.0);
    let trace = PerformanceTrace::new(Some(request_id.as_str()), accepted_at);
    trace.mark("submit_received", session_id.as_str(), Some(&turn_id), None);
    // Conversation 也支持请求级主模型覆盖；先持久化会话模型身份，再接纳
    // canonical Turn，确保后台执行读取到与 accepted 请求一致的 Provider 配置。
    if let Some(config) = request.orchestrator_session_config.as_ref() {
        let save_result = crate::model_config::save_orchestrator_session_override_for_session(
            &state,
            &session_id,
            config,
        );
        if let Err(error) = save_result {
            if created_session {
                let _ = state
                    .rollback_created_session_after_navigation_lock(
                        &session_id,
                        previous_current_session_id,
                    )
                    .await;
            }
            return Err(error);
        }
    }
    let admission = TurnAdmission {
        turn_id: turn_id.clone(),
        request_id: request_id.clone(),
        request_fingerprint: request_fingerprint.clone(),
        profile: ExecutionProfile::Conversation,
    };
    let coordinator_admission = match state
        .turn_coordinator()
        .execute_command(&session_id, TurnCommand::Start(admission))
        .map_err(|error| ApiError::Conflict(error.to_string()))?
    {
        CoordinatorCommandResult::Admission(admission) => admission,
        other => {
            return Err(ApiError::internal_assembly(
                "接纳 conversation Turn",
                format!("Coordinator 返回了非法 Start 结果: {other:?}"),
            ));
        }
    };
    let attempt = match coordinator_admission {
        CoordinatorAdmission::Accepted(attempt) => attempt,
        CoordinatorAdmission::Replay(attempt) => {
            return conversation_turn_replay_response(&state, &session_id, &attempt);
        }
    };

    let (_, orchestrator_thread_id) =
        state
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || {
                MissionId::new(format!("mission-session-conversation-{}", accepted_at.0))
            });
    let message = request.timeline_content(request.trimmed_text().as_deref());
    let mut metadata =
        magi_conversation_runtime::session_images::session_turn_images_metadata(&images);
    if let Some(binding) = request.project_directories.as_ref() {
        metadata.insert(
            "projectDirectories".into(),
            serde_json::to_value(binding).expect("project binding serializes"),
        );
    }
    let context_references = request.context_references();
    metadata.extend(
        magi_conversation_runtime::context_reference::session_context_references_metadata(
            &context_references,
        ),
    );
    // 用户在输入框里用 `/` 选的技能：即使这一轮走的是不带工具的普通对话，技能说明也要作为
    // 本轮上下文交给模型，并记在用户消息上（与主线 Turn 一致）。
    let skill_name = request
        .skill_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(skill_name) = skill_name.as_ref() {
        metadata.insert(
            "skillName".to_string(),
            serde_json::Value::String(skill_name.clone()),
        );
        if let Some(title) = selected_skill_title(&state, &request) {
            metadata.insert("skillTitle".into(), Value::String(title));
        }
    }
    if let Some(command) = request.command.as_ref() {
        // 命令轮次的用户 item 只用于展示和审计，历史重建据此排除，不进入模型上下文。
        metadata.insert(
            magi_session_store::SESSION_COMMAND_METADATA_KEY.to_string(),
            serde_json::to_value(command).expect("session command must serialize"),
        );
    }
    metadata.extend([
        (
            "route".to_string(),
            serde_json::Value::String("chat".to_string()),
        ),
        (
            "executionProfile".to_string(),
            serde_json::Value::String("conversation".to_string()),
        ),
        (
            "requestFingerprint".to_string(),
            serde_json::Value::String(request_fingerprint.clone()),
        ),
        (
            "requestId".to_string(),
            serde_json::Value::String(request_id.clone()),
        ),
        (
            "userMessageId".to_string(),
            serde_json::Value::String(user_message_id.clone()),
        ),
        (
            "traceId".to_string(),
            serde_json::Value::String(request_id.clone()),
        ),
        (
            "attemptId".to_string(),
            serde_json::Value::String(attempt.attempt_id.clone()),
        ),
    ]);
    let entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let (_, user_item) = build_user_message_turn_item(UserMessageTurnItemInput {
        accepted_at,
        message: &message,
        entry_id: &entry_id,
        request_id: Some(request_id.clone()),
        user_message_id: Some(user_message_id.clone()),
        placeholder_message_id: request.placeholder_message_id(),
        metadata,
        task_id: None,
        source_thread_id: orchestrator_thread_id.clone(),
    });
    let mut turn = ActiveExecutionTurn {
        turn_id: turn_id.clone(),
        turn_seq: accepted_at.0,
        accepted_at,
        completed_at: None,
        status: "accepted".to_string(),
        user_message: Some(message.clone()),
        items: vec![user_item],
    };
    turn.normalize();
    let (_entry_id, _sidecar, canonical_turn) = match state
        .turn_event_sink()
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            workspace_id.clone(),
            TimelineEntryInput::new(
                entry_id.clone(),
                TimelineEntryKind::UserMessage,
                message.clone(),
                accepted_at,
            ),
            turn,
        ) {
        Ok(result) => result,
        Err(error) => {
            // 只撤销本次 attempt；同一会话其他请求的去重与重放记录必须保留。
            abort_conversation_attempt(&state, &session_id, &attempt);
            if created_session {
                let _ = state
                    .rollback_created_session_after_navigation_lock(
                        &session_id,
                        previous_current_session_id,
                    )
                    .await;
            }
            return Err(ApiError::internal_assembly("接受 conversation Turn", error));
        }
    };
    trace.mark(
        "admission_completed",
        session_id.as_str(),
        Some(&turn_id),
        None,
    );
    if let Err(error) = state
        .turn_coordinator()
        .begin_session_turn_input(session_id.clone(), turn_id.clone())
    {
        // canonical Turn 已经受理，不能留下没有执行者的 accepted Turn：标记失败并只撤销
        // 本次 attempt，同 requestId 的重放记录保持不变。
        {
            let _terminal_guard = state.lock_session_turn_commit(&session_id).await;
            settle_conversation_turn(&state, &session_id, &turn_id, "failed");
        }
        abort_conversation_attempt(&state, &session_id, &attempt);
        return Err(ApiError::Conflict(format!(
            "建立 conversation Turn 输入通道失败: {error}"
        )));
    }
    publish_session_user_message_event(&state, &session_id, workspace_id.clone(), &message);
    let event_id = EventId::new(format!(
        "event-session-turn-conversation-{}",
        canonical_turn.turn_id
    ));
    let canonical_item = canonical_turn
        .items
        .iter()
        .find(|item| item.item_id == user_message_id)
        .cloned();
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.conversation.accepted",
        json!({
            "session_id": session_id,
            "entry_id": entry_id,
            "accepted_at": accepted_at.0,
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "text": request.trimmed_text(),
            "request_id": request_id,
            "user_message_id": user_message_id,
            "created_session": created_session,
            "route": "chat",
            "execution_profile": "conversation",
            "root_task_id": serde_json::Value::Null,
            "action_task_id": serde_json::Value::Null,
            "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
            "canonical_event_kind": "turn_started",
            "canonical_turn": canonical_turn,
            "canonical_item": canonical_item,
        }),
    )
    .with_context(EventContext {
        session_id: Some(session_id.clone()),
        workspace_id: workspace_id.clone(),
        ..EventContext::default()
    });
    let event_seq = state.event_bus.publish(event);
    if let Err(error) = state.persist_session_state_checkpoint("session_conversation_turn_accepted")
    {
        tracing::warn!(session_id = %session_id, error = ?error, "conversation Turn accepted checkpoint 持久化失败");
    }
    trace.mark(
        "accepted_response_sent",
        session_id.as_str(),
        Some(&turn_id),
        None,
    );
    let execution_request = SessionTurnExecutionRequest {
        project_directories: request.project_directories.clone(),
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        workspace_id: workspace_id.clone(),
        prompt: request.trimmed_text().unwrap_or_else(|| message.clone()),
        images,
        context_references,
        access_profile: request.requested_access_profile(),
        skill_name,
        request_id: Some(request_id),
        user_message_id: Some(user_message_id.clone()),
        placeholder_message_id: request.placeholder_message_id(),
        command: request.command.as_ref().map(|command| match command {
            magi_app_server_protocol::SessionTurnCommand::Compact => {
                SessionTurnCommand::CompactContext {
                    instructions: request.trimmed_text(),
                }
            }
        }),
    };
    schedule_conversation_execution(state.clone(), execution_request, attempt, trace);
    if created_session
        && let Some(first_message) = request
            .trimmed_text()
            .filter(|text| !text.trim().is_empty())
    {
        crate::session_title::spawn_new_session_title_refinement(
            &state,
            &session_id,
            &first_message,
            crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE,
        );
    }
    let session_summary = if created_session {
        state
            .session_store
            .session(&session_id)
            .map(|session| SessionDirectoryEntryDto::from_record(session, true, 0))
    } else {
        None
    };
    Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id,
        entry_id,
        event_id: event_id.clone(),
        accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: state.event_bus.snapshot().next_sequence,
        created_session,
        route: SessionTurnRouteDto::Chat,
        root_task_id: None,
        action_task_id: None,
        execution_chain_ref: None,
        user_message_item_id: Some(user_message_id),
    })
    .with_session_summary(session_summary)
    .with_canonical_event("turn_started", Some(canonical_turn), canonical_item)
    .with_canonical_event_metadata(event_id, event_seq, accepted_at))
}

fn schedule_conversation_execution(
    state: ApiState,
    request: SessionTurnExecutionRequest,
    attempt: magi_conversation_runtime::TurnAttempt,
    trace: PerformanceTrace,
) {
    // 调度之前登记，取消不能漏过尚未进入 spawn_blocking 的执行。
    let activity = state.session_turn_dispatcher().map(|dispatcher| {
        dispatcher.track_conversation_turn(&request.session_id, &request.turn_id)
    });
    tokio::spawn(async move {
        let mut activity = activity;
        let session_id = request.session_id.clone();
        let turn_id = request.turn_id.clone();
        let coordinator = state.turn_coordinator().clone();
        if coordinator
            .execute_command(
                &session_id,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Preparing,
                },
            )
            .is_err()
        {
            trace.mark(
                "canonical_terminal_failed",
                session_id.as_str(),
                Some(&turn_id),
                None,
            );
            return;
        }
        let Some(dispatcher) = state.session_turn_dispatcher().cloned() else {
            drop(activity.take());
            fail_conversation_turn_before_execution(
                &state,
                &request,
                &attempt,
                "conversation dispatcher 未配置",
            )
            .await;
            return;
        };
        if coordinator
            .execute_command(
                &session_id,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Running,
                },
            )
            .is_err()
        {
            return;
        }
        // Coordinator 的 running 状态必须同步投影到 canonical Turn，再启动执行器。
        // 否则首个流式 item 可能携带 running 的 item 状态，但 canonical Turn 仍是
        // accepted/pending；Renderer 用该首帧接管本地 optimistic Turn 后，随后到达的
        // 工具或完整快照会形成 running -> pending 的非法回退。
        match state
            .turn_event_sink()
            .set_status_domain(&session_id, Some(&turn_id), "running")
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                // 当前 canonical Turn 已不是本轮（被中断或已被新轮次取代），终态由取代方负责。
                tracing::warn!(
                    session_id = %session_id,
                    turn_id = %turn_id,
                    "conversation Turn 进入 running 时已不是当前 canonical Turn"
                );
                return;
            }
            Err(error) => {
                tracing::error!(
                    session_id = %session_id,
                    turn_id = %turn_id,
                    %error,
                    "conversation Turn running 状态写回失败"
                );
                drop(activity.take());
                fail_conversation_turn_before_execution(
                    &state,
                    &request,
                    &attempt,
                    &format!("conversation Turn running 状态写回失败: {error}"),
                )
                .await;
                return;
            }
        }
        let failure_request = request.clone();
        let execution = tokio::task::spawn_blocking(move || {
            let _activity = activity;
            dispatcher.execute_conversation_turn(request)
        })
        .await;
        // 执行器与用户取消在同一个 session Turn 提交锁下竞争终态。模型执行不占锁，
        // 只有最终 canonical mutation 占用短临界区，因此迟到结果会被当前终态拒绝。
        let _terminal_guard = state.lock_session_turn_commit(&session_id).await;
        let (terminal_status, canonical_changed) = match execution {
            Ok(Ok(output)) if output.interrupted => (
                CoordinatorTurnStatus::Cancelled,
                settle_conversation_turn(&state, &session_id, &turn_id, "cancelled"),
            ),
            Ok(Ok(_)) => (
                CoordinatorTurnStatus::Completed,
                settle_conversation_turn(&state, &session_id, &turn_id, "completed"),
            ),
            Ok(Err(error)) => {
                ensure_conversation_failure_item(
                    &state,
                    &session_id,
                    &turn_id,
                    &failure_request,
                    &error,
                );
                tracing::warn!(
                    session_id = %session_id,
                    turn_id = %turn_id,
                    error = %error.public_message,
                    "conversation Turn 执行失败"
                );
                (
                    CoordinatorTurnStatus::Failed,
                    settle_conversation_turn(&state, &session_id, &turn_id, "failed"),
                )
            }
            Err(error) => {
                ensure_conversation_failure_item(
                    &state,
                    &session_id,
                    &turn_id,
                    &failure_request,
                    &magi_conversation_runtime::session_turn_execution::SessionTurnExecutionError::runtime_invalid_state_with_message(
                        format!("conversation 执行线程异常退出: {error}"),
                    ),
                );
                tracing::error!(
                    session_id = %session_id,
                    turn_id = %turn_id,
                    ?error,
                    "conversation Turn 执行线程异常退出"
                );
                (
                    CoordinatorTurnStatus::Failed,
                    settle_conversation_turn(&state, &session_id, &turn_id, "failed"),
                )
            }
        };
        state
            .turn_coordinator()
            .close_session_turn_input(&session_id, &turn_id);
        if canonical_changed {
            // Conversation 仅释放无 Task 归属的资源，同会话的独立代理继续运行。
            state.cancel_conversation_execution_resources(
                &session_id,
                magi_browser_authority::BrowserLeaseEndReason::TaskFinished,
            );
        }
        // canonical mutation 已经在 terminal guard 内完成；只有本次执行真正写入了
        // 当前 Turn，Coordinator 才收口同一 attempt。取消或新 Turn 先提交时，迟到
        // 的执行结果不会再次触碰 Coordinator。
        if canonical_changed
            && let Err(error) = coordinator.execute_command(
                &session_id,
                TurnCommand::Finish {
                    attempt: attempt.clone(),
                    status: terminal_status,
                },
            )
        {
            tracing::error!(
                session_id = %session_id,
                turn_id = %turn_id,
                %error,
                "conversation Turn Coordinator 终态收口失败"
            );
        }
        if let Err(error) =
            state.persist_session_state_checkpoint("session_conversation_turn_terminal")
        {
            tracing::warn!(
                session_id = %session_id,
                error = ?error,
                "conversation Turn 终态 checkpoint 持久化失败"
            );
        }
        trace.mark(
            if canonical_changed {
                "canonical_terminal_published"
            } else {
                "canonical_terminal_ignored"
            },
            session_id.as_str(),
            Some(&turn_id),
            None,
        );
        schedule_next_queued_regular_session_turn(state, session_id, None);
    });
}

fn abort_conversation_attempt(
    state: &ApiState,
    session_id: &SessionId,
    attempt: &magi_conversation_runtime::TurnAttempt,
) {
    if let Err(error) = state.turn_coordinator().execute_command(
        session_id,
        TurnCommand::Abort {
            attempt: attempt.clone(),
        },
    ) {
        tracing::warn!(session_id = %session_id, %error, "撤销 conversation Turn attempt 失败");
    }
}

/// 执行器启动前失败的唯一收口：Turn 标记失败、Coordinator 释放占位、关闭输入通道，
/// 再让排队消息继续执行，避免会话永远停在等待状态。
async fn fail_conversation_turn_before_execution(
    state: &ApiState,
    request: &SessionTurnExecutionRequest,
    attempt: &magi_conversation_runtime::TurnAttempt,
    reason: &str,
) {
    let session_id = &request.session_id;
    let turn_id = &request.turn_id;
    {
        let _terminal_guard = state.lock_session_turn_commit(session_id).await;
        ensure_conversation_failure_item(
            state,
            session_id,
            turn_id,
            request,
            &magi_conversation_runtime::session_turn_execution::SessionTurnExecutionError::runtime_invalid_state_with_message(
                reason.to_string(),
            ),
        );
        if settle_conversation_turn(state, session_id, turn_id, "failed")
            && let Err(error) = state.turn_coordinator().execute_command(
                session_id,
                TurnCommand::Finish {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Failed,
                },
            )
        {
            tracing::error!(
                session_id = %session_id,
                turn_id = %turn_id,
                %error,
                "conversation Turn 启动前失败时 Coordinator 终态收口失败"
            );
        }
    }
    state
        .turn_coordinator()
        .close_session_turn_input(session_id, turn_id);
    schedule_next_queued_regular_session_turn(state.clone(), session_id.clone(), None);
}

/// 将 Conversation 执行结果写入 canonical Turn 的唯一终态。
///
/// `expected_turn_id` 保证取消或下一轮已经先提交时，迟到的执行结果不会覆盖新状态。
fn settle_conversation_turn(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: &str,
    status: &str,
) -> bool {
    let updated = match state
        .turn_event_sink()
        .set_status_domain(session_id, Some(turn_id), status)
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::error!(
                session_id = %session_id,
                turn_id,
                requested_status = status,
                %error,
                "conversation Turn 终态写回失败；请检查是否为迟到结果或 canonical 持久化错误"
            );
            return false;
        }
    };
    let Some((sidecar, changed)) = updated else {
        return false;
    };
    if !changed {
        // SessionStore 已确认该 Turn 处于相同终态；不要再次发布 terminal item。
        return false;
    }
    let Some(item_id) = sidecar
        .current_turn
        .as_ref()
        .and_then(|turn| turn.items.last().map(|item| item.item_id.clone()))
    else {
        return true;
    };
    if let Err(error) = publish_current_session_turn_item_event(
        &state.event_bus,
        &state.session_store,
        session_id,
        &sidecar.ownership.workspace_id,
        &item_id,
        None,
    ) {
        tracing::warn!(
            session_id = %session_id,
            turn_id,
            %error,
            "conversation Turn 终态事件发布失败"
        );
    }
    true
}

fn ensure_conversation_failure_item(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: &str,
    request: &SessionTurnExecutionRequest,
    error: &magi_conversation_runtime::session_turn_execution::SessionTurnExecutionError,
) {
    let has_error_item = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| {
            turn.turn_id == turn_id
                && turn.items.iter().any(|item| {
                    item.kind == "assistant_error"
                        && item.request_id.as_deref() == request.request_id.as_deref()
                })
        });
    if has_error_item {
        return;
    }
    let Some(source_thread_id) = state
        .session_store
        .orchestrator_thread_for_session(session_id)
        .map(|thread| thread.thread_id)
    else {
        return;
    };
    let mut item = magi_conversation_runtime::session_writeback::session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error.public_message.clone()),
        Some(format!("turn-item-assistant-error-{}", UtcMillis::now().0)),
        source_thread_id,
    );
    item.request_id = request.request_id.clone();
    item.user_message_id = request.user_message_id.clone();
    item.placeholder_message_id = request.placeholder_message_id.clone();
    if let Some(diagnostic) = error.model_failure.as_deref() {
        item.metadata.insert(
            "modelFailure".to_string(),
            serde_json::to_value(diagnostic).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Err(write_error) =
        magi_conversation_runtime::session_writeback::upsert_session_turn_item_for_turn(
            &state.session_store,
            session_id,
            Some(turn_id),
            item,
            None,
        )
    {
        tracing::warn!(
            session_id = %session_id,
            turn_id,
            error = %write_error,
            "conversation Turn 失败事实写回失败"
        );
    }
}

/// 所有可执行的主线 Turn 共用同一个任务执行状态机。
///
/// 普通输入与 Goal 共用带工具的主线 Task；是否派发由模型按共享唤起规则判断，
/// 接纳层不按文本裁剪代理工具，也不把 Goal/内部 root Task 等同于实际派发。
struct MainlineSessionTurnInput {
    state: ApiState,
    request: SessionTurnRequestDto,
    images: Vec<magi_conversation_runtime::session_images::SessionTurnImage>,
    workspace_id: Option<WorkspaceId>,
    accepted_at: UtcMillis,
    decision: SessionTurnIntentDecision,
    creation_request_fingerprint: String,
    job: Option<magi_app_server_protocol::generated::PluginJobContext>,
    plugin_message: Option<magi_app_server_protocol::generated::PluginSessionMessageOrigin>,
}

async fn submit_mainline_session_turn(
    input: MainlineSessionTurnInput,
) -> Result<SessionTurnResponseDto, ApiError> {
    let MainlineSessionTurnInput {
        state,
        request,
        images,
        workspace_id,
        accepted_at,
        decision,
        creation_request_fingerprint,
        job,
        plugin_message,
    } = input;
    let trace = PerformanceTrace::new(request.request_id().as_deref(), accepted_at);
    let requested_session_label = request
        .requested_session_id()
        .map(|session_id| session_id.to_string())
        .unwrap_or_else(|| "new-session".to_string());
    trace.mark("submit_received", &requested_session_label, None, None);
    let route = decision.route;
    let user_text = request.trimmed_text().unwrap_or_else(|| {
        if request.images.is_empty() {
            request.timeline_content(None)
        } else {
            "[图片输入]".to_string()
        }
    });
    let goal_mode = request.goal_mode;
    let execution_goal = if goal_mode {
        format!("{}\n\n用户原始输入：{}", goal_mode_tool_intent(), user_text)
    } else {
        decision.execution_goal.clone().unwrap_or(user_text.clone())
    };
    let mut required_tool_chain = decision.required_tool_chain.clone();
    let mut goal_start_turn = false;
    if goal_mode {
        let has_unfinished_goal = request
            .requested_session_id()
            .and_then(|session_id| state.session_store.current_unfinished_goal(&session_id))
            .is_some();
        goal_start_turn = !has_unfinished_goal;
        required_tool_chain.retain(|tool| tool != "get_goal" && tool != "create_goal");

        let mut goal_tool_chain = vec!["get_goal".to_string()];
        if !has_unfinished_goal {
            goal_tool_chain.push("create_goal".to_string());
        }
        goal_tool_chain.append(&mut required_tool_chain);
        required_tool_chain = goal_tool_chain;
    }

    let (accepted, event_id, canonical_event_seq, canonical_occurred_at) =
        accept_session_task_submission_at(
            &state,
            &request,
            SessionTaskSubmissionInput {
                creation_request_fingerprint: Some(creation_request_fingerprint),
                images,
                workspace_id,
                task_title: decision
                    .task_title
                    .clone()
                    .or_else(|| Some(request.mission_title(Some(&user_text)))),
                execution_goal: Some(execution_goal),
                task_tier: TaskTier::ExecutionChain,
                // Goal mode stays on the mainline Chat route for UX continuity, but its
                // lifecycle contract requires Goal/Plan tools. Plain Chat alone disables the
                // tool surface so workspace preparation and tool schemas do not delay text
                // replies.
                use_tools: goal_mode || !matches!(route, SessionTurnRouteDto::Chat),
                accepted_at,
                required_tool_chain,
                completion_contract: decision.completion_contract.clone(),
                recovery_checkpoint: decision.recovery_checkpoint.clone(),
                denied_tools: session_turn_denied_tools(&request),
                user_message_metadata: {
                    let mut metadata = std::collections::HashMap::from([(
                        "route".to_string(),
                        serde_json::Value::String(session_turn_route_name(route).to_string()),
                    )]);
                    if let Some(origin) = plugin_message {
                        metadata.insert(
                            "pluginMessage".into(),
                            serde_json::to_value(origin).expect("origin serializes"),
                        );
                    }
                    if let Some(id) = request.plugin_command_id.as_ref() {
                        metadata.insert("pluginCommandId".into(), Value::String(id.clone()));
                    }
                    if let Some(job) = job {
                        metadata.insert(
                            "pluginJob".into(),
                            serde_json::to_value(job).expect("job serializes"),
                        );
                    }
                    if let Some(title) = selected_skill_title(&state, &request) {
                        metadata.insert("skillTitle".into(), Value::String(title));
                    }
                    if request.resume {
                        metadata.insert("renderable".to_string(), serde_json::Value::Bool(false));
                    }
                    metadata
                },
            },
        )
        .await?;
    trace.mark(
        "admission_completed",
        accepted.session_id.as_str(),
        None,
        None,
    );
    let execution_chain_ref = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.ownership.execution_chain_ref);
    let (accepted_canonical_turn, accepted_canonical_item) =
        dispatch_accepted_canonical_event(&accepted);
    let session_summary = accepted_session_directory_entry(&state, &accepted);
    trace.mark(
        "accepted_response_sent",
        accepted.session_id.as_str(),
        Some(&format!("turn-session-action-{}", accepted.accepted_at.0)),
        None,
    );
    if goal_start_turn {
        remember_goal_start_request(
            &state,
            &accepted.session_id,
            &accepted.root_task_id,
            &request,
        );
    }
    schedule_session_task_dispatch(state.clone(), accepted.clone());
    Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id: accepted.session_id,
        entry_id: accepted.entry_id,
        event_id: event_id.clone(),
        accepted_at: accepted.accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: state.event_bus.snapshot().next_sequence,
        created_session: accepted.created_session,
        route,
        root_task_id: Some(accepted.root_task_id),
        action_task_id: Some(accepted.action_task_id),
        execution_chain_ref,
        user_message_item_id: accepted.user_message_item_id,
    })
    .with_session_summary(session_summary)
    .with_canonical_event(
        "turn_started",
        accepted_canonical_turn,
        accepted_canonical_item,
    )
    .with_canonical_event_metadata(
        event_id.clone(),
        canonical_event_seq,
        canonical_occurred_at,
    ))
}

fn session_turn_terminal_canonical_turn(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: Option<&str>,
) -> Option<CanonicalTurn> {
    state
        .session_store
        .canonical_turns_for_session(session_id)
        .into_iter()
        .filter(|turn| turn_id.is_none_or(|turn_id| turn.turn_id == turn_id))
        .max_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then(left.turn_id.cmp(&right.turn_id))
        })
}

pub(crate) fn append_terminal_canonical_payload(
    payload: &mut serde_json::Value,
    state: &ApiState,
    session_id: &SessionId,
    turn_id: Option<&str>,
) {
    let Some(canonical_turn) = session_turn_terminal_canonical_turn(state, session_id, turn_id)
    else {
        return;
    };
    if !canonical_turn.status.is_terminal() {
        return;
    }
    let canonical_item = canonical_turn
        .items
        .iter()
        .rev()
        .find(|item| item.visibility.renderable && item.status.is_terminal())
        .or_else(|| {
            canonical_turn
                .items
                .iter()
                .rev()
                .find(|item| item.visibility.renderable)
        })
        .or_else(|| canonical_turn.items.last())
        .cloned();
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "canonical_schema_version".to_string(),
            json!(CANONICAL_TURN_SCHEMA_VERSION),
        );
        object.insert("canonical_event_kind".to_string(), json!("turn_completed"));
        object.insert("canonical_turn".to_string(), json!(canonical_turn));
        object.insert("canonical_item".to_string(), json!(canonical_item));
    }
}

fn session_workspace_for_event(state: &ApiState, session_id: &SessionId) -> Option<WorkspaceId> {
    state
        .session_store
        .session(session_id)
        .and_then(|session| state.session_workspace_id(&session))
}

pub(crate) fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 把 `SessionTurnRequestDto` 中的用户输入字段转换成 [`UserSignal`]。
fn user_signal_from_request(
    request: &SessionTurnRequestDto,
    images: &[magi_conversation_runtime::session_images::SessionTurnImage],
    accepted_at: UtcMillis,
) -> UserSignal {
    UserSignal {
        text: request.trimmed_text(),
        images: magi_conversation_runtime::session_images::session_turn_image_sources(images),
        request_id: request.request_id(),
        user_message_id: request.user_message_id(),
        placeholder_message_id: request.placeholder_message_id(),
        accepted_at,
    }
}

fn session_turn_route_name(route: crate::dto::SessionTurnRouteDto) -> &'static str {
    match route {
        crate::dto::SessionTurnRouteDto::Chat => "chat",
        crate::dto::SessionTurnRouteDto::Execute => "execute",
        crate::dto::SessionTurnRouteDto::Continue => "continue",
        crate::dto::SessionTurnRouteDto::Steer => "steer",
    }
}

pub(crate) fn publish_session_user_message_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<WorkspaceId>,
    message: &str,
) {
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-message-user"),
            "message.created",
            json!({
                "session_id": session_id.to_string(),
                "role": "user",
                "content": message,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id,
            ..EventContext::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::SessionTurnRequestDto;
    use super::monotonic_accepted_at;
    use std::sync::{Arc, Mutex};

    fn command_state(active: bool) -> (crate::ApiState, tempfile::TempDir) {
        use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
        let dir = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({
            "sdkVersion": 2, "id": "acme.cmd", "version": "1.0.0", "name": "Cmd", "description": "",
            "extension": {"hooks": []},
            "permissions": [{"kind": "extension", "targets": ["full-trust"]}],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type": "object", "additionalProperties": false},
            "contributions": {"commands": [{"id": "hello", "title": "Hello", "description": ""}]}
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(&mut bytes);
            for (name, body) in [
                ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
                (
                    "extension/index.mjs",
                    b"export default async () => {}".to_vec(),
                ),
            ] {
                writer
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(&body).unwrap();
            }
            writer.finish().unwrap();
        }
        let package = PluginPackage::from_archive(bytes.get_ref()).unwrap();
        let mut manager = PluginManager::open(dir.path().join("plugins")).unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "c.zip".into(),
                },
            )
            .unwrap();
        if active {
            manager.enable("acme.cmd").unwrap();
            manager
                .authorize("acme.cmd", package.manifest().permissions.clone())
                .unwrap();
            manager.activate("acme.cmd").unwrap();
        }
        let state = crate::ApiState::new(
            "magi-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
            Arc::new(magi_session_store::SessionStore::default()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        )
        .with_plugin_manager(Arc::new(Mutex::new(manager)));
        (state, dir)
    }

    fn command_request(extra: serde_json::Value) -> SessionTurnRequestDto {
        let mut value = serde_json::json!({
            "scope": "personal", "text": "世界", "pluginCommandId": "plugin/acme.cmd/hello"
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn plugin_commands_are_accepted_only_for_active_declared_commands_and_never_mixed() {
        use super::{resolve_plugin_command_selection, validate_session_turn_input};
        let (state, _dir) = command_state(true);
        let request = command_request(serde_json::json!({}));
        validate_session_turn_input(&request).unwrap();
        resolve_plugin_command_selection(&state, &request).unwrap();
        assert!(matches!(
            super::decide_session_turn(&state, &request).unwrap().route,
            super::SessionTurnRouteDto::Execute
        ));
        let empty = command_request(serde_json::json!({"text": ""}));
        let decision = super::decide_session_turn(&state, &empty).unwrap();
        assert_eq!(decision.task_title.as_deref(), Some("/Hello"));
        assert_eq!(
            decision.execution_goal.as_deref(),
            Some("执行插件命令 /Hello")
        );
        // 未声明的命令、格式错误的身份。
        for id in ["plugin/acme.cmd/ghost", "plugin/other/hello"] {
            let bad = command_request(serde_json::json!({"pluginCommandId": id}));
            assert!(
                resolve_plugin_command_selection(&state, &bad).is_err(),
                "{id}"
            );
        }
        let malformed = command_request(serde_json::json!({"pluginCommandId": "hello"}));
        assert!(resolve_plugin_command_selection(&state, &malformed).is_err());
        // 命令不能与技能、目标模式、会话命令或图片同时提交。
        for extra in [
            serde_json::json!({"skillName": "review"}),
            serde_json::json!({"goalMode": true}),
            serde_json::json!({"command": "compact", "sessionId": "s1"}),
        ] {
            assert!(
                validate_session_turn_input(&command_request(extra.clone())).is_err(),
                "{extra}"
            );
        }
        // 未激活的插件不提供命令。
        let (inactive, _dir2) = command_state(false);
        assert!(resolve_plugin_command_selection(&inactive, &request).is_err());
    }

    struct DenyForbidden;
    impl magi_conversation_runtime::extension_hooks::InputGuard for DenyForbidden {
        fn check<'a>(
            &'a self,
            view: magi_conversation_runtime::extension_hooks::InputView<'a>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = magi_conversation_runtime::extension_hooks::InputDecision,
                    > + Send
                    + 'a,
            >,
        > {
            use magi_conversation_runtime::extension_hooks::InputDecision;
            Box::pin(async move {
                if view.text.contains("forbidden") {
                    InputDecision::Blocked {
                        plugin: "acme.guard".into(),
                        reason: "输入违规".into(),
                    }
                } else {
                    InputDecision::Proceed
                }
            })
        }
    }

    #[tokio::test]
    async fn blocked_input_is_rejected_before_any_session_or_turn_fact_exists() {
        let mut harness = crate::turn_harness::MagiTurnHarness::new_task("不会执行");
        harness.state.input_guard = Some(std::sync::Arc::new(DenyForbidden));
        let sessions_before = harness.state.session_store.sessions().len();
        let error = harness
            .submit(None, "this is forbidden", "req-blocked", "msg-blocked")
            .await
            .expect_err("被扩展阻止的输入必须被拒绝");
        assert!(
            matches!(&error, crate::errors::ApiError::Forbidden(message) if message.contains("acme.guard") && message.contains("输入违规")),
            "{error:?}"
        );
        assert_eq!(
            harness.state.session_store.sessions().len(),
            sessions_before,
            "被阻止的输入不得创建会话"
        );
        assert!(
            harness
                .state
                .session_store
                .canonical_turn_for_request_id("req-blocked")
                .is_none(),
            "被阻止的输入不得留下 Turn 事实"
        );
        // 正常输入不受影响。
        assert!(
            harness
                .submit(None, "hello", "req-ok", "msg-ok")
                .await
                .is_ok()
        );
    }

    #[test]
    fn acceptance_time_is_unique_and_monotonic_under_concurrent_submissions() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let times = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        let mut times = Vec::new();
                        for _ in 0..128 {
                            barrier.wait();
                            times.push(monotonic_accepted_at().0);
                        }
                        assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
                        times
                    })
                })
                .collect();
            threads
                .into_iter()
                .flat_map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut unique = times.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), times.len());
        assert!(monotonic_accepted_at().0 > *unique.last().unwrap());
    }
}
