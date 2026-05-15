//! Task System v2 — M17a：派发提交载体（DispatchSubmissionRequest /
//! DispatchSubmissionAccepted）从 magi-api/task_execution.rs 下沉到
//! conversation-runtime。
//!
//! 这两个 DTO 与 ApiState / ApiError 无任何运行期耦合，是 v2 dispatch 流程的
//! "请求 → 接受" 一次性数据载体。magi-api 通过 `pub use` 重导出维持外部
//! import 路径不变；M17b 继续把"接受派发提交"这段只依赖 SessionStore / TaskStore /
//! TaskExecutionRegistry 的流程下沉到这里；magi-api 仅保留 run_dispatch_submission 与
//! runner 启动桥接。

use std::path::Path;
use std::sync::{Arc, Mutex};

use magi_agent_role::AgentRoleRegistry;
use magi_bridge_client::ModelBridgeClient;
use magi_core::{
    DomainError, ExecutionOwnership, MissionId, SessionId, TaskExecutionTarget, TaskId, TaskKind,
    TaskStatus, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventContext, InMemoryEventBus, task_events};
use magi_orchestrator::{
    DispatchMemoryExtractionInput, ExecutionWritebackPlans, task_store::TaskStore,
};
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, ActiveExecutionTurnLane, SessionStore,
    TimelineEntryKind,
};
use magi_spawn_graph::SpawnGraph;

use crate::mission_decomposition::decompose_mission;
use crate::session_thread::ensure_thread_for_role;
use crate::task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry};
use crate::task_graph_builder::{
    TASK_MAX_PHASES, TASK_MIN_PHASES, TaskGraphBuildResult, TaskGraphSubmission, build_task_policy,
    cleanup_task_tree, infer_dispatch_task_role, insert_task_graph, make_dispatch_task,
    task_phase_count_is_valid,
};

#[derive(Clone, Debug)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub entry_id: String,
    pub timeline_message: String,
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub skill_name: Option<String>,
    pub target_role: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionAccepted {
    pub session_id: SessionId,
    pub entry_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub runner_started: bool,
}

pub struct DispatchSubmissionRuntime<'a> {
    pub session_store: &'a SessionStore,
    pub task_store: &'a TaskStore,
    pub execution_registry: &'a TaskExecutionRegistry,
    pub event_bus: &'a InMemoryEventBus,
    pub agent_role_registry: &'a AgentRoleRegistry,
    pub spawn_graph: &'a Mutex<SpawnGraph>,
    pub model_bridge_client: Option<&'a Arc<dyn ModelBridgeClient>>,
    pub workspace_root_path: Option<&'a Path>,
}

#[derive(Debug)]
pub enum DispatchSubmissionRunError {
    InvalidInput(String),
    Internal(String),
}

impl DispatchSubmissionRunError {
    pub fn into_message(self) -> String {
        match self {
            Self::InvalidInput(message) | Self::Internal(message) => message,
        }
    }
}

#[derive(Debug)]
pub enum DispatchSubmissionAcceptError {
    Conflict { message: String },
    Internal { message: String },
}

impl DispatchSubmissionAcceptError {
    pub fn from_store_error(error: DomainError) -> Self {
        match error {
            DomainError::InvalidState { message } if message.contains("active current_turn") => {
                Self::Conflict { message }
            }
            other => Self::Internal {
                message: other.to_string(),
            },
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Conflict { message } | Self::Internal { message } => message,
        }
    }
}

pub fn ensure_dispatch_submission_acceptance_available(
    session_store: &SessionStore,
    request: &DispatchSubmissionRequest,
) -> Result<(), DispatchSubmissionAcceptError> {
    session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(DispatchSubmissionAcceptError::from_store_error)
}

pub fn cleanup_rejected_dispatch(
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    graph: &TaskGraphSubmission,
) {
    if let Some(chain) = graph.active_execution_chain.as_ref() {
        for branch in &chain.branches {
            let _ = execution_registry.remove(&branch.task_id);
        }
    }
    if let Some(task_store) = task_store {
        cleanup_task_tree(task_store, &graph.root_task_id);
    }
}

fn build_task_graph(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
    mission_id: &MissionId,
    obj_task_id: &TaskId,
    act_task_id: &TaskId,
    accepted_at: UtcMillis,
    execution_goal: &str,
    now: &UtcMillis,
) -> Result<TaskGraphBuildResult, DispatchSubmissionRunError> {
    let prompt_text = execution_goal.trim();
    if prompt_text.is_empty() {
        return Err(DispatchSubmissionRunError::InvalidInput(
            "任务派发必须提供非空 execution_goal".to_string(),
        ));
    }
    let workspace_root_path = runtime.workspace_root_path;
    let plan = decompose_mission(
        runtime.model_bridge_client,
        workspace_root_path,
        Some(prompt_text),
    )
    .ok_or_else(|| DispatchSubmissionRunError::Internal("无法生成结构化任务计划".to_string()))?;
    if !task_phase_count_is_valid(plan.phases.len()) {
        return Err(DispatchSubmissionRunError::Internal(format!(
            "任务图需要 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 phase"
        )));
    }

    insert_task_graph(
        runtime.task_store,
        mission_id,
        obj_task_id,
        act_task_id,
        accepted_at,
        request
            .target_role
            .as_deref()
            .unwrap_or_else(|| infer_dispatch_task_role(request.skill_name.as_deref())),
        now,
        &plan,
        runtime.agent_role_registry,
        runtime.spawn_graph,
    )
    .map_err(DispatchSubmissionRunError::Internal)
}

pub fn run_dispatch_submission(
    runtime: &DispatchSubmissionRuntime<'_>,
    request: &DispatchSubmissionRequest,
) -> Result<TaskGraphSubmission, DispatchSubmissionRunError> {
    let _ = runtime
        .session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(DispatchSubmissionAcceptError::from_store_error)
        .map_err(|err| match err {
            DispatchSubmissionAcceptError::Conflict { message }
            | DispatchSubmissionAcceptError::Internal { message } => {
                DispatchSubmissionRunError::Internal(message)
            }
        })?;

    let accepted_at = request.accepted_at;
    let session_id = &request.session_id;
    let entry_id = request.entry_id.as_str();
    let trimmed_text = request.trimmed_text.as_deref();
    let execution_goal = request
        .execution_goal
        .as_deref()
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .ok_or_else(|| {
            DispatchSubmissionRunError::InvalidInput(
                "任务派发必须提供非空 execution_goal".to_string(),
            )
        })?;

    let now = UtcMillis::now();
    let (mission_id, orchestrator_thread_id) =
        runtime
            .session_store
            .ensure_session_mission(session_id, now, || {
                MissionId::new(format!("mission-session-action-{}", accepted_at.0))
            });
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
    runtime.task_store.insert_task(objective);

    let task_graph = match build_task_graph(
        runtime,
        request,
        &mission_id,
        &obj_task_id,
        &act_task_id,
        accepted_at,
        execution_goal,
        &now,
    ) {
        Ok(graph) => graph,
        Err(err) => {
            cleanup_task_tree(runtime.task_store, &obj_task_id);
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
    let _ = runtime.event_bus.publish(event);

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
    runtime.execution_registry.insert(
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
            thread_id: orchestrator_thread_id.clone(),
            is_primary: true,
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            ownership: ownership.clone(),
            writebacks: ExecutionWritebackPlans::from_session_action_input(
                DispatchMemoryExtractionInput {
                    accepted_at,
                    session_id,
                    timeline_entry_id: entry_id,
                    text: trimmed_text,
                    skill_name: request.skill_name.as_deref(),
                },
            ),
            use_tools: true,
            skill_name: request.skill_name.clone(),
        },
    );

    let role_for_task = |task_id: &TaskId| -> String {
        runtime
            .task_store
            .get_task(task_id)
            .and_then(|task| task.executor_binding_target_role().map(str::to_string))
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
        let sub_task_role_id = role_for_task(sub_task_id);
        let sub_thread_id = ensure_thread_for_role(
            runtime.session_store,
            &request.session_id,
            &mission_id,
            &sub_task_role_id,
            &worker_id,
            sub_task_id,
            now,
        );
        runtime.execution_registry.insert(
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
    let dispatch_lane_task_ids = dispatch_task_ids.clone();
    let mut current_turn = ActiveExecutionTurn {
        turn_id: format!("turn-session-action-{}", accepted_at.0),
        turn_seq: accepted_at.0,
        accepted_at,
        status: "accepted".to_string(),
        completed_at: None,
        user_message: trimmed_text.map(str::to_string),
        items: vec![ActiveExecutionTurnItem {
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
            source_thread_id: orchestrator_thread_id.clone(),
        }],
        worker_lanes: dispatch_lane_task_ids
            .iter()
            .enumerate()
            .map(|(index, sub_task_id)| {
                let role_id = role_for_task(sub_task_id);
                let thread_id = ensure_thread_for_role(
                    runtime.session_store,
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
                    title: runtime
                        .task_store
                        .get_task(sub_task_id)
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

#[derive(Clone, Debug)]
pub struct ReplannedTaskExecutionBranchSpec {
    pub task_id: TaskId,
    pub is_primary: bool,
}

pub fn replace_replanned_task_execution_branches(
    session_store: &SessionStore,
    task_store: &TaskStore,
    execution_registry: &TaskExecutionRegistry,
    session_id: &SessionId,
    primary_action_task_id: &TaskId,
    leaf_action_task_ids: &[TaskId],
) -> Result<(), DispatchSubmissionRunError> {
    let mut branch_specs = Vec::with_capacity(leaf_action_task_ids.len() + 1);
    branch_specs.push(ReplannedTaskExecutionBranchSpec {
        task_id: primary_action_task_id.clone(),
        is_primary: true,
    });
    branch_specs.extend(leaf_action_task_ids.iter().cloned().map(|task_id| {
        ReplannedTaskExecutionBranchSpec {
            task_id,
            is_primary: false,
        }
    }));
    update_session_execution_branches(
        session_store,
        task_store,
        execution_registry,
        session_id,
        &branch_specs,
    )
}

fn update_session_execution_branches(
    session_store: &SessionStore,
    task_store: &TaskStore,
    execution_registry: &TaskExecutionRegistry,
    session_id: &SessionId,
    branch_specs: &[ReplannedTaskExecutionBranchSpec],
) -> Result<(), DispatchSubmissionRunError> {
    if branch_specs.is_empty() {
        return Ok(());
    }
    let mut active_chain = session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
        .ok_or_else(|| {
            DispatchSubmissionRunError::InvalidInput(
                "当前会话没有可注册任务的活跃执行链".to_string(),
            )
        })?;
    if active_chain.session_id != *session_id {
        return Err(DispatchSubmissionRunError::InvalidInput(
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
        let _ = execution_registry.remove(task_id);
    }

    let mut appended_lane_count = 0usize;
    let mut new_branches = Vec::with_capacity(branch_specs.len());
    let mut new_lanes = Vec::new();

    for spec in branch_specs {
        let task = task_store.get_task(&spec.task_id).ok_or_else(|| {
            DispatchSubmissionRunError::InvalidInput(format!("待注册任务不存在: {}", spec.task_id))
        })?;
        if task.mission_id != mission_id || task.root_task_id != root_task_id {
            return Err(DispatchSubmissionRunError::InvalidInput(format!(
                "任务 {} 不属于当前执行链",
                spec.task_id
            )));
        }
        let role_id = task
            .executor_binding_target_role()
            .map(str::to_string)
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
        let thread_id_for_plan = ensure_thread_for_role(
            session_store,
            session_id,
            &mission_id,
            role_id.as_str(),
            &worker_id,
            &spec.task_id,
            now,
        );
        execution_registry.insert(
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
                source_thread_id: lane.thread_id.clone(),
            });
            next_item_seq = next_item_seq.saturating_add(1);
        }
        turn.normalize();
    }
    active_chain.normalize();
    session_store
        .upsert_active_execution_chain(session_id.clone(), active_chain)
        .map_err(|error| DispatchSubmissionRunError::Internal(error.to_string()))?;
    Ok(())
}

pub fn accept_dispatch_submission(
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    request: DispatchSubmissionRequest,
    graph: TaskGraphSubmission,
) -> Result<DispatchSubmissionAccepted, DispatchSubmissionAcceptError> {
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = session_store.accept_active_execution_chain_with_timeline_entry(
            request.session_id.clone(),
            request.entry_id.clone(),
            TimelineEntryKind::UserMessage,
            request.timeline_message.clone(),
            request.accepted_at,
            active_execution_chain,
        );
        if let Err(error) = accept_result {
            cleanup_rejected_dispatch(task_store, execution_registry, &graph);
            return Err(DispatchSubmissionAcceptError::from_store_error(error));
        }
    }

    Ok(DispatchSubmissionAccepted {
        session_id: request.session_id,
        entry_id: request.entry_id,
        accepted_at: request.accepted_at,
        created_session: request.created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
        runner_started: false,
    })
}
