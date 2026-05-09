use axum::{Json, Router, extract::State, routing::post};
use magi_bridge_client::{
    ChatToolChoice, ChatToolDefinition, ChatToolFunctionDefinition, ModelInvocationRequest,
    LOOPBACK_MODEL_PROVIDER,
};
use magi_core::{EventId, MissionId, SessionId, Task, TaskId, TaskKind, TaskStatus, UtcMillis};
use magi_event_bus::EventEnvelope;
use magi_session_store::TimelineEntryKind;
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

use super::session_scope::{parse_session_id, session_workspace_id};
use crate::{
    errors::ApiError,
    execution_chain_recovery::finalize_terminal_worker_branches,
    dispatch_execution::{
        ensure_session_active_execution_chain, register_appended_task_execution_branch,
        replace_replanned_task_execution_branches, replan_deep_task_graph,
    },
    state::ApiState,
};

// ---------------------------------------------------------------------------
// Intake classification (design 8)
// ---------------------------------------------------------------------------

static INTAKE_CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntakeRequest {
    session_id: Option<String>,
    message: String,
    #[serde(default)]
    context_task_id: Option<String>,
    #[serde(default)]
    force_supplement_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IntakeClassification {
    DecisionAnswer,
    Pause,
    Replan,
    SupplementContext,
    AppendTask,
    NewObjective,
    GeneralChat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IntakeIntentDecision {
    classification: IntakeClassification,
    task_title: Option<String>,
    task_goal: Option<String>,
}

fn decide_intake_with_task_planner(
    state: &ApiState,
    message: &str,
    root_task: &Task,
    context_task: &Task,
) -> Result<IntakeIntentDecision, ApiError> {
    let client = state.task_planning_model_client().cloned().ok_or_else(|| {
        ApiError::InvalidInput("Intake 分类器未配置任务规划模型客户端".to_string())
    })?;
    let response = client
        .invoke(ModelInvocationRequest {
            provider: LOOPBACK_MODEL_PROVIDER.to_string(),
            prompt: build_intake_classifier_prompt(message, root_task, context_task),
            messages: None,
            tools: Some(vec![intake_classifier_tool()]),
            tool_choice: Some(ChatToolChoice::force_function("classify_session_intake")),
        })
        .map_err(|error| ApiError::model_invocation_failed("Intake 分类失败", error))?;
    if !response.ok {
        return Err(ApiError::ModelInvocationFailed(
            "Intake 分类器返回失败状态".to_string(),
        ));
    }
    validate_intake_decision(parse_intake_decision(&response.payload)?)
}

fn validate_intake_decision(
    decision: IntakeIntentDecision,
) -> Result<IntakeIntentDecision, ApiError> {
    if matches!(decision.classification, IntakeClassification::AppendTask)
        && (decision.task_title.is_none() || decision.task_goal.is_none())
    {
        return Err(ApiError::InvalidInput(
            "Intake 分类器判定追加任务但缺少 taskTitle/taskGoal".to_string(),
        ));
    }
    Ok(decision)
}

fn build_intake_classifier_prompt(message: &str, root_task: &Task, context_task: &Task) -> String {
    format!(
        "Session Intake 编排分类器\n\
         请只调用 classify_session_intake 工具，输出运行中任务链收到用户中途输入后的处理方式。\n\
         classification 只能是 decision_answer、pause、replan、supplement_context、append_task、new_objective、general_chat。\n\
         decision_answer：用户在回答待确认 Decision。\n\
         pause：用户明确要求暂停、中断或停止当前任务链。\n\
         replan：用户明确要求改变现有目标、约束或执行方案，并希望重规划剩余任务。\n\
         supplement_context：用户补充上下文、事实、限制或说明，应写入当前任务上下文。\n\
         append_task：用户明确要求向当前任务图追加一个新的、可执行、可验证的子任务；不要仅因为“顺便、另外、再加、追加”等连接词选择 append_task。\n\
         new_objective：用户提出与当前任务链并列的新目标，应让用户通过新 session action 提交。\n\
         general_chat：普通聊天、追问、状态询问、表达偏好但没有改变任务图的输入。\n\
         当选择 append_task 时必须给出 taskTitle 与 taskGoal；其他分类设为 null。\n\
         rootTaskTitle=\"{}\"\n\
         rootTaskGoal=\"{}\"\n\
         contextTaskId=\"{}\"\n\
         contextTaskKind=\"{:?}\"\n\
         contextTaskTitle=\"{}\"\n\
         contextTaskGoal=\"{}\"\n\
         userText=\"{}\"",
        root_task.title,
        root_task.goal,
        context_task.task_id,
        context_task.kind,
        context_task.title,
        context_task.goal,
        message.trim()
    )
}

fn intake_classifier_tool() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: "classify_session_intake".to_string(),
            description: "判断运行中任务链收到用户中途输入后的处理方式。".to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["classification", "taskTitle", "taskGoal"],
                "properties": {
                    "classification": {
                        "type": "string",
                        "enum": [
                            "decision_answer",
                            "pause",
                            "replan",
                            "supplement_context",
                            "append_task",
                            "new_objective",
                            "general_chat"
                        ]
                    },
                    "taskTitle": { "type": ["string", "null"] },
                    "taskGoal": { "type": ["string", "null"] }
                }
            }),
        },
    }
}

fn parse_intake_decision(payload: &str) -> Result<IntakeIntentDecision, ApiError> {
    let normalized_payload = payload
        .trim()
        .strip_prefix("loopback-model::")
        .unwrap_or_else(|| payload.trim())
        .trim();
    let parsed =
        serde_json::from_str::<serde_json::Value>(normalized_payload).map_err(|error| {
            ApiError::InvalidInput(format!("Intake 分类器输出不是有效 JSON: {error}"))
        })?;
    let calls = parsed
        .get("tool_calls")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            ApiError::InvalidInput("Intake 分类器未调用 classify_session_intake 工具".to_string())
        })?;
    for call in calls {
        if let Some(arguments) = intake_arguments_from_tool_call(call) {
            return intake_decision_from_value(arguments?);
        }
    }
    Err(ApiError::InvalidInput(
        "Intake 分类器未调用 classify_session_intake 工具".to_string(),
    ))
}

fn intake_arguments_from_tool_call(
    call: &serde_json::Value,
) -> Option<Result<serde_json::Value, ApiError>> {
    let function = call.get("function")?;
    if function.get("name").and_then(|value| value.as_str())? != "classify_session_intake" {
        return None;
    }
    let Some(arguments) = function.get("arguments").and_then(|value| value.as_str()) else {
        return Some(Err(ApiError::InvalidInput(
            "Intake 分类器工具参数缺失".to_string(),
        )));
    };
    Some(serde_json::from_str(arguments).map_err(|error| {
        ApiError::InvalidInput(format!("Intake 分类器工具参数不是有效 JSON: {error}"))
    }))
}

fn intake_decision_from_value(value: serde_json::Value) -> Result<IntakeIntentDecision, ApiError> {
    let classification = match value.get("classification").and_then(|value| value.as_str()) {
        Some("decision_answer") => IntakeClassification::DecisionAnswer,
        Some("pause") => IntakeClassification::Pause,
        Some("replan") => IntakeClassification::Replan,
        Some("supplement_context") => IntakeClassification::SupplementContext,
        Some("append_task") => IntakeClassification::AppendTask,
        Some("new_objective") => IntakeClassification::NewObjective,
        Some("general_chat") => IntakeClassification::GeneralChat,
        Some(other) => {
            return Err(ApiError::InvalidInput(format!(
                "Intake 分类器返回未知 classification: {other}"
            )));
        }
        None => {
            return Err(ApiError::InvalidInput(
                "Intake 分类器缺少 classification".to_string(),
            ));
        }
    };
    Ok(IntakeIntentDecision {
        classification,
        task_title: optional_trimmed_field(&value, "taskTitle"),
        task_goal: optional_trimmed_field(&value, "taskGoal"),
    })
}

fn optional_trimmed_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn resolve_context_task(
    store: &magi_orchestrator::task_store::TaskStore,
    mission_id: &MissionId,
    root_task: &Task,
    context_task_id: Option<&str>,
) -> Result<Task, ApiError> {
    let Some(raw_context_task_id) = context_task_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(root_task.clone());
    };
    let context_task_id = TaskId::new(raw_context_task_id);
    let context_task = store.get_task(&context_task_id).ok_or_else(|| {
        ApiError::InvalidInput(format!("上下文任务不存在: {}", raw_context_task_id))
    })?;
    if context_task.mission_id != *mission_id {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话",
            raw_context_task_id
        )));
    }
    Ok(context_task)
}

fn is_pending_decision(task: &Task) -> bool {
    task.kind == TaskKind::Decision && task.status == TaskStatus::AwaitingApproval
}

fn decision_matches_context(task: &Task, context_task_id: &TaskId) -> bool {
    task.task_id == *context_task_id
        || task
            .parent_task_id
            .as_ref()
            .is_some_and(|parent_task_id| parent_task_id == context_task_id)
        || task
            .decision_payload
            .as_ref()
            .and_then(|payload| payload.target_task_id.as_ref())
            .is_some_and(|target_task_id| target_task_id == context_task_id)
}

fn resolve_structural_context_task_id(root_task: &Task, context_task: &Task) -> TaskId {
    match context_task.kind {
        TaskKind::Objective | TaskKind::Phase | TaskKind::WorkPackage => {
            context_task.task_id.clone()
        }
        _ => context_task
            .parent_task_id
            .clone()
            .unwrap_or_else(|| root_task.task_id.clone()),
    }
}

fn build_intake_replan_prompt(root_task: &Task, context_task: &Task, message: &str) -> String {
    let root_goal = root_task.goal.trim();
    let objective_text = if root_goal.is_empty() {
        root_task.title.as_str()
    } else {
        root_goal
    };
    let context_goal = context_task.goal.trim();
    let context_text = if context_goal.is_empty() {
        context_task.title.as_str()
    } else {
        context_goal
    };
    format!(
        "当前任务目标：{}\n当前上下文任务：{} ({:?})\n用户重规划约束：{}\n请基于最新约束重规划剩余任务图，保留已完成节点，不重写已完成工作。",
        objective_text,
        context_text,
        context_task.kind,
        message.trim()
    )
}

fn record_supplement_context(
    state: &ApiState,
    store: &magi_orchestrator::task_store::TaskStore,
    session_id: &SessionId,
    context_task_id: &TaskId,
    message: &str,
) -> Result<serde_json::Value, ApiError> {
    let context_ref = format!(
        "intake-context-{}-{}",
        UtcMillis::now().0,
        INTAKE_CONTEXT_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let context_entry = store
        .append_context_entry(context_task_id, context_ref.clone(), message.to_string())
        .map_err(|e| ApiError::internal_assembly("补充上下文失败", e.to_string()))?;
    state.session_store.append_timeline_entry(
        session_id.clone(),
        TimelineEntryKind::UserMessage,
        format!("[补充上下文] {}", message.trim()),
    );
    state.persist_session_durable_state()?;
    Ok(json!({
        "classification": "supplement_context",
        "contextRef": context_ref,
        "content": context_entry.content,
        "note": "补充上下文已写入当前任务上下文",
        "contextTaskId": context_task_id.to_string(),
    }))
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
    let context_task = resolve_context_task(
        store,
        &mission_id,
        &root_task,
        request.context_task_id.as_deref(),
    )?;
    let context_task_id = context_task.task_id.clone();

    if request.force_supplement_context {
        return Ok(Json(record_supplement_context(
            &state,
            store,
            &session_id,
            &context_task_id,
            &request.message,
        )?));
    }

    let decision =
        decide_intake_with_task_planner(&state, &request.message, &root_task, &context_task)?;
    let classification = decision.classification.clone();

    match classification {
        IntakeClassification::DecisionAnswer => {
            let tasks = store.get_tasks_by_mission(&mission_id);
            let pending_decision = tasks
                .iter()
                .find(|task| {
                    is_pending_decision(task) && decision_matches_context(task, &context_task_id)
                })
                .or_else(|| tasks.iter().find(|task| is_pending_decision(task)))
                .cloned();
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
                    "contextTaskId": context_task_id.to_string(),
                })));
            }
            return Ok(Json(json!({
                "classification": "decision_answer",
                "resolved": false,
                "reason": "没有待处理的 Decision 任务",
                "contextTaskId": context_task_id.to_string(),
            })));
        }
        IntakeClassification::Pause => {
            let manager = state.runner_manager().ok_or_else(|| {
                ApiError::internal_assembly("intake pause", "runner_manager 未配置")
            })?;
            let pause_target_id = if context_task.status == TaskStatus::Running {
                &context_task_id
            } else {
                &root_task_id
            };
            manager
                .pause_task(pause_target_id.as_str())
                .map_err(|e| ApiError::internal_assembly("暂停失败", e))?;
            return Ok(Json(json!({
                "classification": "pause",
                "paused": true,
                "rootTaskId": root_task_id.to_string(),
                "pausedTaskId": pause_target_id.to_string(),
                "contextTaskId": context_task_id.to_string(),
            })));
        }
        IntakeClassification::Replan => {
            ensure_session_active_execution_chain(&state, &session_id)?;
            let prompt = build_intake_replan_prompt(&root_task, &context_task, &request.message);
            let session = state
                .session_store
                .session(&session_id)
                .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
            let workspace_id = session_workspace_id(&state, &session);
            let replan = replan_deep_task_graph(
                &state,
                &root_task_id,
                &prompt,
                Some(&context_task),
                &workspace_id,
                request.message.trim(),
            )?;
            replace_replanned_task_execution_branches(
                &state,
                &session_id,
                &replan.primary_action_task_id,
                &replan.dispatch_task_ids,
            )?;
            return Ok(Json(json!({
                "classification": "replan",
                "replan": true,
                "rootTaskId": root_task_id.to_string(),
                "targetTaskId": root_task_id.to_string(),
                "cancelledTaskIds": replan.cancelled_task_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "primaryActionTaskId": replan.primary_action_task_id.to_string(),
                "leafActionTaskIds": replan.leaf_action_task_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "validationTaskIds": replan.validation_task_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "contextTaskId": context_task_id.to_string(),
            })));
        }
        IntakeClassification::SupplementContext => {
            return Ok(Json(record_supplement_context(
                &state,
                store,
                &session_id,
                &context_task_id,
                &request.message,
            )?));
        }
        IntakeClassification::AppendTask => {
            ensure_session_active_execution_chain(&state, &session_id)?;
            let parent_task_id = resolve_structural_context_task_id(&root_task, &context_task);
            let parent_task = store
                .get_task(&parent_task_id)
                .unwrap_or_else(|| root_task.clone());
            let task_title = decision.task_title.ok_or_else(|| {
                ApiError::InvalidInput("Intake 分类器判定追加任务但缺少 taskTitle".to_string())
            })?;
            let task_goal = decision.task_goal.ok_or_else(|| {
                ApiError::InvalidInput("Intake 分类器判定追加任务但缺少 taskGoal".to_string())
            })?;
            let new_task_id =
                TaskId::new(format!("{}-intake-{}", parent_task_id, UtcMillis::now().0));
            let new_task = magi_core::Task {
                task_id: new_task_id.clone(),
                mission_id: mission_id.clone(),
                root_task_id: root_task_id.clone(),
                parent_task_id: Some(parent_task_id.clone()),
                kind: magi_core::TaskKind::Action,
                title: task_title,
                goal: task_goal,
                status: magi_core::TaskStatus::Ready,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: parent_task.policy_snapshot.clone(),
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: parent_task.workspace_scope.clone(),
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
            store
                .append_required_child(&parent_task_id, &new_task_id)
                .map_err(|e| ApiError::internal_assembly("追加任务失败", e.to_string()))?;
            register_appended_task_execution_branch(&state, &session_id, &new_task_id)?;
            return Ok(Json(json!({
                "classification": "append_task",
                "addedTaskId": new_task_id.to_string(),
                "parentTaskId": parent_task_id.to_string(),
                "contextTaskId": context_task_id.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{ExecutionOwnership, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn make_intake_task(
        task_id: &str,
        root_task_id: &str,
        parent_task_id: Option<&str>,
        kind: TaskKind,
    ) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-1"),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent_task_id.map(TaskId::new),
            kind,
            title: format!("Task {task_id}"),
            goal: format!("Goal for {task_id}"),
            status: TaskStatus::Ready,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    #[test]
    fn resolve_structural_context_task_id_uses_parent_for_leaf_context() {
        let root = make_intake_task("obj-1", "obj-1", None, TaskKind::Objective);
        let phase = make_intake_task("phase-1", "obj-1", Some("obj-1"), TaskKind::Phase);
        let action = make_intake_task("act-1", "obj-1", Some("phase-1"), TaskKind::Action);

        assert_eq!(
            resolve_structural_context_task_id(&root, &root),
            TaskId::new("obj-1")
        );
        assert_eq!(
            resolve_structural_context_task_id(&root, &phase),
            TaskId::new("phase-1")
        );
        assert_eq!(
            resolve_structural_context_task_id(&root, &action),
            TaskId::new("phase-1")
        );
    }

    fn intake_classifier_payload(arguments: serde_json::Value) -> String {
        serde_json::json!({
            "content": null,
            "finish_reason": "tool_calls",
            "tool_calls": [{
                "id": "call-classify-session-intake",
                "type": "function",
                "function": {
                    "name": "classify_session_intake",
                    "arguments": arguments.to_string(),
                }
            }]
        })
        .to_string()
    }

    #[test]
    fn parse_intake_decision_reads_tool_call_payload() {
        let decision = parse_intake_decision(&intake_classifier_payload(json!({
            "classification": "append_task",
            "taskTitle": "补充移动端验收",
            "taskGoal": "完成移动端视口真实验收并记录结论",
        })))
        .expect("tool payload should parse");

        assert_eq!(decision.classification, IntakeClassification::AppendTask);
        assert_eq!(decision.task_title.as_deref(), Some("补充移动端验收"));
        assert_eq!(
            decision.task_goal.as_deref(),
            Some("完成移动端视口真实验收并记录结论")
        );
    }

    #[test]
    fn parse_intake_decision_requires_tool_call_payload() {
        let error = parse_intake_decision(
            r#"{"classification":"append_task","taskTitle":"误建任务","taskGoal":"不应接受裸 JSON"}"#,
        )
        .expect_err("裸 JSON 不能绕过强制 tool call");

        assert!(
            error
                .message()
                .contains("Intake 分类器未调用 classify_session_intake 工具")
        );
    }

    #[test]
    fn validate_intake_decision_requires_append_task_fields() {
        let error = validate_intake_decision(IntakeIntentDecision {
            classification: IntakeClassification::AppendTask,
            task_title: Some("补充验收".to_string()),
            task_goal: None,
        })
        .expect_err("append_task 必须携带结构化目标");

        assert!(error.message().contains("缺少 taskTitle/taskGoal"));
    }

    #[test]
    fn intake_classifier_prompt_blocks_keyword_only_task_creation() {
        let root = make_intake_task("obj-1", "obj-1", None, TaskKind::Objective);
        let action = make_intake_task("act-1", "obj-1", Some("obj-1"), TaskKind::Action);
        let prompt = build_intake_classifier_prompt("顺便看看当前状态", &root, &action);

        assert_eq!(
            prompt
                .matches("不要仅因为“顺便、另外、再加、追加”等连接词选择 append_task")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn forced_supplement_context_records_content_and_task_ref() {
        let session_id = SessionId::new("session-intake-guide");
        let mission_id = MissionId::new("mission-1");
        let task_store = Arc::new(TaskStore::new());
        task_store.insert_task(make_intake_task(
            "obj-1",
            "obj-1",
            None,
            TaskKind::Objective,
        ));
        task_store.insert_task(make_intake_task(
            "act-1",
            "obj-1",
            Some("obj-1"),
            TaskKind::Action,
        ));

        let session_store = Arc::new(SessionStore::default());
        session_store
            .create_session(session_id.clone(), "intake guide")
            .expect("session should create");
        session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(WorkspaceId::new("workspace-guide")),
                mission_id: Some(mission_id),
                ..ExecutionOwnership::default()
            },
        );

        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            session_store.clone(),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(task_store.clone());

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/session/intake")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "sessionId": session_id.to_string(),
                            "message": "后续执行必须优先验证真实浏览器状态",
                            "contextTaskId": "act-1",
                            "forceSupplementContext": true,
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("payload should deserialize");
        assert_eq!(payload["classification"], "supplement_context");
        let context_ref = payload["contextRef"]
            .as_str()
            .expect("contextRef should exist")
            .to_string();
        assert_eq!(payload["content"], "后续执行必须优先验证真实浏览器状态");

        let updated_task = task_store
            .get_task(&TaskId::new("act-1"))
            .expect("context task should exist");
        assert_eq!(updated_task.context_refs, vec![context_ref.clone()]);
        let entries = task_store.context_entries_for_refs(&[context_ref]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "后续执行必须优先验证真实浏览器状态");
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .any(|entry| entry.message.contains("后续执行必须优先验证真实浏览器状态"))
        );
    }
}
