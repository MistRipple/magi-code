//! 任务系统 — session turn 终态收尾 (finalize / reconcile)。
//!
//! 公开 API 使用显式依赖参数（`&SessionStore`、`&InMemoryEventBus`、`Option<&TaskStore>`），
//! 不再耦合 ApiState。magi-api 侧保留薄壳转发。

use magi_core::{
    SessionId, Task, TaskId, TaskKind, TaskStatus, ThreadId, UtcMillis, public_runtime_excerpt,
};
use magi_event_bus::InMemoryEventBus;
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{ActiveExecutionTurn, SessionStore};

use crate::session_turn_coordinator::{CoordinatorTurnStatus, SessionTurnCoordinator};
use crate::session_writeback::{
    CanonicalTurnEventSink, SessionStatePersistCallback, append_session_turn_item_for_turn,
    persist_session_state_checkpoint, publish_current_session_turn_item_event,
    publish_session_turn_item_event, session_turn_item,
};
use crate::turn_contract::TurnCommand;

const TASK_CONTEXT_MAX_CHARS: usize = 4000;
const TASK_CONTEXT_MAX_REFS: usize = 8;
const ROOT_COMPLETION_SUMMARY_MAX_CHARS: usize = 2400;
const TASK_FAILURE_DETAIL_MAX_CHARS: usize = 4096;
const SESSION_INTERRUPTION_NOTICE_KIND: &str = "session_interrupted";
const SESSION_INTERRUPTION_SOURCE_KEY: &str = "interruptionSource";
const SESSION_INTERRUPTION_SOURCE_DAEMON_RESTART: &str = "daemon_restart";
const SESSION_INTERRUPTION_RECOVERY_STATE_KEY: &str = "recoveryState";
const SESSION_INTERRUPTION_RECOVERY_READY: &str = "ready";
const SESSION_INTERRUPTION_RECOVERY_CLAIMED: &str = "claimed";

pub struct FinalizeBackgroundSessionTaskTurnContext<'a> {
    pub session_store: &'a SessionStore,
    pub event_bus: &'a InMemoryEventBus,
    pub task_store: Option<&'a TaskStore>,
    pub session_id: &'a SessionId,
    pub root_task_id: &'a TaskId,
    pub runner_status: &'a str,
    pub expected_turn_id: Option<&'a str>,
    /// 生产 Task 终态必须先由 SessionStore durable mutation 成功，再由 Coordinator
    /// 收口 attempt；None 仅供不依赖 Coordinator 的纯 runtime 单元测试。
    pub coordinator: Option<&'a SessionTurnCoordinator>,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
}

pub fn turn_item_status_for_task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "cancelled",
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
) -> Result<(), String> {
    for sidecar in
        session_store.active_execution_sidecars_for_task(&task.task_id, &task.root_task_id)
    {
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
        let published = append_session_turn_item_for_turn(
            session_store,
            &sidecar.session_id,
            Some(&turn.turn_id),
            item,
            task_store,
        )?
        .ok_or_else(|| {
            format!(
                "会话 {} 的当前 Turn 已不可写，无法记录任务 {} 的状态 {}",
                sidecar.session_id,
                task.task_id,
                task_status_text(new_status)
            )
        })?;
        let workspace_id = sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.workspace_id.clone());
        publish_session_turn_item_event(event_bus, &sidecar.session_id, &workspace_id, &published);
    }
    Ok(())
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
/// 自动生成的 orchestrator summary 双显。
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
    session_id: &SessionId,
    root_task: &Task,
    task_store: &TaskStore,
    expected_turn_id: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
        return Ok(None);
    };
    let Some(turn) = sidecar.current_turn.as_ref() else {
        return Ok(None);
    };
    if expected_turn_id.is_some_and(|expected| expected != turn.turn_id) {
        return Ok(None);
    }
    let Some(orchestrator_thread) = session_store.orchestrator_thread_for_session(session_id)
    else {
        return Ok(None);
    };
    if let Some(response) = latest_root_task_assistant_final(turn, &root_task.task_id)
        .or_else(|| latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id))
    {
        return Ok(Some(response));
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

    let Some(_published) = append_session_turn_item_for_turn(
        session_store,
        session_id,
        expected_turn_id,
        final_item,
        Some(task_store),
    )?
    else {
        return Ok(None);
    };
    Ok(session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .as_ref()
        .and_then(|turn| {
            latest_root_task_assistant_final(turn, &root_task.task_id).or_else(|| {
                latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id)
            })
        }))
}

pub fn finalize_background_session_task_turn_if_root_completed(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
    session_id: &SessionId,
    root_task_id: &TaskId,
    persist_session_state: Option<&SessionStatePersistCallback>,
) -> Result<bool, String> {
    finalize_completed_root_task_turn_for_turn(
        session_store,
        event_bus,
        task_store,
        session_id,
        root_task_id,
        None,
        persist_session_state,
    )
}

/// 仅供没有统一 Coordinator 的嵌入测试读取已完成 root；生产路径使用
/// `finalize_background_session_task_turn_if_root_terminal`。
pub fn finalize_background_session_task_turn_if_root_completed_for_turn(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
    session_id: &SessionId,
    root_task_id: &TaskId,
    expected_turn_id: Option<&str>,
    persist_session_state: Option<&SessionStatePersistCallback>,
) -> Result<bool, String> {
    finalize_completed_root_task_turn_for_turn(
        session_store,
        event_bus,
        task_store,
        session_id,
        root_task_id,
        expected_turn_id,
        persist_session_state,
    )
}

fn finalize_completed_root_task_turn_for_turn(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
    session_id: &SessionId,
    root_task_id: &TaskId,
    expected_turn_id: Option<&str>,
    persist_session_state: Option<&SessionStatePersistCallback>,
) -> Result<bool, String> {
    let Some(task_store) = task_store else {
        return Ok(false);
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return Ok(false);
    };
    if root_task.status != TaskStatus::Completed {
        return Ok(false);
    }

    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
        return Ok(false);
    };
    let Some(active_chain) = sidecar.active_execution_chain.as_ref() else {
        return Ok(false);
    };
    if active_chain.root_task_id != *root_task_id {
        return Ok(false);
    }
    let workspace_id = active_chain.workspace_id.clone();
    let Some(turn) = sidecar.current_turn.as_ref() else {
        return Ok(false);
    };
    if expected_turn_id.is_some_and(|expected| expected != turn.turn_id) {
        return Ok(false);
    }
    if current_turn_status_is_terminal(&turn.status) {
        let archived = archive_terminal_active_execution_chain(
            session_store,
            Some(task_store),
            session_id,
            root_task_id,
        )?;
        if archived {
            persist_session_state_checkpoint(persist_session_state, "session_task_chain_archived")?;
        }
        return Ok(archived);
    }
    let Some(orchestrator_thread) = session_store.orchestrator_thread_for_session(session_id)
    else {
        return Ok(false);
    };
    let response = match latest_root_task_assistant_final(turn, &root_task.task_id)
        .or_else(|| latest_orchestrator_assistant_final(turn, &orchestrator_thread.thread_id))
    {
        Some(response) => Some(response),
        None => ensure_root_completion_final_item(
            session_store,
            session_id,
            &root_task,
            task_store,
            expected_turn_id,
        )?,
    };
    let event_item_id = response
        .as_ref()
        .map(|(_, item_id)| item_id.clone())
        .or_else(|| terminal_turn_event_anchor_item_id(turn, &orchestrator_thread.thread_id));
    let Some(event_item_id) = event_item_id else {
        return Ok(false);
    };

    if let Err(error) =
        update_current_turn_completed_from_root(session_store, session_id, expected_turn_id)
    {
        return Err(format!("根任务完成时 Turn completed 状态提交失败: {error}"));
    }
    archive_terminal_active_execution_chain(
        session_store,
        Some(task_store),
        session_id,
        root_task_id,
    )?;
    persist_session_state_checkpoint(persist_session_state, "session_task_turn_completed")?;
    if let Err(error) = publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        &workspace_id,
        &event_item_id,
        Some(task_store),
    ) {
        return Err(format!("根任务完成时 Turn 终态事件发布失败: {error}"));
    }
    Ok(true)
}

/// 终态 root 不再占用会话的 active execution chain。
///
/// canonical turn、任务树和线程历史仍然保留；这里仅移除“当前正在执行”的所有权，
/// 防止一个已经完成的任务长期把后续普通对话绑定到过期任务链。
fn archive_terminal_active_execution_chain(
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    session_id: &SessionId,
    root_task_id: &TaskId,
) -> Result<bool, String> {
    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
        return Ok(false);
    };
    let Some(chain) = sidecar.active_execution_chain.as_ref() else {
        return Ok(false);
    };
    if chain.root_task_id != *root_task_id
        || chain.recovery_ref.is_some()
        || sidecar
            .current_turn
            .as_ref()
            .is_some_and(daemon_restart_recovery_is_pending)
    {
        return Ok(false);
    }
    // Failed root 仍可能带有可继续的 branch checkpoint。保留这条 active
    // execution chain，让 Continue 复用原 mission/root/chain；只有没有可恢复
    // branch 时才释放 session ownership。Completed/Killed root 仍按原规则归档。
    if task_store.is_some_and(|store| {
        store
            .get_task(root_task_id)
            .is_some_and(|root| root.status == TaskStatus::Failed)
    }) && chain.branches.iter().any(|branch| {
        crate::execution_chain_recovery::active_execution_branch_is_continue_recoverable(
            None, task_store, chain, branch,
        )
    }) {
        return Ok(false);
    }
    session_store
        .archive_active_execution_chain(session_id, root_task_id)
        .map(|_| true)
        .map_err(|error| format!("归档终态执行链失败: {error}"))
}

fn daemon_restart_recovery_is_pending(turn: &ActiveExecutionTurn) -> bool {
    turn.items.iter().any(|item| {
        item.metadata
            .get("noticeKind")
            .and_then(serde_json::Value::as_str)
            == Some(SESSION_INTERRUPTION_NOTICE_KIND)
            && item
                .metadata
                .get(SESSION_INTERRUPTION_SOURCE_KEY)
                .and_then(serde_json::Value::as_str)
                == Some(SESSION_INTERRUPTION_SOURCE_DAEMON_RESTART)
            && matches!(
                item.metadata
                    .get(SESSION_INTERRUPTION_RECOVERY_STATE_KEY)
                    .and_then(serde_json::Value::as_str),
                Some(SESSION_INTERRUPTION_RECOVERY_READY)
                    | Some(SESSION_INTERRUPTION_RECOVERY_CLAIMED)
            )
    })
}

fn terminal_chain_requires_archival(root_status: TaskStatus, turn_status: &str) -> bool {
    matches!(
        root_status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    ) || matches!(
        turn_status.trim().to_ascii_lowercase().as_str(),
        "interrupted" | "cancelled" | "canceled"
    )
}

fn task_failure_output(task: &Task) -> Option<String> {
    task.output_refs.iter().find_map(|output| {
        let detail = public_runtime_excerpt(output, TASK_FAILURE_DETAIL_MAX_CHARS);
        (!detail.trim().is_empty()).then_some(detail)
    })
}

fn task_failure_detail(task_store: &TaskStore, root_task: &Task) -> Option<String> {
    if let Some(detail) = task_failure_output(root_task) {
        return Some(detail);
    }

    let mut failed_descendants = task_store
        .get_tasks_by_mission(&root_task.mission_id)
        .into_iter()
        .filter(|task| task.root_task_id == root_task.task_id)
        .filter(|task| task.task_id != root_task.task_id)
        .filter(|task| task.status == TaskStatus::Failed)
        .collect::<Vec<_>>();
    failed_descendants.sort_by_key(|task| task.updated_at.0);
    failed_descendants
        .into_iter()
        .rev()
        .find_map(|task| task_failure_output(&task))
}

fn task_failure_message(task_store: &TaskStore, root_task: &Task) -> String {
    match task_failure_detail(task_store, root_task) {
        Some(detail) => format!("任务执行失败：{detail}"),
        None => format!(
            "任务执行失败，但运行时未记录直接错误信息。失败阶段：task_execution；任务：{}。",
            root_task.task_id
        ),
    }
}

fn update_current_turn_completed_from_root(
    session_store: &SessionStore,
    session_id: &SessionId,
    expected_turn_id: Option<&str>,
) -> Result<(), String> {
    match CanonicalTurnEventSink::for_store(session_store, None)
        .complete_from_root_task(session_id, expected_turn_id)
        .map_err(|error| format!("完成当前 Turn 失败: {error}"))?
    {
        Some(_) => Ok(()),
        None => Err(format!("会话 {} 没有可完成的当前 Turn", session_id)),
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

fn finish_coordinator_after_durable_terminal(
    coordinator: Option<&SessionTurnCoordinator>,
    session_store: &SessionStore,
    session_id: &SessionId,
    expected_turn_id: Option<&str>,
) -> Result<(), String> {
    let Some(coordinator) = coordinator else {
        return Ok(());
    };
    let sidecar = session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| format!("session {session_id} 终态后缺少 runtime sidecar"))?;
    let turn = sidecar
        .current_turn
        .as_ref()
        .ok_or_else(|| format!("session {session_id} 终态后缺少 current Turn"))?;
    if expected_turn_id.is_some_and(|expected| expected != turn.turn_id) {
        return Err(format!(
            "session {session_id} 终态后 current Turn {} 与 expected Turn 不一致",
            turn.turn_id
        ));
    }
    let attempt = coordinator
        .current_attempt(session_id, &turn.turn_id)
        .map_err(|error| {
            format!(
                "Task Turn {} 缺少 Coordinator attempt: {error}",
                turn.turn_id
            )
        })?;
    if attempt.profile != crate::session_turn_coordinator::ExecutionProfile::Task {
        return Err(format!(
            "Task Turn {} 的 Coordinator profile 不是 task",
            turn.turn_id
        ));
    }
    let status = match turn.status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "succeeded" | "success" => CoordinatorTurnStatus::Completed,
        "failed" | "error" => CoordinatorTurnStatus::Failed,
        "cancelled" | "canceled" | "interrupted" => CoordinatorTurnStatus::Cancelled,
        other => return Err(format!("Task Turn {} 终态非法: {other}", turn.turn_id)),
    };
    coordinator
        .execute_command(session_id, TurnCommand::Finish { attempt, status })
        .map(|_| ())
        .map_err(|error| {
            format!(
                "Task Turn {} Coordinator 终态收口失败: {error}",
                turn.turn_id
            )
        })
}

pub fn finalize_background_session_task_turn_if_root_terminal(
    context: FinalizeBackgroundSessionTaskTurnContext<'_>,
) -> Result<bool, String> {
    let FinalizeBackgroundSessionTaskTurnContext {
        session_store,
        event_bus,
        task_store,
        session_id,
        root_task_id,
        runner_status,
        expected_turn_id,
        coordinator,
        persist_session_state,
    } = context;
    match finalize_completed_root_task_turn_for_turn(
        session_store,
        event_bus,
        task_store,
        session_id,
        root_task_id,
        expected_turn_id,
        persist_session_state,
    ) {
        Ok(true) => {
            finish_coordinator_after_durable_terminal(
                coordinator,
                session_store,
                session_id,
                expected_turn_id,
            )?;
            return Ok(true);
        }
        Ok(false) => {}
        Err(error) => return Err(error),
    }

    let Some(task_store) = task_store else {
        return Ok(false);
    };
    let Some(root_task) = task_store.get_task(root_task_id) else {
        return Ok(false);
    };
    let (turn_status, title, message) = match root_task.status {
        TaskStatus::Failed => (
            "failed",
            "任务执行失败",
            task_failure_message(task_store, &root_task),
        ),
        TaskStatus::Killed => (
            "cancelled",
            "任务执行已终止",
            "任务执行已终止。".to_string(),
        ),
        _ if runner_status == "error" => (
            "failed",
            "任务执行失败",
            task_failure_message(task_store, &root_task),
        ),
        _ if runner_status == "stopped" || runner_status == "killed" => (
            "cancelled",
            "任务执行已终止",
            "任务执行已终止。".to_string(),
        ),
        _ => return Ok(false),
    };

    let Some(sidecar) = session_store.runtime_sidecar(session_id) else {
        return Ok(false);
    };
    let Some(active_chain) = sidecar.active_execution_chain.as_ref() else {
        return Ok(false);
    };
    if active_chain.root_task_id != *root_task_id {
        return Ok(false);
    }
    if expected_turn_id.is_some_and(|expected| {
        sidecar
            .current_turn
            .as_ref()
            .is_none_or(|turn| turn.turn_id != expected)
    }) {
        return Ok(false);
    }
    let Some(current_turn) = sidecar.current_turn.as_ref() else {
        return Ok(false);
    };
    let workspace_id = active_chain.workspace_id.clone();
    // Preparation can fail before materialize_dispatch_submission_after_acceptance
    // creates the orchestrator thread.  The accepted canonical Turn already carries
    // the deterministic source thread on its user item; use it as the failure item
    // anchor and keep terminalization independent from a late thread projection.
    let orchestrator_thread_id = session_store
        .orchestrator_thread_for_session(session_id)
        .map(|thread| thread.thread_id)
        .or_else(|| {
            current_turn
                .items
                .first()
                .map(|item| item.source_thread_id.clone())
        })
        .unwrap_or_else(|| ThreadId::new(format!("thread-orchestrator-{session_id}")));
    let existing_error_item_id = current_turn
        .items
        .iter()
        .find(|item| {
            item.kind == "assistant_error" && item.source_thread_id == orchestrator_thread_id
        })
        .map(|item| item.item_id.clone());

    // 已经收口的 Turn 不再重复 checkpoint 或广播同一个错误 item。只有仍占用
    // active chain 的历史终态需要在这里完成一次归档，归档成功后由调用方释放 lease。
    if current_turn_status_is_terminal(&current_turn.status) {
        let archived = if terminal_chain_requires_archival(root_task.status, &current_turn.status) {
            archive_terminal_active_execution_chain(
                session_store,
                Some(task_store),
                session_id,
                root_task_id,
            )?
        } else {
            false
        };
        if archived {
            persist_session_state_checkpoint(persist_session_state, "session_task_chain_archived")?;
        }
        return Ok(archived);
    }

    let item_id = if let Some(item_id) = existing_error_item_id {
        item_id
    } else {
        let item_id = format!("turn-item-assistant-error-{}", UtcMillis::now().0);
        let mut error_item = session_turn_item(
            "assistant_error",
            turn_status,
            Some(title.to_string()),
            Some(message),
            Some(item_id.clone()),
            orchestrator_thread_id.clone(),
        );
        error_item.task_id = Some(root_task_id.clone());
        append_session_turn_item_for_turn(
            session_store,
            session_id,
            expected_turn_id,
            error_item,
            Some(task_store),
        )?
        .ok_or_else(|| {
            format!("终态 Turn 没有可写入的错误 item: session={session_id}, task={root_task_id}")
        })?
        .item
        .item_id
    };

    CanonicalTurnEventSink::for_store(session_store, Some(task_store))
        .set_status_domain(session_id, expected_turn_id, turn_status)
        .map_err(|error| format!("终态 Turn 失败状态提交失败: {error}"))?
        .ok_or_else(|| {
            format!("终态 Turn 没有可更新的失败状态: session={session_id}, task={root_task_id}")
        })?;
    if terminal_chain_requires_archival(root_task.status, turn_status) {
        archive_terminal_active_execution_chain(
            session_store,
            Some(task_store),
            session_id,
            root_task_id,
        )?;
    }
    persist_session_state_checkpoint(persist_session_state, "session_task_turn_failed")?;
    if let Err(error) = publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        &workspace_id,
        &item_id,
        Some(task_store),
    ) {
        return Err(format!("根任务失败时 Turn 终态事件发布失败: {error}"));
    }
    finish_coordinator_after_durable_terminal(
        coordinator,
        session_store,
        session_id,
        expected_turn_id,
    )?;

    Ok(true)
}

pub fn reconcile_terminal_session_task_turns(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
) -> usize {
    reconcile_terminal_session_task_turns_with_coordinator(
        session_store,
        event_bus,
        task_store,
        None,
    )
}

/// daemon 恢复时使用 Coordinator 版本；终态任务只通过 canonical durable mutation
/// 后的统一入口收口 Turn，避免启动 reconcile 又建立一条独立终态路径。
pub fn reconcile_terminal_session_task_turns_with_coordinator(
    session_store: &SessionStore,
    event_bus: &InMemoryEventBus,
    task_store: Option<&TaskStore>,
    coordinator: Option<&SessionTurnCoordinator>,
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
                if current_turn_status_is_completed(&turn.status)
                    && !terminal_chain_requires_archival(root_task.status, &turn.status)
                {
                    return None;
                }
            } else if current_turn_status_is_terminal(&turn.status)
                && !terminal_chain_requires_archival(root_task.status, &turn.status)
            {
                return None;
            }
            Some((
                sidecar.session_id.clone(),
                chain.root_task_id.clone(),
                runner_status,
                turn.turn_id.clone(),
            ))
        })
        .collect::<Vec<_>>();

    candidates
        .into_iter()
        .filter(|(session_id, root_task_id, runner_status, turn_id)| {
            finalize_background_session_task_turn_if_root_terminal(
                FinalizeBackgroundSessionTaskTurnContext {
                    session_store,
                    event_bus,
                    task_store: Some(task_store),
                    session_id,
                    root_task_id,
                    runner_status,
                    expected_turn_id: Some(turn_id),
                    coordinator,
                    persist_session_state: None,
                },
            )
            .is_ok_and(|finalized| finalized)
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
            | "interrupted"
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
    use magi_core::{MissionId, TaskRuntimePayload};
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, SessionExecutionSidecarStatus,
    };

    fn failed_task(
        task_id: &str,
        mission_id: &MissionId,
        root_task_id: &TaskId,
        parent_task_id: Option<TaskId>,
        output_refs: Vec<String>,
    ) -> Task {
        let now = UtcMillis::now();
        Task {
            task_id: TaskId::new(task_id),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id,
            kind: TaskKind::LocalAgent,
            title: "失败任务".to_string(),
            goal: "验证直接错误信息".to_string(),
            status: TaskStatus::Failed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs,
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

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

    #[test]
    fn killed_task_status_uses_cancelled_session_turn_item_status() {
        assert_eq!(
            turn_item_status_for_task_status(TaskStatus::Killed),
            "cancelled"
        );
    }

    #[test]
    fn task_failure_message_exposes_redacted_root_error() {
        let task_store = TaskStore::new();
        let mission_id = MissionId::new("mission-direct-failure");
        let root_task_id = TaskId::new("task-direct-failure");
        let root_task = failed_task(
            root_task_id.as_str(),
            &mission_id,
            &root_task_id,
            None,
            vec![
                "provider timeout at /Users/xie/private/model.json; Authorization: Bearer secret-token"
                    .to_string(),
            ],
        );

        let message = task_failure_message(&task_store, &root_task);

        assert!(message.contains("provider timeout"));
        assert!(message.contains("[path]"));
        assert!(message.contains("[redacted]"));
        assert!(!message.contains("/Users/xie"));
        assert!(!message.contains("secret-token"));
        assert!(!message.contains("目标面板"));
        assert!(!message.contains("右侧代理"));
    }

    #[test]
    fn task_failure_message_uses_failed_descendant_detail() {
        let task_store = TaskStore::new();
        let mission_id = MissionId::new("mission-child-failure");
        let root_task_id = TaskId::new("task-child-failure-root");
        let root_task = failed_task(
            root_task_id.as_str(),
            &mission_id,
            &root_task_id,
            None,
            Vec::new(),
        );
        task_store
            .insert_task(failed_task(
                "task-child-failure-worker",
                &mission_id,
                &root_task_id,
                Some(root_task_id.clone()),
                vec!["工具 file_write 执行失败：permission denied".to_string()],
            ))
            .expect("子任务应插入");

        let message = task_failure_message(&task_store, &root_task);

        assert!(message.contains("file_write"));
        assert!(message.contains("permission denied"));
    }

    #[test]
    fn task_failure_message_states_when_runtime_detail_is_missing() {
        let task_store = TaskStore::new();
        let mission_id = MissionId::new("mission-missing-failure");
        let root_task_id = TaskId::new("task-missing-failure");
        let root_task = failed_task(
            root_task_id.as_str(),
            &mission_id,
            &root_task_id,
            None,
            Vec::new(),
        );

        let message = task_failure_message(&task_store, &root_task);

        assert!(message.contains("运行时未记录直接错误信息"));
        assert!(message.contains(root_task_id.as_str()));
        assert!(!message.contains("目标面板"));
        assert!(!message.contains("右侧代理"));
    }

    #[test]
    fn reconcile_archives_cancelled_terminal_chain_without_losing_turn_history() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(16);
        let task_store = TaskStore::new();
        let session_id = SessionId::new("session-terminal-chain-archive");
        let mission_id = MissionId::new("mission-terminal-chain-archive");
        let root_task_id = TaskId::new("task-terminal-chain-archive");
        let now = UtcMillis::now();

        session_store
            .create_session(session_id.clone(), "terminal chain archive")
            .expect("session should create");
        session_store.ensure_session_mission(&session_id, now, || mission_id.clone());
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-terminal-chain-archive".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: vec![root_task_id.clone()],
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "entry-terminal-chain-archive".to_string(),
                        trimmed_text: Some("停止旧任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-terminal-chain-archive".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        completed_at: Some(now),
                        status: "cancelled".to_string(),
                        user_message: Some("停止旧任务".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("terminal chain should persist");
        task_store
            .insert_task(Task {
                task_id: root_task_id.clone(),
                mission_id,
                root_task_id: root_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::LocalAgent,
                title: "已停止任务".to_string(),
                goal: "验证旧任务不会继续占用会话".to_string(),
                status: TaskStatus::Failed,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            })
            .expect("任务应插入");

        assert_eq!(
            reconcile_terminal_session_task_turns(&session_store, &event_bus, Some(&task_store),),
            1
        );
        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should remain for canonical history");
        assert!(sidecar.active_execution_chain.is_none());
        assert_eq!(sidecar.status, SessionExecutionSidecarStatus::Detached);
        assert_eq!(
            sidecar
                .current_turn
                .as_ref()
                .map(|turn| turn.status.as_str()),
            Some("cancelled")
        );
        assert!(
            session_store
                .ensure_current_turn_acceptance_available(&session_id)
                .is_ok(),
            "终态链收口后必须允许会话直接接受下一条普通消息"
        );
        assert_eq!(
            reconcile_terminal_session_task_turns(&session_store, &event_bus, Some(&task_store),),
            0
        );
    }
}
