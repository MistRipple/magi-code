use magi_bridge_client::ModelInvocationRequest;
use magi_bridge_client::SHADOW_MODEL_PROVIDER;
#[cfg(test)]
use magi_core::SessionId;
use magi_core::{
    ExecutionOwnership, ExecutorBinding, MissionId, Task, TaskExecutionTarget, TaskId, TaskKind,
    TaskPolicy, TaskStatus, UtcMillis, WorkerId,
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
    task_execution::{DispatchSubmissionRequest, ShadowTaskExecutionPlan},
};

pub(crate) struct ShadowTaskGraphSubmission {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
}

#[derive(Clone, Debug)]
struct DeepTaskGraphBuildResult {
    leaf_action_task_ids: Vec<TaskId>,
    total_task_count: usize,
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
    let execution_goal = request.execution_goal.as_deref().or(trimmed_text);

    let mission_id = MissionId::new(format!("mission-session-action-{}", accepted_at.0));
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));

    let obj_task_id = TaskId::new(format!("task-obj-{}", accepted_at.0));
    let act_task_id = TaskId::new(format!("task-act-{}", accepted_at.0));

    let now = UtcMillis::now();
    let task_goal_text = execution_goal.unwrap_or("").to_string();
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

    // 深度模式但 execution_goal 为空时回退到普通模式（避免无法分解任务图）
    let effective_deep_task =
        request.deep_task && execution_goal.map_or(false, |g| !g.trim().is_empty());

    if !effective_deep_task {
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

    let deep_graph = if effective_deep_task {
        match build_deep_task_graph(
            state,
            &mission_id,
            &obj_task_id,
            &act_task_id,
            accepted_at,
            request
                .target_role
                .as_deref()
                .or_else(|| Some(infer_dispatch_task_role(request.skill_name.as_deref()))),
            execution_goal,
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
    let mut sub_task_ids: Vec<TaskId> = Vec::new();
    if let Some(graph) = deep_graph {
        total_task_count = graph.total_task_count;
        sub_task_ids = graph.leaf_action_task_ids;
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

    for sub_task_id in &sub_task_ids {
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
                    sub_task_ids
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
    for sub_task_id in &sub_task_ids {
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
    let mut current_turn = ActiveExecutionTurn {
        turn_id: format!("turn-session-action-{}", accepted_at.0),
        turn_seq: accepted_at.0,
        accepted_at,
        status: "accepted".to_string(),
        completed_at: None,
        user_message: trimmed_text.map(str::to_string),
        items: vec![
            ActiveExecutionTurnItem {
                item_id: format!("turn-item-user-{}", accepted_at.0),
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
                thread_visible: false,
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
                thread_visible: true,
                worker_visible: false,
            },
        ],
        worker_lanes: sub_task_ids
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
    for (index, sub_task_id) in sub_task_ids.iter().enumerate() {
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
            created_session: false,
            mission_title: mission_title.clone(),
            task_title: format!("执行: {mission_title}"),
            trimmed_text: trimmed_text.map(str::to_string),
            execution_goal: None,
            deep_task: request.deep_task,
            skill_name: request.skill_name.clone(),
            target_role: None,
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
    target_role: Option<&str>,
    prompt: Option<&str>,
    now: &UtcMillis,
) -> Result<DeepTaskGraphBuildResult, ApiError> {
    let prompt_text = prompt
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| ApiError::internal_assembly("构建深度任务图失败", "prompt 为空"))?;
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
    let root_policy = Some(build_policy_for_mode(true));
    let mut total_task_count = 1usize;
    let mut leaf_action_task_ids = Vec::new();
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
                    target_role.unwrap_or_else(|| infer_dispatch_task_role(None))
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
        &plan,
        &phase_ids,
        &leaf_action_task_ids,
    )?;

    Ok(DeepTaskGraphBuildResult {
        leaf_action_task_ids,
        total_task_count,
    })
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

fn decompose_mission(
    state: &ApiState,
    prompt: Option<&str>,
    _now: &UtcMillis,
) -> Option<DeepTaskGraphPlan> {
    let prompt_text = prompt.filter(|s| !s.trim().is_empty())?;
    let client = state.model_bridge_client()?;
    let request = ModelInvocationRequest {
        provider: SHADOW_MODEL_PROVIDER.to_string(),
        prompt: format!(
            "请输出严格 JSON，描述 3 个 phase 的深度任务图。只允许返回 JSON，不要 Markdown，不要解释。\n\nJSON 结构：\n{{\n  \"phases\": [\n    {{\n      \"title\": \"规划\",\n      \"workPackages\": [\n        {{\n          \"title\": \"规划工作包\",\n          \"actions\": [\n            {{\n              \"title\": \"动作标题\",\n              \"goal\": \"动作目标\",\n              \"dependsOn\": [\"其他动作标题\"],\n              \"writeScope\": \"可选\"\n            }}\n          ]\n        }}\n      ]\n    }}\n  ]\n}}\n\n必须满足：phase 恰好 3 个，workPackages 和 actions 不能为空，actions 的 dependsOn 只能引用同一 phase 内已定义的 action 标题。任务：{}",
            prompt_text
        ),
        messages: None,
        tools: None,
        tool_choice: None,
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
    let json_text = if let Some(start) = trimmed.find('{') {
        &trimmed[start..]
    } else {
        trimmed
    };
    let mut plan: DeepTaskGraphPlan = serde_json::from_str(json_text).ok()?;
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

fn cleanup_shadow_task_tree(
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
                created_session: false,
                mission_title: "模型判定任务".to_string(),
                task_title: "执行: 模型判定任务".to_string(),
                trimmed_text: Some("用户原始任务描述".to_string()),
                execution_goal: Some("模型结构化执行目标".to_string()),
                deep_task: false,
                skill_name: None,
                target_role: None,
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
        assert!(
            turn.items
                .first()
                .is_some_and(|item| item.kind == "user_message"
                    && item.content.as_deref() == Some("用户原始任务描述")
                    && !item.thread_visible),
            "turn ordered items 必须保留用户原文，但不能进入响应区渲染"
        );
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

    #[test]
    fn decomposition_parser_rejects_prompt_echo_lines() {
        let response = "shadow-model::请将以下任务分解为 2-5 个具体的子任务。每行一个子任务标题，不要编号，不要额外说明。\n\n任务：请分析并拆分这个复杂任务";

        let plan = parse_decomposition_response(response, "请分析并拆分这个复杂任务");

        assert!(plan.is_none(), "提示词回显不能被当成结构化计划");
    }

    #[test]
    fn decomposition_parser_accepts_structured_json_plan() {
        let response = r#"{
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
}"#;

        let plan = parse_decomposition_response(response, "请分析并拆分这个复杂任务");

        let plan = plan.expect("structured plan should parse");
        assert_eq!(plan.phases.len(), 3);
        assert_eq!(plan.phases[0].work_packages[0].actions[0].title, "分析目标");
        assert_eq!(
            plan.phases[2].work_packages[0].actions[0]
                .write_scope
                .as_deref(),
            Some("src/")
        );
    }
}
