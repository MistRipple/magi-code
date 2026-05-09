use magi_bridge_client::{
    ChatCompletionPayload, ChatToolChoice, ChatToolDefinition, ChatToolFunctionDefinition,
    ModelInvocationRequest, LOOPBACK_MODEL_PROVIDER,
};
use magi_core::{
    ExecutionOwnership, ExecutorBinding, MissionId, SessionId, Task, TaskExecutionTarget, TaskId,
    TaskKind, TaskPolicy, TaskStatus, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, task_events};
use magi_orchestrator::{
    ExecutionWritebackPlans,
    task_store::TaskStore,
    task_worker_catalog::{compatible_task_role_for_kind, resolve_task_role},
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, ActiveExecutionTurnLane,
};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use crate::dto::SessionTurnRequestDto;
use crate::{
    errors::ApiError,
    state::ApiState,
    task_execution::{DispatchSubmissionRequest, TaskExecutionPlan},
};

pub(crate) struct TaskGraphSubmission {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
}

static REPLAN_GRAPH_COUNTER: AtomicU64 = AtomicU64::new(1);
const TASK_PLAN_TOOL_NAME: &str = "create_task_plan";
const TASK_MIN_PHASES: usize = 1;
const TASK_MAX_PHASES: usize = 8;

#[derive(Clone, Debug)]
pub(crate) struct TaskGraphBuildResult {
    pub leaf_action_task_ids: Vec<TaskId>,
    pub validation_task_ids: Vec<TaskId>,
    pub dispatch_task_ids: Vec<TaskId>,
    pub total_task_count: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct TaskGraphReplanResult {
    pub cancelled_task_ids: Vec<TaskId>,
    pub primary_action_task_id: TaskId,
    pub leaf_action_task_ids: Vec<TaskId>,
    pub validation_task_ids: Vec<TaskId>,
    pub dispatch_task_ids: Vec<TaskId>,
}

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

    let mission_id = MissionId::new(format!("mission-session-action-{}", accepted_at.0));
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));

    let obj_task_id = TaskId::new(format!("task-obj-{}", accepted_at.0));
    let act_task_id = TaskId::new(format!("task-act-{}", accepted_at.0));

    let now = UtcMillis::now();
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
    let role_for_task = |task_id: &TaskId| {
        state
            .task_store()
            .and_then(|store| store.get_task(task_id))
            .and_then(|task| task.executor_binding.map(|binding| binding.target_role))
            .map(|role| role.trim().to_string())
            .filter(|role| !role.is_empty())
    };
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
    let visible_lane_task_ids = if dispatch_task_ids.is_empty() {
        vec![act_task_id.clone()]
    } else {
        dispatch_task_ids.clone()
    };
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
                thread_visible: true,
                worker_visible: false,
            },
            ActiveExecutionTurnItem {
                item_id: format!("turn-item-phase-{}", accepted_at.0),
                item_seq: 2,
                lane_id: None,
                lane_seq: None,
                kind: "assistant_phase".to_string(),
                status: "pending".to_string(),
                source: "orchestrator".to_string(),
                title: Some("任务理解".to_string()),
                content: Some(
                    "我先把这个目标拆成可执行步骤，随后分派给合适的执行者；结果回来后继续在主线里整合判断。"
                        .to_string(),
                ),
                task_id: Some(act_task_id.clone()),
                worker_id: Some(worker_id.clone()),
                role_id: role_for_task(&act_task_id),
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
                thread_visible: true,
                worker_visible: false,
            },
        ],
        worker_lanes: visible_lane_task_ids
            .iter()
            .enumerate()
            .map(|(index, sub_task_id)| ActiveExecutionTurnLane {
                lane_id: format!("lane-{}", sub_task_id),
                lane_seq: index + 1,
                task_id: sub_task_id.clone(),
                worker_id: worker_id.clone(),
                role_id: role_for_task(sub_task_id),
                title: state
                    .task_store()
                    .and_then(|store| store.get_task(sub_task_id))
                    .map(|task| task.title)
                    .unwrap_or_else(|| sub_task_id.to_string()),
                is_primary: false,
            })
            .collect(),
    };
    for (index, sub_task_id) in visible_lane_task_ids.iter().enumerate() {
        let lane_id = format!("lane-{}", sub_task_id);
        let lane_title = current_turn
            .worker_lanes
            .iter()
            .find(|lane| lane.task_id == *sub_task_id)
            .map(|lane| lane.title.clone())
            .unwrap_or_else(|| sub_task_id.to_string());
        current_turn.items.push(ActiveExecutionTurnItem {
            item_id: format!("turn-item-worker-spawned-{}-{}", accepted_at.0, index),
            item_seq: index + 3,
            lane_id: Some(lane_id),
            lane_seq: Some(index + 1),
            kind: "worker_spawned".to_string(),
            status: "pending".to_string(),
            source: worker_id.to_string(),
            title: Some(lane_title.clone()),
            content: Some(format!("已为 {} 创建执行步骤。", lane_title)),
            task_id: Some(sub_task_id.clone()),
            worker_id: Some(worker_id.clone()),
            role_id: role_for_task(sub_task_id),
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
            thread_visible: false,
            worker_visible: true,
        });
    }
    let lane_count = current_turn.worker_lanes.len();
    if lane_count > 0 {
        let orchestration_summary =
            task_graph_mainline_summary(task_store, &obj_task_id, &current_turn.worker_lanes);
        current_turn.items.push(ActiveExecutionTurnItem {
            item_id: format!("turn-item-orchestrator-dispatch-{}", accepted_at.0),
            item_seq: lane_count + 3,
            lane_id: None,
            lane_seq: None,
            kind: "assistant_phase".to_string(),
            status: "pending".to_string(),
            source: "orchestrator".to_string(),
            title: Some("任务分配".to_string()),
            content: Some(orchestration_summary),
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
            timeline_entry_id: None,
            thread_visible: true,
            worker_visible: false,
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

fn task_graph_mainline_summary(
    task_store: &TaskStore,
    root_task_id: &TaskId,
    worker_lanes: &[ActiveExecutionTurnLane],
) -> String {
    let mut phase_count = 0usize;
    let mut pending = task_store.get_children(root_task_id);

    while let Some(task) = pending.pop() {
        match task.kind {
            TaskKind::Phase => phase_count += 1,
            TaskKind::Objective | TaskKind::WorkPackage | TaskKind::Decision => {}
            TaskKind::Action | TaskKind::Validation | TaskKind::Repair => {}
        }
        pending.extend(task_store.get_children(&task.task_id));
    }

    if phase_count > 0 {
        return format!(
            "已完成任务分派：上方卡片会按执行步骤展示负责人、目标和状态。接下来我会回收结果，继续在主线里整合判断并推进下一步。"
        );
    }

    format!(
        "已创建 {} 个执行步骤；上方卡片会展示负责人、目标和状态。接下来我会回收结果，继续在主线里整合判断并推进下一步。",
        worker_lanes.len(),
    )
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

fn build_task_policy() -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "Autonomous".to_string(),
        approval_mode: "DecisionOnly".to_string(),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        network_mode: "full".to_string(),
        command_mode: "full".to_string(),
        retry_limit: 1,
        repair_limit: 1,
        validation_profile: None,
        checkpoint_mode: "task_or_phase".to_string(),
        background_allowed: true,
        escalation_conditions: vec![
            "permission_boundary".to_string(),
            "irreversible_action".to_string(),
            "conflicting_requirements".to_string(),
            "architecture_fork".to_string(),
            "repair_budget_exhausted".to_string(),
            "missing_acceptance_criteria".to_string(),
            "unsafe_or_destructive_action".to_string(),
        ],
    }
}

fn task_phase_count_is_valid(count: usize) -> bool {
    (TASK_MIN_PHASES..=TASK_MAX_PHASES).contains(&count)
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
    let plan = decompose_mission(state, Some(prompt_text), now, workspace_id).ok_or_else(|| {
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
    )
}

fn insert_task_graph(
    task_store: &magi_orchestrator::task_store::TaskStore,
    mission_id: &MissionId,
    root_task_id: &TaskId,
    primary_action_task_id: &TaskId,
    accepted_at: UtcMillis,
    target_role: &str,
    now: &UtcMillis,
    plan: &TaskGraphPlan,
) -> Result<TaskGraphBuildResult, ApiError> {
    let task_policy = Some(build_task_policy());
    let mut total_task_count = 1usize;
    let mut leaf_action_task_ids = Vec::new();
    let mut validation_task_ids = Vec::new();
    let mut dispatch_task_ids = Vec::new();
    let mut phase_ids = Vec::with_capacity(plan.phases.len());
    let mut phase_action_ids_by_index: Vec<Vec<TaskId>> = Vec::with_capacity(plan.phases.len());

    for (phase_index, phase_plan) in plan.phases.iter().enumerate() {
        let phase_id = TaskId::new(format!("task-phase-{}-{}", accepted_at.0, phase_index));
        phase_ids.push(phase_id.clone());
        task_store.insert_task(make_dispatch_task(
            phase_id.clone(),
            mission_id.clone(),
            root_task_id.clone(),
            Some(root_task_id.clone()),
            TaskKind::Phase,
            phase_plan.title.clone(),
            format!("推进 {} 步骤", phase_plan.title),
            TaskStatus::Ready,
            *now,
            Some("architect"),
            None,
            task_policy.clone(),
        ));
        total_task_count += 1;

        let mut action_ids_by_title = std::collections::HashMap::<String, TaskId>::new();
        let mut phase_action_ids: Vec<TaskId> = Vec::new();

        for (package_index, package_plan) in phase_plan.work_packages.iter().enumerate() {
            let package_id = TaskId::new(format!(
                "task-wp-{}-{}-{}",
                accepted_at.0, phase_index, package_index
            ));
            task_store.insert_task(make_dispatch_task(
                package_id.clone(),
                mission_id.clone(),
                root_task_id.clone(),
                Some(phase_id.clone()),
                TaskKind::WorkPackage,
                package_plan.title.clone(),
                format!("完成 {}", package_plan.title),
                TaskStatus::Ready,
                *now,
                Some("integration-dev"),
                None,
                task_policy.clone(),
            ));
            total_task_count += 1;

            let mut current_package_action_ids = Vec::new();
            let mut current_package_dependency_specs = Vec::new();

            for (action_index, action_plan) in package_plan.actions.iter().enumerate() {
                let is_primary_action = phase_index == 0 && package_index == 0 && action_index == 0;
                let action_id = if is_primary_action {
                    primary_action_task_id.clone()
                } else {
                    TaskId::new(format!(
                        "task-action-{}-{}-{}-{}",
                        accepted_at.0, phase_index, package_index, action_index
                    ))
                };

                if action_ids_by_title
                    .insert(action_plan.title.clone(), action_id.clone())
                    .is_some()
                {
                    return Err(ApiError::internal_assembly(
                        "构建任务图失败",
                        format!("同一 phase 内的 action 标题重复: {}", action_plan.title),
                    ));
                }

                let action_role_candidate = if is_primary_action {
                    target_role.to_string()
                } else {
                    infer_dispatch_task_role(Some(action_plan.goal.as_str())).to_string()
                };
                let action_role =
                    compatible_task_role_for_kind(TaskKind::Action, Some(&action_role_candidate))
                        .ok_or_else(|| {
                        ApiError::internal_assembly(
                            "构建任务图失败",
                            format!("无法为 action {} 解析可执行角色", action_plan.title),
                        )
                    })?;

                task_store.insert_task(make_dispatch_task(
                    action_id.clone(),
                    mission_id.clone(),
                    root_task_id.clone(),
                    Some(package_id.clone()),
                    TaskKind::Action,
                    action_plan.title.clone(),
                    action_plan.goal.clone(),
                    TaskStatus::Ready,
                    *now,
                    Some(action_role.as_str()),
                    action_plan.write_scope.as_deref(),
                    task_policy.clone(),
                ));
                total_task_count += 1;
                current_package_action_ids.push(action_id.clone());
                current_package_dependency_specs
                    .push((action_id.clone(), action_plan.depends_on.clone()));
                if !is_primary_action {
                    leaf_action_task_ids.push(action_id.clone());
                    dispatch_task_ids.push(action_id.clone());
                }
                phase_action_ids.push(action_id.clone());
            }

            if current_package_action_ids.is_empty() {
                return Err(ApiError::internal_assembly(
                    "构建任务图失败",
                    format!("{} 不能为空动作列表", package_plan.title),
                ));
            }

            for (action_id, dependency_titles) in current_package_dependency_specs {
                for dependency_title in dependency_titles {
                    let dependency_id =
                        action_ids_by_title.get(&dependency_title).ok_or_else(|| {
                            ApiError::internal_assembly(
                                "构建任务图失败",
                                format!(
                                    "action 依赖引用不存在或不在同一 phase 内: {}",
                                    dependency_title
                                ),
                            )
                        })?;
                    task_store
                        .add_dependency(&action_id, dependency_id)
                        .map_err(|err| {
                            ApiError::internal_assembly("构建任务图失败", err.to_string())
                        })?;
                }
            }
        }

        if phase_action_ids.is_empty() {
            return Err(ApiError::internal_assembly(
                "构建任务图失败",
                format!("{} 至少需要一个 action", phase_plan.title),
            ));
        }
        phase_action_ids_by_index.push(phase_action_ids.clone());

        let validation_id =
            TaskId::new(format!("task-validation-{}-{}", accepted_at.0, phase_index));
        task_store.insert_task(make_dispatch_task(
            validation_id.clone(),
            mission_id.clone(),
            root_task_id.clone(),
            Some(phase_id.clone()),
            TaskKind::Validation,
            format!("{} 验证", phase_plan.title),
            phase_validation_goal(phase_index, plan.phases.len(), &phase_plan.title),
            TaskStatus::Ready,
            *now,
            Some("reviewer"),
            None,
            task_policy.clone(),
        ));
        for action_id in &phase_action_ids {
            task_store
                .add_dependency(&validation_id, action_id)
                .map_err(|err| {
                    ApiError::internal_assembly("构建任务图失败", err.to_string())
                })?;
        }
        validation_task_ids.push(validation_id.clone());
        dispatch_task_ids.push(validation_id);
        total_task_count += 1;
    }

    for phase_index in 1..phase_ids.len() {
        task_store
            .add_dependency(&phase_ids[phase_index], &phase_ids[phase_index - 1])
            .map_err(|err| ApiError::internal_assembly("构建任务图失败", err.to_string()))?;
    }

    if let Some(delivery_action_ids) = phase_action_ids_by_index.last() {
        let execution_action_ids = phase_action_ids_by_index
            .iter()
            .enumerate()
            .filter(|(phase_index, _)| *phase_index > 0 && *phase_index + 1 < plan.phases.len())
            .flat_map(|(_, action_ids)| action_ids.iter())
            .collect::<Vec<_>>();
        for delivery_action_id in delivery_action_ids {
            for execution_action_id in &execution_action_ids {
                task_store
                    .add_dependency(delivery_action_id, execution_action_id)
                    .map_err(|err| {
                        ApiError::internal_assembly("构建任务图失败", err.to_string())
                    })?;
            }
        }
    }

    validate_task_graph(
        task_store,
        root_task_id,
        plan,
        &phase_ids,
        &leaf_action_task_ids,
    )?;

    Ok(TaskGraphBuildResult {
        leaf_action_task_ids,
        validation_task_ids,
        dispatch_task_ids,
        total_task_count,
    })
}

fn phase_validation_goal(phase_index: usize, phase_count: usize, phase_title: &str) -> String {
    if phase_index == 0 {
        format!(
            "验证 {phase_title} 步骤产出是否包含目标、边界、执行计划和验收标准；只验证规划文本完整性，不验证后续执行结果、文件内容或工作区变更。"
        )
    } else if phase_index + 1 == phase_count {
        format!("验证 {phase_title} 步骤是否基于前序执行产出完成最终交付，不重复执行工具。")
    } else {
        format!("验证 {phase_title} 步骤是否按当前批次目标完成实际执行和工具结果。")
    }
}

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
    let root_task = task_store
        .get_task(root_task_id)
        .ok_or_else(|| ApiError::internal_assembly("重规划任务图失败", "root task 不存在"))?;
    if root_task.kind != TaskKind::Objective {
        return Err(ApiError::internal_assembly(
            "重规划任务图失败",
            "root 必须是 Objective",
        ));
    }
    if !root_task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.background_allowed)
    {
        return Err(ApiError::InvalidInput(
            "当前任务不存在任务图，不能重规划".to_string(),
        ));
    }

    let prompt_text = prompt.trim();
    if prompt_text.is_empty() {
        return Err(ApiError::internal_assembly(
            "重规划任务图失败",
            "prompt 为空",
        ));
    }

    let build_seed_base = UtcMillis::now();
    let build_seed = UtcMillis(
        build_seed_base
            .0
            .saturating_add(REPLAN_GRAPH_COUNTER.fetch_add(1, Ordering::Relaxed)),
    );
    let plan = decompose_mission(state, Some(prompt_text), &build_seed, workspace_id).ok_or_else(
        || ApiError::internal_assembly("重规划任务图失败", "无法生成结构化任务计划"),
    )?;
    if !task_phase_count_is_valid(plan.phases.len()) {
        return Err(ApiError::internal_assembly(
            "重规划任务图失败",
            format!("任务图需要 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 phase"),
        ));
    }

    let replan_cancel_candidates = task_store
        .collect_subtree_ids(root_task_id)
        .into_iter()
        .filter(|task_id| task_id != root_task_id)
        .filter(|task_id| {
            task_store.get_task(task_id).is_some_and(|task| {
                !matches!(
                    task.status,
                    TaskStatus::Completed
                        | TaskStatus::Failed
                        | TaskStatus::Cancelled
                        | TaskStatus::Skipped
                )
            })
        })
        .collect::<Vec<_>>();

    let target_role = context_task
        .and_then(|task| {
            task.executor_binding
                .as_ref()
                .map(|binding| binding.target_role.trim().to_string())
        })
        .filter(|role| !role.is_empty())
        .unwrap_or_else(|| infer_dispatch_task_role(Some(prompt_text)).to_string());

    let primary_action_task_id =
        TaskId::new(format!("task-act-replan-{}-{}", root_task_id, build_seed.0));
    let build = insert_task_graph(
        task_store,
        &root_task.mission_id,
        root_task_id,
        &primary_action_task_id,
        build_seed,
        &target_role,
        &build_seed,
        &plan,
    )?;

    let mut cancelled_task_ids = Vec::new();
    for task_id in &replan_cancel_candidates {
        if let Some(task) = task_store.get_task(task_id) {
            if matches!(
                task.status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Skipped
            ) {
                continue;
            }
            task_store
                .update_status(task_id, TaskStatus::Cancelled)
                .map_err(|err| {
                    ApiError::internal_assembly("重规划任务图失败", err.to_string())
                })?;
            if let Some(lease) = task_store.get_active_lease(task_id) {
                task_store.revoke_lease(task_id, &lease.lease_id);
            }
            cancelled_task_ids.push(task_id.clone());
        }
    }

    if root_task.status != TaskStatus::Running {
        task_store
            .update_status(root_task_id, TaskStatus::Running)
            .map_err(|err| ApiError::internal_assembly("重规划任务图失败", err.to_string()))?;
    }

    let event = task_events::task_graph_replanned_event(
        root_task.mission_id.as_str(),
        root_task_id.as_str(),
        build.total_task_count,
        reason,
    )
    .with_context(EventContext {
        mission_id: Some(root_task.mission_id.clone()),
        task_id: Some(root_task_id.clone()),
        ..EventContext::default()
    });
    let _ = state.event_bus.publish(event);

    Ok(TaskGraphReplanResult {
        cancelled_task_ids,
        primary_action_task_id,
        leaf_action_task_ids: build.leaf_action_task_ids,
        validation_task_ids: build.validation_task_ids,
        dispatch_task_ids: build.dispatch_task_ids,
    })
}

pub(crate) fn ensure_session_active_execution_chain(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), ApiError> {
    let has_active_chain = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
        .is_some();
    if has_active_chain {
        return Ok(());
    }
    Err(ApiError::InvalidInput(
        "当前会话没有可注册任务的活跃执行链".to_string(),
    ))
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
        let role_id = task
            .executor_binding
            .as_ref()
            .map(|binding| binding.target_role.trim())
            .filter(|role| !role.is_empty())
            .or_else(|| resolve_task_role(&task).map(str::trim))
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(ToOwned::to_owned);
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
            new_lanes.push(ActiveExecutionTurnLane {
                lane_id,
                lane_seq,
                task_id: spec.task_id.clone(),
                worker_id: worker_id.clone(),
                role_id,
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
                role_id: lane.role_id.clone(),
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
                thread_visible: false,
                worker_visible: true,
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

fn validate_task_graph(
    task_store: &magi_orchestrator::task_store::TaskStore,
    root_task_id: &TaskId,
    plan: &TaskGraphPlan,
    phase_ids: &[TaskId],
    leaf_action_task_ids: &[TaskId],
) -> Result<(), ApiError> {
    let root = task_store
        .get_task(root_task_id)
        .ok_or_else(|| ApiError::internal_assembly("校验任务图失败", "root task 不存在"))?;
    if root.kind != TaskKind::Objective {
        return Err(ApiError::internal_assembly(
            "校验任务图失败",
            "root 必须是 Objective",
        ));
    }
    if phase_ids.len() != plan.phases.len() || !task_phase_count_is_valid(plan.phases.len()) {
        return Err(ApiError::internal_assembly(
            "校验任务图失败",
            format!("任务图需要 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 Phase"),
        ));
    }

    for (phase_index, phase_id) in phase_ids.iter().enumerate() {
        let phase = task_store
            .get_task(phase_id)
            .ok_or_else(|| ApiError::internal_assembly("校验任务图失败", "Phase 不存在"))?;
        if phase.kind != TaskKind::Phase {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "Phase 节点类型错误",
            ));
        }
        if phase.title != plan.phases[phase_index].title {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "Phase 标题与结构化计划不一致",
            ));
        }

        if phase_index > 0
            && !phase
                .dependency_ids
                .iter()
                .any(|dep| dep == &phase_ids[phase_index - 1])
        {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "Phase 之间必须形成按计划批次推进的依赖链",
            ));
        }

        let packages: Vec<Task> = task_store
            .get_children(phase_id)
            .into_iter()
            .filter(|task| task.kind == TaskKind::WorkPackage)
            .collect();
        if packages.len() != plan.phases[phase_index].work_packages.len() || packages.is_empty() {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "Phase 的工作包数量与计划不一致",
            ));
        }

        let mut phase_action_ids = Vec::new();
        for (package_index, package) in packages.iter().enumerate() {
            let package_plan = &plan.phases[phase_index].work_packages[package_index];
            if package.title != package_plan.title {
                return Err(ApiError::internal_assembly(
                    "校验任务图失败",
                    "WorkPackage 标题与结构化计划不一致",
                ));
            }
            let children = task_store.get_children(&package.task_id);
            let actions: Vec<Task> = children
                .iter()
                .filter(|task| task.kind == TaskKind::Action)
                .cloned()
                .collect();
            if actions.len() != package_plan.actions.len() || actions.is_empty() {
                return Err(ApiError::internal_assembly(
                    "校验任务图失败",
                    "WorkPackage 的 Action 数量与计划不一致",
                ));
            }
            phase_action_ids.extend(actions.iter().map(|action| action.task_id.clone()));
        }
        let validations: Vec<Task> = task_store
            .get_children(phase_id)
            .into_iter()
            .filter(|task| task.kind == TaskKind::Validation)
            .collect();
        if validations.len() != 1 {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "每个 Phase 必须包含 1 个 Validation",
            ));
        }
        let validation = &validations[0];
        if !validation.dependency_ids.iter().all(|dependency_id| {
            phase_action_ids
                .iter()
                .any(|action_id| action_id == dependency_id)
        }) {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "Validation 必须依赖当前 Phase 内的 Action",
            ));
        }
    }

    for action_id in leaf_action_task_ids {
        let action = task_store
            .get_task(action_id)
            .ok_or_else(|| ApiError::internal_assembly("校验任务图失败", "Action 不存在"))?;
        if action.kind != TaskKind::Action {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "叶子节点必须是 Action",
            ));
        }
        if action.parent_task_id.is_none() {
            return Err(ApiError::internal_assembly(
                "校验任务图失败",
                "Action 必须有父节点",
            ));
        }
    }

    Ok(())
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskGraphPlan {
    phases: Vec<TaskPhasePlan>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskPhasePlan {
    title: String,
    work_packages: Vec<TaskWorkPackagePlan>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskWorkPackagePlan {
    title: String,
    actions: Vec<TaskActionPlan>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskActionPlan {
    title: String,
    goal: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    write_scope: Option<String>,
}

fn task_plan_tool() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: TASK_PLAN_TOOL_NAME.to_string(),
            description: "创建严格结构化的任务图计划，供 Task Graph 构建器直接消费。"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["phases"],
                "properties": {
                    "phases": {
                        "type": "array",
                        "minItems": TASK_MIN_PHASES,
                        "maxItems": TASK_MAX_PHASES,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["title", "workPackages"],
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "阶段标题。第一阶段必须是规划，最后一阶段必须是交付；中间可以有一个或多个按批次推进的执行阶段。"
                                },
                                "workPackages": {
                                    "type": "array",
                                    "minItems": 1,
                                    "items": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "required": ["title", "actions"],
                                        "properties": {
                                            "title": {
                                                "type": "string",
                                                "description": "工作包标题，表达一组可交付的相关动作。"
                                            },
                                            "actions": {
                                                "type": "array",
                                                "minItems": 1,
                                                "items": {
                                                    "type": "object",
                                                    "additionalProperties": false,
                                                    "required": ["title", "goal"],
                                                    "properties": {
                                                        "title": {
                                                            "type": "string",
                                                            "description": "动作标题，必须短小且可执行。"
                                                        },
                                                        "goal": {
                                                            "type": "string",
                                                            "description": "动作目标，必须说明完成标准或产出。"
                                                        },
                                                        "dependsOn": {
                                                            "type": "array",
                                                            "items": { "type": "string" },
                                                            "description": "同一阶段内已定义动作的标题。"
                                                        },
                                                        "writeScope": {
                                                            "type": ["string", "null"],
                                                            "description": "可选写入范围，例如 crates/magi-api 或 web/src。"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }),
        },
    }
}

fn decompose_mission(
    state: &ApiState,
    prompt: Option<&str>,
    _now: &UtcMillis,
    workspace_id: &Option<WorkspaceId>,
) -> Option<TaskGraphPlan> {
    let prompt_text = prompt.filter(|s| !s.trim().is_empty())?;
    let client = state.model_bridge_client()?;
    let workspace_context = state
        .workspace_root_path(workspace_id)
        .map(|path| {
            format!(
                "\n当前工作区根目录：{}\n如果任务目标提到当前项目、当前仓库、本项目或 codebase，计划里的 action goal 必须要求读取这个工作区的真实目录、配置和关键源码，不要让 worker 等用户粘贴项目结构。",
                path.display()
            )
        })
        .unwrap_or_default();
    let request = ModelInvocationRequest {
        provider: LOOPBACK_MODEL_PROVIDER.to_string(),
        prompt: format!(
            "任务图规划器。\n\
             请只调用 {TASK_PLAN_TOOL_NAME} 工具输出结构化计划，不要返回自然语言正文。\n\
             计划必须包含 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 phase：第一 phase 是规划，最后 phase 是交付，中间 phase 是一个或多个按实际批次推进的执行阶段。\n\
             如果任务目标包含“第一批/第二批/下一批/继续创建任务/发现后继续推进/多段任务”等纵向编排要求，必须把每一批推进建模为独立执行 phase，不能把多批命令塞进同一个 action。\n\
             每个 phase 至少 1 个 workPackage，每个 workPackage 至少 1 个 action。\n\
             action 的 dependsOn 只能引用同一 phase 内已定义的较早 action 标题。\n\
             action goal 必须描述可验证产出或完成标准。\n\
             原始任务目标是唯一主事实，必须逐字保留其中的路径、工具名、命令、标记字符串和“必须/要求”条款；不得把它改写成历史任务、泛化检查或只读替代目标。\n\
             规划 phase 只输出目标、边界、执行计划和验收标准，不得调用工具，不得执行用户目标里的写入、删除、移动、补丁或其他有副作用操作。\n\
             中间执行 phase 是唯一可以执行用户目标和写操作的阶段；如果目标包含明确工具链路，对应批次的执行 action goal 必须按原始顺序列出这些工具和验收标记。\n\
             交付 phase 只能基于执行产出和验证证据总结，不得调用工具，不得重复写入、删除、移动、补丁或重新执行用户目标。\n\
             任务目标：\n<<<MAGI_TASK_GOAL>>>\n{}\n<<<END_MAGI_TASK_GOAL>>>{}",
            prompt_text, workspace_context
        ),
        messages: None,
        tools: Some(vec![task_plan_tool()]),
        tool_choice: Some(ChatToolChoice::force_function(TASK_PLAN_TOOL_NAME)),
    };
    let response = client.invoke(request).ok()?;
    if !response.ok {
        return None;
    }
    parse_decomposition_response(&response.payload, prompt_text)
}

fn parse_decomposition_response(
    response: &str,
    original_prompt: &str,
) -> Option<TaskGraphPlan> {
    let trimmed = response.trim();
    let normalized = trimmed
        .strip_prefix("loopback-model::")
        .unwrap_or(trimmed)
        .trim();

    if let Ok(payload) = serde_json::from_str::<ChatCompletionPayload>(normalized)
        && let Some(arguments) = payload
            .tool_calls
            .iter()
            .find(|call| call.function.name == TASK_PLAN_TOOL_NAME)
            .map(|call| call.function.arguments.as_str())
        && let Ok(plan_value) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(plan) = parse_decomposition_plan(plan_value, original_prompt)
    {
        return Some(plan);
    }

    let plan_value: serde_json::Value = serde_json::from_str(normalized).ok()?;
    parse_decomposition_plan(plan_value, original_prompt)
}

fn parse_decomposition_plan(
    plan_value: serde_json::Value,
    original_prompt: &str,
) -> Option<TaskGraphPlan> {
    let mut plan: TaskGraphPlan = serde_json::from_value(plan_value).ok()?;
    if !task_phase_count_is_valid(plan.phases.len()) {
        return None;
    }

    let last_phase_index = plan.phases.len().saturating_sub(1);
    for (phase_index, phase) in plan.phases.iter_mut().enumerate() {
        phase.title = normalize_plan_text(&phase.title, original_prompt)?;
        if phase.work_packages.is_empty() {
            return None;
        }
        for package in &mut phase.work_packages {
            package.title = normalize_plan_text(&package.title, original_prompt)?;
            if package.actions.is_empty() {
                return None;
            }
            for action in &mut package.actions {
                action.title = normalize_plan_text(&action.title, original_prompt)?;
                action.goal = normalize_plan_text(&action.goal, original_prompt)?;
                if phase_index == last_phase_index {
                    action.title = normalize_delivery_action_title(&action.title);
                }
                if action
                    .depends_on
                    .iter()
                    .any(|dependency| dependency.trim().is_empty())
                {
                    return None;
                }
                action.write_scope = action
                    .write_scope
                    .take()
                    .map(|scope| scope.trim().to_string())
                    .filter(|scope| !scope.is_empty());
            }
        }
    }
    Some(plan)
}

fn normalize_delivery_action_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed == "验证交付" || trimmed == "交付验证" {
        return "汇总结果".to_string();
    }
    trimmed.to_string()
}

fn normalize_plan_text(value: &str, original_prompt: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() || text == original_prompt.trim() {
        return None;
    }
    Some(text.to_string())
}

/// Build a Task with sensible defaults for dispatch execution entries.
fn make_dispatch_task(
    task_id: TaskId,
    mission_id: MissionId,
    root_task_id: TaskId,
    parent_task_id: Option<TaskId>,
    kind: TaskKind,
    title: String,
    goal: String,
    status: TaskStatus,
    now: UtcMillis,
    target_role: Option<&str>,
    write_scope: Option<&str>,
    policy_snapshot: Option<TaskPolicy>,
) -> Task {
    Task {
        task_id,
        mission_id,
        root_task_id,
        parent_task_id,
        kind,
        title,
        goal,
        status,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot,
        executor_binding: target_role.map(|role| ExecutorBinding {
            target_role: role.to_string(),
            capability_requirements: Vec::new(),
            parallelism_group: None,
            exclusive_scope: None,
            worker_selector: None,
        }),
        context_refs: Vec::new(),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: write_scope.map(str::to_string),
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        created_at: now,
        updated_at: now,
    }
}

pub(crate) fn cleanup_task_tree(
    task_store: &magi_orchestrator::task_store::TaskStore,
    root_task_id: &TaskId,
) {
    let task_ids = task_store.collect_subtree_ids(root_task_id);
    for task_id in task_ids.into_iter().rev() {
        let _ = task_store.remove_task(&task_id);
    }
}

fn infer_dispatch_task_role(skill_name: Option<&str>) -> &'static str {
    let Some(skill_name) = skill_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return "integration-dev";
    };

    let skill = skill_name.to_ascii_lowercase();
    if skill.contains("front") || skill.contains("ui") {
        "frontend-dev"
    } else if skill.contains("back") || skill.contains("api") || skill.contains("server") {
        "backend-dev"
    } else if skill.contains("review") || skill.contains("audit") {
        "reviewer"
    } else if skill.contains("test") || skill.contains("qa") || skill.contains("verify") {
        "test-engineer"
    } else if skill.contains("doc") || skill.contains("write") {
        "doc-writer"
    } else if skill.contains("debug") || skill.contains("fix") || skill.contains("bug") {
        "debugger"
    } else if skill.contains("data") || skill.contains("etl") || skill.contains("metric") {
        "data-engineer"
    } else if skill.contains("devops") || skill.contains("infra") || skill.contains("deploy") {
        "devops-engineer"
    } else if skill.contains("security") || skill.contains("auth") || skill.contains("sec") {
        "security-analyst"
    } else if skill.contains("arch") || skill.contains("design") {
        "architect"
    } else {
        "integration-dev"
    }
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
        assert!(user_item.thread_visible);
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
    fn task_turn_keeps_normal_mainline_dialog_and_orchestrator_steps() {
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
            .filter(|item| item.thread_visible)
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
            visible_items.iter().any(|(kind, content)| {
                *kind == "assistant_phase" && content.contains("拆成可执行步骤")
            }),
            "编排者开始拆解任务的行为必须作为主线普通文本"
        );
        assert!(
            visible_items.iter().any(|(kind, content)| {
                *kind == "assistant_phase" && content.contains("继续在主线里整合")
            }),
            "任务分配卡之后必须继续保留编排者自己的行为说明"
        );
        assert!(
            turn.items.iter().any(|item| item.kind == "worker_spawned"
                && !item.thread_visible
                && item.worker_visible),
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
        let mission_id = MissionId::new(format!("mission-session-action-{}", accepted_at.0));
        assert!(
            task_store.get_tasks_by_mission(&mission_id).is_empty(),
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
        for role in action_roles {
            assert!(
                magi_orchestrator::task_worker_catalog::supported_kinds_for_role(&role)
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
        assert_eq!(turn.worker_lanes[0].role_id.as_deref(), Some("reviewer"));
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
