use axum::{Json, Router, extract::State, routing::post};
use magi_core::{EventId, SessionId, TaskId, UtcMillis};
use magi_event_bus::EventEnvelope;
use serde::Deserialize;
use serde_json::json;

use super::session_scope::parse_session_id;
use crate::{
    errors::ApiError, execution_chain_recovery::finalize_terminal_worker_branches, state::ApiState,
};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/task/interrupt", post(interrupt_task))
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
struct TaskIdRequest {
    task_id: String,
    session_id: Option<String>,
}

/// 中断当前任务链：只暂停原执行树，继续操作统一走 `/api/session/continue`。
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
    let subtree_task_ids = store.collect_subtree_ids(&root_task_id);
    let cancelled_tool_process_count = subtree_task_ids
        .iter()
        .map(|subtree_task_id| {
            state.cancel_active_tool_executions(Some(&session_id), None, Some(subtree_task_id))
        })
        .sum::<usize>();
    for subtree_task_id in subtree_task_ids {
        if let Some(lease) = store.get_active_lease(&subtree_task_id) {
            store.revoke_lease(&subtree_task_id, &lease.lease_id);
        }
        state.session_store.remove_timeline_entry(
            &session_id,
            &format!("timeline-streaming-{}", subtree_task_id),
        );
    }
    finalize_terminal_worker_branches(&state, &session_id)?;
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
        "cancelledToolProcessCount": cancelled_tool_process_count,
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}
