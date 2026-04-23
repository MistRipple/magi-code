use axum::{Json, Router, extract::State, routing::post};
use magi_core::{EventId, SessionId, TaskId, TaskStatus, UtcMillis};
use magi_event_bus::EventEnvelope;
use serde::Deserialize;
use serde_json::json;

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/task/append", post(append_task))
        .route("/task/start", post(start_task))
        .route("/task/delete", post(delete_task))
        .route("/task/interrupt", post(interrupt_task))
        .route("/task/clear-all", post(clear_all_tasks))
        .route("/task/queued/update", post(update_queued_task))
        .route("/task/queued/delete", post(delete_queued_task))
        .route("/interaction/confirm-recovery", post(confirm_recovery))
        .route("/interaction/response", post(interaction_response))
        .route(
            "/interaction/clarification",
            post(interaction_clarification),
        )
        .route("/interaction/worker-question", post(worker_question))
        .route("/chain/abandon", post(abandon_chain))
}

fn parse_session_id(value: Option<&str>) -> Result<SessionId, ApiError> {
    let session_id = value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    Ok(SessionId::new(session_id))
}

fn require_session_owned_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    task_id: &str,
) -> Result<(SessionId, magi_core::Task), ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务链".to_string()))?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("session task guard", "task_store 未配置"))?;
    let tid = TaskId::new(task_id);
    let task = store
        .get_task(&tid)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id))?;
    if task.mission_id != mission_id {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话 {}",
            task_id, session_id
        )));
    }
    Ok((session_id, task))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendTaskRequest {
    task_id: String,
    content: String,
    session_id: Option<String>,
}

async fn append_task(
    State(state): State<ApiState>,
    Json(request): Json<AppendTaskRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-task-append-{}", now.0));
    let (_session_id, task) =
        require_session_owned_task(&state, request.session_id.as_deref(), &request.task_id)?;
    let tid = task.task_id.clone();

    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("append task", "task_store 未配置"))?;
    let input_ref = format!("text:{}", request.content);
    store
        .append_input_ref(&tid, input_ref)
        .map_err(|error| ApiError::internal_assembly("追加任务内容失败", error))?;

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.append.submitted",
        json!({
            "taskId": request.task_id,
            "content": request.content,
            "appendedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("追加任务事件发布失败", err))?;

    Ok(Json(json!({
        "taskId": request.task_id,
        "appended": true,
        "eventId": event_id.to_string(),
        "appendedAt": now.0,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdRequest {
    task_id: String,
    session_id: Option<String>,
}

async fn start_task(
    State(state): State<ApiState>,
    Json(request): Json<TaskIdRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-task-start-{}", now.0));
    let task_id = request.task_id;
    let (_session_id, task) =
        require_session_owned_task(&state, request.session_id.as_deref(), &task_id)?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("start task", "task_store 未配置"))?;
    let tid = task.task_id.clone();
    if task.status != TaskStatus::Draft {
        return Err(ApiError::InvalidInput("当前任务状态不支持启动".to_string()));
    }
    store
        .update_status(&tid, TaskStatus::Ready)
        .map_err(|error| ApiError::internal_assembly("启动任务状态更新失败", error))?;

    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("start task", "runner_manager 未配置"))?;
    manager.start(&task_id).map_err(|error| {
        ApiError::internal_assembly("启动任务 Runner 失败", format!("{error:?}"))
    })?;

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.start.requested",
        json!({
            "taskId": task_id.clone(),
            "requestedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("启动任务事件发布失败", err))?;

    Ok(Json(json!({
        "taskId": task_id,
        "started": true,
        "storeUpdated": true,
        "runnerStarted": true,
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}

async fn delete_task(
    State(state): State<ApiState>,
    Json(request): Json<TaskIdRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-task-delete-{}", now.0));
    let task_id = request.task_id;

    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("delete task", "task_store 未配置"))?;
    let (_session_id, task) =
        require_session_owned_task(&state, request.session_id.as_deref(), &task_id)?;
    let tid = task.task_id.clone();
    store
        .update_status(&tid, TaskStatus::Cancelled)
        .map_err(|error| ApiError::internal_assembly("删除任务状态更新失败", error))?;
    store
        .remove_task(&tid)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.delete.requested",
        json!({
            "taskId": task_id.clone(),
            "requestedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("删除任务事件发布失败", err))?;

    Ok(Json(json!({
        "taskId": task_id,
        "deleted": true,
        "storeUpdated": true,
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}

/// 中断任务：暂停原执行树，而不是取消当前 task。
async fn interrupt_task(
    State(state): State<ApiState>,
    Json(request): Json<TaskIdRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-task-interrupt-{}", now.0));

    let task_id = request.task_id.trim();
    if task_id.is_empty() {
        return Err(ApiError::InvalidInput("taskId 不能为空".to_string()));
    }
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("interrupt task", "task_store 未配置"))?;
    let (session_id, task) =
        require_session_owned_task(&state, request.session_id.as_deref(), task_id)?;
    let root_task_id = task.root_task_id.clone();
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("interrupt task", "runner_manager 未配置"))?;
    manager
        .pause_tree(root_task_id.as_str())
        .map_err(|error| ApiError::internal_assembly("中断任务状态更新失败", error))?;
    for subtree_task_id in store.collect_subtree_ids(&root_task_id) {
        if let Some(lease) = store.get_active_lease(&subtree_task_id) {
            store.revoke_lease(&subtree_task_id, &lease.lease_id);
        }
    }
    let _ = state
        .session_store
        .update_current_turn_status(&session_id, "blocked");

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.interrupt.requested",
        json!({
            "taskId": task_id,
            "rootTaskId": root_task_id.to_string(),
            "requestedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("中断事件发布失败", err))?;

    Ok(Json(json!({
        "interrupted": true,
        "storeUpdated": true,
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearAllTasksRequest {
    session_id: Option<String>,
}

async fn clear_all_tasks(
    State(state): State<ApiState>,
    Json(request): Json<ClearAllTasksRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-task-clear-all-{}", now.0));
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有可清理的任务链".to_string()))?;

    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("clear all tasks", "task_store 未配置"))?;
    let tasks = store.get_tasks_by_mission(&mission_id);
    for task in tasks {
        let _ = store.update_status(&task.task_id, TaskStatus::Cancelled);
        let _ = store.remove_task(&task.task_id);
    }

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.clear-all.requested",
        json!({
            "sessionId": session_id.to_string(),
            "requestedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("清除所有任务事件发布失败", err))?;

    Ok(Json(json!({
        "cleared": true,
        "storeCleared": true,
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueuedTaskRequest {
    queue_id: String,
    content: Option<String>,
}

async fn update_queued_task(
    State(state): State<ApiState>,
    Json(request): Json<QueuedTaskRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-queued-update-{}", now.0));
    let tid = TaskId::new(&request.queue_id);

    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("update queued task", "task_store 未配置"))?;
    let task = store
        .get_task(&tid)
        .ok_or_else(|| ApiError::not_found("队列任务不存在", &request.queue_id))?;
    if task.status != TaskStatus::Draft && task.status != TaskStatus::Ready {
        return Err(ApiError::InvalidInput(
            "仅 Draft 或 Ready 状态的任务可编辑".to_string(),
        ));
    }
    let content = request.content.unwrap_or_default();
    store
        .update_task_goal(&tid, content.clone())
        .map_err(|error| ApiError::internal_assembly("更新队列任务失败", error))?;

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.queued.updated",
        json!({
            "queueId": request.queue_id,
            "content": content,
            "updatedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("更新队列任务事件发布失败", err))?;

    Ok(Json(json!({
        "queueId": request.queue_id,
        "updated": true,
        "eventId": event_id.to_string(),
        "updatedAt": now.0,
    })))
}

async fn delete_queued_task(
    State(state): State<ApiState>,
    Json(request): Json<QueuedTaskRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-queued-delete-{}", now.0));
    let tid = TaskId::new(&request.queue_id);

    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("delete queued task", "task_store 未配置"))?;
    let task = store
        .get_task(&tid)
        .ok_or_else(|| ApiError::not_found("队列任务不存在", &request.queue_id))?;
    if task.status != TaskStatus::Draft && task.status != TaskStatus::Ready {
        return Err(ApiError::InvalidInput(
            "仅 Draft 或 Ready 状态的任务可删除".to_string(),
        ));
    }
    store.remove_task(&tid);

    let event = EventEnvelope::domain(
        event_id.clone(),
        "task.queued.deleted",
        json!({
            "queueId": request.queue_id,
            "deletedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("删除队列任务事件发布失败", err))?;

    Ok(Json(json!({
        "queueId": request.queue_id,
        "deleted": true,
        "eventId": event_id.to_string(),
        "deletedAt": now.0,
    })))
}

async fn confirm_recovery(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-recovery-confirm-{}", now.0));

    let task_id = request
        .get("taskId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("taskId 不能为空".to_string()))?;
    let tid = TaskId::new(task_id);

    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("confirm recovery", "task_store 未配置"))?;
    let task = store
        .get_task(&tid)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id))?;
    if task.status != TaskStatus::Failed && task.status != TaskStatus::Blocked {
        return Err(ApiError::InvalidInput(
            "当前任务状态不支持恢复确认".to_string(),
        ));
    }
    store
        .update_status(&tid, TaskStatus::Ready)
        .map_err(|error| ApiError::internal_assembly("恢复确认状态更新失败", error))?;

    if let Some(manager) = state.runner_manager() {
        let _ = manager.start(task_id);
    }

    let event = EventEnvelope::domain(
        event_id.clone(),
        "recovery.confirmed",
        json!({
            "taskId": task_id,
            "confirmedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("恢复确认事件发布失败", err))?;

    Ok(Json(json!({
        "taskId": task_id,
        "confirmed": true,
        "storeUpdated": true,
        "eventId": event_id.to_string(),
        "confirmedAt": now.0,
    })))
}

/// 交互响应：通过事件总线发布用户交互响应事件
async fn interaction_response(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-interaction-response-{}", now.0));

    let event = EventEnvelope::domain(
        event_id.clone(),
        "interaction.response.submitted",
        json!({
            "request": request,
            "submittedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("交互响应事件发布失败", err))?;

    Ok(Json(json!({
        "submitted": true,
        "eventId": event_id.to_string(),
        "submittedAt": now.0,
    })))
}

/// 澄清提交：通过事件总线发布澄清交互事件
async fn interaction_clarification(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-clarification-{}", now.0));

    let event = EventEnvelope::domain(
        event_id.clone(),
        "interaction.clarification.submitted",
        json!({
            "request": request,
            "submittedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("澄清事件发布失败", err))?;

    Ok(Json(json!({
        "submitted": true,
        "eventId": event_id.to_string(),
        "submittedAt": now.0,
    })))
}

/// Worker 提问：通过事件总线发布 worker 提问交互事件
async fn worker_question(
    State(state): State<ApiState>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-worker-question-{}", now.0));

    let event = EventEnvelope::domain(
        event_id.clone(),
        "worker.question.response.submitted",
        json!({
            "request": request,
            "submittedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("Worker 提问响应事件发布失败", err))?;

    Ok(Json(json!({
        "submitted": true,
        "eventId": event_id.to_string(),
        "submittedAt": now.0,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AbandonChainRequest {
    chain_id: String,
}

async fn abandon_chain(
    State(state): State<ApiState>,
    Json(request): Json<AbandonChainRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = UtcMillis::now();
    let event_id = EventId::new(format!("event-chain-abandon-{}", now.0));

    let event = EventEnvelope::domain(
        event_id.clone(),
        "chain.abandon.requested",
        json!({
            "chainId": request.chain_id,
            "requestedAt": now.0,
        }),
    );
    state
        .event_bus
        .publish(event)
        .map_err(|err| ApiError::event_publish_failed("放弃执行链事件发布失败", err))?;

    Ok(Json(json!({
        "chainId": request.chain_id,
        "abandoned": true,
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}
