use crate::RunnerStartError;
pub use crate::session_turn_execution::{
    BUSINESS_MODEL_PROVIDER, SessionTurnExecutionOutput, SessionTurnExecutionRequest,
};
use crate::{
    errors::ApiError,
    model_config::NormalizedModelConfig,
    prompt_utils::prepend_session_instructions,
    session_turn_execution::{SessionTurnExecutionRuntime, run_session_turn_execution},
    settings_store::SettingsStore,
    dispatch_execution::{
        TaskGraphSubmission, cleanup_task_tree, run_dispatch_submission,
    },
    state::{ApiState, ExecutionPipeline},
    usage_recording::{ModelUsageBinding, model_usage_binding_for_worker},
};
use magi_conversation_runtime::session_writeback::{
    append_session_turn_item_with_task_store, publish_current_session_turn_item_event,
    publish_session_turn_item_event, session_turn_item,
};
use magi_conversation_runtime::{
    SKILL_APPLY_TOOL_NAME, public_builtin_tool_definitions, skill_apply_tool_definition,
};
use magi_bridge_client::{ChatToolDefinition, ModelBridgeClient};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
};
use magi_core::{
    ApprovalRequirement, DomainError, EventId, ExecutionOwnership, ExecutionResultStatus, LeaseId,
    RiskLevel, SessionId, Task, TaskExecutionTarget, TaskId, TaskKind, TaskStatus, ToolCallId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_governance::ToolKind;
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_knowledge_store::{KnowledgeKind, KnowledgeRecord, KnowledgeStore};
use magi_orchestrator::{
    ExecutionContextSummary, ExecutionWritebackPlans,
    task_runner::{EventBasedResultReceiver, TaskDispatcher, TaskOutcome, TaskResult, WorkerInfo},
    task_store::TaskStore,
};
use magi_conversation_runtime::{ConversationRegistry, StreamFanOut};
use magi_session_store::{SessionStore, TimelineEntryKind, timeline_entry_visible_text};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolRegistry,
};
use magi_workspace::WorkspaceStore;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug)]
pub enum TaskExecutionPlan {
    Dispatch {
        target: TaskExecutionTarget,
        worker_id: WorkerId,
        lane_id: Option<String>,
        lane_seq: Option<usize>,
        /// lane 绑定的 thread，由 dispatch_execution::ensure_thread_for_role 创建或命中，
        /// 是 task_llm_loop 读取跨 task 历史消息的唯一路由键。
        thread_id: magi_core::ThreadId,
        is_primary: bool,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
    },
}

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

fn turn_item_status_for_task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Draft => "pending",
        TaskStatus::Ready
        | TaskStatus::Running
        | TaskStatus::AwaitingApproval
        | TaskStatus::Blocked
        | TaskStatus::Verifying
        | TaskStatus::Repairing => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled | TaskStatus::Skipped => "cancelled",
    }
}

fn task_status_text(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Draft => "draft",
        TaskStatus::Ready => "ready",
        TaskStatus::Running => "running",
        TaskStatus::AwaitingApproval => "awaiting_approval",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Verifying => "verifying",
        TaskStatus::Repairing => "repairing",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Skipped => "skipped",
    }
}

fn current_turn_status_accepts_task_status_item(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pending"
            | "queued"
            | "running"
            | "started"
            | "streaming"
            | "blocked"
            | "awaiting_approval"
            | "review_required"
            | "repairing"
            | "verifying"
    )
}

pub fn publish_task_status_turn_item_for_active_sessions(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    task: &Task,
    new_status: TaskStatus,
) {
    for sidecar in session_store.active_execution_sidecars() {
        let Some(turn) = sidecar.current_turn.as_ref() else {
            continue;
        };
        if !current_turn_status_accepts_task_status_item(&turn.status) {
            continue;
        }
        let active_chain_matches = sidecar
            .active_execution_chain
            .as_ref()
            .is_some_and(|chain| {
                chain.root_task_id == task.root_task_id
                    || chain.root_task_id == task.task_id
                    || chain
                        .active_branch_task_ids
                        .iter()
                        .any(|task_id| task_id == &task.task_id)
            });
        let turn_matches = turn
            .items
            .iter()
            .any(|item| item.task_id.as_ref() == Some(&task.task_id))
            || turn
                .worker_lanes
                .iter()
                .any(|lane| lane.task_id == task.task_id);
        if !active_chain_matches && !turn_matches {
            continue;
        }

        let lane = turn
            .worker_lanes
            .iter()
            .find(|lane| lane.task_id == task.task_id);
        // task_status item 归属其对应的 thread：若存在 worker lane 则取 lane 的 thread,
        // 否则落到 session 的 orchestrator thread（主线兜底）。
        let source_thread_id = match lane {
            Some(lane) => lane.thread_id.clone(),
            None => match session_store.orchestrator_thread_for_session(&sidecar.session_id) {
                Some(thread) => thread.thread_id,
                None => continue,
            },
        };
        let item_id = format!("turn-item-task-status-{}-{}", turn.turn_id, task.task_id);
        let mut item = session_turn_item(
            "task_status",
            turn_item_status_for_task_status(new_status),
            Some(task.title.clone()),
            Some(format!("{}：{}", task.title, task_status_text(new_status))),
            Some(item_id),
            source_thread_id,
        );
        item.source = "task".to_string();
        item.task_id = Some(task.task_id.clone());
        item.role_id = task
            .executor_binding
            .as_ref()
            .map(|binding| binding.target_role.clone())
            .filter(|role_id| !role_id.trim().is_empty());
        if let Some(lane) = lane {
            item.lane_id = Some(lane.lane_id.clone());
            item.lane_seq = Some(lane.lane_seq);
            item.worker_id = Some(lane.worker_id.clone());
        }
        if let Some(published) = append_session_turn_item_with_task_store(
            session_store,
            &sidecar.session_id,
            item,
            task_store,
        ) {
            let workspace_id = sidecar
                .active_execution_chain
                .as_ref()
                .and_then(|chain| chain.workspace_id.clone());
            publish_session_turn_item_event(
                event_bus,
                &sidecar.session_id,
                &workspace_id,
                &published,
            );
        }
    }
}

const TASK_CONTEXT_MAX_CHARS: usize = 4000;
const TASK_CONTEXT_MAX_REFS: usize = 8;
const ROOT_COMPLETION_SUMMARY_MAX_CHARS: usize = 2400;

fn compact_task_context_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TASK_CONTEXT_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut compact = trimmed
        .chars()
        .take(TASK_CONTEXT_MAX_CHARS)
        .collect::<String>();
    compact.push_str("…[truncated]");
    compact
}

fn format_task_ref_list(refs: &[String]) -> String {
    if refs.is_empty() {
        return "无".to_string();
    }
    let mut formatted = refs
        .iter()
        .take(TASK_CONTEXT_MAX_REFS)
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, compact_task_context_text(item)))
        .collect::<Vec<_>>();
    let remaining = refs.len().saturating_sub(TASK_CONTEXT_MAX_REFS);
    if remaining > 0 {
        formatted.push(format!("... (+{remaining} more)"));
    }
    formatted.join("\n")
}

fn format_dependency_task_context(dependency: &Task) -> String {
    format!(
        "[dependency-task]\nid: {}\nkind: {:?}\nstatus: {:?}\ntitle: {}\ngoal: {}\noutput_refs:\n{}\nevidence_refs:\n{}",
        dependency.task_id,
        dependency.kind,
        dependency.status,
        compact_task_context_text(&dependency.title),
        compact_task_context_text(&dependency.goal),
        format_task_ref_list(&dependency.output_refs),
        format_task_ref_list(&dependency.evidence_refs)
    )
}

/// 主线 assistant_final 扫描：只认 `source_thread_id == orchestrator_thread_id` 的 item。
fn latest_orchestrator_assistant_final(
    turn: &magi_session_store::ActiveExecutionTurn,
    orchestrator_thread_id: &magi_core::ThreadId,
) -> Option<(String, String)> {
    turn.items
        .iter()
        .filter(|item| {
            item.kind == "assistant_final" && &item.source_thread_id == orchestrator_thread_id
        })
        .filter_map(|item| {
            let content = item.content.as_ref()?.trim();
            if content.is_empty() {
                return None;
            }
            Some((item.item_seq, content.to_string(), item.item_id.clone()))
        })
        .max_by_key(|(item_seq, _, _)| *item_seq)
        .map(|(_, content, item_id)| (content, item_id))
}

fn compact_root_completion_summary(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= ROOT_COMPLETION_SUMMARY_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut compact = trimmed
        .chars()
        .take(ROOT_COMPLETION_SUMMARY_MAX_CHARS)
        .collect::<String>();
    compact.push('…');
    compact
}

fn completion_summary_rank(task: &Task) -> u8 {
    match task.kind {
        TaskKind::Action => 5,
        TaskKind::Repair => 4,
        TaskKind::WorkPackage | TaskKind::Phase => 3,
        TaskKind::Validation => 2,
        TaskKind::Objective => 1,
        TaskKind::Decision => 0,
    }
}

fn strip_known_delivery_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.strip_prefix(prefix)
        .map(str::trim)
        .filter(|rest| !rest.is_empty())
}

fn text_from_structured_task_output(output: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(output.trim()).ok()?;
    let blocks = value.get("blocks")?.as_array()?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(|value| value.as_str()) == Some("text"))
        .filter_map(|block| block.get("content").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn normalize_root_completion_output(output: &str) -> Option<String> {
    let source = text_from_structured_task_output(output).unwrap_or_else(|| output.to_string());
    let normalized = source.replace('\r', "");
    let mut lines = Vec::new();
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("修改的文件列表") || trimmed.starts_with("关键代码片段")
        {
            break;
        }
        if trimmed.starts_with("验证已完成，交付如下")
            || trimmed.starts_with("交付如下")
            || trimmed.starts_with("已完成多端稳定性只读验证")
        {
            continue;
        }
        let trimmed = trimmed
            .strip_prefix("主线汇总：")
            .or_else(|| trimmed.strip_prefix("主线总结："))
            .unwrap_or(trimmed)
            .trim();
        if trimmed.is_empty() || trimmed == "无" || trimmed == "- 无" {
            continue;
        }
        lines.push(trimmed);
    }

    let mut text = lines.join("\n");
    if text.is_empty() {
        text = output.trim().to_string();
    }
    text = text
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '“' | '”'))
        .trim()
        .to_string();

    for marker in [
        "一句自然语言总结：",
        "自然语言总结：",
        "最终结论：",
        "关键验证结果：",
    ] {
        if let Some(index) = text.rfind(marker) {
            text = text[index + marker.len()..].trim().to_string();
            break;
        }
    }
    while let Some(rest) = text.strip_prefix("- ").map(str::trim) {
        text = rest.to_string();
    }

    if text.starts_with("目标：") && text.contains("边界：") && text.contains("验收标准：")
    {
        return None;
    }

    loop {
        let before = text.clone();
        if let Some(rest) = strip_known_delivery_prefix(&text, "通过。") {
            text = rest.to_string();
        }
        if let Some(rest) = strip_known_delivery_prefix(&text, "通过：") {
            text = rest.to_string();
        }
        if let Some(rest) = strip_known_delivery_prefix(&text, "验收结论：") {
            text = rest.to_string();
        }
        if let Some(rest) = strip_known_delivery_prefix(&text, "最终结论：") {
            text = rest.to_string();
        }
        if let Some(rest) =
            strip_known_delivery_prefix(&text, "当前交付已基于执行产出完成验证，且未重复执行工具；")
        {
            text = rest.to_string();
        }
        if let Some(rest) = strip_known_delivery_prefix(&text, "证据显示") {
            text = rest.to_string();
        }
        if let Some(rest) = strip_known_delivery_prefix(&text, "验证通过：") {
            text = format!("已验证：{rest}");
        }
        if text == before {
            break;
        }
    }

    let text = compact_root_completion_summary(&text);
    (!text.trim().is_empty()).then_some(text)
}

fn root_completion_outputs(task_store: &TaskStore, root_task: &Task) -> Vec<String> {
    let root_outputs = root_task
        .output_refs
        .iter()
        .filter_map(|output| normalize_root_completion_output(output))
        .collect::<Vec<_>>();
    if !root_outputs.is_empty() {
        return root_outputs;
    }

    let mut candidates = task_store
        .get_tasks_by_mission(&root_task.mission_id)
        .into_iter()
        .filter(|task| task.root_task_id == root_task.task_id)
        .filter(|task| task.task_id != root_task.task_id)
        .filter(|task| task.status == TaskStatus::Completed)
        .filter(|task| {
            task.output_refs
                .iter()
                .any(|output| !output.trim().is_empty())
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|task| (completion_summary_rank(task), task.updated_at.0));

    for task in candidates.into_iter().rev() {
        let mut outputs = Vec::new();
        for output in task.output_refs {
            let Some(summary) = normalize_root_completion_output(&output) else {
                continue;
            };
            if outputs.iter().any(|existing| existing == &summary) {
                continue;
            }
            outputs.push(summary);
            if outputs.len() >= 3 {
                break;
            }
        }
        if !outputs.is_empty() {
            return outputs;
        }
    }
    Vec::new()
}

fn format_root_completion_summary(outputs: &[String]) -> String {
    match outputs {
        [] => "已完成。详细步骤和工具记录已保留在任务卡里。".to_string(),
        [only] => format!("已完成：{only}\n\n详细步骤和工具记录已保留在任务卡里。"),
        many => {
            let bullets = many
                .iter()
                .map(|output| {
                    let single_line = output
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("- {single_line}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "已完成，关键结果是：\n\n{bullets}\n\n详细步骤和工具记录已保留在任务卡里。"
            )
        }
    }
}

fn build_root_completion_summary(task_store: &TaskStore, root_task: &Task) -> String {
    let outputs = root_completion_outputs(task_store, root_task);
    format_root_completion_summary(&outputs)
}

fn ensure_root_completion_final_item(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    root_task: &Task,
    task_store: &TaskStore,
) -> Option<(String, String)> {
    let sidecar = state.session_store.runtime_sidecar(session_id)?;
    let turn = sidecar.current_turn.as_ref()?;
    // session 一生一 mission：root completion 阶段必须存在 orchestrator thread。
    let orchestrator_thread = state
        .session_store
        .orchestrator_thread_for_session(session_id)?;
    if let Some(response) = latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id) {
        return Some(response);
    }

    let item_id = format!("turn-item-orchestrator-final-{}", root_task.task_id);
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("任务完成".to_string()),
        Some(build_root_completion_summary(task_store, root_task)),
        Some(item_id),
        orchestrator_thread.thread_id.clone(),
    );
    final_item.source = "orchestrator".to_string();
    final_item.task_id = Some(root_task.task_id.clone());

    if let Some(published) = append_session_turn_item_with_task_store(
        state.session_store.as_ref(),
        session_id,
        final_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(&state.event_bus, session_id, workspace_id, &published);
    }

    state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .as_ref()
        .and_then(|turn| latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id))
}

pub fn finalize_background_session_task_turn_if_root_completed(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> bool {
    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return false;
    };
    if root_task.status != TaskStatus::Completed {
        return false;
    }

    let Some(sidecar) = state.session_store.runtime_sidecar(session_id) else {
        return false;
    };
    let Some(active_chain) = sidecar.active_execution_chain.as_ref() else {
        return false;
    };
    if active_chain.root_task_id != *root_task_id {
        return false;
    }
    let workspace_id = active_chain.workspace_id.clone();
    let Some(turn) = sidecar.current_turn.as_ref() else {
        return false;
    };
    let Some(orchestrator_thread) = state
        .session_store
        .orchestrator_thread_for_session(session_id)
    else {
        return false;
    };
    let response =
        latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id).or_else(|| {
            ensure_root_completion_final_item(state, session_id, &workspace_id, &root_task, task_store)
        });
    let event_item_id = response
        .as_ref()
        .map(|(_, item_id)| item_id.clone())
        .or_else(|| terminal_turn_event_anchor_item_id(turn, &orchestrator_thread.thread_id));
    let Some(event_item_id) = event_item_id else {
        return false;
    };

    if update_current_turn_completed_from_root(state, session_id).is_err() {
        return false;
    }
    publish_current_session_turn_item_event(
        &state.event_bus,
        state.session_store.as_ref(),
        session_id,
        &workspace_id,
        &event_item_id,
        state.task_store(),
    );
    if let Some((response_text, _)) = response {
        let _ = state.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-message-assistant-{}", UtcMillis::now().0)),
                "message.created",
                serde_json::json!({
                    "session_id": session_id.to_string(),
                    "role": "assistant",
                    "content": response_text,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                ..EventContext::default()
            }),
        );
    }
    true
}

fn update_current_turn_completed_from_root(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), ()> {
    match state
        .session_store
        .complete_current_turn_from_completed_root_task(session_id)
        .map_err(|_| ())?
    {
        Some(_) => Ok(()),
        None => Err(()),
    }
}

fn terminal_turn_event_anchor_item_id(
    turn: &magi_session_store::ActiveExecutionTurn,
    orchestrator_thread_id: &magi_core::ThreadId,
) -> Option<String> {
    turn.items
        .iter()
        .filter(|item| &item.source_thread_id == orchestrator_thread_id)
        .max_by_key(|item| item.item_seq)
        .or_else(|| turn.items.iter().max_by_key(|item| item.item_seq))
        .map(|item| item.item_id.clone())
}

pub fn finalize_background_session_task_turn_if_root_terminal(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &TaskId,
    runner_status: &str,
) -> bool {
    if finalize_background_session_task_turn_if_root_completed(state, session_id, root_task_id) {
        return true;
    }

    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return false;
    };
    let (turn_status, message) = match root_task.status {
        TaskStatus::Failed => ("failed", "任务执行失败，未生成最终回复。"),
        TaskStatus::Blocked => ("blocked", "任务执行需要处理，等待进一步操作。"),
        TaskStatus::Cancelled => ("cancelled", "任务执行已取消。"),
        _ if runner_status == "error" => ("failed", "任务执行异常，未生成最终回复。"),
        _ if runner_status == "blocked" => ("blocked", "任务执行需要处理，等待进一步操作。"),
        _ if runner_status == "stopped" || runner_status == "cancelled" => {
            ("cancelled", "任务执行已取消。")
        }
        _ => return false,
    };

    let Some(sidecar) = state.session_store.runtime_sidecar(session_id) else {
        return false;
    };
    let Some(active_chain) = sidecar.active_execution_chain.as_ref() else {
        return false;
    };
    if active_chain.root_task_id != *root_task_id {
        return false;
    }
    let workspace_id = active_chain.workspace_id.clone();
    if sidecar
        .current_turn
        .as_ref()
        .is_some_and(|turn| current_turn_status_is_terminal(&turn.status))
    {
        return true;
    }
    // session 一生一 mission：终态写 assistant_error 必须存在 orchestrator thread。
    let Some(orchestrator_thread) = state
        .session_store
        .orchestrator_thread_for_session(session_id)
    else {
        return false;
    };
    if sidecar.current_turn.as_ref().is_some_and(|turn| {
        turn.status == turn_status
            && turn.items.iter().any(|item| {
                item.kind == "assistant_error"
                    && item.source_thread_id == orchestrator_thread.thread_id
            })
    }) {
        return true;
    }

    if state
        .session_store
        .update_current_turn_status(session_id, turn_status)
        .is_err()
    {
        return false;
    }

    let item_id = format!("turn-item-assistant-error-{}", UtcMillis::now().0);
    let mut error_item = session_turn_item(
        "assistant_error",
        turn_status,
        Some("任务执行未完成".to_string()),
        Some(message.to_string()),
        Some(item_id.clone()),
        orchestrator_thread.thread_id.clone(),
    );
    error_item.task_id = Some(root_task_id.clone());
    if let Some(published) = append_session_turn_item_with_task_store(
        state.session_store.as_ref(),
        session_id,
        error_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(&state.event_bus, session_id, &workspace_id, &published);
    }

    true
}

pub fn reconcile_terminal_session_task_turns(state: &ApiState) -> usize {
    let Some(task_store) = state.task_store() else {
        return 0;
    };
    let candidates = state
        .session_store
        .runtime_sidecars()
        .into_iter()
        .filter_map(|sidecar| {
            let turn = sidecar.current_turn.as_ref()?;
            let chain = sidecar.active_execution_chain.as_ref()?;
            let root_task = task_store.get_task(&chain.root_task_id)?;
            let runner_status = runner_status_for_terminal_task(root_task.status)?;
            if runner_status == "completed" {
                if current_turn_status_is_completed(&turn.status) {
                    return None;
                }
            } else if current_turn_status_is_terminal(&turn.status) {
                return None;
            }
            Some((
                sidecar.session_id.clone(),
                chain.root_task_id.clone(),
                runner_status,
            ))
        })
        .collect::<Vec<_>>();

    candidates
        .into_iter()
        .filter(|(session_id, root_task_id, runner_status)| {
            finalize_background_session_task_turn_if_root_terminal(
                state,
                session_id,
                root_task_id,
                runner_status,
            )
        })
        .count()
}

fn current_turn_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "blocked"
            | "cancelled"
            | "canceled"
    )
}

fn current_turn_status_is_completed(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "complete" | "succeeded" | "success"
    )
}

fn runner_status_for_terminal_task(status: TaskStatus) -> Option<&'static str> {
    match status {
        TaskStatus::Completed => Some("completed"),
        TaskStatus::Failed => Some("error"),
        TaskStatus::Blocked => Some("blocked"),
        TaskStatus::Cancelled => Some("cancelled"),
        _ => None,
    }
}

#[derive(Clone, Default)]
pub struct TaskExecutionRegistry {
    plans: Arc<RwLock<HashMap<TaskId, TaskExecutionPlan>>>,
}

impl TaskExecutionRegistry {
    pub fn insert(&self, task_id: TaskId, plan: TaskExecutionPlan) {
        self.plans
            .write()
            .expect("task execution registry write lock poisoned")
            .insert(task_id, plan);
    }

    pub fn remove(&self, task_id: &TaskId) -> Option<TaskExecutionPlan> {
        self.plans
            .write()
            .expect("task execution registry write lock poisoned")
            .remove(task_id)
    }
}

#[derive(Clone)]
pub struct LlmTaskDispatcher {
    event_bus: Arc<InMemoryEventBus>,
    pipeline: ExecutionPipeline,
    session_store: Arc<SessionStore>,
    execution_registry: TaskExecutionRegistry,
    result_receiver: Arc<EventBasedResultReceiver>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    knowledge_store: Option<Arc<KnowledgeStore>>,
    knowledge_persist_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    settings_store: Option<Arc<crate::settings_store::SettingsStore>>,
    context_runtime: Option<Arc<ContextRuntime>>,
    workspace_registry: Option<Arc<WorkspaceStore>>,
    tool_registry: Option<ToolRegistry>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    snapshot_manager: Option<Arc<magi_snapshot::SnapshotManager>>,
    /// Task System v2：Conversation 注册中心，承载 Turn 状态机与单 Conversation 不并发不变式。
    conversation_registry: Option<Arc<ConversationRegistry>>,
    /// Task System v2：统一 StreamEvent 派生通道。
    stream_fanout: Option<Arc<StreamFanOut>>,
    /// Task System v2：AgentRole 注册表（来自 ApiState，注入到 task_llm_loop）。
    agent_role_registry: Option<Arc<magi_agent_role::AgentRoleRegistry>>,
    /// Task System v2 — L5：父子任务拓扑图。S7 协调器三件套（agent_spawn / send_message /
    /// task_stop）需要在 task_llm_loop 中读写。设计为构造期必填，避免运行期再做空检查。
    spawn_graph: Arc<std::sync::Mutex<magi_spawn_graph::SpawnGraph>>,
    /// Task System v2 — L13：session 维度的 TodoLedger 索引。S9 中模型通过
    /// `todo_write` 工具往这里写分解 + 进度；下一轮 Turn 起始时把快照注入 system prompt。
    todo_ledger_registry: Arc<magi_todo_ledger::TodoLedgerRegistry>,
    /// Task System v2 — L14：workspace 维度的 ProjectMemory 索引。S10 中模型通过
    /// `memory_write` 工具新增/删除项目记忆条目；每次 Turn 起始把 MEMORY.md 视图注入
    /// system prompt，跨 conversation 复用。
    project_memory_registry: Arc<magi_project_memory::ProjectMemoryRegistry>,
    /// Task System v2 — Tier 4 / L11：workspace 维度的 MissionCharter 索引。S11 中模型
    /// 通过 `mission_charter_write` 工具增量更新 mission 宪章；每次 Turn 起始把当前
    /// mission 的 charter 注入 system prompt，跨 conversation 锚定目标契约。
    mission_charter_registry: Arc<magi_mission_charter::MissionCharterRegistry>,
    /// Task System v2 — Tier 4 / L12：workspace 维度的 Plan 索引。S12 中模型通过
    /// `plan_write` 工具整体替换 mission.plan.steps；每次 Turn 起始把当前 plan
    /// 注入 system prompt，长链路推进时保留计划上下文。
    plan_registry: Arc<magi_plan::PlanRegistry>,
    /// Task System v2 — Tier 4 / L13：workspace 维度的 MissionWorkspace 索引。S13
    /// 中每个 Mission 拥有独占的 artifacts/logs/memory 目录骨架；Turn 起始时把目录
    /// 路径注入 system prompt，让 agent 把产物落在 mission 内而不是无主目录。
    mission_workspace_registry: Arc<magi_mission_workspace::MissionWorkspaceRegistry>,
    /// Task System v2 — Tier 4 / L18：workspace 维度的 KnowledgeGraph 索引。S14
    /// 中每个 Mission 累积"已知事实"（symbols / decisions / risks）；Turn 起始时把
    /// live facts 注入 system prompt，避免长 mission 中模型重新讨论已经达成的结论。
    knowledge_graph_registry: Arc<magi_knowledge_graph::KnowledgeGraphRegistry>,
    /// Task System v2 — Tier 4 / L19：workspace 维度的 ValidationRunner 索引。S15
    /// 中每个 Mission 在 Plan 节点上挂载验证记录（test_suite / type_check /
    /// integration_smoke / benchmark）；Coordinator 判定 Plan 节点完成的硬门槛
    /// 是：至少 1 条 Pass，且当前无 Fail。
    validation_runner_registry: Arc<magi_validation_runner::ValidationRunnerRegistry>,
    /// Task System v2 — Tier 4 / L20：workspace 维度的 Checkpoint 索引。S16 中每个
    /// Mission 维护一份 append-only 的检查点日志（process_restart / context_compaction
    /// / phase_transition / manual），让事后能定位到“恢复到 Tn”所需要的最小语义快照。
    checkpoint_registry: Arc<magi_checkpoint::CheckpointRegistry>,
    /// Task System v2 — Tier 4 / L21：workspace 维度的 HumanCheckpoint 索引。S17 中
    /// orchestrator 通过 human_checkpoint_request 申请人工审核点，mission 会进入
    /// awaiting_human 状态，operator 审批前 Coordinator 不再派发新工作。
    human_checkpoint_registry: Arc<magi_human_checkpoint::HumanCheckpointRegistry>,
    /// 强制同步执行 dispatch，用于普通模式的同步 for 循环（设计 §1.3）。
    force_sync_dispatch: Arc<std::sync::atomic::AtomicUsize>,
}

pub fn resolve_configured_model_client(
    settings_store: Option<&Arc<SettingsStore>>,
    fallback: Option<Arc<dyn ModelBridgeClient>>,
) -> Option<Arc<dyn ModelBridgeClient>> {
    if let Some(store) = settings_store {
        let config = store.get_section("auxiliary");
        let normalized = NormalizedModelConfig::from_settings_value(&config, "openai");
        if let Some(client) = normalized.to_http_model_client("gpt-4") {
            return Some(Arc::new(client));
        }
    }
    fallback
}

impl LlmTaskDispatcher {
    pub fn new(
        event_bus: Arc<InMemoryEventBus>,
        pipeline: ExecutionPipeline,
        session_store: Arc<SessionStore>,
        execution_registry: TaskExecutionRegistry,
        result_receiver: Arc<EventBasedResultReceiver>,
        spawn_graph: Arc<std::sync::Mutex<magi_spawn_graph::SpawnGraph>>,
    ) -> Self {
        Self {
            event_bus,
            pipeline,
            session_store,
            execution_registry,
            result_receiver,
            model_bridge_client: None,
            knowledge_store: None,
            knowledge_persist_callback: None,
            settings_store: None,
            context_runtime: None,
            workspace_registry: None,
            tool_registry: None,
            skill_runtime: None,
            snapshot_manager: None,
            conversation_registry: None,
            stream_fanout: None,
            agent_role_registry: None,
            spawn_graph,
            todo_ledger_registry: Arc::new(magi_todo_ledger::TodoLedgerRegistry::new()),
            project_memory_registry: Arc::new(magi_project_memory::ProjectMemoryRegistry::new()),
            mission_charter_registry: Arc::new(magi_mission_charter::MissionCharterRegistry::new()),
            plan_registry: Arc::new(magi_plan::PlanRegistry::new()),
            mission_workspace_registry: Arc::new(
                magi_mission_workspace::MissionWorkspaceRegistry::new(),
            ),
            knowledge_graph_registry: Arc::new(
                magi_knowledge_graph::KnowledgeGraphRegistry::new(),
            ),
            validation_runner_registry: Arc::new(
                magi_validation_runner::ValidationRunnerRegistry::new(),
            ),
            checkpoint_registry: Arc::new(magi_checkpoint::CheckpointRegistry::new()),
            human_checkpoint_registry: Arc::new(
                magi_human_checkpoint::HumanCheckpointRegistry::new(),
            ),
            force_sync_dispatch: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn set_force_sync_dispatch(&self, force: bool) {
        if force {
            self.force_sync_dispatch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return;
        }
        let _ = self.force_sync_dispatch.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |current| Some(current.saturating_sub(1)),
        );
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_knowledge_store(mut self, store: Arc<KnowledgeStore>) -> Self {
        self.knowledge_store = Some(store);
        self
    }

    pub fn with_knowledge_persist_callback(
        mut self,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.knowledge_persist_callback = Some(callback);
        self
    }

    pub fn with_settings_store(mut self, store: Arc<crate::settings_store::SettingsStore>) -> Self {
        self.settings_store = Some(store);
        self
    }

    pub fn with_context_runtime(mut self, runtime: Arc<ContextRuntime>) -> Self {
        self.context_runtime = Some(runtime);
        self
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceStore>) -> Self {
        self.workspace_registry = Some(registry);
        self
    }

    pub fn with_tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn with_skill_runtime(mut self, runtime: Arc<magi_skill_runtime::SkillRuntime>) -> Self {
        self.skill_runtime = Some(runtime);
        self
    }

    pub fn with_snapshot_manager(mut self, manager: Arc<magi_snapshot::SnapshotManager>) -> Self {
        self.snapshot_manager = Some(manager);
        self
    }

    pub fn with_conversation_registry(mut self, registry: Arc<ConversationRegistry>) -> Self {
        self.conversation_registry = Some(registry);
        self
    }

    pub fn with_stream_fanout(mut self, fanout: Arc<StreamFanOut>) -> Self {
        self.stream_fanout = Some(fanout);
        self
    }

    pub fn with_agent_role_registry(
        mut self,
        registry: Arc<magi_agent_role::AgentRoleRegistry>,
    ) -> Self {
        self.agent_role_registry = Some(registry);
        self
    }

    pub fn with_todo_ledger_registry(
        mut self,
        registry: Arc<magi_todo_ledger::TodoLedgerRegistry>,
    ) -> Self {
        self.todo_ledger_registry = registry;
        self
    }

    pub fn todo_ledger_registry(&self) -> Arc<magi_todo_ledger::TodoLedgerRegistry> {
        self.todo_ledger_registry.clone()
    }

    pub fn with_project_memory_registry(
        mut self,
        registry: Arc<magi_project_memory::ProjectMemoryRegistry>,
    ) -> Self {
        self.project_memory_registry = registry;
        self
    }

    pub fn project_memory_registry(&self) -> Arc<magi_project_memory::ProjectMemoryRegistry> {
        self.project_memory_registry.clone()
    }

    pub fn with_mission_charter_registry(
        mut self,
        registry: Arc<magi_mission_charter::MissionCharterRegistry>,
    ) -> Self {
        self.mission_charter_registry = registry;
        self
    }

    pub fn mission_charter_registry(
        &self,
    ) -> Arc<magi_mission_charter::MissionCharterRegistry> {
        self.mission_charter_registry.clone()
    }

    pub fn with_plan_registry(mut self, registry: Arc<magi_plan::PlanRegistry>) -> Self {
        self.plan_registry = registry;
        self
    }

    pub fn plan_registry(&self) -> Arc<magi_plan::PlanRegistry> {
        self.plan_registry.clone()
    }

    pub fn with_mission_workspace_registry(
        mut self,
        registry: Arc<magi_mission_workspace::MissionWorkspaceRegistry>,
    ) -> Self {
        self.mission_workspace_registry = registry;
        self
    }

    pub fn mission_workspace_registry(
        &self,
    ) -> Arc<magi_mission_workspace::MissionWorkspaceRegistry> {
        self.mission_workspace_registry.clone()
    }

    pub fn with_knowledge_graph_registry(
        mut self,
        registry: Arc<magi_knowledge_graph::KnowledgeGraphRegistry>,
    ) -> Self {
        self.knowledge_graph_registry = registry;
        self
    }

    pub fn knowledge_graph_registry(
        &self,
    ) -> Arc<magi_knowledge_graph::KnowledgeGraphRegistry> {
        self.knowledge_graph_registry.clone()
    }

    pub fn with_validation_runner_registry(
        mut self,
        registry: Arc<magi_validation_runner::ValidationRunnerRegistry>,
    ) -> Self {
        self.validation_runner_registry = registry;
        self
    }

    pub fn validation_runner_registry(
        &self,
    ) -> Arc<magi_validation_runner::ValidationRunnerRegistry> {
        self.validation_runner_registry.clone()
    }

    pub fn with_checkpoint_registry(
        mut self,
        registry: Arc<magi_checkpoint::CheckpointRegistry>,
    ) -> Self {
        self.checkpoint_registry = registry;
        self
    }

    pub fn checkpoint_registry(&self) -> Arc<magi_checkpoint::CheckpointRegistry> {
        self.checkpoint_registry.clone()
    }

    pub fn with_human_checkpoint_registry(
        mut self,
        registry: Arc<magi_human_checkpoint::HumanCheckpointRegistry>,
    ) -> Self {
        self.human_checkpoint_registry = registry;
        self
    }

    pub fn human_checkpoint_registry(
        &self,
    ) -> Arc<magi_human_checkpoint::HumanCheckpointRegistry> {
        self.human_checkpoint_registry.clone()
    }

    fn publish_task_dispatched_event(
        &self,
        task_id: &TaskId,
        mission_id: &magi_core::MissionId,
        worker: &WorkerInfo,
        lease_id: &LeaseId,
        kind: magi_core::TaskKind,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
    ) {
        let event = EventEnvelope::domain(
            EventId::new(format!("event-task-dispatched-{}", UtcMillis::now().0)),
            "task.dispatched",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": mission_id.to_string(),
                "session_id": session_id.map(ToString::to_string),
                "workspace_id": workspace_id.map(ToString::to_string),
                "worker_id": worker.worker_id.to_string(),
                "role": worker.role,
                "lease_id": lease_id.to_string(),
                "kind": format!("{:?}", kind),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: session_id.cloned(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn push_result(&self, task_id: &TaskId, lease_id: &LeaseId, outcome: TaskOutcome) {
        self.result_receiver.push_result(TaskResult {
            task_id: task_id.clone(),
            lease_id: lease_id.clone(),
            outcome,
        });
    }

    /// S7-D：LocalBash 变体直接走 ShellExec，绕过 LLM 循环 / agent role / prompt 组装。
    /// 失败原因有两类：tool_registry 缺失（架构破坏，应 panic 一致行为）或
    /// shell 退出非零（作为 TaskOutcome::Failed 上报，留 payload 给主线核查）。
    fn execute_local_bash_variant(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        command: &str,
        working_dir: Option<&str>,
        worker_id: Option<&WorkerId>,
    ) -> TaskOutcome {
        let Some(registry) = self.tool_registry.as_ref() else {
            return TaskOutcome::Failed {
                error: format!(
                    "LocalBash task {} 无法执行：ToolRegistry 未配置",
                    task.task_id
                ),
            };
        };
        let mut payload = serde_json::json!({ "command": command });
        if let Some(dir) = working_dir {
            payload["working_dir"] = serde_json::Value::String(dir.to_string());
        }
        let input = ToolExecutionInput {
            tool_call_id: ToolCallId::new(format!("local-bash-{}", task.task_id)),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: payload.to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Medium,
        };
        let context = ToolExecutionContext {
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            working_directory: working_dir.map(PathBuf::from),
        };
        let output = registry.execute_with_context(input, context);
        match output.status {
            ExecutionResultStatus::Succeeded => TaskOutcome::Completed {
                output_refs: vec![output.payload],
            },
            other => TaskOutcome::Failed {
                error: format!(
                    "LocalBash task {} shell_exec 失败 (status={:?})：{}",
                    task.task_id, other, output.payload
                ),
            },
        }
    }

    fn execute_dispatch_plan(
        &self,
        task: &magi_core::Task,
        task_id: &TaskId,
        lease_id: &LeaseId,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ownership: ExecutionOwnership,
        writebacks: ExecutionWritebackPlans,
        use_tools: bool,
        skill_name: Option<String>,
        usage_binding: ModelUsageBinding,
        worker_lane_id: Option<String>,
        worker_lane_seq: Option<usize>,
        worker_id: WorkerId,
        thread_id: magi_core::ThreadId,
        system_prompt: Option<String>,
    ) {
        // 仅在有 writebacks 时（即主 action task）才生成 streaming entry_id。
        // sub-task 的 writebacks 为空，不需要在 timeline 中创建流式条目。
        let streaming_entry_id = if writebacks.is_empty() {
            None
        } else {
            Some(format!("timeline-streaming-{}", task.task_id))
        };
        // S7-D：LocalBash 变体直接走 ShellExec，绕过 LLM 循环。
        if let magi_core::TaskVariant::LocalBash {
            command,
            working_dir,
        } = &task.variant
        {
            let outcome = self.execute_local_bash_variant(
                task,
                &session_id,
                &workspace_id,
                command,
                working_dir.as_deref(),
                Some(&worker_id),
            );
            if matches!(&outcome, TaskOutcome::Completed { .. }) {
                self.session_store
                    .bind_execution_ownership(session_id.clone(), ownership);
                writebacks.apply(&self.pipeline.memory_store);
            }
            self.push_result(task_id, lease_id, outcome);
            return;
        }
        let (outcome, context_summary) = self.invoke_llm_with_tools(
            task,
            task_id,
            lease_id,
            &session_id,
            &workspace_id,
            use_tools,
            skill_name,
            &usage_binding,
            streaming_entry_id.as_deref(),
            worker_lane_id.as_deref(),
            worker_lane_seq,
            Some(&worker_id),
            &thread_id,
            system_prompt,
        );
        if matches!(&outcome, TaskOutcome::Completed { .. }) {
            self.session_store
                .bind_execution_ownership(session_id.clone(), ownership);
            let should_extract_knowledge = !writebacks.is_empty();
            writebacks.apply(&self.pipeline.memory_store);
            if should_extract_knowledge {
                self.extract_and_persist_knowledge(&session_id, &workspace_id, &outcome);
            }
            self.publish_execution_overview(task, &session_id, &workspace_id, context_summary);
        }
        self.push_result(task_id, lease_id, outcome);
    }

    fn extract_and_persist_knowledge(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        outcome: &TaskOutcome,
    ) {
        let Some(store) = self.knowledge_store.as_ref() else {
            return;
        };
        let TaskOutcome::Completed { output_refs } = outcome else {
            return;
        };

        let timeline_text = self
            .session_store
            .timeline_for_session(session_id)
            .into_iter()
            .rev()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
                )
            })
            .take(12)
            .filter_map(|entry| timeline_entry_visible_text(&entry.message))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n\n");
        let output_text = output_refs.join("\n\n");
        let extraction_text = format!("{timeline_text}\n\n{output_text}");
        let learnings = extract_learning_candidates(&extraction_text);
        if learnings.is_empty() {
            return;
        }

        let existing = store.list();
        let mut inserted = 0usize;
        for (index, learning) in learnings.into_iter().enumerate() {
            if knowledge_duplicate(
                &existing,
                KnowledgeKind::Learning,
                workspace_id.as_ref(),
                &learning.content,
            ) {
                continue;
            }
            let now = UtcMillis::now();
            store.upsert(KnowledgeRecord {
                knowledge_id: format!("learning-auto-{}-{index}", now.0),
                kind: KnowledgeKind::Learning,
                title: title_from_learning_content(&learning.content),
                content: learning.content,
                tags: learning.tags,
                workspace_id: workspace_id.clone(),
                source_ref: Some(
                    learning
                        .context
                        .unwrap_or_else(|| format!("session:{}", session_id.as_str())),
                ),
                updated_at: now,
            });
            inserted += 1;
        }
        if inserted > 0 {
            if let Some(callback) = self.knowledge_persist_callback.as_ref() {
                callback();
            }
        }
    }

    fn publish_execution_overview(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        context_summary: Option<ExecutionContextSummary>,
    ) {
        let context_payload = context_summary
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let event = EventEnvelope::audit(
            EventId::new(format!("event-mission-overview-{}", UtcMillis::now().0)),
            "mission.execution.overview",
            serde_json::json!({
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "context": context_payload,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn build_tool_definitions(&self, task: Option<&magi_core::Task>) -> Vec<ChatToolDefinition> {
        let Some(ref registry) = self.tool_registry else {
            return Vec::new();
        };
        if task
            .and_then(|task| task.policy_snapshot.as_ref())
            .is_some_and(|policy| policy.command_mode.eq_ignore_ascii_case("no_tools"))
        {
            return Vec::new();
        }
        let registry = if let Some(policy) = task.and_then(|task| task.policy_snapshot.as_ref()) {
            registry.filtered_clone(&policy.allowed_tools, &policy.denied_tools)
        } else {
            registry.clone()
        };
        let mut definitions = public_builtin_tool_definitions(&registry)
            .into_iter()
            .filter(|definition| definition.function.name != SKILL_APPLY_TOOL_NAME)
            .collect::<Vec<_>>();
        if self.skill_runtime.is_some() {
            definitions.push(skill_apply_tool_definition());
        }
        definitions
    }

    fn resolve_workspace_root_path(&self, workspace_id: &Option<WorkspaceId>) -> Option<PathBuf> {
        let workspace_id = workspace_id.as_ref()?;
        self.workspace_registry
            .as_ref()?
            .workspaces()
            .into_iter()
            .find(|workspace| workspace.workspace_id == *workspace_id)
            .map(|workspace| PathBuf::from(workspace.root_path.as_str()))
    }

    fn task_fact_context_parts(&self, task: &magi_core::Task) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(scope) = task
            .workspace_scope
            .as_deref()
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
        {
            parts.push(format!("[task-workspace] {scope}"));
        }
        if !task.input_refs.is_empty() {
            parts.push(format!(
                "[task-input] {}",
                format_task_ref_list(&task.input_refs)
            ));
        }

        let task_store = self.pipeline.execution_runtime.task_store();
        for dependency_id in &task.dependency_ids {
            if let Some(dependency) = task_store.get_task(dependency_id) {
                parts.push(format_dependency_task_context(&dependency));
            } else {
                parts.push(format!("[dependency] id={dependency_id} status=missing"));
            }
        }
        if parts.is_empty() && task.kind != TaskKind::Validation {
            return parts;
        }
        parts.insert(
            0,
            "[current-task-rule] 当前任务标题、目标、input_refs、依赖任务输出和 task-context 是本次执行的主事实；knowledge/memory 只能补充，不能改写当前任务目标。目标中的路径、工具名、命令、标记字符串以及“必须/要求”条款必须逐项执行或明确说明无法执行的真实原因，不能替换成历史任务或泛化检查。"
                .to_string(),
        );
        if task.kind == TaskKind::Validation {
            parts.insert(
                1,
                "[validation-rule] 只验证本任务 dependency/input 指向的当前执行产出；不得把历史经验、知识库记录或其他会话目标当成本次交付对象。"
                    .to_string(),
            );
        }
        parts
    }

    fn assemble_prompt(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> (String, Option<ExecutionContextSummary>) {
        let base_prompt = if task.goal.is_empty() {
            task.title.clone()
        } else {
            format!("{}\n\n{}", task.title, task.goal)
        };
        let user_rules_prefix = self.resolve_user_rules_prompt();
        let safeguard_prefix = self.resolve_safeguard_prompt();
        let task_fact_context_parts = self.task_fact_context_parts(task);

        let Some(ref ctx_runtime) = self.context_runtime else {
            if task_fact_context_parts.is_empty() {
                return (
                    prepend_session_instructions(
                        user_rules_prefix.as_deref(),
                        safeguard_prefix.as_deref(),
                        &base_prompt,
                    ),
                    None,
                );
            }
            let ctx_text = task_fact_context_parts.join("\n");
            return (
                prepend_session_instructions(
                    user_rules_prefix.as_deref(),
                    safeguard_prefix.as_deref(),
                    &format!("--- Context ---\n{ctx_text}\n--- Task ---\n{base_prompt}"),
                ),
                None,
            );
        };

        let ws_id = workspace_id
            .clone()
            .unwrap_or_else(|| WorkspaceId::new("default"));
        let result = ctx_runtime.assemble_execution_context(&ExecutionContextAssemblyRequest {
            session_id: session_id.clone(),
            workspace_id: ws_id,
            project_key: None,
            clues: ExecutionContextClues {
                mission: Some(task.title.clone()),
                assignment: None,
                task: Some(task.goal.clone()),
            },
            budget: ContextBudget {
                max_turns: 3,
                max_knowledge: 3,
                max_memory: 2,
                max_shared_items: 1,
                max_file_summaries: 2,
            },
        });
        let task_context_entries = self
            .pipeline
            .execution_runtime
            .task_store()
            .context_entries_for_refs(&task.context_refs);
        let has_context = !result.selected_knowledge.is_empty()
            || !result.selected_memory.is_empty()
            || !result.selected_shared_context.is_empty()
            || !task_fact_context_parts.is_empty()
            || !task_context_entries.is_empty();

        let context_summary = ExecutionContextSummary::from_context_assembly(&result);

        if !has_context {
            return (
                prepend_session_instructions(
                    user_rules_prefix.as_deref(),
                    safeguard_prefix.as_deref(),
                    &base_prompt,
                ),
                Some(context_summary),
            );
        }
        let mut ctx_parts: Vec<String> = Vec::new();
        ctx_parts.extend(task_fact_context_parts);
        for entry in &task_context_entries {
            ctx_parts.push(format!(
                "[task-context] {}: {}",
                entry.context_ref,
                compact_task_context_text(&entry.content)
            ));
        }
        for item in &result.selected_knowledge {
            ctx_parts.push(format!("[knowledge] {}: {}", item.title, item.excerpt));
        }
        for item in &result.selected_memory {
            ctx_parts.push(format!("[memory] {}", item.content));
        }
        for item in &result.selected_shared_context {
            ctx_parts.push(format!("[context] {}: {}", item.title, item.content));
        }
        let ctx_text = ctx_parts.join("\n");
        (
            prepend_session_instructions(
                user_rules_prefix.as_deref(),
                safeguard_prefix.as_deref(),
                &format!("--- Context ---\n{ctx_text}\n--- Task ---\n{base_prompt}"),
            ),
            Some(context_summary),
        )
    }

    fn resolve_user_rules_prompt(&self) -> Option<String> {
        let store = self.settings_store.as_ref()?;
        let raw = store.get_section("userRules");
        match raw {
            serde_json::Value::String(value) => {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            serde_json::Value::Object(map) => {
                let candidate = map
                    .get("userRules")
                    .and_then(|value| value.as_str())
                    .or_else(|| map.get("content").and_then(|value| value.as_str()))
                    .or_else(|| map.get("prompt").and_then(|value| value.as_str()))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                (!candidate.is_empty()).then_some(candidate)
            }
            _ => None,
        }
    }

    fn resolve_safeguard_prompt(&self) -> Option<String> {
        // S8：单一事实源 —— 危险模式集合从 SafetyGate 派生，确保 prompt 文案与
        // 运行期 enforcement 共用同一份规则。
        let gate = self.build_safety_gate()?;
        let patterns = gate
            .rules()
            .iter()
            .filter(|rule| rule.enabled)
            .map(|rule| rule.pattern.trim())
            .filter(|pattern| !pattern.is_empty())
            .collect::<Vec<_>>();
        if patterns.is_empty() {
            return None;
        }
        Some(format!(
            "执行 shell / git / 文件写操作前，如果命中以下危险模式，必须先向用户确认，不得直接执行（违规调用会被 SafetyGate 在运行期直接拦截）：\n{}",
            patterns
                .iter()
                .map(|pattern| format!("- {}", pattern))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    /// S8：依据当前 settings 快照构造 SafetyGate。
    /// 调用者每次进入 LLM 轮次循环前都构造一次；引擎本身无状态，可在该轮次内共享。
    pub(crate) fn build_safety_gate(&self) -> Option<magi_safety_gate::SafetyGate> {
        let store = self.settings_store.as_ref()?;
        let raw = store.get_section("safeguardConfig");
        let rules = raw
            .get("rules")
            .map(magi_safety_gate::rules_from_settings_value)
            .unwrap_or_default();
        if rules.is_empty() {
            None
        } else {
            Some(magi_safety_gate::SafetyGate::new(rules))
        }
    }

    fn resolve_model_client(&self) -> Option<Arc<dyn ModelBridgeClient>> {
        resolve_configured_model_client(
            self.settings_store.as_ref(),
            self.model_bridge_client.clone(),
        )
    }

    fn apply_skill_prompt_injections(
        &self,
        mut prompt: String,
        skill_name: Option<&str>,
    ) -> String {
        let Some(skill_id) = skill_name else {
            return prompt;
        };
        let Some(ref skill_rt) = self.skill_runtime else {
            return prompt;
        };
        let plan = skill_rt.build_tool_runtime_plan(magi_skill_runtime::SkillSelection {
            skill_ids: vec![skill_id.to_string()],
            requested_tools: vec![],
        });
        for injection in plan.prompt_injections {
            prompt = format!("{}\n\n{}", injection.body, prompt);
        }
        prompt
    }

    pub fn execute_session_turn(
        &self,
        request: SessionTurnExecutionRequest,
    ) -> Result<SessionTurnExecutionOutput, ApiError> {
        let Some(client) = self.resolve_model_client() else {
            return Err(ApiError::internal_assembly(
                "执行 session turn 失败",
                "model bridge client 未配置",
            ));
        };

        let prompt = self.apply_skill_prompt_injections(
            prepend_session_instructions(
                self.resolve_user_rules_prompt().as_deref(),
                self.resolve_safeguard_prompt().as_deref(),
                &request.prompt,
            ),
            request.skill_name.as_deref(),
        );

        let tools = if request.use_tools {
            let tool_defs = self.build_tool_definitions(None);
            (!tool_defs.is_empty()).then_some(tool_defs)
        } else {
            None
        };
        run_session_turn_execution(SessionTurnExecutionRuntime {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            settings_store: self.settings_store.as_ref(),
            tool_registry: self.tool_registry.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            snapshot_manager: self.snapshot_manager.as_ref(),
            request,
            prompt,
            tools,
        })
        .map_err(|msg| ApiError::model_invocation_failed("执行 session turn 失败", msg))
    }

    fn invoke_llm_with_tools(
        &self,
        task: &magi_core::Task,
        task_id: &TaskId,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        use_tools: bool,
        skill_name: Option<String>,
        usage_binding: &ModelUsageBinding,
        streaming_entry_id: Option<&str>,
        worker_lane_id: Option<&str>,
        worker_lane_seq: Option<usize>,
        worker_id: Option<&WorkerId>,
        thread_id: &magi_core::ThreadId,
        system_prompt: Option<String>,
    ) -> (TaskOutcome, Option<ExecutionContextSummary>) {
        let Some(client) = self.resolve_model_client() else {
            tracing::error!(task_id = %task.task_id, "invoke_llm_with_tools: no model bridge client configured");
            return (
                TaskOutcome::Failed {
                    error: format!(
                        "no model bridge client configured for task {}",
                        task.task_id
                    ),
                },
                None,
            );
        };

        let (prompt, context_summary) = self.assemble_prompt(task, session_id, workspace_id);
        let prompt = self.apply_skill_prompt_injections(prompt, skill_name.as_deref());
        let workspace_root_path = self.resolve_workspace_root_path(workspace_id);

        // P7：orchestrator_thread_id 为主线可见性锚点，分派到达时必然已 spawn；缺失即架构破坏。
        let orchestrator_thread_id = self
            .session_store
            .orchestrator_thread_for_session(session_id)
            .map(|thread| thread.thread_id)
            .unwrap_or_else(|| {
                panic!(
                    "session {session_id} missing orchestrator thread when dispatching task {}",
                    task.task_id
                )
            });

        let tools = if use_tools {
            let tool_defs = self.build_tool_definitions(Some(task));
            if tool_defs.is_empty() {
                None
            } else {
                Some(tool_defs)
            }
        } else {
            None
        };

        let conversation_registry = self
            .conversation_registry
            .as_ref()
            .expect("LlmTaskDispatcher 缺少 ConversationRegistry，无法走 Task System v2 Turn 状态机");
        let stream_fanout = self
            .stream_fanout
            .as_ref()
            .expect("LlmTaskDispatcher 缺少 StreamFanOut，无法发布 v2 流派生事件");
        let agent_role_registry = self
            .agent_role_registry
            .as_ref()
            .expect("LlmTaskDispatcher 缺少 AgentRoleRegistry，无法解析 task→role");
        let safety_gate = self.build_safety_gate();
        let todo_ledger = self.todo_ledger_registry.get_or_create(session_id);
        let project_memory = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.project_memory_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "ProjectMemory: 打开失败，本次 Turn 不注入项目记忆");
                    None
                }
            }
        });
        let mission_charter = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.mission_charter_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "MissionCharter: 打开失败，本次 Turn 不注入 mission 宪章");
                    None
                }
            }
        });
        let plan = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.plan_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "Plan: 打开失败，本次 Turn 不注入 mission 计划");
                    None
                }
            }
        });
        let mission_workspace = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.mission_workspace_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "MissionWorkspace: 打开失败，本次 Turn 不注入工作目录视图");
                    None
                }
            }
        });
        let knowledge_graph = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.knowledge_graph_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "KnowledgeGraph: 打开失败，本次 Turn 不注入 mission KG");
                    None
                }
            }
        });
        let validation_runner = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.validation_runner_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "ValidationRunner: 打开失败，本次 Turn 不注入验证结果");
                    None
                }
            }
        });
        let checkpoint = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.checkpoint_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "Checkpoint: 打开失败，本次 Turn 不注入检查点日志");
                    None
                }
            }
        });
        let human_checkpoint = workspace_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.human_checkpoint_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "HumanCheckpoint: 打开失败，本次 Turn 不注入人工审核点");
                    None
                }
            }
        });
        crate::task_llm_loop::run_task_llm_loop(crate::task_llm_loop::TaskLlmLoopRequest {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            settings_store: self.settings_store.as_ref(),
            tool_registry: self.tool_registry.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            task_store: self.pipeline.execution_runtime.task_store(),
            conversation_registry: conversation_registry.as_ref(),
            stream_fanout: stream_fanout.as_ref(),
            agent_role_registry: agent_role_registry.as_ref(),
            spawn_graph: self.spawn_graph.as_ref(),
            safety_gate: safety_gate.as_ref(),
            todo_ledger: todo_ledger.as_ref(),
            project_memory: project_memory.as_deref(),
            mission_charter: mission_charter.as_deref(),
            plan: plan.as_deref(),
            mission_workspace: mission_workspace.as_deref(),
            knowledge_graph: knowledge_graph.as_deref(),
            validation_runner: validation_runner.as_deref(),
            checkpoint: checkpoint.as_deref(),
            human_checkpoint: human_checkpoint.as_deref(),
            task,
            task_id,
            lease_id,
            session_id,
            workspace_id,
            prompt,
            tools,
            usage_binding,
            streaming_entry_id,
            worker_lane_id,
            worker_lane_seq,
            worker_id,
            thread_id,
            orchestrator_thread_id: &orchestrator_thread_id,
            context_summary,
            system_prompt,
            workspace_root_path,
        })
    }

    /// Synchronous inner dispatch logic; invoked either directly or inside
    /// `tokio::task::spawn_blocking` so the LLM call does not starve the
    /// async runtime (design §1.3).
    fn dispatch_inner(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
    ) -> Result<(), String> {
        let Some(plan) = self.execution_registry.remove(&task.task_id) else {
            let error = format!(
                "任务 {} 缺少结构化执行计划，已拒绝无计划执行路径",
                task.task_id
            );
            tracing::error!(
                task_id = %task.task_id,
                mission_id = %task.mission_id,
                worker_id = %worker.worker_id,
                "task dispatch missing execution plan"
            );
            self.push_result(
                &task.task_id,
                &lease.lease_id,
                TaskOutcome::Failed { error },
            );
            return Ok(());
        };

        match plan {
            TaskExecutionPlan::Dispatch {
                target: _,
                worker_id,
                lane_id,
                lane_seq,
                thread_id,
                is_primary,
                session_id,
                workspace_id,
                ownership,
                writebacks,
                use_tools,
                skill_name,
            } => {
                self.publish_task_dispatched_event(
                    &task.task_id,
                    &task.mission_id,
                    worker,
                    &lease.lease_id,
                    task.kind,
                    Some(&session_id),
                    workspace_id.as_ref(),
                );
                self.execute_dispatch_plan(
                    task,
                    &task.task_id,
                    &lease.lease_id,
                    session_id,
                    workspace_id,
                    ownership,
                    writebacks,
                    use_tools,
                    skill_name,
                    model_usage_binding_for_worker(worker, is_primary),
                    lane_id,
                    lane_seq,
                    worker_id,
                    thread_id,
                    worker.system_prompt_template.clone(),
                );
            }
        }

        Ok(())
    }
}

struct LearningCandidate {
    content: String,
    context: Option<String>,
    tags: Vec<String>,
}

fn extract_learning_candidates(text: &str) -> Vec<LearningCandidate> {
    let markers = [
        "经验",
        "教训",
        "结论",
        "注意",
        "建议",
        "最佳实践",
        "踩坑",
        "坑点",
        "要点",
        "important",
        "note",
        "lesson",
        "tip",
        "best practice",
    ];
    let mut candidates = Vec::new();
    for raw in text.lines() {
        let line = raw
            .trim()
            .trim_start_matches(['-', '*', '•', '1', '2', '3', '4', '5', '.', ' '])
            .trim();
        if line.chars().count() < 12 || line.chars().count() > 600 {
            continue;
        }
        let lower = line.to_lowercase();
        if !markers
            .iter()
            .any(|marker| lower.contains(&marker.to_lowercase()))
        {
            continue;
        }
        if candidates.iter().any(|candidate: &LearningCandidate| {
            normalized_text(&candidate.content) == normalized_text(line)
        }) {
            continue;
        }
        candidates.push(LearningCandidate {
            content: line.to_string(),
            context: None,
            tags: vec!["auto".to_string(), "learning".to_string()],
        });
        if candidates.len() >= 5 {
            break;
        }
    }
    candidates
}

fn normalized_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn knowledge_duplicate(
    existing: &[KnowledgeRecord],
    kind: KnowledgeKind,
    workspace_id: Option<&WorkspaceId>,
    content: &str,
) -> bool {
    let normalized = normalized_text(content);
    existing.iter().any(|record| {
        record.kind == kind && record.workspace_id.as_ref() == workspace_id && {
            let record_text = normalized_text(&record.content);
            record_text == normalized
                || record_text.contains(&normalized)
                || normalized.contains(&record_text)
        }
    })
}

fn title_from_learning_content(content: &str) -> String {
    let mut title = content.chars().take(80).collect::<String>();
    if content.chars().count() > 80 {
        title.push('…');
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{MissionId, Task, TaskKind, ThreadId};
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn,
        ExecutionThread, ExecutionThreadStatus,
    };
    use magi_workspace::WorkspaceStore;

    fn learning_record(id: &str, workspace_id: Option<&str>, content: &str) -> KnowledgeRecord {
        KnowledgeRecord {
            knowledge_id: id.to_string(),
            kind: KnowledgeKind::Learning,
            title: content.to_string(),
            content: content.to_string(),
            tags: Vec::new(),
            workspace_id: workspace_id.map(WorkspaceId::new),
            source_ref: None,
            updated_at: UtcMillis::now(),
        }
    }

    fn task_execution_dispatcher_for_prompt_tests(
        task_store: Arc<TaskStore>,
        context_runtime: Option<Arc<ContextRuntime>>,
    ) -> LlmTaskDispatcher {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = magi_orchestrator::OrchestratorService::new(Arc::clone(&event_bus));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator
            .execution_runtime(
                magi_worker_runtime::WorkerRuntime::new_compare(Arc::clone(&event_bus)),
                tool_registry.clone(),
                magi_skill_runtime::SkillDispatchRuntime::new(
                    tool_registry,
                    magi_bridge_client::BridgeDispatchRuntime::new(),
                ),
            )
            .with_task_store(task_store);
        let dispatcher = LlmTaskDispatcher::new(
            event_bus,
            ExecutionPipeline {
                orchestrator,
                execution_runtime,
                memory_store: magi_memory_store::MemoryStore::new(),
            },
            Arc::new(SessionStore::new()),
            TaskExecutionRegistry::default(),
            Arc::new(EventBasedResultReceiver::new()),
            Arc::new(std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new())),
        );
        if let Some(runtime) = context_runtime {
            dispatcher.with_context_runtime(runtime)
        } else {
            dispatcher
        }
    }

    fn prompt_test_task(
        task_id: &str,
        kind: TaskKind,
        title: &str,
        goal: &str,
        status: TaskStatus,
    ) -> Task {
        let now = UtcMillis::now();
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-prompt-context"),
            root_task_id: TaskId::new("root-prompt-context"),
            parent_task_id: None,
            kind,
            title: title.to_string(),
            goal: goal.to_string(),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: Some("/Users/xie/code/TEST".to_string()),
            write_scope: None,
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

    #[test]
    fn validation_prompt_includes_dependency_outputs_without_context_runtime() {
        let task_store = Arc::new(TaskStore::new());
        let mut action = prompt_test_task(
            "action-current-delivery",
            TaskKind::Action,
            "执行只读检查",
            "检查 /Users/xie/code/TEST 当前项目结构",
            TaskStatus::Completed,
        );
        action.output_refs =
            vec!["当前任务输出：/Users/xie/code/TEST 已完成只读检查，未修改文件。".to_string()];
        action.evidence_refs =
            vec!["evidence://task/action-current-delivery/output/0?ref=readonly-check".to_string()];
        task_store.insert_task(action);

        let mut validation = prompt_test_task(
            "validation-current-delivery",
            TaskKind::Validation,
            "验证交付",
            "验证 /Users/xie/code/TEST 的只读任务交付结果",
            TaskStatus::Ready,
        );
        validation.dependency_ids = vec![TaskId::new("action-current-delivery")];
        task_store.insert_task(validation.clone());

        let dispatcher = task_execution_dispatcher_for_prompt_tests(task_store, None);
        let (prompt, context_summary) = dispatcher.assemble_prompt(
            &validation,
            &SessionId::new("session-prompt-context"),
            &Some(WorkspaceId::new("workspace-prompt-context")),
        );

        assert!(context_summary.is_none());
        assert!(prompt.contains("[validation-rule]"));
        assert!(prompt.contains("当前任务输出：/Users/xie/code/TEST 已完成只读检查"));
        assert!(prompt.contains("evidence://task/action-current-delivery/output/0"));
        assert!(prompt.contains("--- Task ---\n验证交付"));
    }

    #[test]
    fn validation_prompt_keeps_dependency_outputs_before_external_context() {
        let task_store = Arc::new(TaskStore::new());
        let mut action = prompt_test_task(
            "action-current-priority",
            TaskKind::Action,
            "执行当前项目检查",
            "检查 /Users/xie/code/TEST 当前项目",
            TaskStatus::Completed,
        );
        action.output_refs = vec!["当前项目事实：TEST 工作区检查已完成。".to_string()];
        task_store.insert_task(action);

        let mut validation = prompt_test_task(
            "validation-current-priority",
            TaskKind::Validation,
            "验证交付",
            "验证 /Users/xie/code/TEST 的只读任务交付结果",
            TaskStatus::Ready,
        );
        validation.dependency_ids = vec![TaskId::new("action-current-priority")];
        task_store.insert_task(validation.clone());

        let knowledge_store = KnowledgeStore::new();
        knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "knowledge-old-autosave".to_string(),
            kind: KnowledgeKind::Learning,
            title: "验证交付".to_string(),
            content: "自动保存规则旧上下文：这不是当前 TEST 工作区任务。".to_string(),
            tags: Vec::new(),
            workspace_id: Some(WorkspaceId::new("workspace-prompt-context")),
            source_ref: None,
            updated_at: UtcMillis::now(),
        });
        let context_runtime = Arc::new(ContextRuntime::new(
            knowledge_store,
            magi_memory_store::MemoryStore::new(),
        ));
        let dispatcher =
            task_execution_dispatcher_for_prompt_tests(task_store, Some(context_runtime));
        let (prompt, context_summary) = dispatcher.assemble_prompt(
            &validation,
            &SessionId::new("session-prompt-context"),
            &Some(WorkspaceId::new("workspace-prompt-context")),
        );

        assert!(context_summary.is_some());
        let dependency_index = prompt
            .find("[dependency-task]")
            .expect("dependency output should be present");
        let task_index = prompt
            .find("--- Task ---")
            .expect("task section should exist");
        assert!(
            dependency_index < task_index,
            "当前依赖输出必须在任务正文之前进入 prompt"
        );
        if let Some(knowledge_index) = prompt.find("[knowledge]") {
            assert!(
                dependency_index < knowledge_index,
                "当前依赖输出必须排在 knowledge/memory 之前"
            );
        }
    }

    #[test]
    fn learning_duplicate_detection_is_workspace_scoped() {
        let content = "最佳实践：同一条经验可以在不同 workspace 分别沉淀";
        let existing = vec![learning_record(
            "learning-workspace-a",
            Some("workspace-a"),
            content,
        )];

        assert!(knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            Some(&WorkspaceId::new("workspace-a")),
            content,
        ));
        assert!(!knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            Some(&WorkspaceId::new("workspace-b")),
            content,
        ));
        assert!(!knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            None,
            content,
        ));
    }

    #[test]
    fn failed_background_finalizer_does_not_duplicate_existing_error_item() {
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let session_store = Arc::new(SessionStore::new());
        let task_store = Arc::new(TaskStore::new());
        let session_id = SessionId::new("session-background-finalizer-idempotent");
        let mission_id = MissionId::new("mission-background-finalizer-idempotent");
        let root_task_id = TaskId::new("task-background-finalizer-idempotent");
        let now = UtcMillis::now();

        session_store
            .create_session(session_id.clone(), "background finalizer idempotent")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-background-finalizer-idempotent".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: Vec::new(),
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-background-finalizer-idempotent".to_string(),
                        trimmed_text: Some("执行后台任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: None,
                },
            )
            .expect("active chain should be stored");
        let mut error_item = session_turn_item(
            "assistant_error",
            "failed",
            Some("回复生成失败".to_string()),
            Some("model bridge unavailable".to_string()),
            Some("turn-item-existing-error".to_string()),
            orchestrator_thread_id.clone(),
        );
        error_item.task_id = Some(root_task_id.clone());
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-background-finalizer-idempotent".to_string(),
                    turn_seq: 1,
                    accepted_at: now,
                    completed_at: Some(UtcMillis(now.0 + 42)),
                    status: "failed".to_string(),
                    user_message: Some("执行后台任务".to_string()),
                    items: vec![error_item],
                    worker_lanes: Vec::new(),
                },
            )
            .expect("failed current turn should be stored");
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Objective,
            title: "后台任务".to_string(),
            goal: "验证终态观察幂等".to_string(),
            status: TaskStatus::Failed,
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
            variant: magi_core::TaskVariant::default(),
            created_at: now,
            updated_at: now,
        });
        let state = ApiState::new(
            "test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::clone(&task_store));

        assert!(finalize_background_session_task_turn_if_root_terminal(
            &state,
            &session_id,
            &root_task_id,
            "error",
        ));

        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain inspectable");
        let error_items = turn
            .items
            .iter()
            .filter(|item| item.kind == "assistant_error")
            .collect::<Vec<_>>();
        assert_eq!(error_items.len(), 1);
        assert_eq!(
            error_items[0].content.as_deref(),
            Some("model bridge unavailable")
        );
        assert!(
            event_bus.snapshot().recent_events.is_empty(),
            "已有具体错误时 finalizer 不应再发布泛化失败卡"
        );
    }

    #[test]
    fn completed_background_finalizer_appends_orchestrator_final_without_leaking_worker_final() {
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let session_store = Arc::new(SessionStore::new());
        let task_store = Arc::new(TaskStore::new());
        let session_id = SessionId::new("session-background-finalizer-worker-only");
        let mission_id = MissionId::new("mission-background-finalizer-worker-only");
        let root_task_id = TaskId::new("task-background-finalizer-worker-only-root");
        let worker_task_id = TaskId::new("task-background-finalizer-worker-only-action");
        let now = UtcMillis::now();

        session_store
            .create_session(session_id.clone(), "background finalizer worker only")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        let worker_thread_id = ThreadId::new("thread-integration-dev-finalizer-worker-only");
        session_store.register_thread(ExecutionThread {
            thread_id: worker_thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: "integration-dev".to_string(),
            worker_instance_id: WorkerId::new("worker-finalizer-worker-only"),
            status: ExecutionThreadStatus::Active,
            created_at: now,
            last_used_at: now,
            handled_task_ids: vec![worker_task_id.clone()],
            message_history: Vec::new(),
        });

        let mut phase_item = session_turn_item(
            "assistant_phase",
            "pending",
            Some("任务状态".to_string()),
            Some("已接收请求，正在整理执行步骤。".to_string()),
            Some("turn-item-phase-worker-only".to_string()),
            orchestrator_thread_id.clone(),
        );
        phase_item.task_id = Some(root_task_id.clone());

        let mut worker_final_item = session_turn_item(
            "assistant_final",
            "completed",
            Some("worker 最终输出".to_string()),
            Some("worker 内部最终输出，不应漂移成主线回复。".to_string()),
            Some("turn-item-worker-final-only".to_string()),
            worker_thread_id.clone(),
        );
        worker_final_item.item_seq = 2;
        worker_final_item.task_id = Some(worker_task_id.clone());
        worker_final_item.worker_id = Some(WorkerId::new("worker-finalizer-worker-only"));

        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-background-finalizer-worker-only".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![worker_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-background-finalizer-worker-only".to_string(),
                        trimmed_text: Some("执行深度任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-background-finalizer-worker-only".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "accepted".to_string(),
                        user_message: Some("执行深度任务".to_string()),
                        items: vec![phase_item, worker_final_item],
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be stored");

        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Objective,
            title: "后台深度任务".to_string(),
            goal: "验证 worker-only final 不阻塞 turn 终态".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: vec![worker_task_id.clone()],
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
            variant: magi_core::TaskVariant::default(),
            created_at: now,
            updated_at: now,
        });
        task_store.insert_task(Task {
            task_id: worker_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: TaskKind::Validation,
            title: "交付验证".to_string(),
            goal: "验证 worker 交付结果".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: vec!["交付验收通过：orchestrator-final-marker".to_string()],
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            variant: magi_core::TaskVariant::default(),
            created_at: now,
            updated_at: UtcMillis(now.0 + 1),
        });
        let state = ApiState::new(
            "test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::clone(&task_store));

        assert!(finalize_background_session_task_turn_if_root_terminal(
            &state,
            &session_id,
            &root_task_id,
            "completed",
        ));

        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("turn should remain inspectable");
        assert_eq!(turn.status, "completed");
        assert!(turn.completed_at.is_some());
        assert!(
            turn.items
                .iter()
                .find(|item| item.item_id == "turn-item-phase-worker-only")
                .is_some_and(|item| item.status == "completed"),
            "主线任务卡应原位收尾"
        );
        let orchestrator_final = turn
            .items
            .iter()
            .find(|item| {
                item.item_id == format!("turn-item-orchestrator-final-{root_task_id}")
                    && session_store
                        .resolve_thread_visibility(&session_id, &item.source_thread_id)
                        == Some(magi_session_store::ThreadVisibility::Main)
            })
            .expect("root 完成后必须追加编排者主线最终回复");
        let final_content = orchestrator_final.content.as_deref().unwrap_or_default();
        assert!(
            final_content.contains("已完成")
                && final_content.contains("orchestrator-final-marker"),
            "编排者最终回复应像正常对话一样收口任务交付摘要"
        );
        assert!(
            !final_content.contains("worker 内部最终输出"),
            "worker-only final 不能直接漂移为主线最终回复"
        );
        assert!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| event.event_type == "session.turn.item"
                    && event.payload["current_turn"]["status"].as_str() == Some("completed")
                    && event.payload["current_turn"]["response_duration_ms"].is_number()),
            "终态事件必须携带 completed current_turn 和总耗时"
        );
        assert!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| event.event_type == "message.created"
                    && event.payload["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("orchestrator-final-marker"))),
            "legacy message 事件只能来自编排者最终回复，不能来自 worker-only final"
        );
    }

    #[test]
    fn root_completion_summary_reads_like_conversation_for_multiple_outputs() {
        let outputs = [
            "验证通过：已完成第一段任务。\n\n修改的文件列表：无\n关键代码片段：无",
            "通过。 当前交付已基于执行产出完成验证，且未重复执行工具；已完成第二段任务。",
            r#"{"blocks":[{"content":"shell_exec: 命令执行成功: printf SHOULD_STAY_IN_TOOL_CARD","type":"tool_call"},{"content":"两个验证命令均已成功执行，输出符合预期。","type":"text"}]}"#,
            "目标：规划文本\n边界：只规划\n验收标准：不要进入最终回复",
            "修改的文件列表： - 无 关键验证结果： - 内部验证细节。 最终结论： - 已完成第三段任务。",
            "关键验证结果： - `printf A` 成功。一句自然语言总结：两个验证都已完成。",
        ]
        .into_iter()
        .filter_map(normalize_root_completion_output)
        .collect::<Vec<_>>();

        let summary = format_root_completion_summary(&outputs);

        assert!(summary.contains("已完成，关键结果是："));
        assert!(summary.contains("- 已验证：已完成第一段任务。"));
        assert!(summary.contains("- 已完成第二段任务。"));
        assert!(summary.contains("- 两个验证命令均已成功执行，输出符合预期。"));
        assert!(summary.contains("- 已完成第三段任务。"));
        assert!(summary.contains("- 两个验证都已完成。"));
        assert!(summary.contains("详细步骤和工具记录已保留在任务卡里"));
        assert!(!summary.contains("SHOULD_STAY_IN_TOOL_CARD"));
        assert!(!summary.contains("内部验证细节"));
        assert!(!summary.contains("printf A"));
        assert!(!summary.contains("当前交付已基于执行产出完成验证"));
        assert!(!summary.contains("规划文本"));
        assert!(!summary.contains("修改的文件列表"));
        assert!(!summary.contains("关键代码片段"));
    }

    #[test]
    fn root_completion_outputs_prefers_latest_delivery_stage_over_intermediate_execution() {
        let task_store = TaskStore::new();
        let mission_id = MissionId::new("mission-root-summary-latest-delivery");
        let root_task_id = TaskId::new("task-root-summary-latest-delivery");
        let now = UtcMillis::now();
        let root_task = Task {
            task_id: root_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Objective,
            title: "多端验证".to_string(),
            goal: "验证多端任务主线最终收口".to_string(),
            status: TaskStatus::Completed,
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
            variant: magi_core::TaskVariant::default(),
            created_at: now,
            updated_at: now,
        };
        task_store.insert_task(root_task.clone());

        let mut execution = root_task.clone();
        execution.task_id = TaskId::new("task-root-summary-execution");
        execution.parent_task_id = Some(root_task_id.clone());
        execution.kind = TaskKind::Action;
        execution.title = "执行验证".to_string();
        execution.output_refs = vec![
            "已完成多端稳定性只读验证：\n- 前端端点：`FRONTEND_MARKER`\n- 后端端点：`BACKEND_MARKER`"
                .to_string(),
        ];
        execution.updated_at = UtcMillis(now.0 + 1);
        task_store.insert_task(execution);

        let mut delivery = root_task.clone();
        delivery.task_id = TaskId::new("task-root-summary-delivery");
        delivery.parent_task_id = Some(root_task_id.clone());
        delivery.kind = TaskKind::Action;
        delivery.title = "交付总结".to_string();
        delivery.output_refs = vec![
            "验证已完成，交付如下：\n- 前端端点：已成功执行只读命令，输出 `FRONTEND_MARKER`\n- 后端端点：已成功执行只读命令，输出 `BACKEND_MARKER`\n主线汇总：两个端点都已完成只读验证。"
                .to_string(),
        ];
        delivery.updated_at = UtcMillis(now.0 + 2);
        task_store.insert_task(delivery);

        let outputs = root_completion_outputs(&task_store, &root_task);
        let summary = format_root_completion_summary(&outputs);

        assert_eq!(outputs.len(), 1);
        assert!(summary.contains("两个端点都已完成只读验证"));
        assert!(summary.contains("FRONTEND_MARKER"));
        assert!(!summary.contains("已完成多端稳定性只读验证"));
        assert!(!summary.contains("验证已完成，交付如下"));
        assert!(!summary.contains("主线汇总"));
    }

    #[test]
    fn terminal_reconcile_closes_nonterminal_turn_from_completed_root_task() {
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let session_store = Arc::new(SessionStore::new());
        let task_store = Arc::new(TaskStore::new());
        let session_id = SessionId::new("session-terminal-reconcile-completed-root");
        let mission_id = MissionId::new("mission-terminal-reconcile-completed-root");
        let root_task_id = TaskId::new("task-terminal-reconcile-completed-root");
        let now = UtcMillis::now();

        session_store
            .create_session(session_id.clone(), "terminal reconcile completed root")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        let mut phase_item = session_turn_item(
            "assistant_phase",
            "pending",
            Some("任务状态".to_string()),
            Some("任务运行中".to_string()),
            Some("turn-item-terminal-reconcile-phase".to_string()),
            orchestrator_thread_id.clone(),
        );
        phase_item.task_id = Some(root_task_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-terminal-reconcile-completed-root".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-terminal-reconcile-completed-root".to_string(),
                        trimmed_text: Some("执行任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-terminal-reconcile-completed-root".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: None,
                        status: "accepted".to_string(),
                        user_message: Some("执行任务".to_string()),
                        items: vec![phase_item],
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be stored");
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Objective,
            title: "已完成 root".to_string(),
            goal: "验证启动恢复收敛".to_string(),
            status: TaskStatus::Completed,
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
            variant: magi_core::TaskVariant::default(),
            created_at: now,
            updated_at: now,
        });
        let state = ApiState::new(
            "test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::clone(&task_store));

        assert_eq!(reconcile_terminal_session_task_turns(&state), 1);
        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("turn should remain inspectable");
        assert_eq!(turn.status, "completed");
        assert!(turn.completed_at.is_some());
        assert!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .any(|event| event.event_type == "session.turn.item"
                    && event.payload["current_turn"]["status"].as_str() == Some("completed")),
            "reconcile 应发布 canonical terminal turn 事件"
        );
    }

    #[test]
    fn terminal_finalizer_does_not_rewrite_cancelled_turn_to_failed() {
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let session_store = Arc::new(SessionStore::new());
        let task_store = Arc::new(TaskStore::new());
        let session_id = SessionId::new("session-terminal-cancelled-wins");
        let mission_id = MissionId::new("mission-terminal-cancelled-wins");
        let root_task_id = TaskId::new("task-terminal-cancelled-wins");
        let now = UtcMillis::now();

        session_store
            .create_session(session_id.clone(), "terminal cancelled wins")
            .expect("session should be creatable");
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        let mut phase_item = session_turn_item(
            "assistant_phase",
            "cancelled",
            Some("任务状态".to_string()),
            Some("任务已取消".to_string()),
            Some("turn-item-terminal-cancelled-phase".to_string()),
            orchestrator_thread_id.clone(),
        );
        phase_item.task_id = Some(root_task_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-terminal-cancelled-wins".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-terminal-cancelled-wins".to_string(),
                        trimmed_text: Some("执行任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-terminal-cancelled-wins".to_string(),
                        turn_seq: 1,
                        accepted_at: now,
                        completed_at: Some(now),
                        status: "cancelled".to_string(),
                        user_message: Some("执行任务".to_string()),
                        items: vec![phase_item],
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be stored");
        task_store.insert_task(Task {
            task_id: root_task_id.clone(),
            mission_id,
            root_task_id: root_task_id.clone(),
            parent_task_id: None,
            kind: TaskKind::Objective,
            title: "失败 root".to_string(),
            goal: "验证迟到失败不会覆盖取消".to_string(),
            status: TaskStatus::Failed,
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
            variant: magi_core::TaskVariant::default(),
            created_at: now,
            updated_at: now,
        });
        let state = ApiState::new(
            "test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::new()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::clone(&task_store));

        assert!(finalize_background_session_task_turn_if_root_terminal(
            &state,
            &session_id,
            &root_task_id,
            "error",
        ));
        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("turn should remain inspectable");
        assert_eq!(turn.status, "cancelled");
        assert!(
            event_bus
                .snapshot()
                .recent_events
                .iter()
                .all(|event| event.event_type != "session.turn.item"),
            "迟到失败不应发布 canonical failed 事件覆盖已取消 turn"
        );
    }
}

impl TaskDispatcher for LlmTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_core::AssignmentLease,
    ) -> Result<(), String> {
        // 普通模式的同步 for 循环要求 dispatch 同步完成，直接走 inner。
        if self
            .force_sync_dispatch
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
        {
            return self.dispatch_inner(task, worker, lease);
        }

        let dispatcher = self.clone();
        let task = task.clone();
        let worker = worker.clone();
        let lease = lease.clone();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.clone().spawn(async move {
                let result = handle
                    .spawn_blocking(move || {
                        if let Err(err) = dispatcher.dispatch_inner(&task, &worker, &lease) {
                            tracing::error!("dispatch_inner failed: {}", err);
                            dispatcher.push_result(
                                &task.task_id,
                                &lease.lease_id,
                                TaskOutcome::Failed {
                                    error: format!("dispatch failed: {}", err),
                                },
                            );
                        }
                    })
                    .await;
                if let Err(err) = result {
                    tracing::error!("dispatch spawn_blocking panicked: {:?}", err);
                }
            });
            Ok(())
        } else {
            // 不在 tokio 运行时中（例如同步测试环境），直接同步执行。
            self.dispatch_inner(&task, &worker, &lease)
        }
    }
}

fn submit_task_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    state
        .session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(map_dispatch_store_error)?;
    let graph = run_dispatch_submission(state, &request)?;
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = state
            .session_store
            .accept_active_execution_chain_with_timeline_entry(
                request.session_id.clone(),
                request.entry_id.clone(),
                TimelineEntryKind::UserMessage,
                request.timeline_message.clone(),
                request.accepted_at,
                active_execution_chain,
            );
        if let Err(error) = accept_result {
            cleanup_rejected_dispatch(state, &graph);
            return Err(map_dispatch_store_error(error));
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

fn cleanup_rejected_dispatch(state: &ApiState, graph: &TaskGraphSubmission) {
    if let Some(chain) = graph.active_execution_chain.as_ref() {
        let registry = state.task_execution_registry();
        for branch in &chain.branches {
            let _ = registry.remove(&branch.task_id);
        }
    }
    if let Some(task_store) = state.task_store() {
        cleanup_task_tree(task_store, &graph.root_task_id);
    }
}

fn map_dispatch_store_error(error: DomainError) -> ApiError {
    match error {
        DomainError::InvalidState { message } if message.contains("active current_turn") => {
            ApiError::conflict("任务派发失败", &message)
        }
        other => ApiError::internal_assembly("任务派发失败", other),
    }
}

pub fn submit_dispatch_submission(
    state: &ApiState,
    request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    submit_task_submission(state, request)
}

pub fn drive_dispatch_submission(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let manager = state.runner_manager().ok_or_else(|| {
        ApiError::internal_assembly("任务派发失败", "runner_manager 未配置")
    })?;
    let task_store = state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("任务派发失败", "task_store 未配置")
    })?;

    let root_task = task_store.get_task(&accepted.root_task_id).ok_or_else(|| {
        ApiError::internal_assembly("任务派发失败", "root task 不存在")
    })?;
    let background_allowed = root_task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.background_allowed)
        .unwrap_or(false);

    if background_allowed {
        match manager.start(
            accepted.root_task_id.as_str(),
            Some(accepted.session_id.clone()),
        ) {
            Ok(_) | Err(RunnerStartError::AlreadyRunning) => {
                accepted.runner_started = true;
                Ok(())
            }
            Err(RunnerStartError::NotFound) => Err(ApiError::internal_assembly(
                "任务派发失败",
                "root task 不存在",
            )),
        }
    } else {
        let execution = crate::a_path::drive_a_path(
            state,
            &accepted.root_task_id,
            &accepted.action_task_id,
            "任务派发失败",
        )?;
        accepted.runner_started = execution.runner_started;
        Ok(())
    }
}
