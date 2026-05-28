use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use magi_core::{MissionId, Task, TaskId, TaskKind, TaskProjection, TaskStatus};
use magi_session_store::ActiveExecutionTurn;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::session_scope::parse_session_id;
use crate::{
    errors::ApiError, session_continue::active_execution_branch_is_continue_recoverable,
    state::ApiState,
};

const DEFAULT_SESSION_TASK_HISTORY_LIMIT: usize = 12;
const MAX_SESSION_TASK_HISTORY_LIMIT: usize = 50;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/tasks/session-history", get(get_session_task_history))
        .route("/tasks/projection/{root_task_id}", get(get_task_projection))
        .route("/tasks/{task_id}", get(get_task))
        .route(
            "/tasks/{root_task_id}/delivery-package",
            get(get_delivery_package),
        )
}

fn require_task_store(
    state: &ApiState,
) -> Result<&magi_orchestrator::task_store::TaskStore, ApiError> {
    state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("任务存储未配置", "task_store is not configured")
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionScopedTaskQuery {
    session_id: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTaskHistoryResponse {
    session_id: String,
    items: Vec<SessionTaskHistoryItemDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTaskHistoryItemDto {
    root_task: Task,
    runner_status: String,
    display_status: String,
    execution_mode: String,
    active: bool,
    archived: bool,
    restartable: bool,
    updated_at: magi_core::UtcMillis,
}

fn require_session_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    task_id: &TaskId,
) -> Result<(), ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务".to_string()))?;
    let store = require_task_store(state)?;
    let task = store
        .get_task(task_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id.as_str()))?;
    if task.mission_id != mission_id {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话 {}",
            task_id, session_id
        )));
    }
    Ok(())
}

fn push_session_history_root(
    store: &magi_orchestrator::task_store::TaskStore,
    task_id: &TaskId,
    seen: &mut HashSet<TaskId>,
    roots: &mut Vec<TaskId>,
) {
    let Some(task) = store.get_task(task_id) else {
        return;
    };
    if seen.insert(task.root_task_id.clone()) {
        roots.push(task.root_task_id);
    }
}

fn session_task_history_root_ids(
    state: &ApiState,
    session_id: &magi_core::SessionId,
    store: &magi_orchestrator::task_store::TaskStore,
) -> Vec<TaskId> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    if let Some(sidecar) = state.session_store.runtime_sidecar(session_id) {
        if let Some(chain) = sidecar.active_execution_chain.as_ref() {
            push_session_history_root(store, &chain.root_task_id, &mut seen, &mut roots);
        }
        if let Some(turn) = sidecar.current_turn.as_ref() {
            for item in &turn.items {
                if let Some(task_id) = item.task_id.as_ref() {
                    push_session_history_root(store, task_id, &mut seen, &mut roots);
                }
            }
        }
    }

    for turn in state.session_store.canonical_turns_for_session(session_id) {
        for item in turn.items {
            if let Some(task_id) = item.worker.and_then(|worker| worker.task_id) {
                push_session_history_root(store, &task_id, &mut seen, &mut roots);
            }
        }
    }

    roots
}

fn active_session_root_task_id(
    state: &ApiState,
    session_id: &magi_core::SessionId,
) -> Option<TaskId> {
    state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| {
            sidecar
                .active_execution_chain
                .map(|chain| chain.root_task_id)
        })
}

fn task_status_from_turn_status(status: &str) -> TaskStatus {
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "succeeded" | "success" => TaskStatus::Completed,
        "failed" | "error" | "blocked" => TaskStatus::Failed,
        "cancelled" | "canceled" | "killed" => TaskStatus::Killed,
        "running" | "executing" | "repairing" | "verifying" => TaskStatus::Running,
        _ => TaskStatus::Pending,
    }
}

fn runner_status_from_task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "error",
        TaskStatus::Killed => "killed",
    }
}

fn synthetic_history_task_from_turn(
    session_id: &magi_core::SessionId,
    turn: &ActiveExecutionTurn,
    task_id: &TaskId,
) -> Task {
    let title = turn
        .user_message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            turn.items.iter().find_map(|item| {
                item.title
                    .as_deref()
                    .or(item.content.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| task_id.to_string());
    let status = task_status_from_turn_status(&turn.status);
    Task {
        task_id: task_id.clone(),
        mission_id: MissionId::new(format!("mission-history-{}", session_id)),
        root_task_id: task_id.clone(),
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: title.clone(),
        goal: title,
        status,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: Default::default(),
        created_at: turn.accepted_at,
        updated_at: turn.completed_at.unwrap_or(turn.accepted_at),
    }
}

fn runner_status_for_projection(
    state: &ApiState,
    root_id: &TaskId,
    projection: &mut TaskProjection,
) {
    apply_authoritative_runner_status(state, root_id, projection);
}

async fn get_session_task_history(
    State(state): State<ApiState>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<SessionTaskHistoryResponse>, ApiError> {
    let store = require_task_store(&state)?;
    let session_id = parse_session_id(query.session_id.as_deref())?;
    state
        .session_store
        .session(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let active_root_task_id = active_session_root_task_id(&state, &session_id);
    let mut items = Vec::new();
    let mut visible_roots = HashSet::new();

    for root_id in session_task_history_root_ids(&state, &session_id, store) {
        let Some(mut projection) = store.build_projection(&root_id) else {
            continue;
        };
        visible_roots.insert(root_id.clone());
        runner_status_for_projection(&state, &root_id, &mut projection);
        let active = active_root_task_id.as_ref() == Some(&root_id);
        let updated_at = projection
            .tasks
            .iter()
            .map(|task| task.updated_at)
            .max()
            .unwrap_or(projection.root_task.updated_at);
        items.push(SessionTaskHistoryItemDto {
            root_task: projection.root_task.clone(),
            runner_status: projection.runner_status,
            display_status: projection.display_status,
            execution_mode: projection.execution_mode,
            active,
            archived: !active,
            restartable: !matches!(
                projection.root_task.status,
                TaskStatus::Running | TaskStatus::Pending
            ),
            updated_at,
        });
    }

    if let Some(turn) = state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
    {
        for task_id in turn.items.iter().filter_map(|item| item.task_id.as_ref()) {
            if visible_roots.contains(task_id) || store.get_task(task_id).is_some() {
                continue;
            }
            visible_roots.insert(task_id.clone());
            let task = synthetic_history_task_from_turn(&session_id, &turn, task_id);
            let runner_status = runner_status_from_task_status(task.status).to_string();
            items.push(SessionTaskHistoryItemDto {
                root_task: task.clone(),
                runner_status,
                display_status: "已完成".to_string(),
                execution_mode: "session_turn".to_string(),
                active: false,
                archived: true,
                restartable: false,
                updated_at: task.updated_at,
            });
        }
    }

    items.sort_by(|left, right| {
        right.updated_at.0.cmp(&left.updated_at.0).then_with(|| {
            right
                .root_task
                .task_id
                .as_str()
                .cmp(left.root_task.task_id.as_str())
        })
    });
    let limit = query
        .limit
        .unwrap_or(DEFAULT_SESSION_TASK_HISTORY_LIMIT)
        .clamp(1, MAX_SESSION_TASK_HISTORY_LIMIT);
    items.truncate(limit);

    Ok(Json(SessionTaskHistoryResponse {
        session_id: session_id.to_string(),
        items,
    }))
}

async fn get_task_projection(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    require_session_task(&state, query.session_id.as_deref(), &root_id)?;
    let mut projection = store
        .build_projection(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    apply_authoritative_runner_status(&state, &root_id, &mut projection);
    apply_recoverable_chain_summary(
        &state,
        query.session_id.as_deref(),
        &root_id,
        &mut projection,
    )?;
    let value = serde_json::to_value(&projection)
        .map_err(|err| ApiError::internal_assembly("序列化任务投影失败", err))?;
    Ok(Json(value))
}

fn apply_recoverable_chain_summary(
    state: &ApiState,
    session_id_value: Option<&str>,
    root_task_id: &TaskId,
    projection: &mut TaskProjection,
) -> Result<(), ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let Some(chain) = state.session_store.active_execution_chain(&session_id) else {
        projection.has_recoverable_chain = false;
        projection.recoverable_branch_count = 0;
        return Ok(());
    };
    if &chain.root_task_id != root_task_id {
        projection.has_recoverable_chain = false;
        projection.recoverable_branch_count = 0;
        return Ok(());
    }
    let worker_runtime_handle = state
        .execution_pipeline()
        .map(|pipeline| pipeline.execution_runtime.worker_runtime());
    let count = chain
        .branches
        .iter()
        .filter(|branch| {
            active_execution_branch_is_continue_recoverable(
                worker_runtime_handle,
                state.task_store(),
                &chain,
                branch,
            )
        })
        .count();
    projection.has_recoverable_chain = count > 0;
    projection.recoverable_branch_count = count;
    Ok(())
}

fn apply_authoritative_runner_status(
    state: &ApiState,
    root_task_id: &TaskId,
    projection: &mut TaskProjection,
) {
    let Some(snapshot) = state
        .runner_manager()
        .and_then(|manager| manager.status(root_task_id.as_str()))
    else {
        return;
    };
    if let Some(status) = normalize_runner_status(&snapshot.status) {
        projection.runner_status = status.to_string();
    }
}

fn normalize_runner_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => Some("running"),
        "completed" => Some("completed"),
        "error" => Some("error"),
        "killed" => Some("killed"),
        "pending" => Some("pending"),
        "idle" => Some("idle"),
        _ => None,
    }
}

async fn get_task(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    require_session_task(&state, query.session_id.as_deref(), &id)?;
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;
    let value = serde_json::to_value(&task)
        .map_err(|err| ApiError::internal_assembly("序列化任务失败", err))?;
    Ok(Json(value))
}

async fn get_delivery_package(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    require_session_task(&state, query.session_id.as_deref(), &root_id)?;

    let package = store
        .build_delivery_package(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;

    Ok(Json(package))
}
