use axum::{Json, Router, extract::State, routing::post};
use magi_core::{EventId, SessionId, TaskId, UtcMillis};
use magi_event_bus::EventEnvelope;
use serde::Deserialize;
use serde_json::json;

use super::session_scope::parse_session_id;
use crate::{
    errors::ApiError, execution_chain_recovery::finalize_terminal_worker_branches, state::ApiState,
};

// ---------------------------------------------------------------------------
// Intake classification (design 8)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntakeRequest {
    session_id: Option<String>,
    message: String,
    #[serde(default)]
    context_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IntakeClassification {
    DecisionAnswer,
    Pause,
    Replin,
    SupplementContext,
    AppendTask,
    NewObjective,
    GeneralChat,
}

fn classify_intake(message: &str) -> IntakeClassification {
    let lower = message.trim().to_lowercase();
    // 决策回答
    if lower.starts_with("选择")
        || lower.starts_with("选 ")
        || lower.starts_with("确认")
        || lower.starts_with("同意")
        || lower.starts_with("驳回")
        || lower.starts_with("跳过")
        || lower.starts_with("取消")
    {
        return IntakeClassification::DecisionAnswer;
    }
    // 暂停
    if lower.contains("暂停")
        || lower.contains("停止")
        || lower.contains("先停")
        || lower.contains("中断")
    {
        return IntakeClassification::Pause;
    }
    // 重规划
    if lower.contains("重新规划")
        || lower.contains("改一下")
        || lower.contains("修改目标")
        || lower.contains("调整")
    {
        return IntakeClassification::Replin;
    }
    // 补充上下文
    if lower.contains("补充")
        || lower.contains("上下文")
        || lower.contains("补充信息")
        || lower.contains("补充说明")
    {
        return IntakeClassification::SupplementContext;
    }
    // 新增任务
    if lower.contains("顺便")
        || lower.contains("再加")
        || lower.contains("追加")
        || lower.contains("另外")
    {
        return IntakeClassification::AppendTask;
    }
    // 新目标
    if lower.contains("新任务") || lower.contains("新目标") || lower.contains("换个方向")
    {
        return IntakeClassification::NewObjective;
    }
    IntakeClassification::GeneralChat
}

/// 处理深度模式运行中的用户中途输入（design 8）。
/// 先进行 Intake 分类，再根据分类结果写入 Mission context、触发 replan、
/// resolve Decision 或追加子任务，禁止直接塞给当前 worker。
async fn handle_intake(
    State(state): State<ApiState>,
    Json(request): Json<IntakeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("intake", "task_store 未配置"))?;

    let ownership = state
        .session_store
        .execution_ownership(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let mission_id = ownership
        .mission_id
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务链".to_string()))?;

    // 定位当前活跃的 root task
    let root_task = store
        .get_tasks_by_mission(&mission_id)
        .into_iter()
        .find(|t| t.parent_task_id.is_none())
        .ok_or_else(|| ApiError::InvalidInput("当前 Mission 没有根任务".to_string()))?;
    let root_task_id = root_task.task_id.clone();

    let classification = classify_intake(&request.message);

    match classification {
        IntakeClassification::DecisionAnswer => {
            // 查找当前 pending 的 Decision task
            let pending_decision = store
                .get_tasks_by_mission(&mission_id)
                .into_iter()
                .find(|t| {
                    t.kind == magi_core::TaskKind::Decision
                        && t.status == magi_core::TaskStatus::AwaitingApproval
                });
            if let Some(decision) = pending_decision {
                // 简单匹配：取消息中第一个包含的 option_id
                let chosen = decision
                    .decision_payload
                    .as_ref()
                    .and_then(|payload| {
                        payload.options.iter().find_map(|opt| {
                            if request.message.contains(&opt.option_id)
                                || request.message.contains(&opt.label)
                            {
                                Some(opt.option_id.clone())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or_else(|| "continue".to_string());
                store
                    .resolve_decision(&decision.task_id, &chosen, None)
                    .map_err(|e| ApiError::InvalidInput(e))?;
                return Ok(Json(json!({
                    "classification": "decision_answer",
                    "resolved": true,
                    "decisionTaskId": decision.task_id.to_string(),
                    "chosenOption": chosen,
                })));
            }
            return Ok(Json(json!({
                "classification": "decision_answer",
                "resolved": false,
                "reason": "没有待处理的 Decision 任务",
            })));
        }
        IntakeClassification::Pause => {
            let manager = state.runner_manager().ok_or_else(|| {
                ApiError::internal_assembly("intake pause", "runner_manager 未配置")
            })?;
            manager
                .pause_tree(root_task_id.as_str())
                .map_err(|e| ApiError::internal_assembly("暂停失败", e))?;
            return Ok(Json(json!({
                "classification": "pause",
                "paused": true,
                "rootTaskId": root_task_id.to_string(),
                "contextTaskId": request.context_task_id,
            })));
        }
        IntakeClassification::Replin => {
            let manager = state.runner_manager().ok_or_else(|| {
                ApiError::internal_assembly("intake replan", "runner_manager 未配置")
            })?;
            let cancelled = manager
                .replan(root_task_id.as_str())
                .map_err(|e| ApiError::internal_assembly("重规划失败", e))?;
            return Ok(Json(json!({
                "classification": "replan",
                "replan": true,
                "cancelledTaskIds": cancelled.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "contextTaskId": request.context_task_id,
            })));
        }
        IntakeClassification::SupplementContext => {
            // 将补充信息写入 root task 的 context_refs
            let context_ref = format!("intake-context-{}", UtcMillis::now().0);
            let mut root = root_task;
            root.context_refs.push(context_ref.clone());
            root.updated_at = UtcMillis::now();
            store.insert_task(root);
            return Ok(Json(json!({
                "classification": "supplement_context",
                "contextRef": context_ref,
                "note": "补充上下文已写入 Mission context",
                "contextTaskId": request.context_task_id,
            })));
        }
        IntakeClassification::AppendTask => {
            // 在 root 下追加一个新的 Action task（Draft 状态，由 Runner 后续推进）
            let new_task_id =
                TaskId::new(format!("{}-intake-{}", root_task_id, UtcMillis::now().0));
            let new_task = magi_core::Task {
                task_id: new_task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id: Some(root_task_id.clone()),
                kind: magi_core::TaskKind::Action,
                title: request.message.clone(),
                goal: request.message.clone(),
                status: magi_core::TaskStatus::Draft,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: root_task.policy_snapshot.clone(),
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: root_task.workspace_scope.clone(),
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            };
            store.insert_task(new_task);
            return Ok(Json(json!({
                "classification": "append_task",
                "addedTaskId": new_task_id.to_string(),
            })));
        }
        IntakeClassification::NewObjective => {
            return Ok(Json(json!({
                "classification": "new_objective",
                "note": "新目标请通过新 session action 提交",
            })));
        }
        IntakeClassification::GeneralChat => {
            return Ok(Json(json!({
                "classification": "general_chat",
                "note": "普通聊天消息暂不写入任务图",
            })));
        }
    }
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/task/interrupt", post(interrupt_task))
        .route("/session/intake", post(handle_intake))
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
    for subtree_task_id in store.collect_subtree_ids(&root_task_id) {
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
        "eventId": event_id.to_string(),
        "requestedAt": now.0,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_intake_decision_answer() {
        assert_eq!(
            classify_intake("选择 A"),
            IntakeClassification::DecisionAnswer
        );
        assert_eq!(
            classify_intake("确认"),
            IntakeClassification::DecisionAnswer
        );
        assert_eq!(
            classify_intake("同意继续"),
            IntakeClassification::DecisionAnswer
        );
        assert_eq!(
            classify_intake("驳回方案"),
            IntakeClassification::DecisionAnswer
        );
        assert_eq!(
            classify_intake("跳过此步骤"),
            IntakeClassification::DecisionAnswer
        );
        assert_eq!(
            classify_intake("取消操作"),
            IntakeClassification::DecisionAnswer
        );
    }

    #[test]
    fn classify_intake_pause() {
        assert_eq!(classify_intake("暂停一下"), IntakeClassification::Pause);
        assert_eq!(classify_intake("停止当前任务"), IntakeClassification::Pause);
        assert_eq!(classify_intake("先停一下"), IntakeClassification::Pause);
        assert_eq!(classify_intake("中断执行"), IntakeClassification::Pause);
    }

    #[test]
    fn classify_intake_replan() {
        assert_eq!(classify_intake("重新规划"), IntakeClassification::Replin);
        assert_eq!(classify_intake("改一下目标"), IntakeClassification::Replin);
        assert_eq!(
            classify_intake("修改目标方向"),
            IntakeClassification::Replin
        );
        assert_eq!(classify_intake("调整方案"), IntakeClassification::Replin);
    }

    #[test]
    fn classify_intake_supplement_context() {
        assert_eq!(
            classify_intake("补充上下文"),
            IntakeClassification::SupplementContext
        );
        assert_eq!(
            classify_intake("补充一些信息"),
            IntakeClassification::SupplementContext
        );
        assert_eq!(
            classify_intake("这里需要补充说明"),
            IntakeClassification::SupplementContext
        );
    }

    #[test]
    fn classify_intake_append_task() {
        assert_eq!(
            classify_intake("顺便加个任务"),
            IntakeClassification::AppendTask
        );
        assert_eq!(
            classify_intake("再加一个功能"),
            IntakeClassification::AppendTask
        );
        assert_eq!(
            classify_intake("追加需求"),
            IntakeClassification::AppendTask
        );
        assert_eq!(
            classify_intake("另外还需要"),
            IntakeClassification::AppendTask
        );
    }

    #[test]
    fn classify_intake_new_objective() {
        assert_eq!(
            classify_intake("新任务：优化性能"),
            IntakeClassification::NewObjective
        );
        assert_eq!(
            classify_intake("换个方向做"),
            IntakeClassification::NewObjective
        );
        assert_eq!(
            classify_intake("设定新目标"),
            IntakeClassification::NewObjective
        );
    }

    #[test]
    fn classify_intake_general_chat() {
        assert_eq!(classify_intake("你好"), IntakeClassification::GeneralChat);
        assert_eq!(
            classify_intake("今天天气不错"),
            IntakeClassification::GeneralChat
        );
        assert_eq!(
            classify_intake("帮我看看这个代码"),
            IntakeClassification::GeneralChat
        );
    }
}
