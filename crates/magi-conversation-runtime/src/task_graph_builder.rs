//! Task System v2 — M12：任务图构建器从 magi-api/dispatch_execution.rs 下沉到
//! conversation-runtime。
//!
//! 本模块承担"派发数据模型 + 任务图落盘 + 任务图校验 + 派发清理"等纯函数 /
//! 弱状态助手。错误统一使用 `String`，由 magi-api 上层通过
//! `.map_err(|msg| ApiError::internal_assembly("...", msg))` 桥接到 `ApiError`。
//!
//! M13 会把 `decompose_mission` / `task_plan_tool` / `parse_decomposition_*` 也迁
//! 到本 crate（或 magi-plan）；那时 `TaskGraphPlan` 系列 deserializable 结构会被
//! 共用，因此一并下沉到这里，避免 M12/M13 之间反复挪动。

use magi_agent_role::AgentRoleRegistry;
use magi_core::{
    ExecutorBinding, MissionId, Task, TaskId, TaskKind, TaskPolicy, TaskStatus, UtcMillis,
};
use magi_orchestrator::task_store::TaskStore;
use magi_orchestrator::task_worker_catalog::compatible_task_role_for_kind;
use magi_session_store::ActiveExecutionChain;
use magi_spawn_graph::SpawnGraph;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

pub const TASK_MIN_PHASES: usize = 1;
pub const TASK_MAX_PHASES: usize = 8;

pub struct TaskGraphSubmission {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
}

#[derive(Clone, Debug)]
pub struct TaskGraphBuildResult {
    pub leaf_action_task_ids: Vec<TaskId>,
    pub validation_task_ids: Vec<TaskId>,
    pub dispatch_task_ids: Vec<TaskId>,
    pub total_task_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphPlan {
    pub phases: Vec<TaskPhasePlan>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPhasePlan {
    pub title: String,
    pub work_packages: Vec<TaskWorkPackagePlan>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkPackagePlan {
    pub title: String,
    pub actions: Vec<TaskActionPlan>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskActionPlan {
    pub title: String,
    pub goal: String,
    /// LLM 在 task 图规划时显式声明的执行角色（P3 结构化契约）。
    ///
    /// 为空时回退到 `infer_dispatch_task_role(goal)` 启发式猜测。角色合法性校验由
    /// `compatible_task_role_for_kind` 统一兜底：不认识的角色会被替换为默认值，不会
    /// 拒收整个 task 图。这与派发"先保证能跑、再提高命中率"的策略一致。
    #[serde(default)]
    pub role_id: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub write_scope: Option<String>,
}

pub fn task_phase_count_is_valid(count: usize) -> bool {
    (TASK_MIN_PHASES..=TASK_MAX_PHASES).contains(&count)
}

pub fn build_task_policy() -> TaskPolicy {
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

pub fn phase_validation_goal(phase_index: usize, phase_count: usize, phase_title: &str) -> String {
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

pub fn infer_dispatch_task_role(skill_name: Option<&str>) -> &'static str {
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

pub fn make_dispatch_task(
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
        variant: magi_core::TaskVariant::default(),
        created_at: now,
        updated_at: now,
    }
}

pub fn cleanup_task_tree(task_store: &TaskStore, root_task_id: &TaskId) {
    let task_ids = task_store.collect_subtree_ids(root_task_id);
    for task_id in task_ids.into_iter().rev() {
        let _ = task_store.remove_task(&task_id);
    }
}

/// Task System v2 — L5：把刚 insert 的子任务挂到 SpawnGraph。
/// 容错策略：图层只是父子关系镜像，不参与正确性判定；遇到 EdgeAlreadyExists（重入路径）
/// 或 limits 超限（极端栈深），降级为 warn 日志，不阻塞 task 派发。
pub fn register_spawn_edge(
    spawn_graph: &Mutex<SpawnGraph>,
    parent: TaskId,
    child: TaskId,
    kind: TaskKind,
) {
    let mut graph = match spawn_graph.lock() {
        Ok(guard) => guard,
        Err(err) => {
            tracing::warn!(?err, "SpawnGraph mutex poisoned, skip register_spawn_edge");
            return;
        }
    };
    if let Err(err) = graph.add_edge(parent.clone(), child.clone(), kind, SystemTime::now()) {
        match err {
            magi_spawn_graph::SpawnGraphError::EdgeAlreadyExists { .. } => {
                // 重入路径（同一 dispatch 被重放）属于幂等场景，无需提示。
            }
            other => {
                tracing::warn!(
                    parent = %parent.as_str(),
                    child = %child.as_str(),
                    error = %other,
                    "SpawnGraph add_edge 失败，已忽略并继续派发"
                );
            }
        }
    }
}

pub fn insert_task_graph(
    task_store: &TaskStore,
    mission_id: &MissionId,
    root_task_id: &TaskId,
    primary_action_task_id: &TaskId,
    accepted_at: UtcMillis,
    target_role: &str,
    now: &UtcMillis,
    plan: &TaskGraphPlan,
    agent_role_registry: &AgentRoleRegistry,
    spawn_graph: &Mutex<SpawnGraph>,
) -> Result<TaskGraphBuildResult, String> {
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
        register_spawn_edge(
            spawn_graph,
            root_task_id.clone(),
            phase_id.clone(),
            TaskKind::Phase,
        );
        total_task_count += 1;

        let mut action_ids_by_title = HashMap::<String, TaskId>::new();
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
            register_spawn_edge(
                spawn_graph,
                phase_id.clone(),
                package_id.clone(),
                TaskKind::WorkPackage,
            );
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
                    return Err(format!(
                        "同一 phase 内的 action 标题重复: {}",
                        action_plan.title
                    ));
                }

                let action_role_candidate = if is_primary_action {
                    target_role.to_string()
                } else {
                    // P3 角色解析链：LLM 显式声明的 roleId 优先 > 基于 goal 的启发式推断。
                    // 合法性由下一步 `compatible_task_role_for_kind` 统一收敛，非法值会
                    // 退回 TaskKind::Action 的默认 role，避免因 LLM 输出不规范拒收整个 task 图。
                    action_plan
                        .role_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|role| !role.is_empty())
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| {
                            infer_dispatch_task_role(Some(action_plan.goal.as_str())).to_string()
                        })
                };
                let action_role = compatible_task_role_for_kind(
                    agent_role_registry,
                    TaskKind::Action,
                    Some(action_role_candidate.as_str()),
                )
                .ok_or_else(|| {
                    format!("无法为 action {} 解析可执行角色", action_plan.title)
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
                register_spawn_edge(
                    spawn_graph,
                    package_id.clone(),
                    action_id.clone(),
                    TaskKind::Action,
                );
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
                return Err(format!("{} 不能为空动作列表", package_plan.title));
            }

            for (action_id, dependency_titles) in current_package_dependency_specs {
                for dependency_title in dependency_titles {
                    let dependency_id =
                        action_ids_by_title.get(&dependency_title).ok_or_else(|| {
                            format!(
                                "action 依赖引用不存在或不在同一 phase 内: {}",
                                dependency_title
                            )
                        })?;
                    task_store
                        .add_dependency(&action_id, dependency_id)
                        .map_err(|err| err.to_string())?;
                }
            }
        }

        if phase_action_ids.is_empty() {
            return Err(format!("{} 至少需要一个 action", phase_plan.title));
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
        register_spawn_edge(
            spawn_graph,
            phase_id.clone(),
            validation_id.clone(),
            TaskKind::Validation,
        );
        for action_id in &phase_action_ids {
            task_store
                .add_dependency(&validation_id, action_id)
                .map_err(|err| err.to_string())?;
        }
        validation_task_ids.push(validation_id.clone());
        dispatch_task_ids.push(validation_id);
        total_task_count += 1;
    }

    for phase_index in 1..phase_ids.len() {
        task_store
            .add_dependency(&phase_ids[phase_index], &phase_ids[phase_index - 1])
            .map_err(|err| err.to_string())?;
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
                    .map_err(|err| err.to_string())?;
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

pub fn validate_task_graph(
    task_store: &TaskStore,
    root_task_id: &TaskId,
    plan: &TaskGraphPlan,
    phase_ids: &[TaskId],
    leaf_action_task_ids: &[TaskId],
) -> Result<(), String> {
    let root = task_store
        .get_task(root_task_id)
        .ok_or_else(|| "root task 不存在".to_string())?;
    if root.kind != TaskKind::Objective {
        return Err("root 必须是 Objective".to_string());
    }
    if phase_ids.len() != plan.phases.len() || !task_phase_count_is_valid(plan.phases.len()) {
        return Err(format!(
            "任务图需要 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 Phase"
        ));
    }

    for (phase_index, phase_id) in phase_ids.iter().enumerate() {
        let phase = task_store
            .get_task(phase_id)
            .ok_or_else(|| "Phase 不存在".to_string())?;
        if phase.kind != TaskKind::Phase {
            return Err("Phase 节点类型错误".to_string());
        }
        if phase.title != plan.phases[phase_index].title {
            return Err("Phase 标题与结构化计划不一致".to_string());
        }

        if phase_index > 0
            && !phase
                .dependency_ids
                .iter()
                .any(|dep| dep == &phase_ids[phase_index - 1])
        {
            return Err("Phase 之间必须形成按计划批次推进的依赖链".to_string());
        }

        let packages: Vec<Task> = task_store
            .get_children(phase_id)
            .into_iter()
            .filter(|task| task.kind == TaskKind::WorkPackage)
            .collect();
        if packages.len() != plan.phases[phase_index].work_packages.len() || packages.is_empty() {
            return Err("Phase 的工作包数量与计划不一致".to_string());
        }

        let mut phase_action_ids = Vec::new();
        for (package_index, package) in packages.iter().enumerate() {
            let package_plan = &plan.phases[phase_index].work_packages[package_index];
            if package.title != package_plan.title {
                return Err("WorkPackage 标题与结构化计划不一致".to_string());
            }
            let children = task_store.get_children(&package.task_id);
            let actions: Vec<Task> = children
                .iter()
                .filter(|task| task.kind == TaskKind::Action)
                .cloned()
                .collect();
            if actions.len() != package_plan.actions.len() || actions.is_empty() {
                return Err("WorkPackage 的 Action 数量与计划不一致".to_string());
            }
            phase_action_ids.extend(actions.iter().map(|action| action.task_id.clone()));
        }
        let validations: Vec<Task> = task_store
            .get_children(phase_id)
            .into_iter()
            .filter(|task| task.kind == TaskKind::Validation)
            .collect();
        if validations.len() != 1 {
            return Err("每个 Phase 必须包含 1 个 Validation".to_string());
        }
        let validation = &validations[0];
        if !validation.dependency_ids.iter().all(|dependency_id| {
            phase_action_ids
                .iter()
                .any(|action_id| action_id == dependency_id)
        }) {
            return Err("Validation 必须依赖当前 Phase 内的 Action".to_string());
        }
    }

    for action_id in leaf_action_task_ids {
        let action = task_store
            .get_task(action_id)
            .ok_or_else(|| "Action 不存在".to_string())?;
        if action.kind != TaskKind::Action {
            return Err("叶子节点必须是 Action".to_string());
        }
        if action.parent_task_id.is_none() {
            return Err("Action 必须有父节点".to_string());
        }
    }

    Ok(())
}
