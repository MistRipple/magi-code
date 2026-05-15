//! Task System v2 — M14：任务图重规划入口从 magi-api/dispatch_execution.rs 下沉
//! 到 conversation-runtime。
//!
//! `replan_task_graph` 与 `ensure_session_active_execution_chain` 都不再持有
//! `&ApiState`，改为接收显式 stores / registries / event bus / model bridge client
//! / workspace_root_path。错误统一为 [`TaskGraphReplanError`]，magi-api 调用点
//! 在 `routes/tasks_graph.rs` 上做 `.map_err(...)` 桥接到 `ApiError`。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use magi_agent_role::AgentRoleRegistry;
use magi_bridge_client::ModelBridgeClient;
use magi_core::{SessionId, Task, TaskId, TaskKind, TaskStatus, UtcMillis};
use magi_event_bus::{EventContext, InMemoryEventBus, task_events};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::SessionStore;
use magi_spawn_graph::SpawnGraph;

use crate::mission_decomposition::decompose_mission;
use crate::task_graph_builder::{
    TASK_MAX_PHASES, TASK_MIN_PHASES, infer_dispatch_task_role, insert_task_graph,
    task_phase_count_is_valid,
};

static REPLAN_GRAPH_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct TaskGraphReplanResult {
    pub cancelled_task_ids: Vec<TaskId>,
    pub primary_action_task_id: TaskId,
    pub leaf_action_task_ids: Vec<TaskId>,
    pub validation_task_ids: Vec<TaskId>,
    pub dispatch_task_ids: Vec<TaskId>,
}

#[derive(Debug)]
pub enum TaskGraphReplanError {
    InvalidInput(String),
    Internal(String),
}

impl TaskGraphReplanError {
    pub fn into_message(self) -> String {
        match self {
            Self::InvalidInput(message) | Self::Internal(message) => message,
        }
    }
}

pub fn ensure_session_active_execution_chain(
    session_store: &SessionStore,
    session_id: &SessionId,
) -> Result<(), TaskGraphReplanError> {
    let has_active_chain = session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
        .is_some();
    if has_active_chain {
        return Ok(());
    }
    Err(TaskGraphReplanError::InvalidInput(
        "当前会话没有可注册任务的活跃执行链".to_string(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn replan_task_graph(
    task_store: &TaskStore,
    agent_role_registry: &AgentRoleRegistry,
    spawn_graph: &Mutex<SpawnGraph>,
    event_bus: &InMemoryEventBus,
    model_bridge_client: Option<&Arc<dyn ModelBridgeClient>>,
    workspace_root_path: Option<&Path>,
    root_task_id: &TaskId,
    prompt: &str,
    context_task: Option<&Task>,
    reason: &str,
) -> Result<TaskGraphReplanResult, TaskGraphReplanError> {
    let root_task = task_store
        .get_task(root_task_id)
        .ok_or_else(|| TaskGraphReplanError::Internal("root task 不存在".to_string()))?;
    if root_task.kind != TaskKind::Objective {
        return Err(TaskGraphReplanError::Internal(
            "root 必须是 Objective".to_string(),
        ));
    }
    if !root_task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.background_allowed)
    {
        return Err(TaskGraphReplanError::InvalidInput(
            "当前任务不存在任务图，不能重规划".to_string(),
        ));
    }

    let prompt_text = prompt.trim();
    if prompt_text.is_empty() {
        return Err(TaskGraphReplanError::Internal("prompt 为空".to_string()));
    }

    let build_seed_base = UtcMillis::now();
    let build_seed = UtcMillis(
        build_seed_base
            .0
            .saturating_add(REPLAN_GRAPH_COUNTER.fetch_add(1, Ordering::Relaxed)),
    );
    let plan = decompose_mission(model_bridge_client, workspace_root_path, Some(prompt_text))
        .ok_or_else(|| {
            TaskGraphReplanError::Internal("无法生成结构化任务计划".to_string())
        })?;
    if !task_phase_count_is_valid(plan.phases.len()) {
        return Err(TaskGraphReplanError::Internal(format!(
            "任务图需要 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 phase"
        )));
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
        .and_then(|task| task.executor_binding_target_role().map(str::to_string))
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
        agent_role_registry,
        spawn_graph,
    )
    .map_err(TaskGraphReplanError::Internal)?;

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
                .map_err(|err| TaskGraphReplanError::Internal(err.to_string()))?;
            if let Some(lease) = task_store.get_active_lease(task_id) {
                task_store.revoke_lease(task_id, &lease.lease_id);
            }
            cancelled_task_ids.push(task_id.clone());
        }
    }

    if root_task.status != TaskStatus::Running {
        task_store
            .update_status(root_task_id, TaskStatus::Running)
            .map_err(|err| TaskGraphReplanError::Internal(err.to_string()))?;
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
    let _ = event_bus.publish(event);

    Ok(TaskGraphReplanResult {
        cancelled_task_ids,
        primary_action_task_id,
        leaf_action_task_ids: build.leaf_action_task_ids,
        validation_task_ids: build.validation_task_ids,
        dispatch_task_ids: build.dispatch_task_ids,
    })
}
