//! 任务系统 - Task helper functions (visibility / validation / required-tool-chain)
//!
//! conversation-runtime 统一承载 task 可见性、验证判定与 required-tool-chain 纯函数。
//! 本模块严格遵守"无写回依赖"原则——任何需要 session writeback
//! 的 publish/upsert helper 都不放在本模块。

use magi_bridge_client::{ChatToolChoice, ChatToolDefinition};
use magi_core::{Task, TaskKind, TaskTier, ThreadId, WorkerId};
use magi_orchestrator::task_worker_catalog::default_task_role_for_kind;
use magi_orchestrator::{task_store::TaskStore, task_worker_catalog::resolve_task_role};
use magi_session_store::ActiveExecutionTurnItem;
use magi_tool_runtime::BuiltinToolName;

/// conversation_loop / session_turn 默认工具调用轮数上限：避免一次任务被模型反复 fanout。
pub const BASE_TOOL_CALL_ROUNDS: usize = 16;
/// 强制工具链 + base 上限交叠时的硬天花板，防止失控放大。
pub const MAX_TOOL_CALL_ROUNDS: usize = 32;

pub(crate) fn is_orchestration_builtin_tool(tool: BuiltinToolName) -> bool {
    matches!(
        tool,
        BuiltinToolName::AgentSpawn
            | BuiltinToolName::TodoWrite
            | BuiltinToolName::MemoryWrite
            | BuiltinToolName::AgentWait
    )
}

pub(crate) fn is_long_mission_builtin_tool(tool: BuiltinToolName) -> bool {
    matches!(
        tool,
        BuiltinToolName::MissionCharterWrite
            | BuiltinToolName::PlanWrite
            | BuiltinToolName::KgWrite
            | BuiltinToolName::ValidationRecord
            | BuiltinToolName::Checkpoint
            | BuiltinToolName::HumanCheckpointRequest
    )
}

pub(crate) fn task_role_id(task: Option<&Task>) -> Option<&str> {
    let task = task?;
    task.executor_binding_target_role()
        .or_else(|| default_task_role_for_kind(task.kind))
}

pub(crate) fn task_is_coordinator(
    task: Option<&Task>,
    registry: Option<&magi_agent_role::AgentRoleRegistry>,
) -> bool {
    let Some(registry) = registry else {
        return false;
    };
    task_role_id(task)
        .and_then(|role_id| registry.get(role_id))
        .is_some_and(|role| role.coordinator_mode)
}

pub(crate) fn task_is_long_mission(task: Option<&Task>) -> bool {
    task.and_then(|task| task.policy_snapshot.as_ref())
        .is_some_and(|policy| policy.task_tier == TaskTier::LongMission)
}

pub(crate) fn task_can_see_builtin_tool(
    task: Option<&Task>,
    registry: Option<&magi_agent_role::AgentRoleRegistry>,
    tool: BuiltinToolName,
) -> bool {
    if is_long_mission_builtin_tool(tool) {
        return task_is_coordinator(task, registry) && task_is_long_mission(task);
    }
    if is_orchestration_builtin_tool(tool) {
        return task_is_coordinator(task, registry);
    }
    true
}

/// 单一可见性枚举：item 的归属由 `source_thread_id` 决定，本枚举仅承担
/// "把该 task 的 turn item 写到主线 thread 还是 task 详情 thread"的派发判断。
/// - `Mainline`：item.source_thread_id = orchestrator thread，前端 projection
///   会把它归到主线时间线。orchestrator 自身 turn 与无独立详情页的子任务
///   都走这条路径。
/// - `Sidechain`：item.source_thread_id = task thread，归到对应代理详情。
///   agent_spawn 的主线可见部分由父代理 ToolCall 卡承接展示，
///   sidechain 不再向主线写摘要 item。代理与父代理的 item 在前端按 `metadata.taskId`
///   过滤到 RightPane 子标签，主线仅 turnSeq/itemSeq 排序，因此本变体不再持有
///   lane_id/lane_seq——它们随 Task #105 退役。
#[derive(Clone, Debug)]
pub enum TaskTurnVisibility {
    Mainline {
        /// 主线 thread = session 的 orchestrator thread。所有 mainline item 写到这里。
        thread_id: ThreadId,
    },
    Sidechain {
        /// task thread = 代理本次执行的独占 thread。所有 sidechain item 写到这里。
        thread_id: ThreadId,
        role_id: String,
        worker_id: WorkerId,
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
    is_sidechain: bool,
    worker_id: Option<&WorkerId>,
    thread_id: &ThreadId,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
) -> TaskTurnVisibility {
    if let (true, Some(worker_id)) = (is_sidechain, worker_id) {
        let role_id = resolve_task_role(task, agent_role_registry)
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(ToOwned::to_owned)
            .expect("sidechain task must carry resolvable role_id");
        return TaskTurnVisibility::Sidechain {
            thread_id: thread_id.clone(),
            role_id,
            worker_id: worker_id.clone(),
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
            ..
        } => {
            item.source_thread_id = thread_id.clone();
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

/// final 回复的归属规则与执行细节一致：代理 task 的 final 永远只归 task 详情。
/// 主线由父代理的 agent_spawn ToolCall 卡承接代理入口，sidechain 不再向主线写摘要。
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
    let _ = task_kind;
    if failed_tool_summaries.is_empty() {
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
    task.kind == TaskKind::LocalAgent
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
    task.kind == TaskKind::LocalAgent && task.goal.contains("只验证规划文本完整性")
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
    task.kind == TaskKind::LocalAgent && task.goal.contains("实际执行和工具结果")
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
        for tool_name in task_required_tool_chain(&dependency, None) {
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

pub fn task_required_tool_chain(
    task: &Task,
    registry: Option<&magi_agent_role::AgentRoleRegistry>,
) -> Vec<String> {
    if task.kind != TaskKind::LocalAgent {
        return Vec::new();
    }
    if task.executor_binding_target_role() == Some("coordinator")
        || task_is_coordinator(Some(task), registry)
    {
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
    for (canonical_name, position) in semantic_required_tool_references(&normalized) {
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

fn semantic_required_tool_references(text: &str) -> Vec<(&'static str, usize)> {
    let mut matches = Vec::new();
    push_first_semantic_match(
        &mut matches,
        "file_write",
        text,
        &[
            "创建文件",
            "新建文件",
            "写入文件",
            "写文件",
            "生成文件",
            "保存到",
            "文件内容必须包含",
            "create file",
            "create a file",
            "write file",
            "write a file",
            "file content must contain",
        ],
    );
    push_first_semantic_match(
        &mut matches,
        "file_read",
        text,
        &[
            "读取该文件",
            "读取文件",
            "读回文件",
            "验证内容",
            "校验内容",
            "read the file",
            "read file",
            "verify content",
            "validate content",
        ],
    );
    matches
}

fn push_first_semantic_match(
    matches: &mut Vec<(&'static str, usize)>,
    tool_name: &'static str,
    text: &str,
    phrases: &[&str],
) {
    let Some(position) = phrases.iter().filter_map(|phrase| text.find(phrase)).min() else {
        return;
    };
    matches.push((tool_name, position));
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
