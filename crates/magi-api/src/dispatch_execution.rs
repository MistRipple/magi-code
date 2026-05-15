use magi_core::{
    ExecutionOwnership, MissionId, SessionId, Task, TaskExecutionTarget, TaskId, TaskKind,
    TaskStatus, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, task_events};
use magi_orchestrator::ExecutionWritebackPlans;
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, ActiveExecutionTurnLane,
};
#[cfg(test)]
use crate::dto::SessionTurnRequestDto;
use crate::{
    errors::ApiError,
    state::ApiState,
    task_execution::{DispatchSubmissionRequest, TaskExecutionPlan},
};

// M12/M13/M14：任务图构建器、任务分解管线、任务图重规划入口与派发数据载体已
// 迁移到 magi-conversation-runtime；保留内部 re-export 让 dispatch_execution / 测试
// 代码以原名继续调用。
pub(crate) use magi_conversation_runtime::mission_decomposition::decompose_mission;
#[cfg(test)]
pub(crate) use magi_conversation_runtime::mission_decomposition::{
    TASK_PLAN_TOOL_NAME, parse_decomposition_response,
};
pub(crate) use magi_conversation_runtime::task_graph_builder::{
    TASK_MAX_PHASES, TASK_MIN_PHASES, TaskGraphBuildResult, TaskGraphSubmission, build_task_policy,
    cleanup_task_tree, infer_dispatch_task_role, insert_task_graph, make_dispatch_task,
    task_phase_count_is_valid,
};
pub(crate) use magi_conversation_runtime::task_graph_replan::{
    TaskGraphReplanError, TaskGraphReplanResult,
};
pub(crate) use magi_conversation_runtime::session_thread::ensure_thread_for_role;

#[derive(Clone, Debug)]
struct SessionExecutionBranchSpec {
    task_id: TaskId,
    is_primary: bool,
}

pub(crate) fn run_dispatch_submission(
    state: &ApiState,
    request: &DispatchSubmissionRequest,
) -> Result<TaskGraphSubmission, ApiError> {
    let _ = state.execution_pipeline().ok_or_else(|| {
        ApiError::internal_assembly(
            "任务派发失败",
            "execution pipeline 未配置",
        )
    })?;
    let task_store = state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("任务派发失败", "task_store 未配置")
    })?;

    let accepted_at = request.accepted_at;
    let session_id = &request.session_id;
    let entry_id = request.entry_id.as_str();
    let trimmed_text = request.trimmed_text.as_deref();
    let execution_goal = request
        .execution_goal
        .as_deref()
        .map(str::trim)
        .filter(|goal| !goal.is_empty());
    let execution_goal = execution_goal.ok_or_else(|| {
        ApiError::InvalidInput("任务派发必须提供非空 execution_goal".to_string())
    })?;

    let now = UtcMillis::now();
    // mission 前移：session 的 orchestrator thread 和 mission 绑定共享 session 生命周期。
    // 首次调用 `ensure_session_mission` 时创建 mission + spawn orchestrator thread，
    // 后续所有入口（dispatch / 纯聊天 / 补充上下文）都复用同一 mission_id。
    let (mission_id, orchestrator_thread_id) = state.session_store.ensure_session_mission(
        session_id,
        now,
        || MissionId::new(format!("mission-session-action-{}", accepted_at.0)),
    );
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));

    let obj_task_id = TaskId::new(format!("task-obj-{}", accepted_at.0));
    let act_task_id = TaskId::new(format!("task-act-{}", accepted_at.0));

    let task_goal_text = execution_goal.to_string();
    let objective = make_dispatch_task(
        obj_task_id.clone(),
        mission_id.clone(),
        obj_task_id.clone(),
        None,
        TaskKind::Objective,
        request.mission_title.clone(),
        task_goal_text.clone(),
        TaskStatus::Running,
        now,
        Some("architect"),
        None,
        Some(build_task_policy()),
    );
    task_store.insert_task(objective);

    let task_graph = match build_task_graph(
        state,
        &mission_id,
        &obj_task_id,
        &act_task_id,
        accepted_at,
        request
            .target_role
            .as_deref()
            .unwrap_or_else(|| infer_dispatch_task_role(request.skill_name.as_deref())),
        execution_goal,
        &request.workspace_id,
        &now,
    ) {
        Ok(graph) => graph,
        Err(err) => {
            cleanup_task_tree(task_store, &obj_task_id);
            return Err(err);
        }
    };
    let total_task_count = task_graph.total_task_count;
    let dispatch_task_ids = task_graph.dispatch_task_ids;

    let event = task_events::task_graph_created_event(
        mission_id.as_str(),
        obj_task_id.as_str(),
        total_task_count,
    )
    .with_context(EventContext {
        mission_id: Some(mission_id.clone()),
        task_id: Some(obj_task_id.clone()),
        ..EventContext::default()
    });
    let _ = state.event_bus.publish(event);

    let workspace_id = request.workspace_id.clone();
    let execution_chain_ref = Some(format!("session-action-chain-{}", accepted_at.0));
    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: workspace_id.clone(),
        mission_id: Some(mission_id.clone()),
        task_id: Some(act_task_id.clone()),
        worker_id: Some(worker_id.clone()),
        execution_chain_ref: execution_chain_ref.clone(),
        ..ExecutionOwnership::default()
    };
    state.task_execution_registry().insert(
        act_task_id.clone(),
        TaskExecutionPlan::Dispatch {
            target: TaskExecutionTarget {
                mission_id: mission_id.clone(),
                root_task_id: obj_task_id.clone(),
                task_id: act_task_id.clone(),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: None,
                execution_chain_ref: execution_chain_ref.clone(),
            },
            worker_id: worker_id.clone(),
            lane_id: None,
            lane_seq: None,
            // P7：orchestrator 主动作任务归属 session 的 orchestrator thread，
            // 与 `ActiveExecutionTurn.items` 主线 phase item 同源。
            thread_id: orchestrator_thread_id.clone(),
            is_primary: true,
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            ownership: ownership.clone(),
            writebacks: ExecutionWritebackPlans::from_session_action_input(
                magi_orchestrator::DispatchMemoryExtractionInput {
                    accepted_at,
                    session_id: session_id,
                    timeline_entry_id: entry_id,
                    text: trimmed_text,
                    skill_name: request.skill_name.as_deref(),
                },
            ),
            use_tools: true,
            skill_name: request.skill_name.clone(),
        },
    );

    // P7：role 解析必须成功。任何 sub task 若未绑定 executor_binding.target_role，
    // 属于 task graph 构造阶段的缺陷——直接 panic 暴露，而不是在下游静默掉 thread。
    let role_for_task = |task_id: &TaskId| -> String {
        state
            .task_store()
            .and_then(|store| store.get_task(task_id))
            .and_then(|task| task.executor_binding.map(|binding| binding.target_role))
            .map(|role| role.trim().to_string())
            .filter(|role| !role.is_empty())
            .unwrap_or_else(|| {
                panic!("task {task_id} must declare executor_binding.target_role before dispatch")
            })
    };

    for sub_task_id in &dispatch_task_ids {
        let sub_ownership = ExecutionOwnership {
            session_id: Some(request.session_id.clone()),
            workspace_id: workspace_id.clone(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(TaskId::new(sub_task_id.as_str())),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: execution_chain_ref.clone(),
            ..ExecutionOwnership::default()
        };
        // P7：plan 注册前 ensure 对应 role 的 thread，让 plan 直接持有稳定的 thread_id。
        // 下方 worker_lanes 构造时同样命中 `find_idle_thread_for_role` 复用同一条 thread。
        let sub_task_role_id = role_for_task(sub_task_id);
        let sub_thread_id = ensure_thread_for_role(
            &state.session_store,
            &request.session_id,
            &mission_id,
            &sub_task_role_id,
            &worker_id,
            sub_task_id,
            now,
        );
        state.task_execution_registry().insert(
            sub_task_id.clone(),
            TaskExecutionPlan::Dispatch {
                target: TaskExecutionTarget {
                    mission_id: mission_id.clone(),
                    root_task_id: obj_task_id.clone(),
                    task_id: TaskId::new(sub_task_id.as_str()),
                    requested_worker_id: Some(worker_id.clone()),
                    recovery_id: None,
                    execution_chain_ref: execution_chain_ref.clone(),
                },
                worker_id: worker_id.clone(),
                lane_id: Some(format!("lane-{}", sub_task_id)),
                lane_seq: Some(
                    dispatch_task_ids
                        .iter()
                        .position(|candidate| candidate == sub_task_id)
                        .unwrap_or(0)
                        + 1,
                ),
                // P6b：plan 注册前已 ensure thread，此处直接嵌入供 dispatch_inner 取用，
                // 保证 task_llm_loop 能读取该 thread 的历史 messages。
                thread_id: sub_thread_id.clone(),
                is_primary: false,
                session_id: request.session_id.clone(),
                workspace_id: workspace_id.clone(),
                ownership: sub_ownership,
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: request.skill_name.clone(),
            },
        );
    }

    let mut branches = vec![ActiveExecutionBranch {
        task_id: act_task_id.clone(),
        worker_id: worker_id.clone(),
        stage: "execute".to_string(),
        lease_id: None,
        execution_intent_ref: None,
        binding_lifecycle: None,
        checkpoint_stage: Some("execute".to_string()),
        next_step_index: Some(0),
        checkpoint_at: Some(now),
        resume_mode: Some("stage-restart".to_string()),
        resume_token: None,
        use_tools: true,
        skill_name: request.skill_name.clone(),
        is_primary: true,
    }];
    for sub_task_id in &dispatch_task_ids {
        branches.push(ActiveExecutionBranch {
            task_id: sub_task_id.clone(),
            worker_id: worker_id.clone(),
            stage: "execute".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(0),
            checkpoint_at: Some(now),
            resume_mode: Some("stage-restart".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: request.skill_name.clone(),
            is_primary: false,
        });
    }
    let request_id = request.request_id.clone();
    let user_message_id = request.user_message_id.clone();
    let placeholder_message_id = request.placeholder_message_id.clone();
    let user_message_item_id = user_message_id
        .clone()
        .unwrap_or_else(|| format!("turn-item-user-{}", accepted_at.0));
    // P7：worker_lanes 严格表征"工人 sidechain"。pure-chat（无子任务）下无 lane、
    // 无 worker_spawned item、无任务分配 phase；orchestrator 的回复直接在主线 thread 上。
    let dispatch_lane_task_ids = dispatch_task_ids.clone();
    // orchestrator_thread_id 已由开头 ensure_session_mission 返回，直接复用。
    let mut current_turn = ActiveExecutionTurn {
        turn_id: format!("turn-session-action-{}", accepted_at.0),
        turn_seq: accepted_at.0,
        accepted_at,
        status: "accepted".to_string(),
        completed_at: None,
        user_message: trimmed_text.map(str::to_string),
        items: vec![
            ActiveExecutionTurnItem {
                item_id: user_message_item_id,
                item_seq: 1,
                lane_id: None,
                lane_seq: None,
                kind: "user_message".to_string(),
                status: "completed".to_string(),
                source: "user".to_string(),
                title: None,
                content: trimmed_text.map(str::to_string),
                task_id: Some(act_task_id.clone()),
                worker_id: None,
                role_id: None,
                tool_call_id: None,
                tool_name: None,
                tool_status: None,
                tool_arguments: None,
                tool_result: None,
                tool_error: None,
                request_id: request_id.clone(),
                user_message_id: user_message_id.clone(),
                placeholder_message_id: placeholder_message_id.clone(),
                timeline_entry_id: Some(entry_id.to_string()),
                // P7：user_message 永远归属主线 orchestrator thread。
                source_thread_id: orchestrator_thread_id.clone(),
            },
        ],
        worker_lanes: dispatch_lane_task_ids
            .iter()
            .enumerate()
            .map(|(index, sub_task_id)| {
                // P7：每条 worker lane 必有 role + thread。role_for_task 已在闭包中
                // 强制非空；thread 借助 ensure_thread_for_role 复用 / spawn。
                let role_id = role_for_task(sub_task_id);
                let thread_id = ensure_thread_for_role(
                    &state.session_store,
                    &request.session_id,
                    &mission_id,
                    &role_id,
                    &worker_id,
                    sub_task_id,
                    now,
                );
                ActiveExecutionTurnLane {
                    lane_id: format!("lane-{}", sub_task_id),
                    lane_seq: index + 1,
                    task_id: sub_task_id.clone(),
                    worker_id: worker_id.clone(),
                    role_id,
                    thread_id,
                    title: state
                        .task_store()
                        .and_then(|store| store.get_task(sub_task_id))
                        .map(|task| task.title)
                        .unwrap_or_else(|| sub_task_id.to_string()),
                    is_primary: false,
                }
            })
            .collect(),
    };
    for (index, sub_task_id) in dispatch_lane_task_ids.iter().enumerate() {
        let lane_id = format!("lane-{}", sub_task_id);
        let lane = current_turn
            .worker_lanes
            .iter()
            .find(|lane| lane.task_id == *sub_task_id)
            .expect("worker_lane just constructed for sub_task_id");
        let lane_title = lane.title.clone();
        // P7：worker_spawned item 归属到该 worker 自身 thread（在 worker_lanes 构造时
        // 已 ensure），drawer 打开时能作为该 worker sidechain 的起点事件被检索到。
        let worker_thread_id = lane.thread_id.clone();
        current_turn.items.push(ActiveExecutionTurnItem {
            item_id: format!("turn-item-worker-spawned-{}-{}", accepted_at.0, index),
            item_seq: index + 2,
            lane_id: Some(lane_id),
            lane_seq: Some(index + 1),
            kind: "worker_spawned".to_string(),
            status: "pending".to_string(),
            source: worker_id.to_string(),
            title: Some(lane_title.clone()),
            content: Some(format!("已为 {} 创建执行步骤。", lane_title)),
            task_id: Some(sub_task_id.clone()),
            worker_id: Some(worker_id.clone()),
            role_id: Some(role_for_task(sub_task_id)),
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            request_id: request_id.clone(),
            user_message_id: user_message_id.clone(),
            placeholder_message_id: placeholder_message_id.clone(),
            timeline_entry_id: None,
            source_thread_id: worker_thread_id,
        });
    }
    current_turn.normalize();
    Ok(TaskGraphSubmission {
        root_task_id: obj_task_id.clone(),
        action_task_id: act_task_id.clone(),
        active_execution_chain: Some(ActiveExecutionChain {
            session_id: request.session_id.clone(),
            mission_id,
            root_task_id: obj_task_id,
            execution_chain_ref: execution_chain_ref
                .expect("dispatch execution chain ref should exist"),
            workspace_id,
            active_branch_task_ids: branches
                .iter()
                .map(|branch| branch.task_id.clone())
                .collect(),
            active_worker_bindings: branches
                .iter()
                .map(|branch| branch.worker_id.clone())
                .collect(),
            branches,
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at,
                entry_id: entry_id.to_string(),
                trimmed_text: trimmed_text.map(str::to_string),
                skill_name: request.skill_name.clone(),
            },
            current_turn: Some(current_turn),
        }),
    })
}

#[cfg(test)]
pub(crate) fn run_session_action(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    accepted_at: UtcMillis,
    session_id: &SessionId,
    entry_id: &str,
    trimmed_text: Option<&str>,
) -> Result<TaskGraphSubmission, ApiError> {
    let mission_title = request.mission_title(trimmed_text);
    run_dispatch_submission(
        state,
        &DispatchSubmissionRequest {
            accepted_at,
            session_id: session_id.clone(),
            workspace_id: request
                .requested_workspace_id()
                .map(magi_core::WorkspaceId::new),
            entry_id: entry_id.to_string(),
            timeline_message: request.timeline_message(trimmed_text),
            created_session: false,
            mission_title: mission_title.clone(),
            task_title: format!("执行: {mission_title}"),
            trimmed_text: trimmed_text.map(str::to_string),
            execution_goal: trimmed_text.map(str::to_string),
            skill_name: request.skill_name.clone(),
            target_role: None,
            request_id: request.request_id(),
            user_message_id: request.user_message_id(),
            placeholder_message_id: request.placeholder_message_id(),
        },
    )
}

fn build_task_graph(
    state: &ApiState,
    mission_id: &MissionId,
    root_task_id: &TaskId,
    primary_action_task_id: &TaskId,
    accepted_at: UtcMillis,
    target_role: &str,
    prompt: &str,
    workspace_id: &Option<WorkspaceId>,
    now: &UtcMillis,
) -> Result<TaskGraphBuildResult, ApiError> {
    let prompt_text = prompt.trim();
    if prompt_text.is_empty() {
        return Err(ApiError::internal_assembly(
            "构建任务图失败",
            "prompt 为空",
        ));
    }
    let _ = now;
    let workspace_root_path = state.workspace_root_path(workspace_id);
    let plan = decompose_mission(
        state.model_bridge_client(),
        workspace_root_path.as_deref(),
        Some(prompt_text),
    )
    .ok_or_else(|| {
        ApiError::internal_assembly("构建任务图失败", "无法生成结构化任务计划")
    })?;
    if !task_phase_count_is_valid(plan.phases.len()) {
        return Err(ApiError::internal_assembly(
            "构建任务图失败",
            format!("任务图需要 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 phase"),
        ));
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("构建任务图失败", "task_store 未配置"))?;
    insert_task_graph(
        task_store,
        mission_id,
        root_task_id,
        primary_action_task_id,
        accepted_at,
        target_role,
        now,
        &plan,
        &state.agent_role_registry,
        state.spawn_graph.as_ref(),
    )
    .map_err(|msg| ApiError::internal_assembly("构建任务图失败", msg))
}

/// M14：任务图重规划入口。任务图重规划本体已下沉到
/// `magi_conversation_runtime::task_graph_replan::replan_task_graph`，这里只做
/// ApiState → 显式参数的薄壳桥接 + `TaskGraphReplanError` → `ApiError` 错误分类。
pub(crate) fn replan_task_graph(
    state: &ApiState,
    root_task_id: &TaskId,
    prompt: &str,
    context_task: Option<&Task>,
    workspace_id: &Option<WorkspaceId>,
    reason: &str,
) -> Result<TaskGraphReplanResult, ApiError> {
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("重规划任务图失败", "task_store 未配置"))?;
    let workspace_root_path = state.workspace_root_path(workspace_id);
    magi_conversation_runtime::task_graph_replan::replan_task_graph(
        task_store,
        &state.agent_role_registry,
        state.spawn_graph.as_ref(),
        &state.event_bus,
        state.model_bridge_client(),
        workspace_root_path.as_deref(),
        root_task_id,
        prompt,
        context_task,
        reason,
    )
    .map_err(|err| match err {
        TaskGraphReplanError::InvalidInput(msg) => ApiError::InvalidInput(msg),
        TaskGraphReplanError::Internal(msg) => {
            ApiError::internal_assembly("重规划任务图失败", msg)
        }
    })
}

/// M14：会话活跃执行链校验。本体下沉到 v2，magi-api 仅做错误分类。
pub(crate) fn ensure_session_active_execution_chain(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), ApiError> {
    magi_conversation_runtime::task_graph_replan::ensure_session_active_execution_chain(
        &state.session_store,
        session_id,
    )
    .map_err(|err| match err {
        TaskGraphReplanError::InvalidInput(msg) => ApiError::InvalidInput(msg),
        TaskGraphReplanError::Internal(msg) => {
            ApiError::internal_assembly("活跃执行链校验失败", msg)
        }
    })
}

pub(crate) fn replace_replanned_task_execution_branches(
    state: &ApiState,
    session_id: &SessionId,
    primary_action_task_id: &TaskId,
    leaf_action_task_ids: &[TaskId],
) -> Result<(), ApiError> {
    let mut branch_specs = Vec::with_capacity(leaf_action_task_ids.len() + 1);
    branch_specs.push(SessionExecutionBranchSpec {
        task_id: primary_action_task_id.clone(),
        is_primary: true,
    });
    branch_specs.extend(leaf_action_task_ids.iter().cloned().map(|task_id| {
        SessionExecutionBranchSpec {
            task_id,
            is_primary: false,
        }
    }));
    update_session_execution_branches(state, session_id, &branch_specs)
}

fn update_session_execution_branches(
    state: &ApiState,
    session_id: &SessionId,
    branch_specs: &[SessionExecutionBranchSpec],
) -> Result<(), ApiError> {
    if branch_specs.is_empty() {
        return Ok(());
    }
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("注册任务执行分支失败", "task_store 未配置"))?;
    let mut active_chain = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可注册任务的活跃执行链".to_string()))?;
    if active_chain.session_id != *session_id {
        return Err(ApiError::InvalidInput(
            "活跃执行链不属于当前会话".to_string(),
        ));
    }

    let worker_id = active_chain
        .branches
        .first()
        .map(|branch| branch.worker_id.clone())
        .or_else(|| active_chain.active_worker_bindings.first().cloned())
        .unwrap_or_else(|| {
            WorkerId::new(format!(
                "worker-session-action-{}",
                active_chain.dispatch_context.accepted_at.0
            ))
        });
    let workspace_id = active_chain.workspace_id.clone();
    let mission_id = active_chain.mission_id.clone();
    let root_task_id = active_chain.root_task_id.clone();
    let execution_chain_ref = active_chain.execution_chain_ref.clone();
    let skill_name = active_chain.dispatch_context.skill_name.clone();
    let now = UtcMillis::now();

    let existing_task_ids = active_chain
        .branches
        .iter()
        .map(|branch| branch.task_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let new_task_ids = branch_specs
        .iter()
        .map(|spec| spec.task_id.clone())
        .collect::<std::collections::HashSet<_>>();
    for task_id in existing_task_ids.difference(&new_task_ids) {
        let _ = state.task_execution_registry().remove(task_id);
    }

    let mut appended_lane_count = 0usize;
    let mut new_branches = Vec::with_capacity(branch_specs.len());
    let mut new_lanes = Vec::new();

    for spec in branch_specs {
        let task = task_store
            .get_task(&spec.task_id)
            .ok_or_else(|| ApiError::InvalidInput(format!("待注册任务不存在: {}", spec.task_id)))?;
        if task.mission_id != mission_id || task.root_task_id != root_task_id {
            return Err(ApiError::InvalidInput(format!(
                "任务 {} 不属于当前执行链",
                spec.task_id
            )));
        }
        // P7：role_id 为架构不变量，缺失即分派契约破裂；直接 panic 暴露上游错误，
        // 不再提供 fallback 走 resolve_task_role。
        let role_id: String = task
            .executor_binding
            .as_ref()
            .map(|binding| binding.target_role.trim().to_string())
            .filter(|role| !role.is_empty())
            .unwrap_or_else(|| {
                panic!(
                    "task {} must declare executor_binding.target_role before append_branches",
                    spec.task_id
                )
            });
        let lane_seq = if spec.is_primary {
            None
        } else {
            let seq = appended_lane_count + 1;
            appended_lane_count += 1;
            Some(seq)
        };
        let lane_id = lane_seq.map(|_| format!("lane-{}", spec.task_id));
        let ownership = ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(spec.task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some(execution_chain_ref.clone()),
            ..ExecutionOwnership::default()
        };
        // P6b：注册 plan 之前 ensure thread，下方 new_lanes 构造时命中同一条复用即可，
        // 保证 plan 与 lane 两侧 thread_id 一致，避免同 task 出现两条错位 thread。
        let thread_id_for_plan = ensure_thread_for_role(
            &state.session_store,
            session_id,
            &mission_id,
            role_id.as_str(),
            &worker_id,
            &spec.task_id,
            now,
        );
        state.task_execution_registry().insert(
            spec.task_id.clone(),
            TaskExecutionPlan::Dispatch {
                target: TaskExecutionTarget {
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    task_id: spec.task_id.clone(),
                    requested_worker_id: Some(worker_id.clone()),
                    recovery_id: None,
                    execution_chain_ref: Some(execution_chain_ref.clone()),
                },
                worker_id: worker_id.clone(),
                lane_id: lane_id.clone(),
                lane_seq,
                // P6b：plan 与 lane 同步持有 thread_id，保证执行时能读取持久化历史。
                thread_id: thread_id_for_plan.clone(),
                is_primary: spec.is_primary,
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                ownership,
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: skill_name.clone(),
            },
        );
        new_branches.push(ActiveExecutionBranch {
            task_id: spec.task_id.clone(),
            worker_id: worker_id.clone(),
            stage: "execute".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(0),
            checkpoint_at: Some(now),
            resume_mode: Some("stage-restart".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: skill_name.clone(),
            is_primary: spec.is_primary,
        });
        if let (Some(lane_id), Some(lane_seq)) = (lane_id, lane_seq) {
            // P6b：直接复用 plan 注册时已 ensure 的 thread_id，避免二次 ensure_thread_for_role
            // 造成 handled_task_ids 重复追加。
            new_lanes.push(ActiveExecutionTurnLane {
                lane_id,
                lane_seq,
                task_id: spec.task_id.clone(),
                worker_id: worker_id.clone(),
                role_id,
                thread_id: thread_id_for_plan.clone(),
                title: task.title,
                is_primary: false,
            });
        }
    }

    active_chain.branches = new_branches.clone();
    active_chain.active_branch_task_ids = active_chain
        .branches
        .iter()
        .map(|branch| branch.task_id.clone())
        .collect();
    active_chain.active_worker_bindings = active_chain
        .branches
        .iter()
        .map(|branch| branch.worker_id.clone())
        .collect();

    if let Some(turn) = active_chain.current_turn.as_mut() {
        turn.worker_lanes = new_lanes.clone();
        turn.items.retain(|item| {
            item.kind != "worker_spawned"
                || item
                    .task_id
                    .as_ref()
                    .is_some_and(|task_id| new_task_ids.contains(task_id))
        });
        let mut next_item_seq = turn
            .items
            .iter()
            .map(|item| item.item_seq)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        for lane in &new_lanes {
            let already_has_spawned = turn.items.iter().any(|item| {
                item.kind == "worker_spawned" && item.task_id.as_ref() == Some(&lane.task_id)
            });
            if already_has_spawned {
                continue;
            }
            turn.items.push(ActiveExecutionTurnItem {
                item_id: format!("turn-item-worker-spawned-{}-{}", now.0, lane.lane_seq),
                item_seq: next_item_seq,
                lane_id: Some(lane.lane_id.clone()),
                lane_seq: Some(lane.lane_seq),
                kind: "worker_spawned".to_string(),
                status: "pending".to_string(),
                source: lane.worker_id.to_string(),
                title: Some(lane.title.clone()),
                content: Some(format!("已为 {} 创建执行步骤。", lane.title)),
                task_id: Some(lane.task_id.clone()),
                worker_id: Some(lane.worker_id.clone()),
                role_id: Some(lane.role_id.clone()),
                tool_call_id: None,
                tool_name: None,
                tool_status: None,
                tool_arguments: None,
                tool_result: None,
                tool_error: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                timeline_entry_id: None,
                // P6c：append_branches 时的 worker_spawned item 归属到 lane 自身 worker thread。
                source_thread_id: lane.thread_id.clone(),
            });
            next_item_seq = next_item_seq.saturating_add(1);
        }
        turn.normalize();
    }
    active_chain.normalize();
    state
        .session_store
        .upsert_active_execution_chain(session_id.clone(), active_chain)
        .map_err(|error| ApiError::internal_assembly("注册任务执行分支失败", error))?;
    Ok(())
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, BridgeResponse, ModelBridgeClient,
        ModelInvocationRequest, ModelStreamingDelta,
    };
    use magi_core::{AbsolutePath, SessionId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
    use magi_session_store::SessionStore;
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::{Arc, Mutex};

    use crate::dto::SessionTurnRequestDto;

    struct StaticDeepPlanModelBridgeClient;
    struct PathSensitiveDeepPlanModelBridgeClient;
    struct SequentialBatchDeepPlanModelBridgeClient;

    fn static_deep_plan_payload() -> String {
        task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "梳理目标",
                                    "goal": "明确目标、边界和验收标准"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "执行任务",
                                    "goal": "完成用户目标"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "汇总结果",
                                    "goal": "基于执行结果和验证证据汇总结论"
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
    }

    fn path_sensitive_deep_plan_payload() -> String {
        task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "梳理目标",
                                    "goal": "梳理 /Users/xie/code/TEST 工作区的目标和验收标准"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "执行任务",
                                    "goal": "完成 /Users/xie/code/TEST 的只读检查"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "汇总结果",
                                    "goal": "基于 /Users/xie/code/TEST 的只读任务执行结果汇总结论"
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
    }

    fn sequential_batch_deep_plan_payload() -> String {
        task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "梳理目标",
                                    "goal": "梳理两批纵向任务的目标、边界和验收标准"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "第一批执行",
                    "workPackages": [
                        {
                            "title": "第一批工作包",
                            "actions": [
                                {
                                    "title": "执行第一批",
                                    "goal": "用 shell_exec 执行 printf BATCH_ONE_DONE_NEXT_BATCH_TEST，并根据 NEXT_BATCH 结果确认是否继续第二批"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "第二批执行",
                    "workPackages": [
                        {
                            "title": "第二批工作包",
                            "actions": [
                                {
                                    "title": "执行第二批",
                                    "goal": "在第一批完成并发现 NEXT_BATCH 后，用 shell_exec 执行 printf BATCH_TWO_DONE_FINAL_TEST"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "汇总两批结果",
                                    "goal": "只基于前两批执行产出总结，不重复调用工具"
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
    }

    impl ModelBridgeClient for StaticDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: static_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "static deep plan client does not stream".to_string(),
            })
        }
    }

    impl ModelBridgeClient for PathSensitiveDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: path_sensitive_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "path-sensitive deep plan client does not stream".to_string(),
            })
        }
    }

    impl ModelBridgeClient for SequentialBatchDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: sequential_batch_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "sequential batch deep plan client does not stream".to_string(),
            })
        }
    }

    struct RecordingDeepPlanModelBridgeClient {
        prompt: Arc<Mutex<Option<String>>>,
    }

    impl ModelBridgeClient for RecordingDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            *self.prompt.lock().expect("prompt lock poisoned") = Some(request.prompt);
            Ok(BridgeResponse {
                ok: true,
                payload: static_deep_plan_payload(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    /// Build a minimal ApiState with a execution pipeline and task store.
    fn build_test_state() -> (ApiState, Arc<TaskStore>) {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
        );

        let task_store = Arc::new(TaskStore::new());

        let state = ApiState::new(
            "test-task",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_execution_pipeline(orchestrator, execution_runtime, memory_store)
        .with_task_store(Arc::clone(&task_store))
        .with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));

        (state, task_store)
    }

    // -----------------------------------------------------------------------
    // Test 1: Session action creates Task Graph entries when TaskStore is configured
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_creates_task_graph_entries() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-task-graph-test");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            workspace_path: None,
            text: Some("Hello world".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            images: Vec::new(),
            supplement_context: false,
            context_task_id: None,
        };

        let result = run_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-1",
            Some("Hello world"),
        )
        .expect("loopback session action should create task graph");

        let obj_task_id = result.root_task_id;
        let obj = task_store
            .get_task(&obj_task_id)
            .expect("Objective task should exist in TaskStore");
        assert_eq!(obj.kind, TaskKind::Objective);
        assert_eq!(obj.goal, "Hello world");
        assert_eq!(obj.status, TaskStatus::Running);

        let act_task_id = result.action_task_id;
        let act = task_store
            .get_task(&act_task_id)
            .expect("primary Action task should exist in TaskStore");
        assert_eq!(act.kind, TaskKind::Action);
        assert_eq!(act.root_task_id, obj_task_id.clone());
        assert_eq!(act.status, TaskStatus::Ready);

        let children = task_store.get_children(&obj_task_id);
        assert!(
            !children.is_empty(),
            "Objective should have at least one Phase child"
        );
        assert!(
            children.iter().all(|child| child.kind == TaskKind::Phase),
            "Objective children must all be Phase nodes"
        );
    }

    #[test]
    fn dispatch_execution_goal_does_not_rewrite_turn_user_message() {
        let (state, task_store) = build_test_state();
        let session_id = SessionId::new("session-execution-goal-split");
        state
            .session_store
            .create_session(session_id.clone(), "execution goal split")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-execution-goal-split".to_string(),
                timeline_message: "用户原始任务描述".to_string(),
                created_session: false,
                mission_title: "模型判定任务".to_string(),
                task_title: "执行: 模型判定任务".to_string(),
                trimmed_text: Some("用户原始任务描述".to_string()),
                execution_goal: Some("模型结构化执行目标".to_string()),
                skill_name: None,
                target_role: None,
                request_id: Some("request-loopback-alias".to_string()),
                user_message_id: Some("user-loopback-alias".to_string()),
                placeholder_message_id: Some("assistant-placeholder-loopback-alias".to_string()),
            },
        )
        .expect("dispatch should create task graph");

        let objective = task_store
            .get_task(&submission.root_task_id)
            .expect("objective task should exist");
        assert_eq!(objective.goal, "模型结构化执行目标");

        let turn = submission
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .expect("current turn should exist");
        assert_eq!(turn.user_message.as_deref(), Some("用户原始任务描述"));
        let user_item = turn
            .items
            .first()
            .expect("turn 必须包含 canonical 用户消息");
        assert_eq!(user_item.item_id, "user-loopback-alias");
        assert_eq!(user_item.kind, "user_message");
        assert_eq!(user_item.content.as_deref(), Some("用户原始任务描述"));
        assert_eq!(
            state
                .session_store
                .resolve_thread_visibility(&session_id, &user_item.source_thread_id),
            Some(magi_session_store::ThreadVisibility::Main)
        );
        assert_eq!(
            user_item.request_id.as_deref(),
            Some("request-loopback-alias")
        );
        assert_eq!(
            user_item.user_message_id.as_deref(),
            Some("user-loopback-alias")
        );
        assert_eq!(
            user_item.placeholder_message_id.as_deref(),
            Some("assistant-placeholder-loopback-alias")
        );
    }

    #[test]
    fn task_turn_keeps_user_message_on_mainline_and_routes_workers_to_drawer() {
        let (state, _task_store) = build_test_state();
        let state =
            state.with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-mainline-dialog");
        state
            .session_store
            .create_session(session_id.clone(), "deep mainline dialog")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-deep-mainline-dialog".to_string(),
                timeline_message: "用户原始深度任务".to_string(),
                created_session: false,
                mission_title: "深度任务主线".to_string(),
                task_title: "执行: 深度任务主线".to_string(),
                trimmed_text: Some("用户原始深度任务".to_string()),
                execution_goal: Some("深度任务执行目标".to_string()),
                skill_name: None,
                target_role: None,
                request_id: Some("request-deep-mainline".to_string()),
                user_message_id: Some("user-deep-mainline".to_string()),
                placeholder_message_id: Some("assistant-deep-mainline".to_string()),
            },
        )
        .expect("deep dispatch should create task graph");

        let turn = submission
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
            .expect("current turn should exist");
        let visible_items = turn
            .items
            .iter()
            .filter(|item| {
                state
                    .session_store
                    .resolve_thread_visibility(&session_id, &item.source_thread_id)
                    == Some(magi_session_store::ThreadVisibility::Main)
            })
            .map(|item| {
                (
                    item.kind.as_str(),
                    item.content.as_deref().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();

        assert!(
            visible_items
                .iter()
                .any(|(kind, content)| *kind == "user_message" && *content == "用户原始深度任务"),
            "深度任务主线必须像正常对话一样保留用户消息"
        );
        assert!(
            !visible_items
                .iter()
                .any(|(kind, _)| *kind == "assistant_phase"),
            "P7：后端不再生成 phase 文本，主线不得出现 assistant_phase item"
        );
        assert!(
            turn.items.iter().any(|item| {
                item.kind == "worker_spawned"
                    && matches!(
                        state
                            .session_store
                            .resolve_thread_visibility(&session_id, &item.source_thread_id),
                        Some(magi_session_store::ThreadVisibility::WorkerDrawer { .. })
                    )
            }),
            "任务分配本身仍应通过 worker_dispatch 卡片展示，不作为普通主线文本重复出现"
        );
    }

    #[test]
    fn task_requires_non_empty_execution_goal() {
        let (state, task_store) = build_test_state();
        let session_id = SessionId::new("session-deep-empty-goal");
        state
            .session_store
            .create_session(session_id.clone(), "deep empty goal")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let result = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-empty-goal".to_string(),
                timeline_message: "用户原始深度任务".to_string(),
                created_session: false,
                mission_title: "深度任务".to_string(),
                task_title: "执行: 深度任务".to_string(),
                trimmed_text: Some("用户原始深度任务".to_string()),
                execution_goal: Some("   ".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        );

        assert!(
            matches!(result, Err(ApiError::InvalidInput(message)) if message.contains("execution_goal")),
            "深度任务空 execution_goal 必须直接拒绝"
        );
        assert!(
            task_store
                .get_task(&TaskId::new(format!("task-obj-{}", accepted_at.0)))
                .is_none(),
            "拒绝的深度任务不能留下半截任务图"
        );
    }

    #[test]
    fn task_registers_validation_execution_plans() {
        let (state, task_store) = build_test_state();
        let state =
            state.with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-validation-plans");
        state
            .session_store
            .create_session(session_id.clone(), "deep validation plans")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-validation-plans".to_string(),
                timeline_message: "执行深度验证计划".to_string(),
                created_session: false,
                mission_title: "深度验证计划".to_string(),
                task_title: "执行: 深度验证计划".to_string(),
                trimmed_text: Some("执行深度验证计划".to_string()),
                execution_goal: Some("执行深度验证计划".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep task should create graph");

        let validation_ids = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter(|task_id| {
                task_store
                    .get_task(task_id)
                    .is_some_and(|task| task.kind == TaskKind::Validation)
            })
            .collect::<Vec<_>>();
        assert_eq!(validation_ids.len(), 3);
        for validation_id in validation_ids {
            assert!(
                state
                    .task_execution_registry()
                    .remove(&validation_id)
                    .is_some(),
                "validation task {validation_id} should have a registered execution plan"
            );
        }
    }

    #[test]
    fn task_policies_are_unified_across_all_nodes() {
        let (state, task_store) = build_test_state();
        let state =
            state.with_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-task-unified-policy");
        state
            .session_store
            .create_session(session_id.clone(), "task unified policy")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-task-unified-policy".to_string(),
                timeline_message: "执行统一 policy 验收".to_string(),
                created_session: false,
                mission_title: "统一 policy 验收".to_string(),
                task_title: "执行: 统一 policy 验收".to_string(),
                trimmed_text: Some("执行统一 policy 验收".to_string()),
                execution_goal: Some("执行统一 policy 验收".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("task should create graph");

        let tasks = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .collect::<Vec<_>>();

        let unified = build_task_policy();
        for task in tasks
            .iter()
            .filter(|task| task.kind != TaskKind::Objective)
        {
            let policy = task
                .policy_snapshot
                .as_ref()
                .unwrap_or_else(|| panic!("{:?} task missing policy", task.kind));
            assert_eq!(
                policy.autonomy_level, unified.autonomy_level,
                "{:?} 节点 autonomy_level 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.command_mode, unified.command_mode,
                "{:?} 节点 command_mode 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.retry_limit, unified.retry_limit,
                "{:?} 节点 retry_limit 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.repair_limit, unified.repair_limit,
                "{:?} 节点 repair_limit 必须与统一 policy 一致",
                task.kind
            );
            assert_eq!(
                policy.validation_profile, unified.validation_profile,
                "{:?} 节点 validation_profile 必须与统一 policy 一致",
                task.kind
            );
            assert!(
                policy.allowed_tools.is_empty(),
                "统一 policy 不应在引擎层裁剪工具：{:?} 节点 allowed_tools={:?}",
                task.kind,
                policy.allowed_tools
            );
            assert!(
                policy.denied_tools.is_empty(),
                "统一 policy 不应在引擎层裁剪工具：{:?} 节点 denied_tools={:?}",
                task.kind,
                policy.denied_tools
            );
        }

        let execution_action_id = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Action && task.title == "执行任务")
            .map(|task| task.task_id.clone())
            .expect("execution action should exist");
        let delivery_action = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Action && task.title == "汇总结果")
            .expect("delivery action should exist");
        assert!(
            delivery_action
                .dependency_ids
                .iter()
                .any(|dependency_id| dependency_id == &execution_action_id),
            "交付 action 必须基于执行 action 的产出，而不是重新执行用户目标"
        );

        let planning_validation = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Validation && task.title == "规划 验证")
            .expect("planning validation should exist");
        assert!(
            planning_validation.goal.contains("只验证规划文本完整性"),
            "规划验证 goal 必须保留 LLM 给出的规划阶段验收语义"
        );
    }

    #[test]
    fn task_graph_supports_sequential_execution_batches() {
        let (state, task_store) = build_test_state();
        let state = state.with_model_bridge_client(Arc::new(
            SequentialBatchDeepPlanModelBridgeClient,
        ));
        let session_id = SessionId::new("session-deep-sequential-batches");
        state
            .session_store
            .create_session(session_id.clone(), "deep sequential batches")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-sequential-batches".to_string(),
                timeline_message: "执行多段任务".to_string(),
                created_session: false,
                mission_title: "多段任务".to_string(),
                task_title: "执行: 多段任务".to_string(),
                trimmed_text: Some("执行多段任务".to_string()),
                execution_goal: Some("执行多段任务".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("sequential deep task should create graph");

        let tasks = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .collect::<Vec<_>>();
        let phases = tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Phase)
            .collect::<Vec<_>>();
        assert_eq!(phases.len(), 4, "两批执行应成为独立 phase");
        let first_batch_phase = phases
            .iter()
            .find(|task| task.title == "第一批执行")
            .expect("first batch phase should exist");
        let second_batch_phase = phases
            .iter()
            .find(|task| task.title == "第二批执行")
            .expect("second batch phase should exist");
        assert!(
            second_batch_phase
                .dependency_ids
                .iter()
                .any(|dependency_id| dependency_id == &first_batch_phase.task_id),
            "第二批 phase 必须依赖第一批 phase 完成"
        );

        let unified = build_task_policy();
        let policy_for_action = |title: &str| {
            tasks
                .iter()
                .find(|task| task.kind == TaskKind::Action && task.title == title)
                .and_then(|task| task.policy_snapshot.as_ref())
                .expect("action policy should exist")
        };
        for title in ["执行第一批", "执行第二批", "汇总两批结果"] {
            let policy = policy_for_action(title);
            assert_eq!(
                policy.command_mode, unified.command_mode,
                "{title} 必须使用统一 task policy"
            );
        }

        let execution_action_ids = tasks
            .iter()
            .filter(|task| {
                task.kind == TaskKind::Action
                    && matches!(task.title.as_str(), "执行第一批" | "执行第二批")
            })
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        let delivery_action = tasks
            .iter()
            .find(|task| task.kind == TaskKind::Action && task.title == "汇总两批结果")
            .expect("delivery action should exist");
        for execution_action_id in execution_action_ids {
            assert!(
                delivery_action
                    .dependency_ids
                    .iter()
                    .any(|dependency_id| dependency_id == &execution_action_id),
                "交付 action 必须基于每个执行批次的产出"
            );
        }
    }

    #[test]
    fn task_action_roles_stay_runnable_when_goal_mentions_test_path() {
        let (state, task_store) = build_test_state();
        let state = state.with_model_bridge_client(Arc::new(
            PathSensitiveDeepPlanModelBridgeClient,
        ));
        let session_id = SessionId::new("session-deep-action-role-compat");
        state
            .session_store
            .create_session(session_id.clone(), "deep action role compat")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id,
                workspace_id: None,
                entry_id: "entry-deep-action-role-compat".to_string(),
                timeline_message: "检查 /Users/xie/code/TEST".to_string(),
                created_session: false,
                mission_title: "检查 TEST 工作区".to_string(),
                task_title: "执行: 检查 TEST 工作区".to_string(),
                trimmed_text: Some("检查 /Users/xie/code/TEST".to_string()),
                execution_goal: Some("检查 /Users/xie/code/TEST".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep task should create graph");

        let action_roles = task_store
            .collect_subtree_ids(&submission.root_task_id)
            .into_iter()
            .filter_map(|task_id| task_store.get_task(&task_id))
            .filter(|task| task.kind == TaskKind::Action)
            .map(|task| {
                task.executor_binding
                    .expect("action task should have executor binding")
                    .target_role
            })
            .collect::<Vec<_>>();

        assert_eq!(action_roles.len(), 3);
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        for role in action_roles {
            assert!(
                magi_orchestrator::task_worker_catalog::supported_kinds_for_role(&registry, &role)
                    .contains(&TaskKind::Action),
                "deep action role must be runnable by an Action worker, got {role}"
            );
        }
    }

    #[test]
    fn task_planner_receives_workspace_root_context() {
        let (state, _task_store) = build_test_state();
        let captured_prompt = Arc::new(Mutex::new(None));
        let state = state.with_model_bridge_client(Arc::new(
            RecordingDeepPlanModelBridgeClient {
                prompt: Arc::clone(&captured_prompt),
            },
        ));
        let workspace_id = WorkspaceId::new("workspace-deep-current-project");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-deep-current-project"),
            )
            .expect("workspace should be registered");
        let session_id = SessionId::new("session-deep-current-project");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "deep current project",
                Some(workspace_id.to_string()),
            )
            .expect("session should be creatable");

        run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at: UtcMillis::now(),
                session_id,
                workspace_id: Some(workspace_id),
                entry_id: "entry-deep-current-project".to_string(),
                timeline_message: "深度分析当前项目".to_string(),
                created_session: false,
                mission_title: "深度分析当前项目".to_string(),
                task_title: "执行: 深度分析当前项目".to_string(),
                trimmed_text: Some("深度分析当前项目".to_string()),
                execution_goal: Some("深度分析当前项目".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("deep task should create graph");

        let prompt = captured_prompt
            .lock()
            .expect("prompt lock poisoned")
            .clone()
            .expect("planner prompt should be captured");
        assert!(prompt.contains("/tmp/magi-deep-current-project"));
        assert!(prompt.contains("读取这个工作区的真实目录"));
    }

    // -----------------------------------------------------------------------
    // Test 2: Task status reflects dispatch outcome (failure path)
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_registers_execution_plan() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-task-status-test");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            workspace_path: None,
            text: Some("Run a failing action".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            images: Vec::new(),
            supplement_context: false,
            context_task_id: None,
        };

        let submission = run_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-2",
            Some("Run a failing action"),
        )
        .expect("loopback session action should register a task execution plan");

        let obj = task_store
            .get_task(&submission.root_task_id)
            .expect("objective task should exist");
        let act = task_store
            .get_task(&submission.action_task_id)
            .expect("action task should exist");

        assert_eq!(obj.status, TaskStatus::Running);
        assert_eq!(act.status, TaskStatus::Ready);
        assert!(
            state
                .task_execution_registry()
                .remove(&submission.action_task_id)
                .is_some(),
            "action task should have a registered execution plan",
        );
    }

    #[test]
    fn intake_replan_registers_new_branches_and_replaces_active_chain() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-intake-replan-registers");
        state
            .session_store
            .create_session(session_id.clone(), "intake replan")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-intake-replan".to_string(),
                timeline_message: "旧任务".to_string(),
                created_session: false,
                mission_title: "旧任务".to_string(),
                task_title: "执行: 旧任务".to_string(),
                trimmed_text: Some("旧任务".to_string()),
                execution_goal: Some("旧任务".to_string()),
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("dispatch should create active chain");
        state
            .session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                submission
                    .active_execution_chain
                    .expect("active chain should exist"),
            )
            .expect("active chain should be stored");

        let root_task = task_store
            .get_task(&submission.root_task_id)
            .expect("root task should exist");
        let primary_task_id = TaskId::new("task-replan-primary");
        let leaf_task_id = TaskId::new("task-replan-leaf");
        for (task_id, title, is_primary) in [
            (&primary_task_id, "重规划主任务", true),
            (&leaf_task_id, "重规划 worker 任务", false),
        ] {
            task_store.insert_task(make_dispatch_task(
                task_id.clone(),
                root_task.mission_id.clone(),
                submission.root_task_id.clone(),
                Some(submission.root_task_id.clone()),
                TaskKind::Action,
                title.to_string(),
                title.to_string(),
                TaskStatus::Ready,
                UtcMillis::now(),
                Some(if is_primary {
                    "integration-dev"
                } else {
                    "reviewer"
                }),
                None,
                root_task.policy_snapshot.clone(),
            ));
        }

        replace_replanned_task_execution_branches(
            &state,
            &session_id,
            &primary_task_id,
            &[leaf_task_id.clone()],
        )
        .expect("replanned branches should replace active chain");

        assert!(
            state
                .task_execution_registry()
                .remove(&submission.action_task_id)
                .is_none(),
            "重规划后旧 action plan 不能继续留在 registry"
        );
        assert!(
            state
                .task_execution_registry()
                .remove(&primary_task_id)
                .is_some(),
            "重规划主任务必须注册 execution plan"
        );
        assert!(
            state
                .task_execution_registry()
                .remove(&leaf_task_id)
                .is_some(),
            "重规划 worker 任务必须注册 execution plan"
        );
        let sidecar = state
            .session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let chain = sidecar
            .active_execution_chain
            .expect("active chain should exist");
        assert!(chain.active_branch_task_ids.contains(&primary_task_id));
        assert!(chain.active_branch_task_ids.contains(&leaf_task_id));
        assert!(
            !chain
                .active_branch_task_ids
                .contains(&submission.action_task_id)
        );
        let turn = chain.current_turn.expect("current turn should exist");
        assert_eq!(turn.worker_lanes.len(), 1);
        assert_eq!(turn.worker_lanes[0].task_id, leaf_task_id);
        assert_eq!(turn.worker_lanes[0].role_id, "reviewer");
    }

    // -----------------------------------------------------------------------
    // Test 3: No TaskStore configured
    // -----------------------------------------------------------------------

    #[test]
    fn no_task_store_is_rejected() {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
        );

        // No with_task_store — TaskStore is None
        let state = ApiState::new(
            "test-no-task-store",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_execution_pipeline(orchestrator, execution_runtime, memory_store);

        assert!(state.task_store().is_none());

        let session_id = SessionId::new("session-no-store");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionTurnRequestDto {
            session_id: None,
            workspace_id: None,
            workspace_path: None,
            text: Some("test".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            images: Vec::new(),
            supplement_context: false,
            context_task_id: None,
        };

        let result = run_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-3",
            Some("test"),
        );
        assert!(result.is_err());
    }

    fn task_plan_response(arguments: serde_json::Value) -> String {
        serde_json::json!({
            "content": null,
            "finish_reason": "tool_calls",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": TASK_PLAN_TOOL_NAME,
                    "arguments": arguments.to_string(),
                }
            }]
        })
        .to_string()
    }

    #[test]
    fn decomposition_parser_rejects_prompt_echo_lines() {
        let response = task_plan_response(serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "请分析并拆分这个复杂任务",
                                    "goal": "梳理约束"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "实现方案",
                                    "goal": "落地实现",
                                    "dependsOn": ["请分析并拆分这个复杂任务"]
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "验证结果",
                                    "goal": "确认交付",
                                    "dependsOn": ["实现方案"],
                                    "writeScope": "src/"
                                }
                            ]
                        }
                    ]
                }
            ]
        }));

        let plan = parse_decomposition_response(response.as_str(), "请分析并拆分这个复杂任务");

        assert!(plan.is_none(), "提示词回显不能被当成结构化计划");
    }

    #[test]
    fn decomposition_parser_accepts_structured_tool_plan() {
        let plan_value = serde_json::json!({
            "phases": [
                {
                    "title": "规划",
                    "workPackages": [
                        {
                            "title": "规划工作包",
                            "actions": [
                                {
                                    "title": "分析目标",
                                    "goal": "梳理约束"
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "执行",
                    "workPackages": [
                        {
                            "title": "执行工作包",
                            "actions": [
                                {
                                    "title": "实现方案",
                                    "goal": "落地实现",
                                    "dependsOn": ["分析目标"]
                                }
                            ]
                        }
                    ]
                },
                {
                    "title": "交付",
                    "workPackages": [
                        {
                            "title": "交付工作包",
                            "actions": [
                                {
                                    "title": "验证结果",
                                    "goal": "确认交付",
                                    "dependsOn": ["实现方案"],
                                    "writeScope": "src/"
                                }
                            ]
                        }
                    ]
                }
            ]
        });
        let response = task_plan_response(plan_value.clone());

        let plan = parse_decomposition_response(response.as_str(), "请分析并拆分这个复杂任务");

        let plan = plan.expect("structured plan should parse");
        assert_eq!(plan.phases.len(), 3);
        assert_eq!(plan.phases[0].work_packages[0].actions[0].title, "分析目标");
        assert_eq!(
            plan.phases[2].work_packages[0].actions[0]
                .write_scope
                .as_deref(),
            Some("src/")
        );

        let prefixed_response = format!("loopback-model::{}", plan_value);
        let prefixed_plan =
            parse_decomposition_response(prefixed_response.as_str(), "请分析并拆分这个复杂任务")
                .expect("prefixed structured plan should parse");
        assert_eq!(
            prefixed_plan.phases[1].work_packages[0].actions[0].title,
            "实现方案"
        );
    }
}
