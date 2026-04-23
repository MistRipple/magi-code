use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use magi_core::{MissionId, SessionId, TaskId, TaskKind, TaskStatus, UtcMillis};
use serde::Deserialize;

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/tasks/graph/{root_task_id}", get(get_task_projection))
        .route("/tasks/{task_id}", get(get_task))
        .route("/tasks/create", post(create_task))
        .route("/tasks/{task_id}/status", post(update_task_status))
        .route("/tasks/{task_id}/lease", get(get_task_lease))
        .route("/tasks/{task_id}/pause", post(pause_task))
        .route("/tasks/{task_id}/resume", post(resume_task_control))
        .route("/tasks/{task_id}/cancel", post(cancel_tree))
        .route("/tasks/{task_id}/replan", post(replan_tree))
        .route("/tasks/{task_id}/decision", post(resolve_decision))
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
}

fn parse_session_id(value: Option<&str>) -> Result<SessionId, ApiError> {
    let session_id = value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    Ok(SessionId::new(session_id))
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
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务链".to_string()))?;
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

async fn get_task_projection(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    require_session_task(&state, query.session_id.as_deref(), &root_id)?;
    let projection = store
        .build_projection(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    let value = serde_json::to_value(&projection)
        .map_err(|err| ApiError::internal_assembly("序列化任务投影失败", err))?;
    Ok(Json(value))
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

#[derive(Debug, Deserialize)]
struct CreateTaskRequest {
    task_id: String,
    mission_id: String,
    root_task_id: String,
    parent_task_id: Option<String>,
    kind: TaskKind,
    title: String,
    goal: String,
    #[serde(default = "default_draft_status")]
    status: TaskStatus,
    #[serde(default)]
    dependency_ids: Vec<String>,
    #[serde(default)]
    context_refs: Vec<String>,
    #[serde(default)]
    knowledge_refs: Vec<String>,
    workspace_scope: Option<String>,
    write_scope: Option<String>,
}

fn default_draft_status() -> TaskStatus {
    TaskStatus::Draft
}

async fn create_task(
    State(state): State<ApiState>,
    Json(request): Json<CreateTaskRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let now = UtcMillis::now();
    let task = magi_core::Task {
        task_id: TaskId::new(&request.task_id),
        mission_id: MissionId::new(&request.mission_id),
        root_task_id: TaskId::new(&request.root_task_id),
        parent_task_id: request.parent_task_id.as_deref().map(TaskId::new),
        kind: request.kind,
        title: request.title,
        goal: request.goal,
        status: request.status,
        dependency_ids: request
            .dependency_ids
            .iter()
            .map(|s| TaskId::new(s))
            .collect(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: None,
        context_refs: request.context_refs,
        knowledge_refs: request.knowledge_refs,
        workspace_scope: request.workspace_scope,
        write_scope: request.write_scope,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        created_at: now,
        updated_at: now,
    };
    store.insert_task(task.clone());
    let value = serde_json::to_value(&task)
        .map_err(|err| ApiError::internal_assembly("序列化任务失败", err))?;
    Ok(Json(value))
}

#[derive(Debug, Deserialize)]
struct UpdateStatusRequest {
    status: TaskStatus,
}

async fn update_task_status(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Json(request): Json<UpdateStatusRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    store
        .update_status(&id, request.status)
        .map_err(|_err| ApiError::not_found("任务不存在", &task_id))?;
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;
    let value = serde_json::to_value(&task)
        .map_err(|err| ApiError::internal_assembly("序列化任务失败", err))?;
    Ok(Json(value))
}

async fn get_task_lease(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    require_session_task(&state, query.session_id.as_deref(), &id)?;
    let value = serde_json::to_value(store.get_active_lease(&id))
        .map_err(|err| ApiError::internal_assembly("序列化租约失败", err))?;
    Ok(Json(value))
}

// ---------------------------------------------------------------------------
// Control commands (pause / resume / cancel / replan / decision)
// ---------------------------------------------------------------------------

async fn pause_task(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;
    if task.status != TaskStatus::Running {
        return Err(ApiError::InvalidInput(format!(
            "cannot pause task in {:?} state",
            task.status
        )));
    }
    store
        .update_status(&id, TaskStatus::Blocked)
        .map_err(|e| ApiError::internal_assembly("暂停任务失败", e))?;
    let children = store.get_children(&id);
    let mut paused_children = 0u32;
    for child in &children {
        if child.status == TaskStatus::Running {
            let _ = store.update_status(&child.task_id, TaskStatus::Blocked);
            paused_children += 1;
        }
    }
    Ok(Json(serde_json::json!({
        "taskId": task_id,
        "paused": true,
        "pausedChildren": paused_children,
    })))
}

async fn resume_task_control(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;
    if task.status != TaskStatus::Blocked {
        return Err(ApiError::InvalidInput(format!(
            "cannot resume task in {:?} state",
            task.status
        )));
    }
    store
        .update_status(&id, TaskStatus::Running)
        .map_err(|e| ApiError::internal_assembly("恢复任务失败", e))?;
    let children = store.get_children(&id);
    let mut resumed_children = 0u32;
    for child in &children {
        if child.status == TaskStatus::Blocked {
            let _ = store.update_status(&child.task_id, TaskStatus::Ready);
            resumed_children += 1;
        }
    }
    Ok(Json(serde_json::json!({
        "taskId": task_id,
        "resumed": true,
        "resumedChildren": resumed_children,
    })))
}

async fn cancel_tree(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&task_id);
    if store.get_task(&root_id).is_none() {
        return Err(ApiError::not_found("任务不存在", &task_id));
    }
    let all_ids = store.collect_subtree_ids(&root_id);
    let mut cancelled = 0u32;
    for id in &all_ids {
        if let Some(task) = store.get_task(id) {
            if !matches!(
                task.status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Skipped
            ) {
                let _ = store.update_status(id, TaskStatus::Cancelled);
                if let Some(lease) = store.get_active_lease(id) {
                    store.revoke_lease(id, &lease.lease_id);
                }
                cancelled += 1;
            }
        }
    }
    Ok(Json(serde_json::json!({
        "taskId": task_id,
        "cancelled": cancelled,
        "totalTasks": all_ids.len(),
    })))
}

async fn replan_tree(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&task_id);
    if store.get_task(&root_id).is_none() {
        return Err(ApiError::not_found("任务不存在", &task_id));
    }
    let all_ids = store.collect_subtree_ids(&root_id);
    let mut cancelled_ids: Vec<String> = Vec::new();
    for id in &all_ids {
        if *id == root_id {
            continue;
        }
        if let Some(task) = store.get_task(id) {
            if !matches!(
                task.status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Skipped
            ) {
                let _ = store.update_status(id, TaskStatus::Cancelled);
                if let Some(lease) = store.get_active_lease(id) {
                    store.revoke_lease(id, &lease.lease_id);
                }
                cancelled_ids.push(id.to_string());
            }
        }
    }
    let _ = store.update_status(&root_id, TaskStatus::Running);
    Ok(Json(serde_json::json!({
        "taskId": task_id,
        "replanned": true,
        "cancelledIds": cancelled_ids,
    })))
}

#[derive(Debug, Deserialize)]
struct ResolveDecisionRequest {
    chosen_option: String,
    evidence: Option<serde_json::Value>,
}

async fn resolve_decision(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Json(request): Json<ResolveDecisionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("决策任务不存在", &task_id))?;
    if task.kind != TaskKind::Decision {
        return Err(ApiError::InvalidInput(format!(
            "{} 不是 Decision 类型任务",
            task_id
        )));
    }
    if task.status != TaskStatus::AwaitingApproval {
        return Err(ApiError::InvalidInput(format!(
            "决策任务 {} 不在 AwaitingApproval 状态",
            task_id
        )));
    }
    store.set_output_refs(
        &id,
        vec![format!("decision_chosen:{}", request.chosen_option)],
    );
    if let Some(ref ev) = request.evidence {
        store.set_evidence_refs(&id, vec![serde_json::to_string(ev).unwrap_or_default()]);
    }
    store
        .update_status(&id, TaskStatus::Completed)
        .map_err(|e| ApiError::internal_assembly("完成决策失败", e))?;
    // Unblock parent if no other blockers remain.
    if let Some(ref parent_id) = task.parent_task_id {
        let siblings = store.get_children(parent_id);
        let still_blocked = siblings.iter().any(|s| {
            s.task_id != id
                && (s.status == TaskStatus::Blocked || s.status == TaskStatus::AwaitingApproval)
        });
        if !still_blocked {
            if let Some(parent) = store.get_task(parent_id) {
                if parent.status == TaskStatus::Blocked {
                    let _ = store.update_status(parent_id, TaskStatus::Running);
                }
            }
        }
    }
    Ok(Json(serde_json::json!({
        "taskId": task_id,
        "resolved": true,
        "chosenOption": request.chosen_option,
    })))
}
