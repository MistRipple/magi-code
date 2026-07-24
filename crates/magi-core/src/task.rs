use serde::{Deserialize, Serialize};

use crate::ids::{MissionId, PlanItemId, TaskId};
use crate::value_objects::UtcMillis;

/// 子代理上下文引用类型。引用只描述来源，不隐式展开正文；需要正文时必须通过
/// `context_read` 显式读取。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentContextReferenceKind {
    ConversationTurn,
    TaskOutput,
    TaskEvidence,
    File,
    Knowledge,
    Other,
}

/// 子代理上下文包中的一条结构化引用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentContextReference {
    pub reference_id: String,
    pub kind: AgentContextReferenceKind,
    pub title: String,
    pub source_ref: String,
    pub preview: String,
    #[serde(default)]
    pub estimated_tokens: usize,
}

/// 主线在代理运行期间发送的上下文补充。补充进入同一个上下文包并递增 revision，
/// 不建立第二套临时消息事实源。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentContextSupplement {
    pub supplement_id: String,
    pub author_task_id: TaskId,
    pub message: String,
    #[serde(default)]
    pub references: Vec<AgentContextReference>,
    #[serde(default)]
    pub estimated_tokens: usize,
    pub created_at: UtcMillis,
}

/// 子代理唯一的启动上下文合同。它只包含当前子任务需要的摘要、约束、交付定义和
/// 显式引用；主会话近期记录不再自动复制到子代理 prompt。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentContextPackage {
    pub package_id: String,
    pub revision: u64,
    pub parent_task_id: TaskId,
    pub summary: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    pub expected_output: String,
    #[serde(default)]
    pub references: Vec<AgentContextReference>,
    #[serde(default)]
    pub supplements: Vec<AgentContextSupplement>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl AgentContextPackage {
    pub fn render_for_prompt(&self) -> String {
        let constraints = if self.constraints.is_empty() {
            "- 无额外约束".to_string()
        } else {
            self.constraints
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let references = if self.references.is_empty() {
            "- 无；需要主会话或同一执行链信息时使用 context_search/context_read".to_string()
        } else {
            self.references
                .iter()
                .map(|item| {
                    format!(
                        "- [{}] {} ({:?}, 约 {} tokens): {}",
                        item.reference_id,
                        item.title,
                        item.kind,
                        item.estimated_tokens,
                        item.preview
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let supplements = if self.supplements.is_empty() {
            String::new()
        } else {
            format!(
                "\n补充上下文：\n{}",
                self.supplements
                    .iter()
                    .map(|item| format!("- [{}] {}", item.supplement_id, item.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        format!(
            "[agent-context-package]\npackage_id: {}\nrevision: {}\nparent_task_id: {}\n任务摘要：{}\n交付要求：{}\n约束：\n{}\n可按需读取的引用：\n{}{}\n\n上下文使用规则：引用预览仅用于判断相关性；需要正文时必须调用 context_read。不得假定拥有主对话完整历史。",
            self.package_id,
            self.revision,
            self.parent_task_id,
            self.summary,
            self.expected_output,
            constraints,
            references,
            supplements,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentContextAccessOperation {
    Search,
    Read,
    Send,
    Request,
}

/// 上下文访问审计记录。只记录来源标识和 token 估算，不复制隐藏 prompt 或思考内容。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentContextAccessRecord {
    pub record_id: String,
    pub operation: AgentContextAccessOperation,
    pub query: Option<String>,
    #[serde(default)]
    pub reference_ids: Vec<String>,
    #[serde(default)]
    pub estimated_tokens: usize,
    pub occurred_at: UtcMillis,
}

/// 任务系统 L11：任务运行变体。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    LocalAgent,
    LocalWorkflow,
    RemoteAgent,
    MonitorMcp,
    InProcessTeammate,
    Dream,
}

/// 任务系统 L11：任务生命周期，固定为 5 态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

/// 用户可见计划项状态。顶层计划同一时刻只能有一个进行项；并行执行由
/// ActiveExecutionChain 表达，不通过多个顶层进行项模拟。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Canceled,
}

impl PlanItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Canceled => "canceled",
        }
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (Self::Pending, Self::InProgress | Self::Canceled)
                    | (
                        Self::InProgress,
                        Self::Completed | Self::Blocked | Self::Canceled
                    )
                    | (Self::Blocked, Self::InProgress | Self::Canceled)
            )
    }
}

/// 计划级状态用于表达暂停与终止，避免停止会话后仍把步骤显示为进行中。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    #[default]
    Active,
    Paused,
    Completed,
    Canceled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    #[serde(default = "empty_plan_item_id")]
    pub item_id: PlanItemId,
    #[serde(alias = "content")]
    pub title: String,
    pub status: PlanItemStatus,
}

fn empty_plan_item_id() -> PlanItemId {
    PlanItemId::new("")
}

impl PlanItem {
    pub fn new(item_id: PlanItemId, title: impl Into<String>, status: PlanItemStatus) -> Self {
        Self {
            item_id,
            title: title.into(),
            status,
        }
    }
}

pub const TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT: &str = "任务运行失败，详情已记录在日志中。";

pub fn task_output_ref_is_internal_runtime_failure(output_ref: &str) -> bool {
    let normalized = output_ref.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }

    [
        "llm invocation failed",
        "model invocation failed",
        "model bridge client",
        "provider transport failed",
        "provider rejected request",
        "invalid base_url",
        "connection refused",
        "connection reset",
        "http client error",
        "spawn_blocking panicked",
        "dispatch spawn_blocking panicked",
        "dispatch failed",
        "thread panicked",
        "panicked at",
        "stack backtrace",
        "backtrace:",
        "internal assembly",
        "dispatcher missing",
        "task_store",
        "runner_manager",
        "workerruntime",
        "humancheckpointstore",
        "sessionstore",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
        || (contains_any(&normalized, &["timed out", "timeout"])
            && contains_any(
                &normalized,
                &["llm", "model", "provider", "transport", "request"],
            ))
}

pub fn public_task_output_refs(status: TaskStatus, output_refs: &[String]) -> Vec<String> {
    if status != TaskStatus::Failed {
        return output_refs.to_vec();
    }

    let mut redacted = false;
    let visible_refs = output_refs
        .iter()
        .filter_map(|output_ref| {
            if task_output_ref_is_internal_runtime_failure(output_ref) {
                redacted = true;
                None
            } else {
                Some(output_ref.clone())
            }
        })
        .collect::<Vec<_>>();

    if redacted && visible_refs.is_empty() {
        vec![TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT.to_string()]
    } else {
        visible_refs
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

/// 任务系统的内部运行形态。长期工作由 Session Goal 持续推进，进入任务调度的
/// 内容统一使用 ExecutionChain，避免目标与旧治理链形成双重生命周期。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTier {
    ExecutionChain,
}

fn default_task_tier() -> TaskTier {
    TaskTier::ExecutionChain
}
// --- AccessProfile

/// 产品级访问模式。它是用户可理解的主权限心智，运行期只从这个枚举读取权限。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessProfile {
    /// 只读分析：允许读、搜索、诊断；写入和外部副作用直接拒绝，不升级访问模式。
    ReadOnly,
    /// 受限执行：默认模式；常规 workspace 操作自动执行，高风险动作直接拦截。
    #[default]
    Restricted,
    /// 完全授权：跳过常规风险拦截；产品级硬阻断和任务/角色约束仍然生效。
    FullAccess,
}

impl AccessProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Restricted => "restricted",
            Self::FullAccess => "full_access",
        }
    }

    pub fn constrained_by_command_mode(self, command_mode: &str) -> Self {
        if command_mode.eq_ignore_ascii_case("read_only") {
            Self::ReadOnly
        } else {
            self
        }
    }
}

impl std::str::FromStr for AccessProfile {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "read_only" => Ok(Self::ReadOnly),
            "restricted" => Ok(Self::Restricted),
            "full_access" => Ok(Self::FullAccess),
            _ => Err(()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskPolicy {
    pub autonomy_level: String,
    #[serde(default)]
    pub access_profile: AccessProfile,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    #[serde(default)]
    pub read_only_paths: Vec<String>,
    pub network_mode: String,
    pub command_mode: String,
    pub retry_limit: u32,
    pub validation_profile: Option<String>,
    pub checkpoint_mode: String,
    #[serde(default = "default_task_tier")]
    pub task_tier: TaskTier,
    pub background_allowed: bool,
    pub escalation_conditions: Vec<String>,
}

impl TaskPolicy {
    pub fn effective_access_profile(&self) -> AccessProfile {
        self.access_profile
            .constrained_by_command_mode(&self.command_mode)
    }
}

/// 任务系统 L11：变体负载。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRuntimePayload {
    #[default]
    None,
    AgentContext {
        package: Box<AgentContextPackage>,
        #[serde(default)]
        accesses: Vec<AgentContextAccessRecord>,
    },
}

/// 任务执行器绑定合同。
///
/// 这是任务分派、子 agent spawn、任务恢复之间共享的稳定结构。字段为空表示未绑定，
/// 不再允许调用方写入任意 JSON 字段形成隐式协议。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskExecutorBinding {
    pub target_role: Option<String>,
    pub parallelism_group: Option<String>,
    pub exclusive_scope: Option<String>,
    pub active_skill_id: Option<String>,
    #[serde(default)]
    pub canonical_task_name: Option<String>,
    #[serde(default)]
    pub plan_item_id: Option<PlanItemId>,
    /// 当前 root coordinator 必须完成的显式工具链。
    ///
    /// 这是调度请求的结构化执行约束，不从模型输出反推。只有入口已经确认的产品
    /// 动作（例如 Goal 续跑先读取权威状态、用户明确要求创建真实子代理）才会写入；
    /// 其余协作决策仍由 root coordinator 在完整工具面中自行决定。
    #[serde(default)]
    pub required_tool_chain: Vec<String>,
}

impl TaskExecutorBinding {
    pub fn for_role(role: impl Into<String>) -> Self {
        Self {
            target_role: normalized_string(role.into()),
            ..Self::default()
        }
    }

    pub fn with_parallelism_group(mut self, group: Option<String>) -> Self {
        self.parallelism_group = normalized_optional_string(group);
        self
    }

    pub fn with_exclusive_scope(mut self, scope: Option<String>) -> Self {
        self.exclusive_scope = normalized_optional_string(scope);
        self
    }

    pub fn with_active_skill_id(mut self, skill_id: Option<String>) -> Self {
        self.active_skill_id = normalized_optional_string(skill_id);
        self
    }

    pub fn with_canonical_task_name(mut self, task_name: impl Into<String>) -> Self {
        self.canonical_task_name = normalized_string(task_name.into());
        self
    }

    pub fn with_plan_item_id(mut self, plan_item_id: Option<PlanItemId>) -> Self {
        self.plan_item_id = plan_item_id;
        self
    }

    pub fn with_required_tool_chain(mut self, tool_names: Vec<String>) -> Self {
        let mut ordered_tool_names = Vec::new();
        for tool_name in tool_names {
            if !ordered_tool_names.contains(&tool_name) {
                ordered_tool_names.push(tool_name);
            }
        }
        self.required_tool_chain = ordered_tool_names;
        self
    }

    fn target_role(&self) -> Option<&str> {
        normalized_str_ref(self.target_role.as_deref())
    }

    fn parallelism_group(&self) -> Option<&str> {
        normalized_str_ref(self.parallelism_group.as_deref())
    }

    fn exclusive_scope(&self) -> Option<&str> {
        normalized_str_ref(self.exclusive_scope.as_deref())
    }

    fn active_skill_id(&self) -> Option<&str> {
        normalized_str_ref(self.active_skill_id.as_deref())
    }

    fn canonical_task_name(&self) -> Option<&str> {
        normalized_str_ref(self.canonical_task_name.as_deref())
    }

    fn required_tool_chain(&self) -> &[String] {
        &self.required_tool_chain
    }
}
// --- Task

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub root_task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub kind: TaskKind,
    pub title: String,
    pub goal: String,
    pub status: TaskStatus,
    pub dependency_ids: Vec<TaskId>,
    pub required_children: Vec<TaskId>,
    pub policy_snapshot: Option<TaskPolicy>,
    pub executor_binding: Option<TaskExecutorBinding>,
    pub knowledge_refs: Vec<String>,
    pub workspace_scope: Option<String>,
    pub write_scope: Option<String>,
    pub input_refs: Vec<String>,
    pub output_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub retry_count: u32,
    /// 任务系统 — L11：运行变体的专用负载。
    #[serde(default)]
    pub runtime_payload: TaskRuntimePayload,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl Task {
    pub fn agent_context_package(&self) -> Option<&AgentContextPackage> {
        match &self.runtime_payload {
            TaskRuntimePayload::AgentContext { package, .. } => Some(package.as_ref()),
            TaskRuntimePayload::None => None,
        }
    }

    pub fn agent_context_accesses(&self) -> &[AgentContextAccessRecord] {
        match &self.runtime_payload {
            TaskRuntimePayload::AgentContext { accesses, .. } => accesses,
            TaskRuntimePayload::None => &[],
        }
    }

    pub fn executor_binding_target_role(&self) -> Option<&str> {
        self.executor_binding
            .as_ref()
            .and_then(TaskExecutorBinding::target_role)
    }

    pub fn executor_binding_parallelism_group(&self) -> Option<&str> {
        self.executor_binding
            .as_ref()
            .and_then(TaskExecutorBinding::parallelism_group)
    }

    pub fn executor_binding_exclusive_scope(&self) -> Option<&str> {
        self.executor_binding
            .as_ref()
            .and_then(TaskExecutorBinding::exclusive_scope)
    }

    pub fn executor_binding_active_skill_id(&self) -> Option<&str> {
        self.executor_binding
            .as_ref()
            .and_then(TaskExecutorBinding::active_skill_id)
    }

    pub fn canonical_task_name(&self) -> Option<&str> {
        self.executor_binding
            .as_ref()
            .and_then(TaskExecutorBinding::canonical_task_name)
    }

    pub fn required_tool_chain(&self) -> &[String] {
        self.executor_binding
            .as_ref()
            .map(TaskExecutorBinding::required_tool_chain)
            .unwrap_or_default()
    }

    pub fn plan_item_id(&self) -> Option<&PlanItemId> {
        self.executor_binding
            .as_ref()
            .and_then(|binding| binding.plan_item_id.as_ref())
    }
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(normalized_string)
}

fn normalized_string(value: impl AsRef<str>) -> Option<String> {
    let trimmed = value.as_ref().trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalized_str_ref(value: Option<&str>) -> Option<&str> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
// --- AgentRunProjection (view contract)

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRunProjection {
    pub root_task: Task,
    pub tasks: Vec<Task>,
    pub running_tasks: Vec<TaskId>,
    pub pending_tasks: Vec<TaskId>,
    pub completed_tasks: Vec<TaskId>,
    pub failed_tasks: Vec<TaskId>,
    pub killed_tasks: Vec<TaskId>,
    pub progress_summary: ProgressSummary,
    pub aggregate_status: TaskStatus,
    pub display_status: String,
    pub execution_mode: String,
    pub runner_status: String,
    #[serde(default)]
    pub has_recoverable_chain: bool,
    #[serde(default)]
    pub recoverable_branch_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProgressSummary {
    pub total_tasks: u32,
    pub pending_tasks: u32,
    pub running_tasks: u32,
    pub completed_tasks: u32,
    pub failed_tasks: u32,
    pub killed_tasks: u32,
    pub settled_tasks: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_task_output_refs_redacts_internal_failed_details() {
        let output_refs = vec![
            "LLM invocation failed (round 0): provider transport failed: timed out".to_string(),
        ];

        assert_eq!(
            public_task_output_refs(TaskStatus::Failed, &output_refs),
            vec![TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT.to_string()]
        );
    }

    #[test]
    fn public_task_output_refs_preserves_user_meaningful_failure() {
        let output_refs = vec!["测试失败：断言不匹配".to_string()];

        assert_eq!(
            public_task_output_refs(TaskStatus::Failed, &output_refs),
            output_refs
        );
        assert!(!task_output_ref_is_internal_runtime_failure(
            "工具执行失败，任务不能标记完成：file_write: denied"
        ));
    }

    #[test]
    fn public_task_output_refs_preserves_completed_outputs() {
        let output_refs = vec!["provider transport failed: timed out".to_string()];

        assert_eq!(
            public_task_output_refs(TaskStatus::Completed, &output_refs),
            output_refs
        );
    }

    #[test]
    fn required_tool_chain_preserves_declared_execution_order() {
        let binding = TaskExecutorBinding::for_role("coordinator").with_required_tool_chain(vec![
            "agent_spawn".to_string(),
            "agent_wait".to_string(),
            "agent_spawn".to_string(),
        ]);

        assert_eq!(
            binding.required_tool_chain(),
            ["agent_spawn", "agent_wait"],
            "结构化工具链必须按入口声明的顺序执行，只去除重复项"
        );
    }
}
