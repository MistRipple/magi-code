//! Task System v2 — session turn 终态收尾 (finalize / reconcile)。
//!
//! 公开 API 使用显式依赖参数（`&SessionStore`、`&InMemoryEventBus`、`Option<&TaskStore>`），
//! 不再耦合 ApiState。magi-api 侧保留薄壳转发。

use magi_core::{
    EventId, SessionId, Task, TaskId, TaskKind, TaskStatus, ThreadId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{ActiveExecutionTurn, SessionStore};

use crate::session_writeback::{
    append_session_turn_item_with_task_store, publish_current_session_turn_item_event,
    publish_session_turn_item_event, session_turn_item,
};

const TASK_CONTEXT_MAX_CHARS: usize = 4000;
const TASK_CONTEXT_MAX_REFS: usize = 8;
const ROOT_COMPLETION_SUMMARY_MAX_CHARS: usize = 2400;

pub fn turn_item_status_for_task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
    }
}

pub fn task_status_text(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
    }
}

pub fn current_turn_status_accepts_task_status_item(status: &str) -> bool {
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
            .any(|item| item.task_id.as_ref() == Some(&task.task_id));
        if !active_chain_matches && !turn_matches {
            continue;
        }

        let branch = sidecar.active_execution_chain.as_ref().and_then(|chain| {
            chain
                .branches
                .iter()
                .find(|branch| branch.task_id == task.task_id)
        });
        let source_thread_id = match branch {
            Some(branch) => branch.thread_id.clone(),
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
        item.role_id = task.executor_binding_target_role().map(str::to_string);
        if let Some(branch) = branch {
            item.worker_id = Some(branch.worker_id.clone());
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

pub fn compact_task_context_text(text: &str) -> String {
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

pub fn format_task_ref_list(refs: &[String]) -> String {
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

pub fn format_dependency_task_context(dependency: &Task) -> String {
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
pub fn latest_orchestrator_assistant_final(
    turn: &ActiveExecutionTurn,
    orchestrator_thread_id: &ThreadId,
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

/// root task 的最终答复优先作为主线回复。
///
/// coordinator root 现在有独立 task thread，但前端会把不带 worker/role 的 root item
/// 投射到主线。如果这里只认 session orchestrator thread，root 已经生成的 final 会和
/// fallback orchestrator summary 双显。
pub fn latest_root_task_assistant_final(
    turn: &ActiveExecutionTurn,
    root_task_id: &TaskId,
) -> Option<(String, String)> {
    turn.items
        .iter()
        .filter(|item| {
            item.kind == "assistant_final"
                && item.task_id.as_ref() == Some(root_task_id)
                && item.worker_id.is_none()
                && item.role_id.is_none()
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

pub fn compact_root_completion_summary(text: &str) -> String {
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

pub fn completion_summary_rank(task: &Task) -> u8 {
    match task.kind {
        TaskKind::LocalAgent => 5,
        TaskKind::LocalWorkflow => 4,
        TaskKind::RemoteAgent => 2,
        TaskKind::MonitorMcp | TaskKind::InProcessTeammate => 1,
        TaskKind::Dream => 0,
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

pub fn normalize_root_completion_output(output: &str) -> Option<String> {
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

pub fn root_completion_outputs(task_store: &TaskStore, root_task: &Task) -> Vec<String> {
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

pub fn format_root_completion_summary(outputs: &[String]) -> String {
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
            format!("已完成，关键结果是：\n\n{bullets}\n\n详细步骤和工具记录已保留在任务卡里。")
        }
    }
}

pub fn build_root_completion_summary(task_store: &TaskStore, root_task: &Task) -> String {
    let outputs = root_completion_outputs(task_store, root_task);
    format_root_completion_summary(&outputs)
}

fn ensure_root_completion_final_item(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    root_task: &Task,
    task_store: &TaskStore,
) -> Option<(String, String)> {
    let sidecar = session_store.runtime_sidecar(session_id)?;
    let turn = sidecar.current_turn.as_ref()?;
    let orchestrator_thread = session_store.orchestrator_thread_for_session(session_id)?;
    if let Some(response) = latest_root_task_assistant_final(turn, &root_task.task_id)
        .or_else(|| latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id))
    {
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
        session_store,
        session_id,
        final_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }

    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .as_ref()
        .and_then(|turn| {
            latest_root_task_assistant_final(turn, &root_task.task_id).or_else(|| {
                latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id)
            })
        })
}

pub fn finalize_background_session_task_turn_if_root_completed(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> bool {
    let Some(task_store) = task_store else {
        return false;
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return false;
    };
    if root_task.status != TaskStatus::Completed {
        return false;
    }

    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
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
    let Some(orchestrator_thread) = session_store.orchestrator_thread_for_session(session_id)
    else {
        return false;
    };
    let response = latest_root_task_assistant_final(turn, &root_task.task_id)
        .or_else(|| latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id))
        .or_else(|| {
            ensure_root_completion_final_item(
                session_store,
                event_bus,
                session_id,
                &workspace_id,
                &root_task,
                task_store,
            )
        });
    let event_item_id = response
        .as_ref()
        .map(|(_, item_id)| item_id.clone())
        .or_else(|| terminal_turn_event_anchor_item_id(turn, &orchestrator_thread.thread_id));
    let Some(event_item_id) = event_item_id else {
        return false;
    };

    if update_current_turn_completed_from_root(session_store, session_id).is_err() {
        return false;
    }
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        &workspace_id,
        &event_item_id,
        Some(task_store),
    );
    if let Some((response_text, _)) = response {
        let _ = event_bus.publish(
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
    session_store: &SessionStore,
    session_id: &SessionId,
) -> Result<(), ()> {
    match session_store
        .complete_current_turn_from_completed_root_task(session_id)
        .map_err(|_| ())?
    {
        Some(_) => Ok(()),
        None => Err(()),
    }
}

pub fn terminal_turn_event_anchor_item_id(
    turn: &ActiveExecutionTurn,
    orchestrator_thread_id: &ThreadId,
) -> Option<String> {
    turn.items
        .iter()
        .filter(|item| &item.source_thread_id == orchestrator_thread_id)
        .max_by_key(|item| item.item_seq)
        .or_else(|| turn.items.iter().max_by_key(|item| item.item_seq))
        .map(|item| item.item_id.clone())
}

pub fn finalize_background_session_task_turn_if_root_terminal(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
    session_id: &SessionId,
    root_task_id: &TaskId,
    runner_status: &str,
) -> bool {
    if finalize_background_session_task_turn_if_root_completed(
        session_store,
        event_bus,
        task_store,
        session_id,
        root_task_id,
    ) {
        return true;
    }

    let Some(task_store) = task_store else {
        return false;
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return false;
    };
    let (turn_status, message) = match root_task.status {
        TaskStatus::Failed => ("failed", "任务执行失败，未生成最终回复。"),
        TaskStatus::Killed => ("killed", "任务执行已终止。"),
        _ if runner_status == "error" => ("failed", "任务执行异常，未生成最终回复。"),
        _ if runner_status == "stopped" || runner_status == "killed" => {
            ("killed", "任务执行已终止。")
        }
        _ => return false,
    };

    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
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
    let Some(orchestrator_thread) = session_store.orchestrator_thread_for_session(session_id)
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

    if session_store
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
        session_store,
        session_id,
        error_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, &workspace_id, &published);
    }

    true
}

pub fn reconcile_terminal_session_task_turns(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
) -> usize {
    let Some(task_store) = task_store else {
        return 0;
    };
    let candidates = session_store
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
                session_store,
                event_bus,
                Some(task_store),
                session_id,
                root_task_id,
                runner_status,
            )
        })
        .count()
}

pub fn current_turn_status_is_terminal(status: &str) -> bool {
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

pub fn current_turn_status_is_completed(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "complete" | "succeeded" | "success"
    )
}

pub fn runner_status_for_terminal_task(status: TaskStatus) -> Option<&'static str> {
    match status {
        TaskStatus::Completed => Some("completed"),
        TaskStatus::Failed => Some("error"),
        TaskStatus::Killed => Some("killed"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_root_task_assistant_final_prefers_root_without_worker_binding() {
        let root_task_id = TaskId::new("task-root");
        let child_task_id = TaskId::new("task-child");
        let root_thread_id = ThreadId::new("thread-root");
        let child_thread_id = ThreadId::new("thread-child");
        let now = UtcMillis::now();
        let mut child_final = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("代理答复".to_string()),
            Some("turn-item-child-final".to_string()),
            child_thread_id,
        );
        child_final.item_seq = 1;
        child_final.task_id = Some(child_task_id);
        child_final.worker_id = Some(magi_core::WorkerId::new("worker-child"));
        child_final.role_id = Some("explorer".to_string());
        let mut root_final = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("主线答复".to_string()),
            Some("turn-item-root-final".to_string()),
            root_thread_id,
        );
        root_final.item_seq = 2;
        root_final.task_id = Some(root_task_id.clone());
        let turn = ActiveExecutionTurn {
            turn_id: "turn-root-final".to_string(),
            turn_seq: 1,
            accepted_at: now,
            completed_at: None,
            status: "running".to_string(),
            user_message: None,
            items: vec![child_final, root_final],
        };

        let response = latest_root_task_assistant_final(&turn, &root_task_id)
            .expect("root final should be selected");

        assert_eq!(response.0, "主线答复");
        assert_eq!(response.1, "turn-item-root-final");
    }
}
