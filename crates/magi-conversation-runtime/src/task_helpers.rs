//! Task System v2 - Task helper functions (visibility / validation / required-tool-chain)
//!
//! P7：从 magi-api task_llm_loop.rs 平移过来的纯函数与小型可见性枚举，
//! 由 conversation-runtime 统一承载，magi-api 通过 re-export 调用。
//! 本模块严格遵守"无写回依赖"原则——任何需要 session_turn_writeback
//! 的 publish/upsert helper 不在本模块，留待 M06 与 writeback 一同迁入 v2。

use magi_bridge_client::{ChatToolChoice, ChatToolDefinition};
use magi_core::{Task, TaskKind, ThreadId, WorkerId};
use magi_orchestrator::{task_store::TaskStore, task_worker_catalog::resolve_task_role};
use magi_session_store::ActiveExecutionTurnItem;
use magi_tool_runtime::BuiltinToolName;

/// task_llm_loop / session_turn 默认工具调用轮数上限：避免一次任务被模型反复 fanout。
pub const BASE_TOOL_CALL_ROUNDS: usize = 16;
/// 强制工具链 + base 上限交叠时的硬天花板，防止失控放大。
pub const MAX_TOOL_CALL_ROUNDS: usize = 32;

/// 单一可见性枚举：item 的归属由 `source_thread_id` 决定，本枚举仅承担
/// "把该 task 的 turn item 写到主线 thread 还是 worker drawer thread"的派发判断。
/// - `Mainline`：item.source_thread_id = orchestrator thread，前端 projection
///   会把它归到主线时间线。orchestrator 自身 turn 与"无独立 worker drawer 的子任务"
///   都走这条路径。
/// - `Sidechain`：item.source_thread_id = lane 绑定的 worker thread，归到对应
///   role 的 drawer。primary worker sidechain（同一 turn 内主 dispatch 拉起的
///   worker 任务）会同时在主线 publish `worker_status` 摘要 item（其 source_thread_id
///   仍是 orchestrator）以填充 dispatch 卡 liveActivity。
#[derive(Clone, Debug)]
pub enum TaskTurnVisibility {
    Mainline {
        /// 主线 thread = session 的 orchestrator thread。所有 mainline item 写到这里。
        thread_id: ThreadId,
    },
    Sidechain {
        /// drawer thread = role 维度的 worker thread。所有 sidechain item 写到这里。
        thread_id: ThreadId,
        /// 主线常驻 thread，用于 `publish_worker_lane_summary` 等场景把摘要写回主线。
        orchestrator_thread_id: ThreadId,
        role_id: String,
        worker_id: WorkerId,
        lane_id: String,
        lane_seq: Option<usize>,
        /// 当前 sidechain 是否同时为主线 dispatch 卡的 primary worker：是则需要
        /// 在主线 publish 摘要 item；否则 drawer 里安静执行不污染主线。
        has_mainline_summary: bool,
    },
}

impl TaskTurnVisibility {
    pub fn thread_id(&self) -> &ThreadId {
        match self {
            Self::Mainline { thread_id } => thread_id,
            Self::Sidechain { thread_id, .. } => thread_id,
        }
    }

    pub fn is_mainline(&self) -> bool {
        matches!(self, Self::Mainline { .. })
    }

    /// worker 执行下发工具调用时需要传入 worker_id（影响 executor 分派）。
    /// Mainline task 不绑定 worker。
    pub fn worker_id(&self) -> Option<&WorkerId> {
        match self {
            Self::Mainline { .. } => None,
            Self::Sidechain { worker_id, .. } => Some(worker_id),
        }
    }
}

pub fn task_turn_visibility(
    task: &Task,
    worker_lane_id: Option<&str>,
    worker_lane_seq: Option<usize>,
    worker_id: Option<&WorkerId>,
    thread_id: &ThreadId,
    orchestrator_thread_id: &ThreadId,
    primary_worker_sidechain: bool,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
) -> TaskTurnVisibility {
    let lane_id = worker_lane_id
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .map(ToOwned::to_owned);
    if let (Some(lane_id), Some(worker_id)) = (lane_id, worker_id) {
        let role_id = resolve_task_role(task, agent_role_registry)
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(ToOwned::to_owned)
            .expect("worker drawer task must carry resolvable role_id");
        return TaskTurnVisibility::Sidechain {
            thread_id: thread_id.clone(),
            orchestrator_thread_id: orchestrator_thread_id.clone(),
            role_id,
            worker_id: worker_id.clone(),
            lane_id,
            lane_seq: worker_lane_seq,
            has_mainline_summary: primary_worker_sidechain,
        };
    }
    TaskTurnVisibility::Mainline {
        thread_id: thread_id.clone(),
    }
}

pub fn apply_task_turn_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &Task,
    visibility: &TaskTurnVisibility,
) {
    item.task_id = Some(task.task_id.clone());
    match visibility {
        TaskTurnVisibility::Mainline { thread_id } => {
            item.source_thread_id = thread_id.clone();
        }
        TaskTurnVisibility::Sidechain {
            thread_id,
            role_id,
            worker_id,
            lane_id,
            lane_seq,
            ..
        } => {
            item.source_thread_id = thread_id.clone();
            item.lane_id = Some(lane_id.clone());
            item.lane_seq = *lane_seq;
            item.worker_id = Some(worker_id.clone());
            item.role_id = Some(role_id.clone());
            item.source = role_id.clone();
        }
    }
}

/// worker 执行细节（thinking / stream / tool / 失败原因等）一律写入 drawer：
/// 即便上层 caller 误判为 Mainline，只要该 task 关联到 lane 即被强制视作 sidechain。
/// 保证 drawer 永远拿到完整 transcript，主线只承载摘要。
pub fn apply_task_worker_detail_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &Task,
    visibility: &TaskTurnVisibility,
) {
    apply_task_turn_visibility(item, task, visibility);
}

/// final 回复的归属规则与执行细节一致：sidechain task 的 final 永远只归 worker drawer，
/// 主线消费的是 `worker_status` 摘要 item（由 publish_worker_lane_summary 写入）。
pub fn apply_task_final_visibility(
    item: &mut ActiveExecutionTurnItem,
    task_store: &TaskStore,
    task: &Task,
    visibility: &TaskTurnVisibility,
) {
    apply_task_turn_visibility(item, task, visibility);
    let _ = task_store;
}

pub fn task_tool_failure_reason(
    task_kind: TaskKind,
    failed_tool_summaries: &[String],
) -> Option<String> {
    if task_kind == TaskKind::Validation || failed_tool_summaries.is_empty() {
        return None;
    }
    let compact = failed_tool_summaries
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let suffix = if failed_tool_summaries.len() > 3 {
        format!("；另有 {} 个工具失败", failed_tool_summaries.len() - 3)
    } else {
        String::new()
    };
    Some(format!("工具执行失败，任务不能标记完成：{compact}{suffix}"))
}

pub fn validation_result_rejects_delivery(content: &str) -> bool {
    let leading = content.trim_start().chars().take(240).collect::<String>();
    let lower = leading.to_ascii_lowercase();
    let normalized = leading
        .chars()
        .filter(|ch| !matches!(ch, '*' | '_' | '`' | '#' | '>' | ' ' | '\t' | '\r' | '\n'))
        .collect::<String>();
    let negative_markers = [
        "不通过",
        "未通过",
        "部分通过",
        "验收未通过",
        "验证未通过",
        "无法确认",
        "未能确认",
        "不能判定",
        "不满足",
    ];
    negative_markers
        .iter()
        .any(|marker| normalized.contains(marker))
        || lower.starts_with("failed")
        || lower.starts_with("failure")
        || lower.starts_with("not passed")
        || lower.contains("not passed")
        || lower.contains("does not pass")
}

pub fn compact_validation_failure(content: &str) -> String {
    let trimmed = content.trim();
    let compact = trimmed.chars().take(240).collect::<String>();
    if trimmed.chars().count() > 240 {
        format!("验证未通过: {compact}…")
    } else {
        format!("验证未通过: {compact}")
    }
}

pub fn deterministic_task_final_content(task: &Task, task_store: &TaskStore) -> Option<String> {
    if is_planning_no_tool_action(task) {
        return Some(deterministic_planning_content(task));
    }
    if is_planning_text_validation(task) {
        return deterministic_planning_validation_content(task, task_store);
    }
    if is_execution_tool_validation(task) {
        return deterministic_execution_tool_validation_content(task, task_store);
    }
    None
}

pub fn is_planning_no_tool_action(task: &Task) -> bool {
    task.kind == TaskKind::Action
        && task.title.contains("梳理目标")
        && task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.command_mode.eq_ignore_ascii_case("no_tools"))
        && task.dependency_ids.is_empty()
}

pub fn deterministic_planning_content(task: &Task) -> String {
    let goal = extract_task_goal(&task.goal).unwrap_or_else(|| task.goal.trim().to_string());
    format!(
        "目标：{goal}\n\n边界：规划步骤只整理目标、边界、执行计划和验收标准，不调用工具，不执行文件、shell 或网络操作。\n\n执行计划：执行步骤负责按用户目标调用工具并产生可验证结果；交付步骤只基于执行产出总结，不重复调用工具。\n\n验收标准：规划文本必须包含目标、边界、执行计划、验收标准四部分；执行结果必须以真实工具结果为准，失败或阻塞不得伪装成功。"
    )
}

pub fn is_planning_text_validation(task: &Task) -> bool {
    task.kind == TaskKind::Validation && task.goal.contains("只验证规划文本完整性")
}

pub fn deterministic_planning_validation_content(
    task: &Task,
    task_store: &TaskStore,
) -> Option<String> {
    let dependency_text = task
        .dependency_ids
        .iter()
        .filter_map(|dependency_id| task_store.get_task(dependency_id))
        .flat_map(|dependency| dependency.output_refs)
        .collect::<Vec<_>>()
        .join("\n\n");
    let has_required_sections = ["目标：", "边界：", "执行计划：", "验收标准："]
        .iter()
        .all(|section| dependency_text.contains(section));
    has_required_sections.then(|| {
        "通过。规划文本已包含目标、边界、执行计划和验收标准；本步骤未验证后续执行结果、文件内容或工作区变更。".to_string()
    })
}

pub fn is_execution_tool_validation(task: &Task) -> bool {
    task.kind == TaskKind::Validation && task.goal.contains("实际执行和工具结果")
}

pub fn deterministic_execution_tool_validation_content(
    task: &Task,
    task_store: &TaskStore,
) -> Option<String> {
    let dependencies = task
        .dependency_ids
        .iter()
        .filter_map(|dependency_id| task_store.get_task(dependency_id))
        .collect::<Vec<_>>();
    if dependencies.is_empty() {
        return None;
    }

    let mut required_tools = Vec::new();
    let mut observed_tools = Vec::new();
    let mut failed_tools = Vec::new();
    let mut has_final_text = false;

    for dependency in dependencies {
        for tool_name in task_required_tool_chain(&dependency) {
            if !required_tools.iter().any(|existing| existing == &tool_name) {
                required_tools.push(tool_name);
            }
        }
        for output in dependency.output_refs {
            collect_dependency_output_validation_facts(
                &output,
                &mut observed_tools,
                &mut failed_tools,
                &mut has_final_text,
            );
        }
    }

    let missing_tools = required_tools
        .iter()
        .filter(|tool_name| !observed_tools.iter().any(|observed| observed == *tool_name))
        .cloned()
        .collect::<Vec<_>>();

    if !failed_tools.is_empty() || !missing_tools.is_empty() || !has_final_text {
        return None;
    }

    let tools = if observed_tools.is_empty() {
        "无工具调用".to_string()
    } else {
        observed_tools.join(", ")
    };
    Some(format!(
        "通过。已基于依赖任务的结构化输出核验当前执行产物，工具调用均成功且最终回复已生成；已验证工具：{tools}。"
    ))
}

pub fn collect_dependency_output_validation_facts(
    output: &str,
    observed_tools: &mut Vec<String>,
    failed_tools: &mut Vec<String>,
    has_final_text: &mut bool,
) {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        *has_final_text = true;
        return;
    };
    let Some(blocks) = value.get("blocks").and_then(serde_json::Value::as_array) else {
        if !trimmed.is_empty() {
            *has_final_text = true;
        }
        return;
    };
    for block in blocks {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("tool_call") => {
                let Some(tool_call) = block.get("toolCall") else {
                    continue;
                };
                let Some(tool_name) = tool_call
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(canonical_tool_call_name)
                else {
                    continue;
                };
                if !observed_tools.iter().any(|observed| observed == &tool_name) {
                    observed_tools.push(tool_name.clone());
                }
                let status = tool_call
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if status != "success" {
                    failed_tools.push(tool_name.clone());
                    continue;
                }
                let result_status = tool_call
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|result| serde_json::from_str::<serde_json::Value>(result).ok())
                    .and_then(|result| {
                        result
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    });
                if result_status
                    .as_deref()
                    .is_some_and(|status| status != "succeeded")
                {
                    failed_tools.push(tool_name);
                }
            }
            Some("text") => {
                if block
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|content| !content.trim().is_empty())
                {
                    *has_final_text = true;
                }
            }
            _ => {}
        }
    }
}

pub fn extract_task_goal(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("<<<MAGI_TASK_GOAL>>>")?;
    let (goal, _) = rest.split_once("<<<END_MAGI_TASK_GOAL>>>")?;
    Some(
        goal.trim()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn task_required_tool_chain(task: &Task) -> Vec<String> {
    if task.kind != TaskKind::Action {
        return Vec::new();
    }
    if task
        .policy_snapshot
        .as_ref()
        .is_some_and(|policy| !policy.command_mode.eq_ignore_ascii_case("full"))
    {
        return Vec::new();
    }
    let normalized = task.goal.to_ascii_lowercase();
    let mut matches: Vec<(&'static str, usize)> = Vec::new();
    for (alias, canonical_name) in public_builtin_tool_reference_aliases() {
        let Some(position) = tool_reference_position(&normalized, alias) else {
            continue;
        };
        if let Some((_, existing_position)) =
            matches.iter_mut().find(|(name, _)| *name == canonical_name)
        {
            *existing_position = (*existing_position).min(position);
        } else {
            matches.push((canonical_name, position));
        }
    }
    matches.sort_by_key(|(_, position)| *position);
    matches
        .into_iter()
        .map(|(tool_name, _)| tool_name.to_string())
        .collect()
}

pub fn public_builtin_tool_reference_aliases() -> Vec<(&'static str, &'static str)> {
    let mut aliases = Vec::new();
    for tool in BuiltinToolName::ALL {
        if tool.is_public_tool_surface() {
            let name = tool.as_str();
            aliases.push((name, name));
        }
    }
    aliases.extend([
        ("file_view", "file_read"),
        ("file_create", "file_write"),
        ("file_edit", "file_patch"),
        ("file_insert", "file_patch"),
        ("code_search_regex", "search_text"),
        ("code_search_semantic", "search_semantic"),
        ("shell", "shell_exec"),
        ("project_knowledge_query", "knowledge_query"),
    ]);
    aliases
}

pub fn tool_reference_position(text: &str, tool_name: &str) -> Option<usize> {
    text.match_indices(tool_name).find_map(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + tool_name.len()..].chars().next();
        (is_tool_reference_boundary(before) && is_tool_reference_boundary(after)).then_some(start)
    })
}

pub fn is_tool_reference_boundary(value: Option<char>) -> bool {
    value
        .map(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
        .unwrap_or(true)
}

pub fn forced_task_tool_choice_for_round(
    required_tool_chain: &[String],
    tools: Option<&Vec<ChatToolDefinition>>,
    completed_required_tool_names: &[String],
) -> Option<ChatToolChoice> {
    let forced_tool_name = required_tool_chain
        .iter()
        .find(|tool_name| {
            !completed_required_tool_names
                .iter()
                .any(|completed| completed == *tool_name)
        })?
        .trim();
    if forced_tool_name.is_empty() {
        return None;
    }
    let tool_is_available = tools
        .map(|definitions| {
            definitions
                .iter()
                .any(|definition| definition.function.name == forced_tool_name)
        })
        .unwrap_or(false);
    tool_is_available.then(|| ChatToolChoice::force_function(forced_tool_name))
}

pub fn record_completed_required_tools(
    completed: &mut Vec<String>,
    required_tool_chain: &[String],
    tool_call_names: &[String],
) {
    for tool_name in tool_call_names {
        if !required_tool_chain
            .iter()
            .any(|required| required == tool_name)
        {
            continue;
        }
        if !completed
            .iter()
            .any(|completed_name| completed_name == tool_name)
        {
            completed.push(tool_name.clone());
        }
    }
}

pub fn required_tool_chain_is_complete(
    required_tool_chain: &[String],
    completed: &[String],
) -> bool {
    required_tool_chain.iter().all(|required| {
        completed
            .iter()
            .any(|completed_name| completed_name == required)
    })
}

pub fn required_tool_chain_recovery_prompt(
    required_tool_chain: &[String],
    completed: &[String],
) -> String {
    let missing = required_tool_chain
        .iter()
        .filter(|required| {
            !completed
                .iter()
                .any(|completed_name| completed_name == *required)
        })
        .cloned()
        .collect::<Vec<_>>();
    format!(
        "上一轮提前给出了文字回复，但当前 action 明确要求调用的内置工具链尚未完成。已完成：{}。仍需继续调用：{}。请继续调用下一个缺失工具，不要总结。",
        if completed.is_empty() {
            "无".to_string()
        } else {
            completed.join(", ")
        },
        missing.join(", ")
    )
}

pub fn tool_call_round_limit(required_tool_chain: &[String]) -> usize {
    BASE_TOOL_CALL_ROUNDS
        .max(required_tool_chain.len().saturating_add(2))
        .min(MAX_TOOL_CALL_ROUNDS)
}

pub fn canonical_tool_call_name(tool_name: &str) -> String {
    BuiltinToolName::from_str(tool_name.trim())
        .map(|tool| tool.as_str().to_string())
        .unwrap_or_else(|| tool_name.trim().to_string())
}
