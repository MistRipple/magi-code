//! Agent 运行控制应用服务。
//!
//! HTTP 与 App Server 只适配 wire；转交的身份、Goal 版本、TaskStore
//! mutation 与独立 Runner 启动在这里共用；原 lease/在途请求继续结算，不停止主线。

use crate::{
    dto::SessionScopeKindDto, errors::ApiError, scope_binding::require_session_request_scope,
    state::ApiState,
};
use magi_app_server_protocol::generated::{
    AgentTransferParams, AgentTransferResult, AgentTransferTarget,
};
use magi_core::{
    AgentLifetime, AgentRunOwner, EventId, TaskId, TaskStatus, UtcMillis,
    agent_operation_id_is_valid,
};
use magi_event_bus::{EventContext, EventEnvelope};
use serde_json::json;

fn continuation_sources(
    state: &ApiState,
    request: &magi_app_server_protocol::generated::AgentContinueParams,
) -> Result<Vec<magi_core::Task>, ApiError> {
    let session = magi_core::SessionId::new(&request.session_id);
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续代理", "task_store 未配置"))?;
    let root = store
        .get_task(&TaskId::new(&request.task_id))
        .ok_or_else(|| ApiError::not_found("代理不存在", &request.task_id))?;
    let activation = root
        .agent_activation()
        .ok_or_else(|| ApiError::InvalidInput("代理缺少持久身份".into()))?;
    if activation.session_id != session || activation.generation != request.expected_generation {
        return Err(ApiError::Conflict("代理会话或 activation 已变化".into()));
    }
    let transfer = root
        .agent_run_transfer()
        .ok_or_else(|| ApiError::InvalidInput("独立继续只接受原转交子树根".into()))?;
    if let AgentRunOwner::Goal {
        goal_id,
        control_revision,
    } = &transfer.owner
        && state
            .session_store
            .current_goal(&session)
            .is_none_or(|goal| {
                &goal.goal_id != goal_id
                    || goal.control_revision != *control_revision
                    || goal.status != magi_session_store::GoalStatus::Active
            })
    {
        return Err(ApiError::Conflict(
            "代理 Goal 拥有方已撤销，不能继续".into(),
        ));
    }
    let current = state
        .session_store
        .agent_turn_for_task(&session, &root.task_id)
        .map_err(|error| ApiError::internal_assembly("读取代理 Turn", error))?
        .ok_or_else(|| ApiError::InvalidInput("代理缺少独立 Turn".into()))?;
    if current.turn_id != request.expected_turn_id {
        return Err(ApiError::Conflict(
            "代理执行 Turn 已变化，请刷新后重试".into(),
        ));
    }
    let mut sources = Vec::new();
    for id in store.collect_execution_subtree_ids(&root.task_id) {
        let task = store
            .get_task(&id)
            .ok_or_else(|| ApiError::not_found("恢复代理任务缺失", id.as_str()))?;
        if matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            || store.get_active_lease(&id).is_some()
        {
            return Err(ApiError::Conflict("代理子树仍在执行，不能重复继续".into()));
        }
        if task.status != TaskStatus::Failed {
            continue;
        }
        let turn = state
            .session_store
            .agent_turn_for_task(&session, &id)
            .map_err(|error| ApiError::internal_assembly("读取代理来源", error))?
            .ok_or_else(|| ApiError::Conflict("恢复任务缺少原 Agent Turn".into()))?;
        if turn.status != magi_session_store::CanonicalTurnStatus::Interrupted {
            // TaskStore 的 Failed 同时承载执行失败与重启中断；canonical 终态
            // 已收口的后代只保留历史证据，不能阻断其他中断任务或被重新派发。
            if id != root.task_id && turn.status.is_terminal() {
                continue;
            }
            return Err(ApiError::Conflict(
                "只能显式继续中断的代理，失败或取消结果需先核对".into(),
            ));
        }
        for step in &turn.model_steps {
            if step.executor_capabilities.as_ref().is_none_or(|caps| {
                caps.resume != magi_app_server_protocol::ExecutorResumeMode::Checkpoint
                    || caps.context != magi_app_server_protocol::ExecutorContextOwnership::MagiOwned
            }) {
                return Err(ApiError::Conflict(
                    "原执行器不支持 Magi 检查点恢复，请先核对外部进展".into(),
                ));
            }
        }
        sources.push(task);
    }
    if sources
        .first()
        .is_none_or(|task| task.task_id != root.task_id)
    {
        return Err(ApiError::Conflict("目标代理已结束，不能继续".into()));
    }
    let ids = sources
        .iter()
        .map(|task| &task.task_id)
        .collect::<std::collections::HashSet<_>>();
    if state
        .session_store
        .pending_user_questions(&session)
        .map_err(|error| ApiError::internal_assembly("读取代理问题", error))?
        .iter()
        .any(|item| item.task_id.as_ref().is_some_and(|id| ids.contains(id)))
        || state
            .session_store
            .pending_tool_approvals(&session)
            .map_err(|error| ApiError::internal_assembly("读取代理审批", error))?
            .iter()
            .any(|item| ids.contains(&item.task_id))
    {
        return Err(ApiError::Conflict(
            "请先处理原代理的问题或审批，再显式继续".into(),
        ));
    }
    Ok(sources)
}

pub(crate) async fn continue_agent_run(
    state: &ApiState,
    request: magi_app_server_protocol::generated::AgentContinueParams,
) -> Result<magi_app_server_protocol::generated::AgentContinueResult, ApiError> {
    if request.task_id.trim().is_empty()
        || request.expected_turn_id.trim().is_empty()
        || request.operation_id.trim().is_empty()
        || !agent_operation_id_is_valid(&request.operation_id)
        || request.expected_generation == 0
    {
        return Err(ApiError::InvalidInput(
            "代理继续需要有效 Task、Turn、operation 和 activation 版本".into(),
        ));
    }
    let scope = require_session_request_scope(
        state,
        Some(&request.session_id),
        match request.scope {
            magi_app_server_protocol::SessionScope::Personal => SessionScopeKindDto::Personal,
            magi_app_server_protocol::SessionScope::Workspace => SessionScopeKindDto::Workspace,
        },
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let _project_admission = scope
        .workspace_id()
        .as_ref()
        .map(|id| state.try_project_directory_admission(id))
        .transpose()?;
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("继续代理", "runner_manager 未配置"))?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("继续代理", "task_store 未配置"))?;
    // 与主线/Goal 控制共用生命周期锁；只 quiesce 原独立范围，不占用或改写主线 Turn。
    let _lifecycle = state.lock_session_lifecycle(&scope.session_id).await;
    let _restart = manager.lock_for_restart(&request.task_id).await;
    let task_id = TaskId::new(&request.task_id);
    if let Some(existing) = state
        .session_store
        .agent_continuation_for_operation(&scope.session_id, &task_id, &request.operation_id)
        .map_err(|error| ApiError::internal_assembly("读取代理继续回执", error))?
    {
        let binding = existing
            .agent_turn_binding()
            .map_err(|error| ApiError::internal_assembly("读取代理绑定", error))?
            .expect("agent continuation");
        let task = store
            .get_task(&task_id)
            .ok_or_else(|| ApiError::not_found("代理不存在", &request.task_id))?;
        if binding.previous_turn_id.as_deref() != Some(request.expected_turn_id.as_str())
            || task.agent_activation().is_none_or(|identity| {
                identity.generation != request.expected_generation
                    || identity.session_id != scope.session_id
            })
        {
            return Err(ApiError::Conflict("继续 operation 已用于其他来源".into()));
        }
        return Ok(continuation_result(&request, existing.turn_id, true));
    }
    let sources = continuation_sources(state, &request)?;
    manager.quiesce_for_restart(&request.task_id).await;
    let sources_after_quiesce = continuation_sources(state, &request)?;
    if sources
        .iter()
        .map(|task| (&task.task_id, task.updated_at, task.retry_count))
        .collect::<Vec<_>>()
        != sources_after_quiesce
            .iter()
            .map(|task| (&task.task_id, task.updated_at, task.retry_count))
            .collect::<Vec<_>>()
    {
        return Err(ApiError::Conflict("代理结算期间恢复来源已变化".into()));
    }
    let execution_roots = sources
        .iter()
        .map(|task| {
            let workspace_id = scope.workspace_id();
            let directories = crate::task_dispatch::validate_or_migrate_directory_binding(
                state,
                workspace_id.as_ref(),
                task.executor_binding
                    .as_ref()
                    .and_then(|binding| binding.project_directories.clone()),
            )?;
            match directories {
                Some(binding) => crate::task_dispatch::bound_project_execution_root(
                    state,
                    &scope.session_id,
                    &binding,
                ),
                None => Ok(state.personal_session_execution_root_path(&scope.session_id)),
            }
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let turns = state
        .session_store
        .prepare_transferred_agent_continuation(
            &scope.session_id,
            &sources,
            &request.expected_turn_id,
            &request.operation_id,
        )
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    let accepted_turn = turns[0].turn_id.clone();
    let prepare_and_start = || -> Result<(), ApiError> {
        let workspace_id = scope.workspace_id();
        let settings = std::sync::Arc::new(state.settings_store.execution_snapshot());
        for ((task, turn), execution_root) in sources.iter().zip(&turns).zip(&execution_roots) {
            let binding = turn
                .agent_turn_binding()
                .map_err(|error| ApiError::internal_assembly("恢复代理绑定", error))?
                .expect("continued agent binding");
            use magi_conversation_runtime::task_execution_registry::TaskExecutionPlan;
            let plan = TaskExecutionPlan::Dispatch {
                target: magi_core::TaskExecutionTarget {
                    mission_id: task.mission_id.clone(),
                    root_task_id: task.root_task_id.clone(),
                    task_id: task.task_id.clone(),
                    requested_worker_id: Some(binding.worker_id.clone()),
                    recovery_id: None,
                    execution_chain_ref: None,
                },
                worker_id: binding.worker_id,
                thread_id: binding.thread_id,
                is_primary: false,
                session_id: scope.session_id.clone(),
                turn_id: turn.turn_id.clone(),
                workspace_id: workspace_id.clone(),
                execution_root: Some(execution_root.clone()),
                writebacks: Default::default(),
                use_tools: true,
                skill_name: turn
                    .agent_active_skill_id()
                    .map_err(|error| ApiError::internal_assembly("恢复代理 Skill", error))?,
                images: Vec::new(),
                execution_settings_snapshot: Some(settings.clone()),
            };
            state
                .task_execution_registry()
                .insert(task.task_id.clone(), plan)
                .map_err(|_| ApiError::Conflict("代理已有执行计划，不能重复注册".into()))?;
        }
        store
            .reopen_transferred_agent_scope(&task_id, &sources)
            .map_err(|error| ApiError::internal_assembly("重开代理任务", error))?;
        manager
            .start_after_quiesce(&request.task_id, Some(scope.session_id.clone()))
            .map_err(|error| ApiError::internal_assembly("启动独立代理", format!("{error:?}")))?;
        Ok(())
    };
    if let Err(error) = prepare_and_start() {
        for (task, turn) in sources.iter().zip(&turns) {
            state
                .task_execution_registry()
                .remove_if_turn_matches(&task.task_id, &turn.turn_id);
            if store
                .get_task(&task.task_id)
                .is_some_and(|task| task.status == TaskStatus::Pending)
                && let Err(settlement) = store.revoke_active_lease_and_set_task_terminal(
                    &task.task_id,
                    &task.root_task_id,
                    TaskStatus::Failed,
                    vec![error.message().into()],
                )
            {
                tracing::error!(task_id = %task.task_id, %settlement, "独立继续启动失败后的 Task 收口失败");
            }
            if let Err(settlement) = state.session_store.finish_agent_turn(
                &scope.session_id,
                &task.task_id,
                &turn.turn_id,
                magi_session_store::CanonicalTurnStatus::Interrupted,
            ) {
                tracing::error!(task_id = %task.task_id, %settlement, "独立继续启动失败后的 Turn 收口失败");
            }
        }
        return Err(error);
    }
    let _ = state.event_bus.publish(EventEnvelope::domain(EventId::unique("agent-run-continued"), "agent.run.continued", json!({"taskId":task_id,"sessionId":scope.session_id,"turnId":accepted_turn,"sourceTurnId":request.expected_turn_id}))
        .with_context(EventContext { session_id: Some(scope.session_id.clone()), task_id: Some(task_id), workspace_id: scope.workspace_id(), ..Default::default() }));
    Ok(continuation_result(&request, accepted_turn, false))
}

fn continuation_result(
    request: &magi_app_server_protocol::generated::AgentContinueParams,
    turn_id: String,
    replayed: bool,
) -> magi_app_server_protocol::generated::AgentContinueResult {
    magi_app_server_protocol::generated::AgentContinueResult {
        session_id: request.session_id.clone(),
        task_id: request.task_id.clone(),
        operation_id: request.operation_id.clone(),
        source_turn_id: request.expected_turn_id.clone(),
        turn_id,
        replayed,
    }
}

pub(crate) fn project_agent_continuation(
    state: &ApiState,
    scope: &crate::scope_binding::SessionRequestScope,
    task: &magi_core::Task,
) -> (
    Option<magi_app_server_protocol::generated::AgentContinueParams>,
    Option<String>,
) {
    let Some(identity) = task
        .agent_activation()
        .filter(|identity| identity.session_id == scope.session_id)
    else {
        return (None, None);
    };
    if task.agent_run_transfer().is_none() {
        return (None, None);
    }
    let Ok(Some(turn)) = state
        .session_store
        .agent_turn_for_task(&scope.session_id, &task.task_id)
    else {
        return (None, None);
    };
    if turn.status != magi_session_store::CanonicalTurnStatus::Interrupted {
        return (None, None);
    }
    use sha2::{Digest, Sha256};
    let request = magi_app_server_protocol::generated::AgentContinueParams {
        session_id: scope.session_id.to_string(),
        scope: if scope.workspace_id().is_some() {
            magi_app_server_protocol::SessionScope::Workspace
        } else {
            magi_app_server_protocol::SessionScope::Personal
        },
        workspace_id: scope.workspace_id().map(|id| id.to_string()),
        workspace_path: scope.workspace_path(),
        operation_id: format!(
            "agent-continue-ui-{}",
            magi_core::hex_lower(Sha256::digest(
                serde_json::to_vec(&(&task.task_id, &turn.turn_id, identity.generation))
                    .expect("continue identity serializes")
            ))
        ),
        task_id: task.task_id.to_string(),
        expected_generation: identity.generation,
        expected_turn_id: turn.turn_id,
    };
    match continuation_sources(state, &request) {
        Ok(_) => (Some(request), None),
        Err(error) => (None, Some(error.message().into())),
    }
}

pub(crate) async fn transfer_agent_run(
    state: &ApiState,
    request: AgentTransferParams,
) -> Result<AgentTransferResult, ApiError> {
    if request.operation_id.trim().is_empty()
        || !agent_operation_id_is_valid(&request.operation_id)
        || request.task_id.trim().is_empty()
    {
        return Err(ApiError::InvalidInput(
            "operationId 须为 1–200 字符，taskId 不能为空".into(),
        ));
    }
    if request.expected_parent_turn_id.trim().is_empty() || request.expected_generation == 0 {
        return Err(ApiError::InvalidInput(
            "expectedParentTurnId 不能为空，expectedGeneration 必须为正数".into(),
        ));
    }
    let scope_kind = match request.scope {
        magi_app_server_protocol::SessionScope::Personal => SessionScopeKindDto::Personal,
        magi_app_server_protocol::SessionScope::Workspace => SessionScopeKindDto::Workspace,
    };
    let scope = require_session_request_scope(
        state,
        Some(&request.session_id),
        scope_kind,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("转交代理", "runner_manager 未配置"))?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("转交代理", "task_store 未配置"))?;
    let child_id = TaskId::new(request.task_id.trim());

    let _lifecycle_guard = state.lock_session_lifecycle(&scope.session_id).await;
    let _child_restart_guard = manager.lock_for_restart(child_id.as_str()).await;
    let TransferSource {
        child,
        parent,
        agent_turn,
        binding,
    } = read_transfer_source(state, &scope.session_id, &request)?;
    let activation = child
        .agent_activation()
        .expect("transfer source validated activation");
    let parent_id = parent.task_id.clone();

    if let Some(existing) = child.agent_run_transfer() {
        if existing.operation_id != request.operation_id
            || !existing_transfer_owner_matches_request(existing, &request)
        {
            return Err(ApiError::Conflict(
                "代理子树已经转交，不能改写拥有方".into(),
            ));
        }
        // 读取幂等回执不能要求当前 Goal 仍活跃，更不能因 Goal 已暂停/清除
        // 重新启动已失去授权的任务。只有仍具备拥有方执行资格时才尝试恢复 Runner。
        let runner_started = if state
            .session_store
            .turn_execution_is_active(&scope.session_id, &agent_turn.turn_id)
        {
            ensure_agent_runner(manager, &child, &scope.session_id)?
        } else {
            false
        };
        return Ok(transfer_result(
            &scope.session_id,
            &child_id,
            existing,
            runner_started,
        ));
    }
    let owner = validate_new_transfer(state, &scope.session_id, &request, &child, &parent)?;
    let transfer = magi_core::AgentRunTransfer {
        operation_id: request.operation_id.clone(),
        session_id: scope.session_id.clone(),
        source_turn_id: binding.parent_turn_id.clone(),
        transferred_by: parent_id,
        activation_generation: activation.generation,
        owner,
        transferred_at: UtcMillis::now(),
    };
    // 先验证无写入准入，再沿现有资源入口冻结独立副本；准备期间父轮/Goal
    // 仍可能变化，因此实际转交提交在后面同一 canonical 锁下再次复核。
    state
        .session_store
        .commit_agent_run_transfer(&child, &transfer, || Ok(()))
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    let dispatcher = state
        .session_turn_dispatcher()
        .cloned()
        .ok_or_else(|| ApiError::internal_assembly("转交资源准备", "dispatcher 未配置"))?;
    let resource_task = child_id.clone();
    let resource_session = scope.session_id.clone();
    let workspace = scope.workspace_id();
    tokio::task::spawn_blocking(move || {
        dispatcher.prepare_agent_run_transfer_resources(
            &resource_task,
            &resource_session,
            &workspace,
        )
    })
    .await
    .map_err(|error| ApiError::internal_assembly("转交资源线程", error.to_string()))?
    .map_err(ApiError::Conflict)?;
    // 不暂停当前线程或重发模型：TaskStore 原子改变 scope，原 Runner 与
    // Dispatcher 逐次读取同一拥有方；原 lease 与在途 Step 继续正常结算。
    state
        .session_store
        .commit_agent_run_transfer(&child, &transfer, || {
            store.transfer_agent_subtree(&transfer.transferred_by, &child_id, transfer.clone())
        })
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    let runner_started = ensure_agent_runner(manager, &child, &scope.session_id)?;
    let event_id = EventId::unique("agent-run-transferred");
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            event_id.clone(),
            "agent.run.transferred",
            json!({
                "operationId": transfer.operation_id,
                "sessionId": scope.session_id,
                "taskId": child_id,
                "rootTaskId": parent.root_task_id,
                "sourceTurnId": transfer.source_turn_id,
                "owner": transfer.owner,
                "activationGeneration": transfer.activation_generation,
                "transferredAt": transfer.transferred_at,
            }),
        )
        .with_context(EventContext {
            session_id: Some(scope.session_id.clone()),
            mission_id: Some(parent.mission_id.clone()),
            task_id: Some(child_id.clone()),
            workspace_id: scope.workspace_id(),
            ..EventContext::default()
        }),
    );
    Ok(transfer_result(
        &scope.session_id,
        &child_id,
        &transfer,
        runner_started,
    ))
}

struct TransferSource {
    child: magi_core::Task,
    parent: magi_core::Task,
    agent_turn: magi_session_store::CanonicalTurn,
    binding: magi_session_store::AgentTurnBinding,
}

/// 读取与执行共用同一来源校验；投影只返回可提交命令，不准备资源、不保存授权。
fn read_transfer_source(
    state: &ApiState,
    session: &magi_core::SessionId,
    request: &AgentTransferParams,
) -> Result<TransferSource, ApiError> {
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("转交代理", "task_store 未配置"))?;
    let child_id = TaskId::new(request.task_id.trim());
    let child = store
        .get_task(&child_id)
        .ok_or_else(|| ApiError::not_found("代理任务不存在", child_id.as_str()))?;
    let parent_id = child
        .parent_task_id
        .clone()
        .ok_or_else(|| ApiError::InvalidInput("只有直接子代理可以转交".into()))?;
    let parent = store
        .get_task(&parent_id)
        .ok_or_else(|| ApiError::InvalidInput("代理父任务不存在，不能转交".into()))?;
    let activation = child
        .agent_activation()
        .ok_or_else(|| ApiError::InvalidInput("代理缺少可转交的持久身份".into()))?;
    if activation.session_id != *session {
        return Err(ApiError::InvalidInput("代理任务不属于当前会话".into()));
    }
    let agent_turn = state
        .session_store
        .agent_turn_for_task(session, &child_id)
        .map_err(|error| ApiError::internal_assembly("读取代理 Turn", error.to_string()))?
        .ok_or_else(|| ApiError::InvalidInput("代理尚未建立独立执行 Turn，不能转交".into()))?;
    let binding = agent_turn
        .agent_turn_binding()
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?
        .ok_or_else(|| ApiError::InvalidInput("代理 Turn 缺少父链绑定".into()))?;
    if binding.parent_task_id != parent_id
        || binding.parent_turn_id != request.expected_parent_turn_id
        || activation.generation != request.expected_generation
    {
        return Err(ApiError::Conflict(
            "代理父 Turn 或 activation 已变化，请刷新后重试".into(),
        ));
    }

    Ok(TransferSource {
        child,
        parent,
        agent_turn,
        binding,
    })
}

fn validate_new_transfer(
    state: &ApiState,
    session: &magi_core::SessionId,
    request: &AgentTransferParams,
    child: &magi_core::Task,
    parent: &magi_core::Task,
) -> Result<AgentRunOwner, ApiError> {
    let activation = child
        .agent_activation()
        .ok_or_else(|| ApiError::InvalidInput("代理缺少持久身份".into()))?;
    let owner = transfer_owner(state, session, request, activation)?;
    if parent.status != TaskStatus::Running
        || !matches!(child.status, TaskStatus::Pending | TaskStatus::Running)
    {
        return Err(ApiError::InvalidInput(
            "父任务或代理已进入终态，不能转交".into(),
        ));
    }
    Ok(owner)
}

pub(crate) struct AgentTransferProjection {
    pub request: Option<AgentTransferParams>,
    pub owner: Option<AgentTransferTarget>,
    pub unavailable_reason: Option<&'static str>,
}

pub(crate) fn project_agent_transfer(
    state: &ApiState,
    scope: &crate::scope_binding::SessionRequestScope,
    child: &magi_core::Task,
) -> AgentTransferProjection {
    let unavailable = |reason| AgentTransferProjection {
        request: None,
        owner: None,
        unavailable_reason: Some(reason),
    };
    let Some(activation) = child
        .agent_activation()
        .filter(|activation| activation.session_id == scope.session_id)
    else {
        return unavailable("source_unavailable");
    };
    if let Some(transfer) = child.agent_run_transfer() {
        return AgentTransferProjection {
            request: None,
            owner: Some(owner_to_wire(&transfer.owner)),
            unavailable_reason: None,
        };
    }
    let (owner, goal_id, revision) = match &activation.lifetime {
        AgentLifetime::Turn => return unavailable("turn_lifetime"),
        AgentLifetime::Session => (AgentTransferTarget::Session, None, None),
        AgentLifetime::Goal { goal_id } => {
            let Some(goal) = state
                .session_store
                .current_goal(&scope.session_id)
                .filter(|goal| &goal.goal_id == goal_id)
            else {
                return unavailable("owner_unavailable");
            };
            (
                AgentTransferTarget::Goal,
                Some(goal_id.to_string()),
                Some(goal.control_revision),
            )
        }
    };
    if state.runner_manager().is_none() || state.session_turn_dispatcher().is_none() {
        return unavailable("runtime_unavailable");
    }
    let Ok(Some(turn)) = state
        .session_store
        .agent_turn_for_task(&scope.session_id, &child.task_id)
    else {
        return unavailable("source_unavailable");
    };
    let Ok(Some(binding)) = turn.agent_turn_binding() else {
        return unavailable("source_unavailable");
    };
    let mut request = AgentTransferParams {
        session_id: scope.session_id.to_string(),
        scope: if scope.workspace_id().is_some() {
            magi_app_server_protocol::SessionScope::Workspace
        } else {
            magi_app_server_protocol::SessionScope::Personal
        },
        workspace_id: scope.workspace_id().map(|id| id.to_string()),
        workspace_path: scope.workspace_path(),
        operation_id: String::new(),
        task_id: child.task_id.to_string(),
        expected_parent_turn_id: binding.parent_turn_id,
        expected_generation: activation.generation,
        owner,
        goal_id,
        expected_goal_control_revision: revision,
    };
    // 同一份控制建议使用稳定幂等身份；刷新或丢失响应后重试不会变成新转交。
    use sha2::{Digest, Sha256};
    request.operation_id = format!(
        "agent-transfer-ui-{}",
        magi_core::hex_lower(Sha256::digest(
            serde_json::to_vec(&request).expect("wire request serializable")
        ))
    );
    let Ok(source) = read_transfer_source(state, &scope.session_id, &request) else {
        return unavailable("source_unavailable");
    };
    let Ok(owner) = validate_new_transfer(
        state,
        &scope.session_id,
        &request,
        &source.child,
        &source.parent,
    ) else {
        return unavailable("owner_unavailable");
    };
    let transfer = magi_core::AgentRunTransfer {
        operation_id: request.operation_id.clone(),
        session_id: scope.session_id.clone(),
        source_turn_id: request.expected_parent_turn_id.clone(),
        transferred_by: source.parent.task_id,
        activation_generation: activation.generation,
        owner,
        transferred_at: UtcMillis::now(),
    };
    if state
        .session_store
        .commit_agent_run_transfer(&source.child, &transfer, || Ok(()))
        .is_err()
    {
        return unavailable("source_unavailable");
    }
    AgentTransferProjection {
        request: Some(request),
        owner: None,
        unavailable_reason: None,
    }
}

fn existing_transfer_owner_matches_request(
    existing: &magi_core::AgentRunTransfer,
    request: &AgentTransferParams,
) -> bool {
    match (&existing.owner, &request.owner) {
        (AgentRunOwner::Session, AgentTransferTarget::Session) => {
            request.goal_id.is_none() && request.expected_goal_control_revision.is_none()
        }
        (
            AgentRunOwner::Goal {
                goal_id,
                control_revision,
            },
            AgentTransferTarget::Goal,
        ) => {
            request.goal_id.as_deref() == Some(goal_id.as_str())
                && request.expected_goal_control_revision == Some(*control_revision)
        }
        _ => false,
    }
}

fn transfer_owner(
    state: &ApiState,
    session_id: &magi_core::SessionId,
    request: &AgentTransferParams,
    activation: &magi_core::AgentActivation,
) -> Result<AgentRunOwner, ApiError> {
    match request.owner {
        AgentTransferTarget::Session => {
            if request.goal_id.is_some() || request.expected_goal_control_revision.is_some() {
                return Err(ApiError::InvalidInput(
                    "Session 转交不能携带 Goal 参数".into(),
                ));
            }
            if !matches!(activation.lifetime, AgentLifetime::Session) {
                return Err(ApiError::InvalidInput(
                    "只有 lifetime=session 的代理可以转为 Session 拥有".into(),
                ));
            }
            Ok(AgentRunOwner::Session)
        }
        AgentTransferTarget::Goal => {
            let goal_id = request
                .goal_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(magi_core::GoalId::new)
                .ok_or_else(|| ApiError::InvalidInput("Goal 转交需要 goalId".into()))?;
            let control_revision = request.expected_goal_control_revision.ok_or_else(|| {
                ApiError::InvalidInput("Goal 转交需要 expectedGoalControlRevision".into())
            })?;
            if activation.lifetime
                != (AgentLifetime::Goal {
                    goal_id: goal_id.clone(),
                })
            {
                return Err(ApiError::InvalidInput(
                    "代理 lifetime 与目标不一致，不能扩大拥有范围".into(),
                ));
            }
            let goal = state
                .session_store
                .current_goal(session_id)
                .filter(|goal| {
                    goal.goal_id == goal_id && goal.status == magi_session_store::GoalStatus::Active
                })
                .ok_or_else(|| {
                    ApiError::InvalidInput("Goal 不存在、未激活或不属于当前会话".into())
                })?;
            if goal.control_revision != control_revision {
                return Err(ApiError::Conflict(
                    "Goal 控制版本已变化，请刷新后重新转交".into(),
                ));
            }
            Ok(AgentRunOwner::Goal {
                goal_id,
                control_revision,
            })
        }
    }
}

fn ensure_agent_runner(
    manager: &crate::state::RunnerManager,
    task: &magi_core::Task,
    session: &magi_core::SessionId,
) -> Result<bool, ApiError> {
    if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
        return Ok(false);
    }
    match manager.start_after_quiesce(task.task_id.as_str(), Some(session.clone())) {
        Ok(_) | Err(crate::state::RunnerStartError::AlreadyRunning) => Ok(true),
        Err(error) => Err(ApiError::internal_assembly(
            "启动转交代理",
            format!("{error:?}"),
        )),
    }
}

fn owner_to_wire(owner: &AgentRunOwner) -> AgentTransferTarget {
    match owner {
        AgentRunOwner::Session => AgentTransferTarget::Session,
        AgentRunOwner::Goal { .. } => AgentTransferTarget::Goal,
    }
}

fn transfer_result(
    session_id: &magi_core::SessionId,
    task_id: &TaskId,
    transfer: &magi_core::AgentRunTransfer,
    runner_started: bool,
) -> AgentTransferResult {
    let goal_id = match &transfer.owner {
        AgentRunOwner::Goal { goal_id, .. } => Some(goal_id.to_string()),
        AgentRunOwner::Session => None,
    };
    let goal_control_revision = match &transfer.owner {
        AgentRunOwner::Goal {
            control_revision, ..
        } => Some(*control_revision),
        AgentRunOwner::Session => None,
    };
    AgentTransferResult {
        session_id: session_id.to_string(),
        operation_id: transfer.operation_id.clone(),
        task_id: task_id.to_string(),
        source_turn_id: transfer.source_turn_id.clone(),
        activation_generation: transfer.activation_generation,
        owner: owner_to_wire(&transfer.owner),
        goal_id,
        goal_control_revision,
        transferred_at: transfer.transferred_at.0,
        runner_started,
    }
}
