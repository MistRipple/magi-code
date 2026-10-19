//! 任务系统 - Task helper functions (visibility / validation / required-tool-chain)
//!
//! conversation-runtime 统一承载 task 可见性、验证判定与 required-tool-chain 纯函数。
//! 本模块严格遵守"无写回依赖"原则——任何需要 session writeback
//! 的 publish/upsert helper 都不放在本模块。

use magi_bridge_client::{ChatToolCall, ChatToolChoice, ChatToolDefinition};
use magi_core::{PlanItemStatus, PlanState, SessionId, Task, TaskKind, ThreadId, WorkerId};
use magi_orchestrator::task_worker_catalog::default_task_role_for_kind;
use magi_orchestrator::{task_store::TaskStore, task_worker_catalog::resolve_task_role};
use magi_session_store::{ActiveExecutionTurnItem, SessionStore};
use magi_tool_runtime::BuiltinToolName;

/// 目标模式的生命周期状态由执行器读取，不由模型提示词推断。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GoalModeLifecycleState {
    /// 当前执行轮次是否仍必须创建 Goal。创建成功后即使 Goal 随后完成，也不能再次创建。
    pub goal_creation_required: bool,
    /// 当前权威计划或 Goal 状态是否要求本轮收口 Goal。
    pub goal_terminalization_required: bool,
}

/// 构造目标模式唯一的生命周期工具链。
///
/// Goal 生命周期工具由运行时拥有顺序，用户输入和旧任务 checkpoint 只能追加业务工具，
/// 不能插入、删除或重排 `get_goal`、`create_goal`、`update_plan`、`update_goal`。
pub fn goal_mode_required_tool_chain(
    state: GoalModeLifecycleState,
    declared: &[String],
) -> Vec<String> {
    let mut required = vec!["get_goal".to_string()];
    if state.goal_creation_required {
        required.push("create_goal".to_string());
    }
    required.push("update_plan".to_string());
    if state.goal_terminalization_required {
        required.push("update_goal".to_string());
    }
    for tool_name in declared {
        let tool_name = canonical_tool_call_name(tool_name);
        if tool_name.is_empty() {
            continue;
        }
        if matches!(
            tool_name.as_str(),
            "get_goal" | "create_goal" | "update_plan" | "update_goal"
        ) {
            continue;
        }
        if !required.iter().any(|existing| existing == &tool_name) {
            required.push(tool_name.clone());
        }
    }
    required
}

/// 读取 Goal/Plan 的权威收口条件，不改写任何持久化状态。
pub fn goal_mode_requires_terminalization(
    session_store: &SessionStore,
    session_id: &SessionId,
) -> bool {
    let Some(goal) = session_store.current_unfinished_goal(session_id) else {
        return false;
    };
    let Some(plan) = session_store.plan(session_id) else {
        return false;
    };
    if plan.goal_id.as_ref() != Some(&goal.goal_id) {
        return false;
    }
    matches!(plan.state, PlanState::Completed | PlanState::Canceled)
        || plan
            .items
            .iter()
            .any(|item| item.status == PlanItemStatus::Blocked)
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
    let Some(task) = task else {
        return false;
    };
    // 共享 Goal/计划/主分支控制只认任务树根 coordinator 身份。
    // 显式授予 worker 的委派权由 task_can_delegate 单独判断，不能据此
    // 取得根身份，也不能靠伪造 coordinator 角色绑定扩大共享控制范围。
    if task.parent_task_id.is_some() || task.root_task_id != task.task_id {
        return false;
    }
    let Some(registry) = registry else {
        return false;
    };
    task_role_id(Some(task))
        .and_then(|role_id| registry.get(role_id))
        .is_some_and(|role| role.coordinator_mode)
}

/// 委派能力独立于 root coordinator 身份；不会获得共享 Goal/计划/Git 权限。
pub(crate) fn task_can_delegate(
    task: Option<&Task>,
    registry: Option<&magi_agent_role::AgentRoleRegistry>,
) -> bool {
    task_is_coordinator(task, registry)
        || task.is_some_and(|task| {
            task.parent_task_id.is_some()
                && task.kind == TaskKind::LocalAgent
                && task.delegation_allowed()
        })
}

pub(crate) fn task_can_see_builtin_tool(
    task: Option<&Task>,
    registry: Option<&magi_agent_role::AgentRoleRegistry>,
    tool: BuiltinToolName,
) -> bool {
    tool.model_call_scope().allows(
        task.is_some(),
        task_is_coordinator(task, registry),
        task_can_delegate(task, registry),
    )
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

/// 任务必须完成的工具链只来自结构化声明（TaskPolicy/入口契约），不从目标文本推断。
pub fn task_required_tool_chain(task: &Task) -> Vec<String> {
    if task.kind != TaskKind::LocalAgent {
        return Vec::new();
    }
    task.required_tool_chain()
        .iter()
        .map(|name| canonical_tool_call_name(name))
        .filter(|name| !name.is_empty())
        .collect()
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

/// 结构化必经动作在完成前只暴露当前一步的工具定义。
///
/// `tool_choice` 只是协议层提示：部分上游只支持 `auto`，无法表达强制调用。
/// 因此运行时必须把已确认的产品动作收敛为单一工具面，确保同一执行契约在不同
/// 模型协议下都成立；动作完成后立即恢复完整工具面，不影响 coordinator 的自主编排。
pub fn required_tool_definitions_for_round(
    tools: &[ChatToolDefinition],
    required_tool_chain: &[String],
    completed_required_tool_names: &[String],
) -> Vec<ChatToolDefinition> {
    let Some(required_tool_name) = required_tool_chain.iter().find(|tool_name| {
        !completed_required_tool_names
            .iter()
            .any(|completed| completed == *tool_name)
    }) else {
        return tools.to_vec();
    };

    let constrained_tools = tools
        .iter()
        .filter(|tool| tool.function.name == *required_tool_name)
        .cloned()
        .collect::<Vec<_>>();

    if constrained_tools.is_empty() {
        tools.to_vec()
    } else {
        constrained_tools
    }
}

/// 目标模式的严格工具面。
///
/// 目标模式不能因为当前工具面缺少必经步骤而改用完整工具面。完整工具面会
/// 让模型绕过 Goal/Plan 生命周期，随后再由结果校验被动发现，属于 fail-open。
/// 普通任务的恢复工具仍使用 `required_tool_definitions_for_round`，只有目标模式
/// 走这个 fail-closed 版本。
pub fn strict_goal_mode_tool_definitions_for_round(
    tools: &[ChatToolDefinition],
    required_tool_chain: &[String],
    completed_required_tool_names: &[String],
) -> Vec<ChatToolDefinition> {
    let Some(required_tool_name) = required_tool_chain.iter().find(|tool_name| {
        !completed_required_tool_names
            .iter()
            .any(|completed| completed == *tool_name)
    }) else {
        return tools.to_vec();
    };

    tools
        .iter()
        .filter(|tool| tool.function.name == *required_tool_name)
        .cloned()
        .collect()
}

pub(crate) const AGENT_SPAWN_LIFECYCLE_TOOLS: [BuiltinToolName; 2] =
    [BuiltinToolName::AgentWait, BuiltinToolName::AgentCancel];

/// 创建能力依赖结果收集和取消能力。只收紧 spawn，不替 Task/Skill 开放缺失工具，
/// 也不隐藏仍可用于管理已有代理的工具。应在阶段临时筛选前执行。
pub(crate) fn enforce_agent_coordination_surface(
    mut tools: Vec<ChatToolDefinition>,
) -> Vec<ChatToolDefinition> {
    if AGENT_SPAWN_LIFECYCLE_TOOLS.iter().any(|required| {
        !tools
            .iter()
            .any(|tool| tool.function.name == required.as_str())
    }) {
        tools.retain(|tool| tool.function.name != "agent_spawn");
    }
    tools
}

/// 校验目标模式当前轮的工具调用批次。
///
/// 目标生命周期（get_goal / create_goal / update_plan 等必经步骤）未完成时是严格的单步状态机：
/// 只能调用当前必经工具，且每轮一个。生命周期完成后进入业务阶段，模型可以像普通任务一样
/// 在一轮里并行调用多个工具（读文件、搜索等），否则分析类目标一并行就被拒绝并整体受阻；
/// 唯一保留的约束是 `create_goal` / `update_goal` 这类改变目标本身状态的工具必须单独调用，
/// 不能和其它工具混在同一批里，避免目标被收口后同批的副作用仍在执行。
/// 校验必须发生在任何工具执行之前。
pub fn goal_mode_tool_batch_violation(
    required_tool_chain: &[String],
    completed_required_tool_names: &[String],
    tool_calls: &[ChatToolCall],
) -> Option<String> {
    let next_required_tool = required_tool_chain.iter().find(|tool_name| {
        !completed_required_tool_names
            .iter()
            .any(|completed| completed == *tool_name)
    });

    let Some(next_required_tool) = next_required_tool else {
        if tool_calls.len() > 1
            && let Some(control_tool) = tool_calls
                .iter()
                .map(|call| canonical_tool_call_name(&call.function.name))
                .find(|name| matches!(name.as_str(), "create_goal" | "update_goal"))
        {
            return Some(format!(
                "目标控制工具 {control_tool} 必须单独调用，实际与其它 {} 个工具调用混在同一批；已拒绝执行，避免目标状态变更与其它副作用同批发生。",
                tool_calls.len() - 1
            ));
        }
        return None;
    };

    if tool_calls.len() > 1 {
        return Some(format!(
            "目标生命周期未完成时每轮只能调用一个工具（当前必须调用 {next_required_tool}），实际收到 {} 个工具调用；已拒绝执行，避免批量副作用或跳过生命周期。",
            tool_calls.len()
        ));
    }
    if tool_calls.is_empty() {
        return None;
    }

    let actual_tool = canonical_tool_call_name(&tool_calls[0].function.name);
    if actual_tool != *next_required_tool {
        return Some(format!(
            "严格目标模式当前必须先调用 {next_required_tool}，实际收到 {actual_tool}；已拒绝执行，避免跳过 Goal/Plan 生命周期。"
        ));
    }

    None
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

pub fn canonical_tool_call_name(tool_name: &str) -> String {
    BuiltinToolName::from_name(tool_name.trim())
        .map(|tool| tool.as_str().to_string())
        .unwrap_or_else(|| tool_name.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{ChatToolFunction, ChatToolFunctionDefinition, ChatToolOrigin};

    fn tool(name: &str) -> ChatToolDefinition {
        ChatToolDefinition {
            kind: "function".to_string(),
            function: ChatToolFunctionDefinition {
                name: name.to_string(),
                description: name.to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
            origin: ChatToolOrigin::Builtin,
        }
    }

    #[test]
    fn required_tool_surface_exposes_only_current_contract_step() {
        let tools = enforce_agent_coordination_surface(vec![
            tool("file_read"),
            tool("agent_spawn"),
            tool("agent_wait"),
            tool("agent_cancel"),
        ]);
        let required = vec!["agent_spawn".to_string(), "agent_wait".to_string()];

        let first = required_tool_definitions_for_round(&tools, &required, &[]);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].function.name, "agent_spawn");

        let second =
            required_tool_definitions_for_round(&tools, &required, &["agent_spawn".to_string()]);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].function.name, "agent_wait");

        let restored = required_tool_definitions_for_round(&tools, &required, &required);
        assert_eq!(restored.len(), tools.len());
    }

    fn call(name: &str) -> ChatToolCall {
        ChatToolCall {
            id: format!("call-{name}"),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn strict_goal_tool_surface_fails_closed_when_required_tool_is_missing() {
        let tools = vec![tool("get_goal"), tool("shell_exec")];
        let visible = strict_goal_mode_tool_definitions_for_round(
            &tools,
            &["update_plan".to_string(), "shell_exec".to_string()],
            &[],
        );
        assert!(visible.is_empty());
    }

    #[test]
    fn strict_goal_tool_batch_rejects_duplicates_and_skips() {
        let required = ["get_goal".to_string(), "update_plan".to_string()];
        assert!(goal_mode_tool_batch_violation(&required, &[], &[call("get_goal")]).is_none());
        assert!(
            goal_mode_tool_batch_violation(&required, &[], &[call("get_goal"), call("get_goal")])
                .is_some()
        );
        assert!(goal_mode_tool_batch_violation(&required, &[], &[call("update_plan")]).is_some());
        assert!(
            goal_mode_tool_batch_violation(
                &required,
                &["get_goal".to_string()],
                &[call("update_plan")]
            )
            .is_none()
        );
        assert!(
            goal_mode_tool_batch_violation(&required, &required, &[call("shell_exec")]).is_none()
        );
        assert!(
            goal_mode_tool_batch_violation(
                &required,
                &required,
                &[call("shell_exec"), call("file_read")]
            )
            .is_none(),
            "Goal 生命周期完成后业务工具可以并行调用"
        );
        assert!(
            goal_mode_tool_batch_violation(
                &required,
                &required,
                &[call("update_goal"), call("file_read")]
            )
            .is_some(),
            "目标控制工具不能和其它工具混在同一批"
        );
        assert!(
            goal_mode_tool_batch_violation(&required, &[], &[call("get_goal"), call("file_read")])
                .is_some(),
            "生命周期未完成时仍然每轮只允许一个工具"
        );
    }

    #[test]
    fn goal_mode_lifecycle_owns_order_and_cannot_be_reordered_by_declared_tools() {
        let chain = goal_mode_required_tool_chain(
            GoalModeLifecycleState {
                goal_creation_required: true,
                goal_terminalization_required: true,
            },
            &[
                "update_goal".to_string(),
                "shell_exec".to_string(),
                "create_goal".to_string(),
                "update_plan".to_string(),
            ],
        );

        assert_eq!(
            chain,
            [
                "get_goal",
                "create_goal",
                "update_plan",
                "update_goal",
                "shell_exec"
            ]
        );
    }

    #[test]
    fn goal_mode_creation_is_not_reintroduced_after_goal_terminalization() {
        let chain = goal_mode_required_tool_chain(
            GoalModeLifecycleState {
                goal_creation_required: false,
                goal_terminalization_required: false,
            },
            &[],
        );

        assert_eq!(chain, ["get_goal", "update_plan"]);
        assert!(!chain.iter().any(|tool| *tool == "create_goal"));
    }

    #[test]
    fn missing_required_tool_does_not_hide_available_diagnostics_surface() {
        let tools = vec![tool("file_read")];
        let visible =
            required_tool_definitions_for_round(&tools, &["agent_spawn".to_string()], &[]);

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].function.name, "file_read");
    }
}
