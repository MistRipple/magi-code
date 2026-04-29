use magi_bridge_client::{
    ChatCompletionPayload, ChatToolChoice, ChatToolDefinition, ChatToolFunctionDefinition,
    ModelInvocationRequest, SHADOW_MODEL_PROVIDER,
};
use magi_core::{
    ExecutionOwnership, ExecutorBinding, MissionId, SessionId, Task, TaskExecutionTarget, TaskId,
    TaskKind, TaskPolicy, TaskStatus, UtcMillis, WorkerId,
};
use magi_event_bus::{EventContext, task_events};
use magi_orchestrator::{ExecutionWritebackPlans, task_worker_catalog::resolve_task_role};
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
    task_execution::{DispatchSubmissionRequest, ShadowTaskExecutionPlan},
};

pub(crate) struct ShadowTaskGraphSubmission {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
}

static REPLAN_GRAPH_COUNTER: AtomicU64 = AtomicU64::new(1);
const DEEP_TASK_PLAN_TOOL_NAME: &str = "create_deep_task_plan";

#[derive(Clone, Debug)]
pub(crate) struct DeepTaskGraphBuildResult {
    pub leaf_action_task_ids: Vec<TaskId>,
    pub validation_task_ids: Vec<TaskId>,
    pub dispatch_task_ids: Vec<TaskId>,
    pub total_task_count: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct DeepTaskGraphReplanResult {
    pub cancelled_task_ids: Vec<TaskId>,
    pub primary_action_task_id: TaskId,
    pub leaf_action_task_ids: Vec<TaskId>,
    pub validation_task_ids: Vec<TaskId>,
    pub dispatch_task_ids: Vec<TaskId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionExecutionBranchUpdateMode {
    Append,
    Replace,
}

#[derive(Clone, Debug)]
struct SessionExecutionBranchSpec {
    task_id: TaskId,
    is_primary: bool,
}

pub(crate) fn run_shadow_dispatch_submission(
    state: &ApiState,
    request: &DispatchSubmissionRequest,
) -> Result<ShadowTaskGraphSubmission, ApiError> {
    let _ = state.shadow_execution_pipeline().ok_or_else(|| {
        ApiError::internal_assembly(
            "执行 shadow dispatch 失败",
            "shadow execution pipeline 未配置",
        )
    })?;
    let task_store = state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("执行 shadow dispatch 失败", "task_store 未配置")
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
    if request.deep_task && execution_goal.is_none() {
        return Err(ApiError::InvalidInput(
            "深度任务必须提供非空 execution_goal".to_string(),
        ));
    }

    let mission_id = MissionId::new(format!("mission-session-action-{}", accepted_at.0));
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));

    let obj_task_id = TaskId::new(format!("task-obj-{}", accepted_at.0));
    let act_task_id = TaskId::new(format!("task-act-{}", accepted_at.0));

    let now = UtcMillis::now();
    let task_goal_text = execution_goal.or(trimmed_text).unwrap_or("").to_string();
    let objective = make_shadow_task(
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
        Some(build_policy_for_mode(request.deep_task)),
    );
    task_store.insert_task(objective);

    if !request.deep_task {
        let action = make_shadow_task(
            act_task_id.clone(),
            mission_id.clone(),
            obj_task_id.clone(),
            Some(obj_task_id.clone()),
            TaskKind::Action,
            request.task_title.clone(),
            task_goal_text.clone(),
            TaskStatus::Ready,
            now,
            Some(
                request
                    .target_role
                    .as_deref()
                    .unwrap_or_else(|| infer_dispatch_task_role(request.skill_name.as_deref())),
            ),
            None,
            Some(build_policy_for_mode(false)),
        );
        task_store.insert_task(action);
    }

    let deep_graph = if request.deep_task {
        match build_deep_task_graph(
            state,
            &mission_id,
            &obj_task_id,
            &act_task_id,
            accepted_at,
            request
                .target_role
                .as_deref()
                .unwrap_or_else(|| infer_dispatch_task_role(request.skill_name.as_deref())),
            execution_goal.expect("deep_task execution_goal 已在建图前校验"),
            &now,
        ) {
            Ok(graph) => Some(graph),
            Err(err) => {
                cleanup_shadow_task_tree(task_store, &obj_task_id);
                return Err(err);
            }
        }
    } else {
        None
    };
    let mut total_task_count = 2usize;
    let mut dispatch_task_ids: Vec<TaskId> = Vec::new();
    if let Some(graph) = deep_graph {
        total_task_count = graph.total_task_count;
        dispatch_task_ids = graph.dispatch_task_ids;
    }

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
    state.shadow_task_execution_registry().insert(
        act_task_id.clone(),
        ShadowTaskExecutionPlan::Dispatch {
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
                    deep_task: request.deep_task,
                },
            ),
            use_tools: request.deep_task,
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
        state.shadow_task_execution_registry().insert(
            sub_task_id.clone(),
            ShadowTaskExecutionPlan::Dispatch {
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
        use_tools: request.deep_task,
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
                thread_visible: !request.deep_task,
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
                content: Some("已接收请求，正在整理执行步骤。".to_string()),
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
        worker_lanes: dispatch_task_ids
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
    for (index, sub_task_id) in dispatch_task_ids.iter().enumerate() {
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
            content: Some(format!("已为 {} 创建执行分支。", lane_title)),
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
    current_turn.normalize();
    Ok(ShadowTaskGraphSubmission {
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
                deep_task: request.deep_task,
                skill_name: request.skill_name.clone(),
            },
            current_turn: Some(current_turn),
        }),
    })
}

#[cfg(test)]
pub(crate) fn run_shadow_session_action(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    accepted_at: UtcMillis,
    session_id: &SessionId,
    entry_id: &str,
    trimmed_text: Option<&str>,
) -> Result<ShadowTaskGraphSubmission, ApiError> {
    let mission_title = request.mission_title(trimmed_text);
    run_shadow_dispatch_submission(
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
            execution_goal: None,
            deep_task: request.deep_task,
            skill_name: request.skill_name.clone(),
            target_role: None,
            request_id: request.request_id(),
            user_message_id: request.user_message_id(),
            placeholder_message_id: request.placeholder_message_id(),
        },
    )
}

fn build_policy_for_mode(deep_task: bool) -> TaskPolicy {
    if deep_task {
        TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 3,
            repair_limit: 3,
            validation_profile: Some("required".to_string()),
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
    } else {
        TaskPolicy {
            autonomy_level: "Assisted".to_string(),
            approval_mode: "Interactive".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: None,
            checkpoint_mode: "turn".to_string(),
            background_allowed: false,
            escalation_conditions: vec![
                "permission_boundary".to_string(),
                "irreversible_action".to_string(),
                "conflicting_requirements".to_string(),
            ],
        }
    }
}

fn build_deep_task_graph(
    state: &ApiState,
    mission_id: &MissionId,
    root_task_id: &TaskId,
    primary_action_task_id: &TaskId,
    accepted_at: UtcMillis,
    target_role: &str,
    prompt: &str,
    now: &UtcMillis,
) -> Result<DeepTaskGraphBuildResult, ApiError> {
    let prompt_text = prompt.trim();
    if prompt_text.is_empty() {
        return Err(ApiError::internal_assembly(
            "构建深度任务图失败",
            "prompt 为空",
        ));
    }
    let plan = decompose_mission(state, Some(prompt_text), now).ok_or_else(|| {
        ApiError::internal_assembly("构建深度任务图失败", "无法生成结构化深度计划")
    })?;
    if plan.phases.len() != 3 {
        return Err(ApiError::internal_assembly(
            "构建深度任务图失败",
            "深度模式必须包含 3 个 phase",
        ));
    }

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("构建深度任务图失败", "task_store 未配置"))?;
    insert_deep_task_graph(
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

fn insert_deep_task_graph(
    task_store: &magi_orchestrator::task_store::TaskStore,
    mission_id: &MissionId,
    root_task_id: &TaskId,
    primary_action_task_id: &TaskId,
    accepted_at: UtcMillis,
    target_role: &str,
    now: &UtcMillis,
    plan: &DeepTaskGraphPlan,
) -> Result<DeepTaskGraphBuildResult, ApiError> {
    let root_policy = Some(build_policy_for_mode(true));
    let mut total_task_count = 1usize;
    let mut leaf_action_task_ids = Vec::new();
    let mut validation_task_ids = Vec::new();
    let mut dispatch_task_ids = Vec::new();
    let mut phase_ids = Vec::with_capacity(plan.phases.len());

    for (phase_index, phase_plan) in plan.phases.iter().enumerate() {
        let phase_id = TaskId::new(format!("task-phase-{}-{}", accepted_at.0, phase_index));
        phase_ids.push(phase_id.clone());
        task_store.insert_task(make_shadow_task(
            phase_id.clone(),
            mission_id.clone(),
            root_task_id.clone(),
            Some(root_task_id.clone()),
            TaskKind::Phase,
            phase_plan.title.clone(),
            format!("推进 {} 阶段", phase_plan.title),
            TaskStatus::Ready,
            *now,
            Some("architect"),
            None,
            root_policy.clone(),
        ));
        total_task_count += 1;

        let mut action_ids_by_title = std::collections::HashMap::<String, TaskId>::new();
        let mut phase_action_ids: Vec<TaskId> = Vec::new();

        for (package_index, package_plan) in phase_plan.work_packages.iter().enumerate() {
            let package_id = TaskId::new(format!(
                "task-wp-{}-{}-{}",
                accepted_at.0, phase_index, package_index
            ));
            task_store.insert_task(make_shadow_task(
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
                root_policy.clone(),
            ));
            total_task_count += 1;

            let mut current_package_action_ids = Vec::new();
            let mut current_package_dependency_specs = Vec::new();

            for (action_index, action_plan) in package_plan.actions.iter().enumerate() {
                let is_primary_action = phase_index == 1 && package_index == 0 && action_index == 0;
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
                        "构建深度任务图失败",
                        format!("同一 phase 内的 action 标题重复: {}", action_plan.title),
                    ));
                }

                let action_role = if is_primary_action {
                    target_role
                } else {
                    infer_dispatch_task_role(Some(action_plan.goal.as_str()))
                };

                task_store.insert_task(make_shadow_task(
                    action_id.clone(),
                    mission_id.clone(),
                    root_task_id.clone(),
                    Some(package_id.clone()),
                    TaskKind::Action,
                    action_plan.title.clone(),
                    action_plan.goal.clone(),
                    TaskStatus::Ready,
                    *now,
                    Some(action_role),
                    action_plan.write_scope.as_deref(),
                    root_policy.clone(),
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
                    "构建深度任务图失败",
                    format!("{} 不能为空动作列表", package_plan.title),
                ));
            }

            for (action_id, dependency_titles) in current_package_dependency_specs {
                for dependency_title in dependency_titles {
                    let dependency_id =
                        action_ids_by_title.get(&dependency_title).ok_or_else(|| {
                            ApiError::internal_assembly(
                                "构建深度任务图失败",
                                format!(
                                    "action 依赖引用不存在或不在同一 phase 内: {}",
                                    dependency_title
                                ),
                            )
                        })?;
                    task_store
                        .add_dependency(&action_id, dependency_id)
                        .map_err(|err| {
                            ApiError::internal_assembly("构建深度任务图失败", err.to_string())
                        })?;
                }
            }
        }

        if phase_action_ids.is_empty() {
            return Err(ApiError::internal_assembly(
                "构建深度任务图失败",
                format!("{} 至少需要一个 action", phase_plan.title),
            ));
        }

        let validation_id =
            TaskId::new(format!("task-validation-{}-{}", accepted_at.0, phase_index));
        task_store.insert_task(make_shadow_task(
            validation_id.clone(),
            mission_id.clone(),
            root_task_id.clone(),
            Some(phase_id.clone()),
            TaskKind::Validation,
            format!("{} 验证", phase_plan.title),
            format!("验证 {} 阶段的全部产出", phase_plan.title),
            TaskStatus::Ready,
            *now,
            Some("reviewer"),
            None,
            root_policy.clone(),
        ));
        for action_id in &phase_action_ids {
            task_store
                .add_dependency(&validation_id, action_id)
                .map_err(|err| {
                    ApiError::internal_assembly("构建深度任务图失败", err.to_string())
                })?;
        }
        validation_task_ids.push(validation_id.clone());
        dispatch_task_ids.push(validation_id);
        total_task_count += 1;
    }

    task_store
        .add_dependency(&phase_ids[1], &phase_ids[0])
        .map_err(|err| ApiError::internal_assembly("构建深度任务图失败", err.to_string()))?;
    task_store
        .add_dependency(&phase_ids[2], &phase_ids[1])
        .map_err(|err| ApiError::internal_assembly("构建深度任务图失败", err.to_string()))?;

    validate_deep_task_graph(
        task_store,
        root_task_id,
        plan,
        &phase_ids,
        &leaf_action_task_ids,
    )?;

    Ok(DeepTaskGraphBuildResult {
        leaf_action_task_ids,
        validation_task_ids,
        dispatch_task_ids,
        total_task_count,
    })
}

pub(crate) fn replan_deep_task_graph(
    state: &ApiState,
    root_task_id: &TaskId,
    prompt: &str,
    context_task: Option<&Task>,
    reason: &str,
) -> Result<DeepTaskGraphReplanResult, ApiError> {
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("重规划深度任务图失败", "task_store 未配置"))?;
    let root_task = task_store
        .get_task(root_task_id)
        .ok_or_else(|| ApiError::internal_assembly("重规划深度任务图失败", "root task 不存在"))?;
    if root_task.kind != TaskKind::Objective {
        return Err(ApiError::internal_assembly(
            "重规划深度任务图失败",
            "root 必须是 Objective",
        ));
    }
    if !root_task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.background_allowed)
    {
        return Err(ApiError::InvalidInput(
            "当前任务不是深度模式，不能重规划任务图".to_string(),
        ));
    }

    let prompt_text = prompt.trim();
    if prompt_text.is_empty() {
        return Err(ApiError::internal_assembly(
            "重规划深度任务图失败",
            "prompt 为空",
        ));
    }

    let build_seed_base = UtcMillis::now();
    let build_seed = UtcMillis(
        build_seed_base
            .0
            .saturating_add(REPLAN_GRAPH_COUNTER.fetch_add(1, Ordering::Relaxed)),
    );
    let plan = decompose_mission(state, Some(prompt_text), &build_seed).ok_or_else(|| {
        ApiError::internal_assembly("重规划深度任务图失败", "无法生成结构化深度计划")
    })?;
    if plan.phases.len() != 3 {
        return Err(ApiError::internal_assembly(
            "重规划深度任务图失败",
            "深度模式必须包含 3 个 phase",
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
    let build = insert_deep_task_graph(
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
                    ApiError::internal_assembly("重规划深度任务图失败", err.to_string())
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
            .map_err(|err| ApiError::internal_assembly("重规划深度任务图失败", err.to_string()))?;
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

    Ok(DeepTaskGraphReplanResult {
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

pub(crate) fn register_appended_task_execution_branch(
    state: &ApiState,
    session_id: &SessionId,
    task_id: &TaskId,
) -> Result<(), ApiError> {
    update_session_execution_branches(
        state,
        session_id,
        &[SessionExecutionBranchSpec {
            task_id: task_id.clone(),
            is_primary: false,
        }],
        SessionExecutionBranchUpdateMode::Append,
    )
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
    update_session_execution_branches(
        state,
        session_id,
        &branch_specs,
        SessionExecutionBranchUpdateMode::Replace,
    )
}

fn update_session_execution_branches(
    state: &ApiState,
    session_id: &SessionId,
    branch_specs: &[SessionExecutionBranchSpec],
    mode: SessionExecutionBranchUpdateMode,
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
    if mode == SessionExecutionBranchUpdateMode::Replace {
        for task_id in existing_task_ids.difference(&new_task_ids) {
            let _ = state.shadow_task_execution_registry().remove(task_id);
        }
    }

    let next_lane_seq = active_chain
        .current_turn
        .as_ref()
        .and_then(|turn| turn.worker_lanes.iter().map(|lane| lane.lane_seq).max())
        .unwrap_or(0)
        .saturating_add(1);
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
        let role_id = resolve_task_role(&task)
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(ToOwned::to_owned);
        let lane_seq = if spec.is_primary {
            None
        } else {
            let seq = if mode == SessionExecutionBranchUpdateMode::Replace {
                appended_lane_count + 1
            } else {
                next_lane_seq + appended_lane_count
            };
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
        state.shadow_task_execution_registry().insert(
            spec.task_id.clone(),
            ShadowTaskExecutionPlan::Dispatch {
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

    match mode {
        SessionExecutionBranchUpdateMode::Append => {
            active_chain
                .branches
                .retain(|branch| !new_task_ids.contains(&branch.task_id));
            active_chain.branches.extend(new_branches.clone());
        }
        SessionExecutionBranchUpdateMode::Replace => {
            active_chain.branches = new_branches.clone();
        }
    }
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
        match mode {
            SessionExecutionBranchUpdateMode::Append => {
                turn.worker_lanes
                    .retain(|lane| !new_task_ids.contains(&lane.task_id));
                turn.worker_lanes.extend(new_lanes.clone());
            }
            SessionExecutionBranchUpdateMode::Replace => {
                turn.worker_lanes = new_lanes.clone();
                turn.items.retain(|item| {
                    item.kind != "worker_spawned"
                        || item
                            .task_id
                            .as_ref()
                            .is_some_and(|task_id| new_task_ids.contains(task_id))
                });
            }
        }
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
                content: Some(format!("已为 {} 创建执行分支。", lane.title)),
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

fn validate_deep_task_graph(
    task_store: &magi_orchestrator::task_store::TaskStore,
    root_task_id: &TaskId,
    plan: &DeepTaskGraphPlan,
    phase_ids: &[TaskId],
    leaf_action_task_ids: &[TaskId],
) -> Result<(), ApiError> {
    let root = task_store
        .get_task(root_task_id)
        .ok_or_else(|| ApiError::internal_assembly("校验深度任务图失败", "root task 不存在"))?;
    if root.kind != TaskKind::Objective {
        return Err(ApiError::internal_assembly(
            "校验深度任务图失败",
            "root 必须是 Objective",
        ));
    }
    if phase_ids.len() != 3 || plan.phases.len() != 3 {
        return Err(ApiError::internal_assembly(
            "校验深度任务图失败",
            "深度模式必须包含 3 个 Phase",
        ));
    }

    for (phase_index, phase_id) in phase_ids.iter().enumerate() {
        let phase = task_store
            .get_task(phase_id)
            .ok_or_else(|| ApiError::internal_assembly("校验深度任务图失败", "Phase 不存在"))?;
        if phase.kind != TaskKind::Phase {
            return Err(ApiError::internal_assembly(
                "校验深度任务图失败",
                "Phase 节点类型错误",
            ));
        }
        if phase.title != plan.phases[phase_index].title {
            return Err(ApiError::internal_assembly(
                "校验深度任务图失败",
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
                "校验深度任务图失败",
                "Phase 之间必须形成规划→执行→交付依赖链",
            ));
        }

        let packages: Vec<Task> = task_store
            .get_children(phase_id)
            .into_iter()
            .filter(|task| task.kind == TaskKind::WorkPackage)
            .collect();
        if packages.len() != plan.phases[phase_index].work_packages.len() || packages.is_empty() {
            return Err(ApiError::internal_assembly(
                "校验深度任务图失败",
                "Phase 的工作包数量与计划不一致",
            ));
        }

        let mut phase_action_ids = Vec::new();
        for (package_index, package) in packages.iter().enumerate() {
            let package_plan = &plan.phases[phase_index].work_packages[package_index];
            if package.title != package_plan.title {
                return Err(ApiError::internal_assembly(
                    "校验深度任务图失败",
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
                    "校验深度任务图失败",
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
                "校验深度任务图失败",
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
                "校验深度任务图失败",
                "Validation 必须依赖当前 Phase 内的 Action",
            ));
        }
    }

    for action_id in leaf_action_task_ids {
        let action = task_store
            .get_task(action_id)
            .ok_or_else(|| ApiError::internal_assembly("校验深度任务图失败", "Action 不存在"))?;
        if action.kind != TaskKind::Action {
            return Err(ApiError::internal_assembly(
                "校验深度任务图失败",
                "叶子节点必须是 Action",
            ));
        }
        if action.parent_task_id.is_none() {
            return Err(ApiError::internal_assembly(
                "校验深度任务图失败",
                "Action 必须有父节点",
            ));
        }
    }

    Ok(())
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeepTaskGraphPlan {
    phases: Vec<DeepTaskPhasePlan>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeepTaskPhasePlan {
    title: String,
    work_packages: Vec<DeepTaskWorkPackagePlan>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeepTaskWorkPackagePlan {
    title: String,
    actions: Vec<DeepTaskActionPlan>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeepTaskActionPlan {
    title: String,
    goal: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    write_scope: Option<String>,
}

fn deep_task_plan_tool() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: DEEP_TASK_PLAN_TOOL_NAME.to_string(),
            description: "创建严格结构化的深度任务图计划，供 Task Graph 构建器直接消费。"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["phases"],
                "properties": {
                    "phases": {
                        "type": "array",
                        "minItems": 3,
                        "maxItems": 3,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["title", "workPackages"],
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "阶段标题，建议依次覆盖规划、执行、交付。"
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
) -> Option<DeepTaskGraphPlan> {
    let prompt_text = prompt.filter(|s| !s.trim().is_empty())?;
    let client = state.task_planning_model_client()?;
    let request = ModelInvocationRequest {
        provider: SHADOW_MODEL_PROVIDER.to_string(),
        prompt: format!(
            "深度任务图规划器。\n\
             请只调用 {DEEP_TASK_PLAN_TOOL_NAME} 工具输出结构化计划，不要返回自然语言正文。\n\
             计划必须恰好包含 3 个 phase，分别覆盖规划、执行、交付。\n\
             每个 phase 至少 1 个 workPackage，每个 workPackage 至少 1 个 action。\n\
             action 的 dependsOn 只能引用同一 phase 内已定义的较早 action 标题。\n\
             action goal 必须描述可验证产出或完成标准。\n\
             任务目标：{}",
            prompt_text
        ),
        messages: None,
        tools: Some(vec![deep_task_plan_tool()]),
        tool_choice: Some(ChatToolChoice::force_function(DEEP_TASK_PLAN_TOOL_NAME)),
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
) -> Option<DeepTaskGraphPlan> {
    let trimmed = response.trim();
    let normalized = trimmed
        .strip_prefix("shadow-model::")
        .unwrap_or(trimmed)
        .trim();

    if let Ok(payload) = serde_json::from_str::<ChatCompletionPayload>(normalized)
        && let Some(arguments) = payload
            .tool_calls
            .iter()
            .find(|call| call.function.name == DEEP_TASK_PLAN_TOOL_NAME)
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
) -> Option<DeepTaskGraphPlan> {
    let mut plan: DeepTaskGraphPlan = serde_json::from_value(plan_value).ok()?;
    if plan.phases.len() != 3 {
        return None;
    }

    for phase in &mut plan.phases {
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

fn normalize_plan_text(value: &str, original_prompt: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() || text == original_prompt.trim() {
        return None;
    }
    Some(text.to_string())
}

/// Build a Task with sensible defaults for shadow execution entries.
fn make_shadow_task(
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

pub(crate) fn cleanup_shadow_task_tree(
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
    use magi_core::SessionId;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
    use magi_session_store::SessionStore;
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    use crate::dto::SessionTurnRequestDto;

    struct StaticDeepPlanModelBridgeClient;

    impl ModelBridgeClient for StaticDeepPlanModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: deep_task_plan_response(serde_json::json!({
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
                                            "title": "验证交付",
                                            "goal": "验证执行结果并交付"
                                        }
                                    ]
                                }
                            ]
                        }
                    ]
                })),
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

    /// Build a minimal ApiState with a shadow execution pipeline and task store.
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
            "test-shadow",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_shadow_execution_pipeline(orchestrator, execution_runtime, memory_store)
        .with_task_store(Arc::clone(&task_store));

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
            text: Some("Hello world".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            deep_task: false,
            images: Vec::new(),
        };

        let result = run_shadow_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-1",
            Some("Hello world"),
        )
        .expect("shadow session action should create task graph");

        // Verify Objective task was created
        let obj_task_id = result.root_task_id;
        let obj = task_store.get_task(&obj_task_id);
        assert!(obj.is_some(), "Objective task should exist in TaskStore");
        let obj = obj.unwrap();
        assert_eq!(obj.kind, TaskKind::Objective);
        assert_eq!(obj.title, "Hello world");
        assert_eq!(obj.status, TaskStatus::Running);

        // Verify Action task was created
        let act_task_id = result.action_task_id;
        let act = task_store.get_task(&act_task_id);
        assert!(act.is_some(), "Action task should exist in TaskStore");
        let act = act.unwrap();
        assert_eq!(act.kind, TaskKind::Action);
        assert_eq!(act.title, "执行: Hello world");
        assert_eq!(act.goal, "Hello world");
        assert_eq!(act.parent_task_id, Some(obj_task_id.clone()));
        assert_eq!(act.root_task_id, obj_task_id.clone());
        assert_eq!(act.status, TaskStatus::Ready);

        let children = task_store.get_children(&obj_task_id);
        assert_eq!(children.len(), 1);
        let child_ids: Vec<&str> = children.iter().map(|t| t.task_id.as_str()).collect();
        assert!(child_ids.contains(&act_task_id.as_str()));
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
        let submission = run_shadow_dispatch_submission(
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
                deep_task: false,
                skill_name: None,
                target_role: None,
                request_id: Some("request-shadow-alias".to_string()),
                user_message_id: Some("user-shadow-alias".to_string()),
                placeholder_message_id: Some("assistant-placeholder-shadow-alias".to_string()),
            },
        )
        .expect("shadow dispatch should create task graph");

        let action = task_store
            .get_task(&submission.action_task_id)
            .expect("action task should exist");
        assert_eq!(action.goal, "模型结构化执行目标");

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
        assert_eq!(user_item.item_id, "user-shadow-alias");
        assert_eq!(user_item.kind, "user_message");
        assert_eq!(user_item.content.as_deref(), Some("用户原始任务描述"));
        assert!(user_item.thread_visible);
        assert_eq!(
            user_item.request_id.as_deref(),
            Some("request-shadow-alias")
        );
        assert_eq!(
            user_item.user_message_id.as_deref(),
            Some("user-shadow-alias")
        );
        assert_eq!(
            user_item.placeholder_message_id.as_deref(),
            Some("assistant-placeholder-shadow-alias")
        );
    }

    #[test]
    fn deep_task_requires_non_empty_execution_goal() {
        let (state, task_store) = build_test_state();
        let session_id = SessionId::new("session-deep-empty-goal");
        state
            .session_store
            .create_session(session_id.clone(), "deep empty goal")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let result = run_shadow_dispatch_submission(
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
                deep_task: true,
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
    fn deep_task_registers_validation_execution_plans() {
        let (state, task_store) = build_test_state();
        let state =
            state.with_task_planning_model_bridge_client(Arc::new(StaticDeepPlanModelBridgeClient));
        let session_id = SessionId::new("session-deep-validation-plans");
        state
            .session_store
            .create_session(session_id.clone(), "deep validation plans")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_shadow_dispatch_submission(
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
                deep_task: true,
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
                    .shadow_task_execution_registry()
                    .remove(&validation_id)
                    .is_some(),
                "validation task {validation_id} should have a registered execution plan"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 2: Task status reflects dispatch outcome (failure path)
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_registers_shadow_execution_plan() {
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
            text: Some("Run a failing action".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            deep_task: false,
            images: Vec::new(),
        };

        let submission = run_shadow_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-2",
            Some("Run a failing action"),
        )
        .expect("shadow session action should register a task execution plan");

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
                .shadow_task_execution_registry()
                .remove(&submission.action_task_id)
                .is_some(),
            "action task should have a registered execution plan",
        );
    }

    #[test]
    fn intake_append_task_registers_execution_plan_and_worker_lane() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-intake-append-registers");
        state
            .session_store
            .create_session(session_id.clone(), "intake append")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let submission = run_shadow_dispatch_submission(
            &state,
            &DispatchSubmissionRequest {
                accepted_at,
                session_id: session_id.clone(),
                workspace_id: None,
                entry_id: "entry-intake-append".to_string(),
                timeline_message: "初始任务".to_string(),
                created_session: false,
                mission_title: "初始任务".to_string(),
                task_title: "执行: 初始任务".to_string(),
                trimmed_text: Some("初始任务".to_string()),
                execution_goal: None,
                deep_task: false,
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("shadow dispatch should create active chain");
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
        let appended_task_id = TaskId::new("task-intake-appended");
        task_store.insert_task(make_shadow_task(
            appended_task_id.clone(),
            root_task.mission_id.clone(),
            submission.root_task_id.clone(),
            Some(submission.root_task_id.clone()),
            TaskKind::Action,
            "追加验证任务".to_string(),
            "验证追加任务注册执行链".to_string(),
            TaskStatus::Ready,
            UtcMillis::now(),
            Some("integration-dev"),
            None,
            root_task.policy_snapshot.clone(),
        ));
        task_store
            .append_required_child(&submission.root_task_id, &appended_task_id)
            .expect("child should be appended");

        register_appended_task_execution_branch(&state, &session_id, &appended_task_id)
            .expect("appended task should register execution branch");

        assert!(
            state
                .shadow_task_execution_registry()
                .remove(&appended_task_id)
                .is_some(),
            "追加任务必须注册 execution plan"
        );
        let sidecar = state
            .session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let chain = sidecar
            .active_execution_chain
            .expect("active chain should exist");
        assert!(
            chain.active_branch_task_ids.contains(&appended_task_id),
            "追加任务必须进入 active branch 列表"
        );
        let turn = chain.current_turn.expect("current turn should exist");
        assert!(
            turn.worker_lanes
                .iter()
                .any(|lane| lane.task_id == appended_task_id
                    && lane.role_id.as_deref() == Some("integration-dev")),
            "追加任务必须进入 worker lane，并保留产品可见角色"
        );
        assert!(
            turn.items.iter().any(|item| {
                item.kind == "worker_spawned"
                    && item.task_id.as_ref() == Some(&appended_task_id)
                    && item.worker_visible
            }),
            "追加任务必须产生 worker 可见的分支卡片事件"
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
        let submission = run_shadow_dispatch_submission(
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
                execution_goal: None,
                deep_task: false,
                skill_name: None,
                target_role: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
            },
        )
        .expect("shadow dispatch should create active chain");
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
            task_store.insert_task(make_shadow_task(
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
                .shadow_task_execution_registry()
                .remove(&submission.action_task_id)
                .is_none(),
            "重规划后旧 action plan 不能继续留在 registry"
        );
        assert!(
            state
                .shadow_task_execution_registry()
                .remove(&primary_task_id)
                .is_some(),
            "重规划主任务必须注册 execution plan"
        );
        assert!(
            state
                .shadow_task_execution_registry()
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
        .with_shadow_execution_pipeline(orchestrator, execution_runtime, memory_store);

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
            text: Some("test".to_string()),
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            deep_task: false,
            images: Vec::new(),
        };

        let result = run_shadow_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-3",
            Some("test"),
        );
        assert!(result.is_err());
    }

    fn deep_task_plan_response(arguments: serde_json::Value) -> String {
        serde_json::json!({
            "content": null,
            "finish_reason": "tool_calls",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": DEEP_TASK_PLAN_TOOL_NAME,
                    "arguments": arguments.to_string(),
                }
            }]
        })
        .to_string()
    }

    #[test]
    fn decomposition_parser_rejects_prompt_echo_lines() {
        let response = deep_task_plan_response(serde_json::json!({
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
        let response = deep_task_plan_response(plan_value.clone());

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

        let prefixed_response = format!("shadow-model::{}", plan_value);
        let prefixed_plan =
            parse_decomposition_response(prefixed_response.as_str(), "请分析并拆分这个复杂任务")
                .expect("prefixed structured plan should parse");
        assert_eq!(
            prefixed_plan.phases[1].work_packages[0].actions[0].title,
            "实现方案"
        );
    }
}
