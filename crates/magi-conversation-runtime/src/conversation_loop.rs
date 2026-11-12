use crate::context_authority::{
    ContextAuthority, ContextCompactionMode, ContextCompactionTerminal, ContextPrepareRequest,
    current_session_file_facts, estimate_context_usage_breakdown,
};

#[cfg(test)]
use crate::context_authority::{
    ThreadHistoryCompactionDecision, thread_history_compaction_decision,
};
use crate::model_config::resolve_vision_execution_config;
use crate::model_context_window::{
    conservative_context_limit_recovery_window, resolve_model_context_window_with_override,
};
use crate::session_writeback::{
    CanonicalTurnEventSink, ContextCompactionWritebackContext, SessionStatePersistCallback,
    SessionTurnStreamPublishGate, SessionTurnStreamUpdate, apply_model_response_round,
    new_context_compaction_item_id, persist_session_state_checkpoint,
    publish_current_session_turn_item_event, publish_model_retry_runtime_event,
    publish_session_turn_item_event, publish_session_turn_item_stream_event, session_turn_item,
    session_turn_stream_update, upsert_context_compaction_completed_notice,
    upsert_context_compaction_progress_notice, upsert_session_turn_item_for_turn,
};
use crate::task_execution_registry::TaskExecutionRegistry;
use crate::task_runner_bridge::TaskOutcome;
use crate::tool_call_validation::{
    ToolCallFailureDiagnostic, ToolCallValidationTracker, invalid_tool_result_message,
    validate_tool_call_batch,
};
use crate::tool_execution_ledger::ToolExecutionLedger;
use crate::tool_result_utils::{
    DEFAULT_TOOL_RETRY_LIMIT, MAX_POLICY_REJECTIONS_PER_TASK, emergency_model_round_failure,
    infer_tool_call_status, non_retryable_tool_failure, summarize_tool_result,
    turn_item_status_for_tool_result,
};
use crate::tool_surface_state::{
    BrowserToolSurfaceContext, RefreshLiveExternalToolDefinitionsInput,
    activated_skill_id_from_tool_result, build_browser_tool_surface,
    constrain_task_tool_definitions, refresh_live_external_tool_definitions,
};
use crate::{
    ConversationRegistry, GoalModeLifecycleState, MailboxAuthor, RuntimeSignal, TaskSignalBoundary,
    TaskToolBatchEnv, TaskTurnVisibility, apply_task_final_visibility, apply_task_turn_visibility,
    apply_task_worker_detail_visibility, canonical_tool_call_name, compact_validation_failure,
    execute_task_tool_call_batch, forced_task_tool_choice_for_round, goal_mode_required_tool_chain,
    goal_mode_requires_terminalization, goal_mode_tool_batch_violation,
    record_completed_required_tools, required_tool_chain_is_complete,
    required_tool_chain_recovery_prompt, required_tool_definitions_for_round,
    strict_goal_mode_tool_definitions_for_round, task_required_tool_chain, task_turn_visibility,
    validation_result_rejects_delivery,
};
use crate::{
    model_error::{
        MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS,
        MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS, ModelFailureDiagnostic,
        classify_model_invocation_error, model_empty_response_recovery_prompt,
        model_stream_interruption_recovery_prompt,
    },
    prompt_utils::{
        PromptFragmentKind, current_access_profile_prompt, current_agent_mode_prompt,
        current_turn_context_priority_prompt, dynamic_skill_prompt_message,
        normalize_model_visible_content, project_execution_context_system_prompt,
        runtime_context_message, skill_prompt_message, system_prompt_fragment_message,
    },
    session_images::{SessionTurnImage, session_turn_image_sources},
    usage_recording::{
        ContextUsageRuntimeTracker, ContextUsageRuntimeTrackerInput, ModelUsageBinding,
        publish_model_usage_record_for_turn,
        publish_model_usage_record_for_turn_with_context_window,
        resolved_main_source_id_for_usage_binding, resolved_model_for_usage_binding,
        resolved_provider_for_usage_binding, vision_model_usage_binding,
    },
};
use magi_bridge_client::{
    BridgeClientError, ChatMessage, ChatToolCall, ChatToolDefinition, ModelBridgeClient,
    ModelInvocationRequest, ModelProviderContext, ModelResponseStatus, ModelStreamingDelta,
};
#[cfg(test)]
use magi_core::UtcMillis;
use magi_core::{
    EventId, ExecutionResultStatus, LeaseId, SessionId, Task, TaskCompletionAttempt,
    TaskCompletionEvidence, TaskEvidenceRequirement, TaskId, TaskStatus, ThreadId, WorkspaceId,
    public_runtime_excerpt,
};
#[cfg(test)]
use magi_event_bus::SessionRuntimeUsageObservation;
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_orchestrator::{ExecutionContextSummary, task_store::TaskStore};
use magi_session_store::{SessionStore, ThreadChatMessage};
#[cfg(test)]
use magi_session_store::{ThreadChatToolCall, ThreadChatToolFunction};
use magi_settings_store::SettingsStore;
use magi_tool_runtime::{ToolExecutionProgress, ToolRegistry};
use magi_usage_authority::UsageCallStatus;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

mod thread_history;
pub(crate) use thread_history::thread_chat_message_to_chat_message;
use thread_history::*;
mod signals;
use signals::*;
mod turn_items;
use turn_items::*;

/// 模型在完成合同未满足时给出最终答复后，最多追加的纠正提示次数（各类纠正共享）。
const COMPLETION_RECOVERY_MAX_ATTEMPTS: usize = 4;

pub struct PreparedWorkspaceExecution {
    pub task: Task,
    pub root: Option<PathBuf>,
    pub resources: crate::workspace_execution::WorkspaceExecutionResources,
}

pub type PrepareWorkspace<'a> = dyn Fn() -> Result<PreparedWorkspaceExecution, String> + Sync + 'a;
pub type RefreshToolDefinitions<'a> = dyn Fn(Option<&str>) -> Vec<ChatToolDefinition> + Sync + 'a;

pub struct ConversationLoopRequest<'a> {
    pub client: &'a dyn ModelBridgeClient,
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub settings_store: Option<&'a Arc<SettingsStore>>,
    pub live_settings_store: Option<&'a Arc<SettingsStore>>,
    pub tool_registry: Option<&'a ToolRegistry>,
    pub skill_runtime: Option<&'a magi_skill_runtime::SkillRuntime>,
    pub skill_dispatch_runtime: Option<&'a magi_skill_runtime::SkillDispatchRuntime>,
    pub skill_name: Option<String>,
    pub task_store: &'a TaskStore,
    pub execution_registry: &'a TaskExecutionRegistry,
    /// 主线和子代理共用的输入及交互入口；执行资格由原 Task/lease 和 canonical Turn 校验。
    pub conversation_registry: &'a ConversationRegistry,
    /// 任务系统 — AgentRole 注册表。task_turn_visibility 解析 role_id 时
    /// 必须走该注册表，不再依赖硬编码的 kind→role 默认 mapping。
    pub agent_role_registry: &'a magi_agent_role::AgentRoleRegistry,
    /// 任务系统 — L5：父子任务拓扑图。S7 协调工具（agent_spawn）
    /// 在 execute_task_tool_call 中拦截时操作此结构。
    /// 任务系统 — L12：本次轮次的 SafetyGate 快照。`None` 表示当前没有
    /// 启用任何危险模式规则（既无内置也无用户自定义），此时拦截器走 pass-through。
    /// 在 execute_task_tool_call 中工具调用执行前做语义判定。
    pub safety_gate: Option<&'a magi_safety_gate::SafetyGate>,
    /// 任务系统 — L13：当前 session 的 PlanStore。模型通过 `update_plan`
    /// 工具往里写分解 + 进度；本 Turn 起始时把快照渲染成 system prompt 注入。
    pub plan_store: &'a magi_plan::PlanStore,
    /// 任务系统 — L14：当前 workspace 的 ProjectMemory。`None` 表示当前 task
    /// 不绑定 workspace（极少数 orchestration-only 场景），此时不注入 prompt、
    /// 也不允许 `memory_write` 工具调用成功。
    pub project_memory: Option<&'a magi_project_memory::ProjectMemoryStore>,
    pub task: &'a magi_core::Task,
    pub task_id: &'a TaskId,
    pub lease_id: &'a LeaseId,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub prompt: String,
    /// 本次执行的引用和任务事实，与用户正文分开装配，不写入对话 transcript。
    pub task_context_messages: Vec<ChatMessage>,
    pub images: Vec<SessionTurnImage>,
    pub tools: Option<Vec<ChatToolDefinition>>,
    /// 每个新 Step 通过原派发目录重建；None 只消费调用者传入的固定工具面。
    pub refresh_tool_definitions: Option<&'a RefreshToolDefinitions<'a>>,
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
    /// 当前 session 的文件变更账本。任务系统 的主线/代理工具写入都必须通过
    /// 同一个 SnapshotSession 记录，才能把文件变更归因到主线或具体代理 worker。
    pub workspace_resources: Option<crate::workspace_execution::WorkspaceExecutionResources>,
    pub prepare_workspace: Option<&'a PrepareWorkspace<'a>>,
    pub execution_group_id: Option<String>,
    pub persist_session_state: Option<&'a SessionStatePersistCallback>,
    /// 扩展钩子端口（`tool.call` / `tool.result`）；None 表示没有可执行扩展。
    pub extension_hooks: Option<&'a dyn crate::extension_hooks::ExtensionHooks>,
}

fn direct_runtime_error(error: &BridgeClientError, default_message: &str) -> String {
    let mut raw_error = error.to_string();
    if let Some(code) = error.code() {
        raw_error.push_str(&format!(" (error_code={code})"));
    }
    let detail = public_runtime_excerpt(&raw_error, 4096);
    if detail.trim().is_empty() {
        default_message.to_string()
    } else {
        detail
    }
}

pub(crate) struct TaskCommandCompletion<'a> {
    pub event_bus: &'a InMemoryEventBus,
    pub session_store: &'a SessionStore,
    pub task_store: &'a TaskStore,
    pub task: &'a Task,
    pub session_id: &'a SessionId,
    pub workspace_id: &'a Option<WorkspaceId>,
    pub visibility: &'a TaskTurnVisibility,
    pub turn_id: &'a str,
    pub persist: Option<&'a SessionStatePersistCallback>,
    pub content: &'a str,
}

/// 无模型命令与模型执行共用完成契约校验和 canonical 最终结果提交。
pub(crate) fn complete_task_command(
    input: TaskCommandCompletion<'_>,
) -> Result<TaskOutcome, String> {
    let outcome = validated_task_completion(
        input.task,
        vec![input.content.to_owned()],
        input.content.to_owned(),
        vec![],
    )?;
    append_task_final_turn_item(
        TaskTurnWritebackContext {
            event_bus: input.event_bus,
            session_store: input.session_store,
            task_store: input.task_store,
            task: input.task,
            session_id: input.session_id,
            workspace_id: input.workspace_id,
            turn_visibility: input.visibility,
            persist_session_state: input.persist,
            expected_turn_id: Some(input.turn_id),
            model_step_id: None,
            writeback_error: &std::sync::Mutex::new(None),
        },
        input.content,
        None,
        None,
        None,
        &[],
        &[],
    )?;
    Ok(outcome)
}

fn validated_task_completion(
    task: &Task,
    output_refs: Vec<String>,
    final_response: String,
    evidence: Vec<TaskCompletionEvidence>,
) -> Result<TaskOutcome, String> {
    let attempt = TaskCompletionAttempt {
        output_refs,
        final_response: Some(final_response),
        evidence,
    };
    task.completion_contract().validate(&attempt)?;
    Ok(TaskOutcome::Completed { attempt })
}

fn task_round_tool_definitions(
    active_tools: &[ChatToolDefinition],
    required_tool_chain: &[String],
    completed_required_tool_names: &[String],
    constrain_to_recovery_tool: bool,
) -> Vec<ChatToolDefinition> {
    if constrain_to_recovery_tool {
        required_tool_definitions_for_round(
            active_tools,
            required_tool_chain,
            completed_required_tool_names,
        )
    } else {
        active_tools.to_vec()
    }
}

pub(crate) fn run_conversation_loop(
    request: ConversationLoopRequest<'_>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    // Runner 的准入和 TaskStore 租约负责单次派发；这里直接进入共同 Step 循环，
    // 不再用第二套内存 Turn 状态包住整段 IO。注册前投递的初始信号保留在同一 FIFO。
    request
        .conversation_registry
        .open_task_signal_channel(request.session_id, request.task_id);
    let _signals = TaskSignalScope {
        registry: request.conversation_registry,
        session_id: request.session_id,
        task_id: request.task_id,
    };
    let initial_signals = request
        .conversation_registry
        .drain_task_signals(request.session_id, request.task_id);
    run_conversation_loop_inner(request, initial_signals)
}

fn build_task_context_base_messages(
    static_messages: &[ChatMessage],
    project_memory: Option<&magi_project_memory::ProjectMemoryStore>,
    memory_write_visible: bool,
    plan_store: &magi_plan::PlanStore,
    include_plan: bool,
    mailbox_prompt: Option<&str>,
) -> Vec<ChatMessage> {
    let mut messages = static_messages.to_vec();
    if let Some(store) = project_memory {
        let rendered = if memory_write_visible {
            store.render_for_prompt()
        } else {
            store.render_for_prompt_read_only()
        };
        match rendered {
            Ok(Some(rendered)) => messages.push(system_prompt_fragment_message(
                PromptFragmentKind::ProjectMemory,
                rendered,
            )),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "ProjectMemory: 渲染 prompt 失败，本轮跳过");
            }
        }
    }
    if include_plan && let Some(rendered) = plan_store.render_for_prompt() {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::UserPlan,
            rendered,
        ));
    }
    if let Some(mailbox_prompt) = mailbox_prompt {
        messages.push(system_prompt_fragment_message(
            PromptFragmentKind::Mailbox,
            mailbox_prompt,
        ));
    }
    messages
}

/// 运行时规则独立于任务正文，始终位于 transcript 前；下一轮替换同一片段，
/// 不把已撤销的工具能力留在历史中。压缩重建也经过此入口。
fn refresh_current_agent_mode(
    messages: &mut Vec<ChatMessage>,
    task: &Task,
    roles: &magi_agent_role::AgentRoleRegistry,
    limits: &crate::execution_admission::ExecutionAdmissionLimits,
    tools: &[ChatToolDefinition],
    model_capabilities: Option<&str>,
) {
    let kind = PromptFragmentKind::CurrentAgentMode;
    let prefix = format!("<magi-developer-fragment id=\"{}\"", kind.as_str());
    messages.retain(|message| {
        !(message.context_origin == magi_bridge_client::ChatMessageOrigin::SystemInstruction
            && message.role == kind.role()
            && message
                .content
                .as_deref()
                .is_some_and(|text| text.starts_with(&prefix)))
    });
    if let Some(mut prompt) = current_agent_mode_prompt(task, limits, roles, tools) {
        if tools.iter().any(|tool| tool.function.name == "agent_spawn")
            && let Some(capabilities) = model_capabilities
        {
            prompt.push_str("\n\n");
            prompt.push_str(capabilities);
        }
        if tools
            .iter()
            .any(|tool| tool.function.name == "update_execution_steps")
        {
            prompt.push_str(
                "\n\n# 执行步骤（update_execution_steps）\n\
                 默认不用。只有确认任务需要多阶段的实际修改、构建验证或委派子任务，且用户需要看到进度时，才在第一次执行这些操作前提交 3–6 个步骤；此前的只读探索不算步骤，探索之后再决定是否需要。\n\
                 不用：问答、解释、只读的查看/分析/总结（如“了解项目”“解释这段代码”）、单步或几次工具调用就能完成的操作、探索性检索。拿不准就不用。\n\
                 用法：每次提交完整步骤列表并沿用 stepId；标题用用户的语言，每条一个简短动作。把正在做的步骤设为 running（可与它的第一个工具调用放在同一轮），成功执行后设为 completed；失败、阻塞、取消须写 reason。完成由服务端按该步骤运行期间成功的工具调用确认，没有版本号和证据参数。步骤不能代替实际工作，也不要在正文里复述步骤列表。最终答复时仍未收口的步骤由系统结算，无需为此额外调用。",
            );
        }
        let position = messages
            .iter()
            .position(|message| !matches!(message.role.as_str(), "system" | "developer"))
            .unwrap_or(messages.len());
        messages.insert(position, system_prompt_fragment_message(kind, prompt));
    }
}

/// 原生主任务和子代理共同的模型/工具 Step 循环。
fn run_conversation_loop_inner(
    request: ConversationLoopRequest<'_>,
    initial_signals: Vec<RuntimeSignal>,
) -> (TaskOutcome, Option<ExecutionContextSummary>) {
    let ConversationLoopRequest {
        client,
        event_bus,
        session_store,
        settings_store,
        live_settings_store,
        tool_registry,
        skill_runtime,
        skill_dispatch_runtime,
        skill_name,
        task_store,
        execution_registry,
        conversation_registry,
        agent_role_registry,
        safety_gate,
        plan_store,
        project_memory,
        task,
        task_id,
        lease_id,
        session_id,
        workspace_id,
        prompt,
        task_context_messages,
        images,
        tools,
        refresh_tool_definitions,
        usage_binding,
        streaming_entry_id,
        is_sidechain,
        worker_id,
        thread_id,
        context_summary,
        system_prompt,
        mut workspace_root_path,
        mut workspace_resources,
        prepare_workspace,
        execution_group_id,
        persist_session_state,
        extension_hooks,
    } = request;
    let execution_guard = crate::task_execution_guard::TaskExecutionGuard::new(
        task_store,
        session_store,
        execution_registry,
        session_id,
        task_id,
        lease_id,
    );
    let expected_turn_id = execution_registry.turn_id(task_id);
    // 恢复同一主线任务时也重新开放输入边界；子代理不能取得 Session 引导。
    if !is_sidechain
        && let Some(turn_id) = expected_turn_id.as_deref()
        && conversation_registry
            .turn_coordinator()
            .current_attempt(session_id, turn_id)
            .is_ok()
    {
        match conversation_registry
            .turn_coordinator()
            .begin_session_turn_input(session_id.clone(), turn_id.to_string())
        {
            Ok(()) => {}
            Err(crate::SessionTurnInputError::AlreadyActive { active_turn_id })
                if active_turn_id == turn_id => {}
            Err(error) => {
                return (
                    TaskOutcome::Failed {
                        error: format!("建立任务引导输入边界失败: {error}"),
                    },
                    context_summary,
                );
            }
        }
    }
    tracing::info!(
        target: "magi.performance",
        session_id = %session_id,
        task_id = %task_id,
        turn_id = expected_turn_id.as_deref().unwrap_or_default(),
        provider_call_id = "",
        stage = "runner_started",
        elapsed_ms = 0u64,
        "conversation response timing"
    );
    let first_provider_delta_reported = std::cell::Cell::new(false);
    let first_provider_raw_delta_reported = std::cell::Cell::new(false);

    let mut static_context_messages = Vec::new();
    // ===================================================================
    // Prompt 装配 · 缓存边界
    // -------------------------------------------------------------------
    // S-ID 是逻辑标识（外部 dispatcher / docs 交叉引用稳定），下方
    // **emission order 按 LLM prompt 缓存友好度重排**：STATIC → SEMI-STATIC
    // → DYNAMIC。任何一段 DYNAMIC 内容变化都会让其下方所有消息的缓存键失
    // 效，因此越静态的段越往前推。修改本块时务必保持这个分层不变。
    //
    //   Tier A · STATIC      —— 同一角色 / workspace / mission 多轮内不变
    //     S1   角色 / agent role 系统提示  (assemble_prompt 上游产出)
    //     S8b  Workspace 根目录上下文
    //
    //   Tier B · SEMI-STATIC —— 同一项目跨轮通常稳定，偶有更新
    //     S10  ProjectMemory 索引
    //
    //   Tier C · DYNAMIC     —— 每轮都可能变化
    //     S9   PlanStore 快照
    //     Mailbox 待处理消息
    //     Thread 历史 (append-only — 前缀稳定，append 不破前缀缓存)
    //     本轮 user 输入（只含当前任务正文）
    //
    // 上游 task_execution_dispatcher::assemble_prompt 将当前任务事实与参考数据分开装配，用户正文放进 `prompt`，将角色、用户规则和安全规则放进 `system_prompt`。
    //   S1 → system_prompt（本函数首条 Role developer fragment）
    //   S2 base task goal / title → prompt 用户消息
    //   S3 上下文摘要 (knowledge / memory / shared_context) → 独立引用消息
    //   S4 task_fact_context → 独立任务事实消息
    //   S5 skill prompt injections → 独立的 Skill developer 消息
    //   S6 用户规则 (settings.userRules) → system_prompt developer 前缀
    //   S7 安全规则 / S8 SafetyGate 危险模式 → system_prompt developer 前缀
    // 当前任务事实保持 user role 以保留本轮输入边界；运行时安全与授权规则保持
    // developer role，二者不能通过把所有内容拼成一条 system 字符串来混淆。
    // ===================================================================

    // -------- Tier A · STATIC --------
    // [CACHE: STATIC] S1 · 角色 / agent role 系统提示。
    if let Some(system) = system_prompt {
        static_context_messages.push(system_prompt_fragment_message(
            PromptFragmentKind::Role,
            system,
        ));
    }
    if let Some(skill_message) = skill_runtime.and_then(|runtime| {
        skill_name
            .as_deref()
            .and_then(|id| crate::prompt_utils::skill_prompt_message(runtime, id))
    }) {
        static_context_messages.push(skill_message);
    }
    // 执行 cwd 与项目归属分别由 daemon 提供，个人目录不能被描述成项目。
    let project_rules_root = project_memory.and_then(|store| store.root().parent());
    let workspace_rules = match crate::workspace_rules::render_workspace_rules(
        project_rules_root,
        workspace_resources
            .as_ref()
            .and_then(|resources| resources.directories.as_ref()),
    ) {
        Ok(rules) => rules,
        Err(error) => return (TaskOutcome::Failed { error }, context_summary),
    };
    static_context_messages.push(system_prompt_fragment_message(
        PromptFragmentKind::WorkspaceContext,
        format!(
            "{}{}",
            project_execution_context_system_prompt(
                workspace_root_path.as_ref().and_then(|root| root.to_str()),
                workspace_id.is_some(),
                workspace_resources
                    .as_ref()
                    .and_then(|resources| resources.directories.as_ref()),
            ),
            workspace_rules
        ),
    ));
    // [CACHE: STATIC] 扩展 `prompt.contribute`（阶段 Start）：任务开始时征询一次，稳定片段随后冻结进
    // 前缀，之后的每个 Step 不再变化，因此不会破坏缓存；易变片段只在每轮尾部出现。
    if let Some(hooks) = extension_hooks {
        static_context_messages.extend(crate::extension_hooks::contribute_prompt(
            hooks,
            &crate::extension_hooks::ToolHookScope {
                session_id: session_id.as_str(),
                task_id: task_id.as_str(),
                workspace_id: workspace_id.as_ref().map(|id| id.as_str()),
                worker_id: worker_id.map(|id| id.as_str()),
                round: 0,
            },
            crate::extension_hooks::ContributePhase::Start,
        ));
    }
    let memory_write_visible = tools.as_ref().is_some_and(|definitions| {
        definitions
            .iter()
            .any(|definition| definition.function.name == "memory_write")
    });
    // Mailbox 是 Task 生命周期内的运行时投递视图，不能伪装成已持久化的 transcript。
    let runtime_signals = std::cell::RefCell::new(initial_signals);
    let is_primary_execution = !is_sidechain && task.parent_task_id.is_none();
    let can_observe_plan = || {
        session_store
            .plan_for_execution_observer(session_id, task_id.as_str())
            .is_some()
    };
    let owns_active_plan = || {
        is_primary_execution
            && session_store
                .active_plan_for_execution_owner(session_id, task_id.as_str())
                .is_some()
    };
    let current_access_profile = task
        .policy_snapshot
        .as_ref()
        .map(magi_core::TaskPolicy::effective_access_profile)
        .unwrap_or_default();
    let current_command_mode = task
        .policy_snapshot
        .as_ref()
        .map(|policy| policy.command_mode.as_str())
        .unwrap_or("full");
    let mut messages = static_context_messages.clone();
    messages.push(system_prompt_fragment_message(
        PromptFragmentKind::CurrentAccessProfile,
        current_access_profile_prompt(current_access_profile, current_command_mode),
    ));
    messages.push(system_prompt_fragment_message(
        PromptFragmentKind::CurrentTurnPriority,
        current_turn_context_priority_prompt(),
    ));
    let agent_model_capabilities = crate::prompt_utils::agent_model_capabilities_prompt(
        settings_store.map(AsRef::as_ref),
        task,
        session_id,
        agent_role_registry,
    );
    refresh_current_agent_mode(
        &mut messages,
        task,
        agent_role_registry,
        execution_registry.execution_admission().limits(),
        tools.as_deref().unwrap_or_default(),
        Some(&agent_model_capabilities),
    );
    // 主线 branch 跨 task/Turn，当前任务引用属于尾部；worker branch 只拥有一个 task，
    // 冻结的交接引用在该 branch 生命周期内稳定，可以放在历史前。计划、邮箱和
    // 非持久历史说明对所有分支都属于易变尾部，不进入 transcript。
    let stable_task_context = if is_sidechain {
        task_context_messages.as_slice()
    } else {
        &[]
    };
    let round_task_context = if is_sidechain {
        &[]
    } else {
        task_context_messages.as_slice()
    };
    // 历史边界说明只在回合开始或压缩重建时才存在，不会写进 thread 历史；若插在历史与当前
    // 输入之间，下一回合重建历史时它消失，上一回合的整段内容都会错位而无法复用缓存。
    // 因此所有分支把它放在易变尾部，位置随每次请求稳定在当前输入之后。
    let mut request_controls = crate::request_controls::RequestControls::default();
    let history_boundary_note = std::cell::Cell::new(None::<&'static str>);
    // 最近一次请求视图对已有结果的改写量，随 Step 一起冻结，用于缓存断代归因。
    let history_projection =
        std::cell::RefCell::new(magi_session_store::HistoryProjection::default());
    let contribution_memo = crate::extension_hooks::ContributionMemo::default();
    let current_round = std::cell::Cell::new(0usize);
    let volatile_context_tail = || -> Vec<ChatMessage> {
        let mut tail = Vec::new();
        let pending_approval_ids = conversation_registry
            .tool_approvals()
            .pending_for_session(session_id)
            .into_iter()
            .map(|approval| approval.approval_id)
            .collect::<std::collections::HashSet<_>>();
        let mailbox_prompt =
            render_task_signals_for_prompt(&runtime_signals.borrow(), &pending_approval_ids);
        if let Some(note) = history_boundary_note.get() {
            tail.push(system_prompt_fragment_message(
                PromptFragmentKind::ThreadHistoryBoundary,
                note,
            ));
        }
        tail.extend(build_task_context_base_messages(
            round_task_context,
            None,
            memory_write_visible,
            plan_store,
            can_observe_plan(),
            mailbox_prompt.as_deref(),
        ));
        // 扩展 `prompt.contribute`（阶段 Round）：只接受易变片段，位于每轮尾部（RoundContext），
        // 不进入历史链与缓存前缀。
        if let Some(hooks) = extension_hooks {
            tail.extend(contribution_memo.round(
                hooks,
                &crate::extension_hooks::ToolHookScope {
                    session_id: session_id.as_str(),
                    task_id: task_id.as_str(),
                    workspace_id: workspace_id.as_ref().map(|id| id.as_str()),
                    worker_id: worker_id.map(|id| id.as_str()),
                    round: current_round.get(),
                },
            ));
        }
        tail
    };
    messages.extend(build_task_context_base_messages(
        stable_task_context,
        project_memory,
        memory_write_visible,
        plan_store,
        false,
        None,
    ));
    // [CACHE: APPEND-ONLY] Runtime tail · Thread 历史。
    // worker thread 为单 task 独占；历史由 canonical 投影，不能注入同 role 的其他任务。
    // 上下文权威层只生成「摘要 + 最近完整消息」模型视图，不改变 canonical 事实。
    let resumed_task =
        task_is_resuming_existing_thread(session_store, session_id, task_id, thread_id);
    let mut current_user_messages: Vec<_> = (!resumed_task)
        .then(|| ChatMessage {
            input_request_id: (!is_sidechain)
                .then_some(expected_turn_id.as_deref())
                .flatten()
                .and_then(|turn_id| {
                    crate::model_step::root_input_request_id(session_store, session_id, turn_id)
                }),
            context_origin: Default::default(),
            role: "user".to_string(),
            content: Some(prompt.clone()),
            images: session_turn_image_sources(&images),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        })
        .into_iter()
        .collect();
    if !resumed_task
        && !is_sidechain
        && let Some(turn) = expected_turn_id.as_deref().and_then(|turn_id| {
            session_store.canonical_turn_for_session_turn_id(session_id, turn_id)
        })
    {
        for item in turn.items.iter().filter(|item| {
            item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage
                && item.metadata.contains_key("pluginMessage")
        }) {
            let request_id = item
                .metadata
                .get("requestId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            if let Some(root) = current_user_messages
                .iter_mut()
                .find(|message| message.input_request_id == request_id)
            {
                root.content = item.model_input_content();
            } else {
                current_user_messages.push(ChatMessage {
                    input_request_id: request_id,
                    content: item.model_input_content(),
                    context_origin: Default::default(),
                    role: "user".into(),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                });
            }
        }
    }
    let mut additional_context = estimate_context_usage_breakdown(&messages, tools.as_deref());
    additional_context.merge(&estimate_context_usage_breakdown(
        &current_user_messages,
        None,
    ));
    additional_context.merge(&estimate_context_usage_breakdown(
        &volatile_context_tail(),
        None,
    ));
    let selected_context_model = resolved_model_for_usage_binding(
        settings_store.or(live_settings_store),
        usage_binding,
        session_id,
    )
    .unwrap_or_default();
    let selected_main_source_id = resolved_main_source_id_for_usage_binding(
        settings_store.or(live_settings_store),
        usage_binding,
        session_id,
    );
    let current_turn_contains_images = !images.is_empty();
    let vision_execution_config = match resolve_vision_execution_config(
        settings_store.or(live_settings_store).map(Arc::as_ref),
        selected_main_source_id.as_deref(),
        &selected_context_model,
        current_turn_contains_images,
    ) {
        Ok(config) => config,
        Err(error) => {
            return (TaskOutcome::Failed { error }, context_summary);
        }
    };
    let vision_client = vision_execution_config
        .as_ref()
        .and_then(|config| config.to_http_vision_client());
    let client: &dyn ModelBridgeClient = vision_client
        .as_ref()
        .map(|client| client as &dyn ModelBridgeClient)
        .unwrap_or(client);
    let vision_usage_binding = vision_execution_config
        .as_ref()
        .map(|_| vision_model_usage_binding());
    let usage_binding = vision_usage_binding.as_ref().unwrap_or(usage_binding);
    let resolved_context_model = vision_execution_config
        .as_ref()
        .and_then(|config| config.require_model().ok())
        .unwrap_or(&selected_context_model)
        .to_string();
    let context_settings = settings_store.or(live_settings_store).map(Arc::as_ref);
    let configured_context_window = vision_execution_config
        .as_ref()
        .and_then(|config| config.context_window_tokens())
        .or_else(|| {
            task.executor_binding
                .as_ref()
                .and_then(|binding| binding.model_selection.as_ref())
                .and_then(|selection| selection.context_window_tokens)
        });
    let mut effective_context_window = if vision_execution_config.is_none()
        && let Some(source_id) = selected_main_source_id.as_deref()
    {
        crate::model_context_window::resolve_main_source_model_context_window(
            context_settings,
            source_id,
            &resolved_context_model,
            configured_context_window,
        )
    } else {
        resolve_model_context_window_with_override(
            // 与模型 client、vision 配置和 usage binding 使用同一份已接纳的执行快照。
            // 不能在 Turn 执行中改用 live settings，否则模型仍是旧配置而窗口可能
            // 来自新配置，/compact 会按错误的分母规划历史。
            context_settings,
            &resolved_context_model,
            configured_context_window,
        )
    };
    let initial_skill_name = skill_name.clone();
    let turn_visibility = task_turn_visibility(
        task,
        is_sidechain,
        worker_id,
        thread_id,
        agent_role_registry,
    );
    let writeback_error = std::sync::Mutex::new(None::<String>);
    let turn_writeback_context = TaskTurnWritebackContext {
        model_step_id: None,
        event_bus,
        session_store,
        task_store,
        task,
        session_id,
        workspace_id,
        turn_visibility: &turn_visibility,
        persist_session_state,
        expected_turn_id: expected_turn_id.as_deref(),
        writeback_error: &writeback_error,
    };
    let prepare_task_history = |phase: &'static str,
                                context_window: u64,
                                additional_context: magi_usage_authority::ContextUsageBreakdown,
                                mode: ContextCompactionMode| {
        let compaction_item_id = new_context_compaction_item_id(task_id.as_str(), thread_id, phase);
        let compaction_writeback = ContextCompactionWritebackContext {
            event_bus,
            session_store,
            session_id,
            workspace_id,
            thread_id,
            item_id: &compaction_item_id,
            phase,
            persist_session_state,
            task: Some(task),
            turn_visibility: Some(&turn_visibility),
            expected_turn_id: expected_turn_id.as_deref(),
        };
        let compaction_observer =
            |progress| upsert_context_compaction_progress_notice(compaction_writeback, progress);
        let compaction_cancelled = || !execution_guard.is_active();
        let mut prepared = ContextAuthority::new(
            client,
            event_bus,
            session_store,
            session_id,
            workspace_id,
            thread_id,
            settings_store,
        )
        .with_expected_turn_id(expected_turn_id.as_deref())
        .with_task_id(task_id)
        .with_extension_hooks(extension_hooks)
        .with_tool_scope(crate::extension_hooks::ToolHookScope {
            session_id: session_id.as_str(),
            task_id: task_id.as_str(),
            workspace_id: workspace_id.as_ref().map(|id| id.as_str()),
            worker_id: worker_id.map(|id| id.as_str()),
            round: current_round.get(),
        })
        .with_compaction_runtime(&compaction_observer, &compaction_cancelled)
        .prepare(ContextPrepareRequest {
            phase,
            context_window_tokens: context_window,
            additional_context,
            persist_checkpoint: vision_execution_config.is_none(),
            model_identity: Some(magi_usage_authority::ModelIdentitySnapshot::new(
                vision_execution_config
                    .as_ref()
                    .map(|_| "vision")
                    .unwrap_or("configured"),
                resolved_context_model.clone(),
                0,
            )),
            mode,
        });
        if let Some(compaction) = prepared.compaction.as_ref()
            && let Err(error) =
                upsert_context_compaction_completed_notice(compaction_writeback, compaction)
        {
            tracing::error!(
                %task_id,
                phase,
                %error,
                "任务上下文压缩完成事实写回失败"
            );
            prepared.compaction = None;
            prepared.terminal = Some(ContextCompactionTerminal::Failed);
        }
        prepared
    };
    let initial_prepared_history = prepare_task_history(
        "pre_turn",
        effective_context_window,
        additional_context.clone(),
        ContextCompactionMode::Automatic,
    );
    if let Some(terminal) = initial_prepared_history.terminal {
        return (
            task_context_compaction_terminal_outcome_or_failed(
                turn_writeback_context,
                terminal,
                streaming_entry_id,
            ),
            context_summary,
        );
    }
    let mut history_generation = initial_prepared_history.checkpoint_generation;
    let thread_history_snapshot = initial_prepared_history.messages;
    let routed_history = thread_history_snapshot
        .iter()
        // 当前输入在 canonical 中已接纳，实际正文和图片由本轮消息在末尾装配一次。
        .filter(|message| {
            !message.input_request_id.as_ref().is_some_and(|id| {
                current_user_messages
                    .iter()
                    .any(|current| current.input_request_id.as_ref() == Some(id))
            })
        })
        .map(thread_chat_message_to_chat_message)
        .collect::<Vec<_>>();
    if !routed_history.is_empty() {
        messages.extend(routed_history);
        const PERSISTED_HISTORY_NOTE: &str = "本轮请求中的历史消息是当前 task 已持久化的运行记录，其中最近一条用户输入是本轮当前任务。它们是恢复后的工作事实：已完成的工具结果必须直接继承，不要从头重复；结果未知的外部操作必须先检查当前状态。历史与当前任务冲突时，以当前任务为准。";
        history_boundary_note.set(Some(PERSISTED_HISTORY_NOTE));
    }
    // [CACHE: DYNAMIC] Runtime tail · 本轮 user 输入。
    // 新 task 首次启动才追加该输入；恢复 runner 必须复用 thread 中已持久化的原始
    // 用户消息（包括图片），不能把同一任务再次作为一轮全新输入发送给模型。
    for current_user_message in current_user_messages {
        if let Err(error) = persist_task_input(turn_writeback_context, &current_user_message) {
            return (TaskOutcome::Failed { error }, context_summary);
        }
        messages.push(current_user_message);
        if let Err(error) =
            persist_session_state_checkpoint(persist_session_state, "task_thread_user_input")
        {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &format!("任务用户消息持久化失败：{error}"),
                streaming_entry_id,
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
    }
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
    let mut final_model_round: Option<usize> = None;
    let mut final_model_step_id = None;
    let mut final_provider_context = Vec::new();
    let mut active_skill_name = skill_name;
    let native_tools_enabled = tools.is_some() || refresh_tool_definitions.is_some();
    let mut active_tools = tools.unwrap_or_default();
    let mut loaded_external_tool_names = BTreeSet::new();
    let mut tool_call_validation_tracker = ToolCallValidationTracker::default();
    // 本任务累计被策略拒绝（相同调用不可重试）的次数；达到上限才把拒绝升级为任务失败。
    let mut policy_rejection_count = 0usize;
    let recovered_tools = match ToolExecutionLedger::from_canonical_task(
        session_store,
        task_store,
        session_id,
        task,
        thread_id,
        tool_registry,
    ) {
        Ok(ledger) => ledger,
        Err(error) => {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &error,
                streaming_entry_id,
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
    };
    let mut tool_call_records = tool_call_records_from_thread_history(&recovered_tools.history);
    let mut plugin_completions =
        match crate::plugin_tool_completion::recover(&recovered_tools.history) {
            Ok(completions) => completions,
            Err(error) => {
                let error = task_failure_with_error_item(
                    turn_writeback_context,
                    &error,
                    streaming_entry_id,
                    None,
                    None,
                );
                return (TaskOutcome::Failed { error }, context_summary);
            }
        };
    let mut tool_execution_ledger = recovered_tools.ledger.with_current_file_facts(
        &current_session_file_facts(session_store, session_id),
        workspace_root_path.as_deref(),
    );
    let declared_required_tool_chain = task_required_tool_chain(task);
    let required_evidence_tools = task_required_evidence_tools(task);
    let historical_completion_evidence =
        successful_tool_evidence_from_thread_history(&recovered_tools.history);
    let historical_completed_tool_names = historical_completion_evidence
        .iter()
        .filter_map(|evidence| match evidence {
            TaskCompletionEvidence::SuccessfulToolCall { tool_name, .. } => Some(tool_name.clone()),
            TaskCompletionEvidence::ExternalExecutor { .. } => None,
        })
        .collect::<Vec<_>>();
    let mut goal_creation_required = task.is_goal_mode()
        && session_store.current_unfinished_goal(session_id).is_none()
        && !historical_completed_tool_names
            .iter()
            .any(|tool_name| tool_name == "create_goal");
    let mut required_tool_chain = if task.is_goal_mode() {
        goal_mode_required_tool_chain(
            GoalModeLifecycleState {
                goal_creation_required,
                goal_terminalization_required: goal_mode_requires_terminalization(
                    session_store,
                    session_id,
                ),
            },
            &declared_required_tool_chain,
        )
    } else {
        declared_required_tool_chain.clone()
    };
    let mut completion_evidence = historical_completion_evidence;
    let mut completed_required_tool_names = historical_completed_tool_names
        .iter()
        .filter(|tool_name| required_tool_chain.contains(tool_name))
        .cloned()
        .collect::<Vec<_>>();
    let mut completed_evidence_tool_names = historical_completed_tool_names
        .iter()
        .filter(|tool_name| required_evidence_tools.contains(tool_name))
        .cloned()
        .collect::<Vec<_>>();
    let mut evidence_recovery_tool: Option<String> = None;
    let mut last_stream_item_id: Option<String> = None;
    let mut had_tool_calls = false;
    let mut empty_response_recovery_attempts = 0usize;
    let mut completion_recovery_attempts = 0usize;
    let mut stream_interruption_recovery_attempts = 0usize;
    let mut stream_interruption_non_stream_recovery_attempted = false;
    let mut context_limit_recovery_attempted = false;
    let mut discovery_progress = crate::discovery_progress::DiscoveryProgress::default();
    let mut last_response_observation: Option<String> = None;
    let rebuild_task_context = |static_context_messages: &[ChatMessage],
                                live_messages: &[ChatMessage],
                                phase: &'static str,
                                context_window: u64,
                                round_tools: Option<&[ChatToolDefinition]>,
                                active_skill_name: Option<&str>,
                                mode: ContextCompactionMode,
                                round_context: &[ChatMessage]| {
        let mut context_base_messages = static_context_messages.to_vec();
        context_base_messages.push(system_prompt_fragment_message(
            PromptFragmentKind::CurrentAccessProfile,
            current_access_profile_prompt(current_access_profile, current_command_mode),
        ));
        context_base_messages.push(system_prompt_fragment_message(
            PromptFragmentKind::CurrentTurnPriority,
            current_turn_context_priority_prompt(),
        ));
        refresh_current_agent_mode(
            &mut context_base_messages,
            task,
            agent_role_registry,
            execution_registry.execution_admission().limits(),
            round_tools.unwrap_or_default(),
            Some(&agent_model_capabilities),
        );
        if let Some(skill_message) = dynamic_skill_prompt_message(
            skill_runtime,
            initial_skill_name.as_deref(),
            active_skill_name,
        ) {
            context_base_messages.push(skill_message);
        }
        context_base_messages.extend(build_task_context_base_messages(
            stable_task_context,
            project_memory,
            memory_write_visible,
            plan_store,
            false,
            None,
        ));
        let pending_inputs = crate::context_authority::unprojected_input_messages(
            session_store,
            session_id,
            expected_turn_id.as_deref(),
            thread_id,
            live_messages,
        )?;
        let mut additional_context =
            estimate_context_usage_breakdown(&context_base_messages, round_tools);
        additional_context.merge(&estimate_context_usage_breakdown(&pending_inputs, None));
        additional_context.merge(&estimate_context_usage_breakdown(round_context, None));
        let prepared = prepare_task_history(phase, context_window, additional_context, mode);
        if let Some(terminal) = prepared.terminal {
            return Err(terminal);
        }
        // 校验后的历史总是替换旧视图；检查点失效并不一定触发新的压缩。
        let mut rebuilt = context_base_messages;
        rebuilt.extend(
            prepared
                .messages
                .iter()
                .map(thread_chat_message_to_chat_message),
        );
        const REBUILT_HISTORY_NOTE: &str = "本轮请求中的历史消息是当前 task 的有效运行记录（含适用的压缩摘要）。仅使用其中当前有效事实继续执行。";
        history_boundary_note.set(Some(REBUILT_HISTORY_NOTE));
        for input in pending_inputs {
            if !rebuilt
                .iter()
                .any(|message| message.input_request_id == input.input_request_id)
            {
                rebuilt.push(input);
            }
        }
        Ok((rebuilt, prepared.checkpoint_generation))
    };

    // 暂态故障的重试只发生在 HTTP 调用层；这里仅累计它上报的重试次数，用于失败诊断。
    let provider_retry_attempts = std::cell::Cell::new(0usize);
    let mut workspace_prepared = false;
    let mut prepared_execution_task = None;
    let mut retired_workspace_root = None;
    'conversation_round: for round in 0usize.. {
        // 成功工具可交付最终业务结果；恢复时读取原 canonical 回执，
        // 不再调用模型或重放工具。退出循环后仍走原任务验收与终态提交。
        if !plugin_completions.is_empty() {
            // 正常执行与恢复共用同一裁决：同批任何工具结果未确定时，
            // 已成功工具的完成意图不能把整个任务结算为成功。
            plugin_completions = match ToolExecutionLedger::from_canonical_task(
                session_store,
                task_store,
                session_id,
                task,
                thread_id,
                tool_registry,
            )
            .and_then(|tools| crate::plugin_tool_completion::recover(&tools.history))
            {
                Ok(completions) => completions,
                Err(error) => {
                    let error = task_failure_with_error_item(
                        turn_writeback_context,
                        &error,
                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                        None,
                        None,
                    );
                    return (TaskOutcome::Failed { error }, context_summary);
                }
            };
            final_content = plugin_completions
                .iter()
                .map(|completion| completion.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            final_model_round = None;
            final_model_step_id = None;
            final_provider_context.clear();
            break;
        }
        current_round.set(round);
        let request_history = crate::extension_hooks::RequestHistoryProjection::default();
        let with_volatile_context_tail =
            |request_history: &crate::extension_hooks::RequestHistoryProjection,
             messages: &[ChatMessage],
             context_window_tokens: u64,
             steps_tail: Option<&ChatMessage>,
             controls: &crate::request_controls::RequestControls,
             discovery_warning: Option<&ChatMessage>|
             -> Vec<ChatMessage> {
                let (history, projection) = request_history.view(
                    extension_hooks,
                    &crate::extension_hooks::ToolHookScope {
                        session_id: session_id.as_str(),
                        task_id: task_id.as_str(),
                        workspace_id: workspace_id.as_ref().map(|id| id.as_str()),
                        worker_id: worker_id.map(|id| id.as_str()),
                        round,
                    },
                    messages,
                    context_window_tokens,
                );
                *history_projection.borrow_mut() = projection;
                controls.view(
                    &history,
                    volatile_context_tail()
                        .into_iter()
                        .chain(steps_tail.cloned())
                        .chain(discovery_warning.cloned()),
                )
            };

        if let Some(failure) = emergency_model_round_failure(round) {
            let error = match append_task_error_turn_item(
                turn_writeback_context,
                &failure.summary,
                streaming_entry_id,
                None,
                None,
            ) {
                Ok(()) => failure.detail,
                Err(writeback_error) => {
                    format!("{}；失败事实写回失败：{writeback_error}", failure.detail)
                }
            };
            return (TaskOutcome::Failed { error }, context_summary);
        }
        runtime_signals
            .borrow_mut()
            .extend(conversation_registry.drain_task_signals(session_id, task_id));
        if !is_sidechain && let Some(turn_id) = expected_turn_id.as_deref() {
            crate::session_turn_execution::append_session_turn_steers_to_messages(
                &mut messages,
                conversation_registry
                    .turn_coordinator()
                    .drain_session_turn_steers(session_id, turn_id),
            );
        }
        if let Some(refresh) = refresh_tool_definitions {
            active_tools = refresh(active_skill_name.as_deref());
        }
        // 浏览器可用性每轮重新判断：桌面端恰好在重连时，不能让整轮都失去浏览器工具。
        let mut browser_capability_snapshot =
            tool_registry.and_then(|registry| registry.browser_capability_snapshot());
        if let Some(registry) = tool_registry
            && native_tools_enabled
            && client.executor_capabilities().tools
                != magi_bridge_client::ExecutorToolMode::ExecutorOwned
        {
            let policy = task.policy_snapshot.as_ref();
            let access_profile = policy
                .map(magi_core::TaskPolicy::effective_access_profile)
                .unwrap_or_default();
            let allowed_tools = policy
                .filter(|policy| !policy.allowed_tools.is_empty())
                .map(|policy| policy.allowed_tools.as_slice());
            let denied_tools = policy
                .map(|policy| policy.denied_tools.as_slice())
                .unwrap_or_default();
            let browser_surface = build_browser_tool_surface(
                active_tools,
                registry,
                BrowserToolSurfaceContext::new(
                    skill_runtime,
                    active_skill_name.as_deref(),
                    allowed_tools,
                    denied_tools,
                ),
                browser_capability_snapshot.clone(),
            );
            active_tools = browser_surface.definitions;
            browser_capability_snapshot = browser_surface.capability;
            active_tools =
                refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
                    definitions: active_tools,
                    tool_registry: registry,
                    skill_runtime,
                    active_skill_id: active_skill_name.as_deref(),
                    access_profile,
                    allowed_tools,
                    denied_tools,
                    loaded_tool_names: &loaded_external_tool_names,
                });
        }
        active_tools = constrain_task_tool_definitions(active_tools, task.policy_snapshot.as_ref());
        if task.is_goal_mode() {
            if session_store.current_unfinished_goal(session_id).is_some()
                || completed_required_tool_names
                    .iter()
                    .any(|tool_name| tool_name == "create_goal")
            {
                goal_creation_required = false;
            }
            required_tool_chain = goal_mode_required_tool_chain(
                GoalModeLifecycleState {
                    goal_creation_required,
                    goal_terminalization_required: goal_mode_requires_terminalization(
                        session_store,
                        session_id,
                    ),
                },
                &declared_required_tool_chain,
            );
            completed_required_tool_names.retain(|tool_name| {
                required_tool_chain
                    .iter()
                    .any(|required| required == tool_name)
            });
            // Goal creation is a one-time lifecycle decision. It is exposed
            // only for a new Goal task whose canonical chain contains the
            // creation step and until that step succeeds. A completed or
            // externally changed Goal must never make create_goal available
            // again inside the same execution.
            let creation_is_required = required_tool_chain
                .iter()
                .any(|tool_name| tool_name == "create_goal");
            let creation_is_pending = creation_is_required
                && !completed_required_tool_names
                    .iter()
                    .any(|tool_name| tool_name == "create_goal");
            if !creation_is_pending {
                active_tools.retain(|definition| definition.function.name != "create_goal");
            }
        }
        let thinking_item_id = format!("turn-item-assistant-thinking-{task_id}-{round}");
        let stream_item_id = task_stream_item_id(task_id, round, streaming_entry_id);
        last_stream_item_id = Some(stream_item_id.clone());
        let streamed_content = std::cell::RefCell::new(String::new());
        let streamed_visible_content = std::cell::RefCell::new(String::new());
        let streamed_thinking = std::cell::RefCell::new(String::new());
        let last_content_len = std::cell::Cell::new(0usize);
        let last_thinking_len = std::cell::Cell::new(0usize);
        let stream_publish_gate = std::cell::RefCell::new(SessionTurnStreamPublishGate::default());
        let thinking_publish_gate =
            std::cell::RefCell::new(SessionTurnStreamPublishGate::default());
        let round_goal_id = session_store
            .active_goal_for_execution_owner(session_id, task_id.as_str())
            .map(|goal| goal.goal_id);
        // 协作工具面由 root coordinator 身份、访问策略与运行时容量统一决定。
        // 不得根据用户文本缩减工具集，也不得强制某一轮调用 agent_spawn；否则模型
        // 无法在分析、实施、等待之间自主选择正确的协作步骤。
        let evidence_recovery_chain = evidence_recovery_tool.iter().cloned().collect::<Vec<_>>();
        let empty_completed_tools = Vec::new();
        let strict_goal_mode_round = task.is_goal_mode();
        let goal_lifecycle_pending = strict_goal_mode_round
            && !required_tool_chain_is_complete(
                &required_tool_chain,
                &completed_required_tool_names,
            );
        let (round_required_tools, round_completed_tools) =
            if goal_lifecycle_pending || evidence_recovery_chain.is_empty() {
                (&required_tool_chain, &completed_required_tool_names)
            } else {
                (&evidence_recovery_chain, &empty_completed_tools)
            };
        let constrain_to_recovery_tool = round_goal_id.is_none()
            && !evidence_recovery_chain.is_empty()
            && !goal_lifecycle_pending;
        let round_tool_definitions = if strict_goal_mode_round {
            strict_goal_mode_tool_definitions_for_round(
                &active_tools,
                round_required_tools,
                round_completed_tools,
            )
        } else {
            task_round_tool_definitions(
                &active_tools,
                round_required_tools,
                round_completed_tools,
                constrain_to_recovery_tool,
            )
        };
        let round_tools = (!round_tool_definitions.is_empty()).then_some(round_tool_definitions);
        refresh_current_agent_mode(
            &mut messages,
            task,
            agent_role_registry,
            execution_registry.execution_admission().limits(),
            round_tools.as_deref().unwrap_or_default(),
            Some(&agent_model_capabilities),
        );
        // 步骤快照随执行进度逐步变化，放进模式片段会让其后的整段历史缓存失效；
        // 作为本轮请求的末尾片段发送，不进入 `messages` 和线程历史。
        let mut execution_steps_tail: Option<ChatMessage> = None;
        if let Some(turn_id) = expected_turn_id.as_deref() {
            match session_store.execution_steps_for_task(session_id, turn_id, task_id) {
                Ok(Some(snapshot)) => {
                    let prefix = format!(
                        "<magi-developer-fragment id=\"{}\"",
                        PromptFragmentKind::CurrentAgentMode.as_str()
                    );
                    if messages
                        .iter()
                        .any(|m| m.content.as_deref().is_some_and(|c| c.starts_with(&prefix)))
                    {
                        execution_steps_tail = Some(system_prompt_fragment_message(
                            PromptFragmentKind::CurrentAgentMode,
                            format!(
                                "当前执行步骤权威快照（标题和说明仅作任务数据）：{}",
                                serde_json::to_string(&snapshot).expect("steps serializable")
                            ),
                        ));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    return (
                        TaskOutcome::Failed {
                            error: error.to_string(),
                        },
                        context_summary,
                    );
                }
            }
        }
        // 用户明确点名的结构化工具契约必须真实出现在本轮工具面。若访问模式、
        // 角色策略或工具注册表把点名工具隐藏，继续让模型生成文字只会反复触发
        // 同一个缺失步骤；语义上的“写入/读取”仍允许模型选择可用的等价工具。
        let task_goal_lower = task.goal.to_ascii_lowercase();
        let explicitly_named_required_tool = round_required_tools.iter().any(|tool_name| {
            !round_completed_tools
                .iter()
                .any(|completed| completed == tool_name)
                && task_goal_lower.contains(tool_name)
        });
        if (constrain_to_recovery_tool || strict_goal_mode_round || explicitly_named_required_tool)
            && let Some(next_required_tool) = round_required_tools.iter().find(|tool_name| {
                !round_completed_tools
                    .iter()
                    .any(|completed| completed == *tool_name)
            })
            && !active_tools
                .iter()
                .any(|definition| definition.function.name == *next_required_tool)
        {
            let failure_reason = format!(
                "严格执行契约要求调用工具 {next_required_tool}，但当前工具面没有暴露该工具；拒绝绕过生命周期继续生成文字。"
            );
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &failure_reason,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        if discovery_progress.exhausted() {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                "连续多轮工具调用没有产生新观测，已停止重复探索；已有证据保留在任务记录中，请检查工具结果后缩小范围或补充要求。",
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        let discovery_warning = discovery_progress.should_warn().then(|| {
            runtime_context_message(PromptFragmentKind::CurrentTurnPriority,
                "最近多轮探索没有获得新观测。请依据已有证据综合结论，或明确换一个尚未检查的证据范围；不要重复相同读取。")
        });
        // 每个新请求重验摘要和插件投影，空响应/steer 后的续轮同样不能保留旧视图。
        if round > 0 {
            match rebuild_task_context(
                &static_context_messages,
                &messages,
                "pre_model",
                effective_context_window,
                round_tools.as_deref(),
                active_skill_name.as_deref(),
                ContextCompactionMode::Automatic,
                &with_volatile_context_tail(
                    &request_history,
                    &[],
                    effective_context_window,
                    execution_steps_tail.as_ref(),
                    &request_controls,
                    discovery_warning.as_ref(),
                ),
            ) {
                Ok((rebuilt_messages, generation)) => {
                    messages = rebuilt_messages;
                    history_generation = generation;
                }
                Err(terminal) => {
                    return (
                        task_context_compaction_terminal_outcome_or_failed(
                            turn_writeback_context,
                            terminal,
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                        ),
                        context_summary,
                    );
                }
            }
        }
        let model_round_wall_started_at = Instant::now();
        let invocation_request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: crate::session_turn_execution::BUSINESS_MODEL_PROVIDER.to_string(),
            prompt: prompt.clone(),
            messages: Some(with_volatile_context_tail(
                &request_history,
                &messages,
                effective_context_window,
                execution_steps_tail.as_ref(),
                &request_controls,
                discovery_warning.as_ref(),
            )),
            tools: round_tools.clone(),
            tool_choice: if constrain_to_recovery_tool || strict_goal_mode_round {
                forced_task_tool_choice_for_round(
                    round_required_tools,
                    round_tools.as_ref(),
                    round_completed_tools,
                )
            } else {
                None
            },
        };
        let round_call_id = format!("task-{}-{}-{round}", task_id, lease_id);
        let mut step_client = crate::model_step::StepModelClient::new(
            client,
            session_store,
            session_id,
            expected_turn_id.as_deref(),
            Some(task_id),
            thread_id,
            &round_call_id,
            history_generation,
        )
        .with_model_binding(settings_store, usage_binding)
        .with_usage_assignment(lease_id.to_string())
        .with_history_projection(history_projection.borrow().clone())
        .with_extension_hooks(extension_hooks);
        if is_sidechain {
            step_client =
                step_client.with_prompt_branch(crate::prompt_lineage::PromptBranchScope::Worker {
                    task_id: task_id.as_str().to_string(),
                });
        }
        if let Some(policy) = prepared_execution_task
            .as_ref()
            .unwrap_or(task)
            .policy_snapshot
            .as_ref()
        {
            let skill_scope = crate::skill_custom_tool_surface::active_skill_tool_execution_policy(
                current_access_profile,
                skill_runtime,
                active_skill_name.as_deref(),
            );
            step_client =
                step_client.with_execution_scope(magi_bridge_client::ModelExecutionScope {
                    working_dir: workspace_root_path.clone(),
                    policy: policy.clone(),
                    requires_native_tools: task.is_goal_mode()
                        || !required_tool_chain.is_empty()
                        || !required_evidence_tools.is_empty()
                        || safety_gate.is_some_and(|gate| gate.rules().iter().any(|rule| {
                            rule.enabled && (rule.category == magi_safety_gate::SafetyCategory::Custom
                                || rule.action != magi_safety_gate::SafetyAction::RequireApprovalInRestricted)
                        })),
                    closed_skill_scope: skill_scope.allowed_tool_names.is_some(),
                });
        }
        let client: &dyn ModelBridgeClient = &step_client;
        let context_breakdown = estimate_context_usage_breakdown(
            &with_volatile_context_tail(
                &request_history,
                &messages,
                effective_context_window,
                execution_steps_tail.as_ref(),
                &request_controls,
                discovery_warning.as_ref(),
            ),
            round_tools.as_deref(),
        );
        let context_usage_tracker = usage_binding.tracks_active_context().then(|| {
            let resolved_provider =
                resolved_provider_for_usage_binding(settings_store, usage_binding, session_id);
            ContextUsageRuntimeTracker::start(ContextUsageRuntimeTrackerInput {
                event_bus,
                settings_store: settings_store.map(Arc::as_ref),
                session_id,
                workspace_id,
                turn_id: expected_turn_id.as_deref(),
                call_id: &round_call_id,
                resolved_model: &resolved_context_model,
                context_window_tokens: effective_context_window,
                context_breakdown: context_breakdown.clone(),
                thread_id: Some(thread_id),
                model_provider: resolved_provider,
                binding_revision: usage_binding.binding_revision(),
                checkpoint_generation: history_generation,
            })
        });
        let invocation_request_template = invocation_request.clone();
        let non_stream_recovery_template = invocation_request.clone();
        let invocation_cancelled = || !execution_guard.is_active();
        tracing::info!(
            target: "magi.performance",
            session_id = %session_id,
            task_id = %task_id,
            turn_id = expected_turn_id.as_deref().unwrap_or_default(),
            round,
            message_count = messages.len(),
            tool_count = round_tools.as_ref().map_or(0, Vec::len),
            elapsed_ms = model_round_wall_started_at.elapsed().as_millis() as u64,
            stage = "task_model_request_ready",
            "conversation response timing"
        );
        tracing::info!(
            target: "magi.performance",
            session_id = %session_id,
            task_id = %task_id,
            turn_id = expected_turn_id.as_deref().unwrap_or_default(),
            provider_call_id = %round_call_id,
            round,
            elapsed_ms = model_round_wall_started_at.elapsed().as_millis() as u64,
            stage = "provider_request_started",
            "conversation response timing"
        );

        let on_retry = |retry_event: &magi_bridge_client::ModelRetryRuntimeEvent| {
            if retry_event.phase == magi_bridge_client::ModelRetryRuntimePhase::Scheduled {
                provider_retry_attempts.set(provider_retry_attempts.get() + 1);
            }
            publish_model_retry_runtime_event(
                turn_writeback_context.event_bus,
                turn_writeback_context.session_id,
                turn_writeback_context.workspace_id,
                &stream_item_id,
                Some(&task.task_id),
                retry_event,
            );
        };

        let response = if streaming_entry_id.is_some() {
            // 模型客户端只发这一帧新增的文字；这里累积成到目前为止的全文，再算对外发布的增量。
            let delta_content = std::cell::RefCell::new(String::new());
            let delta_thinking = std::cell::RefCell::new(String::new());
            let on_delta = |delta: &ModelStreamingDelta| {
                if invocation_cancelled()
                    || turn_writeback_context
                        .writeback_error
                        .lock()
                        .expect("writeback error lock should hold")
                        .is_some()
                {
                    return;
                }
                if !first_provider_raw_delta_reported.get()
                    && (!delta.content.is_empty()
                        || !delta.thinking.is_empty()
                        || !delta.tool_calls.is_empty())
                {
                    first_provider_raw_delta_reported.set(true);
                    tracing::info!(
                        target: "magi.performance",
                        session_id = %session_id,
                        task_id = %task_id,
                        turn_id = expected_turn_id.as_deref().unwrap_or_default(),
                        provider_call_id = %round_call_id,
                        round,
                        elapsed_ms = model_round_wall_started_at.elapsed().as_millis() as u64,
                        stage = "provider_first_raw_delta",
                        tool_call_count = delta.tool_calls.len(),
                        "conversation response timing"
                    );
                }
                if !first_provider_delta_reported.get()
                    && (!delta.content.is_empty() || !delta.thinking.is_empty())
                {
                    first_provider_delta_reported.set(true);
                    tracing::info!(
                        target: "magi.performance",
                        session_id = %session_id,
                        task_id = %task_id,
                        turn_id = expected_turn_id.as_deref().unwrap_or_default(),
                        provider_call_id = %round_call_id,
                        round,
                        elapsed_ms = model_round_wall_started_at.elapsed().as_millis() as u64,
                        stage = "provider_first_delta",
                        "conversation response timing"
                    );
                }
                let mut accumulated_content = delta_content.borrow_mut();
                let mut accumulated_thinking = delta_thinking.borrow_mut();
                delta.accumulate_into(&mut accumulated_content, &mut accumulated_thinking);
                if let Some(tracker) = context_usage_tracker.as_ref() {
                    tracker.observe_accumulated_output(&accumulated_content, &accumulated_thinking);
                }
                if let Err(error) = publish_task_thinking_delta(
                    turn_writeback_context,
                    &thinking_item_id,
                    round,
                    &last_thinking_len,
                    &streamed_thinking,
                    &thinking_publish_gate,
                    &accumulated_thinking,
                ) {
                    record_task_writeback_error(turn_writeback_context, error);
                    return;
                }
                if let Err(error) = publish_task_content_delta(
                    turn_writeback_context,
                    TaskContentDelta {
                        item_id: &stream_item_id,
                        model_round: round,
                        last_sent_len: &last_content_len,
                        streamed_content: &streamed_content,
                        streamed_visible_content: &streamed_visible_content,
                        publish_gate: &stream_publish_gate,
                        accumulated_content: &accumulated_content,
                    },
                ) {
                    record_task_writeback_error(turn_writeback_context, error);
                }
            };

            'streaming_invocation: {
                match client.invoke_streaming_with_cancellation(
                    invocation_request_template.clone(),
                    &on_delta,
                    &on_retry,
                    &invocation_cancelled,
                ) {
                    Ok(response) => response,
                    Err(error) => {
                        let raw_error_message = error.to_string();
                        let classification = classify_model_invocation_error(&raw_error_message);
                        let error_message = classification.public_message.to_string();
                        let error_detail = direct_runtime_error(&error, &error_message);
                        publish_model_usage_record_for_turn(
                            event_bus,
                            session_store,
                            settings_store,
                            expected_turn_id.as_deref(),
                            crate::usage_recording::ModelUsageRecordInput {
                                context_breakdown: None,
                                session_id,
                                workspace_id,
                                binding: usage_binding,
                                call_id: step_client
                                    .last_step_id()
                                    .unwrap_or_else(|| round_call_id.clone()),
                                usage: None,
                                status: UsageCallStatus::Failed,
                                assignment_id: Some(lease_id.to_string()),
                                error_code: Some(classification.code.to_string()),
                            },
                        );

                        if error.context_overflow().is_some() && !context_limit_recovery_attempted {
                            context_limit_recovery_attempted = true;
                            let reported_context_limit = error
                                .context_overflow()
                                .and_then(|info| info.context_limit_tokens);
                            effective_context_window =
                                reported_context_limit.unwrap_or_else(|| {
                                    conservative_context_limit_recovery_window(
                                        effective_context_window,
                                    )
                                });
                            match rebuild_task_context(
                                &static_context_messages,
                                &messages,
                                "context_limit_recovery",
                                effective_context_window,
                                round_tools.as_deref(),
                                active_skill_name.as_deref(),
                                ContextCompactionMode::Recovery,
                                &with_volatile_context_tail(
                                    &request_history,
                                    &[],
                                    effective_context_window,
                                    execution_steps_tail.as_ref(),
                                    &request_controls,
                                    discovery_warning.as_ref(),
                                ),
                            ) {
                                Ok((rebuilt_messages, generation)) => {
                                    messages = rebuilt_messages;
                                    history_generation = generation;
                                    tracing::warn!(
                                        task_id = %task.task_id,
                                        round,
                                    context_limit = effective_context_window,
                                    "任务模型上下文超限，已按有效历史重建请求"
                                    );
                                    continue 'conversation_round;
                                }
                                Err(terminal) => {
                                    return (
                                        task_context_compaction_terminal_outcome_or_failed(
                                            turn_writeback_context,
                                            terminal,
                                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                                        ),
                                        context_summary,
                                    );
                                }
                            }
                        }
                        let partial_visible_content =
                            streamed_visible_content.borrow().trim().to_string();
                        let partial_thinking = streamed_thinking.borrow().trim().to_string();
                        if classification.code == "model_empty_response"
                            && empty_response_recovery_attempts
                                < MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
                        {
                            empty_response_recovery_attempts += 1;
                            request_controls.recover(
                                model_empty_response_recovery_prompt(had_tool_calls).to_string(),
                            );
                            tracing::warn!(
                                task_id = %task.task_id,
                                round,
                                attempt = empty_response_recovery_attempts,
                                max_attempts = MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS,
                                after_tool_calls = had_tool_calls,
                                "子代理模型桥接空响应，追加用户可见答复约束后继续执行"
                            );
                            continue 'conversation_round;
                        }
                        if classification.code == "model_stream_interrupted"
                            && (stream_interruption_recovery_attempts
                                < MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS
                                || !stream_interruption_non_stream_recovery_attempted)
                        {
                            if !partial_thinking.is_empty()
                                && let Err(error) = upsert_task_thinking_turn_item(
                                    turn_writeback_context,
                                    &thinking_item_id,
                                    round,
                                    "completed",
                                    &partial_thinking,
                                    None,
                                    &thinking_publish_gate,
                                )
                            {
                                let error_text = format!("流式中断后的思考写回失败：{error}");
                                let error = task_failure_with_error_item(
                                    turn_writeback_context,
                                    &error_text,
                                    streaming_entry_id.or(last_stream_item_id.as_deref()),
                                    None,
                                    None,
                                );
                                return (TaskOutcome::Failed { error }, context_summary);
                            }
                            if !partial_visible_content.is_empty() {
                                if let Err(error) = upsert_task_stream_turn_item(
                                    turn_writeback_context,
                                    &stream_item_id,
                                    round,
                                    "completed",
                                    &partial_visible_content,
                                    &[],
                                    None,
                                    &stream_publish_gate,
                                ) {
                                    let error_text = format!("流式中断后的回复写回失败：{error}");
                                    let error = task_failure_with_error_item(
                                        turn_writeback_context,
                                        &error_text,
                                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                                        None,
                                        None,
                                    );
                                    return (TaskOutcome::Failed { error }, context_summary);
                                }
                                messages.push(ChatMessage {
                                    input_request_id: None,
                                    context_origin: Default::default(),
                                    role: "assistant".to_string(),
                                    content: Some(partial_visible_content.clone()),
                                    images: Vec::new(),
                                    tool_calls: Vec::new(),
                                    tool_call_id: None,
                                    provider_context: Vec::new(),
                                });
                            }
                            let recovery_prompt = model_stream_interruption_recovery_prompt(
                                !partial_visible_content.is_empty(),
                            );
                            request_controls.recover(recovery_prompt.to_string());
                            if stream_interruption_recovery_attempts
                                < MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS
                            {
                                stream_interruption_recovery_attempts += 1;
                                tracing::warn!(
                                    task_id = %task.task_id,
                                    round = round,
                                    attempt = stream_interruption_recovery_attempts,
                                    max_attempts = MODEL_STREAM_INTERRUPTION_RECOVERY_MAX_ATTEMPTS,
                                    preserved_visible_chars = partial_visible_content.len(),
                                    ?error,
                                    "子代理模型流中断，保留片段后继续执行"
                                );
                                continue 'conversation_round;
                            }

                            stream_interruption_non_stream_recovery_attempted = true;
                            let mut recovery_request = non_stream_recovery_template;
                            recovery_request.messages = Some(with_volatile_context_tail(
                                &crate::extension_hooks::RequestHistoryProjection::default(),
                                &messages,
                                effective_context_window,
                                execution_steps_tail.as_ref(),
                                &request_controls,
                                discovery_warning.as_ref(),
                            ));
                            tracing::warn!(
                                task_id = %task.task_id,
                                round = round,
                                preserved_visible_chars = partial_visible_content.len(),
                                "子代理流式恢复已耗尽，降级为非流式完成请求"
                            );
                            match client.invoke_with_retry_events_and_cancellation(
                                recovery_request,
                                &on_retry,
                                &invocation_cancelled,
                            ) {
                                Ok(response) => break 'streaming_invocation response,
                                Err(recovery_error) => {
                                    let recovery_raw_error = recovery_error.to_string();
                                    let recovery_classification =
                                        classify_model_invocation_error(&recovery_raw_error);
                                    let recovery_message =
                                        recovery_classification.public_message.to_string();
                                    let recovery_detail =
                                        direct_runtime_error(&recovery_error, &recovery_message);
                                    publish_model_usage_record_for_turn(
                                        event_bus,
                                        session_store,
                                        settings_store,
                                        expected_turn_id.as_deref(),
                                        crate::usage_recording::ModelUsageRecordInput {
                                            context_breakdown: None,
                                            session_id,
                                            workspace_id,
                                            binding: usage_binding,
                                            call_id: step_client
                                                .last_step_id()
                                                .unwrap_or_else(|| round_call_id.clone()),
                                            usage: None,
                                            status: UsageCallStatus::Failed,
                                            assignment_id: Some(lease_id.to_string()),
                                            error_code: Some(
                                                recovery_classification.code.to_string(),
                                            ),
                                        },
                                    );
                                    tracing::error!(
                                        task_id = %task.task_id,
                                        round = round,
                                        ?recovery_error,
                                        "子代理非流式降级请求失败"
                                    );
                                    let model_failure = ModelFailureDiagnostic::from_invocation(
                                        recovery_classification,
                                        &recovery_detail,
                                        "response_stream_recovery",
                                        provider_retry_attempts.get()
                                            + stream_interruption_recovery_attempts
                                            + 1,
                                    );
                                    if execution_guard.is_active()
                                        && let Err(writeback_error) = append_task_error_turn_item(
                                            turn_writeback_context,
                                            &recovery_message,
                                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                                            Some(&model_failure),
                                            None,
                                        )
                                    {
                                        return (
                                            TaskOutcome::Failed {
                                                error: format!(
                                                    "{recovery_detail}；失败事实写回失败：{writeback_error}"
                                                ),
                                            },
                                            context_summary,
                                        );
                                    }
                                    return (
                                        TaskOutcome::Failed {
                                            error: recovery_detail,
                                        },
                                        context_summary,
                                    );
                                }
                            }
                        }
                        tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM streaming invocation failed");
                        let stage = if classification.code == "model_stream_interrupted" {
                            "response_stream"
                        } else if classification.code == "model_empty_response" {
                            "response_validation"
                        } else {
                            "request_dispatch"
                        };
                        let retry_attempts = provider_retry_attempts.get()
                            + stream_interruption_recovery_attempts
                            + empty_response_recovery_attempts;
                        let model_failure = if classification.code == "model_empty_response" {
                            ModelFailureDiagnostic::empty_response(
                                had_tool_calls,
                                retry_attempts,
                                Some(&error_detail),
                            )
                        } else {
                            ModelFailureDiagnostic::from_invocation(
                                classification,
                                &error_detail,
                                stage,
                                retry_attempts,
                            )
                        };
                        if execution_guard.is_active()
                            && let Err(writeback_error) = append_task_error_turn_item(
                                turn_writeback_context,
                                &error_message,
                                streaming_entry_id.or(last_stream_item_id.as_deref()),
                                Some(&model_failure),
                                None,
                            )
                        {
                            return (
                                TaskOutcome::Failed {
                                    error: format!(
                                        "{error_detail}；失败事实写回失败：{writeback_error}"
                                    ),
                                },
                                context_summary,
                            );
                        }
                        return (
                            TaskOutcome::Failed {
                                error: error_detail,
                            },
                            context_summary,
                        );
                    }
                }
            }
        } else {
            match client.invoke_with_retry_events_and_cancellation(
                invocation_request_template.clone(),
                &on_retry,
                &invocation_cancelled,
            ) {
                Ok(response) => response,
                Err(error) => {
                    if invocation_cancelled() {
                        return (
                            TaskOutcome::Failed {
                                error: "任务已中断".to_string(),
                            },
                            context_summary,
                        );
                    }
                    let raw_error_message = error.to_string();
                    let classification = classify_model_invocation_error(&raw_error_message);
                    let error_message = classification.public_message.to_string();
                    let error_detail = direct_runtime_error(&error, &error_message);
                    publish_model_usage_record_for_turn(
                        event_bus,
                        session_store,
                        settings_store,
                        expected_turn_id.as_deref(),
                        crate::usage_recording::ModelUsageRecordInput {
                            context_breakdown: None,
                            session_id,
                            workspace_id,
                            binding: usage_binding,
                            call_id: step_client
                                .last_step_id()
                                .unwrap_or_else(|| round_call_id.clone()),
                            usage: None,
                            status: UsageCallStatus::Failed,
                            assignment_id: Some(lease_id.to_string()),
                            error_code: Some(classification.code.to_string()),
                        },
                    );
                    if invocation_cancelled() {
                        return (
                            TaskOutcome::Failed {
                                error: "任务已中断".to_string(),
                            },
                            context_summary,
                        );
                    }
                    if error.context_overflow().is_some() && !context_limit_recovery_attempted {
                        context_limit_recovery_attempted = true;
                        let reported_context_limit = error
                            .context_overflow()
                            .and_then(|info| info.context_limit_tokens);
                        effective_context_window = reported_context_limit.unwrap_or_else(|| {
                            conservative_context_limit_recovery_window(effective_context_window)
                        });
                        match rebuild_task_context(
                            &static_context_messages,
                            &messages,
                            "context_limit_recovery",
                            effective_context_window,
                            round_tools.as_deref(),
                            active_skill_name.as_deref(),
                            ContextCompactionMode::Recovery,
                            &with_volatile_context_tail(
                                &request_history,
                                &[],
                                effective_context_window,
                                execution_steps_tail.as_ref(),
                                &request_controls,
                                discovery_warning.as_ref(),
                            ),
                        ) {
                            Ok((rebuilt_messages, generation)) => {
                                messages = rebuilt_messages;
                                history_generation = generation;
                                tracing::warn!(
                                    task_id = %task.task_id,
                                    round,
                                context_limit = effective_context_window,
                                "非流式任务模型上下文超限，已按有效历史重建请求"
                                );
                                continue 'conversation_round;
                            }
                            Err(terminal) => {
                                return (
                                    task_context_compaction_terminal_outcome_or_failed(
                                        turn_writeback_context,
                                        terminal,
                                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                                    ),
                                    context_summary,
                                );
                            }
                        }
                    }
                    if classification.code == "model_empty_response"
                        && empty_response_recovery_attempts
                            < MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
                    {
                        empty_response_recovery_attempts += 1;
                        request_controls.recover(
                            model_empty_response_recovery_prompt(had_tool_calls).to_string(),
                        );
                        tracing::warn!(
                            task_id = %task.task_id,
                            round,
                            attempt = empty_response_recovery_attempts,
                            max_attempts = MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS,
                            after_tool_calls = had_tool_calls,
                            "子代理模型空响应，追加用户可见答复约束后继续执行"
                        );
                        continue 'conversation_round;
                    }
                    tracing::error!(task_id = %task.task_id, round = round, ?error, "LLM invocation failed");
                    let retry_attempts =
                        provider_retry_attempts.get() + empty_response_recovery_attempts;
                    let model_failure = if classification.code == "model_empty_response" {
                        ModelFailureDiagnostic::empty_response(
                            had_tool_calls,
                            retry_attempts,
                            Some(&error_detail),
                        )
                    } else {
                        ModelFailureDiagnostic::from_invocation(
                            classification,
                            &error_detail,
                            "request_dispatch",
                            retry_attempts,
                        )
                    };
                    if execution_guard.is_active()
                        && let Err(writeback_error) = append_task_error_turn_item(
                            turn_writeback_context,
                            &error_message,
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                            Some(&model_failure),
                            None,
                        )
                    {
                        return (
                            TaskOutcome::Failed {
                                error: format!(
                                    "{error_detail}；失败事实写回失败：{writeback_error}"
                                ),
                            },
                            context_summary,
                        );
                    }
                    return (
                        TaskOutcome::Failed {
                            error: error_detail,
                        },
                        context_summary,
                    );
                }
            }
        };
        if !first_provider_delta_reported.get() {
            // 工具调用首轮可能只有提供方 tool-call block，没有可见文本 delta；统一在该
            // 轮响应收口处记录补充观测；真正的 provider_first_delta 仍由后续可见
            // 内容回调记录，避免把工具调用响应误标为首 delta。
            tracing::info!(
                target: "magi.performance",
                session_id = %session_id,
                task_id = %task_id,
                turn_id = expected_turn_id.as_deref().unwrap_or_default(),
                provider_call_id = %round_call_id,
                round,
                observation = "response_completion_without_visible_delta",
                elapsed_ms = model_round_wall_started_at.elapsed().as_millis() as u64,
                stage = "provider_response_received",
                "conversation response timing"
            );
        }
        tracing::info!(
            target: "magi.performance",
            session_id = %session_id,
            task_id = %task_id,
            turn_id = expected_turn_id.as_deref().unwrap_or_default(),
            round,
            elapsed_ms = model_round_wall_started_at.elapsed().as_millis() as u64,
            stage = "task_model_response_received",
            "conversation response timing"
        );

        if let Some(writeback_error) = take_task_writeback_error(turn_writeback_context) {
            let error_text = format!("模型流式响应写回失败：{writeback_error}");
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &error_text,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        if !execution_guard.is_active() {
            return (
                TaskOutcome::Failed {
                    error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
                },
                context_summary,
            );
        }
        let response_step_id = step_client.last_step_id();
        let turn_writeback_context = TaskTurnWritebackContext {
            model_step_id: response_step_id.as_deref(),
            ..turn_writeback_context
        };
        let parsed = response;
        if client.executor_capabilities().tools
            == magi_bridge_client::ExecutorToolMode::ExecutorOwned
        {
            let receipts = match magi_bridge_client::executor_receipts(&parsed.provider_context) {
                Ok(receipts) => receipts,
                Err(error) => {
                    let error = task_failure_with_error_item(
                        turn_writeback_context,
                        &error,
                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                        None,
                        None,
                    );
                    return (TaskOutcome::Failed { error }, context_summary);
                }
            };
            if !receipts.is_empty() && response_step_id.is_none() {
                let error = task_failure_with_error_item(
                    turn_writeback_context,
                    "外部执行器交付缺少持久 Step 身份",
                    streaming_entry_id.or(last_stream_item_id.as_deref()),
                    None,
                    None,
                );
                return (TaskOutcome::Failed { error }, context_summary);
            }
            // 每个已结算 Step 分别保留证据；后续补充不能覆盖前次外部执行事实。
            completion_evidence.extend(receipts.into_iter().map(|receipt| {
                TaskCompletionEvidence::ExternalExecutor {
                    model_step_id: response_step_id.clone().expect("receipt Step 已检查"),
                    receipt,
                }
            }));
        }
        last_response_observation = Some(format!(
            "模型轮次={}，status={:?}，finish_reason={}，正文字符数={}，thinking字符数={}，工具调用数={}",
            round + 1,
            parsed.status,
            parsed.finish_reason.as_deref().unwrap_or("<missing>"),
            parsed.content.as_deref().map(str::len).unwrap_or(0),
            parsed.thinking.as_deref().map(str::len).unwrap_or(0),
            parsed.tool_calls.len(),
        ));
        let tool_validation = validate_tool_call_batch(
            &parsed.tool_calls,
            round_tools.as_deref().unwrap_or_default(),
        );
        let round_has_tool_calls = !tool_validation.valid_calls.is_empty();
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
        if let Some(thinking) = final_thinking
            && let Err(error) = upsert_task_thinking_turn_item(
                turn_writeback_context,
                &thinking_item_id,
                round,
                "completed",
                &thinking,
                None,
                &thinking_publish_gate,
            )
        {
            let error_text = format!("思考结果写回失败：{error}");
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &error_text,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        let streamed_content = streamed_content.into_inner();
        let streamed_visible_content = streamed_visible_content.into_inner();
        let parsed_visible_content = parsed
            .content
            .as_deref()
            .map(normalize_model_visible_content)
            .filter(|content| !content.trim().is_empty());
        let completed_stream_content = if !streamed_visible_content.trim().is_empty() {
            Some(streamed_visible_content.clone())
        } else {
            parsed_visible_content.clone()
        };
        if let Some(completed_stream_content) = completed_stream_content.as_ref()
            && let Err(error) = upsert_task_stream_turn_item(
                turn_writeback_context,
                &stream_item_id,
                round,
                "completed",
                completed_stream_content,
                &parsed.provider_context,
                None,
                &stream_publish_gate,
            )
        {
            let error_text = format!("回复结果写回失败：{error}");
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &error_text,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        let has_actionable_output = completed_stream_content.is_some() || round_has_tool_calls;
        if has_actionable_output {
            request_controls.clear();
        }
        let response_contract_failure = match parsed.status {
            ModelResponseStatus::Incomplete => Some(ModelFailureDiagnostic::incomplete_response(
                parsed.finish_reason.as_deref(),
                completed_stream_content
                    .as_deref()
                    .map(str::len)
                    .unwrap_or(0),
            )),
            ModelResponseStatus::RequiresToolExecution if parsed.tool_calls.is_empty() => Some(
                ModelFailureDiagnostic::missing_tool_call(parsed.finish_reason.as_deref()),
            ),
            ModelResponseStatus::Completed | ModelResponseStatus::RequiresToolExecution => None,
        };
        publish_model_usage_record_for_turn_with_context_window(
            event_bus,
            session_store,
            settings_store,
            expected_turn_id.as_deref(),
            effective_context_window,
            crate::usage_recording::ModelUsageRecordInput {
                context_breakdown: Some(context_breakdown),
                session_id,
                workspace_id,
                binding: usage_binding,
                call_id: step_client
                    .last_step_id()
                    .unwrap_or_else(|| round_call_id.clone()),
                usage: parsed.usage.as_ref(),
                status: if has_actionable_output && response_contract_failure.is_none() {
                    UsageCallStatus::Success
                } else {
                    UsageCallStatus::Failed
                },
                assignment_id: Some(lease_id.to_string()),
                error_code: if let Some(failure) = response_contract_failure.as_ref() {
                    Some(failure.code.clone())
                } else if !tool_validation.invalid_calls.is_empty()
                    && tool_validation.valid_calls.is_empty()
                {
                    tool_validation
                        .invalid_calls
                        .first()
                        .map(|invalid| invalid.issue.code.clone())
                } else {
                    (!has_actionable_output).then(|| "model_empty_response".to_string())
                },
            },
        );
        let assistant_history_content = parsed
            .content
            .clone()
            .or_else(|| completed_stream_content.clone());
        if !round_has_tool_calls && let Some(ref content) = parsed.content {
            final_content = content.clone();
            final_model_round = Some(round);
        } else if !round_has_tool_calls && !streamed_content.trim().is_empty() {
            final_content = streamed_content.clone();
            final_model_round = Some(round);
        }
        if !round_has_tool_calls {
            final_model_step_id = response_step_id.clone();
            final_provider_context = parsed.provider_context.clone();
        }
        if round_has_tool_calls {
            had_tool_calls = true;
        }

        let assistant_response_message = ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "assistant".to_string(),
            content: assistant_history_content.clone(),
            images: Vec::new(),
            tool_calls: parsed.tool_calls.clone(),
            tool_call_id: None,
            provider_context: parsed.provider_context.clone(),
        };
        if parsed.tool_calls.is_empty()
            && (assistant_response_message
                .content
                .as_deref()
                .is_some_and(|content| !content.trim().is_empty())
                || !assistant_response_message.provider_context.is_empty())
            && let Err(error) = persist_session_state_checkpoint(
                persist_session_state,
                "task_thread_assistant_response",
            )
        {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &format!("任务 assistant 响应持久化失败：{error}"),
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }

        if let Some(failure) = response_contract_failure {
            let error = match append_task_error_turn_item(
                turn_writeback_context,
                &failure.summary,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                Some(&failure),
                None,
            ) {
                Ok(()) => failure.detail.clone(),
                Err(writeback_error) => {
                    format!("{}；失败事实写回失败：{writeback_error}", failure.detail)
                }
            };
            return (TaskOutcome::Failed { error }, context_summary);
        }

        if parsed.tool_calls.is_empty() {
            // 模型给出最终答复但完成合同未满足时追加一次纠正提示。所有纠正共享同一个
            // 次数上限；超过上限就结束循环，由循环后的合同检查以明确原因收口。
            let completion_recovery_prompt = if !required_tool_chain_is_complete(
                &required_tool_chain,
                &completed_required_tool_names,
            ) {
                Some(required_tool_chain_recovery_prompt(
                    &required_tool_chain,
                    &completed_required_tool_names,
                ))
            } else if let Some(missing_tool) =
                missing_required_evidence_tool(task, &completion_evidence)
            {
                evidence_recovery_tool = Some(missing_tool);
                Some(required_evidence_recovery_prompt(
                    task,
                    &completion_evidence,
                ))
            } else if let Some(recovery_prompt) =
                agent_coordination_recovery_prompt(task, task_store, &tool_call_records)
            {
                Some(recovery_prompt)
            } else if owns_active_plan() {
                plan_store.render_execution_follow_up_prompt()
            } else {
                None
            };
            if let Some(recovery_prompt) = completion_recovery_prompt {
                if completion_recovery_attempts >= COMPLETION_RECOVERY_MAX_ATTEMPTS {
                    break;
                }
                completion_recovery_attempts += 1;
                messages.push(assistant_response_message.clone());
                request_controls.recover(recovery_prompt);
                continue;
            }
            if !has_actionable_output
                && empty_response_recovery_attempts < MODEL_EMPTY_RESPONSE_RECOVERY_MAX_ATTEMPTS
            {
                empty_response_recovery_attempts += 1;
                if !assistant_response_message.provider_context.is_empty() {
                    messages.push(assistant_response_message.clone());
                }
                request_controls
                    .recover(model_empty_response_recovery_prompt(had_tool_calls).to_string());
                continue;
            }
            // 先取走全部已接纳输入；只有引导和代理信号都为空时才关闭通道和授权。
            match conversation_registry.take_task_inputs_or_close(
                session_id,
                task_id,
                (!is_sidechain)
                    .then_some(expected_turn_id.as_deref())
                    .flatten(),
            ) {
                TaskSignalBoundary::Pending(signals) => {
                    messages.push(assistant_response_message.clone());
                    runtime_signals.borrow_mut().extend(signals);
                    continue;
                }
                TaskSignalBoundary::PendingSteers(steers) => {
                    messages.push(assistant_response_message.clone());
                    crate::session_turn_execution::append_session_turn_steers_to_messages(
                        &mut messages,
                        steers,
                    );
                    continue;
                }
                TaskSignalBoundary::Closed => break,
            }
        }

        let assistant_tool_message = assistant_response_message;
        let assistant_provider_context = assistant_tool_message.provider_context.clone();
        // 模型返回的 assistant tool-call 是 canonical Turn 事实的一部分，即使某个
        // call 随后因工具面校验失败也不能只留在本地 messages 向量里。复用流式 item
        // 身份写入正文、工具调用和 provider context，ThreadChatMessage 再由 canonical
        // projection 重建。
        let mut assistant_tool_item = session_turn_item(
            "assistant_stream",
            "completed",
            Some("生成回复".to_string()),
            assistant_tool_message.content.clone(),
            Some(stream_item_id.clone()),
            thread_id.clone(),
        );
        apply_model_response_round(&mut assistant_tool_item, round);
        apply_step_identity(&mut assistant_tool_item, turn_writeback_context);

        apply_task_worker_detail_visibility(&mut assistant_tool_item, task, &turn_visibility);
        if !assistant_provider_context.is_empty() {
            assistant_tool_item.metadata.insert(
                "providerContext".to_string(),
                serde_json::to_value(&assistant_provider_context)
                    .expect("模型提供方上下文必须能够序列化"),
            );
        }
        if !assistant_tool_message.tool_calls.is_empty() {
            assistant_tool_item.metadata.insert(
                "toolCalls".to_string(),
                serde_json::to_value(&assistant_tool_message.tool_calls)
                    .expect("模型工具调用必须能够序列化"),
            );
        }
        if assistant_tool_message.content.is_some()
            || !assistant_tool_message.tool_calls.is_empty()
            || !assistant_tool_message.provider_context.is_empty()
        {
            let published = match upsert_session_turn_item_for_turn(
                session_store,
                session_id,
                expected_turn_id.as_deref(),
                assistant_tool_item,
                Some(task_store),
            ) {
                Ok(Some(published)) => published,
                Ok(None) => {
                    let error = task_failure_with_error_item(
                        turn_writeback_context,
                        "当前 Turn 已不可写，无法保存 assistant tool-call 事实",
                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                        None,
                        None,
                    );
                    return (TaskOutcome::Failed { error }, context_summary);
                }
                Err(error) => {
                    let error = task_failure_with_error_item(
                        turn_writeback_context,
                        &format!("保存 assistant tool-call 事实失败：{error}"),
                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                        None,
                        None,
                    );
                    return (TaskOutcome::Failed { error }, context_summary);
                }
            };
            publish_session_turn_item_event(event_bus, session_id, workspace_id, &published);
        }
        if let Err(error) = persist_session_state_checkpoint(
            persist_session_state,
            "task_thread_assistant_tool_calls",
        ) {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &format!("任务工具调用消息持久化失败：{error}"),
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        messages.push(assistant_tool_message);

        let invalid_tool_calls = tool_validation.invalid_calls;
        for invalid in &invalid_tool_calls {
            let tool_result_message = invalid_tool_result_message(invalid);
            // Thread 历史从 canonical 重建；只追加投影会丢失“未执行”的拒绝事实，
            // 重启后反而被解释为结果未知的副作用调用。复用唯一工具结果写回入口。
            if let Err(error) = upsert_task_tool_call_result_turn_item(
                turn_writeback_context,
                &invalid.call,
                tool_result_message.content.as_deref().unwrap_or_default(),
                ExecutionResultStatus::Rejected,
            ) {
                let error = task_failure_with_error_item(
                    turn_writeback_context,
                    &format!("无效工具拒绝结果写回失败：{error}"),
                    streaming_entry_id.or(last_stream_item_id.as_deref()),
                    None,
                    None,
                );
                return (TaskOutcome::Failed { error }, context_summary);
            }
            tool_call_records.push(tool_call_record(
                &invalid.call,
                tool_result_message.content.as_deref().unwrap_or_default(),
            ));
            if let Err(error) = persist_session_state_checkpoint(
                persist_session_state,
                "task_thread_invalid_tool_result",
            ) {
                let error = task_failure_with_error_item(
                    turn_writeback_context,
                    &format!("无效工具结果持久化失败：{error}"),
                    streaming_entry_id.or(last_stream_item_id.as_deref()),
                    None,
                    None,
                );
                return (TaskOutcome::Failed { error }, context_summary);
            }
            messages.push(tool_result_message);
        }
        // Goal 模式遇到混合批次时，invalid call 不能与 valid call 部分执行；否则模型
        // 即使绕过了当前严格工具面，批次里的浏览器/文件副作用仍可能已经发生。
        let valid_tool_calls = if strict_goal_mode_round && !invalid_tool_calls.is_empty() {
            Vec::new()
        } else {
            tool_validation.valid_calls
        };
        if strict_goal_mode_round
            && !valid_tool_calls.is_empty()
            && let Some(failure) = goal_mode_tool_batch_violation(
                round_required_tools,
                round_completed_tools,
                &valid_tool_calls,
            )
        {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &failure,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
        let needs_workspace = valid_tool_calls.iter().any(|call| {
            crate::task_execution_dispatcher::tool_requires_execution_resources(
                &call.function.name,
                &call.function.arguments,
                is_sidechain,
            )
        });
        if needs_workspace && !workspace_prepared {
            if let Some(prepare) = prepare_workspace {
                match prepare().and_then(|prepared| {
                    crate::workspace_rules::render_workspace_rules(
                        project_rules_root,
                        prepared.resources.directories.as_ref(),
                    )
                    .map(|rules| (prepared, rules))
                }) {
                    Ok((prepared, rules)) => {
                        if workspace_root_path != prepared.root {
                            retired_workspace_root = workspace_root_path.clone();
                            workspace_root_path = prepared.root;
                            tool_execution_ledger
                                .rebind_workspace_root(workspace_root_path.as_deref());
                        }
                        let updated = system_prompt_fragment_message(
                            PromptFragmentKind::WorkspaceContext,
                            format!(
                                "{}{}",
                                project_execution_context_system_prompt(
                                    workspace_root_path.as_ref().and_then(|root| root.to_str()),
                                    workspace_id.is_some(),
                                    prepared.resources.directories.as_ref(),
                                ),
                                rules
                            ),
                        );
                        for history in [&mut static_context_messages, &mut messages] {
                            if let Some(message) = history.iter_mut().find(|message| {
                                message.content.as_deref().is_some_and(|content| {
                                    content.starts_with(
                                        "<magi-developer-fragment id=\"workspace_context\"",
                                    )
                                })
                            }) {
                                *message = updated.clone();
                            }
                        }
                        workspace_resources = Some(prepared.resources);
                        prepared_execution_task = Some(prepared.task);
                    }
                    Err(error) => {
                        // assistant 已经声明了这些调用，但尚未执行；逐项留下明确回执，
                        // 避免恢复时将它们误认为结果未知、可能已产生副作用的调用。
                        for call in &valid_tool_calls {
                            let result = serde_json::json!({
                                "tool": call.function.name,
                                "status": "failed",
                                "error_code": "workspace_preparation_failed",
                                "execution_started": false,
                                "error": error,
                            })
                            .to_string();
                            if let Err(writeback_error) = upsert_task_tool_call_result_turn_item(
                                turn_writeback_context,
                                call,
                                &result,
                                ExecutionResultStatus::Failed,
                            ) {
                                return (
                                    TaskOutcome::Failed {
                                        error: format!(
                                            "工作区准备失败：{error}；未执行回执写入失败：{writeback_error}"
                                        ),
                                    },
                                    context_summary,
                                );
                            }
                        }
                        if let Err(writeback_error) = persist_session_state_checkpoint(
                            persist_session_state,
                            "task_thread_workspace_preparation_failed",
                        ) {
                            return (
                                TaskOutcome::Failed {
                                    error: format!(
                                        "工作区准备失败：{error}；线程回执写入失败：{writeback_error}"
                                    ),
                                },
                                context_summary,
                            );
                        }
                        let error = task_failure_with_error_item(
                            turn_writeback_context,
                            &format!("工作区执行准备失败：{error}"),
                            streaming_entry_id.or(last_stream_item_id.as_deref()),
                            None,
                            None,
                        );
                        return (TaskOutcome::Failed { error }, context_summary);
                    }
                }
            }
            workspace_prepared = true;
        }

        let stale_workspace_arguments = retired_workspace_root.as_ref().is_some_and(|old_root| {
            valid_tool_calls.iter().any(|call| {
                crate::task_execution_dispatcher::tool_requires_execution_resources(
                    &call.function.name,
                    &call.function.arguments,
                    is_sidechain,
                ) && crate::task_execution_dispatcher::arguments_reference_retired_root(
                    &call.function.name,
                    &call.function.arguments,
                    old_root,
                    workspace_root_path.as_deref(),
                )
            })
        });
        if !stale_workspace_arguments {
            for tool_call in &valid_tool_calls {
                if let Err(writeback_error) = append_task_tool_call_started_turn_item(
                    turn_writeback_context,
                    tool_call,
                    &assistant_provider_context,
                ) {
                    let error_text = format!("工具开始事实写回失败：{writeback_error}");
                    let error = task_failure_with_error_item(
                        turn_writeback_context,
                        &error_text,
                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                        None,
                        None,
                    );
                    return (TaskOutcome::Failed { error }, context_summary);
                }
            }
        }

        let tool_progress_callback = |progress: ToolExecutionProgress| {
            let Some(tool_call) = valid_tool_calls
                .iter()
                .find(|tool_call| tool_call.id == progress.tool_call_id.as_str())
            else {
                return Err("工具进度不属于当前批次".to_string());
            };
            if let Err(error) = upsert_task_tool_call_progress_turn_item(
                turn_writeback_context,
                tool_call,
                progress,
            ) {
                record_task_writeback_error(turn_writeback_context, error.clone());
                return Err(error);
            }
            Ok(())
        };
        let tool_results = if stale_workspace_arguments {
            valid_tool_calls.iter().map(|call| (serde_json::json!({
                "tool": call.function.name,
                "status": "failed",
                "error_code": "workspace_execution_root_changed",
                "execution_started": false,
                "error": "已按需分配隔离执行目录；本批调用仍引用原目录，尚未执行。请根据当前 workspace 上下文改用新目录或相对路径后重新调用。",
                "execution_root": workspace_root_path,
            }).to_string(), ExecutionResultStatus::Failed)).collect::<Vec<_>>()
        } else {
            // 扩展 `tool.call`：在账本指纹、路径与并发分类、权限和审批之前生效；
            // 被阻止的调用不进入原生管线，改写后的参数已用本轮工具定义重新校验。
            let hooked = extension_hooks.map(|hooks| {
                crate::extension_hooks::prepare_tool_calls(
                    hooks,
                    &crate::extension_hooks::ToolHookScope {
                        session_id: session_id.as_str(),
                        task_id: task_id.as_str(),
                        workspace_id: workspace_id.as_ref().map(|id| id.as_str()),
                        worker_id: turn_visibility.worker_id().map(|id| id.as_str()),
                        round,
                    },
                    &valid_tool_calls,
                    round_tools.as_deref().unwrap_or_default(),
                )
            });
            let native_calls = hooked
                .as_ref()
                .map_or(valid_tool_calls.as_slice(), |hooked| {
                    hooked.execute.as_slice()
                });
            let native_results = execute_task_tool_call_batch(
                &TaskToolBatchEnv {
                    event_bus,
                    tool_registry,
                    agent_role_registry,
                    skill_runtime,
                    skill_dispatch_runtime,
                    skill_name: active_skill_name.as_deref(),
                    task_store,
                    session_store,
                    execution_registry,
                    conversation_registry,
                    safety_gate,
                    plan_store,
                    project_memory,
                    task: prepared_execution_task.as_ref().unwrap_or(task),
                    session_id,
                    workspace_id,
                    workspace_root_path: workspace_root_path.as_ref(),
                    worker_id: turn_visibility.worker_id(),
                    browser_capability_snapshot,
                    on_progress: Some(&tool_progress_callback),
                    workspace_resources: workspace_resources.clone(),
                    execution_group_id: execution_group_id.clone(),
                    execution_allowed: &|| execution_guard.is_active(),
                },
                native_calls,
                &mut tool_execution_ledger,
            );
            match hooked {
                Some(hooked) => hooked.merge(native_results),
                None => native_results,
            }
        };

        if let Some(writeback_error) = take_task_writeback_error(turn_writeback_context) {
            let error_text = format!("工具执行过程写回失败：{writeback_error}");
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &error_text,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }

        let mut completed_tool_names_this_round = Vec::new();
        let mut activated_skill_this_round = None;
        let mut deterministic_tool_failure = None;
        let mut terminal_tool_failure = None;
        let mut round_had_discovery_tool = false;
        let mut round_had_successful_non_discovery_tool = false;
        for (tool_call, (result, tool_status)) in valid_tool_calls.iter().zip(tool_results) {
            if let Some(skill_id) =
                activated_skill_id_from_tool_result(&tool_call.function.name, &result, tool_status)
                && active_skill_name.as_deref() != Some(skill_id.as_str())
            {
                // 先保存恢复所需的限制，再发布 Skill 成功结果。否则在两次写入之间
                // 崩溃会留下成功回执，却以旧工具范围恢复执行。
                let activation = crate::tool_surface_state::persist_task_skill_activation(
                    execution_registry,
                    session_store,
                    task_id,
                    session_id,
                    expected_turn_id.as_deref(),
                    skill_id.clone(),
                    persist_session_state,
                );
                if let Err(error) = activation {
                    let error = task_failure_with_error_item(
                        turn_writeback_context,
                        &format!("Skill 激活状态未持久化，已停止后续执行：{error}"),
                        streaming_entry_id.or(last_stream_item_id.as_deref()),
                        None,
                        None,
                    );
                    return (TaskOutcome::Failed { error }, context_summary);
                }
            }
            if let Err(error) = upsert_task_tool_call_result_turn_item(
                turn_writeback_context,
                tool_call,
                &result,
                tool_status,
            ) {
                let error_text = format!("工具结果事实写回失败：{error}");
                let error = task_failure_with_error_item(
                    turn_writeback_context,
                    &error_text,
                    streaming_entry_id.or(last_stream_item_id.as_deref()),
                    None,
                    None,
                );
                return (TaskOutcome::Failed { error }, context_summary);
            }
            let canonical_tool_name = canonical_tool_call_name(&tool_call.function.name);
            if tool_status == ExecutionResultStatus::Succeeded
                && let Some(completion) =
                    crate::plugin_tool_completion::from_result(&tool_call.function.name, &result)
            {
                plugin_completions.push(completion);
            }
            if canonical_tool_name == "tool_catalog"
                && matches!(tool_status, ExecutionResultStatus::Succeeded)
                && !tool_result_execution_was_skipped(&result)
            {
                crate::tool_surface_state::extend_discovered_tool_names(
                    &mut loaded_external_tool_names,
                    &result,
                );
            }
            if discovery_tool_call(tool_call) {
                round_had_discovery_tool = true;
                discovery_progress.observe(
                    &canonical_tool_name,
                    &tool_call.function.arguments,
                    &result,
                    matches!(tool_status, ExecutionResultStatus::Succeeded)
                        && !tool_result_execution_was_skipped(&result),
                );
            } else if matches!(tool_status, ExecutionResultStatus::Succeeded)
                && !tool_result_execution_was_skipped(&result)
            {
                round_had_successful_non_discovery_tool = true;
            }
            if !tool_result_execution_was_skipped(&result)
                && matches!(tool_status, ExecutionResultStatus::Succeeded)
            {
                completed_tool_names_this_round.push(canonical_tool_name.clone());
                completion_evidence.push(TaskCompletionEvidence::SuccessfulToolCall {
                    call_id: tool_call.id.clone(),
                    tool_name: canonical_tool_name.clone(),
                    arguments: serde_json::from_str(&tool_call.function.arguments).unwrap_or_else(
                        |_| serde_json::Value::String(tool_call.function.arguments.clone()),
                    ),
                    result: result.clone(),
                });
            }
            tool_call_records.push(tool_call_record(tool_call, &result));
            if let Some(failure) = tool_execution_ledger.observe_tool_result(
                &canonical_tool_name,
                &tool_call.function.arguments,
                &result,
                tool_status,
                task.policy_snapshot
                    .as_ref()
                    .map(|policy| policy.retry_limit)
                    .unwrap_or(DEFAULT_TOOL_RETRY_LIMIT),
            ) {
                deterministic_tool_failure.get_or_insert(failure);
            }
            if let Some(failure) =
                non_retryable_tool_failure(&canonical_tool_name, &result, tool_status)
            {
                // 单次策略拒绝只是模型这一步选错了工具或路径：结果已交回模型，让它换个方式继续；
                // 同一任务里反复撞策略才终止，避免空转。
                policy_rejection_count += 1;
                if policy_rejection_count >= MAX_POLICY_REJECTIONS_PER_TASK {
                    terminal_tool_failure.get_or_insert(failure);
                }
            }
            if let Some(skill_id) =
                activated_skill_id_from_tool_result(&tool_call.function.name, &result, tool_status)
            {
                activated_skill_this_round = Some(skill_id);
            }
            // 本地历史只保留原生投影，扩展改写由统一请求视图入口应用。
            let model_visible =
                crate::history_projection::project_tool_result(&result, tool_status);
            let tool_result_message = ChatMessage {
                input_request_id: None,
                context_origin: Default::default(),
                role: "tool".to_string(),
                content: Some(model_visible),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call.id.clone()),
                provider_context: Vec::new(),
            };
            if let Err(error) =
                persist_session_state_checkpoint(persist_session_state, "task_thread_tool_result")
            {
                let error = task_failure_with_error_item(
                    turn_writeback_context,
                    &format!("工具结果消息持久化失败：{error}"),
                    streaming_entry_id.or(last_stream_item_id.as_deref()),
                    None,
                    None,
                );
                return (TaskOutcome::Failed { error }, context_summary);
            }
            messages.push(tool_result_message);
        }
        discovery_progress.finish_round(
            round_had_discovery_tool,
            round_had_successful_non_discovery_tool,
        );
        if let Some(failure) = terminal_tool_failure.or(deterministic_tool_failure) {
            let error = match append_task_error_turn_item(
                turn_writeback_context,
                &failure.summary,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            ) {
                Ok(()) => failure.detail.clone(),
                Err(writeback_error) => {
                    format!("{}；失败事实写回失败：{writeback_error}", failure.detail)
                }
            };
            return (TaskOutcome::Failed { error }, context_summary);
        }
        let repeated_tool_call_failure = if invalid_tool_calls.is_empty() {
            tool_call_validation_tracker.record_valid_round();
            None
        } else {
            let attempts = tool_call_validation_tracker.record_round();
            let mut unavailable_failure = None;
            for invalid in &invalid_tool_calls {
                if invalid.issue.reason_code != "tool_not_available" {
                    continue;
                }
                let unavailable_attempts =
                    tool_call_validation_tracker.record_unavailable_tool(&invalid.issue.tool_name);
                if unavailable_attempts >= 2 {
                    unavailable_failure = Some(ToolCallFailureDiagnostic::repeated(
                        &invalid.issue,
                        unavailable_attempts.saturating_sub(1),
                    ));
                    break;
                }
            }
            if unavailable_failure.is_some() {
                unavailable_failure
            } else if attempts >= 2 {
                invalid_tool_calls.first().map(|invalid| {
                    ToolCallFailureDiagnostic::repeated(&invalid.issue, attempts.saturating_sub(1))
                })
            } else {
                for invalid in &invalid_tool_calls {
                    tracing::warn!(
                        task_id = %task.task_id,
                        round,
                        tool = %invalid.issue.tool_name,
                        reason_code = %invalid.issue.reason_code,
                        "模型提交了无效工具调用，已拒绝执行并请求模型修正"
                    );
                }
                None
            }
        };
        if let Some(tool_call_failure) = repeated_tool_call_failure {
            let error = match append_task_error_turn_item(
                turn_writeback_context,
                &tool_call_failure.summary,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                Some(&tool_call_failure),
            ) {
                Ok(()) => tool_call_failure.detail.clone(),
                Err(writeback_error) => format!(
                    "{}；失败事实写回失败：{writeback_error}",
                    tool_call_failure.detail
                ),
            };
            return (TaskOutcome::Failed { error }, context_summary);
        }
        if let Some(skill_id) = activated_skill_this_round
            && active_skill_name.as_deref() != Some(skill_id.as_str())
            && let Some(runtime) = skill_runtime
        {
            active_skill_name = Some(skill_id.clone());
            if let Some(skill_message) = skill_prompt_message(runtime, &skill_id) {
                messages.push(skill_message);
            }
        }
        record_completed_required_tools(
            &mut completed_required_tool_names,
            &required_tool_chain,
            &completed_tool_names_this_round,
        );
        if task.is_goal_mode() {
            if session_store.current_unfinished_goal(session_id).is_some()
                || completed_required_tool_names
                    .iter()
                    .any(|tool_name| tool_name == "create_goal")
            {
                goal_creation_required = false;
            }
            required_tool_chain = goal_mode_required_tool_chain(
                GoalModeLifecycleState {
                    goal_creation_required,
                    goal_terminalization_required: goal_mode_requires_terminalization(
                        session_store,
                        session_id,
                    ),
                },
                &declared_required_tool_chain,
            );
            completed_required_tool_names.retain(|tool_name| {
                required_tool_chain
                    .iter()
                    .any(|required| required == tool_name)
            });
        }
        record_completed_required_tools(
            &mut completed_evidence_tool_names,
            &required_evidence_tools,
            &completed_tool_names_this_round,
        );
        if evidence_recovery_tool.as_ref().is_some_and(|tool_name| {
            !task
                .completion_contract()
                .evidence_requirements
                .iter()
                .any(|requirement| {
                    requirement.tool_name() == tool_name
                        && !requirement.is_satisfied_by(&completion_evidence)
                })
        }) {
            evidence_recovery_tool = None;
        }
    }

    if !required_tool_chain_is_complete(&required_tool_chain, &completed_required_tool_names) {
        let failure_reason = required_tool_chain_recovery_prompt(
            &required_tool_chain,
            &completed_required_tool_names,
        );
        let error = task_failure_with_error_item(
            turn_writeback_context,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
            None,
            None,
        );
        return (TaskOutcome::Failed { error }, context_summary);
    }

    if missing_required_evidence_tool(task, &completion_evidence).is_some() {
        let failure_reason = required_evidence_recovery_prompt(task, &completion_evidence);
        let error = task_failure_with_error_item(
            turn_writeback_context,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
            None,
            None,
        );
        return (TaskOutcome::Failed { error }, context_summary);
    }

    if let Some(recovery_prompt) =
        agent_coordination_recovery_prompt(task, task_store, &tool_call_records)
    {
        let error = task_failure_with_error_item(
            turn_writeback_context,
            &recovery_prompt,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
            None,
            None,
        );
        return (TaskOutcome::Failed { error }, context_summary);
    }

    if plugin_completions.is_empty() {
        final_content = normalize_model_visible_content(final_content);
    }
    if final_content.trim().is_empty() {
        let retry_attempts = empty_response_recovery_attempts
            + stream_interruption_recovery_attempts
            + provider_retry_attempts.get()
            + usize::from(stream_interruption_non_stream_recovery_attempted);
        let model_failure = ModelFailureDiagnostic::empty_response(
            had_tool_calls,
            retry_attempts,
            last_response_observation.as_deref(),
        );
        let error = match append_task_error_turn_item(
            turn_writeback_context,
            &model_failure.summary,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
            Some(&model_failure),
            None,
        ) {
            Ok(()) => model_failure.detail.clone(),
            Err(writeback_error) => format!(
                "{}；失败事实写回失败：{writeback_error}",
                model_failure.detail
            ),
        };
        return (TaskOutcome::Failed { error }, context_summary);
    }
    if !execution_guard.is_active() {
        return (
            TaskOutcome::Failed {
                error: "任务执行已被中断，丢弃晚到模型结果".to_string(),
            },
            context_summary,
        );
    }

    if task_has_validation_gate(task) && validation_result_rejects_delivery(&final_content) {
        let failure_reason = compact_validation_failure(&final_content);
        let error = task_failure_with_error_item(
            turn_writeback_context,
            &failure_reason,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
            None,
            None,
        );
        return (TaskOutcome::Failed { error }, context_summary);
    }

    // 步骤只是进度展示：纠正后仍未收口时不判任务失败，Turn 终态结算会把未完成步骤标为未确认，
    // 不会伪造完成。
    let output_refs = vec![build_output_content(
        tool_call_records,
        final_content.clone(),
    )];
    let outcome = match validated_task_completion(
        task,
        output_refs,
        final_content.clone(),
        completion_evidence,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let error = task_failure_with_error_item(
                turn_writeback_context,
                &error,
                streaming_entry_id.or(last_stream_item_id.as_deref()),
                None,
                None,
            );
            return (TaskOutcome::Failed { error }, context_summary);
        }
    };

    if let Err(writeback_error) = append_task_final_turn_item(
        TaskTurnWritebackContext {
            model_step_id: final_model_step_id.as_deref(),
            ..turn_writeback_context
        },
        &final_content,
        plugin_completions
            .is_empty()
            .then(|| last_stream_item_id.as_deref().or(streaming_entry_id))
            .flatten(),
        streaming_entry_id,
        final_model_round,
        &final_provider_context,
        &plugin_completions,
    ) {
        let error_text = format!("最终回复写回失败：{writeback_error}");
        let error = task_failure_with_error_item(
            turn_writeback_context,
            &error_text,
            streaming_entry_id.or(last_stream_item_id.as_deref()),
            None,
            None,
        );
        return (TaskOutcome::Failed { error }, context_summary);
    }

    (outcome, context_summary)
}

fn tool_result_execution_was_skipped(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|value| {
            value
                .get("execution")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .is_some_and(|execution| execution == "skipped")
}

fn task_has_validation_gate(task: &Task) -> bool {
    task.policy_snapshot
        .as_ref()
        .is_some_and(|policy| policy.validation_profile.is_some())
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

#[cfg(test)]
mod tests;
