use crate::{
    builtin_tool_schema::internal_builtin_tool_rejection_payload,
    prompt_utils::{
        normalize_model_stream_preview_content, normalize_model_visible_content,
        workspace_context_system_prompt,
    },
    session_turn_writeback::{
        append_session_turn_item_with_task_store, publish_current_session_turn_item_event,
        publish_session_turn_item_event, session_turn_item,
        upsert_session_turn_item_with_task_store,
    },
    settings_store::SettingsStore,
    skill_apply_tool::{SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime},
    tool_result_utils::{
        infer_tool_call_status, summarize_tool_result, tool_execution_status_label,
        turn_item_status_for_tool_result,
    },
    usage_recording::{ModelUsageBinding, publish_model_usage_record},
};
use magi_bridge_client::{
    ChatMessage, ChatToolCall, ChatToolChoice, ChatToolDefinition, ModelBridgeClient,
    ModelInvocationRequest, ModelStreamingDelta, LOOPBACK_MODEL_PROVIDER,
    tool_concurrency::{ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs},
};
use magi_conversation_runtime::{
    ConversationRegistry, RoundOutcome, StreamEvent, StreamFanOut, TurnDriver,
};
use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, LeaseId, RiskLevel, SessionId, TaskId,
    TaskKind, TaskStatus, ThreadId, ToolCallId, UtcMillis, WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::ToolKind;
use magi_orchestrator::{
    ExecutionContextSummary, task_runner::TaskOutcome, task_store::TaskStore,
    task_worker_catalog::resolve_task_role,
};
use magi_session_store::{
    ActiveExecutionTurnItem, SessionStore, ThreadChatMessage, ThreadChatToolCall,
    ThreadChatToolFunction, TimelineEntryKind,
};
use magi_tool_runtime::{
    BuiltinToolName, ToolExecutionContext, ToolExecutionInput, ToolExecutionPolicy, ToolRegistry,
};
use magi_usage_authority::UsageCallStatus;
use std::{path::PathBuf, sync::Arc, thread};

const BASE_TOOL_CALL_ROUNDS: usize = 16;
const MAX_TOOL_CALL_ROUNDS: usize = 32;

pub(crate) struct TaskLlmLoopRequest<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub task_store: &'a TaskStore,
    /// Task System v2：Turn 状态机驱动。每次 LLM 调用都通过 advance_turn 驱动，
    /// 显式经过 Pending → Modeling → Done/Failed 不变式（同一 Conversation 不并发）。
    pub conversation_registry: &'a ConversationRegistry,
    /// Task System v2 — 统一流派生通道。模型 token / 工具事件 / 系统信号在这里
    /// 扇出给下游订阅者（writeback / projection / 未来 UI bridge）。
    pub stream_fanout: &'a StreamFanOut,
    /// Task System v2 — AgentRole 注册表。task_turn_visibility 解析 role_id 时
    /// 必须走该注册表，不再依赖硬编码的 kind→role 默认 mapping。
    pub agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    /// Task System v2 — L5：父子任务拓扑图。S7 协调工具（agent_spawn / task_stop）
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
    /// Task System v2 — Tier 4 / L11：当前 workspace 的 MissionCharter 索引。`None` 表示
    /// 当前 task 不绑定 workspace（极少数 orchestration-only 场景），此时不注入 prompt、
    /// 也不允许 `mission_charter_write` 工具调用成功。
    pub mission_charter: Option<&'a magi_mission_charter::MissionCharterStore>,
    /// Task System v2 — Tier 4 / L12：当前 workspace 的 Plan 索引。`None` 表示当前 task
    /// 不绑定 workspace；此时不注入 prompt，也不允许 `plan_write` 工具调用成功。
    pub plan: Option<&'a magi_plan::PlanStore>,
    /// Task System v2 — Tier 4 / L13：当前 workspace 的 MissionWorkspace 索引。`None`
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
    /// 表示当前 task 不绑定 workspace；此时不注入人工审核点摘要，也不允许
    /// `human_checkpoint_request` 工具落盘。pending 状态会强制 Coordinator 停止派发新工作。
    pub human_checkpoint: Option<&'a magi_human_checkpoint::HumanCheckpointStore>,
    pub task: &'a magi_core::Task,
    pub task_id: &'a TaskId,
    pub lease_id: &'a LeaseId,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub prompt: String,
    pub tools: Option<Vec<ChatToolDefinition>>,
    pub usage_binding: &'a ModelUsageBinding,
    pub streaming_entry_id: Option<&'a str>,
    pub worker_lane_id: Option<&'a str>,
    pub worker_lane_seq: Option<usize>,
    pub worker_id: Option<&'a magi_core::WorkerId>,
    /// P7：lane 必须绑定到 thread。LLM 入口会 prepend 该 thread 的历史、
    /// 结束时把本轮消息 append 回 thread。orchestrator task 走 session 的
    /// orchestrator thread；worker task 走对应 role 的 worker thread。
    pub thread_id: &'a ThreadId,
    /// P7：本 session 的 orchestrator thread。Sidechain task 在主线 publish
    /// `worker_status` 摘要 item 时把 source_thread_id 指向它，确保前端 projection
    /// 把摘要归到主线时间线。当 task 自身就是 orchestrator task 时，与
    /// `thread_id` 相等。
    pub orchestrator_thread_id: &'a ThreadId,
    pub context_summary: Option<ExecutionContextSummary>,
    pub system_prompt: Option<String>,
    pub workspace_root_path: Option<PathBuf>,
}

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
enum TaskTurnVisibility {
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
        worker_id: magi_core::WorkerId,
        lane_id: String,
        lane_seq: Option<usize>,
        /// 当前 sidechain 是否同时为主线 dispatch 卡的 primary worker：是则需要
        /// 在主线 publish 摘要 item；否则 drawer 里安静执行不污染主线。
        has_mainline_summary: bool,
    },
}

impl TaskTurnVisibility {
    fn thread_id(&self) -> &ThreadId {
        match self {
            Self::Mainline { thread_id } => thread_id,
            Self::Sidechain { thread_id, .. } => thread_id,
        }
    }

    fn is_mainline(&self) -> bool {
        matches!(self, Self::Mainline { .. })
    }

    /// worker 执行下发工具调用时需要传入 worker_id（影响 executor 分派）。
    /// Mainline task 不绑定 worker。
    fn worker_id(&self) -> Option<&magi_core::WorkerId> {
        match self {
            Self::Mainline { .. } => None,
            Self::Sidechain { worker_id, .. } => Some(worker_id),
        }
    }
}

fn task_turn_visibility(
    task: &magi_core::Task,
    worker_lane_id: Option<&str>,
    worker_lane_seq: Option<usize>,
    worker_id: Option<&magi_core::WorkerId>,
    thread_id: &ThreadId,
    orchestrator_thread_id: &ThreadId,
    primary_worker_sidechain: bool,
    agent_role_registry: &magi_agent_role::AgentRoleRegistry,
) -> TaskTurnVisibility {
    let lane_id = worker_lane_id
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .map(ToOwned::to_owned);
    // 仅当存在 lane + worker_id 时该 task 才属于 worker drawer；否则保留 Mainline。
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

fn apply_task_turn_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &magi_core::Task,
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
fn apply_task_worker_detail_visibility(
    item: &mut ActiveExecutionTurnItem,
    task: &magi_core::Task,
    visibility: &TaskTurnVisibility,
) {
    apply_task_turn_visibility(item, task, visibility);
}

/// final 回复的归属规则与执行细节一致：sidechain task 的 final 永远只归 worker drawer，
/// 主线消费的是 `worker_status` 摘要 item（由 publish_worker_lane_summary 写入）。
fn apply_task_final_visibility(
    item: &mut ActiveExecutionTurnItem,
    task_store: &TaskStore,
    task: &magi_core::Task,
    visibility: &TaskTurnVisibility,
) {
    apply_task_turn_visibility(item, task, visibility);
    let _ = task_store;
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
/// 压缩为 thread 持久化格式。系统边界提示 / 历史回放 / 工作区提示等重复上下文
/// 不再次写入 —— 它们在下一 task 时会由 run_task_llm_loop 自动重新构造。
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

pub(crate) fn run_task_llm_loop(
    request: TaskLlmLoopRequest<'_>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    // Task System v2 切入：经由 ConversationRegistry 拿到本 session 的 Conversation，
    // 用 advance_turn 驱动 Turn 状态机；模型 IO + 工具 IO 段折叠到 driver 内部一次性执行。
    let registry = request.conversation_registry;
    let conv_handle = registry.conversation_for(request.session_id);
    let driver = TaskLlmTurnDriver::new(request);
    let mut conversation = conv_handle
        .lock()
        .expect("Conversation mutex poisoned in task_llm_loop");
    match conversation.advance_turn(driver) {
        Ok(outcome) => outcome,
        Err(err) => {
            tracing::error!(?err, "task_llm_loop advance_turn 失败");
            (
                TaskOutcome::Failed {
                    error: format!("Conversation::advance_turn 失败: {err}"),
                },
                None,
            )
        }
    }
}

/// Task System v2 — 把 v1 一次完整的 LLM IO + 工具 IO 段折叠成一个 round 的 driver。
///
/// 当前 slice S2 范围内 driver 的 round_limit = 1：driver 内部仍保留 v1 多轮工具调用
/// for 循环（围绕 `messages` 累积器）。下一 slice（S3 StreamFanOut）把模型 / 工具 / 系统
/// 三路 callback 切到统一通道时，会自然把每个 LLM 调用拆成独立 round，driver 就能进入
/// 真正的"每 round 一次模型调用 + 工具批"形态。Conversation::advance_turn 已经提供了
/// 多 round 骨架，driver 只要后续拆 execute_round 即可。
struct TaskLlmTurnDriver<'a> {
    request: Option<TaskLlmLoopRequest<'a>>,
    /// execute_round 跑完后把 outcome 存到这里，finalize_success 再交付出去。
    captured: Option<(TaskOutcome, Option<ExecutionContextSummary>)>,
}

impl<'a> TaskLlmTurnDriver<'a> {
    fn new(request: TaskLlmLoopRequest<'a>) -> Self {
        Self {
            request: Some(request),
            captured: None,
        }
    }
}

impl<'a> TurnDriver for TaskLlmTurnDriver<'a> {
    type Outcome = (TaskOutcome, Option<ExecutionContextSummary>);

    fn round_limit(&self) -> usize {
        1
    }

    fn execute_round(&mut self, _round: usize) -> RoundOutcome {
        let request = self
            .request
            .take()
            .expect("TaskLlmTurnDriver::execute_round 重入");
        let outcome = run_task_llm_loop_inner(request);
        let is_failure = matches!(outcome.0, TaskOutcome::Failed { .. });
        self.captured = Some(outcome);
        if is_failure {
            // Turn 状态机记账：失败也通过 finalize_round_failure 路径出。
            RoundOutcome::Failed("task_llm_loop_inner returned Failed".to_string())
        } else {
            RoundOutcome::Done
        }
    }

    fn finalize_success(self) -> Self::Outcome {
        self.captured
            .expect("TaskLlmTurnDriver::finalize_success 没有捕获到 outcome")
    }

    fn finalize_round_failure(self, _reason: String) -> Self::Outcome {
        self.captured
            .expect("TaskLlmTurnDriver::finalize_round_failure 没有捕获到 outcome")
    }

    fn finalize_exhausted(self) -> Self::Outcome {
        // round_limit = 1 时不会触发 exhausted，但保留兜底返回。
        (
            TaskOutcome::Failed {
                error: "task_llm_loop driver 在 round_limit 内未产出 outcome".to_string(),
            },
            None,
        )
    }
}

/// v1 一轮 LLM IO + 工具 IO 全段——driver 内部唯一调用点。
/// S2 阶段保持单调用入口，下一 slice 在此基础上拆 per-round 边界。
fn run_task_llm_loop_inner(
    request: TaskLlmLoopRequest<'_>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    let TaskLlmLoopRequest {
        client,
        event_bus,
        session_store,
        settings_store,
        tool_registry,
        skill_runtime,
        task_store,
        conversation_registry: _,
        stream_fanout,
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
        orchestrator_thread_id,
        context_summary,
        system_prompt,
        workspace_root_path,
    } = request;

    let mut messages = Vec::new();
    if let Some(system) = system_prompt {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
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
    // S9：TodoLedger 快照注入。本 session 模型在之前轮次写过 todo_write 时，
    // 这里把当前列表渲染进 system prompt，让本轮 Turn 起点自动看到分解 + 进度。
    if let Some(rendered) = todo_ledger.render_for_prompt() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(rendered),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    // S10：ProjectMemory 索引注入。把 `~/.magi/projects/{slug}/memory/MEMORY.md`
    // 视图渲染进 system prompt，跨 conversation 复用同一项目的长期记忆。
    if let Some(store) = project_memory {
        match store.render_for_prompt() {
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
    // S11：MissionCharter 注入。当前 mission 的"宪章"（goal / 成功标准 / 约束）作为长效
    // 锚点，长对话或多 Turn 时让 orchestrator 不会偏离最初承诺。
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
    // S12：Plan 注入。当前 mission 的执行计划（steps + 状态 + 依赖）让 orchestrator
    // 在多 Turn 推进时持续看到"下一步是什么、上一步是否做完"，避免漂移。
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
    // S13：Mission Workspace 注入。告知 agent 当前 mission 独占的 artifacts/logs/memory
    // 目录，引导其把产物落在 mission 内，避免散落到用户主目录或随机临时目录。
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
    // S14：KnowledgeGraph 注入。把 mission 已经累积的 symbols / decisions / risks 摊在
    // 系统提示里，避免长 mission 跨多个 Conversation 时模型重新讨论已经达成的结论。
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
    // S15：ValidationRunner 注入。把 mission 当前的 Plan 节点验证结果（test_suite /
    // type_check / integration_smoke / benchmark 的 pass/fail/skipped）摊在系统提示里，
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
    // S16：Checkpoint 注入。把当前 mission 最近若干检查点摊在系统提示里，让模型在跨进程
    // 重启 / context 压缩 / phase 切换之后能定位"上次落到哪一步"，决定是否需要从某个
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
    // S17：HumanCheckpoint 注入。把当前 mission 待解决的人工审核点与最近若干已解决项摊
    // 在系统提示里。pending 项要求 Coordinator 停止派发新工作，直到 operator 给出
    // approve / reject；resolved 项作为审计上下文供模型回顾。
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
    // P6b：thread 累积历史 —— 系统提示后、本轮 user prompt 前。为防止 LLM 混淆"当前 task"
    // 与历史 task，我们在历史末尾、新 user prompt 前插入一条 system 边界标记，让模型明确
    // 接下来要专注于新的任务目标。
    let thread_history_snapshot: Vec<ThreadChatMessage> =
        session_store.thread_message_history(thread_id);
    if !thread_history_snapshot.is_empty() {
        for history_msg in &thread_history_snapshot {
            messages.push(thread_chat_message_to_chat_message(history_msg));
        }
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(
                "以上是你在本 mission 中处理过的历史任务对话，仅供上下文参考。下面是一个新的任务，请聚焦新任务目标独立执行。"
                    .to_string(),
            ),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
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
    let mut failed_tool_summaries: Vec<String> = Vec::new();
    let required_tool_chain = task_required_tool_chain(task);
    let mut completed_required_tool_names: Vec<String> = Vec::new();
    let mut last_stream_item_id: Option<String> = None;
    let mut had_tool_calls = false;
    let primary_worker_sidechain =
        worker_id.is_some() && current_turn_has_worker_lanes(session_store, session_id);
    let turn_visibility = task_turn_visibility(
        task,
        worker_lane_id,
        worker_lane_seq,
        worker_id,
        thread_id,
        orchestrator_thread_id,
        primary_worker_sidechain,
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
        let last_thinking_len = std::cell::Cell::new(0usize);
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
                    &delta.content,
                );
                // Task System v2 — 把模型 token delta 同步扇出到 StreamFanOut。
                // 现存 publish_* 仍负责 session_store 写回与 event_bus 派发；fanout 是
                // 增量观察通道，UI bridge / projection 后续 slice 切到此处订阅时，
                // 上面两个 publish_* 内的 event_bus 分支会随之删除。
                if !delta.content.is_empty() || !delta.thinking.is_empty() {
                    stream_fanout.publish(StreamEvent::ModelDelta {
                        session_id: session_id.clone(),
                        content: delta.content.clone(),
                        thinking: delta.thinking.clone(),
                    });
                }
            };

            match client.invoke_streaming(invocation_request, &on_delta) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
                    stream_fanout.publish(StreamEvent::SystemSignal {
                        session_id: session_id.clone(),
                        code: "llm.invocation_failed".to_string(),
                        detail: Some(format!("round {round}: {error:?}")),
                    });
                    if task_lease_is_current(task_store, task_id, lease_id) {
                        append_task_error_turn_item(
                            event_bus,
                            session_store,
                            task_store,
                            task,
                            session_id,
                            workspace_id,
                            &turn_visibility,
                            &format!("LLM invocation failed (round {round}): {error:?}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: format!("LLM invocation failed (round {round}): {error:?}"),
                        },
                        context_summary,
                    );
                }
            }
        } else {
            match client.invoke(invocation_request) {
                Ok(response) => response,
                Err(error) => {
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
                            &format!("LLM invocation failed (round {round}): {error:?}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: format!("LLM invocation failed (round {round}): {error:?}"),
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
            break;
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
            publish_worker_lane_summary(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                "running",
                &format!("正在调用 {}", tool_call.function.name),
            );
            stream_fanout.publish(StreamEvent::ToolEvent {
                session_id: session_id.clone(),
                tool_call_id: ToolCallId::new(&tool_call.id),
                phase: magi_conversation_runtime::ToolPhase::Started,
                payload: tool_call.function.name.clone(),
            });
        }

        let tool_results = execute_task_tool_call_batch(
            event_bus,
            tool_registry,
            skill_runtime,
            task_store,
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
            let outcome_label = match tool_status {
                ExecutionResultStatus::Succeeded => "完成",
                ExecutionResultStatus::Failed => "失败",
                ExecutionResultStatus::Rejected => "已拒绝",
                ExecutionResultStatus::NeedsApproval => "待审批",
                ExecutionResultStatus::Cancelled => "已取消",
            };
            publish_worker_lane_summary(
                event_bus,
                session_store,
                task_store,
                task,
                session_id,
                workspace_id,
                &turn_visibility,
                if matches!(tool_status, ExecutionResultStatus::Succeeded) {
                    "running"
                } else {
                    "blocked"
                },
                &format!("{} {}", tool_call.function.name, outcome_label),
            );
            let phase = if matches!(tool_status, ExecutionResultStatus::Succeeded) {
                magi_conversation_runtime::ToolPhase::Succeeded
            } else {
                magi_conversation_runtime::ToolPhase::Failed
            };
            stream_fanout.publish(StreamEvent::ToolEvent {
                session_id: session_id.clone(),
                tool_call_id: ToolCallId::new(&tool_call.id),
                phase,
                payload: summarize_tool_result(&result),
            });
            if !matches!(tool_status, ExecutionResultStatus::Succeeded) {
                failed_tool_summaries.push(format!(
                    "{}: {}",
                    tool_call.function.name,
                    summarize_tool_result(&result)
                ));
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
            &parsed
                .tool_calls
                .iter()
                .map(|tool_call| canonical_tool_call_name(&tool_call.function.name))
                .collect::<Vec<_>>(),
        );
    }

    if final_content.trim().is_empty() {
        let failure_reason = if had_tool_calls {
            "模型在工具调用后未返回最终回复"
        } else {
            "模型未返回可显示回复"
        };
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason.to_string(),
            },
            context_summary,
        );
    }
    final_content = normalize_model_visible_content(final_content);
    if final_content.trim().is_empty() {
        let failure_reason = "模型未返回可显示回复";
        append_task_error_turn_item(
            event_bus,
            session_store,
            task_store,
            task,
            session_id,
            workspace_id,
            &turn_visibility,
            failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
        );
        return (
            TaskOutcome::Failed {
                error: failure_reason.to_string(),
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

    if task.kind == TaskKind::Validation && validation_result_rejects_delivery(&final_content) {
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
    publish_worker_lane_summary(
        event_bus,
        session_store,
        task_store,
        task,
        session_id,
        workspace_id,
        &turn_visibility,
        "completed",
        &summarize_final_for_lane(&final_content),
    );

    // P6b：把本轮 LLM 对话追写进 thread 历史，供下一 task 作为上下文。
    // 过滤掉 system 消息（prompt、workspace 上下文、历史边界标记）——这些会在下一 task
    // 启动时由 run_task_llm_loop 自动重建；只沉淀真实对话（user / assistant / tool）。
    // 补写 assistant final：循环里只把 assistant 写进 messages 是在"还有下一轮"时发生，
    // 最终 final_content 作为收尾时没有入列，这里用 final_content 显式收口。
    let mut turn_messages: Vec<ThreadChatMessage> = messages
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

    (
        TaskOutcome::Completed {
            output_refs: vec![build_output_content(tool_call_records, final_content)],
        },
        context_summary,
    )
}

fn task_tool_failure_reason(
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

fn validation_result_rejects_delivery(content: &str) -> bool {
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

fn compact_validation_failure(content: &str) -> String {
    let trimmed = content.trim();
    let compact = trimmed.chars().take(240).collect::<String>();
    if trimmed.chars().count() > 240 {
        format!("验证未通过: {compact}…")
    } else {
        format!("验证未通过: {compact}")
    }
}

fn deterministic_task_final_content(
    task: &magi_core::Task,
    task_store: &TaskStore,
) -> Option<String> {
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

fn is_planning_no_tool_action(task: &magi_core::Task) -> bool {
    task.kind == TaskKind::Action
        && task.title.contains("梳理目标")
        && task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.command_mode.eq_ignore_ascii_case("no_tools"))
        && task.dependency_ids.is_empty()
}

fn deterministic_planning_content(task: &magi_core::Task) -> String {
    let goal = extract_task_goal(&task.goal).unwrap_or_else(|| task.goal.trim().to_string());
    format!(
        "目标：{goal}\n\n边界：规划步骤只整理目标、边界、执行计划和验收标准，不调用工具，不执行文件、shell 或网络操作。\n\n执行计划：执行步骤负责按用户目标调用工具并产生可验证结果；交付步骤只基于执行产出总结，不重复调用工具。\n\n验收标准：规划文本必须包含目标、边界、执行计划、验收标准四部分；执行结果必须以真实工具结果为准，失败或阻塞不得伪装成功。"
    )
}

fn is_planning_text_validation(task: &magi_core::Task) -> bool {
    task.kind == TaskKind::Validation && task.goal.contains("只验证规划文本完整性")
}

fn deterministic_planning_validation_content(
    task: &magi_core::Task,
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

fn is_execution_tool_validation(task: &magi_core::Task) -> bool {
    task.kind == TaskKind::Validation && task.goal.contains("实际执行和工具结果")
}

fn deterministic_execution_tool_validation_content(
    task: &magi_core::Task,
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

fn collect_dependency_output_validation_facts(
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

fn extract_task_goal(value: &str) -> Option<String> {
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

fn task_required_tool_chain(task: &magi_core::Task) -> Vec<String> {
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

fn public_builtin_tool_reference_aliases() -> Vec<(&'static str, &'static str)> {
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

fn tool_reference_position(text: &str, tool_name: &str) -> Option<usize> {
    text.match_indices(tool_name).find_map(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + tool_name.len()..].chars().next();
        (is_tool_reference_boundary(before) && is_tool_reference_boundary(after)).then_some(start)
    })
}

fn is_tool_reference_boundary(value: Option<char>) -> bool {
    value
        .map(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
        .unwrap_or(true)
}

fn forced_task_tool_choice_for_round(
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

fn record_completed_required_tools(
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

fn required_tool_chain_is_complete(required_tool_chain: &[String], completed: &[String]) -> bool {
    required_tool_chain.iter().all(|required| {
        completed
            .iter()
            .any(|completed_name| completed_name == required)
    })
}

fn required_tool_chain_recovery_prompt(
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

fn tool_call_round_limit(required_tool_chain: &[String]) -> usize {
    BASE_TOOL_CALL_ROUNDS
        .max(required_tool_chain.len().saturating_add(2))
        .min(MAX_TOOL_CALL_ROUNDS)
}

fn canonical_tool_call_name(tool_name: &str) -> String {
    BuiltinToolName::from_str(tool_name.trim())
        .map(|tool| tool.as_str().to_string())
        .unwrap_or_else(|| tool_name.trim().to_string())
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
    accumulated_text: &str,
) {
    let visible_text = normalize_model_stream_preview_content(accumulated_text);
    if visible_text.trim().is_empty() {
        return;
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
    // P2：子任务流式文本归 worker sidechain，主线靠 worker_lane_summary 呈现进度，
    // 不再让同一条流式内容同时污染主线与 drawer。
    apply_task_worker_detail_visibility(&mut item, task, turn_visibility);
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
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
        accumulated_thinking,
    );
}

/// 把 worker 最终回复压缩为 dispatch 卡可展示的单行摘要，保持主线信息密度。
fn summarize_final_for_lane(final_content: &str) -> String {
    const MAX_LEN: usize = 120;
    let flat = final_content
        .split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    if flat.chars().count() <= MAX_LEN {
        return flat;
    }
    let mut truncated: String = flat.chars().take(MAX_LEN - 1).collect();
    truncated.push('…');
    truncated
}


/// 发送一条 thread-visible 的 `worker_status` 摘要 item，作为主线 dispatch 卡的
/// liveActivity 数据源。前端 projection 已将 `worker_status` 从消息渲染列表过滤
/// （[turn-projection.ts:241](web/src/stores/turn-projection.ts:241)），它只参与
/// lane 聚合，不会二次污染主线消息流。
///
/// 只有当当前任务属于 worker sidechain（`primary_worker_sidechain` + 带 lane）
/// 时才写入；其余情形（主线 primary、无 lane）没有 dispatch 卡可消费，跳过。
fn publish_worker_lane_summary(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    task_store: &TaskStore,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    turn_visibility: &TaskTurnVisibility,
    status: &str,
    summary: &str,
) {
    let TaskTurnVisibility::Sidechain {
        orchestrator_thread_id,
        role_id,
        worker_id,
        lane_id,
        lane_seq,
        has_mainline_summary,
        ..
    } = turn_visibility
    else {
        return;
    };
    if !has_mainline_summary {
        return;
    }
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return;
    }
    // 摘要归属主线 orchestrator thread：前端 projection 按 source_thread_id==
    // orchestrator 把它投射到主线 dispatch 卡的 liveActivity。同时通过 role_id
    // + lane_id 让主线知道它来自哪条 sidechain，便于点击"查看 drawer"。
    let mut item = session_turn_item(
        "worker_status",
        status,
        Some("执行进展".to_string()),
        Some(trimmed.to_string()),
        Some(format!("turn-item-worker-lane-summary-{lane_id}")),
        orchestrator_thread_id.clone(),
    );
    item.task_id = Some(task.task_id.clone());
    item.lane_id = Some(lane_id.clone());
    item.lane_seq = *lane_seq;
    item.worker_id = Some(worker_id.clone());
    item.role_id = Some(role_id.clone());
    item.source = role_id.clone();
    if let Some(published) =
        upsert_session_turn_item_with_task_store(session_store, session_id, item, Some(task_store))
    {
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
    }
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
        publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
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

fn execute_task_tool_call_batch(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task_store: &TaskStore,
    spawn_graph: &std::sync::Mutex<magi_spawn_graph::SpawnGraph>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
    knowledge_graph: Option<&magi_knowledge_graph::KnowledgeGraphStore>,
    validation_runner: Option<&magi_validation_runner::ValidationStore>,
    checkpoint: Option<&magi_checkpoint::CheckpointStore>,
    human_checkpoint: Option<&magi_human_checkpoint::HumanCheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_calls: &[ChatToolCall],
) -> Vec<(String, ExecutionResultStatus)> {
    let parsed_arguments = tool_calls
        .iter()
        .map(|tool_call| {
            serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments).ok()
        })
        .collect::<Vec<_>>();
    let tool_inputs = tool_calls
        .iter()
        .zip(parsed_arguments.iter())
        .map(|(tool_call, arguments)| ToolConcurrencyInput {
            tool_name: tool_call.function.name.as_str(),
            arguments: arguments.as_ref(),
        })
        .collect::<Vec<_>>();
    let mut results = vec![None; tool_calls.len()];

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    results[tool_index] = Some(execute_task_tool_call(
                        event_bus,
                        tool_registry,
                        skill_runtime,
                        task_store,
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
                        workspace_root_path,
                        worker_id,
                        &tool_calls[tool_index],
                    ));
                }
            }
            ToolBatchKind::Concurrent => {
                thread::scope(|scope| {
                    let handles = batch
                        .tool_indices
                        .iter()
                        .copied()
                        .map(|tool_index| {
                            let tool_call = &tool_calls[tool_index];
                            (
                                tool_index,
                                scope.spawn(move || {
                                    execute_task_tool_call(
                                        event_bus,
                                        tool_registry,
                                        skill_runtime,
                                        task_store,
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
                                        workspace_root_path,
                                        worker_id,
                                        tool_call,
                                    )
                                }),
                            )
                        })
                        .collect::<Vec<_>>();

                    for (tool_index, handle) in handles {
                        let result = handle.join().unwrap_or_else(|_| {
                            (
                                serde_json::json!({
                                    "tool": tool_calls[tool_index].function.name,
                                    "status": "failed",
                                    "error": "任务工具执行线程异常"
                                })
                                .to_string(),
                                ExecutionResultStatus::Failed,
                            )
                        });
                        results[tool_index] = Some(result);
                    }
                });
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(tool_index, result)| {
            result.unwrap_or_else(|| {
                (
                    serde_json::json!({
                        "tool": tool_calls[tool_index].function.name,
                        "status": "failed",
                        "error": "任务工具未产生执行结果"
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                )
            })
        })
        .collect()
}

/// S7-E：协调器三件套统一拦截入口。返回 (payload_json, status)，与
/// `execute_task_tool_call` 的常规工具路径形状一致，便于上层把回执拼回 LLM 消息流。
fn execute_coordinator_tool(
    event_bus: &InMemoryEventBus,
    task_store: &TaskStore,
    spawn_graph: &std::sync::Mutex<magi_spawn_graph::SpawnGraph>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool: magi_tool_runtime::BuiltinToolName,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let parsed: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(value) => value,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "failed",
                    "error": format!("协调器工具参数解析失败: {err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };

    let publish_event = |kind: &str, payload: serde_json::Value| {
        let _ = event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!("event-coordinator-{kind}-{}", UtcMillis::now().0)),
                kind,
                payload,
            )
            .with_context(EventContext {
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );
    };

    match tool {
        magi_tool_runtime::BuiltinToolName::AgentSpawn => {
            let role = parsed
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let goal = parsed
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if role.is_empty() || goal.is_empty() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "agent_spawn 缺少必需字段 role 或 goal",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let task_kind = parsed
                .get("task_kind")
                .and_then(|v| v.as_str())
                .and_then(|s| match s.to_ascii_lowercase().as_str() {
                    "action" => Some(TaskKind::Action),
                    "validation" => Some(TaskKind::Validation),
                    "repair" => Some(TaskKind::Repair),
                    "decision" => Some(TaskKind::Decision),
                    "work_package" | "workpackage" => Some(TaskKind::WorkPackage),
                    "phase" => Some(TaskKind::Phase),
                    "objective" => Some(TaskKind::Objective),
                    _ => None,
                })
                .unwrap_or(TaskKind::Action);
            let now = UtcMillis::now();
            let child_id = TaskId::new(format!(
                "task-spawn-{}-{}",
                task.task_id.as_str(),
                now.0
            ));
            let child = magi_core::Task {
                task_id: child_id.clone(),
                mission_id: task.mission_id.clone(),
                root_task_id: task.root_task_id.clone(),
                parent_task_id: Some(task.task_id.clone()),
                kind: task_kind,
                title: format!("{role}: {goal}"),
                goal: goal.clone(),
                status: TaskStatus::Ready,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: task.policy_snapshot.clone(),
                executor_binding: Some(magi_core::ExecutorBinding {
                    target_role: role.clone(),
                    capability_requirements: Vec::new(),
                    parallelism_group: parsed
                        .get("parallelism_group")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    exclusive_scope: None,
                    worker_selector: None,
                }),
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: task.workspace_scope.clone(),
                write_scope: task.write_scope.clone(),
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
            task_store.insert_task(child);
            // SpawnGraph 边：失败仅 warn（与 dispatch_execution::register_spawn_edge 一致策略）。
            if let Ok(mut graph) = spawn_graph.lock() {
                if let Err(err) = graph.add_edge(
                    task.task_id.clone(),
                    child_id.clone(),
                    task_kind,
                    std::time::SystemTime::now(),
                ) {
                    tracing::warn!(
                        parent = %task.task_id.as_str(),
                        child = %child_id.as_str(),
                        error = %err,
                        "agent_spawn SpawnGraph add_edge 失败，子任务已插入但拓扑边缺失",
                    );
                }
            }
            publish_event(
                "task.coordinator.agent_spawn",
                serde_json::json!({
                    "parent_task_id": task.task_id.to_string(),
                    "child_task_id": child_id.to_string(),
                    "role": role,
                    "goal": goal,
                    "task_kind": format!("{:?}", task_kind),
                }),
            );
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "child_task_id": child_id.to_string(),
                    "role": role,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        magi_tool_runtime::BuiltinToolName::SendMessage => {
            let target = parsed
                .get("target_task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let payload = parsed.get("payload").cloned().unwrap_or(serde_json::Value::Null);
            if target.is_empty() || payload.is_null() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "send_message 缺少必需字段 target_task_id 或 payload",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let target_id = TaskId::new(target.clone());
            if task_store.get_task(&target_id).is_none() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("send_message 目标 task {target} 不存在"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            // S7 暂以事件总线作为跨 task 消息通道；后续 slice 接入 Mailbox 后再切换路由。
            publish_event(
                "task.coordinator.send_message",
                serde_json::json!({
                    "from_task_id": task.task_id.to_string(),
                    "target_task_id": target,
                    "kind": parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("user"),
                    "payload": payload,
                }),
            );
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "target_task_id": target,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        magi_tool_runtime::BuiltinToolName::TaskStop => {
            let target = parsed
                .get("target_task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if target.is_empty() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": "task_stop 缺少必需字段 target_task_id",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let target_id = TaskId::new(target.clone());
            if task_store.get_task(&target_id).is_none() {
                return (
                    serde_json::json!({
                        "tool": tool.as_str(),
                        "status": "failed",
                        "error": format!("task_stop 目标 task {target} 不存在"),
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            }
            let mut cancelled: Vec<String> = Vec::new();
            // 先收集 open 子孙，再统一标记 cancel —— 锁内只做拓扑查询，避免持锁更新 store。
            let descendants = match spawn_graph.lock() {
                Ok(graph) => graph.open_descendants(&target_id),
                Err(err) => {
                    tracing::warn!(?err, "task_stop SpawnGraph mutex poisoned，仅取消目标任务");
                    Vec::new()
                }
            };
            for id in std::iter::once(target_id.clone()).chain(descendants.into_iter()) {
                if task_store
                    .update_status(&id, TaskStatus::Cancelled)
                    .is_ok()
                {
                    cancelled.push(id.to_string());
                    if let Ok(mut graph) = spawn_graph.lock() {
                        let _ = graph.mark_closed(&id, std::time::SystemTime::now());
                    }
                }
            }
            publish_event(
                "task.coordinator.task_stop",
                serde_json::json!({
                    "from_task_id": task.task_id.to_string(),
                    "target_task_id": target,
                    "cancelled_task_ids": cancelled,
                    "reason": parsed.get("reason").and_then(|v| v.as_str()).unwrap_or(""),
                }),
            );
            (
                serde_json::json!({
                    "tool": tool.as_str(),
                    "status": "succeeded",
                    "cancelled_task_ids": cancelled,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        _ => unreachable!("execute_coordinator_tool 只接收 3 个协调器变体"),
    }
}

/// S9：`todo_write` 工具拦截器。整体替换当前 session 的 TodoLedger，并把新快照
/// 同步到事件总线，UI / 测试可以观察。
fn execute_todo_write_tool(
    event_bus: &InMemoryEventBus,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    match magi_todo_ledger::parse_todo_write_arguments(&tool_call.function.arguments) {
        Ok(items) => {
            let stored = todo_ledger.replace(items);
            let snapshot_payload = serde_json::to_value(&stored).unwrap_or(serde_json::Value::Null);
            let _ = event_bus.publish(
                EventEnvelope::domain(
                    EventId::new(format!("event-todo-ledger-updated-{}", UtcMillis::now().0)),
                    "task.todo_ledger.updated",
                    serde_json::json!({
                        "task_id": task.task_id.to_string(),
                        "session_id": session_id.to_string(),
                        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                        "count": stored.len(),
                        "todos": snapshot_payload,
                    }),
                )
                .with_context(EventContext {
                    workspace_id: workspace_id.clone(),
                    session_id: Some(session_id.clone()),
                    mission_id: Some(task.mission_id.clone()),
                    task_id: Some(task.task_id.clone()),
                    ..EventContext::default()
                }),
            );
            (
                serde_json::json!({
                    "tool": "todo_write",
                    "status": "succeeded",
                    "count": stored.len(),
                    "todos": stored,
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            )
        }
        Err(err) => (
            serde_json::json!({
                "tool": "todo_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        ),
    }
}

/// S10：`memory_write` 工具拦截器。把保存/删除请求落地到 workspace 的
/// `~/.magi/projects/{slug}/memory/` 目录，同时把变更事件发到 bus 供 UI 订阅。
/// 当前 task 未绑定 workspace 时直接失败，避免静默丢弃记忆请求。
fn execute_memory_write_tool(
    event_bus: &InMemoryEventBus,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = project_memory else {
        return (
            serde_json::json!({
                "tool": "memory_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位项目记忆目录",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let parsed = match magi_project_memory::parse_memory_write_arguments(
        &tool_call.function.arguments,
    ) {
        Ok(action) => action,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "memory_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let (event_kind, payload, status) = match parsed {
        magi_project_memory::MemoryWriteAction::Save(entry) => {
            let file_stem = entry.file_stem.clone();
            match store.save_entry(&entry) {
                Ok(()) => (
                    "save",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "succeeded",
                        "action": "save",
                        "file_stem": file_stem,
                        "kind": entry.kind.as_str(),
                    }),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(err) => (
                    "save",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "failed",
                        "action": "save",
                        "file_stem": file_stem,
                        "error": err.to_string(),
                    }),
                    ExecutionResultStatus::Failed,
                ),
            }
        }
        magi_project_memory::MemoryWriteAction::Delete { file_stem } => {
            match store.delete_entry(&file_stem) {
                Ok(true) => (
                    "delete",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "succeeded",
                        "action": "delete",
                        "file_stem": file_stem,
                    }),
                    ExecutionResultStatus::Succeeded,
                ),
                Ok(false) => (
                    "delete",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "succeeded",
                        "action": "delete",
                        "file_stem": file_stem,
                        "note": "entry 不存在，已视为幂等删除",
                    }),
                    ExecutionResultStatus::Succeeded,
                ),
                Err(err) => (
                    "delete",
                    serde_json::json!({
                        "tool": "memory_write",
                        "status": "failed",
                        "action": "delete",
                        "file_stem": file_stem,
                        "error": err.to_string(),
                    }),
                    ExecutionResultStatus::Failed,
                ),
            }
        }
    };
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-project-memory-updated-{}", UtcMillis::now().0)),
            "task.project_memory.updated",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "action": event_kind,
                "result": payload,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), status)
}

fn execute_mission_charter_write_tool(
    event_bus: &InMemoryEventBus,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = mission_charter else {
        return (
            serde_json::json!({
                "tool": "mission_charter_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission charter 目录",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match magi_mission_charter::parse_mission_charter_write_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = magi_core::UtcMillis::now();
    let mut charter = match store.load(&task.mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => {
            // 首次写入需要 title + goal 才能构造合法 charter；缺一即拒，避免半成品契约落盘。
            let (Some(title), Some(goal)) = (args.title.clone(), args.goal.clone()) else {
                return (
                    serde_json::json!({
                        "tool": "mission_charter_write",
                        "status": "failed",
                        "error": "首次创建 charter 必须同时提供 title 与 goal",
                    })
                    .to_string(),
                    ExecutionResultStatus::Failed,
                );
            };
            magi_mission_charter::MissionCharter::new(task.mission_id.clone(), title, goal, now)
        }
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "mission_charter_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let changed = magi_mission_charter::apply_charter_update(&mut charter, args, now);
    if let Err(err) = store.save(&charter) {
        return (
            serde_json::json!({
                "tool": "mission_charter_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let payload = serde_json::json!({
        "tool": "mission_charter_write",
        "status": "succeeded",
        "mission_id": charter.mission_id.to_string(),
        "title": charter.title,
        "changed": changed,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-mission-charter-updated-{}",
                UtcMillis::now().0
            )),
            "task.mission_charter.updated",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": charter.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "changed": changed,
                "title": charter.title,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

fn execute_plan_write_tool(
    event_bus: &InMemoryEventBus,
    plan: Option<&magi_plan::PlanStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = plan else {
        return (
            serde_json::json!({
                "tool": "plan_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission plan 目录",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match magi_plan::parse_plan_write_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = magi_core::UtcMillis::now();
    let mut plan_doc = match store.load(&task.mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => magi_plan::Plan::new(task.mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "plan_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let changed = magi_plan::apply_plan_update(&mut plan_doc, args, now);
    if let Err(err) = store.save(&plan_doc) {
        return (
            serde_json::json!({
                "tool": "plan_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let payload = serde_json::json!({
        "tool": "plan_write",
        "status": "succeeded",
        "mission_id": plan_doc.mission_id.to_string(),
        "step_count": plan_doc.steps.len(),
        "changed": changed,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-plan-updated-{}", UtcMillis::now().0)),
            "task.plan.updated",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": plan_doc.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "changed": changed,
                "step_count": plan_doc.steps.len(),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

fn execute_kg_write_tool(
    event_bus: &InMemoryEventBus,
    knowledge_graph: Option<&magi_knowledge_graph::KnowledgeGraphStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = knowledge_graph else {
        return (
            serde_json::json!({
                "tool": "kg_write",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission knowledge graph",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "kg_write",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match magi_knowledge_graph::parse_kg_write_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "kg_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = magi_core::UtcMillis::now();
    let mut graph = match store.load(&task.mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => magi_knowledge_graph::KnowledgeGraph::new(task.mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "kg_write",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let kind = args.kind;
    let fact_id = args.id.clone();
    let changed = magi_knowledge_graph::apply_kg_update(&mut graph, args, now);
    if let Err(err) = store.save(&graph) {
        return (
            serde_json::json!({
                "tool": "kg_write",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let payload = serde_json::json!({
        "tool": "kg_write",
        "status": "succeeded",
        "mission_id": graph.mission_id.to_string(),
        "kind": kind.as_str(),
        "id": fact_id.clone(),
        "fact_count": graph.facts.len(),
        "changed": changed,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-kg-updated-{}", UtcMillis::now().0)),
            "task.kg.updated",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": graph.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "kind": kind.as_str(),
                "id": fact_id,
                "changed": changed,
                "fact_count": graph.facts.len(),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

/// S15：`validation_record` 工具实现。把一次验证（test_suite / type_check /
/// integration_smoke / benchmark）的 pass/fail/skipped 结果写入 mission 维度的
/// `validation.md`。`(plan_step_id, kind)` 唯一，重复写入按 upsert 处理并 bump version。
fn execute_validation_record_tool(
    event_bus: &InMemoryEventBus,
    validation_runner: Option<&magi_validation_runner::ValidationStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = validation_runner else {
        return (
            serde_json::json!({
                "tool": "validation_record",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission validation runner",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "validation_record",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match magi_validation_runner::parse_validation_record_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "validation_record",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = magi_core::UtcMillis::now();
    let mut report = match store.load(&task.mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => magi_validation_runner::ValidationReport::new(task.mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "validation_record",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let plan_step_id = args.plan_step_id.clone();
    let kind = args.kind;
    let outcome = args.outcome;
    let changed = magi_validation_runner::apply_validation_record(&mut report, args, now);
    if let Err(err) = store.save(&report) {
        return (
            serde_json::json!({
                "tool": "validation_record",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let step_passing = report.step_is_passing(&plan_step_id);
    let payload = serde_json::json!({
        "tool": "validation_record",
        "status": "succeeded",
        "mission_id": report.mission_id.to_string(),
        "plan_step_id": plan_step_id,
        "kind": kind.as_str(),
        "outcome": outcome.as_str(),
        "record_count": report.records.len(),
        "changed": changed,
        "step_is_passing": step_passing,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-validation-updated-{}",
                UtcMillis::now().0
            )),
            "task.validation.updated",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": report.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "plan_step_id": plan_step_id,
                "kind": kind.as_str(),
                "outcome": outcome.as_str(),
                "changed": changed,
                "step_is_passing": step_passing,
                "record_count": report.records.len(),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

/// S16：`checkpoint_create` 工具实现。把一次 mission 级检查点（process_restart /
/// context_compaction / phase_transition / manual）append 到 mission 维度的
/// `checkpoints.md`。语义为 append-only：sequence 单调递增，不允许修改或删除历史记录。
fn execute_checkpoint_create_tool(
    event_bus: &InMemoryEventBus,
    checkpoint: Option<&magi_checkpoint::CheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = checkpoint else {
        return (
            serde_json::json!({
                "tool": "checkpoint_create",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission checkpoint store",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match magi_checkpoint::parse_checkpoint_create_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = magi_core::UtcMillis::now();
    let mut log = match store.load(&task.mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => magi_checkpoint::CheckpointLog::new(task.mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "checkpoint_create",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let kind = args.kind;
    let label_snapshot = args.label.clone();
    let sequence = magi_checkpoint::append_checkpoint(&mut log, args, now);
    if let Err(err) = store.save(&log) {
        return (
            serde_json::json!({
                "tool": "checkpoint_create",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let payload = serde_json::json!({
        "tool": "checkpoint_create",
        "status": "succeeded",
        "mission_id": log.mission_id.to_string(),
        "sequence": sequence,
        "kind": kind.as_str(),
        "label": label_snapshot,
        "checkpoint_count": log.checkpoints.len(),
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-checkpoint-appended-{}",
                UtcMillis::now().0
            )),
            "task.checkpoint.appended",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": log.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "sequence": sequence,
                "kind": kind.as_str(),
                "label": label_snapshot,
                "checkpoint_count": log.checkpoints.len(),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

/// S17：`human_checkpoint_request` 工具实现。把 orchestrator 申请的人工审核点写入
/// mission 维度的 `human_checkpoints.md`，记录为 Pending 状态；resolve 由 operator
/// 侧另起 API 调用，不在本工具流程内。pending 项在 prompt 注入时会强制 Coordinator
/// 停止派发新工作，直到 operator approve / reject。
fn execute_human_checkpoint_request_tool(
    event_bus: &InMemoryEventBus,
    human_checkpoint: Option<&magi_human_checkpoint::HumanCheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    let Some(store) = human_checkpoint else {
        return (
            serde_json::json!({
                "tool": "human_checkpoint_request",
                "status": "failed",
                "error": "当前 task 未绑定 workspace，无法定位 mission human_checkpoint store",
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    };
    let args_value: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
        Ok(v) => v,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "human_checkpoint_request",
                    "status": "failed",
                    "error": format!("arguments 非合法 JSON：{err}"),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let args = match magi_human_checkpoint::parse_human_checkpoint_request_arguments(&args_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "human_checkpoint_request",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let now = magi_core::UtcMillis::now();
    let mut log = match store.load(&task.mission_id) {
        Ok(Some(existing)) => existing,
        Ok(None) => magi_human_checkpoint::HumanCheckpointLog::new(task.mission_id.clone(), now),
        Err(err) => {
            return (
                serde_json::json!({
                    "tool": "human_checkpoint_request",
                    "status": "failed",
                    "error": err.to_string(),
                })
                .to_string(),
                ExecutionResultStatus::Failed,
            );
        }
    };
    let plan_step_id_snapshot = args.plan_step_id.clone();
    let prompt_snapshot = args.prompt_to_human.clone();
    let label_snapshot = args.label.clone();
    let sequence = magi_human_checkpoint::append_human_checkpoint_request(&mut log, args, now);
    if let Err(err) = store.save(&log) {
        return (
            serde_json::json!({
                "tool": "human_checkpoint_request",
                "status": "failed",
                "error": err.to_string(),
            })
            .to_string(),
            ExecutionResultStatus::Failed,
        );
    }
    let pending_count = log
        .entries
        .iter()
        .filter(|c| c.status.is_pending())
        .count();
    let payload = serde_json::json!({
        "tool": "human_checkpoint_request",
        "status": "succeeded",
        "mission_id": log.mission_id.to_string(),
        "sequence": sequence,
        "plan_step_id": plan_step_id_snapshot,
        "label": label_snapshot,
        "pending_count": pending_count,
    });
    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-human-checkpoint-requested-{}",
                UtcMillis::now().0
            )),
            "task.human_checkpoint.requested",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": log.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "sequence": sequence,
                "plan_step_id": plan_step_id_snapshot,
                "prompt_to_human": prompt_snapshot,
                "label": label_snapshot,
                "pending_count": pending_count,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );
    (payload.to_string(), ExecutionResultStatus::Succeeded)
}

fn execute_task_tool_call(
    event_bus: &InMemoryEventBus,
    tool_registry: Option<&ToolRegistry>,
    skill_runtime: Option<&magi_skill_runtime::SkillRuntime>,
    task_store: &TaskStore,
    spawn_graph: &std::sync::Mutex<magi_spawn_graph::SpawnGraph>,
    safety_gate: Option<&magi_safety_gate::SafetyGate>,
    todo_ledger: &magi_todo_ledger::TodoLedger,
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    mission_charter: Option<&magi_mission_charter::MissionCharterStore>,
    plan: Option<&magi_plan::PlanStore>,
    knowledge_graph: Option<&magi_knowledge_graph::KnowledgeGraphStore>,
    validation_runner: Option<&magi_validation_runner::ValidationStore>,
    checkpoint: Option<&magi_checkpoint::CheckpointStore>,
    human_checkpoint: Option<&magi_human_checkpoint::HumanCheckpointStore>,
    task: &magi_core::Task,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    workspace_root_path: Option<&PathBuf>,
    worker_id: Option<&magi_core::WorkerId>,
    tool_call: &ChatToolCall,
) -> (String, ExecutionResultStatus) {
    // S7-E：协调器三件套（agent_spawn / send_message / task_stop）由 orchestration 层拦截，
    // 不进 BuiltinTool::execute —— 它们需要 task_store / spawn_graph / event_bus 等上下文。
    // S9：TodoWrite 同样在此层拦截，因为它要操作 session 维度的 TodoLedger。
    // S10：MemoryWrite 同样在此层拦截，因为它要操作 workspace 维度的 ProjectMemoryStore。
    // S11：MissionCharterWrite 同样在此层拦截，因为它要操作 mission 维度的 MissionCharterStore。
    // S12：PlanWrite 同样在此层拦截，因为它要操作 mission 维度的 PlanStore。
    if let Some(canonical) =
        magi_tool_runtime::BuiltinToolName::from_str(tool_call.function.name.as_str())
    {
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::AgentSpawn
                | magi_tool_runtime::BuiltinToolName::SendMessage
                | magi_tool_runtime::BuiltinToolName::TaskStop
        ) {
            return execute_coordinator_tool(
                event_bus,
                task_store,
                spawn_graph,
                task,
                session_id,
                workspace_id,
                canonical,
                tool_call,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::TodoWrite) {
            return execute_todo_write_tool(
                event_bus,
                todo_ledger,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::MemoryWrite) {
            return execute_memory_write_tool(
                event_bus,
                project_memory,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::MissionCharterWrite) {
            return execute_mission_charter_write_tool(
                event_bus,
                mission_charter,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::PlanWrite) {
            return execute_plan_write_tool(
                event_bus,
                plan,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        // S14：KgWrite 同样在此层拦截，因为它要操作 mission 维度的 KnowledgeGraphStore。
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::KgWrite) {
            return execute_kg_write_tool(
                event_bus,
                knowledge_graph,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        // S15：ValidationRecord 同样在此层拦截，因为它要操作 mission 维度的 ValidationStore。
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::ValidationRecord) {
            return execute_validation_record_tool(
                event_bus,
                validation_runner,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        // S16：Checkpoint 同样在此层拦截，因为它要操作 mission 维度的 CheckpointStore。
        if matches!(canonical, magi_tool_runtime::BuiltinToolName::Checkpoint) {
            return execute_checkpoint_create_tool(
                event_bus,
                checkpoint,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
        // S17：HumanCheckpointRequest 同样在此层拦截，因为它要操作 mission 维度的
        // HumanCheckpointStore，并触发 awaiting_human 状态。
        if matches!(
            canonical,
            magi_tool_runtime::BuiltinToolName::HumanCheckpointRequest
        ) {
            return execute_human_checkpoint_request_tool(
                event_bus,
                human_checkpoint,
                task,
                session_id,
                workspace_id,
                tool_call,
            );
        }
    }

    let Some(registry) = tool_registry else {
        return (
            serde_json::json!({ "error": "tool registry not available" }).to_string(),
            ExecutionResultStatus::Failed,
        );
    };

    let _ = event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-task-tool-invoked-{}", UtcMillis::now().0)),
            "task.tool.invoked",
            serde_json::json!({
                "task_id": task.task_id.to_string(),
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "worker_id": worker_id.map(ToString::to_string),
                "tool_name": tool_call.function.name,
                "tool_call_id": tool_call.id,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        }),
    );

    if tool_call.function.name == SKILL_APPLY_TOOL_NAME {
        return execute_skill_apply_from_runtime(&tool_call.function.arguments, skill_runtime);
    }

    if let Some(rejection) = internal_builtin_tool_rejection_payload(&tool_call.function.name) {
        return (rejection, ExecutionResultStatus::Failed);
    }

    if let Some(rejection) = task_policy_tool_rejection(
        task,
        &tool_call.function.name,
        &tool_call.function.arguments,
    ) {
        return (rejection, ExecutionResultStatus::Rejected);
    }

    // S8：SafetyGate 语义判定。Permission 通过后仍可能命中"高危子串"（如
    // `git push --force` / `rm -rf`），此处对 arguments 内容直接做匹配。
    if let Some(gate) = safety_gate {
        if let Some(rejection) =
            safety_gate_rejection(gate, &tool_call.function.name, &tool_call.function.arguments)
        {
            return (rejection, ExecutionResultStatus::Rejected);
        }
    }

    let output = registry.execute_with_policy(
        ToolExecutionInput {
            tool_call_id: ToolCallId::new(&tool_call.id),
            tool_name: tool_call.function.name.clone(),
            tool_kind: ToolKind::Builtin,
            input: tool_call.function.arguments.clone(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        },
        ToolExecutionContext {
            worker_id: worker_id.cloned(),
            task_id: Some(task.task_id.clone()),
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.clone(),
            working_directory: workspace_root_path.cloned(),
        },
        &ToolExecutionPolicy::default(),
    );

    (output.payload, output.status)
}

fn task_policy_tool_rejection(
    task: &magi_core::Task,
    requested_tool_name: &str,
    arguments: &str,
) -> Option<String> {
    let policy_snapshot = task.policy_snapshot.as_ref()?;
    let canonical_tool_name = canonical_builtin_tool_name(requested_tool_name)
        .unwrap_or_else(|| requested_tool_name.trim().to_string());
    // no_tools 是 PermissionEngine 三维之外的全局开关，本层先单独拦截。
    if policy_snapshot.command_mode.eq_ignore_ascii_case("no_tools") {
        return Some(task_policy_rejection_payload(
            &canonical_tool_name,
            format!("当前任务阶段不允许调用工具: {canonical_tool_name}"),
        ));
    }
    // PermissionEngine 比对工具名是按字面比对，因此把 policy 中的别名先 canonical 化。
    let mut canonical_policy = magi_permissions::PermissionPolicy::from_core_policy(policy_snapshot);
    canonical_policy.allowed_tools = policy_snapshot
        .allowed_tools
        .iter()
        .map(|tool| canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string()))
        .collect();
    canonical_policy.denied_tools = policy_snapshot
        .denied_tools
        .iter()
        .map(|tool| canonical_builtin_tool_name(tool).unwrap_or_else(|| tool.trim().to_string()))
        .collect();

    let engine = magi_permissions::PermissionEngine::with_builtin_defaults();
    let is_write_tool = BuiltinToolName::from_str(canonical_tool_name.as_str())
        .is_some_and(|tool| tool.is_write_operation());

    let tool_request = magi_permissions::PermissionRequest::ToolInvocation {
        tool_name: canonical_tool_name.as_str(),
        is_write_tool,
    };
    if let magi_permissions::Decision::Deny { reason } = engine.decide(
        &tool_request,
        &canonical_policy,
        magi_permissions::PermissionMode::Default,
    ) {
        return Some(task_policy_rejection_payload(&canonical_tool_name, reason));
    }
    // shell_exec 在只读任务下需要 access_mode=read_only —— 走 ShellCommand 轴判定。
    if canonical_tool_name == BuiltinToolName::ShellExec.as_str() {
        let shell_request = magi_permissions::PermissionRequest::ShellCommand {
            arguments_json: arguments,
        };
        if let magi_permissions::Decision::Deny { reason } = engine.decide(
            &shell_request,
            &canonical_policy,
            magi_permissions::PermissionMode::Default,
        ) {
            return Some(task_policy_rejection_payload(&canonical_tool_name, reason));
        }
    }
    None
}

fn canonical_builtin_tool_name(tool_name: &str) -> Option<String> {
    BuiltinToolName::from_str(tool_name.trim()).map(|tool| tool.as_str().to_string())
}

fn task_policy_rejection_payload(tool_name: &str, error: String) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "rejected",
        "error": error,
    })
    .to_string()
}

/// S8：把 SafetyGate 的 Block / RequireApproval 判定折叠成"Rejected payload"。
/// 当前 task_llm_loop 没有交互审批通道（governance 走自己的回路），所以
/// RequireApproval 在本层暂时与 Block 同语义——拒绝执行并把原因回灌给模型，
/// 由模型决定是否换更精确的命令或转向人审通道。
fn safety_gate_rejection(
    gate: &magi_safety_gate::SafetyGate,
    tool_name: &str,
    arguments: &str,
) -> Option<String> {
    let canonical_tool_name = canonical_builtin_tool_name(tool_name)
        .unwrap_or_else(|| tool_name.trim().to_string());
    match gate.evaluate(&canonical_tool_name, arguments) {
        magi_safety_gate::SafetyDecision::Allow => None,
        magi_safety_gate::SafetyDecision::Block {
            category, pattern, reason,
        }
        | magi_safety_gate::SafetyDecision::RequireApproval {
            category, pattern, reason,
        } => Some(
            serde_json::json!({
                "tool": canonical_tool_name,
                "status": "rejected",
                "error": reason,
                "safety_gate": {
                    "category": category.as_str(),
                    "pattern": pattern,
                },
            })
            .to_string(),
        ),
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

fn current_turn_has_worker_lanes(session_store: &SessionStore, session_id: &SessionId) -> bool {
    session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| !turn.worker_lanes.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, BridgeResponse, ChatToolFunction,
    };
    use magi_core::{MissionId, Task, TaskKind, TaskStatus, WorkerId};
    use magi_governance::GovernanceService;
    use magi_session_store::{
        ActiveExecutionTurn, ActiveExecutionTurnLane, CanonicalTurnItemKind,
        CanonicalTurnItemStatus, CanonicalTurnStatus, TimelineEntryKind,
    };
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
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
                                "name": "shell",
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

    #[test]
    fn execute_task_tool_call_rejects_internal_process_launch_surface() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-process-launch-rejected");
        let session_id = SessionId::new("session-process-launch-rejected");
        let workspace_id = Some(WorkspaceId::new("workspace-process-launch-rejected"));
        let worker_id = WorkerId::new("worker-process-launch-rejected");
        let call = ChatToolCall {
            id: "tool-call-process-launch".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "process_launch".to_string(),
                arguments: serde_json::json!({ "command": "sleep 60" }).to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &session_id,
            &workspace_id,
            None,
            Some(&worker_id),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "process_launch");
        assert_eq!(parsed["status"], "failed");
        assert!(
            parsed["error"]
                .as_str()
                .expect("error should be string")
                .contains("shell_exec")
        );
    }

    #[test]
    fn execute_task_tool_call_rejects_write_tool_for_readonly_task_policy() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut task = make_task_loop_test_task("task-readonly-write-policy");
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: vec!["file_read".to_string(), "shell_exec".to_string()],
            denied_tools: vec!["file_write".to_string()],
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "read_only".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let call = ChatToolCall {
            id: "tool-call-file-write-readonly".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_write".to_string(),
                arguments: serde_json::json!({
                    "path": "/tmp/readonly-policy.txt",
                    "content": "must-not-write"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-readonly-write-policy"),
            &Some(WorkspaceId::new("workspace-readonly-write-policy")),
            None,
            Some(&WorkerId::new("worker-readonly-write-policy")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "file_write");
        assert_eq!(parsed["status"], "rejected");
    }

    #[test]
    fn execute_task_tool_call_requires_readonly_shell_access_mode_for_readonly_task_policy() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut task = make_task_loop_test_task("task-readonly-shell-policy");
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: vec!["shell_exec".to_string()],
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "read_only".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let call = ChatToolCall {
            id: "tool-call-shell-missing-access".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({ "command": "printf ok" }).to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-readonly-shell-policy"),
            &Some(WorkspaceId::new("workspace-readonly-shell-policy")),
            None,
            Some(&WorkerId::new("worker-readonly-shell-policy")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("access_mode=read_only"));
    }

    #[test]
    fn execute_task_tool_call_rejects_every_tool_for_no_tool_task_policy() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let mut task = make_task_loop_test_task("task-no-tool-policy");
        task.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "Autonomous".to_string(),
            approval_mode: "DecisionOnly".to_string(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            network_mode: "full".to_string(),
            command_mode: "no_tools".to_string(),
            retry_limit: 1,
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });
        let call = ChatToolCall {
            id: "tool-call-no-tool-file-read".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "file_read".to_string(),
                arguments: serde_json::json!({ "path": "/tmp/no-tool-policy.txt" }).to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-no-tool-policy"),
            &Some(WorkspaceId::new("workspace-no-tool-policy")),
            None,
            Some(&WorkerId::new("worker-no-tool-policy")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("不允许调用工具"));
    }

    #[test]
    fn execute_task_tool_call_blocks_shell_force_push_via_safety_gate() {
        // S8：即使 PermissionEngine 放行了 shell_exec（policy 没显式拒绝），
        // SafetyGate 仍要拦截 `git push --force` 这类高危子串。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let gate = magi_safety_gate::SafetyGate::with_builtin_defaults();
        let task = make_task_loop_test_task("task-safety-gate-force-push");
        let call = ChatToolCall {
            id: "tool-call-safety-gate-force-push".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({ "command": "git push --force origin main" })
                    .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            Some(&gate),
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-safety-gate"),
            &Some(WorkspaceId::new("workspace-safety-gate")),
            None,
            Some(&WorkerId::new("worker-safety-gate")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Rejected);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "shell_exec");
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["safety_gate"]["category"], "git_history");
        assert_eq!(parsed["safety_gate"]["pattern"], "git push --force");
    }

    #[test]
    fn execute_task_tool_call_passes_safe_shell_when_gate_present() {
        // SafetyGate 仅拦截命中的高危子串，普通 shell 命令应顺利通过。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let gate = magi_safety_gate::SafetyGate::with_builtin_defaults();
        let task = make_task_loop_test_task("task-safety-gate-safe-shell");
        let call = ChatToolCall {
            id: "tool-call-safety-gate-safe-shell".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({ "command": "ls -la /tmp" }).to_string(),
            },
        };

        let (_payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            Some(&gate),
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-safety-gate-safe"),
            &Some(WorkspaceId::new("workspace-safety-gate-safe")),
            None,
            Some(&WorkerId::new("worker-safety-gate-safe")),
            &call,
        );
        // 不强求 status 是 Succeeded —— tool registry 在测试环境下可能因沙箱无法
        // 真实执行 ls，但至少不能是 SafetyGate 触发的 Rejected。
        assert_ne!(status, ExecutionResultStatus::Rejected);
    }

    #[test]
    fn todo_write_tool_replaces_session_ledger_and_emits_event() {
        // S9：模型调用 `todo_write` 时，orchestration 层应直接写到 session 的
        // TodoLedger（不进入 ToolRegistry），并发 `task.todo_ledger.updated` 事件。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let ledger = magi_todo_ledger::TodoLedger::new();
        let task = make_task_loop_test_task("task-todo-write");

        // 第一次写入：两项 todo。
        let first_call = ChatToolCall {
            id: "tool-call-todo-write-first".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "todo_write".to_string(),
                arguments: serde_json::json!({
                    "todos": [
                        { "content": "拆 S9", "activeForm": "正在拆 S9", "status": "in_progress" },
                        { "content": "跑测试", "activeForm": "正在跑测试", "status": "pending" },
                    ]
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-todo-write"),
            &Some(WorkspaceId::new("workspace-todo-write")),
            None,
            Some(&WorkerId::new("worker-todo-write")),
            &first_call,
        );
        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "todo_write");
        assert_eq!(parsed["count"], 2);
        assert_eq!(ledger.snapshot().len(), 2);
        assert_eq!(ledger.snapshot()[0].content, "拆 S9");

        // 第二次写入：单项 → 整体替换语义验证。
        let second_call = ChatToolCall {
            id: "tool-call-todo-write-second".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "todo_write".to_string(),
                arguments: serde_json::json!({
                    "todos": [
                        { "content": "提交代码", "activeForm": "正在提交", "status": "pending" }
                    ]
                })
                .to_string(),
            },
        };
        let (_, status2) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-todo-write"),
            &Some(WorkspaceId::new("workspace-todo-write")),
            None,
            Some(&WorkerId::new("worker-todo-write")),
            &second_call,
        );
        assert_eq!(status2, ExecutionResultStatus::Succeeded);
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].content, "提交代码");
    }

    #[test]
    fn todo_write_tool_reports_failure_on_malformed_arguments() {
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let ledger = magi_todo_ledger::TodoLedger::new();
        let task = make_task_loop_test_task("task-todo-write-bad");
        let call = ChatToolCall {
            id: "tool-call-todo-write-bad".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "todo_write".to_string(),
                arguments: serde_json::json!({ "items": [] }).to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-todo-write-bad"),
            &Some(WorkspaceId::new("workspace-todo-write-bad")),
            None,
            Some(&WorkerId::new("worker-todo-write-bad")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["status"], "failed");
        assert!(
            parsed["error"].as_str().unwrap().contains("todos"),
            "error 应说明缺少 todos 字段，实际：{}",
            parsed["error"]
        );
        assert!(ledger.is_empty(), "失败时不能改写 ledger");
    }

    #[test]
    fn memory_write_tool_saves_entry_via_project_memory_store() {
        // S10：模型调用 `memory_write { action: save, ... }` 时，orchestration 层应直接写到
        // workspace 对应的 ProjectMemoryStore，并发 `task.project_memory.updated` 事件。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_project_memory::ProjectMemoryStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open project memory store");
        let task = make_task_loop_test_task("task-memory-write-save");

        let call = ChatToolCall {
            id: "tool-call-memory-write-save".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "memory_write".to_string(),
                arguments: serde_json::json!({
                    "action": "save",
                    "file_stem": "feedback_test",
                    "name": "测试反馈",
                    "description": "测试用",
                    "kind": "feedback",
                    "body": "规则：测试别 mock。\n**Why:** 历史上 mock 掩盖了 prod 问题。\n**How to apply:** 集成测试一律连真实依赖。"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            Some(&store),
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-memory-write-save"),
            &Some(WorkspaceId::new("workspace-memory-write-save")),
            None,
            Some(&WorkerId::new("worker-memory-write-save")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "memory_write");
        assert_eq!(parsed["action"], "save");
        assert_eq!(parsed["file_stem"], "feedback_test");
        let entries = store.list_entries().expect("list entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_stem, "feedback_test");
        assert_eq!(entries[0].kind, magi_project_memory::MemoryKind::Feedback);
    }

    #[test]
    fn memory_write_tool_deletes_entry_via_project_memory_store() {
        // S10：模型调用 `memory_write { action: delete, file_stem }` 时应删除对应记忆条目。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_project_memory::ProjectMemoryStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open project memory store");
        store
            .save_entry(&magi_project_memory::MemoryEntry {
                file_stem: "user_role".to_string(),
                name: "用户角色".to_string(),
                description: "用户角色概述".to_string(),
                kind: magi_project_memory::MemoryKind::User,
                body: "资深 Rust 工程师".to_string(),
            })
            .expect("save user entry");

        let task = make_task_loop_test_task("task-memory-write-delete");
        let call = ChatToolCall {
            id: "tool-call-memory-write-delete".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "memory_write".to_string(),
                arguments: serde_json::json!({
                    "action": "delete",
                    "file_stem": "user_role"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            Some(&store),
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-memory-write-delete"),
            &Some(WorkspaceId::new("workspace-memory-write-delete")),
            None,
            Some(&WorkerId::new("worker-memory-write-delete")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["action"], "delete");
        assert_eq!(parsed["file_stem"], "user_role");
        assert!(
            store.list_entries().expect("list").is_empty(),
            "删除后应无遗留条目"
        );
    }

    #[test]
    fn memory_write_tool_fails_without_project_memory_store() {
        // S10：当 workspace 无法解析时（project_memory = None），memory_write 必须返回失败，
        // 不能静默吞掉用户意图。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-memory-write-no-store");
        let call = ChatToolCall {
            id: "tool-call-memory-write-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "memory_write".to_string(),
                arguments: serde_json::json!({
                    "action": "save",
                    "file_stem": "user_role",
                    "name": "x",
                    "description": "y",
                    "kind": "user",
                    "body": "z"
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-memory-write-no-store"),
            &Some(WorkspaceId::new("workspace-memory-write-no-store")),
            None,
            Some(&WorkerId::new("worker-memory-write-no-store")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["status"], "failed");
        assert!(
            parsed["error"]
                .as_str()
                .unwrap()
                .contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
    }

    #[test]
    fn mission_charter_write_tool_creates_charter_on_first_write() {
        // S11：首次调用 mission_charter_write 时需同时提供 title 与 goal，
        // 否则拒绝；提供后落 charter.md，并发 task.mission_charter.updated。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_mission_charter::MissionCharterStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open mission charter store");
        let task = make_task_loop_test_task("task-mission-charter-create");
        let call = ChatToolCall {
            id: "tool-call-mission-charter-create".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "mission_charter_write".to_string(),
                arguments: serde_json::json!({
                    "title": "迁移 v2 18-slice",
                    "goal": "把 Task System v1 全量切到 v2，所有 v1 残留代码清退",
                    "success_criteria": [
                        "全量 cargo build + test 通过",
                        "S1-S18 复盘清单逐条勾选"
                    ],
                    "constraints": ["禁止双轨"],
                    "stakeholders": ["coordinator", "operator"]
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            Some(&store),
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-mission-charter-create"),
            &Some(WorkspaceId::new("workspace-mission-charter-create")),
            None,
            Some(&WorkerId::new("worker-mission-charter-create")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "mission_charter_write");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["title"], "迁移 v2 18-slice");
        let charter = store
            .load(&task.mission_id)
            .expect("load")
            .expect("charter saved");
        assert_eq!(charter.title, "迁移 v2 18-slice");
        assert_eq!(charter.success_criteria.len(), 2);
        assert_eq!(charter.constraints, vec!["禁止双轨".to_string()]);
    }

    #[test]
    fn mission_charter_write_tool_rejects_first_write_without_title_and_goal() {
        // S11：首次写入若只给 success_criteria/constraints/stakeholders 而没有 title/goal，
        // 必须拒绝以避免落下半成品 charter。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_mission_charter::MissionCharterStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open mission charter store");
        let task = make_task_loop_test_task("task-mission-charter-incomplete");
        let call = ChatToolCall {
            id: "tool-call-mission-charter-incomplete".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "mission_charter_write".to_string(),
                arguments: serde_json::json!({
                    "constraints": ["仅给约束不给 title/goal"],
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            Some(&store),
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-mission-charter-incomplete"),
            &Some(WorkspaceId::new("workspace-mission-charter-incomplete")),
            None,
            Some(&WorkerId::new("worker-mission-charter-incomplete")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("title"),
            "error 必须显式告知缺 title/goal，实际：{}",
            parsed["error"]
        );
        assert!(
            store.load(&task.mission_id).expect("load").is_none(),
            "拒绝路径下不能产生 charter 文件"
        );
    }

    #[test]
    fn mission_charter_write_tool_fails_without_store() {
        // S11：workspace 未绑定（mission_charter = None）时，工具必须返回失败。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-mission-charter-no-store");
        let call = ChatToolCall {
            id: "tool-call-mission-charter-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "mission_charter_write".to_string(),
                arguments: serde_json::json!({
                    "title": "x",
                    "goal": "y"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-mission-charter-no-store"),
            &Some(WorkspaceId::new("workspace-mission-charter-no-store")),
            None,
            Some(&WorkerId::new("worker-mission-charter-no-store")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
    }

    #[test]
    fn plan_write_tool_creates_plan_on_first_write() {
        // S12：首次调用 plan_write 时落盘 plan.md，并发 task.plan.updated 事件。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_plan::PlanStore::open_with_home(tmp.path(), &workspace_root)
            .expect("open plan store");
        let task = make_task_loop_test_task("task-plan-create");
        let call = ChatToolCall {
            id: "tool-call-plan-create".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "plan_write".to_string(),
                arguments: serde_json::json!({
                    "steps": [
                        {"id": "s1", "content": "拉取 schema", "status": "in_progress"},
                        {"id": "s2", "content": "回归测试", "depends_on": ["s1"]}
                    ]
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            Some(&store),
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-plan-create"),
            &Some(WorkspaceId::new("workspace-plan-create")),
            None,
            Some(&WorkerId::new("worker-plan-create")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "plan_write");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["step_count"], 2);
        let plan_doc = store
            .load(&task.mission_id)
            .expect("load")
            .expect("plan saved");
        assert_eq!(plan_doc.steps.len(), 2);
        assert_eq!(plan_doc.steps[0].id, "s1");
        assert_eq!(plan_doc.steps[1].depends_on, vec!["s1".to_string()]);
    }

    #[test]
    fn plan_write_tool_rejects_malformed_dependencies() {
        // S12：依赖图必须闭合且不可自指；这里给出未声明的依赖 id，要求工具拒绝。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_plan::PlanStore::open_with_home(tmp.path(), &workspace_root)
            .expect("open plan store");
        let task = make_task_loop_test_task("task-plan-bad-deps");
        let call = ChatToolCall {
            id: "tool-call-plan-bad-deps".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "plan_write".to_string(),
                arguments: serde_json::json!({
                    "steps": [
                        {"id": "s1", "content": "步骤一", "depends_on": ["s99"]}
                    ]
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            Some(&store),
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-plan-bad-deps"),
            &Some(WorkspaceId::new("workspace-plan-bad-deps")),
            None,
            Some(&WorkerId::new("worker-plan-bad-deps")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().len() > 0,
            "拒绝路径必须给出错误描述"
        );
        assert!(
            store.load(&task.mission_id).expect("load").is_none(),
            "拒绝路径下不能产生 plan 文件"
        );
    }

    #[test]
    fn plan_write_tool_fails_without_store() {
        // S12：workspace 未绑定（plan = None）时，工具必须返回失败。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-plan-no-store");
        let call = ChatToolCall {
            id: "tool-call-plan-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "plan_write".to_string(),
                arguments: serde_json::json!({
                    "steps": [
                        {"id": "s1", "content": "x"}
                    ]
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-plan-no-store"),
            &Some(WorkspaceId::new("workspace-plan-no-store")),
            None,
            Some(&WorkerId::new("worker-plan-no-store")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
    }

    #[test]
    fn kg_write_tool_creates_fact_on_first_write() {
        // S14：首次调用 kg_write 时落盘 knowledge.md，并发 task.kg.updated 事件。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store =
            magi_knowledge_graph::KnowledgeGraphStore::open_with_home(tmp.path(), &workspace_root)
                .expect("open kg store");
        let task = make_task_loop_test_task("task-kg-create");
        let call = ChatToolCall {
            id: "tool-call-kg-create".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "kg_write".to_string(),
                arguments: serde_json::json!({
                    "kind": "decision",
                    "id": "adopt-pubsub",
                    "content": "采用基于消息总线的 pub/sub 通信",
                    "reference": "docs/decision/comms.md",
                    "tags": ["arch", "comms"],
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            Some(&store),
            None,
            None,
            None,
            &task,
            &SessionId::new("session-kg-create"),
            &Some(WorkspaceId::new("workspace-kg-create")),
            None,
            Some(&WorkerId::new("worker-kg-create")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "kg_write");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["kind"], "decision");
        assert_eq!(parsed["id"], "adopt-pubsub");
        assert_eq!(parsed["fact_count"], 1);
        assert_eq!(parsed["changed"], true);
        let graph = store
            .load(&task.mission_id)
            .expect("load")
            .expect("graph saved");
        assert_eq!(graph.facts.len(), 1);
        assert_eq!(graph.facts[0].id, "adopt-pubsub");
        assert_eq!(graph.facts[0].version, 1);
    }

    #[test]
    fn kg_write_tool_rejects_malformed_args() {
        // S14：缺少必填字段（content）时工具必须拒绝。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store =
            magi_knowledge_graph::KnowledgeGraphStore::open_with_home(tmp.path(), &workspace_root)
                .expect("open kg store");
        let task = make_task_loop_test_task("task-kg-bad-args");
        let call = ChatToolCall {
            id: "tool-call-kg-bad-args".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "kg_write".to_string(),
                arguments: serde_json::json!({
                    "kind": "symbol",
                    "id": "no-content"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            Some(&store),
            None,
            None,
            None,
            &task,
            &SessionId::new("session-kg-bad-args"),
            &Some(WorkspaceId::new("workspace-kg-bad-args")),
            None,
            Some(&WorkerId::new("worker-kg-bad-args")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().len() > 0,
            "拒绝路径必须给出错误描述"
        );
        assert!(
            store.load(&task.mission_id).expect("load").is_none(),
            "拒绝路径下不能产生 knowledge graph 文件"
        );
    }

    #[test]
    fn kg_write_tool_fails_without_store() {
        // S14：workspace 未绑定（knowledge_graph = None）时工具必须返回失败。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-kg-no-store");
        let call = ChatToolCall {
            id: "tool-call-kg-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "kg_write".to_string(),
                arguments: serde_json::json!({
                    "kind": "risk",
                    "id": "r1",
                    "content": "x"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-kg-no-store"),
            &Some(WorkspaceId::new("workspace-kg-no-store")),
            None,
            Some(&WorkerId::new("worker-kg-no-store")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
    }

    #[test]
    fn validation_record_tool_creates_record_on_first_write() {
        // S15：首次调用 validation_record 时落盘 validation.md，并发 task.validation.updated 事件。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_validation_runner::ValidationStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open validation store");
        let task = make_task_loop_test_task("task-validation-create");
        let call = ChatToolCall {
            id: "tool-call-validation-create".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "validation_record".to_string(),
                arguments: serde_json::json!({
                    "plan_step_id": "s1",
                    "kind": "test_suite",
                    "outcome": "pass",
                    "command": "cargo test -p magi-api",
                    "evidence": "305 passed; 0 failed",
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            Some(&store),
            None,
            None,
            &task,
            &SessionId::new("session-validation-create"),
            &Some(WorkspaceId::new("workspace-validation-create")),
            None,
            Some(&WorkerId::new("worker-validation-create")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "validation_record");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["plan_step_id"], "s1");
        assert_eq!(parsed["kind"], "test_suite");
        assert_eq!(parsed["outcome"], "pass");
        assert_eq!(parsed["record_count"], 1);
        assert_eq!(parsed["changed"], true);
        assert_eq!(parsed["step_is_passing"], true);
        let report = store
            .load(&task.mission_id)
            .expect("load")
            .expect("report saved");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].plan_step_id, "s1");
        assert_eq!(report.records[0].version, 1);
    }

    #[test]
    fn validation_record_tool_rejects_malformed_args() {
        // S15：缺少必填字段（plan_step_id）时工具必须拒绝。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_validation_runner::ValidationStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open validation store");
        let task = make_task_loop_test_task("task-validation-bad-args");
        let call = ChatToolCall {
            id: "tool-call-validation-bad-args".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "validation_record".to_string(),
                arguments: serde_json::json!({
                    "kind": "type_check",
                    "outcome": "pass"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            Some(&store),
            None,
            None,
            &task,
            &SessionId::new("session-validation-bad-args"),
            &Some(WorkspaceId::new("workspace-validation-bad-args")),
            None,
            Some(&WorkerId::new("worker-validation-bad-args")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().len() > 0,
            "拒绝路径必须给出错误描述"
        );
        assert!(
            store.load(&task.mission_id).expect("load").is_none(),
            "拒绝路径下不能产生 validation 文件"
        );
    }

    #[test]
    fn validation_record_tool_fails_without_store() {
        // S15：workspace 未绑定（validation_runner = None）时工具必须返回失败。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-validation-no-store");
        let call = ChatToolCall {
            id: "tool-call-validation-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "validation_record".to_string(),
                arguments: serde_json::json!({
                    "plan_step_id": "s2",
                    "kind": "benchmark",
                    "outcome": "skipped"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-validation-no-store"),
            &Some(WorkspaceId::new("workspace-validation-no-store")),
            None,
            Some(&WorkerId::new("worker-validation-no-store")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
    }

    #[test]
    fn checkpoint_create_tool_appends_record_on_first_write() {
        // S16：首次调用 checkpoint_create 时落盘 checkpoints.md，sequence 从 1 开始，发 task.checkpoint.appended。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_checkpoint::CheckpointStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open checkpoint store");
        let task = make_task_loop_test_task("task-checkpoint-create");
        let call = ChatToolCall {
            id: "tool-call-checkpoint-create".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "checkpoint_create".to_string(),
                arguments: serde_json::json!({
                    "kind": "phase_transition",
                    "label": "Plan v3 完成 setup 阶段",
                    "plan_version": 3,
                    "kg_fact_count": 12,
                    "workspace_commit": "abc123",
                    "notes": "进入 wiring 阶段前的快照",
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            Some(&store),
            None,
            &task,
            &SessionId::new("session-checkpoint-create"),
            &Some(WorkspaceId::new("workspace-checkpoint-create")),
            None,
            Some(&WorkerId::new("worker-checkpoint-create")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "checkpoint_create");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["sequence"], 1);
        assert_eq!(parsed["kind"], "phase_transition");
        assert_eq!(parsed["checkpoint_count"], 1);
        let log = store
            .load(&task.mission_id)
            .expect("load")
            .expect("log saved");
        assert_eq!(log.checkpoints.len(), 1);
        assert_eq!(log.checkpoints[0].sequence, 1);
        assert_eq!(
            log.checkpoints[0].kind,
            magi_checkpoint::CheckpointKind::PhaseTransition
        );
    }

    #[test]
    fn checkpoint_create_tool_rejects_malformed_args() {
        // S16：缺少 kind 字段时工具必须拒绝，并且 checkpoints.md 不能落盘。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_checkpoint::CheckpointStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open checkpoint store");
        let task = make_task_loop_test_task("task-checkpoint-bad-args");
        let call = ChatToolCall {
            id: "tool-call-checkpoint-bad-args".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "checkpoint_create".to_string(),
                arguments: serde_json::json!({
                    "label": "missing kind"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            Some(&store),
            None,
            &task,
            &SessionId::new("session-checkpoint-bad-args"),
            &Some(WorkspaceId::new("workspace-checkpoint-bad-args")),
            None,
            Some(&WorkerId::new("worker-checkpoint-bad-args")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().len() > 0,
            "拒绝路径必须给出错误描述"
        );
        assert!(
            store.load(&task.mission_id).expect("load").is_none(),
            "拒绝路径下不能产生 checkpoint 文件"
        );
    }

    #[test]
    fn checkpoint_create_tool_fails_without_store() {
        // S16：workspace 未绑定（checkpoint = None）时工具必须返回失败。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-checkpoint-no-store");
        let call = ChatToolCall {
            id: "tool-call-checkpoint-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "checkpoint_create".to_string(),
                arguments: serde_json::json!({
                    "kind": "manual"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-checkpoint-no-store"),
            &Some(WorkspaceId::new("workspace-checkpoint-no-store")),
            None,
            Some(&WorkerId::new("worker-checkpoint-no-store")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
    }

    #[test]
    fn human_checkpoint_request_tool_creates_pending_entry() {
        // S17：首次调用 human_checkpoint_request 时落盘 human_checkpoints.md，状态 Pending，发 task.human_checkpoint.requested。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_human_checkpoint::HumanCheckpointStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open human_checkpoint store");
        let task = make_task_loop_test_task("task-human-checkpoint");
        let call = ChatToolCall {
            id: "tool-call-human-checkpoint".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "human_checkpoint_request".to_string(),
                arguments: serde_json::json!({
                    "plan_step_id": "step-deploy-prod",
                    "prompt_to_human": "本次发布会影响生产，是否继续？",
                    "label": "生产部署前的人工复核",
                    "context": "diff 摘要：xxx",
                })
                .to_string(),
            },
        };

        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&store),
            &task,
            &SessionId::new("session-human-checkpoint"),
            &Some(WorkspaceId::new("workspace-human-checkpoint")),
            None,
            Some(&WorkerId::new("worker-human-checkpoint")),
            &call,
        );

        assert_eq!(status, ExecutionResultStatus::Succeeded);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert_eq!(parsed["tool"], "human_checkpoint_request");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["sequence"], 1);
        assert_eq!(parsed["plan_step_id"], "step-deploy-prod");
        assert_eq!(parsed["pending_count"], 1);
        let log = store
            .load(&task.mission_id)
            .expect("load")
            .expect("log saved");
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].sequence, 1);
        assert!(log.entries[0].status.is_pending());
    }

    #[test]
    fn human_checkpoint_request_tool_rejects_malformed_args() {
        // S17：缺少必填字段时工具必须拒绝，并且 human_checkpoints.md 不能落盘。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            magi_core::WorkspaceRootPath::new(tmp.path().to_string_lossy().to_string());
        let store = magi_human_checkpoint::HumanCheckpointStore::open_with_home(
            tmp.path(),
            &workspace_root,
        )
        .expect("open human_checkpoint store");
        let task = make_task_loop_test_task("task-human-checkpoint-bad-args");
        let call = ChatToolCall {
            id: "tool-call-human-checkpoint-bad-args".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "human_checkpoint_request".to_string(),
                arguments: serde_json::json!({
                    "label": "missing required fields"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&store),
            &task,
            &SessionId::new("session-human-checkpoint-bad-args"),
            &Some(WorkspaceId::new("workspace-human-checkpoint-bad-args")),
            None,
            Some(&WorkerId::new("worker-human-checkpoint-bad-args")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().len() > 0,
            "拒绝路径必须给出错误描述"
        );
        assert!(
            store.load(&task.mission_id).expect("load").is_none(),
            "拒绝路径下不能产生 human_checkpoints.md"
        );
    }

    #[test]
    fn human_checkpoint_request_tool_fails_without_store() {
        // S17：workspace 未绑定（human_checkpoint = None）时工具必须返回失败。
        let tool_registry = ToolRegistry::new(
            Arc::new(GovernanceService::default()),
            Arc::new(InMemoryEventBus::new(8)),
        );
        let event_bus = InMemoryEventBus::new(8);
        let task_store = TaskStore::new();
        let spawn_graph = std::sync::Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let task = make_task_loop_test_task("task-human-checkpoint-no-store");
        let call = ChatToolCall {
            id: "tool-call-human-checkpoint-no-store".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: "human_checkpoint_request".to_string(),
                arguments: serde_json::json!({
                    "plan_step_id": "step-x",
                    "prompt_to_human": "需要操作员确认"
                })
                .to_string(),
            },
        };
        let (payload, status) = execute_task_tool_call(
            &event_bus,
            Some(&tool_registry),
            None,
            &task_store,
            &spawn_graph,
            None,
            &magi_todo_ledger::TodoLedger::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &task,
            &SessionId::new("session-human-checkpoint-no-store"),
            &Some(WorkspaceId::new("workspace-human-checkpoint-no-store")),
            None,
            Some(&WorkerId::new("worker-human-checkpoint-no-store")),
            &call,
        );
        assert_eq!(status, ExecutionResultStatus::Failed);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
        assert!(
            parsed["error"].as_str().unwrap().contains("workspace"),
            "error 应说明 workspace 未绑定，实际：{}",
            parsed["error"]
        );
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
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
            background_allowed: true,
            escalation_conditions: Vec::new(),
        });

        assert_eq!(
            task_required_tool_chain(&task),
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
            task_required_tool_chain(&task).is_empty(),
            "只读阶段即使复述用户目标，也不能强制执行写工具链"
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
            repair_limit: 1,
            validation_profile: Some("required".to_string()),
            checkpoint_mode: "task_or_phase".to_string(),
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
        validation.kind = TaskKind::Validation;
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
        validation.kind = TaskKind::Validation;
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

    fn make_task_loop_test_task(task_id: &str) -> Task {
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-task-loop"),
            root_task_id: TaskId::new(task_id),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "验证 worker 工具并发".to_string(),
            goal: "同一轮只读 shell 工具需要并发执行，并保持消息顺序".to_string(),
            status: TaskStatus::Running,
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
        let (_, orchestrator_thread_id) = session_store.ensure_session_mission(
            &session_id,
            now,
            || task.mission_id.clone(),
        );
        // P7：mainline 场景 task 自身 thread = orchestrator thread。
        let thread_id = orchestrator_thread_id.clone();
        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            conversation_registry: &ConversationRegistry::new(),
            stream_fanout: &StreamFanOut::new(),
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
            task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请执行任务".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: Some(&worker_id),
            thread_id: &thread_id,
            orchestrator_thread_id: &orchestrator_thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });
        outcome
    }

    #[test]
    fn validation_task_negative_final_marks_task_failed() {
        let mut task = make_task_loop_test_task("task-validation-negative-final");
        task.kind = TaskKind::Validation;

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
                "integration-dev",
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
        let (_, orchestrator_thread_id) = session_store.ensure_session_mission(
            &session_id,
            now,
            || task.mission_id.clone(),
        );
        let thread_id = orchestrator_thread_id.clone();

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            conversation_registry: &ConversationRegistry::new(),
            stream_fanout: &StreamFanOut::new(),
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
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请调用一个失败工具后总结".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: None,
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: Some(&worker_id),
            thread_id: &thread_id,
            orchestrator_thread_id: &orchestrator_thread_id,
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
    fn task_turn_visibility_does_not_promote_primary_role_to_worker_lane() {
        let task = make_task_loop_test_task("task-primary-role-only");
        let thread_id = ThreadId::new("thread-primary-role-only");
        let orchestrator_thread_id = thread_id.clone();

        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility = task_turn_visibility(
            &task,
            None,
            None,
            None,
            &thread_id,
            &orchestrator_thread_id,
            false,
            &registry,
        );

        // 没有 lane_id + worker_id 配对 → 必须落在 Mainline 分支。
        assert!(visibility.is_mainline());
        assert_eq!(visibility.thread_id(), &thread_id);
        assert!(visibility.worker_id().is_none());
    }

    #[test]
    fn primary_task_worker_details_move_to_sidechain() {
        let task = make_task_loop_test_task("task-primary-deep-sidechain");
        let worker_id = WorkerId::new("worker-primary-deep-sidechain");
        let lane_id = "lane-primary-deep-sidechain";
        let worker_thread_id = ThreadId::new("thread-worker-primary-deep-sidechain");
        let orchestrator_thread_id = ThreadId::new("thread-orch-primary-deep-sidechain");
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility = task_turn_visibility(
            &task,
            Some(lane_id),
            Some(1),
            Some(&worker_id),
            &worker_thread_id,
            &orchestrator_thread_id,
            true,
            &registry,
        );
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
        assert_eq!(tool_item.lane_id.as_deref(), Some(lane_id));
        assert_eq!(tool_item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(tool_item.role_id.as_deref(), Some("integration-dev"));
        assert_eq!(tool_item.source, "integration-dev");

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
        assert_eq!(final_item.role_id.as_deref(), Some("integration-dev"));
        assert_eq!(final_item.source, "integration-dev");
    }

    #[test]
    fn task_turn_visibility_uses_authoritative_worker_lane_from_plan() {
        let task = make_task_loop_test_task("task-worker-lane-order");
        let worker_id = WorkerId::new("worker-worker-lane-order");
        let lane_id = "lane-task-worker-lane-order";
        let worker_thread_id = ThreadId::new("thread-worker-worker-lane-order");
        let orchestrator_thread_id = ThreadId::new("thread-orch-worker-lane-order");
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility = task_turn_visibility(
            &task,
            Some(lane_id),
            Some(3),
            Some(&worker_id),
            &worker_thread_id,
            &orchestrator_thread_id,
            false,
            &registry,
        );
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
        assert_eq!(item.lane_id.as_deref(), Some(lane_id));
        assert_eq!(item.lane_seq, Some(3));
        assert_eq!(item.worker_id.as_ref(), Some(&worker_id));
        assert_eq!(item.role_id.as_deref(), Some("integration-dev"));
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
                    worker_lanes: Vec::new(),
                },
            )
            .expect("turn should be stored");

        let task_store = TaskStore::new();
        let root_task_id = TaskId::new("task-root-final-root-running");
        let task_id = TaskId::new("task-action-final-root-running");
        let mut root_task = make_task_loop_test_task(root_task_id.as_str());
        root_task.kind = TaskKind::Objective;
        root_task.status = TaskStatus::Running;
        task_store.insert_task(root_task);
        let mut task = make_task_loop_test_task(task_id.as_str());
        task.root_task_id = root_task_id;
        task.status = TaskStatus::Completed;
        task_store.insert_task(task.clone());
        // 该用例验证"root 未完成时不能提前收尾主线 turn"，因此 task 本身走 Mainline 路径：
        // 不传 lane_id / worker_id，`task_turn_visibility` 会返回 Mainline，
        // 后续 append_task_final_turn_item 的 `is_mainline()` 分支才会被覆盖到。
        let orchestrator_thread_id = ThreadId::new("thread-orch-final-root-running");
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let visibility = task_turn_visibility(
            &task,
            None,
            None,
            None,
            &orchestrator_thread_id,
            &orchestrator_thread_id,
            false,
            &registry,
        );

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
    fn task_llm_loop_model_failure_writes_failed_turn_item_and_canonical_turn() {
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
                    worker_lanes: Vec::new(),
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
                "integration-dev",
                60_000,
            )
            .expect("lease should be granted");
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);
        let now = UtcMillis::now();
        let (_, orchestrator_thread_id) = session_store.ensure_session_mission(
            &session_id,
            now,
            || task.mission_id.clone(),
        );
        let thread_id = orchestrator_thread_id.clone();

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &FailingTaskModelBridgeClient,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: None,
            skill_runtime: None,
            task_store: &task_store,
            conversation_registry: &ConversationRegistry::new(),
            stream_fanout: &StreamFanOut::new(),
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
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请生成回复".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some(streaming_entry_id),
            worker_lane_id: None,
            worker_lane_seq: None,
            worker_id: None,
            thread_id: &thread_id,
            orchestrator_thread_id: &orchestrator_thread_id,
            context_summary: None,
            system_prompt: None,
            workspace_root_path: None,
        });

        match outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("model bridge unavailable"));
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
        assert!(
            error_item
                .content
                .as_deref()
                .is_some_and(|content| content.contains("model bridge unavailable"))
        );

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
            "失败终态不能再写回 legacy completed snapshot"
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
    fn task_llm_loop_read_only_shell_tools_execute_concurrently_and_preserve_order() {
        let session_store = SessionStore::new();
        let event_bus = InMemoryEventBus::new(64);
        let session_id = SessionId::new("session-task-tool-batch");
        let workspace_id = Some(WorkspaceId::new("workspace-task-tool-batch"));
        let task_id = TaskId::new("task-tool-batch");
        let worker_id = WorkerId::new("worker-task-tool-batch");
        let lane_id = "lane-task-tool-batch".to_string();
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
        let (_, orchestrator_thread_id) = session_store.ensure_session_mission(
            &session_id,
            now,
            || task.mission_id.clone(),
        );
        // worker lane 必须绑定到独立的 worker thread —— 由角色维度 ensure。
        let worker_thread_id = crate::dispatch_execution::ensure_thread_for_role(
            &session_store,
            &session_id,
            &task.mission_id,
            "integration-dev",
            &worker_id,
            &task_id,
            now,
        );
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
                    worker_lanes: vec![ActiveExecutionTurnLane {
                        lane_id: lane_id.clone(),
                        lane_seq: 2,
                        task_id: task_id.clone(),
                        worker_id: worker_id.clone(),
                        role_id: "integration-dev".to_string(),
                        thread_id: worker_thread_id.clone(),
                        title: "验证 worker 工具并发".to_string(),
                        is_primary: false,
                    }],
                    completed_at: None,
                },
            )
            .expect("turn should be creatable");

        let lease = task_store
            .grant_lease(
                &task.task_id,
                &task.root_task_id,
                &worker_id,
                "integration-dev",
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
        tool_registry.register_builtin(Arc::new(ProbeTaskBuiltinTool::new(
            "shell",
            Arc::clone(&probe),
        )));
        let client = TaskToolBatchModelBridgeClient {
            invoke_count: AtomicUsize::new(0),
        };
        let usage_binding = crate::usage_recording::session_turn_model_usage_binding(true);

        let (outcome, _) = run_task_llm_loop(TaskLlmLoopRequest {
            client: &client,
            event_bus: &event_bus,
            session_store: &session_store,
            settings_store: None,
            tool_registry: Some(&tool_registry),
            skill_runtime: None,
            task_store: &task_store,
            conversation_registry: &ConversationRegistry::new(),
            stream_fanout: &StreamFanOut::new(),
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
            task: &task,
            task_id: &task.task_id,
            lease_id: &lease.lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            prompt: "请执行两个只读 shell 工具".to_string(),
            tools: None,
            usage_binding: &usage_binding,
            streaming_entry_id: Some("timeline-streaming-task-tool-batch"),
            worker_lane_id: Some(&lane_id),
            worker_lane_seq: Some(2),
            worker_id: Some(&worker_id),
            thread_id: &worker_thread_id,
            orchestrator_thread_id: &orchestrator_thread_id,
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
                // Sidechain item 的 source_thread_id 必须切换到 worker thread；
                // 主线摘要 (`worker_status`) 再次落回 orchestrator thread。
                let routed_to_worker = item.source_thread_id == worker_thread_id
                    && item.lane_id.as_deref() == Some(lane_id.as_str())
                    && item.lane_seq == Some(2);
                let routed_to_main = item.source_thread_id == orchestrator_thread_id
                    && item.kind == "worker_status";
                routed_to_worker || routed_to_main
            }),
            "worker 输出必须沿用执行计划中的 lane 归属与顺序"
        );
        assert_eq!(
            turn.items
                .iter()
                .map(|item| (item.kind.as_str(), item.tool_call_id.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("tool_call_result", Some("task-tool-shell-a")),
                // P2：每次 tool_call_result 会追加一条 `worker_status` 摘要 item，
                // 作为主线 dispatch 卡的 liveActivity 数据源。摘要不挂 tool_call_id。
                ("worker_status", None),
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
            "worker 工具事件必须携带执行 worker，供 worker tab 和 runtime 归属使用"
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
