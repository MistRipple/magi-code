use crate::session_writeback::{
    SessionTurnStreamUpdate, append_session_turn_item_with_task_store,
    publish_current_session_turn_item_event, publish_session_turn_item_event,
    publish_session_turn_item_event_with_stream_update, session_turn_item,
    session_turn_stream_update, upsert_session_turn_item_with_task_store,
};
use crate::task_execution_registry::TaskExecutionRegistry;
use crate::task_helpers::task_is_long_mission;
use crate::task_runner_bridge::TaskOutcome;
use crate::tool_result_utils::{
    infer_tool_call_status, summarize_tool_result, tool_execution_status_label,
    turn_item_status_for_tool_result,
};
use crate::{
    ConversationRegistry, MailboxAuthor, MailboxItem, MailboxKind, RoundOutcome,
    TaskTurnVisibility, TurnDriver, apply_task_final_visibility, apply_task_turn_visibility,
    apply_task_worker_detail_visibility, canonical_tool_call_name, compact_validation_failure,
    deterministic_task_final_content, execute_task_tool_call_batch,
    forced_task_tool_choice_for_round, record_completed_required_tools,
    required_tool_chain_is_complete, required_tool_chain_recovery_prompt, task_required_tool_chain,
    task_tool_failure_reason, task_turn_visibility, tool_call_round_limit,
    validation_result_rejects_delivery,
};
use crate::{
    model_error::provider_empty_assistant_response_error,
    prompt_utils::{
        normalize_model_stream_preview_content, normalize_model_visible_content,
        workspace_context_system_prompt,
    },
    settings_store::SettingsStore,
    usage_recording::{ModelUsageBinding, publish_model_usage_record, record_mission_turn},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolDefinition, LOOPBACK_MODEL_PROVIDER, ModelBridgeClient,
    ModelInvocationRequest, ModelStreamingDelta,
};
use magi_core::{
    EventId, ExecutionResultStatus, LeaseId, SessionId, Task, TaskId, TaskStatus, ThreadId,
    UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_orchestrator::{ExecutionContextSummary, task_store::TaskStore};
use magi_session_store::{
    SessionStore, ThreadChatMessage, ThreadChatToolCall, ThreadChatToolFunction, TimelineEntryKind,
};
use magi_tool_runtime::ToolRegistry;
use magi_usage_authority::UsageCallStatus;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

pub struct ConversationLoopRequest<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub task_store: &'a TaskStore,
    pub execution_registry: &'a TaskExecutionRegistry,
    /// Task System v2：Turn 状态机驱动。每次 LLM 调用都通过 advance_turn 驱动，
    /// 显式经过 Pending → Modeling → Done/Failed 不变式（同一 Conversation 不并发）。
    pub conversation_registry: &'a ConversationRegistry,
    /// Task System v2 — AgentRole 注册表。task_turn_visibility 解析 role_id 时
    /// 必须走该注册表，不再依赖硬编码的 kind→role 默认 mapping。
    pub agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    /// Task System v2 — L5：父子任务拓扑图。S7 协调工具（agent_spawn）
    /// 在 execute_task_tool_call 中拦截时操作此结构。
    pub spawn_graph: &'a std::sync::Mutex<magi_spawn_graph::SpawnGraph>,
    /// Task System v2 — L12：本次轮次的 SafetyGate 快照。`None` 表示当前没有
    /// 启用任何危险模式规则（既无内置也无用户自定义），此时拦截器走 pass-through。
    /// 在 execute_task_tool_call 中工具调用执行前做语义判定。
    pub safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    /// Task System v2 — L13：当前 session 的 TodoLedger。模型通过 `todo_write`
    /// 工具往里写分解 + 进度；本 Turn 起始时把快照渲染成 system prompt 注入。
    pub todo_ledger: &'a magi_todo_ledger::TodoLedger,
    /// Task System v2 — L14：当前 workspace 的 ProjectMemory。`None` 表示当前 task
    /// 不绑定 workspace（极少数 orchestration-only 场景），此时不注入 prompt、
    /// 也不允许 `memory_write` 工具调用成功。
    pub project_memory: Option<&'a magi_project_memory::ProjectMemoryStore>,
    /// Task System v2 — Tier 4 / L15：当前 workspace 的 MissionCharter 索引。`None` 表示
    /// 当前 task 不绑定 workspace（极少数 orchestration-only 场景），此时不注入 prompt、
    /// 也不允许 `mission_charter_write` 工具调用成功。
    pub mission_charter: Option<&'a magi_mission_charter::MissionCharterStore>,
    /// Task System v2 — Tier 4 / L16：当前 workspace 的 Plan 索引。`None` 表示当前 task
    /// 不绑定 workspace；此时不注入 prompt，也不允许 `plan_write` 工具调用成功。
    pub plan: Option<&'a magi_plan::PlanStore>,
    /// Task System v2 — Tier 4 / L17：当前 workspace 的 MissionWorkspace 索引。`None`
    /// 表示当前 task 不绑定 workspace；此时不注入工作目录视图。
    pub mission_workspace: Option<&'a magi_mission_workspace::MissionWorkspaceStore>,
    /// Task System v2 — Tier 4 / L18：当前 workspace 的 KnowledgeGraph 索引。`None`
    /// 表示当前 task 不绑定 workspace；此时不注入 KG 视图，也不允许 `kg_write` 工具落盘。
    pub knowledge_graph: Option<&'a magi_knowledge_graph::KnowledgeGraphStore>,
    /// Task System v2 — Tier 4 / L19：当前 workspace 的 ValidationRunner 索引。`None`
    /// 表示当前 task 不绑定 workspace；此时不注入验证摘要，也不允许 `validation_record`
    /// 工具落盘。Coordinator 凭这里的 Pass/Fail 判定 Plan 节点是否真完成。
    pub validation_runner: Option<&'a magi_validation_runner::ValidationStore>,
    /// Task System v2 — Tier 4 / L20：当前 workspace 的 Checkpoint 索引。`None`
    /// 表示当前 task 不绑定 workspace；此时不注入最近检查点列表，也不允许
    /// `checkpoint_create` 工具落盘。append-only 语义，仅追加不修改。
    pub checkpoint: Option<&'a magi_checkpoint::CheckpointStore>,
    /// Task System v2 — Tier 4 / L21：当前 workspace 的 HumanCheckpoint 索引。`None`
    /// 表示当前 task 不绑定 workspace 或长任务 store 打开失败；此时不注入人工审核点摘要，
    /// 也不允许 `human_checkpoint_request` 工具落盘。长任务缺少 store 时，agent_spawn
    /// 会在工具层失败，避免绕过 pending 检查。
    pub human_checkpoint: Option<&'a magi_human_checkpoint::HumanCheckpointStore>,
    /// codex goal 桥：mission 维度记账 sidecar 句柄。`None` 表示当前 task 未绑定
    /// workspace 或 dispatcher 未注入 metrics（旧路径回退），此时不做记账写入。
    /// 设计上每轮 LLM 调用后调用一次 `record_mission_turn`，与 `publish_model_usage_record`
    /// 并列收口；失败仅 warn，不阻断主轮次。
    pub mission_metrics: Option<&'a Arc<magi_mission_metrics::MissionMetricsStore>>,
    pub task: &'a magi_core::Task,
    pub task_id: &'a TaskId,
    pub lease_id: &'a LeaseId,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub prompt: String,
    pub tools: Option<Vec<ChatToolDefinition>>,
    pub usage_binding: &'a ModelUsageBinding,
    pub streaming_entry_id: Option<&'a str>,
    /// `true` 表示当前 task 走 sidechain（task 详情），由父代理派发的子任务。
    /// `false` 表示走主线（mainline）orchestrator thread。来源是
    /// `TaskExecutionPlan::Dispatch.is_primary` 的取反——is_primary=true 代表
    /// session 的根任务/直接由用户激活的 orchestrator turn。
    pub is_sidechain: bool,
    pub worker_id: Option<&'a magi_core::WorkerId>,
    /// P7：执行上下文必须绑定到 thread。LLM 入口会 prepend 该 thread 的历史、
    /// 结束时把本轮消息 append 回 thread。orchestrator task 走 session 的
    /// orchestrator thread；代理 task 走本次执行独占的 task thread。
    pub thread_id: &'a ThreadId,
    pub context_summary: Option<ExecutionContextSummary>,
    pub system_prompt: Option<String>,
    pub workspace_root_path: Option<PathBuf>,
}

/// P6b：把 thread 持久化的消息记录（`ThreadChatMessage`）还原为 bridge-client 的
/// `ChatMessage`。两者字段一一对应，独立类型仅是为了避免 session-store 反向依赖
/// bridge-client，不承担额外语义。
fn thread_chat_message_to_chat_message(message: &ThreadChatMessage) -> ChatMessage {
    ChatMessage {
        role: message.role.clone(),
        content: message.content.clone(),
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| ChatToolCall {
                id: call.id.clone(),
                kind: call.kind.clone(),
                function: magi_bridge_client::ChatToolFunction {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                },
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
    }
}

/// P6b：把本轮新产生的 bridge-client 消息（含 system prompt 之外的所有条目）
/// 压缩为 thread 持久化格式。系统提示 / 工作区提示等重复上下文不再次写入。
fn chat_message_to_thread_chat_message(message: &ChatMessage) -> ThreadChatMessage {
    ThreadChatMessage {
        role: message.role.clone(),
        content: message.content.clone(),
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| ThreadChatToolCall {
                id: call.id.clone(),
                kind: call.kind.clone(),
                function: ThreadChatToolFunction {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                },
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
    }
}

const THREAD_HISTORY_COMPACT_THRESHOLD_TOKENS: usize = 18_000;
const THREAD_HISTORY_COMPACT_TARGET_TOKENS: usize = 8_000;
const THREAD_HISTORY_RECENT_MESSAGE_TARGET: usize = 12;
const THREAD_HISTORY_RECENT_MESSAGE_FLOOR: usize = 8;
const THREAD_HISTORY_SUMMARY_EXCERPT_LIMIT: usize = 16;
const THREAD_HISTORY_SUMMARY_EXCERPT_CHARS: usize = 360;
const THREAD_HISTORY_TOOL_ARGUMENT_CHARS: usize = 220;

fn estimate_text_tokens(text: &str) -> usize {
    text.len() / 4 + 1
}

fn estimate_thread_message_tokens(message: &ThreadChatMessage) -> usize {
    let mut total = estimate_text_tokens(&message.role) + 4;
    if let Some(content) = message.content.as_deref() {
        total += estimate_text_tokens(content);
    }
    if let Some(tool_call_id) = message.tool_call_id.as_deref() {
        total += estimate_text_tokens(tool_call_id);
    }
    for call in &message.tool_calls {
        total += estimate_text_tokens(&call.id);
        total += estimate_text_tokens(&call.kind);
        total += estimate_text_tokens(&call.function.name);
        total += estimate_text_tokens(&call.function.arguments);
    }
    total
}

fn estimate_thread_history_tokens(history: &[ThreadChatMessage]) -> usize {
    history.iter().map(estimate_thread_message_tokens).sum()
}

fn truncate_for_thread_summary(value: &str, max_chars: usize) -> String {
    let normalized = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut out = normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn thread_role_label(role: &str) -> &str {
    match role {
        "system" => "系统",
        "user" => "用户",
        "assistant" => "助手",
        "tool" => "工具结果",
        _ => "消息",
    }
}

fn summarize_thread_message(index: usize, message: &ThreadChatMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(content) = message
        .content
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(format!(
            "内容：{}",
            truncate_for_thread_summary(content, THREAD_HISTORY_SUMMARY_EXCERPT_CHARS)
        ));
    }
    if !message.tool_calls.is_empty() {
        let calls = message
            .tool_calls
            .iter()
            .map(|call| {
                let args = truncate_for_thread_summary(
                    &call.function.arguments,
                    THREAD_HISTORY_TOOL_ARGUMENT_CHARS,
                );
                if args.is_empty() {
                    call.function.name.clone()
                } else {
                    format!("{}({args})", call.function.name)
                }
            })
            .collect::<Vec<_>>()
            .join("；");
        parts.push(format!("工具调用：{calls}"));
    }
    if message.role == "tool" {
        if let Some(tool_call_id) = message.tool_call_id.as_deref() {
            parts.push(format!("对应调用：{tool_call_id}"));
        }
    }
    if parts.is_empty() {
        parts.push("空消息".to_string());
    }
    format!(
        "- #{} {}：{}",
        index + 1,
        thread_role_label(&message.role),
        parts.join("；")
    )
}

fn thread_history_tail_is_tool_balanced(tail: &[ThreadChatMessage]) -> bool {
    let mut tool_call_ids = BTreeSet::new();
    let mut tool_result_ids = BTreeSet::new();
    for message in tail {
        for call in &message.tool_calls {
            tool_call_ids.insert(call.id.as_str());
        }
        if message.role == "tool" {
            let Some(tool_call_id) = message.tool_call_id.as_deref() else {
                return false;
            };
            if !tool_call_ids.contains(tool_call_id) {
                return false;
            }
            tool_result_ids.insert(tool_call_id);
        }
    }
    tool_call_ids
        .iter()
        .all(|tool_call_id| tool_result_ids.contains(tool_call_id))
}

fn choose_thread_history_compaction_split(history: &[ThreadChatMessage]) -> Option<usize> {
    if history.len() <= 1 {
        return None;
    }
    let target_tail = THREAD_HISTORY_RECENT_MESSAGE_TARGET.min(history.len().saturating_sub(1));
    let floor_tail = THREAD_HISTORY_RECENT_MESSAGE_FLOOR.min(history.len().saturating_sub(1));
    let max_split = history.len().saturating_sub(floor_tail);
    let mut split = history.len().saturating_sub(target_tail).max(1);

    while split < max_split
        && estimate_thread_history_tokens(&history[split..]) > THREAD_HISTORY_COMPACT_TARGET_TOKENS
    {
        split += 1;
    }
    while split > 0 && !thread_history_tail_is_tool_balanced(&history[split..]) {
        split -= 1;
    }

    (split > 0).then_some(split)
}

fn build_thread_history_compaction_message(
    compacted_prefix: &[ThreadChatMessage],
    original_tokens: usize,
) -> ThreadChatMessage {
    let mut role_counts: BTreeMap<String, usize> = BTreeMap::new();
    for message in compacted_prefix {
        *role_counts.entry(message.role.clone()).or_default() += 1;
    }
    let role_summary = role_counts
        .iter()
        .map(|(role, count)| format!("{} {}", thread_role_label(role), count))
        .collect::<Vec<_>>()
        .join("，");

    let mut selected_indices = BTreeSet::new();
    for index in 0..compacted_prefix.len().min(3) {
        selected_indices.insert(index);
    }
    let remaining_slots =
        THREAD_HISTORY_SUMMARY_EXCERPT_LIMIT.saturating_sub(selected_indices.len());
    let tail_start = compacted_prefix.len().saturating_sub(remaining_slots);
    for index in tail_start..compacted_prefix.len() {
        selected_indices.insert(index);
    }

    let excerpts = selected_indices
        .into_iter()
        .map(|index| summarize_thread_message(index, &compacted_prefix[index]))
        .collect::<Vec<_>>()
        .join("\n");

    let content = format!(
        "[context_compaction]\n\
这是 Magi 自动生成的当前 thread 早期历史摘要，用于替代已压缩的完整消息。它是历史事实；如果它与后续保留的完整消息冲突，以后续完整消息为准。\n\
压缩范围：{} 条消息；压缩前估算 token：{}；角色分布：{}。\n\
关键历史摘录：\n{}\n\
[/context_compaction]",
        compacted_prefix.len(),
        original_tokens,
        if role_summary.is_empty() {
            "无".to_string()
        } else {
            role_summary
        },
        if excerpts.is_empty() {
            "- 无".to_string()
        } else {
            excerpts
        }
    );

    ThreadChatMessage {
        role: "system".to_string(),
        content: Some(content),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

fn compact_thread_history_if_needed(
    history: &[ThreadChatMessage],
) -> Option<Vec<ThreadChatMessage>> {
    let original_tokens = estimate_thread_history_tokens(history);
    if original_tokens < THREAD_HISTORY_COMPACT_THRESHOLD_TOKENS {
        return None;
    }
    let split = choose_thread_history_compaction_split(history)?;
    let compacted_prefix = &history[..split];
    let retained_tail = &history[split..];
    let summary = build_thread_history_compaction_message(compacted_prefix, original_tokens);
    let mut compacted = Vec::with_capacity(retained_tail.len() + 1);
    compacted.push(summary);
    compacted.extend(retained_tail.iter().cloned());

    let compacted_tokens = estimate_thread_history_tokens(&compacted);
    if compacted_tokens >= original_tokens {
        return None;
    }
    Some(compacted)
}

fn compact_and_replace_thread_history(
    session_store: &SessionStore,
    thread_id: &ThreadId,
    history: Vec<ThreadChatMessage>,
    phase: &'static str,
) -> Vec<ThreadChatMessage> {
    let original_count = history.len();
    let original_tokens = estimate_thread_history_tokens(&history);
    let Some(compacted) = compact_thread_history_if_needed(&history) else {
        return history;
    };
    let compacted_count = compacted.len();
    let compacted_tokens = estimate_thread_history_tokens(&compacted);
    session_store.replace_thread_messages(thread_id, compacted.clone(), UtcMillis::now());
    tracing::info!(
        thread_id = %thread_id,
        phase,
        original_count,
        compacted_count,
        original_tokens,
        compacted_tokens,
        "thread 历史已压缩并替换"
    );
    compacted
}

pub fn run_conversation_loop(
    request: ConversationLoopRequest<'_>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    // Task System v2 切入：经由 ConversationRegistry 拿到本 session 的 Conversation，
    // 用 advance_turn 驱动 Turn 状态机；模型 IO + 工具 IO 段折叠到 driver 内部一次性执行。
    let registry = request.conversation_registry;
    let conv_handle = registry.conversation_for_task(request.session_id, request.task_id);
    let driver = ConversationTurnDriver::new(request);
    let mut conversation = conv_handle
        .lock()
        .expect("Conversation mutex poisoned in conversation_loop");
    match conversation.advance_turn(driver) {
        Ok(outcome) => outcome,
        Err(err) => {
            tracing::error!(?err, "conversation_loop advance_turn 失败");
            (
                TaskOutcome::Failed {
                    error: format!("Conversation::advance_turn 失败: {err}"),
                },
                None,
            )
        }
    }
}

/// Task System v2 — 把一次完整的 LLM IO + 工具 IO 段封装成 TurnDriver round。
///
/// 当前 driver 的 round_limit = 1：内部仍保留多轮工具调用 for 循环（围绕
/// `messages` 累积器）。Conversation::advance_turn 提供外层 Turn 状态机，本 driver
/// 承担当前 conversation loop 的模型 IO 与工具 IO。
struct ConversationTurnDriver<'a> {
    request: Option<ConversationLoopRequest<'a>>,
    pending_mailbox_items: Vec<MailboxItem>,
    /// execute_round 跑完后把 outcome 存到这里，finalize_success 再交付出去。
    captured: Option<(TaskOutcome, Option<ExecutionContextSummary>)>,
}

impl<'a> ConversationTurnDriver<'a> {
    fn new(request: ConversationLoopRequest<'a>) -> Self {
        Self {
            request: Some(request),
            pending_mailbox_items: Vec::new(),
            captured: None,
        }
    }
}

impl<'a> TurnDriver for ConversationTurnDriver<'a> {
    type Outcome = (TaskOutcome, Option<ExecutionContextSummary>);

    fn round_limit(&self) -> usize {
        1
    }

    fn accept_mailbox_items(&mut self, items: Vec<MailboxItem>) {
        self.pending_mailbox_items = items;
    }

    fn execute_round(&mut self, _round: usize) -> RoundOutcome {
        let request = self
            .request
            .take()
            .expect("ConversationTurnDriver::execute_round 重入");
        let pending_mailbox_items = std::mem::take(&mut self.pending_mailbox_items);
        let outcome = run_conversation_loop_inner(request, pending_mailbox_items);
        let is_failure = matches!(outcome.0, TaskOutcome::Failed { .. });
        self.captured = Some(outcome);
        if is_failure {
            // Turn 状态机记账：失败也通过 finalize_round_failure 路径出。
            RoundOutcome::Failed("conversation_loop_inner returned Failed".to_string())
        } else {
            RoundOutcome::Done
        }
    }

    fn finalize_success(self) -> Self::Outcome {
        self.captured
            .expect("ConversationTurnDriver::finalize_success 没有捕获到 outcome")
    }

    fn finalize_round_failure(self, _reason: String) -> Self::Outcome {
        self.captured
            .expect("ConversationTurnDriver::finalize_round_failure 没有捕获到 outcome")
    }

    fn finalize_exhausted(self) -> Self::Outcome {
        (
            TaskOutcome::Failed {
                error: "conversation_loop driver 在 round_limit 内未产出 outcome".to_string(),
            },
            None,
        )
    }
}

/// 一轮 LLM IO + 工具 IO 全段——driver 内部唯一调用点。
fn run_conversation_loop_inner(
    request: ConversationLoopRequest<'_>,
    pending_mailbox_items: Vec<MailboxItem>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    let ConversationLoopRequest {
        client,
        event_bus,
        session_store,
        settings_store,
        tool_registry,
        skill_runtime,
        task_store,
        execution_registry,
        conversation_registry,
        agent_role_registry,
        spawn_graph,
        safety_gate,
        todo_ledger,
        project_memory,
        mission_charter,
        plan,
        mission_workspace,
        knowledge_graph,
        validation_runner,
        checkpoint,
        human_checkpoint,
        mission_metrics,
        task,
        task_id,
        lease_id,
        session_id,
        workspace_id,
        prompt,
        tools,
        usage_binding,
        streaming_entry_id,
        is_sidechain,
        worker_id,
        thread_id,
        context_summary,
        system_prompt,
        workspace_root_path,
    } = request;

    let mut messages = Vec::new();
    // ===================================================================
    // 17 段 prompt 装配 · 缓存边界 (Phase 3.2)
    // -------------------------------------------------------------------
    // S-ID 是逻辑标识（外部 dispatcher / docs 交叉引用稳定），下方
    // **emission order 按 LLM prompt 缓存友好度重排**：STATIC → SEMI-STATIC
    // → DYNAMIC。任何一段 DYNAMIC 内容变化都会让其下方所有消息的缓存键失
    // 效，因此越静态的段越往前推。修改本块时务必保持这个分层不变。
    //
    //   Tier A · STATIC      —— 同一角色 / workspace / mission 多轮内不变
    //     S1   角色 / agent role 系统提示  (assemble_prompt 上游产出)
    //     S8b  Workspace 根目录上下文
    //     S13  Mission Workspace 路径
    //
    //   Tier B · SEMI-STATIC —— 同一 mission 跨轮通常稳定，偶有更新
    //     S10  ProjectMemory 索引
    //     S11  MissionCharter
    //
    //   Tier C · DYNAMIC     —— 每轮都可能变化
    //     S9   TodoLedger 快照
    //     S12  Plan
    //     S14  KnowledgeGraph
    //     S15  ValidationRunner
    //     S16  Checkpoint
    //     S17  HumanCheckpoint
    //     Mailbox 待处理消息
    //     Thread 历史 (append-only — 前缀稳定，append 不破前缀缓存)
    //     本轮 user 输入 (S2-S8 由 assemble_prompt 预拼装)
    //
    // S1-S8 由上游 task_execution_dispatcher::assemble_prompt 串到
    // `system_prompt` / `prompt` 两个参数里：
    //   S1 → system_prompt (本函数 system 消息首条)
    //   S2 base task goal / title
    //   S3 上下文摘要 (knowledge / memory / shared_context)
    //   S4 task_fact_context
    //   S5 skill prompt injections (apply_skill_prompt_injections)
    //   S6 用户规则 (settings.userRules)
    //   S7 生命周期通知 (`<system-reminder>` 包装)
    //   S8 SafetyGate 危险模式
    //  S2-S8 进 `prompt` 用户消息，位于运行时尾部。
    // ===================================================================

    // -------- Tier A · STATIC --------
    // [CACHE: STATIC] S1 · 角色 / agent role 系统提示。
    if let Some(system) = system_prompt {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    // [CACHE: STATIC] S8b · Workspace 根目录上下文。
    // 引导模型把"当前项目 / current repo"等措辞默认对齐到该 workspace；
    // 并强制 Git 状态命令前必须先做 NOT_GIT_WORKTREE 探测。
    if let Some(root_path) = workspace_root_path.as_ref() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(workspace_context_system_prompt(
                &root_path.display().to_string(),
            )),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    // [CACHE: STATIC] S13 · Mission Workspace 路径。
    // 告知 agent 当前 mission 独占的 artifacts/logs/memory 目录，
    // 引导其把产物落在 mission 内，避免散落到用户主目录或随机临时目录。
    // 路径自 mission 创建后不变，因此前移到 STATIC 层。
    if let Some(store) = mission_workspace {
        match store.render_for_prompt(&task.mission_id) {
            Ok(rendered) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Err(err) => {
                tracing::warn!(error = %err, "MissionWorkspace: 渲染 prompt 失败，本轮跳过");
            }
        }
    }

    // ---- Cache breakpoint · STATIC → NON-STATIC ----
    // 上面 Tier A 三段同一角色 / workspace / mission 多轮不变，是 prompt
    // 缓存命中的真正受益面。这里插入一条 boundary 标记消息，下游
    // `AnthropicMessagesAdapter` 在 join system 后据此切分 content blocks,
    // 给静态前缀打 `cache_control: {type: ephemeral}`。其他不支持
    // cache_control 的 adapter 会透明剥离这个标记，不影响输出语义。
    //
    // 仅在 STATIC 段实际产出过至少一条消息时插入，避免空前缀触发退化路径。
    if messages.iter().any(|m| m.role == "system") {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(magi_bridge_client::cache_boundary::PROMPT_CACHE_BOUNDARY.to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }

    // -------- Tier B · SEMI-STATIC --------
    // [CACHE: SEMI-STATIC] S10 · ProjectMemory 索引。
    // 把 `~/.magi/projects/{slug}/memory/MEMORY.md` 视图渲染进 system prompt，
    // 跨 conversation 复用同一项目的长期记忆。仅在 memory_write 后变化。
    // 代理也需要读取项目记忆；是否提示写入由实际工具面决定，避免只读代理看到
    // 不可调用的 `memory_write` 指令。
    let memory_write_visible = tools.as_ref().is_some_and(|definitions| {
        definitions
            .iter()
            .any(|definition| definition.function.name == "memory_write")
    });
    if let Some(store) = project_memory {
        let rendered = if memory_write_visible {
            store.render_for_prompt()
        } else {
            store.render_for_prompt_read_only()
        };
        match rendered {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "ProjectMemory: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    // [CACHE: SEMI-STATIC] S11 · MissionCharter。
    // 当前 mission 的"宪章"（goal / 成功标准 / 约束）作为长效锚点，
    // 长对话或多 Turn 时让 orchestrator 不会偏离最初承诺。mission 创建时
    // 锚定，仅在显式编辑后变化。
    if let Some(store) = mission_charter {
        match store.render_for_prompt(&task.mission_id) {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "MissionCharter: 渲染 prompt 失败，本轮跳过");
            }
        }
    }

    // -------- Tier C · DYNAMIC --------
    // [CACHE: DYNAMIC] S9 · TodoLedger 快照。
    // 本 session 模型在之前轮次写过 todo_write 时，这里把当前列表渲染进 system prompt，
    // 让本轮 Turn 起点自动看到分解 + 进度。每轮 todo_write 后变化。
    if let Some(rendered) = todo_ledger.render_for_prompt() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(rendered),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    // [CACHE: DYNAMIC] S12 · Plan。
    // 当前 mission 的执行计划（steps + 状态 + 依赖）让 orchestrator 在多 Turn
    // 推进时持续看到"下一步是什么、上一步是否做完"，避免漂移。Plan step 状态
    // 切换会触发变化。
    if let Some(store) = plan {
        match store.render_for_prompt(&task.mission_id) {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "Plan: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    // [CACHE: DYNAMIC] S14 · KnowledgeGraph。
    // 把 mission 已经累积的 symbols / decisions / risks 摊在系统提示里，
    // 避免长 mission 跨多个 Conversation 时模型重新讨论已经达成的结论。
    if let Some(store) = knowledge_graph {
        match store.render_for_prompt(&task.mission_id) {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "KnowledgeGraph: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    // [CACHE: DYNAMIC] S15 · ValidationRunner。
    // 把 mission 当前的 Plan 节点验证结果（test_suite / type_check /
    // integration_smoke / benchmark 的 pass/fail/skipped）摊在系统提示里，
    // 让模型在判断"Plan 节点是否真完成"时直接看到验证证据，而不是凭印象口头声明。
    if let Some(store) = validation_runner {
        match store.render_for_prompt(&task.mission_id) {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "ValidationRunner: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    // [CACHE: DYNAMIC] S16 · Checkpoint。
    // 把当前 mission 最近若干检查点摊在系统提示里，让模型在跨进程重启 /
    // context 压缩 / phase 切换之后能定位"上次落到哪一步"，决定是否需要从某个
    // checkpoint 重新拉起 mission。
    if let Some(store) = checkpoint {
        match store.render_for_prompt(&task.mission_id) {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "Checkpoint: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    // [CACHE: DYNAMIC] S17 · HumanCheckpoint。
    // 把当前 mission 待解决的人工审核点与最近若干已解决项摊在系统提示里；
    // 真正的 pending 硬约束由 agent_spawn 拦截与 TaskRunner gate 执行。
    if let Some(store) = human_checkpoint {
        match store.render_for_prompt(&task.mission_id) {
            Ok(Some(rendered)) => {
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(rendered),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "HumanCheckpoint: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    // [CACHE: DYNAMIC] Runtime tail · Mailbox 待处理消息。
    // 来自 user / system / 代理回执的跨 task 投递；按 Conversation 层渲染。
    if let Some(rendered) = render_mailbox_items_for_prompt(&pending_mailbox_items) {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(rendered),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    // [CACHE: APPEND-ONLY] Runtime tail · Thread 历史。
    // P6b：只读取当前 thread 内部已经持久化的运行时输入 / 恢复记录。worker thread
    // 为单 task 独占，因此这里不能出现同 role 的历史 task 对话。历史超出水位线时
    // 直接替换为「摘要 + 最近完整消息」，下一轮不再读到旧结构。
    let thread_history_snapshot = compact_and_replace_thread_history(
        session_store,
        thread_id,
        session_store.thread_message_history(thread_id),
        "pre_turn",
    );
    if !thread_history_snapshot.is_empty() {
        for history_msg in &thread_history_snapshot {
            messages.push(thread_chat_message_to_chat_message(history_msg));
        }
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(
                "以上是当前 thread 在本 task 启动前已有的运行时输入或恢复记录。下面的用户消息是本次执行的当前任务事实，必须以当前任务为准。"
                    .to_string(),
            ),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    // [CACHE: DYNAMIC] Runtime tail · 本轮 user 输入。
    // 含 assemble_prompt 预拼装的 S2-S8（base task + 上下文 + skill 注入 +
    // 用户规则 + lifecycle reminder + safeguard），每轮都重新生成。
    let turn_message_start_index = messages.len();
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    let task_context = task_event_context(task, session_id, workspace_id);
    publish_task_llm_started(
        event_bus,
        task,
        session_id,
        workspace_id,
        &prompt,
        &task_context,
    );

    let mut final_content = String::new();
    let mut tool_call_records: Vec<serde_json::Value> = Vec::new();
    let mut unresolved_tool_failures: Vec<(String, String)> = Vec::new();
    let required_tool_chain = task_required_tool_chain(task, Some(agent_role_registry));
    let mut completed_required_tool_names: Vec<String> = Vec::new();
    let mut last_stream_item_id: Option<String> = None;
    let mut had_tool_calls = false;
    let turn_visibility = task_turn_visibility(
        task,
        is_sidechain,
        worker_id,
        thread_id,
        agent_role_registry,
    );

    if let Some(final_content) = deterministic_task_final_content(task, task_store) {
        append_task_final_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &final_content,
            None,
            streaming_entry_id,
            &turn_visibility,
        );
        return (
            TaskOutcome::Completed {
                output_refs: vec![final_content],
            },
            context_summary,
        );
    }

    let tool_call_round_limit = tool_call_round_limit(&required_tool_chain);
    for round in 0..tool_call_round_limit {
        let thinking_item_id = format!("turn-item-assistant-thinking-{task_id}-{round}");
        let stream_item_id = task_stream_item_id(task_id, round, streaming_entry_id);
        last_stream_item_id = Some(stream_item_id.clone());
        let streamed_thinking = std::cell::RefCell::new(String::new());
        let streamed_visible_content = std::cell::RefCell::new(String::new());
        let last_thinking_len = std::cell::Cell::new(0usize);
        let round_started_at = UtcMillis::now();
        let invocation_request = ModelInvocationRequest {
            provider: LOOPBACK_MODEL_PROVIDER.to_string(),
            prompt: prompt.clone(),
            messages: Some(messages.clone()),
            tools: tools.clone(),
            tool_choice: forced_task_tool_choice_for_round(
                &required_tool_chain,
                tools.as_ref(),
                &completed_required_tool_names,
            ),
        };

        let response = if streaming_entry_id.is_some() {
            let on_delta = |delta: &ModelStreamingDelta| {
                publish_task_thinking_delta(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &thinking_item_id,
                    &last_thinking_len,
                    &streamed_thinking,
                    &turn_visibility,
                    &delta.thinking,
                );
                publish_stream_delta(
                    event_bus,
                    session_store,
                    task_store,
                    task,
                    session_id,
                    workspace_id,
                    &stream_item_id,
                    (round == 0).then_some(stream_item_id.as_str()),
                    &turn_visibility,
                    &streamed_visible_content,
                    &delta.content,
                );
            };

            match client.invoke_streaming(invocation_request, &on_delta) {
                Ok(response) => response,
                Err(error) => {
                    let error_message = error.to_string();
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
                    if task_lease_is_current(task_store, task_id, lease_id) {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &error_message,
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: error_message,
                        },
                        context_summary,
                    );
                }
            }
        } else {
            match client.invoke(invocation_request) {
                Ok(response) => response,
                Err(error) => {
                    let error_message = error.to_string();
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM invocation failed");
                    if task_lease_is_current(task_store, task_id, lease_id) {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &error_message,
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: error_message,
                        },
                        context_summary,
                    );
                }
            }
        };

        let parsed = response.parse_chat_payload();
        let final_thinking = parsed
            .thinking
            .as_deref()
            .map(str::trim)
            .filter(|thinking| !thinking.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                let thinking = streamed_thinking.borrow();
                let trimmed = thinking.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            });
        if let Some(thinking) = final_thinking {
            upsert_task_thinking_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &thinking_item_id,
                &turn_visibility,
                "completed",
                &thinking,
                None,
            );
        }
        publish_model_usage_record(
            event_bus,
            session_store,
            settings_store,
            session_id,
            workspace_id,
            usage_binding,
            format!("task-{}-{}-{round}", task_id, lease_id),
            parsed.usage.as_ref(),
            UsageCallStatus::Success,
            Some(lease_id.to_string()),
            None,
        );
        if let Some(metrics_store) = mission_metrics {
            record_mission_turn(
                metrics_store.as_ref(),
                &task.mission_id,
                parsed.usage.as_ref(),
                round_started_at,
                UtcMillis::now(),
                None,
            );
        }

        if let Some(ref content) = parsed.content {
            final_content = content.clone();
        }
        if !parsed.tool_calls.is_empty() {
            had_tool_calls = true;
        }

        if parsed.tool_calls.is_empty() {
            if !required_tool_chain_is_complete(
                &required_tool_chain,
                &completed_required_tool_names,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: parsed.content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(required_tool_chain_recovery_prompt(
                        &required_tool_chain,
                        &completed_required_tool_names,
                    )),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                continue;
            }
            if let Some(recovery_prompt) = agent_spawn_requirement_recovery_prompt(
                task,
                task_store,
                &tool_call_records,
                &[],
                mission_charter,
                plan,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: parsed.content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(recovery_prompt),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                continue;
            }
            if let Some(recovery_prompt) =
                agent_coordination_recovery_prompt(task, task_store, &tool_call_records)
            {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: parsed.content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(recovery_prompt),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                continue;
            }
            if let Some(recovery_prompt) = agent_result_absorption_recovery_prompt(
                parsed.content.as_deref().unwrap_or(""),
                &tool_call_records,
            ) {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: parsed.content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(recovery_prompt),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
                continue;
            }
            break;
        }

        if let Some(recovery_prompt) = agent_spawn_requirement_recovery_prompt(
            task,
            task_store,
            &tool_call_records,
            &parsed.tool_calls,
            mission_charter,
            plan,
        ) {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: parsed.content.clone(),
                tool_calls: parsed.tool_calls.clone(),
                tool_call_id: None,
            });
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: Some(recovery_prompt),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
            continue;
        }

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
        });

        for tool_call in &parsed.tool_calls {
            append_task_tool_call_started_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                tool_call,
            );
        }

        let tool_results = execute_task_tool_call_batch(
            event_bus,
            tool_registry,
            agent_role_registry,
            skill_runtime,
            task_store,
            session_store,
            execution_registry,
            conversation_registry,
            spawn_graph,
            safety_gate,
            todo_ledger,
            project_memory,
            mission_charter,
            plan,
            knowledge_graph,
            validation_runner,
            checkpoint,
            human_checkpoint,
            task,
            session_id,
            workspace_id,
            workspace_root_path.as_ref(),
            turn_visibility.worker_id(),
            &parsed.tool_calls,
        );

        let mut completed_tool_names_this_round = Vec::new();
        let mut content_requirement_failures = Vec::new();
        for (tool_call, (result, tool_status)) in parsed.tool_calls.iter().zip(tool_results) {
            upsert_task_tool_call_result_turn_item(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                tool_call,
                &result,
                tool_status,
            );
            let canonical_tool_name = canonical_tool_call_name(&tool_call.function.name);
            if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
                let failure_summary = format!(
                    "{}: {}",
                    tool_call.function.name,
                    summarize_tool_result(&result)
                );
                if let Some((_, existing_summary)) = unresolved_tool_failures
                    .iter_mut()
                    .find(|(tool_name, _)| tool_name == &canonical_tool_name)
                {
                    *existing_summary = failure_summary;
                } else {
                    unresolved_tool_failures.push((canonical_tool_name.clone(), failure_summary));
                }
            }
            if matches!(tool_status, ExecutionResultStatus::Succeeded) {
                unresolved_tool_failures.retain(|(tool_name, _)| tool_name != &canonical_tool_name);
                if let Some(failure) = validate_task_content_requirements(
                    task,
                    &canonical_tool_name,
                    tool_call,
                    &result,
                ) {
                    content_requirement_failures.push(failure);
                } else {
                    completed_tool_names_this_round.push(canonical_tool_name);
                }
            }
            tool_call_records.push(tool_call_record(tool_call, &result));
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(result),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call.id.clone()),
            });
        }
        record_completed_required_tools(
            &mut completed_required_tool_names,
            &required_tool_chain,
            &completed_tool_names_this_round,
        );
        if !content_requirement_failures.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: Some(format!(
                    "上一轮工具调用没有满足当前任务的硬性内容要求：{}。请基于当前任务原文重新调用下一个缺失工具，必须逐字保留文件名、marker 和每一行要求。",
                    content_requirement_failures.join("；")
                )),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
    }

    if !required_tool_chain_is_complete(&required_tool_chain, &completed_required_tool_names) {
        let failure_reason = required_tool_chain_recovery_prompt(
            &required_tool_chain,
            &completed_required_tool_names,
        );
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }

    if let Some(recovery_prompt) = agent_spawn_requirement_recovery_prompt(
        task,
        task_store,
        &tool_call_records,
        &[],
        mission_charter,
        plan,
    ) {
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &recovery_prompt,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: recovery_prompt,
            },
            context_summary,
        );
    }

    if let Some(recovery_prompt) =
        agent_coordination_recovery_prompt(task, task_store, &tool_call_records)
    {
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &recovery_prompt,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: recovery_prompt,
            },
            context_summary,
        );
    }

    if final_content.trim().is_empty() {
        let failure_reason = provider_empty_assistant_response_error(had_tool_calls);
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }
    final_content = normalize_model_visible_content(final_content);
    if final_content.trim().is_empty() {
        let failure_reason = provider_empty_assistant_response_error(had_tool_calls);
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }
    if !task_lease_is_current(task_store, task_id, lease_id) {
        return (
            TaskOutcome::Failed {
                error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
            },
            context_summary,
        );
    }

    let failed_tool_summaries = unresolved_tool_failures
        .iter()
        .map(|(_, summary)| summary.clone())
        .collect::<Vec<_>>();
    if let Some(failure_reason) = task_tool_failure_reason(task.kind, &failed_tool_summaries) {
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }

    if task_has_validation_gate(task) && validation_result_rejects_delivery(&final_content) {
        let failure_reason = compact_validation_failure(&final_content);
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason,
            },
            context_summary,
        );
    }

    append_task_final_turn_item(
        event_bus,
        session_store,
        task_store,
        task,
        session_id,
        workspace_id,
        &final_content,
        last_stream_item_id.as_deref().or(streaming_entry_id),
        streaming_entry_id,
        &turn_visibility,
    );

    // P6b：把本轮 LLM 对话追写进当前 thread 的审计 / 恢复记录。
    // 过滤掉 system 消息（prompt、workspace 上下文、边界标记），只沉淀真实对话
    //（user / assistant / tool）。
    // 补写 assistant final：循环里只把 assistant 写进 messages 是在"还有下一轮"时发生，
    // 最终 final_content 作为收尾时没有入列，这里用 final_content 显式收口。
    let mut turn_messages: Vec<ThreadChatMessage> = messages[turn_message_start_index..]
        .iter()
        .filter(|msg| msg.role != "system")
        .map(chat_message_to_thread_chat_message)
        .collect();
    turn_messages.push(ThreadChatMessage {
        role: "assistant".to_string(),
        content: Some(final_content.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    session_store.append_thread_messages(thread_id, turn_messages, UtcMillis::now());
    let _ = compact_and_replace_thread_history(
        session_store,
        thread_id,
        session_store.thread_message_history(thread_id),
        "post_turn",
    );

    (
        TaskOutcome::Completed {
            output_refs: vec![build_output_content(tool_call_records, final_content)],
        },
        context_summary,
    )
}

fn render_mailbox_items_for_prompt(items: &[MailboxItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut rendered = String::from(
        "[mailbox]\n以下是本 Conversation 在上一轮 Turn 之后收到的运行时输入；必须把它们当作当前 Turn 的直接输入处理。\n",
    );
    for (index, item) in items.iter().enumerate() {
        match item {
            MailboxItem::User(signal) => {
                rendered.push_str(&format!(
                    "\n- item: {}\n  author: user\n  kind: message\n  trigger_turn: true\n  payload: {}\n",
                    index + 1,
                    signal.text.as_deref().unwrap_or("")
                ));
            }
            MailboxItem::Runtime(signal) => {
                rendered.push_str(&format!(
                    "\n- item: {}\n  author: {}\n  kind: {}\n  trigger_turn: {}\n  payload: {}\n",
                    index + 1,
                    mailbox_author_label(&signal.author),
                    mailbox_kind_label(signal.kind),
                    signal.trigger_turn,
                    signal.payload
                ));
            }
        }
    }
    Some(rendered)
}

fn validate_task_content_requirements(
    task: &Task,
    tool_name: &str,
    tool_call: &ChatToolCall,
    tool_result: &str,
) -> Option<String> {
    let required_literals = task_required_content_literals(task);
    if required_literals.is_empty() {
        return None;
    }
    let observed_content = match tool_name {
        "file_write" => tool_call_content_argument(tool_call),
        "file_read" => tool_result_content_field(tool_result),
        _ => return None,
    };
    let missing = required_literals
        .iter()
        .filter(|literal| {
            observed_content
                .as_deref()
                .is_none_or(|content| !content.contains(literal.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        None
    } else {
        Some(format!("{tool_name} 内容缺少 {}", missing.join(", ")))
    }
}

fn agent_coordination_recovery_prompt(
    task: &Task,
    task_store: &TaskStore,
    tool_call_records: &[serde_json::Value],
) -> Option<String> {
    let child_tasks = task_store.get_children(&task.task_id);
    if child_tasks.is_empty() {
        return None;
    }

    let pending_child_ids = child_tasks
        .iter()
        .filter(|child| matches!(child.status, TaskStatus::Pending | TaskStatus::Running))
        .map(|child| child.task_id.to_string())
        .collect::<Vec<_>>();
    if !pending_child_ids.is_empty() {
        return Some(format!(
            "你已经启动代理，但仍有代理未进入终态：{}。不要给最终答复；必须调用 agent_wait(task_ids=[...]) 等待并收集这些代理结果。如果部分代理不可用，agent_wait 会返回 degraded/fallback 指令，再由主线改派或接管。",
            pending_child_ids.join(", ")
        ));
    }

    let child_ids = child_tasks
        .iter()
        .map(|child| child.task_id.to_string())
        .collect::<BTreeSet<_>>();
    let collected_ids = collected_agent_wait_child_ids(tool_call_records);
    let missing_ids = child_ids
        .difference(&collected_ids)
        .cloned()
        .collect::<Vec<_>>();
    if missing_ids.is_empty() {
        return None;
    }

    Some(format!(
        "代理已经进入终态，但主线尚未通过 agent_wait 收集这些代理结果：{}。不要直接总结；必须调用 agent_wait(task_ids=[...]) 读取 results[].assignment.goal、child_status、result.final_text 后再合并答复。",
        missing_ids.join(", ")
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentWaitResultSignal {
    child_task_id: String,
    title: Option<String>,
    role: Option<String>,
    status: Option<String>,
    child_status: Option<String>,
    final_text: Option<String>,
    summary: Option<String>,
    error: Option<String>,
}

fn agent_result_absorption_recovery_prompt(
    final_content: &str,
    tool_call_records: &[serde_json::Value],
) -> Option<String> {
    let signals = collected_agent_wait_result_signals(tool_call_records);
    if signals.is_empty() {
        return None;
    }
    let missing = signals
        .iter()
        .filter(|signal| !agent_wait_result_is_covered(final_content, signal))
        .map(|signal| {
            signal
                .title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(signal.child_task_id.as_str())
                .to_string()
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return None;
    }

    Some(format!(
        "你已经通过 agent_wait 收集代理结果，但最终答复没有明确吸收这些代理结果：{}。请重新答复：必须逐项读取 results[].assignment.goal、status、child_status、result.final_text、error；用代理标题或职责明确引用来源，合并结论、证据、风险和缺口后再给最终答复。",
        missing.join(", ")
    ))
}

fn collected_agent_wait_result_signals(
    tool_call_records: &[serde_json::Value],
) -> Vec<AgentWaitResultSignal> {
    let mut signals = Vec::new();
    for record in tool_call_records {
        let Some(tool_call) = record.get("toolCall") else {
            continue;
        };
        let Some(tool_name) = tool_call.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if canonical_tool_call_name(tool_name) != "agent_wait" {
            continue;
        }
        let Some(result_text) = tool_call.get("result").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(result_payload) = serde_json::from_str::<serde_json::Value>(result_text) else {
            continue;
        };
        if result_payload
            .get("timed_out")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let Some(results) = result_payload
            .get("results")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for result in results {
            let Some(child_task_id) = result
                .get("child_task_id")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned)
            else {
                continue;
            };
            let title = result
                .get("assignment")
                .and_then(|assignment| assignment.get("title"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| result.get("title").and_then(serde_json::Value::as_str))
                .and_then(non_empty_owned);
            let role = result
                .get("role")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned);
            let status = result
                .get("status")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned);
            let child_status = result
                .get("child_status")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned);
            let final_text = result
                .get("result")
                .and_then(|result| result.get("final_text"))
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned);
            let summary = result
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned);
            let error = result
                .get("error")
                .and_then(serde_json::Value::as_str)
                .and_then(non_empty_owned);
            if title.is_none() && final_text.is_none() && summary.is_none() && error.is_none() {
                continue;
            }
            signals.push(AgentWaitResultSignal {
                child_task_id,
                title,
                role,
                status,
                child_status,
                final_text,
                summary,
                error,
            });
        }
    }
    signals
}

fn agent_wait_result_is_covered(final_content: &str, signal: &AgentWaitResultSignal) -> bool {
    let normalized_final = normalize_absorption_text(final_content);
    if normalized_final.is_empty() {
        return false;
    }
    let mut anchors = Vec::new();
    anchors.push(signal.child_task_id.as_str());
    if let Some(title) = signal.title.as_deref() {
        anchors.push(title);
    }
    if let Some(final_text) = signal.final_text.as_deref() {
        anchors.extend(agent_result_text_anchors(final_text));
    }
    if let Some(summary) = signal.summary.as_deref() {
        anchors.extend(agent_result_text_anchors(summary));
    }
    if let Some(error) = signal.error.as_deref() {
        anchors.extend(agent_result_text_anchors(error));
    }
    let has_anchor = anchors.into_iter().any(|anchor| {
        let normalized_anchor = normalize_absorption_text(anchor);
        normalized_anchor.chars().count() >= 4 && normalized_final.contains(&normalized_anchor)
    });
    if has_anchor {
        return true;
    }

    let failed_or_degraded = signal
        .status
        .as_deref()
        .is_some_and(|status| matches!(status, "failed" | "degraded"))
        || signal
            .child_status
            .as_deref()
            .is_some_and(|status| matches!(status, "failed" | "killed"));
    failed_or_degraded
        && [
            "失败",
            "不可用",
            "降级",
            "改派",
            "接管",
            "failed",
            "degraded",
        ]
        .iter()
        .any(|marker| normalized_final.contains(marker))
}

fn agent_result_text_anchors(value: &str) -> Vec<&str> {
    value
        .split(['\n', '。', '；', ';', '.', '!', '！', '?', '？'])
        .map(str::trim)
        .filter(|part| part.chars().count() >= 8)
        .take(3)
        .collect()
}

fn normalize_absorption_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn non_empty_owned(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn agent_spawn_requirement_recovery_prompt(
    task: &Task,
    task_store: &TaskStore,
    tool_call_records: &[serde_json::Value],
    proposed_tool_calls: &[ChatToolCall],
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
) -> Option<String> {
    if !agent_spawn_required_by_task(task) {
        return None;
    }
    if !task_store.get_children(&task.task_id).is_empty() {
        return None;
    }
    if agent_spawn_was_attempted(tool_call_records) {
        return None;
    }
    if task_is_long_mission(Some(task)) {
        let governance_ready =
            long_mission_governance_prerequisites_available(task, mission_charter, plan);
        if !proposed_tool_calls.is_empty() {
            let proposed_names = proposed_tool_calls
                .iter()
                .map(|tool_call| canonical_tool_call_name(&tool_call.function.name))
                .collect::<Vec<_>>();
            if proposed_names
                .iter()
                .all(|name| long_mission_governance_tool_can_precede_agent_spawn(name))
            {
                return None;
            }
            if proposed_names.iter().all(|name| name == "agent_spawn") && governance_ready {
                return None;
            }
            if proposed_names.iter().any(|name| name == "agent_spawn") && !governance_ready {
                return Some(long_mission_agent_spawn_prerequisite_prompt());
            }
        }
        if !governance_ready {
            return Some(long_mission_agent_spawn_prerequisite_prompt());
        }
    }
    if !proposed_tool_calls.is_empty()
        && proposed_tool_calls
            .iter()
            .all(|tool_call| canonical_tool_call_name(&tool_call.function.name) == "agent_spawn")
    {
        return None;
    }

    Some(
        "用户已经明确要求启动或派发代理。本轮必须调用 agent_spawn 履行代理契约；不要把主线 shell_exec、file_read 或直接总结冒充为代理执行结果。若需要多个代理，应在同一轮发起多次 agent_spawn 并为每个代理写清 display_name、role、goal 与 access_mode。"
            .to_string(),
    )
}

fn long_mission_governance_prerequisites_available(
    task: &Task,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
) -> bool {
    let Some(mission_charter) = mission_charter else {
        return false;
    };
    if !matches!(mission_charter.load(&task.mission_id), Ok(Some(_))) {
        return false;
    }
    let Some(plan) = plan else {
        return false;
    };
    matches!(plan.load(&task.mission_id), Ok(Some(plan)) if !plan.steps.is_empty())
}

fn long_mission_governance_tool_can_precede_agent_spawn(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "mission_charter_write"
            | "plan_write"
            | "todo_write"
            | "memory_write"
            | "kg_write"
            | "human_checkpoint_request"
    )
}

fn long_mission_agent_spawn_prerequisite_prompt() -> String {
    "用户已经明确要求启动或派发代理，但当前任务是 LongMission：必须先调用 mission_charter_write 建立 mission 契约，再调用 plan_write 写入非空执行计划，然后才能 agent_spawn。不要用 todo_write 替代 mission plan；不要在 charter/plan 前派发代理。".to_string()
}

fn agent_spawn_required_by_task(task: &Task) -> bool {
    if task.parent_task_id.is_some() {
        return false;
    }
    let text = format!("{} {}", task.title, task.goal);
    let lowered = text.to_ascii_lowercase();
    if contains_any(
        &text,
        &[
            "不要启动代理",
            "不要使用代理",
            "不需要代理",
            "无需代理",
            "不要派发代理",
        ],
    ) || contains_any(
        &lowered,
        &[
            "no agent",
            "no agents",
            "without agent",
            "without agents",
            "do not use agent",
            "don't use agent",
        ],
    ) {
        return false;
    }

    if lowered.contains("agent_spawn") {
        return true;
    }

    let mentions_agent = text.contains("代理")
        || lowered.contains("agent")
        || contains_any(
            &lowered,
            &["explorer", "reviewer", "architect", "executor", "tester"],
        )
        || contains_any(
            &text,
            &[
                "探索工程师",
                "评审工程师",
                "架构师",
                "执行工程师",
                "测试工程师",
            ],
        );
    if !mentions_agent {
        return false;
    }

    let has_chinese_dispatch_verb = contains_any(
        &text,
        &["启动", "派发", "分配", "调用", "创建", "拉起", "开启"],
    ) || (text.contains("使用")
        && contains_any(
            &text,
            &[
                "使用代理完成",
                "使用代理处理",
                "使用代理执行",
                "使用代理验证",
                "使用代理检查",
                "使用代理审查",
                "使用多个代理",
                "使用两个代理",
                "使用多代理",
            ],
        ));
    let has_english_dispatch_verb = contains_any(
        &lowered,
        &[
            "spawn",
            "start",
            "launch",
            "dispatch",
            "assign",
            "use agent to",
            "use agents to",
        ],
    );
    has_chinese_dispatch_verb || has_english_dispatch_verb
}

fn agent_spawn_was_attempted(tool_call_records: &[serde_json::Value]) -> bool {
    tool_call_records.iter().any(|record| {
        record
            .get("toolCall")
            .and_then(|tool_call| tool_call.get("name"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| canonical_tool_call_name(name) == "agent_spawn")
    })
}

fn collected_agent_wait_child_ids(tool_call_records: &[serde_json::Value]) -> BTreeSet<String> {
    let mut collected = BTreeSet::new();
    for record in tool_call_records {
        let Some(tool_call) = record.get("toolCall") else {
            continue;
        };
        let Some(tool_name) = tool_call.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if canonical_tool_call_name(tool_name) != "agent_wait" {
            continue;
        }
        let Some(result_text) = tool_call.get("result").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(result_payload) = serde_json::from_str::<serde_json::Value>(result_text) else {
            continue;
        };
        if result_payload
            .get("timed_out")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let Some(results) = result_payload
            .get("results")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for result in results {
            if let Some(child_task_id) = result
                .get("child_task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                collected.insert(child_task_id.to_string());
            }
        }
    }
    collected
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn task_required_content_literals(task: &Task) -> Vec<String> {
    if task.kind != magi_core::TaskKind::LocalAgent {
        return Vec::new();
    }
    let goal = task.goal.trim();
    let Some((_, after_anchor)) = goal
        .split_once("文件内容必须包含")
        .or_else(|| goal.split_once("content must contain"))
        .or_else(|| goal.split_once("must contain"))
    else {
        return Vec::new();
    };
    let requirement = after_anchor
        .split(['。', '\n'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches(['：', ':'])
        .trim_start_matches("三行")
        .trim_start_matches(['：', ':'])
        .trim();
    requirement
        .split(['、', '；', ';'])
        .map(|part| part.trim().trim_matches(['，', ',', '。', '.']))
        .filter(|part| part.contains(':'))
        .map(ToOwned::to_owned)
        .collect()
}

fn tool_call_content_argument(tool_call: &ChatToolCall) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
        .ok()
        .and_then(|value| {
            value
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn tool_result_content_field(tool_result: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(tool_result)
        .ok()
        .and_then(|value| {
            value
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn task_has_validation_gate(task: &Task) -> bool {
    task.policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.validation_profile.is_some())
}

fn mailbox_author_label(author: &MailboxAuthor) -> String {
    match author {
        MailboxAuthor::User => "user".to_string(),
        MailboxAuthor::Agent(id) => format!("agent:{id}"),
        MailboxAuthor::System => "system".to_string(),
        MailboxAuthor::Parent(id) => format!("parent:{id}"),
        MailboxAuthor::Child(id) => format!("child:{id}"),
    }
}

fn mailbox_kind_label(kind: MailboxKind) -> &'static str {
    match kind {
        MailboxKind::Message => "message",
        MailboxKind::Decision => "decision",
        MailboxKind::Interrupt => "interrupt",
        MailboxKind::Followup => "followup",
    }
}

fn task_event_context(
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
) -> EventContext {
    EventContext {
        workspace_id: workspace_id.clone(),
        session_id: Some(session_id.clone()),
        mission_id: Some(task.mission_id.clone()),
        task_id: Some(task.task_id.clone()),
        ..EventContext::default()
    }
}

fn publish_task_llm_started(
    event_bus: &InMemoryEventBus,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    prompt: &str,
    task_context: &EventContext,
) {
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-llm-started-{}", UtcMillis::now().0)),
            "task.llm.started",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "prompt_length": prompt.len(),
            }),
        )
        .with_context(task_context.clone()),
    );
}

fn publish_stream_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    timeline_entry_id: Option<&str>,
    turn_visibility: &TaskTurnVisibility,
    streamed_visible_content: &std::cell::RefCell<String>,
    accumulated_text: &str,
) {
    let visible_text = normalize_model_stream_preview_content(accumulated_text);
    if visible_text.trim().is_empty() {
        return;
    }
    let stream_update = {
        let previous = streamed_visible_content.borrow();
        let update = session_turn_stream_update(&previous, &visible_text);
        if update.is_none() {
            return;
        }
        update
    };
    {
        let mut previous = streamed_visible_content.borrow_mut();
        previous.clear();
        previous.push_str(&visible_text);
    }
    if let Some(timeline_entry_id) = timeline_entry_id.filter(|_| turn_visibility.is_mainline()) {
        session_store.upsert_timeline_entry(
            session_id.clone(),
            timeline_entry_id,
            TimelineEntryKind::AssistantMessage,
            &visible_text,
        );
    }
    let mut item = session_turn_item(
        "assistant_stream",
        "running",
        Some("生成回复".to_string()),
        Some(visible_text),
        Some(item_id.to_string()),
        turn_visibility.thread_id().clone(),
    );
    // 子任务流式文本归 task 详情，主线靠父代理的 agent_spawn ToolCall 卡片呈现进度，
    // 不再让同一条流式内容同时污染主线与详情页。
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event_with_stream_update(
            event_bus,
            session_id,
            workspace_id,
            &published,
            stream_update.as_ref(),
        );
    }
}

fn publish_task_thinking_delta(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    last_sent_len: &std::cell::Cell<usize>,
    streamed_thinking: &std::cell::RefCell<String>,
    turn_visibility: &TaskTurnVisibility,
    accumulated_thinking: &str,
) {
    if accumulated_thinking.len() <= last_sent_len.get() {
        return;
    }
    let trimmed = accumulated_thinking.trim();
    if trimmed.is_empty() {
        return;
    }
    let stream_update = {
        let previous = streamed_thinking.borrow();
        let update = session_turn_stream_update(previous.trim(), trimmed);
        if update.is_none() {
            return;
        }
        update
    };
    last_sent_len.set(accumulated_thinking.len());
    {
        let mut thinking = streamed_thinking.borrow_mut();
        thinking.clear();
        thinking.push_str(accumulated_thinking);
    }
    upsert_task_thinking_turn_item(
        event_bus,
        session_store,
        task_store,
        task,
        session_id,
        workspace_id,
        item_id,
        turn_visibility,
        "running",
        trimmed,
        stream_update.as_ref(),
    );
}

fn upsert_task_thinking_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    item_id: &str,
    turn_visibility: &TaskTurnVisibility,
    status: &str,
    thinking: &str,
    stream_update: Option<&SessionTurnStreamUpdate>,
) {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut item = session_turn_item(
        "assistant_thinking",
        status,
        Some("模型思考".to_string()),
        Some(trimmed.to_string()),
        Some(item_id.to_string()),
        turn_visibility.thread_id().clone(),
    );
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event_with_stream_update(
            event_bus,
            session_id,
            workspace_id,
            &published,
            stream_update,
        );
    }
}

fn append_task_tool_call_started_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    tool_call: &ChatToolCall,
) {
    let mut item = session_turn_item(
        "tool_call_started",
        "running",
        Some(tool_call.function.name.clone()),
        Some(format!("正在调用工具：{}", tool_call.function.name)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
        turn_visibility.thread_id().clone(),
    );
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some("running".to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn upsert_task_tool_call_result_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    tool_call: &ChatToolCall,
    tool_result: &str,
    tool_status: ExecutionResultStatus,
) {
    let status_label = tool_execution_status_label(tool_status);
    let mut item = session_turn_item(
        "tool_call_result",
        turn_item_status_for_tool_result(tool_status),
        Some(tool_call.function.name.clone()),
        Some(summarize_tool_result(tool_result)),
        Some(format!("turn-item-tool-{}", tool_call.id)),
        turn_visibility.thread_id().clone(),
    );
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    item.tool_call_id = Some(tool_call.id.clone());
    item.tool_name = Some(tool_call.function.name.clone());
    item.tool_status = Some(status_label.to_string());
    item.tool_arguments = Some(tool_call.function.arguments.clone());
    item.tool_result = Some(tool_result.to_string());
    if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
        item.tool_error = Some(tool_result.to_string());
    }
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
}

fn tool_call_record(tool_call: &ChatToolCall, result: &str) -> serde_json::Value {
    let status = infer_tool_call_status(result);
    serde_json::json!({
        "type": "tool_call",
        "content": format!("{}: {}", tool_call.function.name, summarize_tool_result(result)),
        "toolCall": {
            "id": tool_call.id,
            "name": tool_call.function.name,
            "arguments": serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                .unwrap_or(serde_json::Value::String(tool_call.function.arguments.clone())),
            "status": status,
            "result": result,
        }
    })
}

fn append_task_final_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    final_content: &str,
    final_item_id: Option<&str>,
    timeline_entry_id: Option<&str>,
    turn_visibility: &TaskTurnVisibility,
) {
    let has_requested_final_item_id = final_item_id.is_some();
    let mut final_item = session_turn_item(
        "assistant_final",
        "completed",
        Some("最终回复".to_string()),
        Some(final_content.to_string()),
        final_item_id.map(str::to_string),
        turn_visibility.thread_id().clone(),
    );
    apply_task_final_visibility(&mut final_item, task_store, task, turn_visibility);
    if let Some(timeline_entry_id) = timeline_entry_id {
        final_item.timeline_entry_id = Some(timeline_entry_id.to_string());
    }
    let final_item_id = final_item.item_id.clone();
    if has_requested_final_item_id {
        if let Some(published) = upsert_session_turn_item_with_task_store(
            session_store,
            session_id,
            final_item,
            Some(task_store),
        ) {
            publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
        }
    } else if let Some(published) = append_session_turn_item_with_task_store(
        session_store,
        session_id,
        final_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    let root_task_completed = task_store
        .get_task(&task.root_task_id)
        .is_some_and(|root_task| root_task.status == TaskStatus::Completed);
    if turn_visibility.is_mainline() && root_task_completed {
        let _ = session_store.update_current_turn_status(session_id, "completed");
        publish_current_session_turn_item_event(
            event_bus,
            session_store,
            session_id,
            workspace_id,
            &final_item_id,
            Some(task_store),
        );
    }
}

fn append_task_error_turn_item(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    error_text: &str,
    _streaming_entry_id: Option<&str>,
) {
    let mut error_item = session_turn_item(
        "assistant_error",
        "failed",
        Some("回复生成失败".to_string()),
        Some(error_text.to_string()),
        Some(format!("turn-item-assistant-error-{}", UtcMillis::now().0)),
        turn_visibility.thread_id().clone(),
    );
    apply_task_turn_visibility(&mut error_item, task, turn_visibility);
    let error_item_id = error_item.item_id.clone();
    if let Some(published) = append_session_turn_item_with_task_store(
        session_store,
        session_id,
        error_item,
        Some(task_store),
    ) {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
    if turn_visibility.is_mainline() {
        let _ = session_store.update_current_turn_status(session_id, "failed");
        publish_current_session_turn_item_event(
            event_bus,
            session_store,
            session_id,
            workspace_id,
            &error_item_id,
            Some(task_store),
        );
    }
}

fn build_output_content(
    mut tool_call_records: Vec<serde_json::Value>,
    final_content: String,
) -> String {
    if tool_call_records.is_empty() {
        return final_content;
    }
    tool_call_records.push(serde_json::json!({
        "type": "text",
        "content": final_content,
    }));
    serde_json::json!({ "blocks": tool_call_records }).to_string()
}

fn task_lease_is_current(task_store: &TaskStore, task_id: &TaskId, lease_id: &LeaseId) -> bool {
    task_store
        .get_active_lease(task_id)
        .is_some_and(|lease| lease.lease_id == *lease_id)
}

fn task_stream_item_id(task_id: &TaskId, round: usize, streaming_entry_id: Option<&str>) -> String {
    if round == 0 {
        return streaming_entry_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("turn-item-assistant-stream-{task_id}-{round}"));
    }
    format!("turn-item-assistant-stream-{task_id}-{round}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{BridgeClientError, BridgeErrorLayer, BridgeResponse};
    use magi_core::{
        ApprovalRequirement, MissionId, RiskLevel, Task, TaskKind, TaskStatus, TaskTier, WorkerId,
    };
    use magi_governance::GovernanceService;
    use magi_session_store::{
        ActiveExecutionTurn, CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus,
        ExecutionThread, ExecutionThreadStatus, TimelineEntryKind,
    };
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec, ToolExecutionContext};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    struct TaskToolBatchModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    struct FailingTaskModelBridgeClient;
    struct StaticTaskFinalModelBridgeClient {
        content: &'static str,
    }
    struct TaskToolFailureThenFinalModelBridgeClient {
        invoke_count: AtomicUsize,
    }
    struct RecoverableTaskToolFailureModelBridgeClient {
        invoke_count: AtomicUsize,
    }

    impl ModelBridgeClient for TaskToolBatchModelBridgeClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [
                        {
                            "id": "task-tool-shell-a",
                            "type": "function",
                            "function": {
                                "name": "shell_exec",
                                "arguments": serde_json::json!({
                                    "command": "printf a",
                                    "access_mode": "read_only"
                                }).to_string()
                            }
                        },
                        {
                            "id": "task-tool-shell-b",
                            "type": "function",
                            "function": {
                                "name": "shell_exec",
                                "arguments": serde_json::json!({
                                    "command": "printf b",
                                    "access_mode": "read_only"
                                }).to_string()
                            }
                        }
                    ]
                })
            } else {
                let tool_message_ids = request
                    .messages
                    .as_ref()
                    .expect("工具响应轮次必须携带消息上下文")
                    .iter()
                    .filter(|message| message.role == "tool")
                    .map(|message| message.tool_call_id.as_deref())
                    .collect::<Vec<_>>();
                assert_eq!(
                    tool_message_ids,
                    vec![Some("task-tool-shell-a"), Some("task-tool-shell-b")]
                );
                serde_json::json!({
                    "content": "任务工具调用完成",
                    "finish_reason": "stop"
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            if self.invoke_count.load(Ordering::SeqCst) > 0 {
                on_delta(&ModelStreamingDelta {
                    content: "任务工具调用完成".to_string(),
                    thinking: String::new(),
                });
            }
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for FailingTaskModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32099),
                message: "model bridge unavailable".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for StaticTaskFinalModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            Ok(BridgeResponse {
                ok: true,
                payload: serde_json::json!({
                    "content": self.content,
                    "finish_reason": "stop"
                })
                .to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            on_delta(&ModelStreamingDelta {
                content: self.content.to_string(),
                thinking: String::new(),
            });
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for TaskToolFailureThenFinalModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = if index == 0 {
                serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [{
                        "id": "task-tool-failure",
                        "type": "function",
                        "function": {
                            "name": "missing_builtin_tool",
                            "arguments": "{}"
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "content": "FLOW_SHOULD_NOT_COMPLETE",
                    "finish_reason": "stop"
                })
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            if self.invoke_count.load(Ordering::SeqCst) > 0 {
                on_delta(&ModelStreamingDelta {
                    content: "FLOW_SHOULD_NOT_COMPLETE".to_string(),
                    thinking: String::new(),
                });
            }
            self.invoke(request)
        }
    }

    impl ModelBridgeClient for RecoverableTaskToolFailureModelBridgeClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            let index = self.invoke_count.fetch_add(1, Ordering::SeqCst);
            let payload = match index {
                0 => serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [{
                        "id": "recoverable-tool-failure",
                        "type": "function",
                        "function": {
                            "name": "recoverable_probe",
                            "arguments": "{\"attempt\":1}"
                        }
                    }]
                }),
                1 => serde_json::json!({
                    "content": null,
                    "finish_reason": "tool_calls",
                    "tool_calls": [{
                        "id": "recoverable-tool-success",
                        "type": "function",
                        "function": {
                            "name": "recoverable_probe",
                            "arguments": "{\"attempt\":2}"
                        }
                    }]
                }),
                _ => serde_json::json!({
                    "content": "工具失败已通过重试恢复，任务可以完成。",
                    "finish_reason": "stop"
                }),
            };
            Ok(BridgeResponse {
                ok: true,
                payload: payload.to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            if self.invoke_count.load(Ordering::SeqCst) > 1 {
                on_delta(&ModelStreamingDelta {
                    content: "工具失败已通过重试恢复，任务可以完成。".to_string(),
                    thinking: String::new(),
                });
            }
            self.invoke(request)
        }
    }

    #[test]
    fn full_action_extracts_required_tool_chain_in_goal_order() {
        let mut task = make_task_loop_test_task("task-required-tool-chain");
        task.goal =
            "按顺序调用：1 shell_exec；2 file_mkdir；3 file_write；4 file_read；5 file_remove"
                .to_string();
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });

        assert_eq!(
            task_required_tool_chain(&task, None),
            vec![
                "shell_exec".to_string(),
                "file_mkdir".to_string(),
                "file_write".to_string(),
                "file_read".to_string(),
                "file_remove".to_string()
            ]
        );

        task.policy_snapshot.as_mut().expect("policy").command_mode = "read_only".to_string();
        assert!(
            task_required_tool_chain(&task, None).is_empty(),
            "只读阶段即使复述用户目标，也不能强制执行写工具链"
        );
    }

    #[test]
    fn local_agent_infers_file_write_and_read_from_concrete_file_goal() {
        let mut task = make_task_loop_test_task("task-required-tool-chain-natural-language");
        task.goal = "请在当前工作区创建文件 v2-task-system-e2e.md，文件内容必须包含 marker: V2_TASK_E2E。创建后读取该文件验证内容。"
            .to_string();
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });

        assert_eq!(
            task_required_tool_chain(&task, None),
            vec!["file_write".to_string(), "file_read".to_string()]
        );
    }

    #[test]
    fn coordinator_task_does_not_convert_orchestration_goal_to_forced_tool_chain() {
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let mut task = make_task_loop_test_task("task-coordinator-required-tool-chain");
        task.goal = "LongMission：先 mission_charter_write，再 plan_write，然后两轮 agent_spawn + agent_wait，最后 validation_record 和 checkpoint_create。"
            .to_string();
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        task.executor_binding = Some(serde_json::json!({
            "target_role": "coordinator",
        }));

        assert!(
            task_required_tool_chain(&task, Some(&registry)).is_empty(),
            "协调器必须保留自适应编排空间，不能被执行叶子的强制工具链锁死"
        );
    }

    #[test]
    fn content_requirement_validation_rejects_marker_typos() {
        let mut task = make_task_loop_test_task("task-content-requirement");
        task.goal = "请创建文件 demo.md，文件内容必须包含三行：title: v2 task concrete progress、marker: V2_TASK_E2E_123、status: completed。创建后读取该文件验证内容。"
            .to_string();
        let bad_write = ChatToolCall {
            id: "call-bad-write".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "file_write".to_string(),
                arguments: serde_json::json!({
                    "path": "/tmp/demo.md",
                    "content": "title: v2 task concrete progress\nmarker: V2_TASK_EE_123\nstatus: completed\n"
                })
                .to_string(),
            },
        };
        let good_write = ChatToolCall {
            id: "call-good-write".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "file_write".to_string(),
                arguments: serde_json::json!({
                    "path": "/tmp/demo.md",
                    "content": "title: v2 task concrete progress\nmarker: V2_TASK_E2E_123\nstatus: completed\n"
                })
                .to_string(),
            },
        };

        assert_eq!(
            task_required_content_literals(&task),
            vec![
                "title: v2 task concrete progress".to_string(),
                "marker: V2_TASK_E2E_123".to_string(),
                "status: completed".to_string()
            ]
        );
        assert!(
            validate_task_content_requirements(&task, "file_write", &bad_write, "{}")
                .is_some_and(|failure| failure.contains("marker: V2_TASK_E2E_123"))
        );
        assert!(
            validate_task_content_requirements(&task, "file_write", &good_write, "{}").is_none()
        );
    }

    #[test]
    fn task_tool_call_round_limit_keeps_final_round_after_explicit_chain() {
        let required_tool_chain = [
            "file_mkdir",
            "file_write",
            "file_read",
            "file_patch",
            "search_text",
            "shell_exec",
            "diff_preview",
            "diagram_render",
            "file_remove",
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

        assert!(
            tool_call_round_limit(&required_tool_chain) >= required_tool_chain.len() + 2,
            "显式工具链不能因为固定轮数耗尽而丢失最后的工具或总结轮"
        );
    }

    #[test]
    fn planning_no_tool_action_and_validation_are_deterministic() {
        let task_store = TaskStore::new();
        let mut planning = make_task_loop_test_task("task-planning-deterministic");
        planning.title = "梳理目标".to_string();
        planning.goal = "明确目标、边界和验收标准：<<<MAGI_TASK_GOAL>>>\n执行指定工具链\n<<<END_MAGI_TASK_GOAL>>>"
            .to_string();
        planning.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "no_tools".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let planning_content =
            deterministic_task_final_content(&planning, &task_store).expect("planning content");
        assert!(planning_content.contains("目标：执行指定工具链"));
        assert!(planning_content.contains("边界："));
        assert!(planning_content.contains("执行计划："));
        assert!(planning_content.contains("验收标准："));

        planning.output_refs = vec![planning_content];
        task_store.insert_task(planning);
        let mut validation = make_task_loop_test_task("task-planning-validation-deterministic");
        validation.kind = TaskKind::LocalAgent;
        validation.title = "规划 验证".to_string();
        validation.goal =
            "验证 规划 阶段产出是否包含目标、边界、执行计划和验收标准；只验证规划文本完整性"
                .to_string();
        validation.dependency_ids = vec![TaskId::new("task-planning-deterministic")];
        let validation_content = deterministic_task_final_content(&validation, &task_store)
            .expect("planning validation content");

        assert!(validation_content.starts_with("通过。"));
    }

    #[test]
    fn execution_validation_uses_dependency_structured_output() {
        let task_store = TaskStore::new();
        let mut action = make_task_loop_test_task("task-execution-output");
        action.goal = "按顺序调用 file_mkdir、file_write、file_read、file_patch、search_text、shell_exec、diff_preview、diagram_render、file_remove"
            .to_string();
        action.output_refs = vec![
            serde_json::json!({
                "blocks": [
                    successful_tool_output_block("file_mkdir"),
                    successful_tool_output_block("file_write"),
                    successful_tool_output_block("file_read"),
                    successful_tool_output_block("file_patch"),
                    successful_tool_output_block("search_text"),
                    successful_tool_output_block("shell_exec"),
                    successful_tool_output_block("diff_preview"),
                    successful_tool_output_block("diagram_render"),
                    successful_tool_output_block("file_remove"),
                    {
                        "type": "text",
                        "content": "DEEP_TASK_DONE_TEST"
                    }
                ]
            })
            .to_string(),
        ];
        task_store.insert_task(action);

        let mut validation = make_task_loop_test_task("task-execution-validation");
        validation.kind = TaskKind::LocalAgent;
        validation.title = "执行 验证".to_string();
        validation.goal = "验证 执行 阶段是否按用户目标完成实际执行和工具结果。".to_string();
        validation.dependency_ids = vec![TaskId::new("task-execution-output")];

        let validation_content = deterministic_task_final_content(&validation, &task_store)
            .expect("execution validation should be deterministic from dependency output");

        assert!(validation_content.starts_with("通过。"));
        assert!(validation_content.contains("file_remove"));
        assert!(!validation_result_rejects_delivery(&validation_content));
    }

    fn successful_tool_output_block(tool_name: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "tool_call",
            "content": format!("{tool_name}: ok"),
            "toolCall": {
                "id": format!("call-{tool_name}"),
                "name": tool_name,
                "arguments": {},
                "status": "success",
                "result": serde_json::json!({
                    "tool": tool_name,
                    "status": "succeeded",
                    "summary": "ok"
                }).to_string()
            }
        })
    }

    struct ConcurrentTaskToolProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
        delay: Duration,
    }

    impl ConcurrentTaskToolProbe {
        fn new(delay: Duration) -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                delay,
            }
        }

        fn max_active(&self) -> usize {
            self.max_active.load(Ordering::SeqCst)
        }

        fn record_active_call(&self) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(self.delay);
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct ProbeTaskBuiltinTool {
        name: &'static str,
        probe: Arc<ConcurrentTaskToolProbe>,
    }
    struct RecoverableProbeTool {
        attempts: AtomicUsize,
    }

    impl ProbeTaskBuiltinTool {
        fn new(name: &'static str, probe: Arc<ConcurrentTaskToolProbe>) -> Self {
            Self { name, probe }
        }
    }

    impl BuiltinTool for ProbeTaskBuiltinTool {
        fn name(&self) -> &'static str {
            self.name
        }

        fn execute(&self, input: &str, _context: &ToolExecutionContext) -> String {
            self.probe.record_active_call();
            serde_json::json!({
                "tool": self.name,
                "status": "succeeded",
                "stdout": format!("{} done", self.name),
                "input": input,
            })
            .to_string()
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name.to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    impl BuiltinTool for RecoverableProbeTool {
        fn name(&self) -> &'static str {
            "recoverable_probe"
        }

        fn execute(&self, input: &str, _context: &ToolExecutionContext) -> String {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 1 {
                return serde_json::json!({
                    "tool": self.name(),
                    "status": "failed",
                    "error": "首次验证证据不足",
                    "input": input,
                })
                .to_string();
            }
            serde_json::json!({
                "tool": self.name(),
                "status": "succeeded",
                "stdout": "重试成功",
                "input": input,
            })
            .to_string()
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name().to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    fn make_task_loop_test_task(task_id: &str) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-task-loop"),
            root_task_id: TaskId::new(task_id),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "验证 worker 工具并发".to_string(),
            goal: "确认 worker 在同一轮内可以并发完成只读操作并保持消息顺序".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        }
    }

    fn run_static_task_final(task: &Task, content: &'static str) -> TaskOutcome {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let task_store = TaskStore::new();
        task_store.insert_task(task.clone());
        let worker_id = WorkerId::new(format!("worker-{}", task.task_id));
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "reviewer",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let client = StaticTaskFinalModelBridgeClient { content };
        let session_id = SessionId::new(format!("session-{}", task.task_id));
        let workspace_id = Some(WorkspaceId::new(format!("workspace-{}", task.task_id)));
        session_store
            .create_session(session_id.clone(), "static task final fixture")
            .expect("session should be creatable");
        let now = UtcMillis::now();
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || task.mission_id.clone());
        // P7：mainline 场景 task 自身 thread = orchestrator thread。
        let thread_id = orchestrator_thread_id.clone();
        let (outcome, _) = run_conversation_loop(ConversationLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            execution_registry: &TaskExecutionRegistry::default(),
            conversation_registry: &ConversationRegistry::new(),
            agent_role_registry: &magi_agent_role::AgentRoleRegistry::load_default(),
            spawn_graph: &std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new()),
            safety_gate: None,
            todo_ledger: &magi_todo_ledger::TodoLedger::new(),
            project_memory: None,
            mission_charter: None,
            plan: None,
            mission_workspace: None,
            knowledge_graph: None,
            validation_runner: None,
            checkpoint: None,
            human_checkpoint: None,
            mission_metrics: None,
            task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请执行任务".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            is_sidechain: false,
            worker_id: Some(&worker_id),
            thread_id: &thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });
        outcome
    }

    #[test]
    fn validation_task_negative_final_marks_task_failed() {
        let mut task = make_task_loop_test_task("task-validation-negative-final");
        task.kind = TaskKind::LocalAgent;
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "turn".to_string(),
            task_tier: TaskTier::ExecutionChain,
            background_allowed: false,
            escalation_conditions: Vec::new(),
        });

        let outcome = run_static_task_final(&task, "不通过。\n\n原因：缺少文件写入证据。");

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("验证未通过"));
                assert!(error.contains("缺少文件写入证据"));
            }
            other => panic!("validation negative final must fail task, got {other:?}"),
        }
    }

    #[test]
    fn action_task_negative_wording_does_not_fail_validation_gate() {
        let task = make_task_loop_test_task("task-action-negative-wording");

        let outcome = run_static_task_final(
            &task,
            "不通过这个词只是普通任务报告里的示例，不代表验证结论。",
        );

        match outcome {
            TaskOutcome::Completed { output_refs } => {
                assert_eq!(output_refs.len(), 1);
            }
            other => panic!("action task should not use validation wording gate, got {other:?}"),
        }
    }

    #[test]
    fn validation_gate_rejects_conclusion_negative_and_partial_pass() {
        assert!(validation_result_rejects_delivery(
            "结论：**不通过**。\n缺少关键证据。"
        ));
        assert!(validation_result_rejects_delivery(
            "已部分通过，完整验收未能确认后续步骤。"
        ));
        assert!(!validation_result_rejects_delivery(
            "通过。\n已核验 shell 输出、文件读取和删除结果。"
        ));
    }

    #[test]
    fn agent_coordination_blocks_final_until_children_are_waited() {
        let task_store = TaskStore::new();
        let root = make_task_loop_test_task("task-agent-coordination-root");
        let mut child = make_task_loop_test_task("task-agent-coordination-child");
        child.root_task_id = root.task_id.clone();
        child.parent_task_id = Some(root.task_id.clone());
        child.title = "目录观察代理".to_string();
        child.status = TaskStatus::Running;
        task_store.insert_task(root.clone());
        task_store.insert_task(child.clone());

        let pending_prompt = agent_coordination_recovery_prompt(&root, &task_store, &[])
            .expect("running child should block final answer");
        assert!(pending_prompt.contains("仍有代理未进入终态"));
        assert!(pending_prompt.contains("agent_wait"));

        child.status = TaskStatus::Completed;
        child.output_refs = vec!["代理完成".to_string()];
        task_store.insert_task(child.clone());
        let missing_wait_prompt = agent_coordination_recovery_prompt(&root, &task_store, &[])
            .expect("completed child without agent_wait should block final answer");
        assert!(missing_wait_prompt.contains("尚未通过 agent_wait 收集"));

        let timed_out_wait_record = serde_json::json!({
            "type": "tool_call",
            "toolCall": {
                "name": "agent_wait",
                "result": serde_json::json!({
                    "tool": "agent_wait",
                    "status": "timeout",
                    "timed_out": true,
                    "results": [{ "child_task_id": child.task_id.to_string() }]
                }).to_string()
            }
        });
        assert!(
            agent_coordination_recovery_prompt(&root, &task_store, &[timed_out_wait_record])
                .is_some(),
            "timeout wait 不能算作已收集终态结果"
        );

        let completed_wait_record = serde_json::json!({
            "type": "tool_call",
            "toolCall": {
                "name": "agent_wait",
                "result": serde_json::json!({
                    "tool": "agent_wait",
                    "status": "succeeded",
                    "timed_out": false,
                    "results": [{ "child_task_id": child.task_id.to_string() }]
                }).to_string()
            }
        });
        assert!(
            agent_coordination_recovery_prompt(&root, &task_store, &[completed_wait_record])
                .is_none(),
            "所有代理终态都被 agent_wait 收集后才能允许最终答复"
        );
    }

    #[test]
    fn agent_wait_results_must_be_explicitly_absorbed_before_final_answer() {
        let wait_record = serde_json::json!({
            "type": "tool_call",
            "toolCall": {
                "name": "agent_wait",
                "result": serde_json::json!({
                    "tool": "agent_wait",
                    "status": "succeeded",
                    "timed_out": false,
                    "results": [{
                        "child_task_id": "task-agent-login-review",
                        "status": "succeeded",
                        "child_status": "completed",
                        "role": "reviewer",
                        "assignment": {
                            "title": "登录流程审查代理",
                            "goal": "检查登录流程风险"
                        },
                        "result": {
                            "final_text": "登录流程缺少失败重试提示，需要补充错误态与重试入口。",
                            "truncated": false
                        },
                        "summary": "登录流程缺少失败重试提示"
                    }]
                }).to_string()
            }
        });

        let missing = agent_result_absorption_recovery_prompt(
            "已经完成检查，整体没有明显问题。",
            &[wait_record.clone()],
        )
        .expect("没有吸收代理结果时必须阻止最终答复");
        assert!(missing.contains("登录流程审查代理"));
        assert!(missing.contains("agent_wait"));

        assert!(
            agent_result_absorption_recovery_prompt(
                "根据登录流程审查代理的结果：登录流程缺少失败重试提示，需要补充错误态与重试入口。",
                &[wait_record],
            )
            .is_none(),
            "明确引用代理标题和结论后允许最终答复"
        );
    }

    #[test]
    fn explicit_agent_request_blocks_final_until_agent_spawn_is_attempted() {
        let mut task = make_task_loop_test_task("task-agent-spawn-required-final");
        task.goal = "只读验证：请启动两个只读代理并行工作，一个 explorer 检查目录结构，一个 reviewer 检查配置风险。两个代理都必须完成后再汇总。"
            .to_string();

        let outcome = run_static_task_final(&task, "我直接在主线完成检查。");

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("agent_spawn"));
                assert!(error.contains("代理契约"));
            }
            other => panic!("明确要求代理时不能直接 final，got {other:?}"),
        }
    }

    #[test]
    fn mainline_agent_concept_discussion_does_not_force_agent_spawn() {
        let mut task = make_task_loop_test_task("task-agent-concept-no-spawn");
        task.goal = "请说明主模型和代理的职责边界，以及什么时候应该使用代理。".to_string();

        let outcome = run_static_task_final(&task, "主线可以直接推进关键路径，代理用于并行协作。");

        match outcome {
            TaskOutcome::Completed { .. } => {}
            other => panic!("讨论代理使用方式不应强制 agent_spawn，got {other:?}"),
        }
    }

    #[test]
    fn explicit_agent_request_rejects_mainline_tool_substitution() {
        let task_store = TaskStore::new();
        let mut task = make_task_loop_test_task("task-agent-spawn-required-tools");
        task.goal = "请启动 explorer 代理检查 /tmp，只读执行，代理完成后再汇总。".to_string();
        task_store.insert_task(task.clone());
        let shell_call = ChatToolCall {
            id: "call-shell".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({
                    "command": "ls /tmp",
                    "access_mode": "read_only"
                })
                .to_string(),
            },
        };
        let spawn_call = ChatToolCall {
            id: "call-agent-spawn".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "agent_spawn".to_string(),
                arguments: serde_json::json!({
                    "role": "explorer",
                    "display_name": "目录观察代理",
                    "goal": "检查 /tmp 顶层结构",
                    "access_mode": "read_only"
                })
                .to_string(),
            },
        };

        assert!(
            agent_spawn_requirement_recovery_prompt(
                &task,
                &task_store,
                &[],
                &[shell_call],
                None,
                None
            )
            .is_some(),
            "明确要求代理时，主线工具不能冒充代理契约"
        );
        assert!(
            agent_spawn_requirement_recovery_prompt(
                &task,
                &task_store,
                &[],
                &[spawn_call],
                None,
                None
            )
            .is_none(),
            "同一轮真正发起 agent_spawn 时允许进入工具执行"
        );

        let attempted_spawn_record = serde_json::json!({
            "type": "tool_call",
            "toolCall": {
                "name": "agent_spawn",
                "result": serde_json::json!({
                    "tool": "agent_spawn",
                    "status": "degraded",
                    "fallback_mode": "mainline_or_reassign",
                    "instruction": "改派其他代理或由主线接管"
                }).to_string()
            }
        });
        assert!(
            agent_spawn_requirement_recovery_prompt(
                &task,
                &task_store,
                &[attempted_spawn_record],
                &[],
                None,
                None
            )
            .is_none(),
            "agent_spawn 已尝试但不可用时，后续允许模型按工具结果改派或主线接管"
        );
    }

    #[test]
    fn long_mission_agent_request_requires_governance_before_spawn() {
        let task_store = TaskStore::new();
        let mut task = make_task_loop_test_task("task-long-mission-agent-prerequisite");
        task.goal = "LongMission：先建立治理记录，再启动两个代理并行验证。".to_string();
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "full".to_string(),
            retry_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            task_tier: TaskTier::LongMission,
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        task.executor_binding = Some(serde_json::json!({
            "target_role": "coordinator",
        }));
        task_store.insert_task(task.clone());
        let spawn_call = ChatToolCall {
            id: "call-long-mission-spawn-before-plan".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "agent_spawn".to_string(),
                arguments: serde_json::json!({
                    "role": "explorer",
                    "display_name": "目录调查代理",
                    "goal": "检查目录",
                    "access_mode": "read_only"
                })
                .to_string(),
            },
        };
        let charter_call = ChatToolCall {
            id: "call-long-mission-charter-first".to_string(),
            kind: "function".to_string(),
            function: magi_bridge_client::ChatToolFunction {
                name: "mission_charter_write".to_string(),
                arguments: "{}".to_string(),
            },
        };

        let prompt = agent_spawn_requirement_recovery_prompt(
            &task,
            &task_store,
            &[],
            &[spawn_call],
            None,
            None,
        )
        .expect("LongMission 缺少 charter/plan 时不允许先 agent_spawn");
        assert!(prompt.contains("mission_charter_write"));
        assert!(prompt.contains("plan_write"));
        assert!(
            agent_spawn_requirement_recovery_prompt(
                &task,
                &task_store,
                &[],
                &[charter_call],
                None,
                None
            )
            .is_none(),
            "LongMission 前置治理工具不能被 agent_spawn 硬约束拦截"
        );
    }

    #[test]
    fn ordinary_task_does_not_require_agent_spawn() {
        let task_store = TaskStore::new();
        let mut task = make_task_loop_test_task("task-no-agent-spawn-required");
        task.goal = "检查 /tmp 顶层结构并汇总，不要修改文件。".to_string();
        task_store.insert_task(task.clone());

        assert!(
            agent_spawn_requirement_recovery_prompt(&task, &task_store, &[], &[], None, None)
                .is_none(),
            "普通任务不应被代理硬约束误伤"
        );
    }

    #[test]
    fn action_task_failed_tool_prevents_completed_final() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-failed-tool-final");
        let workspace_id = Some(WorkspaceId::new("workspace-task-failed-tool-final"));
        let task_store = TaskStore::new();
        let task = make_task_loop_test_task("task-failed-tool-final");
        task_store.insert_task(task.clone());
        let worker_id = WorkerId::new("worker-task-failed-tool-final");
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "executor",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let client = TaskToolFailureThenFinalModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };
        session_store
            .create_session(session_id.clone(), "task failed tool fixture")
            .expect("session should be creatable");
        let now = UtcMillis::now();
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || task.mission_id.clone());
        let thread_id = orchestrator_thread_id.clone();

        let (outcome, _) = run_conversation_loop(ConversationLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            execution_registry: &TaskExecutionRegistry::default(),
            conversation_registry: &ConversationRegistry::new(),
            agent_role_registry: &magi_agent_role::AgentRoleRegistry::load_default(),
            spawn_graph: &std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new()),
            safety_gate: None,
            todo_ledger: &magi_todo_ledger::TodoLedger::new(),
            project_memory: None,
            mission_charter: None,
            plan: None,
            mission_workspace: None,
            knowledge_graph: None,
            validation_runner: None,
            checkpoint: None,
            human_checkpoint: None,
            mission_metrics: None,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请调用一个失败工具后总结".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            is_sidechain: false,
            worker_id: Some(&worker_id),
            thread_id: &thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("工具执行失败"));
                assert!(error.contains("missing_builtin_tool"));
            }
            other => panic!("failed tool must fail action task, got {other:?}"),
        }
    }

    #[test]
    fn action_task_tool_failure_can_be_recovered_by_later_success() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-recovered-tool-final");
        let workspace_id = Some(WorkspaceId::new("workspace-task-recovered-tool-final"));
        let task_store = TaskStore::new();
        let task = make_task_loop_test_task("task-recovered-tool-final");
        task_store.insert_task(task.clone());
        let worker_id = WorkerId::new("worker-task-recovered-tool-final");
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "executor",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let client = RecoverableTaskToolFailureModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };
        let tool_event_bus = Arc::new(InMemoryEventBus::new(8));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::clone(&tool_event_bus),
        );
        tool_registry.register_builtin(Arc::new(RecoverableProbeTool {
            attempts: AtomicUsize::new(0),
        }));
        session_store
            .create_session(session_id.clone(), "task recovered tool fixture")
            .expect("session should be creatable");
        let now = UtcMillis::now();
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || task.mission_id.clone());
        let thread_id = orchestrator_thread_id.clone();

        let (outcome, _) = run_conversation_loop(ConversationLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: Some(&tool_registry),
            skill_runtime: None,
            task_store: &task_store,
            execution_registry: &TaskExecutionRegistry::default(),
            conversation_registry: &ConversationRegistry::new(),
            agent_role_registry: &magi_agent_role::AgentRoleRegistry::load_default(),
            spawn_graph: &std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new()),
            safety_gate: None,
            todo_ledger: &magi_todo_ledger::TodoLedger::new(),
            project_memory: None,
            mission_charter: None,
            plan: None,
            mission_workspace: None,
            knowledge_graph: None,
            validation_runner: None,
            checkpoint: None,
            human_checkpoint: None,
            mission_metrics: None,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请先处理失败工具，再通过重试完成任务".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            is_sidechain: false,
            worker_id: Some(&worker_id),
            thread_id: &thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        match outcome {
            TaskOutcome::Completed { output_refs } => {
                assert!(
                    output_refs
                        .first()
                        .is_some_and(|content| content.contains("重试恢复"))
                );
            }
            other => panic!("recovered tool failure should complete action task, got {other:?}"),
        }
    }

    #[test]
    fn task_stream_item_id_reuses_main_timeline_streaming_entry_only_for_first_round() {
        let task_id = TaskId::new("task-stream-main");

        assert_eq!(
            task_stream_item_id(&task_id, 0, Some("timeline-streaming-task-stream-main")),
            "timeline-streaming-task-stream-main"
        );
        assert_eq!(
            task_stream_item_id(&task_id, 3, Some("timeline-streaming-task-stream-main")),
            "turn-item-assistant-stream-task-stream-main-3"
        );
    }

    #[test]
    fn task_stream_item_id_keeps_round_scope_without_main_streaming_entry() {
        let task_id = TaskId::new("task-stream-worker");

        assert_eq!(
            task_stream_item_id(&task_id, 2, None),
            "turn-item-assistant-stream-task-stream-worker-2"
        );
    }

    #[test]
    fn task_turn_visibility_keeps_primary_role_on_mainline_without_sidechain_worker() {
        let task = make_task_loop_test_task("task-primary-role-only");
        let thread_id = ThreadId::new("thread-primary-role-only");

        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility = task_turn_visibility(&task, false, None, &thread_id, &registry);

        // 没有 is_sidechain=true + worker_id 配对 → 必须落在 Mainline 分支。
        assert!(visibility.is_mainline());
        assert_eq!(visibility.thread_id(), &thread_id);
        assert!(visibility.worker_id().is_none());
    }

    #[test]
    fn primary_task_worker_details_move_to_sidechain() {
        let task = make_task_loop_test_task("task-primary-deep-sidechain");
        let worker_id = WorkerId::new("worker-primary-deep-sidechain");
        let worker_thread_id = ThreadId::new("thread-worker-primary-deep-sidechain");
        let orchestrator_thread_id = ThreadId::new("thread-orch-primary-deep-sidechain");
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility =
            task_turn_visibility(&task, true, Some(&worker_id), &worker_thread_id, &registry);
        let mut tool_item = session_turn_item(
            "tool_call_started",
            "running",
            Some("shell_exec".to_string()),
            Some("正在调用工具：shell_exec".to_string()),
            Some("turn-item-primary-tool".to_string()),
            orchestrator_thread_id.clone(),
        );

        apply_task_worker_detail_visibility(&mut tool_item, &task, &visibility);

        // sidechain item 的 source_thread_id 必须切换到 worker thread。
        assert_eq!(tool_item.source_thread_id, worker_thread_id);
        assert_ne!(tool_item.source_thread_id, orchestrator_thread_id);
        assert_eq!(tool_item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(tool_item.role_id.as_deref(), Some("executor"));
        assert_eq!(tool_item.source, "executor");

        let mut final_item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("worker 输出".to_string()),
            Some("turn-item-primary-final".to_string()),
            orchestrator_thread_id.clone(),
        );
        let task_store = TaskStore::new();
        task_store.insert_task(task.clone());
        apply_task_final_visibility(&mut final_item, &task_store, &task, &visibility);

        assert_eq!(final_item.source_thread_id, worker_thread_id);
        assert_ne!(final_item.source_thread_id, orchestrator_thread_id);
        assert_eq!(final_item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(final_item.role_id.as_deref(), Some("executor"));
        assert_eq!(final_item.source, "executor");
    }

    #[test]
    fn task_turn_visibility_routes_sidechain_to_worker_thread() {
        let task = make_task_loop_test_task("task-worker-lane-order");
        let worker_id = WorkerId::new("worker-worker-lane-order");
        let worker_thread_id = ThreadId::new("thread-worker-worker-lane-order");
        let orchestrator_thread_id = ThreadId::new("thread-orch-worker-lane-order");
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility =
            task_turn_visibility(&task, true, Some(&worker_id), &worker_thread_id, &registry);
        let mut item = session_turn_item(
            "assistant_final",
            "completed",
            Some("最终回复".to_string()),
            Some("worker 输出".to_string()),
            Some("turn-item-worker-final".to_string()),
            orchestrator_thread_id.clone(),
        );

        apply_task_turn_visibility(&mut item, &task, &visibility);

        assert_eq!(item.source_thread_id, worker_thread_id);
        assert_ne!(item.source_thread_id, orchestrator_thread_id);
        assert_eq!(item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(item.role_id.as_deref(), Some("executor"));
    }

    #[test]
    fn task_final_turn_item_does_not_complete_turn_before_root_task_completes() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(16);
        let session_id = SessionId::new("session-task-final-root-running");
        session_store
            .create_session(session_id.clone(), "task final root running")
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-final-root-running".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("执行深度任务".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("turn should be stored");

        let task_store = TaskStore::new();
        let root_task_id = TaskId::new("task-root-final-root-running");
        let task_id = TaskId::new("task-action-final-root-running");
        let mut root_task = make_task_loop_test_task(root_task_id.as_str());
        root_task.kind = TaskKind::LocalAgent;
        root_task.status = TaskStatus::Running;
        task_store.insert_task(root_task);
        let mut task = make_task_loop_test_task(task_id.as_str());
        task.root_task_id = root_task_id;
        task.status = TaskStatus::Completed;
        task_store.insert_task(task.clone());
        // 该用例验证"root 未完成时不能提前收尾主线 turn"，因此 task 本身走 Mainline 路径：
        // 传 is_sidechain=false，`task_turn_visibility` 会返回 Mainline，
        // 后续 append_task_final_turn_item 的 `is_mainline()` 分支才会被覆盖到。
        let orchestrator_thread_id = ThreadId::new("thread-orch-final-root-running");
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility =
            task_turn_visibility(&task, false, None, &orchestrator_thread_id, &registry);

        append_task_final_turn_item(
            &event_bus,
            &session_store,
            &task_store,
            &task,
            &session_id,
            &None,
            "primary action 已完成",
            Some("timeline-streaming-task-action-final-root-running"),
            Some("timeline-streaming-task-action-final-root-running"),
            &visibility,
        );

        let current_turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("current turn should remain");
        assert_eq!(current_turn.status, "running");
        assert!(current_turn.completed_at.is_none());
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| !matches!(entry.kind, TimelineEntryKind::AssistantMessage)),
            "root 未完成时不能写入 completed turn snapshot"
        );
    }

    #[test]
    fn conversation_loop_model_failure_writes_failed_turn_item_and_canonical_turn() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-model-failure");
        let workspace_id = Some(WorkspaceId::new("workspace-task-model-failure"));
        let task_id = TaskId::new("task-model-failure");
        let worker_id = WorkerId::new("worker-task-model-failure");
        let streaming_entry_id = "timeline-streaming-task-model-failure";
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task model failure session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-model-failure".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("验证模型失败写回".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let task_store = TaskStore::new();
        let task = make_task_loop_test_task(task_id.as_str());
        task_store.insert_task(task.clone());
        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "executor",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let now = UtcMillis::now();
        let (_, orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || task.mission_id.clone());
        let thread_id = orchestrator_thread_id.clone();

        let (outcome, _) = run_conversation_loop(ConversationLoopRequest {
            client: &FailingTaskModelBridgeClient,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            execution_registry: &TaskExecutionRegistry::default(),
            conversation_registry: &ConversationRegistry::new(),
            agent_role_registry: &magi_agent_role::AgentRoleRegistry::load_default(),
            spawn_graph: &std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new()),
            safety_gate: None,
            todo_ledger: &magi_todo_ledger::TodoLedger::new(),
            project_memory: None,
            mission_charter: None,
            plan: None,
            mission_workspace: None,
            knowledge_graph: None,
            validation_runner: None,
            checkpoint: None,
            human_checkpoint: None,
            mission_metrics: None,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请生成回复".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some(streaming_entry_id),
            is_sidechain: false,
            worker_id: None,
            thread_id: &thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("桥接调用失败[RemoteBusiness]"));
                assert!(error.contains("model bridge unavailable"));
                assert!(!error.contains("LLM invocation failed"));
            }
            other => panic!("model failure must fail the task loop, got {other:?}"),
        }

        let turn = session_store
            .runtime_sidecar(&session_id)
            .and_then(|sidecar| sidecar.current_turn)
            .expect("failed turn should remain inspectable");
        assert_eq!(turn.status, "failed");
        assert!(turn.completed_at.is_some());
        let error_item = turn
            .items
            .iter()
            .find(|item| item.kind == "assistant_error")
            .expect("assistant_error should be appended");
        // Mainline 失败 item 的 source_thread_id 必须等于 orchestrator thread。
        assert_eq!(error_item.source_thread_id, orchestrator_thread_id);
        assert_eq!(error_item.status, "failed");
        assert_eq!(error_item.task_id.as_ref(), Some(&task_id));
        assert!(error_item.content.as_deref().is_some_and(|content| {
            content.contains("桥接调用失败[RemoteBusiness]")
                && content.contains("model bridge unavailable")
                && !content.contains("LLM invocation failed")
        }));

        let canonical_turn = session_store
            .canonical_turns_for_session(&session_id)
            .into_iter()
            .find(|turn| turn.turn_id == "turn-task-model-failure")
            .expect("failed canonical turn should be stored");
        assert_eq!(canonical_turn.status, CanonicalTurnStatus::Failed);
        assert!(canonical_turn.response_duration_ms.is_some());
        assert!(
            canonical_turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::AssistantText
                    && item.status == CanonicalTurnItemStatus::Failed
                    && item
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("model bridge unavailable"))
            }),
            "failed task loop must persist the visible failure as canonical assistant_text"
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != streaming_entry_id),
            "失败终态不能写回 completed snapshot"
        );

        let terminal_error_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == "session.turn.item"
                    && event.payload["item"]["kind"] == "assistant_error"
            })
            .expect("assistant_error item event should be published");
        assert_eq!(
            terminal_error_event.payload["current_turn"]["status"],
            serde_json::Value::String("failed".to_string())
        );
        assert!(
            terminal_error_event.payload["current_turn"]["response_duration_ms"].is_number(),
            "terminal error event must carry backend duration for live UI"
        );
    }

    #[test]
    fn conversation_loop_read_only_shell_tools_execute_concurrently_and_preserve_order() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-tool-batch");
        let workspace_id = Some(WorkspaceId::new("workspace-task-tool-batch"));
        let task_id = TaskId::new("task-tool-batch");
        let worker_id = WorkerId::new("worker-task-tool-batch");
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "task tool batch session",
                workspace_id.as_ref().map(ToString::to_string),
            )
            .expect("session should be creatable");
        let task_store = TaskStore::new();
        let task = make_task_loop_test_task(task_id.as_str());
        task_store.insert_task(task.clone());
        let now = UtcMillis::now();
        let (_, _orchestrator_thread_id) =
            session_store.ensure_session_mission(&session_id, now, || task.mission_id.clone());
        // 子任务必须绑定到本 task 独占的执行 thread；历史 thread 只做审计，不能复用为新的执行上下文。
        let worker_thread_id = {
            let role_id = "executor";
            let new_thread = ExecutionThread {
                thread_id: ThreadId::new(format!(
                    "thread-{role_id}-{}-{}",
                    task_id.as_str(),
                    now.0
                )),
                session_id: session_id.clone(),
                mission_id: task.mission_id.clone(),
                role_id: role_id.to_string(),
                worker_instance_id: worker_id.clone(),
                status: ExecutionThreadStatus::Active,
                created_at: now,
                last_used_at: now,
                handled_task_ids: vec![task_id.clone()],
                message_history: Vec::new(),
            };
            let thread_id = new_thread.thread_id.clone();
            session_store.register_thread(new_thread);
            thread_id
        };
        session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-task-tool-batch".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis::now(),
                    status: "running".to_string(),
                    user_message: Some("验证 worker 工具并发".to_string()),
                    items: Vec::new(),
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "executor",
                60_000,
            )
            .expect("lease should be granted");

        let probe = Arc::new(ConcurrentTaskToolProbe::new(Duration::from_millis(180)));
        let tool_event_bus = Arc::new(InMemoryEventBus::new(8));
        let mut tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::clone(&tool_event_bus),
        );
        tool_registry.register_builtin(Arc::new(ProbeTaskBuiltinTool::new(
            "shell_exec",
            Arc::clone(&probe),
        )));
        let client = TaskToolBatchModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);

        let (outcome, _) = run_conversation_loop(ConversationLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: Some(&tool_registry),
            skill_runtime: None,
            task_store: &task_store,
            execution_registry: &TaskExecutionRegistry::default(),
            conversation_registry: &ConversationRegistry::new(),
            agent_role_registry: &magi_agent_role::AgentRoleRegistry::load_default(),
            spawn_graph: &std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new()),
            safety_gate: None,
            todo_ledger: &magi_todo_ledger::TodoLedger::new(),
            project_memory: None,
            mission_charter: None,
            plan: None,
            mission_workspace: None,
            knowledge_graph: None,
            validation_runner: None,
            checkpoint: None,
            human_checkpoint: None,
            mission_metrics: None,
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请执行两个只读 shell 工具".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some("timeline-streaming-task-tool-batch"),
            is_sidechain: true,
            worker_id: Some(&worker_id),
            thread_id: &worker_thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        assert!(
            probe.max_active() > 1,
            "task worker 中的多个只读 shell 工具调用必须并发执行"
        );
        let output_refs = match outcome {
            TaskOutcome::Completed { output_refs } => output_refs,
            other => panic!("task loop should complete, got {other:?}"),
        };
        let output: serde_json::Value =
            serde_json::from_str(&output_refs[0]).expect("output blocks json");
        assert_eq!(
            output["blocks"][0]["toolCall"]["id"],
            serde_json::Value::String("task-tool-shell-a".to_string())
        );
        assert_eq!(
            output["blocks"][1]["toolCall"]["id"],
            serde_json::Value::String("task-tool-shell-b".to_string())
        );

        let sidecar = session_store
            .runtime_sidecar(&session_id)
            .expect("sidecar should exist");
        let turn = sidecar.current_turn.expect("turn should exist");
        assert!(
            turn.items.iter().all(|item| {
                // Sidechain item 的 source_thread_id 必须切换到 worker thread。
                item.source_thread_id == worker_thread_id
            }),
            "worker 输出必须沿用执行计划中的 sidechain 归属"
        );
        assert_eq!(
            turn.items
                .iter()
                .map(|item| (item.kind.as_str(), item.tool_call_id.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("tool_call_result", Some("task-tool-shell-a")),
                ("tool_call_result", Some("task-tool-shell-b")),
                ("assistant_final", None),
            ]
        );
        assert!(
            session_store
                .timeline_for_session(&session_id)
                .iter()
                .all(|entry| entry.entry_id != "turn-item-assistant-stream-task-tool-batch-1"),
            "工具后的第二轮流式内容不能写成独立主线 timeline entry"
        );
        let tool_events = event_bus.snapshot().recent_events;
        let invoked_events = tool_events
            .iter()
            .filter(|event| event.event_type == "task.tool.invoked")
            .collect::<Vec<_>>();
        assert_eq!(invoked_events.len(), 2);
        assert!(
            invoked_events.iter().all(|event| event.payload["worker_id"]
                == serde_json::Value::String(worker_id.to_string())),
            "worker 工具事件必须携带执行 worker，供代理详情和 runtime 归属使用"
        );
        let runtime_tool_events = tool_event_bus.snapshot().recent_events;
        assert!(
            runtime_tool_events
                .iter()
                .filter(|event| {
                    event.event_type == "tool.invoked" || event.event_type == "tool.usage.recorded"
                })
                .all(|event| event.payload["worker_id"]
                    == serde_json::Value::String(worker_id.to_string())),
            "工具运行时事件也必须沿用同一个 worker 归属"
        );
    }
}
