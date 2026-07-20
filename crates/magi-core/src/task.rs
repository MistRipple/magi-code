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

/// 团队编排模式。
///
/// 该模式是任务运行时合同，不是提示词关键词开关：`explicit_only` 表示只有用户
/// 明确要求时才组队，`automatic` 表示当前任务已经被本地规则判定为需要并行分工，
/// `required` 表示用户明确要求组队，`disabled` 表示用户明确禁止组队。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelegationMode {
    Disabled,
    #[default]
    ExplicitOnly,
    Automatic,
    Required,
}

impl AgentDelegationMode {
    pub fn requires_team(self) -> bool {
        matches!(self, Self::Automatic | Self::Required)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ExplicitOnly => "explicit_only",
            Self::Automatic => "automatic",
            Self::Required => "required",
        }
    }
}

/// 主线协调器的团队执行合同。
///
/// 只在根任务上保存；子任务通过 `agent_spawn` 的角色和 context package 自己描述
/// 执行边界。这样任务是否需要组队、最少人数和并行要求可以被恢复、审计和校验，
/// 不再依赖模型是否碰巧理解了一段提示词。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentDelegationPolicy {
    pub mode: AgentDelegationMode,
    #[serde(default = "default_minimum_agent_count")]
    pub minimum_agent_count: u8,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub recommended_roles: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

impl Default for AgentDelegationPolicy {
    fn default() -> Self {
        Self {
            mode: AgentDelegationMode::ExplicitOnly,
            minimum_agent_count: default_minimum_agent_count(),
            parallel: false,
            recommended_roles: Vec::new(),
            reason: String::new(),
        }
    }
}

const fn default_minimum_agent_count() -> u8 {
    2
}

impl AgentDelegationPolicy {
    pub fn explicit_required(text: &str) -> Self {
        Self {
            mode: AgentDelegationMode::Required,
            minimum_agent_count: requested_agent_count(text),
            parallel: true,
            recommended_roles: recommended_agent_roles(text),
            reason: "用户明确要求代理、团队或并行协作".to_string(),
        }
    }

    pub fn automatic(text: &str) -> Self {
        Self {
            mode: AgentDelegationMode::Automatic,
            minimum_agent_count: 2,
            parallel: true,
            recommended_roles: recommended_agent_roles(text),
            reason: "任务包含多个可独立推进的工作面，自动启用团队协作".to_string(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            mode: AgentDelegationMode::Disabled,
            reason: "用户明确要求主线直接处理".to_string(),
            ..Self::default()
        }
    }

    pub fn render_for_prompt(&self) -> String {
        let roles = if self.recommended_roles.is_empty() {
            "由协调器根据子任务选择可用角色".to_string()
        } else {
            self.recommended_roles.join("、")
        };
        format!(
            "[team-orchestration-contract]\nmode: {}\nminimum_agent_count: {}\nparallel: {}\nrecommended_roles: {}\nreason: {}\n执行合同：{}；至少创建 {} 个真实代理，边界清晰的子任务应在同一轮并行派发；全部代理必须通过 agent_wait 收集，并在最终答复中吸收结果。",
            self.mode.as_str(),
            self.minimum_agent_count,
            self.parallel,
            roles,
            self.reason,
            if self.mode == AgentDelegationMode::Required {
                "用户明确要求组队，禁止由主线单独完成"
            } else {
                "当前任务自动判定需要组队，除非工具不可用或安全策略阻止"
            },
            self.minimum_agent_count,
        )
    }
}

/// 根据用户输入返回一次任务的团队编排合同。
pub fn agent_delegation_policy(text: &str) -> AgentDelegationPolicy {
    if text_prohibits_agent_spawn(text) {
        return AgentDelegationPolicy::disabled();
    }
    if text_requires_agent_spawn(text) {
        return AgentDelegationPolicy::explicit_required(text);
    }
    if text_requires_automatic_agent_team(text) {
        return AgentDelegationPolicy::automatic(text);
    }
    AgentDelegationPolicy::default()
}

/// 判断任务是否应在没有用户显式要求时自动开启团队。
///
/// 只对同时具备多个工作领域和拆分信号的执行请求生效，避免把普通问答、小范围
/// 单文件修改或单条工具调用误判成团队任务。用户的“不要组队”永远优先。
pub fn text_requires_automatic_agent_team(text: &str) -> bool {
    if text_prohibits_agent_spawn(text) || text_requires_agent_spawn(text) {
        return false;
    }
    let normalized = text.to_ascii_lowercase();
    let work_domain_count = [
        &[
            "分析", "检查", "审查", "定位", "探索", "analy", "inspect", "review",
        ][..],
        &[
            "实现",
            "修复",
            "重构",
            "修改",
            "开发",
            "implement",
            "fix",
            "refactor",
        ][..],
        &["测试", "验证", "回归", "test", "verify", "validate"][..],
        &["文档", "说明", "readme", "document"][..],
        &["构建", "发布", "打包", "build", "release", "package"][..],
    ]
    .iter()
    .filter(|domain| {
        domain.iter().any(|marker| {
            normalized
                .match_indices(marker)
                .any(|(index, _)| !prefix_negates_action(&normalized, index))
        })
    })
    .count();
    if work_domain_count < 2 {
        return false;
    }

    let decomposition_signal = [
        "并且",
        "并",
        "同时",
        "然后",
        "以及",
        "分别",
        "一边",
        "分成",
        "拆分",
        "逐项",
        "端到端",
        "全面",
        "完整",
        "系统性",
        "全量",
        "跨模块",
        "and",
        "then",
        "also",
        "separately",
        "end-to-end",
    ]
    .iter()
    .filter(|marker| normalized.contains(*marker))
    .count();
    let structured_signal = normalized
        .chars()
        .filter(|character| matches!(character, ';' | '；' | '\n' | '。' | '！' | '!'))
        .count()
        >= 2
        || normalized.contains("1.")
        || normalized.contains("1、")
        || normalized.contains("- ")
        || normalized.chars().count() >= 100;
    decomposition_signal > 0 || structured_signal
}

fn requested_agent_count(text: &str) -> u8 {
    let normalized = text.to_ascii_lowercase();
    for (marker, count) in [
        ("五个", 5),
        ("四个", 4),
        ("三个", 3),
        ("两个", 2),
        ("5个", 5),
        ("4个", 4),
        ("3个", 3),
        ("2个", 2),
    ] {
        if normalized.contains(marker) {
            return count;
        }
    }
    2
}

fn recommended_agent_roles(text: &str) -> Vec<String> {
    let normalized = text.to_ascii_lowercase();
    let mut roles = Vec::new();
    if [
        "分析", "检查", "审查", "定位", "探索", "analy", "inspect", "review",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        roles.push("explorer".to_string());
    }
    if [
        "实现",
        "修复",
        "重构",
        "修改",
        "开发",
        "implement",
        "fix",
        "refactor",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        roles.push("executor".to_string());
    }
    if ["测试", "验证", "回归", "test", "verify", "validate"]
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        roles.push("tester".to_string());
    }
    if ["审计", "review", "审查"]
        .iter()
        .any(|marker| normalized.contains(marker))
        && !roles.iter().any(|role| role == "explorer")
    {
        roles.push("reviewer".to_string());
    }
    roles.truncate(3);
    roles
}

/// 判断用户文本是否明确要求主线创建子代理。
///
/// 只接受未被否定词修饰的代理模式、代理工具或派发动作。这样“不要创建子代理”不会
/// 因同时包含“创建”和“子代理”而被误判为强制派发；“创建两个子代理，但子代理不能
/// 再创建更多子代理”仍会由前半句正确触发主线派发。
pub fn text_requires_agent_spawn(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let compact = lowered
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '-' | '_' | '\t' | '\n' | '\r'))
        .collect::<String>();

    for marker in [
        "agent_spawn",
        "subagent模式",
        "子代理模式",
        "子agent模式",
        "多代理模式",
        "多agent模式",
        "multiagent模式",
        "团队模式",
        "团队协作",
        "代理模式",
        "并行协作",
        "多角色处理",
        "subagentmode",
        "multiagentmode",
        "使用多个代理",
        "使用两个代理",
        "使用多代理",
        "代理执行",
        "代理完成",
        "代理处理",
        "代理检查",
        "代理审查",
    ] {
        let haystack = if marker.contains("模式") || marker.ends_with("mode") {
            &compact
        } else {
            &lowered
        };
        if contains_non_negated_marker(haystack, marker) {
            return true;
        }
    }

    for verb in [
        "启动", "派发", "分派", "分配", "调用", "创建", "拉起", "开启", "使用",
    ] {
        if action_targets_agent(&lowered, verb, &["子代理", "代理", "agent", "subagent"]) {
            return true;
        }
    }
    for verb in ["spawn", "start", "launch", "dispatch", "assign", "use"] {
        if action_targets_agent(
            &lowered,
            verb,
            &["agent", "agents", "subagent", "subagents"],
        ) {
            return true;
        }
    }
    false
}

/// 判断用户是否明确禁止主线创建子代理。若同一输入中另有明确的正向派发要求，
/// 正向要求优先，避免把“主线创建两个代理，但 worker 不能继续创建代理”误判为禁用。
pub fn text_prohibits_agent_spawn(text: &str) -> bool {
    if text_requires_agent_spawn(text) {
        return false;
    }
    let lowered = text.to_ascii_lowercase();
    for marker in [
        "团队模式",
        "团队协作",
        "多代理",
        "多agent",
        "multi-agent",
        "代理模式",
        "子代理",
        "subagent",
    ] {
        if lowered
            .match_indices(marker)
            .any(|(index, _)| prefix_negates_action(&lowered, index))
        {
            return true;
        }
    }
    for verb in [
        "启动", "派发", "分派", "分配", "调用", "创建", "拉起", "开启", "使用",
    ] {
        if negated_action_targets_agent(
            &lowered,
            verb,
            &[
                "子代理",
                "代理",
                "团队",
                "多代理",
                "agent",
                "subagent",
                "team",
            ],
        ) {
            return true;
        }
    }
    for verb in ["spawn", "start", "launch", "dispatch", "assign", "use"] {
        if negated_action_targets_agent(
            &lowered,
            verb,
            &["agent", "agents", "subagent", "subagents"],
        ) {
            return true;
        }
    }
    false
}

fn contains_non_negated_marker(text: &str, marker: &str) -> bool {
    text.match_indices(marker).any(|(index, _)| {
        !prefix_negates_action(text, index)
            && !prefix_frames_action_as_discussion(text, index)
            && !suffix_frames_marker_as_discussion(text, index, marker)
    })
}

fn suffix_frames_marker_as_discussion(text: &str, marker_index: usize, marker: &str) -> bool {
    let suffix = text[marker_index + marker.len()..]
        .chars()
        .take(20)
        .collect::<String>();
    [
        "是什么",
        "怎么",
        "如何",
        "是否",
        "什么时候",
        "为什么",
        "what",
        "how",
        "whether",
        "when",
        "why",
    ]
    .iter()
    .any(|framing| suffix.contains(framing))
}

fn action_targets_agent(text: &str, verb: &str, targets: &[&str]) -> bool {
    text.match_indices(verb).any(|(index, _)| {
        if prefix_negates_action(text, index) || prefix_frames_action_as_discussion(text, index) {
            return false;
        }
        let suffix = text[index + verb.len()..]
            .chars()
            .take(20)
            .collect::<String>();
        targets.iter().any(|target| suffix.contains(target))
    })
}

fn prefix_frames_action_as_discussion(text: &str, action_index: usize) -> bool {
    let prefix = text[..action_index]
        .chars()
        .rev()
        .take(16)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let trimmed = prefix.trim_end();
    [
        "如何",
        "怎么",
        "怎样",
        "什么时候",
        "何时",
        "是否",
        "为什么",
        "介绍",
        "说明",
        "解释",
        "讨论",
        "分析",
        "how to",
        "when to",
        "whether to",
        "why",
        "explain",
        "describe",
        "discuss",
    ]
    .iter()
    .any(|framing| trimmed.contains(framing))
}

fn negated_action_targets_agent(text: &str, verb: &str, targets: &[&str]) -> bool {
    text.match_indices(verb).any(|(index, _)| {
        if !prefix_negates_action(text, index) {
            return false;
        }
        let suffix = text[index + verb.len()..]
            .chars()
            .take(20)
            .collect::<String>();
        targets.iter().any(|target| suffix.contains(target))
    })
}

fn prefix_negates_action(text: &str, action_index: usize) -> bool {
    let prefix = text[..action_index]
        .chars()
        .rev()
        .take(10)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let trimmed = prefix.trim_end();
    if trimmed.ends_with('不') || trimmed.ends_with('未') {
        return true;
    }
    [
        "不要",
        "禁止",
        "无需",
        "不需要",
        "不必",
        "不得",
        "不能",
        "不可",
        "别",
        "no ",
        "without ",
        "do not ",
        "don't ",
        "never ",
    ]
    .iter()
    .any(|negation| prefix.contains(negation))
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
// --- TaskPolicy

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
    pub delegation_policy: Option<AgentDelegationPolicy>,
    #[serde(default)]
    pub canonical_task_name: Option<String>,
    #[serde(default)]
    pub plan_item_id: Option<PlanItemId>,
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

    pub fn with_delegation_policy(mut self, policy: AgentDelegationPolicy) -> Self {
        self.delegation_policy = Some(policy);
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

    pub fn plan_item_id(&self) -> Option<&PlanItemId> {
        self.executor_binding
            .as_ref()
            .and_then(|binding| binding.plan_item_id.as_ref())
    }

    pub fn delegation_policy(&self) -> Option<&AgentDelegationPolicy> {
        self.executor_binding
            .as_ref()
            .and_then(|binding| binding.delegation_policy.as_ref())
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
    fn agent_spawn_intent_respects_negation_and_nested_worker_constraint() {
        assert!(!text_requires_agent_spawn(
            "请执行 sleep 2，完成后只回复标记，不要创建子代理。"
        ));
        assert!(text_prohibits_agent_spawn(
            "请执行 sleep 2，完成后只回复标记，不要创建子代理。"
        ));
        assert!(text_prohibits_agent_spawn("直接执行，不创建子代理。"));
        assert!(!text_requires_agent_spawn(
            "do not use agents; run the command directly"
        ));
        assert!(text_requires_agent_spawn(
            "请启动两个子代理并行检查；子代理不能创建更多子代理。"
        ));
        assert!(!text_prohibits_agent_spawn(
            "请启动两个子代理并行检查；子代理不能创建更多子代理。"
        ));
        assert!(text_requires_agent_spawn(
            "必须分别调用 agent_spawn 创建两个子代理并等待结果。"
        ));
        assert!(!text_requires_agent_spawn(
            "请说明主模型和代理的职责边界，以及什么时候应该使用代理。"
        ));
        assert!(!text_requires_agent_spawn(
            "请解释 agent_spawn 的参数和使用场景。"
        ));
        assert!(!text_requires_agent_spawn(
            "how to use agents effectively in a coding task?"
        ));
        assert_eq!(
            agent_delegation_policy("请说明主模型和代理的职责边界，以及什么时候应该使用代理。")
                .mode,
            AgentDelegationMode::ExplicitOnly
        );
    }

    #[test]
    fn agent_delegation_policy_distinguishes_explicit_automatic_and_disabled() {
        assert_eq!(
            agent_delegation_policy("检查 /tmp 顶层结构并汇总，不要修改文件。").mode,
            AgentDelegationMode::ExplicitOnly
        );
        assert_eq!(
            agent_delegation_policy("修复登录问题，并运行测试验证回归结果。").mode,
            AgentDelegationMode::Automatic
        );
        assert_eq!(
            agent_delegation_policy("请使用团队模式，分别由 explorer 和 tester 处理。").mode,
            AgentDelegationMode::Required
        );
        assert_eq!(
            agent_delegation_policy("只由主线处理，不要启用团队模式。").mode,
            AgentDelegationMode::Disabled
        );
    }
}
