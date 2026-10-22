//! 任务系统 — conversation dispatcher runtime.
//!
//! Owns the production task dispatch implementation for session turns and conversation loops.

use crate::{
    ConversationRegistry, SKILL_APPLY_TOOL_NAME, active_skill_tool_execution_policy,
    build_skill_custom_tool_definitions,
    context_authority::current_session_file_facts,
    conversation_loop::{self, ConversationLoopRequest},
    execution_admission::ExecutionAdmissionPermit,
    model_config::{
        NormalizedModelConfig, configured_role_engine_model_config, orchestrator_engine_id,
        resolve_orchestrator_model_config, role_engine_is_chatgpt_web,
        session_web_conversation_binding,
    },
    prompt_utils::{
        CURRENT_TASK_PRIORITY_NOTE, PromptFragmentKind, REFERENCE_CONTEXT_PRIORITY_NOTE,
        compose_developer_instructions, render_safeguard_prompt, runtime_context_message,
        user_rules_from_settings,
    },
    public_builtin_tool_definitions,
    session_images::SessionTurnImage,
    session_turn_execution::{
        BUSINESS_MODEL_PROVIDER, SessionTurnExecutionError, SessionTurnExecutionOutput,
        SessionTurnExecutionRequest, SessionTurnExecutionRuntime, TurnTerminalCommitPolicy,
        run_session_turn_execution,
    },
    session_turn_finalize::{format_dependency_task_context, format_task_ref_list},
    session_writeback::SessionStatePersistCallback,
    skill_apply_tool_definition,
    task_completion_notifier::TaskCompletionNotifier,
    task_execution_registry::{TaskExecutionPlan, TaskExecutionRegistry},
    task_helpers::{
        enforce_agent_coordination_surface, task_can_see_builtin_tool, task_is_coordinator,
        task_role_id,
    },
    task_runner_bridge::{TaskCompletionSink, TaskDispatcher, TaskOutcome, TaskResult},
    tool_surface_state::{
        RefreshLiveMcpToolDefinitionsInput, constrain_task_tool_definitions,
        refresh_live_mcp_tool_definitions_with_mode,
    },
    usage_recording::{
        AuxiliaryModelUsageContext, ModelUsageBinding, invoke_auxiliary_model_with_usage,
        model_usage_binding_for_worker_with_settings,
    },
};
use magi_bridge_client::{
    BridgeClientError, BridgeErrorLayer, ChatToolDefinition, ModelBridgeClient,
    ModelInvocationRequest, ModelResponse, ModelStreamingDelta,
};
use magi_context_runtime::{
    ContextBudget, ContextRuntime, ExecutionContextAssemblyRequest, ExecutionContextClues,
    FileSummaryItem, FileSummaryRecord, KnowledgeConsumer, KnowledgeContextRequest,
    KnowledgeContextSelection, RecentTurnSource,
};
use magi_core::{
    AccessProfile, EventId, LeaseId, SessionId, TaskId, TaskKind, UtcMillis, WorkerId, WorkspaceId,
    estimate_text_tokens, public_runtime_excerpt,
};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_knowledge_store::{KnowledgeKind, KnowledgeRecord, KnowledgeStore};
use magi_memory_store::{ExtractedMemory, MemoryExtractionApplyRequest, MemoryLayer, MemoryStore};
use magi_orchestrator::{
    ExecutionContextSummary, ExecutionWritebackPlans, OrchestratedExecutionRuntime,
    task_worker_catalog::WorkerInfo,
};
use magi_session_store::{SessionStore, TimelineEntryKind, timeline_entry_visible_text};
use magi_settings_store::SettingsStore;
use magi_tool_runtime::{BuiltinToolName, ToolRegistry};
use magi_usage_authority::UsagePhase;
use magi_workspace::WorkspaceStore;
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
};

#[cfg(test)]
use crate::session_turn_execution::run_session_turn_execution_for_test;

mod workflow_execution;
use workflow_execution::WorkflowInvocation;

#[derive(Clone)]
pub struct ExecutionPipeline {
    pub execution_runtime: OrchestratedExecutionRuntime,
    pub memory_store: MemoryStore,
}

/// 在首次本地工具调用前准备会话资源。实现由 daemon 的应用层注入；运行时不复制
/// Git 基线、执行租约或变更账本的所有权。
pub type WorkspaceExecutionPreparer = dyn Fn(
        &SessionId,
        &Option<WorkspaceId>,
        &str,
    ) -> Result<Option<Arc<magi_snapshot::SnapshotSession>>, String>
    + Send
    + Sync;

fn tool_requires_workspace_execution_preparation(name: &str, arguments: &str) -> bool {
    BuiltinToolName::from_name(name)
        .map(|tool| tool.requires_workspace_execution_preparation(arguments))
        // MCP 持有自己的执行环境；Skill 发现只加载说明，不执行 handler。
        // 其余 Skill handler / 插件可能访问文件或进程，按本地执行准备。
        .unwrap_or_else(|| name != SKILL_APPLY_TOOL_NAME && !name.starts_with("mcp__"))
}

/// 主线读取共享目录无需扫描；子代理首次访问本地事实前必须分配隔离目录。
pub(crate) fn tool_requires_execution_resources(
    name: &str,
    arguments: &str,
    is_sidechain: bool,
) -> bool {
    if BuiltinToolName::from_name(name)
        .is_some_and(|tool| tool.is_background_process_observation(arguments))
    {
        return false;
    }
    tool_requires_workspace_execution_preparation(name, arguments)
        || (is_sidechain
            && BuiltinToolName::from_name(name)
                .is_some_and(|tool| tool.requires_workspace_context()))
}

/// 模型请求在目录分配前生成，绝对路径不能静默继续指向旧工作区。
/// 检查解码后的参数，兼容 Windows 反斜杠；新目录位于旧目录内时允许引用新目录。
pub(crate) fn arguments_reference_retired_root(
    tool_name: &str,
    arguments: &str,
    retired: &std::path::Path,
    current: Option<&std::path::Path>,
) -> bool {
    let retired_path = magi_tool_runtime::canonicalize_tool_permission_path(retired);
    let current_path = current.map(magi_tool_runtime::canonicalize_tool_permission_path);
    if current_path.as_ref() == Some(&retired_path) {
        return false;
    }
    if magi_tool_runtime::tool_path_access_requests(
        tool_name,
        arguments,
        current,
        AccessProfile::FullAccess,
    )
    .iter()
    .any(|request| {
        let path = magi_tool_runtime::canonicalize_tool_permission_path(&request.absolute_path);
        path.starts_with(&retired_path)
            && !current_path
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
    }) {
        return true;
    }
    fn references(value: &serde_json::Value, retired: &str, current: Option<&str>) -> bool {
        match value {
            serde_json::Value::String(value) => {
                let text_reference = current
                    .map(|current| value.replace(current, ""))
                    .unwrap_or_else(|| value.clone())
                    .contains(retired);
                let path_reference = std::path::Path::new(value).is_absolute() && {
                    let path = magi_tool_runtime::canonicalize_tool_permission_path(
                        std::path::Path::new(value),
                    );
                    let retired = magi_tool_runtime::canonicalize_tool_permission_path(
                        std::path::Path::new(retired),
                    );
                    path.starts_with(retired)
                        && !current.is_some_and(|current| {
                            path.starts_with(magi_tool_runtime::canonicalize_tool_permission_path(
                                std::path::Path::new(current),
                            ))
                        })
                };
                text_reference || path_reference
            }
            serde_json::Value::Array(values) => values
                .iter()
                .any(|value| references(value, retired, current)),
            serde_json::Value::Object(values) => values
                .values()
                .any(|value| references(value, retired, current)),
            _ => false,
        }
    }
    let retired = retired.to_string_lossy();
    let current = current.map(|root| root.to_string_lossy());
    serde_json::from_str(arguments)
        .is_ok_and(|value| references(&value, &retired, current.as_deref()))
}

/// 插件命令不依赖模型配置，但 conversation 执行合同仍要求携带一个 client。
/// 这个占位 client 只会在插件命令已经在执行入口短路时传入，任何模型调用都会显式失败。
struct PluginCommandPlaceholderClient;

impl ModelBridgeClient for PluginCommandPlaceholderClient {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::text_executor_capabilities()
    }

    fn invoke(&self, _request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: "插件命令不允许调用模型".into(),
        })
    }

    fn invoke_streaming(
        &self,
        _request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: "插件命令不允许调用模型".into(),
        })
    }
}

struct TaskDispatchedEventInput<'a> {
    task_id: &'a TaskId,
    mission_id: &'a magi_core::MissionId,
    worker: &'a WorkerInfo,
    lease_id: &'a LeaseId,
    kind: magi_core::TaskKind,
    session_id: Option<&'a SessionId>,
    workspace_id: Option<&'a WorkspaceId>,
}

struct DispatchPlanExecutionInput<'a> {
    task: &'a magi_core::Task,
    lease_id: &'a LeaseId,
    session_id: SessionId,
    turn_id: String,
    workspace_id: Option<WorkspaceId>,
    execution_root: Option<PathBuf>,
    writebacks: ExecutionWritebackPlans,
    use_tools: bool,
    skill_name: Option<String>,
    images: Vec<SessionTurnImage>,
    usage_binding: ModelUsageBinding,
    is_sidechain: bool,
    worker_id: WorkerId,
    worker_role: String,
    thread_id: magi_core::ThreadId,
    system_prompt: Option<String>,
    execution_settings_snapshot: Option<Arc<SettingsStore>>,
}

struct TaskLlmInvocationInput<'a> {
    task: &'a magi_core::Task,
    lease_id: &'a LeaseId,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    execution_root: Option<&'a PathBuf>,
    use_tools: bool,
    skill_name: Option<String>,
    images: Vec<SessionTurnImage>,
    usage_binding: &'a ModelUsageBinding,
    streaming_entry_id: Option<&'a str>,
    is_sidechain: bool,
    worker_id: Option<&'a WorkerId>,
    execution_role_id: Option<&'a str>,
    thread_id: &'a magi_core::ThreadId,
    system_prompt: Option<String>,
    execution_settings_snapshot: Option<Arc<SettingsStore>>,
}

#[derive(Clone)]
pub struct LlmTaskDispatcher {
    event_bus: Arc<InMemoryEventBus>,
    pipeline: ExecutionPipeline,
    session_store: Arc<SessionStore>,
    execution_registry: TaskExecutionRegistry,
    completion_sink: Arc<dyn TaskCompletionSink>,
    completion_notifier: Option<Arc<TaskCompletionNotifier>>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    /// Web 引擎（GPT Web）的 client 工厂，由 daemon 装配宿主实现。
    ///
    /// 未装配时命中 Web 引擎会失败关闭（不退回 HTTP 模型，A19、§5.6）。
    session_engine_factory: Option<Arc<dyn magi_plugin_system::SessionEngineFactory>>,
    workflow_core_factory: Option<Arc<dyn magi_plugin_system::WorkflowCoreFactory>>,
    plugin_command_executor: Option<Arc<dyn magi_plugin_system::PluginCommandExecutor>>,
    /// 按设置事实源代际复用角色模型客户端。HTTP 连接池由 bridge-client 继续统一持有，
    /// 这里只避免每个 Turn 重复解析配置和构造同一角色包装器。
    model_client_cache: Arc<Mutex<HashMap<String, Arc<dyn ModelBridgeClient>>>>,
    /// 按 root task 统计尚未退出的异步 dispatch。Runner 重启必须等待这里的计数
    /// 归零，不能只等待 Runner 自身循环结束。
    dispatch_quiesce: Arc<DispatchQuiesceState>,
    conversation_quiesce: Arc<DispatchQuiesceState>,
    knowledge_store: Option<Arc<KnowledgeStore>>,
    knowledge_persist_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    session_state_persist_callback: Option<Arc<SessionStatePersistCallback>>,
    settings_store: Option<Arc<SettingsStore>>,
    context_runtime: Option<Arc<ContextRuntime>>,
    /// 由 daemon bootstrap 注入的上下文预算，决定每轮 Turn 装配 prompt 时记忆 / 知识 /
    /// shared context 各最多取多少条。未注入时退回 [`default_context_budget_for_unconfigured_dispatcher`]，便于
    /// 在测试和最小依赖场景下仍可工作；生产环境 daemon 必须显式注入以保持单一事实源。
    context_budget: Option<ContextBudget>,
    workspace_registry: Option<Arc<WorkspaceStore>>,
    git_service: Option<Arc<magi_git::GitService>>,
    session_code_contexts: Option<magi_git::SessionCodeContextRegistry>,
    workspace_git_coordinator: Option<magi_git::WorkspaceGitOperationCoordinator>,
    agent_worktree_root: PathBuf,
    tool_registry: Option<ToolRegistry>,
    skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    skill_dispatch_runtime: Option<Arc<magi_skill_runtime::SkillDispatchRuntime>>,
    snapshot_manager: Option<Arc<magi_snapshot::SnapshotManager>>,
    workspace_execution_preparer: Option<Arc<WorkspaceExecutionPreparer>>,
    /// 主线和子代理共用的运行时输入与交互投递入口，不持有独立 Turn 状态。
    conversation_registry: Arc<ConversationRegistry>,
    /// 任务系统：AgentRole 注册表（来自 ApiState，注入到 conversation_loop）。
    agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
    /// 任务系统 — L5：父子任务拓扑图。S7 协调器工具 agent_spawn
    /// 需要在 conversation_loop 中读写。设计为构造期必填，避免运行期再做空检查。
    /// 任务系统 — L14：workspace 维度的 ProjectMemory 索引。S10 中模型通过
    /// `memory_write` 工具新增/删除项目记忆条目；每次 Turn 起始把 MEMORY.md 视图注入
    /// system prompt，跨 conversation 复用。
    project_memory_registry: Arc<magi_project_memory::ProjectMemoryRegistry>,
}

pub struct LlmTaskDispatcherDependencies {
    pub session_store: Arc<SessionStore>,
    pub execution_registry: TaskExecutionRegistry,
    pub completion_sink: Arc<dyn TaskCompletionSink>,
    pub conversation_registry: Arc<ConversationRegistry>,
    pub agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
}

#[derive(Default)]
struct DispatchQuiesceState {
    in_flight: Mutex<HashMap<(TaskId, TaskId), usize>>,
    notify: tokio::sync::Notify,
    idle: Condvar,
}

impl DispatchQuiesceState {
    fn begin(self: &Arc<Self>, root_task_id: TaskId) -> InFlightDispatchGuard {
        self.begin_task(root_task_id.clone(), root_task_id)
    }

    fn begin_task(
        self: &Arc<Self>,
        root_task_id: TaskId,
        task_id: TaskId,
    ) -> InFlightDispatchGuard {
        let mut in_flight = self
            .in_flight
            .lock()
            .expect("dispatch quiesce lock poisoned");
        *in_flight
            .entry((root_task_id.clone(), task_id.clone()))
            .or_default() += 1;
        InFlightDispatchGuard {
            state: Arc::clone(self),
            root_task_id,
            task_id,
        }
    }

    async fn wait_for_root(&self, root_task_id: &TaskId) {
        loop {
            // 先创建 notified future，再检查计数，避免检查与 notify 之间丢失唤醒。
            let notified = self.notify.notified();
            let active = self
                .in_flight
                .lock()
                .expect("dispatch quiesce lock poisoned")
                .keys()
                .any(|(root, _)| root == root_task_id);
            if !active {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_execution_scope(
        &self,
        store: &magi_orchestrator::task_store::TaskStore,
        scope: &TaskId,
    ) {
        let audit_root = store
            .get_task(scope)
            .map_or_else(|| scope.clone(), |task| task.root_task_id);
        loop {
            let notified = self.notify.notified();
            let ownership_changed = store.runtime_change_notification();
            let active = self
                .in_flight
                .lock()
                .expect("dispatch quiesce lock poisoned")
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            // 不持有登记锁查询 TaskStore。来源缺失或损坏时保守等待原审计树，
            // 不能把查询失败误当作已转交或资源已经退出。
            if !active.iter().any(|(root, task)| {
                store
                    .execution_scope_root(task)
                    .map_or(root == &audit_root, |owner| owner == *scope)
            }) {
                return;
            }
            tokio::select! {
                _ = notified => {},
                _ = ownership_changed => {},
            }
        }
    }

    fn wait_for_tasks_blocking(&self, task_ids: &[TaskId], timeout: std::time::Duration) -> bool {
        let in_flight = self
            .in_flight
            .lock()
            .expect("dispatch quiesce lock poisoned");
        let (in_flight, _) = self
            .idle
            .wait_timeout_while(in_flight, timeout, |active| {
                active.keys().any(|(_, task)| task_ids.contains(task))
            })
            .expect("dispatch quiesce wait poisoned");
        !in_flight.keys().any(|(_, task)| task_ids.contains(task))
    }

    fn finish(&self, root_task_id: &TaskId, task_id: &TaskId) {
        let mut in_flight = self
            .in_flight
            .lock()
            .expect("dispatch quiesce lock poisoned");
        let key = (root_task_id.clone(), task_id.clone());
        if let Some(count) = in_flight.get_mut(&key) {
            if *count <= 1 {
                in_flight.remove(&key);
            } else {
                *count -= 1;
            }
        }
        drop(in_flight);
        self.idle.notify_all();
        self.notify.notify_waiters();
    }
}

struct InFlightDispatchGuard {
    state: Arc<DispatchQuiesceState>,
    root_task_id: TaskId,
    task_id: TaskId,
}

/// 纯 Conversation 的租期不包含等待会话锁的终态回调。
pub struct ConversationTurnActivityGuard {
    _inner: InFlightDispatchGuard,
}

impl Drop for InFlightDispatchGuard {
    fn drop(&mut self) {
        self.state.finish(&self.root_task_id, &self.task_id);
    }
}

struct ExecutionPlanCleanup<'a> {
    registry: &'a TaskExecutionRegistry,
    task_id: &'a TaskId,
    turn_id: String,
    retained: bool,
}

impl Drop for ExecutionPlanCleanup<'_> {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        let _ = self
            .registry
            .remove_if_turn_matches(self.task_id, &self.turn_id);
    }
}

/// 代理执行代际的 worktree 清理守卫。
///
/// `invoke_llm_with_tools` 可能因为模型桥、工具执行或 Git runtime panic 而提前
/// 退出。清理必须和执行代际绑定，并由同一个 RAII 守卫覆盖成功、失败、取消和
/// panic 四条路径，避免迟到的旧 dispatch 释放新一轮 worktree。
struct AgentWorktreeCleanup {
    dispatcher: LlmTaskDispatcher,
    task: magi_core::Task,
    lease_id: LeaseId,
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
    is_sidechain: bool,
}

impl Drop for AgentWorktreeCleanup {
    fn drop(&mut self) {
        if !self.is_sidechain {
            return;
        }
        let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = self.dispatcher.finalize_agent_worktree(
                &self.task,
                &self.lease_id,
                &self.session_id,
                &self.workspace_id,
                true,
            );
        }));
        if cleanup.is_err() {
            tracing::error!(
                task_id = %self.task.task_id,
                lease_id = %self.lease_id,
                "代理 worktree 清理发生 panic，至少标记该执行代际为 inactive"
            );
            if let Some(registry) = self.dispatcher.session_code_contexts.as_ref() {
                let _ = registry.release_agent_worktree_for_lease(
                    self.session_id.as_str(),
                    self.task.task_id.as_str(),
                    self.lease_id.as_str(),
                );
            }
        }
    }
}

/// 业务派发模型客户端解析的角色目标。
///
/// 三类目标对应 settings.json 中三段独立配置：
/// - [`RoleTarget::Orchestrator`]：`orchestrator` 段——业务主对话的权威入口，
///   携带 `reasoningEffort` 等全套字段。未配置时使用 daemon bootstrap 注入的
///   `default_client`（由 `MAGI_OPENAI_COMPAT_*` 环境变量提供配置）。
/// - [`RoleTarget::Auxiliary`]：`auxiliary` 段——会话标题精修、知识抽取、会话记忆、
///   Prompt 增强等"低价值/低延迟敏感"任务。未配置时返回 `None`，调用方静默跳过。
/// - [`RoleTarget::Agent`]：代理角色，按 `agents[*]` 段查 `engineId` 绑定，
///   再从 `engines[*]` 段取 llm 配置。未绑定 engine（继承 orchestrator 模式）时
///   返回 `None`，调用方应明确继承 orchestrator；已绑定 engine 时只使用该 engine，
///   调用失败必须暴露为该角色模型失败，不能隐藏切换到其它模型。
#[derive(Clone, Copy, Debug)]
pub enum RoleTarget<'a> {
    Orchestrator,
    Auxiliary,
    Agent { role_id: &'a str },
}

/// 角色模型客户端解析的**单一入口**。
///
/// 三段配置（orchestrator / auxiliary / agents+engines）的读取、归一化、HTTP client
/// 构造全部收敛到此函数；调用方按 [`RoleTarget`] 表达意图，
/// 不再各自重复"读 settings → normalize → build client"的样板。
///
/// 返回 `Result<Option<Arc<dyn ModelBridgeClient>>, String>`：
/// - `Ok(Some(client))`：成功解析出 client；
/// - `Ok(None)`：目标未配置（按 target 含义视作正常的"跳过"或"继承"信号）；
/// - `Err(msg)`：配置存在但字段非法时返回，调用方应失败，避免替代模型掩盖坏配置。
///
/// 顶层会话（编排者）的线程 id。
///
/// 线程维度是 Web 引擎绑定键的一部分（R50）：编排者与每个子代理各用一条
/// ChatGPT 临时对话，绝不复用同一条。
pub const ORCHESTRATOR_THREAD_ID: &str = "orchestrator";

/// 会话引擎解析上下文：统一会话引擎工厂 + 本次调用的线程 id。
///
/// 只有命中非 HTTP 会话引擎时才需要；HTTP 引擎路径完全不受影响。
#[derive(Clone)]
pub struct SessionEngineResolutionContext {
    pub factory: Arc<dyn magi_plugin_system::SessionEngineFactory>,
    pub thread_id: String,
    /// MCP 工具的范围必须绑定真实工作区，不得把 session id 当路径根。
    pub project_id: String,
}

fn web_project_id_for_session(
    session_store: &SessionStore,
    session_id: Option<&SessionId>,
) -> String {
    session_id
        .and_then(|id| session_store.session(id))
        .and_then(|session| session.workspace_id)
        .filter(|workspace_id| !workspace_id.trim().is_empty())
        .unwrap_or_default()
}

pub fn resolve_target_for_role(
    settings_store: Option<&Arc<SettingsStore>>,
    default_client: Option<Arc<dyn ModelBridgeClient>>,
    target: RoleTarget<'_>,
    session_id: Option<&SessionId>,
) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
    resolve_target_for_role_with_engine(settings_store, default_client, target, session_id, None)
}

/// 与 [`resolve_target_for_role`] 同一条解析路径，额外允许解析非 HTTP 会话引擎。
///
/// 未提供 [`SessionEngineResolutionContext`] 时，命中非 HTTP 会话引擎**失败关闭**：宁可返回
/// 明确错误，也不退回 HTTP 模型（A19、§5.6）。之所以不改变既有函数签名，是为了
/// 让不涉及会话引擎的调用方（辅助模型、标题精修等）保持原样。
pub fn resolve_target_for_role_with_engine(
    settings_store: Option<&Arc<SettingsStore>>,
    default_client: Option<Arc<dyn ModelBridgeClient>>,
    target: RoleTarget<'_>,
    session_id: Option<&SessionId>,
    engine: Option<&SessionEngineResolutionContext>,
) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
    match target {
        RoleTarget::Orchestrator => {
            if let Some(store) = settings_store
                && let Some(client) = build_orchestrator_client(store, session_id, engine)?
            {
                return Ok(Some(client));
            }
            Ok(default_client)
        }
        RoleTarget::Auxiliary => {
            let Some(store) = settings_store else {
                return Ok(None);
            };
            build_client_from_section(store, "auxiliary")
        }
        RoleTarget::Agent { role_id } => {
            let Some(store) = settings_store else {
                return Ok(None);
            };
            if role_engine_is_chatgpt_web(store, role_id) {
                // W10：GPT Web 只作为会话级入口存在，子代理不使用 Web 引擎
                // （一个会话只有一条网页对话，子代理无法共用）。
                return Err(format!(
                    "角色 {role_id} 绑定了 GPT Web 引擎；GPT Web 只能作为会话入口，子代理请使用其它模型引擎"
                ));
            }
            let Some(role_model) = configured_role_engine_model_config(store, role_id)? else {
                return Ok(None);
            };
            if role_model.config.is_plugin() {
                let engine = engine.ok_or_else(|| {
                    format!(
                        "角色 {} 绑定了插件会话引擎，但当前运行时未装配会话引擎工厂",
                        role_model.template_id
                    )
                })?;
                return engine
                    .factory
                    .build_session_engine(magi_plugin_system::SessionEngineInvocationSpec {
                        session_id: session_id
                            .map(|id| id.as_str().to_string())
                            .unwrap_or_default(),
                        project_id: engine.project_id.clone(),
                        thread_id: engine.thread_id.clone(),
                        engine_id: role_model.engine_id,
                        binding: serde_json::Value::Null,
                    })
                    .map(Some);
            }
            let primary = role_model.config.to_http_model_client().ok_or_else(|| {
                format!(
                    "角色 {} 的模型引擎 {} 缺少可用 HTTP 模型配置",
                    role_model.template_id, role_model.engine_id
                )
            })?;
            let primary = Arc::new(primary) as Arc<dyn ModelBridgeClient>;
            Ok(Some(primary))
        }
    }
}

/// 内部 helper：从 settings 指定段（"orchestrator" / "auxiliary"）读取并构造 client。
///
/// 未配置（缺 base_url）时返回 `None`，表示该辅助能力静默跳过。
fn build_client_from_section(
    settings_store: &Arc<SettingsStore>,
    section: &str,
) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
    let config = settings_store.get_section(section);
    let normalized = NormalizedModelConfig::from_settings_value(&config)
        .map_err(|error| format!("{section} 模型配置无效：{error}"))?;
    Ok(normalized
        .to_http_model_client()
        .map(|client| Arc::new(client) as Arc<dyn ModelBridgeClient>))
}

/// 构造业务主对话（orchestrator）的 client。
///
/// 主对话采用「全局连接 base + 会话级模型覆盖」两层模型：
/// - 全局 `orchestrator` 段是权威连接 base，只承载 baseUrl / apiKey / urlMode；
/// - 会话级 `orchestrator` 覆盖段只携带 `model` 与 `reasoningEffort` 两个字段，
///   在解析时叠加到全局 base 之上，使各会话能独立切换主模型与思考强度而互不污染。
///
/// 当 `session_id` 缺失或会话级覆盖为空时，不构造主对话 client，避免隐藏默认模型。
fn build_orchestrator_client(
    settings_store: &Arc<SettingsStore>,
    session_id: Option<&SessionId>,
    engine: Option<&SessionEngineResolutionContext>,
) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
    let normalized = resolve_orchestrator_model_config(settings_store, session_id)?;
    if normalized.is_chatgpt_web() || normalized.is_plugin() {
        let engine_id = orchestrator_engine_id(settings_store, session_id)
            .ok_or_else(|| "会话绑定的会话引擎已失效".to_string())?;
        let client = build_session_engine_client(settings_store, session_id, engine, engine_id)?;
        return Ok(Some(client));
    }
    Ok(normalized
        .to_http_model_client()
        .map(|client| Arc::new(client) as Arc<dyn ModelBridgeClient>))
}

/// 构造会话引擎 client。所有非 HTTP 会话引擎都经过这里：会话、线程和引擎身份被闭包进
/// 唯一工厂。只有 GPT Web 适配器读取 `webConversation`；普通插件引擎收到空的 opaque
/// binding，避免把站点专用状态伪装成通用插件配置。
fn build_session_engine_client(
    settings_store: &Arc<SettingsStore>,
    session_id: Option<&SessionId>,
    engine: Option<&SessionEngineResolutionContext>,
    engine_id: String,
) -> Result<Arc<dyn ModelBridgeClient>, String> {
    let engine = engine.ok_or_else(|| {
        "会话引擎需要已注册工厂，但当前运行时未装配（不允许退回 HTTP 模型）".to_string()
    })?;
    let session_value = session_id
        .map(|id| id.as_str().to_string())
        .unwrap_or_default();
    let binding = if magi_web_model::is_chatgpt_web_engine_id(&engine_id) {
        let web_binding = session_id
            .map(|id| session_web_conversation_binding(settings_store, id))
            .unwrap_or_else(magi_web_model::WebConversationBinding::temporary);
        serde_json::to_value(web_binding).map_err(|_| "Web 引擎绑定无法序列化".to_string())?
    } else {
        serde_json::Value::Null
    };
    let spec = magi_plugin_system::SessionEngineInvocationSpec {
        session_id: session_value,
        project_id: engine.project_id.clone(),
        thread_id: engine.thread_id.clone(),
        engine_id,
        binding,
    };
    engine.factory.build_session_engine(spec)
}

/// daemon 未注入 [`ContextBudget`] 时，为最小运行时和测试构造提供的默认预算。
///
/// `max_memory` 必须 ≥ 一批 session-memory 的 slice 数（当前 5），否则会出现
/// "辅助模型提取了 5 条会话记忆，预算却只放 2 条进 prompt"的设计错位。
fn default_context_budget_for_unconfigured_dispatcher() -> ContextBudget {
    ContextBudget {
        max_turns: 8,
        max_knowledge: 6,
        max_memory: 8,
        max_shared_items: 4,
        max_file_summaries: 4,
    }
}

fn refresh_file_summary_projection(
    context_runtime: &ContextRuntime,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &WorkspaceId,
) {
    let facts = current_session_file_facts(session_store, session_id)
        .into_iter()
        .map(|fact| FileSummaryRecord {
            item: FileSummaryItem {
                absolute_path: fact.absolute_path,
                summary: fact.summary,
            },
            workspace_id: Some(workspace_id.clone()),
            project_key: None,
            updated_at: UtcMillis::now(),
        })
        .collect::<Vec<_>>();
    context_runtime
        .file_summary_store()
        .replace_scope(Some(workspace_id), None, facts);
}

impl LlmTaskDispatcher {
    /// 显式转交是资源交接边界：在原父 Turn 仍有效时，为转交根冻结其独立
    /// worktree。仍复用首次本地执行的唯一准备入口，不创建新 Task/lease。
    /// 转交后首次工具或孙代理沿该副本准备，不再获取主线执行租约。
    pub fn prepare_agent_run_transfer_resources(
        &self,
        task_id: &TaskId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<(), String> {
        let store = self.pipeline.execution_runtime.task_store();
        let task = store.get_task(task_id).ok_or("转交代理不存在")?;
        let lease = store
            .get_active_lease(task_id)
            .ok_or("转交代理没有活动执行租约")?;
        if task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.command_mode == "no_tools")
        {
            return Ok(());
        }
        let root = self.resolve_workspace_root_path(session_id, workspace_id);
        self.prepare_task_workspace_execution(
            &task,
            &lease.lease_id,
            session_id,
            workspace_id,
            root.as_ref(),
            true,
            Some(&lease.worker_id),
        )?;
        Ok(())
    }

    fn prepare_workspace_execution(
        &self,
        task_id: &TaskId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<Option<Arc<magi_snapshot::SnapshotSession>>, String> {
        let Some(prepare) = self.workspace_execution_preparer.as_ref() else {
            return Ok(None);
        };
        let turn_id = self
            .execution_registry
            .turn_id(task_id)
            .ok_or_else(|| "准备执行资源时缺少 Turn 身份".to_string())?;
        let mut source = turn_id;
        let mut seen = std::collections::HashSet::new();
        while let Some(binding) = self
            .session_store
            .canonical_turn_for_session_turn_id(session_id, &source)
            .and_then(|turn| turn.agent_turn_binding().ok().flatten())
        {
            if !seen.insert(source.clone()) {
                return Err("Agent Turn 来源链循环".into());
            }
            source = binding.parent_turn_id;
        }
        prepare(session_id, workspace_id, &source)
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_task_workspace_execution(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        execution_root: Option<&PathBuf>,
        is_sidechain: bool,
        worker_id: Option<&WorkerId>,
    ) -> Result<conversation_loop::PreparedWorkspaceExecution, String> {
        let execution_guard = crate::task_execution_guard::TaskExecutionGuard::new(
            self.pipeline.execution_runtime.task_store(),
            &self.session_store,
            &self.execution_registry,
            session_id,
            &task.task_id,
            lease_id,
        );
        let execution_is_active = || execution_guard.is_active();
        if !execution_is_active() {
            return Err("准备执行资源前任务租约已失效".into());
        }
        // 仓库准备可能等另一个任务释放资源，不能同时占住它继续执行所需的模型名额。
        let waiting = self
            .execution_registry
            .execution_admission()
            .suspend_while_waiting(&task.task_id);
        let prepared = (|| -> Result<conversation_loop::PreparedWorkspaceExecution, String> {
            let identity_root = workspace_id
                .as_ref()
                .and_then(|_| self.resolve_workspace_root_path(session_id, workspace_id));
            let transferred = self
                .pipeline
                .execution_runtime
                .task_store()
                .agent_run_transfer_for_task(&task.task_id)
                .map_err(|error| error.to_string())?
                .is_some();
            let snapshot = if transferred {
                // 转交根的独立副本已在交接前准备；嵌套任务从直接父副本出发。
                // 主线可能已结束或换到下一轮，不能再准备/改动它的工作区状态。
                None
            } else {
                self.prepare_workspace_execution(&task.task_id, session_id, workspace_id)?
            };
            let main_root = snapshot
                .as_ref()
                .map(|snapshot| snapshot.workspace_root().to_path_buf());
            if !execution_is_active() {
                return Err("准备执行资源期间任务或父 Turn 已停止".into());
            }
            let root = self.resolve_task_execution_root(
                task,
                lease_id,
                session_id,
                workspace_id,
                main_root.as_ref().or(execution_root),
                is_sidechain,
                worker_id,
            )?;
            if !execution_is_active() {
                return Err("准备执行资源期间任务租约已失效".into());
            }
            Ok(conversation_loop::PreparedWorkspaceExecution {
                task: Self::task_for_execution_root(task, identity_root.as_ref(), root.as_ref()),
                // Git 子代理的变更属于代理分支；不能让其相对路径写入主会话账本。
                snapshot: snapshot
                    .filter(|snapshot| root.as_deref() == Some(snapshot.workspace_root())),
                root,
            })
        })();
        if waiting
            .as_ref()
            .is_some_and(|guard| !guard.resume(execution_is_active))
        {
            return Err("准备执行资源后任务已停止，未取得执行名额".into());
        }
        prepared
    }

    pub fn new(
        event_bus: Arc<InMemoryEventBus>,
        pipeline: ExecutionPipeline,
        dependencies: LlmTaskDispatcherDependencies,
        mission_state_root: PathBuf,
    ) -> Self {
        let LlmTaskDispatcherDependencies {
            session_store,
            execution_registry,
            completion_sink,
            conversation_registry,
            agent_role_registry,
        } = dependencies;
        Self {
            event_bus,
            pipeline,
            session_store,
            execution_registry,
            completion_sink,
            completion_notifier: None,
            model_bridge_client: None,
            session_engine_factory: None,
            workflow_core_factory: None,
            plugin_command_executor: None,
            model_client_cache: Arc::new(Mutex::new(HashMap::new())),
            dispatch_quiesce: Arc::new(DispatchQuiesceState::default()),
            conversation_quiesce: Arc::new(DispatchQuiesceState::default()),
            knowledge_store: None,
            knowledge_persist_callback: None,
            session_state_persist_callback: None,
            settings_store: None,
            context_runtime: None,
            context_budget: None,
            workspace_registry: None,
            git_service: None,
            session_code_contexts: None,
            workspace_git_coordinator: None,
            agent_worktree_root: mission_state_root.join("worktrees"),
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            snapshot_manager: None,
            workspace_execution_preparer: None,
            conversation_registry,
            agent_role_registry,
            project_memory_registry: Arc::new(
                magi_project_memory::ProjectMemoryRegistry::with_home(mission_state_root),
            ),
        }
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    /// 注入 Web 引擎的 client 工厂（宿主实现，见 `magi-api` 的 `WebModelHostFactory`）。
    pub fn with_session_engine_factory(
        mut self,
        factory: Arc<dyn magi_plugin_system::SessionEngineFactory>,
    ) -> Self {
        self.session_engine_factory = Some(factory);
        self
    }

    pub fn with_workflow_core_factory(
        mut self,
        factory: Arc<dyn magi_plugin_system::WorkflowCoreFactory>,
    ) -> Self {
        self.workflow_core_factory = Some(factory);
        self
    }

    pub fn with_plugin_command_executor(
        mut self,
        executor: Arc<dyn magi_plugin_system::PluginCommandExecutor>,
    ) -> Self {
        self.plugin_command_executor = Some(executor);
        self
    }

    pub fn build_workflow_core(
        &self,
        spec: magi_plugin_system::WorkflowCoreInvocationSpec,
    ) -> Result<Arc<dyn magi_plugin_system::workflow::WorkflowCore>, String> {
        let factory = self
            .workflow_core_factory
            .as_ref()
            .ok_or_else(|| "工作流核心工厂未装配".to_string())?;
        factory.build_workflow_core(spec)
    }

    /// 绑定 Task/Turn 来源与完成交付到同一通知器；结果直接提交 TaskStore，
    /// 再唤醒 Turn 侧，不经内存结果队列或 Runner 轮询。
    pub fn with_completion_notifier(mut self, notifier: Arc<TaskCompletionNotifier>) -> Self {
        self.completion_sink = notifier.clone();
        self.completion_notifier = Some(notifier);
        self
    }

    pub fn with_knowledge_store(mut self, store: Arc<KnowledgeStore>) -> Self {
        self.knowledge_store = Some(store);
        self
    }

    pub fn with_knowledge_persist_callback(
        mut self,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.knowledge_persist_callback = Some(callback);
        self
    }

    pub fn with_session_state_persist_callback(
        mut self,
        callback: Arc<SessionStatePersistCallback>,
    ) -> Self {
        self.session_state_persist_callback = Some(callback);
        self
    }

    pub fn with_settings_store(mut self, store: Arc<SettingsStore>) -> Self {
        self.settings_store = Some(store);
        self
    }

    pub fn with_context_runtime(mut self, runtime: Arc<ContextRuntime>) -> Self {
        self.context_runtime = Some(runtime);
        self
    }

    /// 注入 daemon 配置的 [`ContextBudget`]。dispatch summary 与 prompt 注入两条路径必须
    /// 使用同一份预算，避免"UI 看到 6 条候选、实际只投放 2 条"这类分裂。
    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.context_budget = Some(budget);
        self
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceStore>) -> Self {
        self.workspace_registry = Some(registry);
        self
    }

    pub fn with_git_context_runtime(
        mut self,
        git_service: Arc<magi_git::GitService>,
        session_code_contexts: magi_git::SessionCodeContextRegistry,
    ) -> Self {
        self.git_service = Some(git_service);
        self.session_code_contexts = Some(session_code_contexts);
        self
    }

    pub fn with_workspace_git_coordinator(
        mut self,
        coordinator: magi_git::WorkspaceGitOperationCoordinator,
    ) -> Self {
        self.workspace_git_coordinator = Some(coordinator);
        self
    }

    pub fn with_tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn with_skill_runtime(mut self, runtime: Arc<magi_skill_runtime::SkillRuntime>) -> Self {
        self.skill_runtime = Some(runtime);
        self
    }

    fn resolve_registered_skill_id(
        &self,
        requested: Option<&str>,
    ) -> Result<Option<String>, String> {
        let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let unavailable = || {
            format!(
                "skill_unavailable: 已选择的 Skill {requested} 当前不可用，请重新选择有效 Skill"
            )
        };
        let runtime = self.skill_runtime.as_ref().ok_or_else(unavailable)?;
        match runtime.registry().resolve_skill_id(requested) {
            magi_skill_runtime::SkillIdResolution::Found(skill_id) => Ok(Some(skill_id)),
            // 配置撤销不能降级成无 Skill 的全工具执行。
            magi_skill_runtime::SkillIdResolution::NotFound => Err(unavailable()),
            magi_skill_runtime::SkillIdResolution::Ambiguous(_) => Err(format!(
                "skill_ambiguous: 已选择的 Skill {requested} 名称不唯一，请选择完整 Skill ID"
            )),
        }
    }

    pub fn with_skill_dispatch_runtime(
        mut self,
        runtime: Arc<magi_skill_runtime::SkillDispatchRuntime>,
    ) -> Self {
        self.skill_dispatch_runtime = Some(runtime);
        self
    }

    pub fn with_snapshot_manager(mut self, manager: Arc<magi_snapshot::SnapshotManager>) -> Self {
        self.snapshot_manager = Some(manager);
        self
    }

    pub fn with_workspace_execution_preparer(
        mut self,
        prepare: Arc<WorkspaceExecutionPreparer>,
    ) -> Self {
        self.workspace_execution_preparer = Some(prepare);
        self
    }

    pub fn with_project_memory_registry(
        mut self,
        registry: Arc<magi_project_memory::ProjectMemoryRegistry>,
    ) -> Self {
        self.project_memory_registry = registry;
        self
    }

    pub fn project_memory_registry(&self) -> Arc<magi_project_memory::ProjectMemoryRegistry> {
        self.project_memory_registry.clone()
    }

    fn publish_task_dispatched_event(&self, input: TaskDispatchedEventInput<'_>) {
        let TaskDispatchedEventInput {
            task_id,
            mission_id,
            worker,
            lease_id,
            kind,
            session_id,
            workspace_id,
        } = input;
        let event = EventEnvelope::domain(
            EventId::unique("event-task-dispatched"),
            "task.dispatched",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": mission_id.to_string(),
                "session_id": session_id.map(ToString::to_string),
                "workspace_id": workspace_id.map(ToString::to_string),
                "worker_id": worker.worker_id.to_string(),
                "role": worker.role,
                "lease_id": lease_id.to_string(),
                "kind": format!("{:?}", kind),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: session_id.cloned(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn notify_task_result(&self, task_id: &TaskId, lease_id: &LeaseId, outcome: TaskOutcome) {
        self.completion_sink.notify(TaskResult {
            task_id: task_id.clone(),
            lease_id: lease_id.clone(),
            outcome,
        });
    }

    fn execute_dispatch_plan(&self, input: DispatchPlanExecutionInput<'_>) -> TaskOutcome {
        let DispatchPlanExecutionInput {
            task,
            lease_id,
            session_id,
            turn_id,
            workspace_id,
            execution_root,
            writebacks,
            use_tools,
            skill_name,
            images,
            usage_binding,
            is_sidechain,
            worker_id,
            worker_role,
            thread_id,
            system_prompt,
            execution_settings_snapshot,
        } = input;
        let _worktree_cleanup = AgentWorktreeCleanup {
            dispatcher: self.clone(),
            task: task.clone(),
            lease_id: lease_id.clone(),
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            is_sidechain,
        };
        let streaming_entry_id = task_streaming_entry_id(task);
        let (mut outcome, context_summary) = self.invoke_llm_with_tools(TaskLlmInvocationInput {
            task,
            lease_id,
            session_id: &session_id,
            workspace_id: &workspace_id,
            execution_root: execution_root.as_ref(),
            use_tools,
            skill_name,
            images,
            usage_binding: &usage_binding,
            streaming_entry_id: Some(streaming_entry_id.as_str()),
            is_sidechain,
            worker_id: Some(&worker_id),
            execution_role_id: Some(worker_role.as_str()),
            thread_id: &thread_id,
            system_prompt,
            execution_settings_snapshot: execution_settings_snapshot.clone(),
        });
        // canonical 取消先于 TaskStore 的级联清理提交。Guard 会立即让执行退出，
        // 这份回执必须保留取消语义，不能抢先把仍持有旧租约的 Task 写成 Failed。
        if self
            .session_store
            .turn_execution_was_cancelled(&session_id, &turn_id)
        {
            outcome = TaskOutcome::Cancelled;
        }
        if matches!(&outcome, TaskOutcome::Completed { .. }) {
            // Session ownership 已随主线接纳/恢复提交。完成只写回原 Task/Turn，
            // 不用旧执行计划覆盖当前主线或已归档的 Session 归属。
            let should_enrich_session = !writebacks.is_empty();
            writebacks.apply(&self.pipeline.memory_store);
            self.publish_execution_overview(task, &session_id, &workspace_id, context_summary);
            // 完成通知可能立即推进同一 Session 的队列，因此必须先释放
            // 代理 worktree 与临时资源，再提交 TaskStore durable completion。
            // 可写代理的产出（分支、提交、改动路径）随完成结果一起提交，作为回执事实。
            if let Some(workspace_output) = self.finalize_agent_worktree(
                task,
                lease_id,
                &session_id,
                &workspace_id,
                is_sidechain,
            ) && let TaskOutcome::Completed { attempt } = &mut outcome
            {
                attempt.output_refs.push(workspace_output.to_string());
            }
            drop(_worktree_cleanup);
            if should_enrich_session {
                self.schedule_post_completion_enrichment(
                    session_id,
                    turn_id,
                    workspace_id,
                    outcome.clone(),
                    execution_settings_snapshot,
                );
            }
            return outcome;
        }
        drop(_worktree_cleanup);
        outcome
    }

    fn schedule_post_completion_enrichment(
        &self,
        session_id: SessionId,
        turn_id: String,
        workspace_id: Option<WorkspaceId>,
        outcome: TaskOutcome,
        execution_settings_snapshot: Option<Arc<SettingsStore>>,
    ) {
        let dispatcher = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("magi-session-enrichment".to_string())
            .spawn(move || {
                let execution_settings =
                    dispatcher.execution_settings_or_live(execution_settings_snapshot.as_ref());
                dispatcher.extract_and_persist_knowledge(
                    execution_settings,
                    &session_id,
                    Some(&turn_id),
                    &workspace_id,
                    &outcome,
                );
                dispatcher.extract_and_persist_session_memory(
                    execution_settings,
                    &session_id,
                    Some(&turn_id),
                    &workspace_id,
                );
            })
        {
            tracing::warn!(%error, "启动会话后台沉淀线程失败");
        }
    }

    fn extract_and_persist_knowledge(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        workspace_id: &Option<WorkspaceId>,
        outcome: &TaskOutcome,
    ) {
        let Some(store) = self.knowledge_store.as_ref() else {
            return;
        };
        let TaskOutcome::Completed { attempt } = outcome else {
            return;
        };

        let timeline_text = self
            .session_store
            .timeline_for_session(session_id)
            .into_iter()
            .rev()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
                )
            })
            .take(12)
            .filter_map(|entry| timeline_entry_visible_text(&entry.message))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n\n");
        // output_refs 可能包含完整的工具证据（一次大型只读分析可达数百 KB），
        // 不应把它原样复制给辅助模型。知识抽取只需要用户可见的终态答复；
        // 完整证据继续留在 task/session 审计链路中。
        let output_text = attempt
            .final_response
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                attempt
                    .output_refs
                    .iter()
                    .map(|output| public_runtime_excerpt(output, 8_000))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            });
        let extraction_text = format!("{timeline_text}\n\n{output_text}");
        let Some(client) = self
            .resolve_target_cached(settings_store, RoleTarget::Auxiliary, None, "")
            .ok()
            .flatten()
        else {
            self.publish_learning_extraction_diagnostic(
                session_id,
                workspace_id.as_ref(),
                "skipped",
                Some("auxiliary_model_unconfigured"),
                0,
                0,
            );
            return;
        };
        let learnings = match extract_learnings_via_auxiliary(
            AuxiliaryModelExtractionContext {
                client,
                event_bus: self.event_bus.as_ref(),
                session_store: self.session_store.as_ref(),
                settings_store,
                session_id,
                workspace_id,
                expected_turn_id,
            },
            &extraction_text,
        ) {
            Ok(Some(learnings)) => learnings,
            Ok(None) => {
                self.publish_learning_extraction_diagnostic(
                    session_id,
                    workspace_id.as_ref(),
                    "completed",
                    None,
                    0,
                    0,
                );
                return;
            }
            Err(failure) => {
                self.publish_learning_extraction_diagnostic(
                    session_id,
                    workspace_id.as_ref(),
                    "failed",
                    Some(failure.as_str()),
                    0,
                    0,
                );
                return;
            }
        };
        let candidate_count = learnings.len();
        let mut existing = store.list();
        let mut inserted = 0usize;
        for (index, learning) in learnings.into_iter().enumerate() {
            if knowledge_duplicate(
                &existing,
                KnowledgeKind::Learning,
                workspace_id.as_ref(),
                &learning.content,
            ) {
                continue;
            }
            let now = UtcMillis::now();
            let record = KnowledgeRecord {
                knowledge_id: format!("learning-auto-{}-{index}", now.0),
                kind: KnowledgeKind::Learning,
                title: title_from_learning_content(&learning.content),
                content: learning.content,
                tags: learning.tags,
                workspace_id: workspace_id.clone(),
                source_ref: Some(
                    learning
                        .context
                        .unwrap_or_else(|| format!("session:{}", session_id.as_str())),
                ),
                created_at: now,
                updated_at: now,
            };
            store.upsert(record.clone());
            existing.push(record);
            inserted += 1;
        }
        if inserted > 0
            && let Some(callback) = self.knowledge_persist_callback.as_ref()
        {
            callback();
        }
        self.publish_learning_extraction_diagnostic(
            session_id,
            workspace_id.as_ref(),
            "completed",
            None,
            candidate_count,
            inserted,
        );
    }

    /// 走辅助模型把当前会话压缩成 5 类结构化记忆（currentWork / decisions /
    /// importantContext / pendingIssues / nextSteps）。
    ///
    /// 调用时机：与 `extract_and_persist_knowledge` 并列，主 action task 完成且
    /// `writebacks` 非空时触发。配合水位线（自上一次会话记忆抽取以来累计 token
    /// 超过 `SESSION_MEMORY_WATERLINE_TOKENS`）才真正调用 LLM，避免每轮都跑。
    ///
    /// 辅助模型未配置、调用失败、JSON 解析异常一律静默跳过（`tracing::debug!`），
    /// 不做任何"退回到 marker 写回"之类的兜底 —— 与现有辅助模型路径同语义。
    fn extract_and_persist_session_memory(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        workspace_id: &Option<WorkspaceId>,
    ) {
        let Some(client) = self
            .resolve_target_cached(settings_store, RoleTarget::Auxiliary, None, "")
            .ok()
            .flatten()
        else {
            return;
        };

        let timeline = self.session_store.timeline_for_session(session_id);
        let last_extraction_at = self
            .pipeline
            .memory_store
            .extraction_results_for_session(session_id)
            .into_iter()
            .filter(|record| {
                record
                    .source_ref
                    .as_deref()
                    .is_some_and(|s| s.starts_with(SESSION_MEMORY_SOURCE_PREFIX))
            })
            .map(|record| record.created_at)
            .next_back();

        let excerpt_entries: Vec<_> = timeline
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
                )
            })
            .filter(|entry| match last_extraction_at {
                Some(ts) => entry.occurred_at.0 > ts.0,
                None => true,
            })
            .collect();

        let excerpt_text = excerpt_entries
            .iter()
            .filter_map(|entry| {
                let text = timeline_entry_visible_text(&entry.message)?;
                let prefix = match entry.kind {
                    TimelineEntryKind::UserMessage => "用户",
                    TimelineEntryKind::AssistantMessage => "助手",
                    _ => "其他",
                };
                Some(format!("[{prefix}] {text}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        if excerpt_text.is_empty() {
            return;
        }
        if estimate_session_memory_tokens(&excerpt_text) < SESSION_MEMORY_WATERLINE_TOKENS {
            return;
        }

        let Some(slices) = extract_session_memory_via_auxiliary(
            AuxiliaryModelExtractionContext {
                client,
                event_bus: self.event_bus.as_ref(),
                session_store: self.session_store.as_ref(),
                settings_store,
                session_id,
                workspace_id,
                expected_turn_id,
            },
            &excerpt_text,
        ) else {
            return;
        };
        if slices.is_empty() {
            return;
        }

        let now = UtcMillis::now();
        let extraction_id = format!("extract-session-memory-{}-{}", session_id.as_str(), now.0);
        let source_ref = format!(
            "{SESSION_MEMORY_SOURCE_PREFIX}{}/{}",
            session_id.as_str(),
            now.0
        );
        let memories = slices
            .into_iter()
            .enumerate()
            .map(|(index, slice)| ExtractedMemory {
                memory_id: format!(
                    "mem-session-memory-{}-{}-{index}",
                    session_id.as_str(),
                    now.0
                ),
                layer: MemoryLayer::Recent,
                content: format!("[{}] {}", slice.category, slice.content),
                created_at: now,
            })
            .collect::<Vec<_>>();

        self.pipeline
            .memory_store
            .apply_extraction(MemoryExtractionApplyRequest {
                extraction_id,
                session_id: session_id.clone(),
                source_ref: Some(source_ref),
                summary: "辅助模型会话记忆抽取".to_string(),
                memories,
                created_at: now,
            });
    }

    fn publish_execution_overview(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        context_summary: Option<ExecutionContextSummary>,
    ) {
        let context_payload = context_summary
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let event = EventEnvelope::audit(
            EventId::unique("event-mission-overview"),
            "mission.execution.overview",
            serde_json::json!({
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "context": context_payload,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn publish_knowledge_context_diagnostic(
        &self,
        selection: &KnowledgeContextSelection,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
        mission_id: Option<&magi_core::MissionId>,
        task_id: Option<&TaskId>,
    ) {
        let event = EventEnvelope::audit(
            EventId::unique(format!("event-knowledge-context-{}", session_id)),
            "knowledge.context.selected",
            serde_json::json!({
                "consumer": selection.consumer,
                "decision": selection.decision,
                "knowledge_ids": selection.results.iter().map(|item| item.knowledge_id.clone()).collect::<Vec<_>>(),
                "result_kinds": selection.results.iter().map(|item| knowledge_kind_label(item.kind)).collect::<Vec<_>>(),
                "matched_count": selection.matched_count,
                "injected_count": selection.results.len(),
                "injected_chars": selection.injected_chars,
                "graph_focus_count": selection.graph_context.len(),
                "graph_injected_chars": selection.graph_context.iter().map(|graph| graph.injected_chars).sum::<usize>(),
                "graph_node_count": selection.graph_context.iter().map(|graph| graph.nodes.len()).sum::<usize>(),
                "graph_edge_count": selection.graph_context.iter().map(|graph| graph.edges.len()).sum::<usize>(),
                "graph_candidate_edge_count": selection.graph_context.iter().map(|graph| graph.candidate_edge_count).sum::<usize>(),
                "graph_inferred_edge_count": selection.graph_context.iter().map(|graph| graph.inferred_edge_count).sum::<usize>(),
                "graph_dangling_edge_count": selection.graph_context.iter().map(|graph| graph.dangling_edge_count).sum::<usize>(),
                "graph_truncated": selection.graph_context.iter().any(|graph| graph.truncated),
                "truncated": selection.truncated,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            mission_id: mission_id.cloned(),
            task_id: task_id.cloned(),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn publish_learning_extraction_diagnostic(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
        status: &str,
        failure_reason: Option<&str>,
        candidate_count: usize,
        inserted_count: usize,
    ) {
        let event = EventEnvelope::audit(
            EventId::unique(format!("event-learning-extraction-{}", session_id)),
            "knowledge.learning.extraction",
            serde_json::json!({
                "status": status,
                "failure_reason": failure_reason,
                "candidate_count": candidate_count,
                "inserted_count": inserted_count,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    fn build_tool_definitions(
        &self,
        task: &magi_core::Task,
        skill_name: Option<&str>,
        access_profile: AccessProfile,
        workspace_id: Option<&WorkspaceId>,
    ) -> Vec<ChatToolDefinition> {
        // Skill 内容、可选清单与宿主能力均可热更新，名称与 Task 策略不足以
        // 标识目录版本；每次从权威注册状态生成，避免发布旧权限或失效选项。
        let definitions =
            self.build_base_tool_definitions(task, skill_name, access_profile, workspace_id);
        let task_policy = task.policy_snapshot.as_ref();
        let Some(registry) = self.tool_registry.as_ref() else {
            return definitions;
        };
        let definitions =
            refresh_live_mcp_tool_definitions_with_mode(RefreshLiveMcpToolDefinitionsInput {
                definitions,
                tool_registry: registry,
                skill_runtime: self.skill_runtime.as_deref(),
                active_skill_id: skill_name,
                access_profile: tool_surface_access_profile(task, access_profile),
                allowed_tools: task_policy
                    .filter(|policy| !policy.allowed_tools.is_empty())
                    .map(|policy| policy.allowed_tools.as_slice()),
                denied_tools: task_policy
                    .map(|policy| policy.denied_tools.as_slice())
                    .unwrap_or_default(),
                include_external: false,
                workspace_id,
            });
        constrain_task_tool_definitions(definitions, task_policy)
    }

    fn build_base_tool_definitions(
        &self,
        task: &magi_core::Task,
        skill_name: Option<&str>,
        access_profile: AccessProfile,
        workspace_id: Option<&WorkspaceId>,
    ) -> Vec<ChatToolDefinition> {
        let Some(ref registry) = self.tool_registry else {
            return Vec::new();
        };
        if task
            .policy_snapshot
            .as_ref()
            .is_some_and(|policy| policy.command_mode.eq_ignore_ascii_case("no_tools"))
        {
            return Vec::new();
        }
        let tool_surface_access_profile = tool_surface_access_profile(task, access_profile);
        let active_skill_policy = active_skill_tool_execution_policy(
            tool_surface_access_profile,
            self.skill_runtime.as_deref(),
            skill_name,
        );
        let active_skill_allowed_tools = active_skill_policy.allowed_tool_names.as_deref();
        let mut definitions = public_builtin_tool_definitions(registry)
            .into_iter()
            .filter(|definition| {
                BuiltinToolName::from_name(definition.function.name.as_str()).is_some_and(|tool| {
                    task_can_see_builtin_tool(
                        Some(task),
                        Some(self.agent_role_registry.as_ref()),
                        tool,
                    ) && (workspace_id.is_some() || !tool.requires_workspace_context())
                        && builtin_tool_visible_in_access_profile(tool, tool_surface_access_profile)
                        && registry.browser_tool_available(tool)
                })
            })
            .filter(|definition| {
                (task.is_goal_mode()
                    && matches!(
                        definition.function.name.as_str(),
                        "get_goal" | "create_goal" | "update_goal" | "update_plan"
                    ))
                    || definition.function.name == "update_execution_steps"
                    || active_skill_allowed_tools.is_none_or(|allowed_tools| {
                        allowed_tools
                            .iter()
                            .any(|tool_name| tool_name == &definition.function.name)
                    })
            })
            .filter(|definition| definition.function.name != SKILL_APPLY_TOOL_NAME)
            .collect::<Vec<_>>();
        if self.skill_runtime.is_some() && skill_name.is_none() {
            definitions.push(skill_apply_tool_definition(
                self.skill_runtime
                    .as_deref()
                    .expect("skill runtime checked above"),
            ));
        }
        if let (Some(skill_name), Some(skill_runtime)) = (skill_name, self.skill_runtime.as_ref()) {
            let plan = skill_runtime.build_tool_runtime_plan(magi_skill_runtime::SkillSelection {
                skill_ids: vec![skill_name.to_string()],
                requested_tools: Vec::new(),
            });
            definitions.extend(build_skill_custom_tool_definitions(
                skill_name,
                &plan,
                tool_surface_access_profile,
            ));
        }
        enforce_agent_coordination_surface(definitions)
    }
}

fn task_streaming_entry_id(task: &magi_core::Task) -> String {
    format!("timeline-streaming-{}", task.task_id)
}

fn sanitize_git_path_component(value: &str) -> String {
    let value = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let value = value.trim_matches('-');
    if value.is_empty() {
        "allocation".to_string()
    } else {
        value.chars().take(80).collect()
    }
}

fn project_policy_paths_to_execution_root(
    paths: &[String],
    workspace_identity_root: &Path,
    execution_root: &Path,
) -> Vec<String> {
    let identity_root =
        magi_tool_runtime::canonicalize_tool_permission_path(workspace_identity_root);
    paths
        .iter()
        .map(|path| {
            let source = magi_tool_runtime::canonicalize_tool_permission_path(Path::new(path));
            let Ok(relative) = source.strip_prefix(&identity_root) else {
                return path.clone();
            };
            if relative.as_os_str().is_empty() {
                execution_root.to_string_lossy().into_owned()
            } else {
                execution_root.join(relative).to_string_lossy().into_owned()
            }
        })
        .collect()
}

fn block_on_git<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("agent Git runtime should build")
                    .block_on(future)
            })
            .join()
            .expect("agent Git runtime thread should not panic")
    })
}

fn tool_surface_access_profile(
    task: &magi_core::Task,
    access_profile: AccessProfile,
) -> AccessProfile {
    let Some(policy) = task.policy_snapshot.as_ref() else {
        return access_profile;
    };
    policy.effective_access_profile()
}

fn builtin_tool_visible_in_access_profile(
    tool: BuiltinToolName,
    access_profile: AccessProfile,
) -> bool {
    access_profile != AccessProfile::ReadOnly || !tool.is_access_profile_write_operation()
}

impl LlmTaskDispatcher {
    fn resolve_workspace_root_path(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        if let Some(context) = self
            .session_code_contexts
            .as_ref()
            .and_then(|registry| registry.get(session_id.as_str()))
        {
            return Some(context.execution_root);
        }
        let workspace_id = workspace_id.as_ref()?;
        self.workspace_registry
            .as_ref()?
            .workspaces()
            .into_iter()
            .find(|workspace| workspace.workspace_id == *workspace_id)
            .map(|workspace| workspace.native_root_path())
    }

    /// 子代理永不直接复用主会话 live worktree：只读任务拿 detached worktree，
    /// 可写任务拿唯一临时 branch + 独立 worktree。两者都从直接父任务当前工作树快照出发。
    #[allow(clippy::too_many_arguments)]
    fn resolve_task_execution_root(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        execution_root: Option<&PathBuf>,
        is_sidechain: bool,
        worker_id: Option<&WorkerId>,
    ) -> Result<Option<PathBuf>, String> {
        let main_root = execution_root
            .cloned()
            .or_else(|| self.resolve_workspace_root_path(session_id, workspace_id));
        if !is_sidechain {
            return Ok(main_root);
        }
        let context = self
            .session_code_contexts
            .as_ref()
            .and_then(|registry| registry.get(session_id.as_str()));
        if let Some(context) = context.as_ref()
            && let Some(coordinator) = self.workspace_git_coordinator.as_ref()
        {
            coordinator
                .begin_execution(session_id.as_str(), &context.git.git_common_dir)
                .map_err(|error| error.to_string())?;
        }
        let Some(registry) = self.session_code_contexts.as_ref() else {
            if task.agent_run_transfer().is_some() {
                return Err("转交代理缺少原执行目录注册表，不能恢复本地执行".into());
            }
            return Ok(main_root);
        };
        let Some(context) = context else {
            if task.agent_run_transfer().is_some() {
                return Err("转交代理缺少原执行目录上下文，不能回退到主线目录".into());
            }
            // 非 Git workspace 没有 SessionGitContext，只能沿用普通 workspace 语义。
            return Ok(main_root);
        };
        let git_service = self
            .git_service
            .as_ref()
            .ok_or_else(|| "Git service 未注入，无法隔离子代理".to_string())?;
        if let Some(existing) = context.agent_worktrees.iter().find(|worktree| {
            worktree.task_id == task.task_id.as_str()
                && worktree.active
                && worktree.lease_id.as_deref() == Some(lease_id.as_str())
        }) {
            if existing.path.is_dir() {
                return Ok(Some(existing.path.clone()));
            }
            return Err(format!(
                "执行租约 {} 对应的 agent worktree 不可用",
                lease_id
            ));
        }
        if task.agent_activation().is_some() {
            // daemon 重启会撤销旧 lease，但保留 Agent 的 worktree 目录。恢复时
            // 必须复用这份原目录，并验证它仍属于同一 repository；不能从主线 HEAD
            // 重新创建一份看似干净的副本，悄悄丢掉代理留下的文件和分支。
            let previous_allocations = context
                .agent_worktrees
                .iter()
                .filter(|worktree| worktree.task_id == task.task_id.as_str())
                .collect::<Vec<_>>();
            if previous_allocations.len() > 1 {
                return Err("代理存在多份历史 worktree，不能猜测恢复来源".into());
            }
            if let Some(previous) = previous_allocations.first().copied() {
                let worker = worker_id.ok_or("恢复代理 worktree 缺少当前 Worker")?;
                let store = self.pipeline.execution_runtime.task_store();
                let current = store
                    .get_active_lease(&task.task_id)
                    .filter(|lease| &lease.lease_id == lease_id)
                    .ok_or("恢复代理 worktree 当前租约已失效")?;
                if previous.active
                    || current.root_task_id != task.root_task_id
                    || previous.worker_id != worker.as_str()
                {
                    return Err("代理原 worktree 仍占用或当前租约归属变化".into());
                }
                previous
                    .lease_id
                    .as_deref()
                    .and_then(|id| {
                        self.pipeline
                            .execution_runtime
                            .task_store()
                            .get_lease(&LeaseId::new(id))
                    })
                    .filter(|lease| {
                        lease.task_id == task.task_id
                            && lease.root_task_id == task.root_task_id
                            && lease.role == current.role
                            && lease.lease_status
                                != magi_orchestrator::task_store::TaskLeaseState::Active
                    })
                    .ok_or("原代理 worktree 缺少已结算的来源租约，拒绝恢复")?;
                let mode = if task
                    .policy_snapshot
                    .as_ref()
                    .map(magi_core::TaskPolicy::effective_access_profile)
                    .unwrap_or_default()
                    == AccessProfile::ReadOnly
                {
                    magi_git::AgentWorktreeMode::ReadOnly
                } else {
                    magi_git::AgentWorktreeMode::Writable
                };
                if mode != previous.mode {
                    return Err("代理恢复工具范围与原 worktree 模式不一致".into());
                }
                block_on_git(git_service.recover_agent_worktree(
                    &context.git.worktree_path,
                    &context.git.git_common_dir,
                    previous,
                ))
                .map_err(|error| format!("恢复代理 worktree 失败: {error}"))?;
                registry
                    .reattach_agent_worktree(
                        session_id.as_str(),
                        previous,
                        lease_id.as_str(),
                        worker.as_str(),
                    )
                    .map_err(|error| format!("恢复代理 worktree 记录失败: {error}"))?;
                if let Some(persist) = self.session_state_persist_callback.as_deref()
                    && let Err(error) = persist("agent_worktree_reattached")
                {
                    let _ = registry.release_agent_worktree_for_lease(
                        session_id.as_str(),
                        task.task_id.as_str(),
                        lease_id.as_str(),
                    );
                    return Err(format!("恢复代理 worktree 保存失败: {error}"));
                }
                return Ok(Some(previous.path.clone()));
            }
            let current_turn = self.execution_registry.turn_id(&task.task_id);
            let used_local_tools = self
                .session_store
                .canonical_turns_for_session(session_id)
                .iter()
                .filter(|turn| Some(turn.turn_id.as_str()) != current_turn.as_deref())
                .flat_map(|turn| &turn.items)
                .filter(|item| {
                    item.worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        == Some(&task.task_id)
                })
                .filter_map(|item| item.tool.as_ref())
                .any(|tool| {
                    tool_requires_execution_resources(
                        &tool.name,
                        &tool
                            .arguments
                            .as_ref()
                            .map(serde_json::Value::to_string)
                            .unwrap_or_else(|| "{}".into()),
                        true,
                    )
                });
            if task.agent_run_transfer().is_some() || used_local_tools {
                return Err("代理缺少原本地执行 worktree 记录，拒绝从当前主线重建基线".into());
            }
        }
        if context.has_external_drift() {
            return Err(format!(
                "session Git context 已漂移，拒绝为子代理 {} 分配 worktree",
                task.task_id
            ));
        }
        let Some(base_head) = context.git.base_head.clone() else {
            return Err("当前 Git session 没有 base HEAD，无法隔离子代理".to_string());
        };
        let workspace_key = workspace_id
            .as_ref()
            .map(|value| sanitize_git_path_component(value.as_str()))
            .unwrap_or_else(|| "workspace".to_string());
        let session_key = sanitize_git_path_component(session_id.as_str());
        let task_key = sanitize_git_path_component(task.task_id.as_str());
        let allocation_nonce = UtcMillis::now().0;
        let path = self
            .agent_worktree_root
            .join(workspace_key)
            .join(session_key)
            .join(format!("{task_key}-{allocation_nonce}"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 agent worktree 父目录失败: {error}"))?;
        }
        let access_profile = task
            .policy_snapshot
            .as_ref()
            .map(magi_core::TaskPolicy::effective_access_profile)
            .unwrap_or_default();
        let mode = if access_profile == AccessProfile::ReadOnly {
            magi_git::AgentWorktreeMode::ReadOnly
        } else {
            magi_git::AgentWorktreeMode::Writable
        };
        let branch = (mode == magi_git::AgentWorktreeMode::Writable)
            .then(|| format!("magi/agent/{task_key}-{allocation_nonce}"));
        // 首次执行时读取直接父代理当前 worktree，含其尚未提交改动；主线不共享子代理 index。
        let source = task.parent_task_id.as_ref().and_then(|id| {
            context
                .agent_worktrees
                .iter()
                .find(|worktree| worktree.task_id == id.as_str())
        });
        if (task
            .parent_task_id
            .as_ref()
            .is_some_and(|id| id != &task.root_task_id)
            && source.is_none())
            || source.is_some_and(|source| !source.active || !source.path.is_dir())
        {
            return Err("父代理执行资源已释放，拒绝建立嵌套 worktree".into());
        }
        if let Some(source) = source {
            let store = self.pipeline.execution_runtime.task_store();
            let current_parent = task
                .parent_task_id
                .as_ref()
                .and_then(|id| store.get_task(id));
            if current_parent.as_ref().is_none_or(|parent| {
                parent.status != magi_core::TaskStatus::Running
                    || parent.root_task_id != task.root_task_id
                    || parent.mission_id != task.mission_id
                    || store.get_active_lease(&parent.task_id).is_none_or(|lease| {
                        source.lease_id.as_deref() != Some(lease.lease_id.as_str())
                    })
            }) {
                return Err("父代理执行租约已失效，拒绝使用旧 worktree 建立快照".into());
            }
        }
        let source_path = source.map_or(context.git.worktree_path.as_path(), |source| {
            source.path.as_path()
        });
        let source_head = source.map_or(base_head.as_str(), |source| source.base_head.as_str());
        let base_head = block_on_git(git_service.snapshot_worktree(source_path, source_head))
            .map_err(|error| format!("生成子代理基线快照失败: {error}"))?;
        let created = block_on_git(git_service.worktree_create(
            &context.git.worktree_path,
            magi_git::WorktreeCreateOptions {
                path: path.clone(),
                base: base_head.clone(),
                branch: branch.clone(),
                create_branch: branch.is_some(),
                detached: branch.is_none(),
                precondition: context.precondition(),
            },
        ))
        .map_err(|error| format!("创建 agent 隔离 worktree 失败: {error}"))?;
        registry
            .register_agent_worktree(
                session_id.as_str(),
                magi_git::AgentWorktreeContext {
                    task_id: task.task_id.to_string(),
                    lease_id: Some(lease_id.to_string()),
                    worker_id: worker_id.map(ToString::to_string).unwrap_or_default(),
                    path: created.path.clone(),
                    mode,
                    base_head: base_head.clone(),
                    branch: created.branch.clone(),
                    active: true,
                    result_head: None,
                    changed_paths: Vec::new(),
                    applied: false,
                },
            )
            .map_err(|error| error.to_string())?;
        if let Some(persist) = self.session_state_persist_callback.as_deref() {
            persist("agent_worktree_allocated")?;
        }
        self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "agent-worktree-allocated-{}-{}",
                    task.task_id, allocation_nonce
                )),
                "agent.git.worktree.allocated",
                serde_json::json!({
                    "session_id": session_id,
                    "workspace_id": workspace_id,
                    "task_id": task.task_id,
                    "worker_id": worker_id,
                    "mode": mode,
                    "base_head": base_head,
                    "branch": created.branch,
                    "worktree_path": created.path,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );
        Ok(Some(path))
    }

    /// TaskPolicy 持久化的是逻辑 workspace 边界；子代理进入隔离 worktree 后，
    /// 同一边界必须投影到当前执行实例的物理根目录。这里只替换 workspace 内路径，
    /// 显式授权的外部引用路径保持不变，避免把整个 agent worktree 上级目录放入白名单。
    fn task_for_execution_root(
        task: &magi_core::Task,
        workspace_identity_root: Option<&PathBuf>,
        execution_root: Option<&PathBuf>,
    ) -> magi_core::Task {
        let mut projected = task.clone();
        let (Some(identity_root), Some(execution_root), Some(policy)) = (
            workspace_identity_root,
            execution_root,
            projected.policy_snapshot.as_mut(),
        ) else {
            return projected;
        };
        if identity_root == execution_root {
            return projected;
        }

        policy.allowed_paths = project_policy_paths_to_execution_root(
            &policy.allowed_paths,
            identity_root,
            execution_root,
        );
        policy.denied_paths = project_policy_paths_to_execution_root(
            &policy.denied_paths,
            identity_root,
            execution_root,
        );
        policy.read_only_paths = project_policy_paths_to_execution_root(
            &policy.read_only_paths,
            identity_root,
            execution_root,
        );
        projected
    }

    /// 子代理模型调用结束后立即结束 worktree 的 active 生命周期。
    ///
    /// 可写 worktree 中的改动由运行时提交到代理分支并记录产出（分支、提交、改动路径），
    /// 随后安全移除目录；分支保留，供直接父任务通过 agent_apply 应用。只读 worktree 干净时
    /// 移除；存在未提交改动或安全移除失败时保留目录与分配记录，但标记为 inactive，
    /// 避免后续任务误认为仍由 worker 占用。绝不使用 force。
    ///
    /// 返回可写代理的产出描述，由调用方写入子任务终态的 output_refs。
    fn finalize_agent_worktree(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        is_sidechain: bool,
    ) -> Option<serde_json::Value> {
        if !is_sidechain {
            return None;
        }
        let (Some(registry), Some(git_service)) = (
            self.session_code_contexts.as_ref(),
            self.git_service.as_ref(),
        ) else {
            return None;
        };
        let context = registry.get(session_id.as_str())?;
        let allocation = context
            .agent_worktrees
            .iter()
            .find(|worktree| {
                worktree.task_id == task.task_id.as_str()
                    && worktree.active
                    && worktree.lease_id.as_deref() == Some(lease_id.as_str())
            })
            .cloned()?;

        let mut workspace_output = None;
        let mut commit_error = None;
        if allocation.mode == magi_git::AgentWorktreeMode::Writable && allocation.path.exists() {
            match self.commit_agent_worktree_output(task, lease_id, session_id, &allocation) {
                Ok(output) => workspace_output = output,
                Err(error) => commit_error = Some(error),
            }
        }
        let (cleanup_status, retained, cleanup_error) = if !allocation.path.exists() {
            ("already_missing", false, None)
        } else if let Some(error) = commit_error {
            ("retained_commit_failed", true, Some(error))
        } else {
            match block_on_git(git_service.observe(&allocation.path)) {
                Ok(observation) if observation.dirty.has_uncommitted => (
                    "retained_dirty",
                    true,
                    Some(format!(
                        "子代理 worktree 含未提交改动：staged={} unstaged={} untracked={} conflicted={}",
                        observation.dirty.staged,
                        observation.dirty.unstaged,
                        observation.dirty.untracked,
                        observation.dirty.conflicted_paths.len()
                    )),
                ),
                Ok(observation) => match (|| -> Result<(), String> {
                    if allocation.mode == magi_git::AgentWorktreeMode::ReadOnly {
                        registry
                            .record_agent_worktree_result(
                                session_id.as_str(),
                                task.task_id.as_str(),
                                lease_id.as_str(),
                                observation.head.ok_or("只读代理目录缺少 HEAD，不能清理")?,
                                vec![],
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    // 删除目录前先保存可恢复 commit；不能只在删除后的异步进度
                    // checkpoint 里保存，否则崩溃会同时丢失目录和恢复证据。
                    if let Some(persist) = self.session_state_persist_callback.as_deref() {
                        persist("agent_worktree_result_saved")?;
                    }
                    block_on_git(git_service.worktree_remove(
                        &context.git.worktree_path,
                        magi_git::WorktreeRemoveOptions {
                            path: allocation.path.clone(),
                            force: false,
                            confirm_force: false,
                            precondition: context.precondition(),
                        },
                    ))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                })() {
                    Ok(_) => ("removed", false, None),
                    Err(error) => ("retained_cleanup_failed", true, Some(error)),
                },
                Err(error) => ("retained_observation_failed", true, Some(error.to_string())),
            }
        };

        if let Err(error) = registry.release_agent_worktree_for_lease(
            session_id.as_str(),
            task.task_id.as_str(),
            lease_id.as_str(),
        ) {
            tracing::warn!(
                session_id = %session_id,
                task_id = %task.task_id,
                %error,
                "结束 agent worktree 生命周期失败"
            );
            return workspace_output;
        }
        if let Some(persist) = self.session_state_persist_callback.as_deref()
            && let Err(error) = persist("agent_worktree_released")
        {
            tracing::error!(
                session_id = %session_id,
                task_id = %task.task_id,
                %error,
                "agent worktree 生命周期状态持久化失败"
            );
        }
        if retained {
            tracing::warn!(
                session_id = %session_id,
                task_id = %task.task_id,
                worktree_path = %allocation.path.display(),
                cleanup_status,
                cleanup_error = cleanup_error.as_deref().unwrap_or_default(),
                "子代理 worktree 已结束执行但保留目录"
            );
        }
        self.event_bus.publish(
            EventEnvelope::domain(
                EventId::unique(format!("agent-worktree-released-{}", task.task_id)),
                "agent.git.worktree.released",
                serde_json::json!({
                    "session_id": session_id,
                    "workspace_id": workspace_id,
                    "task_id": task.task_id,
                    "lease_id": lease_id,
                    "worker_id": allocation.worker_id,
                    "mode": allocation.mode,
                    "base_head": allocation.base_head,
                    "branch": allocation.branch,
                    "worktree_path": allocation.path,
                    "cleanup_status": cleanup_status,
                    "retained": retained,
                    "cleanup_error": cleanup_error,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );
        workspace_output
    }

    /// 把可写代理 worktree 的改动提交到代理分支并记录产出；没有任何改动时返回 `None`。
    fn commit_agent_worktree_output(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        allocation: &magi_git::AgentWorktreeContext,
    ) -> Result<Option<serde_json::Value>, String> {
        let (Some(registry), Some(git_service)) = (
            self.session_code_contexts.as_ref(),
            self.git_service.as_ref(),
        ) else {
            return Ok(None);
        };
        block_on_git(git_service.commit_all(
            &allocation.path,
            &format!("magi agent: {}", task.title.trim()),
        ))
        .map_err(|error| format!("提交子代理改动失败: {error}"))?;
        let head = block_on_git(git_service.observe_ref(&allocation.path))
            .map_err(|error| format!("读取子代理分支 HEAD 失败: {error}"))?
            .head
            .ok_or_else(|| "子代理分支没有 HEAD".to_string())?;
        let changed_paths =
            block_on_git(git_service.changed_paths(&allocation.path, &allocation.base_head, &head))
                .map_err(|error| format!("统计子代理改动失败: {error}"))?;
        registry
            .record_agent_worktree_result(
                session_id.as_str(),
                task.task_id.as_str(),
                lease_id.as_str(),
                head.clone(),
                changed_paths.clone(),
            )
            .map_err(|error| error.to_string())?;
        if changed_paths.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::json!({
            "agent_workspace": {
                "branch": allocation.branch,
                "base": allocation.base_head,
                "head": head,
                "changed_paths": changed_paths,
            }
        })))
    }

    fn task_fact_context_parts(&self, task: &magi_core::Task) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(scope) = task
            .workspace_scope
            .as_deref()
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
        {
            parts.push(format!("[task-workspace] {scope}"));
        }
        if crate::task_helpers::task_can_delegate(
            Some(task),
            Some(self.agent_role_registry.as_ref()),
        ) && let Some(TaskExecutionPlan::Dispatch { session_id, .. }) =
            self.execution_registry.get(&task.task_id)
        {
            let heads = crate::agent_activation::retained_agent_heads(
                self.pipeline.execution_runtime.task_store(), &self.session_store, &session_id, task,
            ).into_iter().map(|head| serde_json::json!({
                    "activation":head.agent_activation(), "task_id":head.task_id, "title":head.title,
                    "status":head.status, "role":head.executor_binding_target_role(),
                })).collect::<Vec<_>>();
            if !heads.is_empty() {
                parts.push(format!(
                    "[session-agents] 本会话保留代理身份的当前事实：\n{}",
                    serde_json::to_string(&heads).expect("agent heads serialize")
                ));
            }
        }
        if let Some(activation) = task.agent_activation() {
            parts.push(format!("[agent-activation] agent_id={} generation={} lifetime={:?} previous_task_id={:?}。这是新的执行实例；旧产物与 transcript 保留在原 Task/Thread，不能宣称旧分支已合入。",
                activation.agent_id, activation.generation, activation.lifetime, activation.previous_task_id));
            if let Some(previous) = activation
                .previous_task_id
                .as_ref()
                .and_then(|id| self.pipeline.execution_runtime.task_store().get_task(id))
            {
                parts.push(format!("[previous-agent-activation] title={} goal={}\n可通过 context_read 按需读取输出：{}\n证据：{}\n原始执行记录：task:{}:transcript（历史参考，不是本轮工具执行证据）",
                    previous.title, previous.goal,
                    (0..previous.output_refs.len()).map(|index| format!("task:{}:output:{index}", previous.task_id)).collect::<Vec<_>>().join(", "),
                    (0..previous.evidence_refs.len()).map(|index| format!("task:{}:evidence:{index}", previous.task_id)).collect::<Vec<_>>().join(", "), previous.task_id));
            }
        }
        if let Some(package) = task.agent_context_package() {
            parts.push(package.render_for_prompt());
        } else if !task.input_refs.is_empty() {
            parts.push(format!(
                "[task-input] {}",
                format_task_ref_list(&task.input_refs)
            ));
        }
        if let Some(prompt) = crate::context_reference::browser_annotation_references_prompt(
            task.browser_annotation_references(),
        ) {
            parts.push(prompt);
        }
        if let Some(prompt) =
            crate::context_reference::browser_node_selections_prompt(task.browser_node_selections())
        {
            parts.push(prompt);
        }
        if let Some(prompt) =
            crate::context_reference::plugin_selections_prompt(task.plugin_selections())
        {
            parts.push(prompt);
        }

        let task_store = self.pipeline.execution_runtime.task_store();
        for dependency_id in &task.dependency_ids {
            if let Some(dependency) = task_store.get_task(dependency_id) {
                parts.push(format_dependency_task_context(&dependency));
            } else {
                parts.push(format!("[dependency] id={dependency_id} status=missing"));
            }
        }
        if parts.is_empty() && task.kind != TaskKind::LocalAgent {
            return parts;
        }
        let mut ordered_parts = vec![CURRENT_TASK_PRIORITY_NOTE.to_string()];
        if task.kind == TaskKind::LocalAgent {
            ordered_parts.push(
                "[validation-rule] 只验证本任务 dependency/input 指向的当前执行产出；不得把历史经验、知识库记录或其他会话目标当成本次交付对象。"
                    .to_string(),
            );
        }
        ordered_parts.extend(parts);
        ordered_parts
    }

    fn assemble_prompt(
        &self,
        _settings_store: Option<&Arc<SettingsStore>>,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> (
        String,
        Vec<magi_bridge_client::ChatMessage>,
        Option<ExecutionContextSummary>,
    ) {
        let base_prompt = if task.goal.is_empty() {
            task.title.clone()
        } else {
            format!("{}\n\n{}", task.title, task.goal)
        };
        let mut context_messages = Vec::new();
        let task_facts = self.task_fact_context_parts(task);
        if !task_facts.is_empty() {
            context_messages.push(runtime_context_message(
                PromptFragmentKind::TaskContext,
                task_facts.join("\n"),
            ));
        }
        if let Some(snapshot) = task
            .agent_context_package()
            .and_then(|package| package.fork.as_ref())
        {
            context_messages.push(runtime_context_message(
                PromptFragmentKind::ContextReferences,
                format!(
                    "{}\n{}",
                    REFERENCE_CONTEXT_PRIORITY_NOTE,
                    snapshot.render_for_prompt()
                ),
            ));
        }
        let (Some(ctx_runtime), Some(ws_id)) = (&self.context_runtime, workspace_id.clone()) else {
            return (base_prompt, context_messages, None);
        };
        let knowledge_selection = ctx_runtime.select_knowledge_on_demand(KnowledgeContextRequest {
            consumer: KnowledgeConsumer::TaskExecution,
            workspace_id: Some(ws_id.clone()),
            query: base_prompt.clone(),
        });
        self.publish_knowledge_context_diagnostic(
            &knowledge_selection,
            session_id,
            Some(&ws_id),
            Some(&task.mission_id),
            Some(&task.task_id),
        );
        let knowledge_context_prompt = knowledge_selection.render_for_prompt();
        let mut context_budget = self
            .context_budget
            .clone()
            .unwrap_or_else(default_context_budget_for_unconfigured_dispatcher);
        context_budget.max_knowledge = 0;
        if task.kind == TaskKind::LocalAgent
            && !task_is_coordinator(Some(task), Some(self.agent_role_registry.as_ref()))
        {
            context_budget.max_turns = 0;
            context_budget.max_memory = 0;
            context_budget.max_shared_items = 0;
        }
        refresh_file_summary_projection(
            ctx_runtime,
            self.session_store.as_ref(),
            session_id,
            &ws_id,
        );
        let result = ctx_runtime.assemble_execution_context(&ExecutionContextAssemblyRequest {
            session_id: session_id.clone(),
            workspace_id: ws_id,
            project_key: None,
            clues: ExecutionContextClues {
                mission: Some(task.title.clone()),
                assignment: None,
                task: Some(task.goal.clone()),
            },
            budget: context_budget,
        });
        let mut context_summary = ExecutionContextSummary::from_context_assembly(&result);
        context_summary.used_knowledge = knowledge_selection.results.len();
        context_summary.knowledge_ids = knowledge_selection
            .results
            .iter()
            .map(|item| item.knowledge_id.clone())
            .collect();
        context_summary.knowledge_ids.sort();
        context_summary.knowledge_ids.dedup();
        if knowledge_selection.truncated
            && !context_summary
                .truncation_parts
                .iter()
                .any(|part| part == "knowledge")
        {
            context_summary
                .truncation_parts
                .push("knowledge".to_string());
            context_summary.truncation_parts.sort();
            context_summary.truncation_count = context_summary.truncation_parts.len();
        }

        let mut ctx_parts: Vec<String> = Vec::new();
        for item in &result.selected_recent_turns {
            ctx_parts.push(format!(
                "[reference:recent-turn:{}] {}",
                recent_turn_source_label(item.source),
                item.content
            ));
        }
        if let Some(knowledge_context_prompt) = knowledge_context_prompt {
            ctx_parts.push(knowledge_context_prompt);
        }
        for item in &result.selected_memory {
            ctx_parts.push(format!("[reference:memory] {}", item.content));
        }
        for item in &result.selected_shared_context {
            ctx_parts.push(format!(
                "[reference:context] {}: {}",
                item.title, item.content
            ));
        }
        for item in &result.selected_file_summaries {
            ctx_parts.push(format!(
                "[reference:file-summary] {}: {}",
                item.absolute_path, item.summary
            ));
        }
        if !ctx_parts.is_empty() {
            ctx_parts.insert(0, REFERENCE_CONTEXT_PRIORITY_NOTE.to_string());
            context_messages.push(runtime_context_message(
                PromptFragmentKind::ContextReferences,
                ctx_parts.join("\n"),
            ));
        }
        (base_prompt, context_messages, Some(context_summary))
    }

    fn resolve_user_rules_prompt(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
    ) -> Option<String> {
        let store = settings_store?;
        user_rules_from_settings(&store.get_section("userRules"))
    }

    fn resolve_safeguard_prompt(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
    ) -> Option<String> {
        Some(render_safeguard_prompt(
            self.build_safety_gate(settings_store).as_ref(),
        ))
    }

    /// S8：依据当前 settings 快照构造 SafetyGate。
    /// 调用者每次进入 LLM 轮次循环前都构造一次；引擎本身无状态，可在该轮次内共享。
    pub(crate) fn build_safety_gate(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
    ) -> Option<magi_safety_gate::SafetyGate> {
        safety_gate_from_settings(settings_store)
    }

    fn execution_settings_snapshot(&self) -> Option<Arc<SettingsStore>> {
        self.settings_store
            .as_ref()
            .map(|store| Arc::new(store.execution_snapshot()))
    }

    fn execution_settings_or_live<'a>(
        &'a self,
        execution_settings_snapshot: Option<&'a Arc<SettingsStore>>,
    ) -> Option<&'a Arc<SettingsStore>> {
        execution_settings_snapshot.or(self.settings_store.as_ref())
    }

    fn resolve_target_cached(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        target: RoleTarget<'_>,
        session_id: Option<&SessionId>,
        thread_id: &str,
    ) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
        let engine = self.session_engine_resolution_context(thread_id, session_id);
        let Some(settings_store) = settings_store else {
            return resolve_target_for_role_with_engine(
                settings_store,
                self.model_bridge_client.clone(),
                target,
                session_id,
                engine.as_ref(),
            );
        };
        let target_label = match target {
            RoleTarget::Orchestrator => "orchestrator".to_string(),
            RoleTarget::Auxiliary => "auxiliary".to_string(),
            RoleTarget::Agent { role_id } => format!("agent:{role_id}"),
        };
        // 线程维度进入缓存键：Web 引擎的绑定键含线程（R50），少了它子代理与
        // 编排者会共用同一个 client，也就共用同一条 ChatGPT 临时对话。
        let cache_key = format!(
            "{}\u{001f}{}\u{001f}{}\u{001f}{}",
            target_label,
            session_id.map(SessionId::as_str).unwrap_or_default(),
            thread_id,
            settings_store.revision(),
        );
        if let Some(client) = self
            .model_client_cache
            .lock()
            .expect("model client cache lock poisoned")
            .get(&cache_key)
            .cloned()
        {
            return Ok(Some(client));
        }
        let resolved = resolve_target_for_role_with_engine(
            Some(settings_store),
            self.model_bridge_client.clone(),
            target,
            session_id,
            engine.as_ref(),
        )?;
        if let Some(client) = resolved.as_ref() {
            self.model_client_cache
                .lock()
                .expect("model client cache lock poisoned")
                .insert(cache_key, client.clone());
        }
        Ok(resolved)
    }

    /// 构造本次解析的会话引擎上下文；未装配工厂时返回 `None`（命中非 HTTP 引擎即失败关闭）。
    fn session_engine_resolution_context(
        &self,
        thread_id: &str,
        session_id: Option<&SessionId>,
    ) -> Option<SessionEngineResolutionContext> {
        self.session_engine_factory
            .as_ref()
            .map(|factory| SessionEngineResolutionContext {
                factory: Arc::clone(factory),
                thread_id: thread_id.to_string(),
                project_id: web_project_id_for_session(&self.session_store, session_id),
            })
    }

    fn resolve_model_client_for_task(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        task: Option<&magi_core::Task>,
        execution_role_id: Option<&str>,
        session_id: Option<&SessionId>,
    ) -> Result<Arc<dyn ModelBridgeClient>, String> {
        let role_id = execution_role_id
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .or_else(|| task.and_then(|task| task_role_id(Some(task))));

        // 线程维度：顶层会话是编排者线程，每个子代理任务各用一个线程 id，
        // 保证它们不会共用同一条 ChatGPT 临时对话（R50）。
        let thread_id = task
            .map(|task| task.task_id.as_str().to_string())
            .unwrap_or_else(|| ORCHESTRATOR_THREAD_ID.to_string());

        // 有 role_id 时走 RoleTarget::Agent，让 resolve_target_for_role 统一处理:
        //   - 角色未配置 engineId → Ok(None) → 显式继承 orchestrator
        //   - 角色已配置 engineId → 只使用该角色模型，失败直接暴露
        // 无 role_id（顶层会话调用）或角色未配置 → 直接走 orchestrator。
        if let Some(role_id) = role_id
            && let Some(client) = self.resolve_target_cached(
                settings_store,
                RoleTarget::Agent { role_id },
                session_id,
                &thread_id,
            )?
        {
            return Ok(client);
        }

        self.resolve_target_cached(
            settings_store,
            RoleTarget::Orchestrator,
            session_id,
            &thread_id,
        )?
        .ok_or_else(|| "model bridge client 未配置".to_string())
    }

    #[cfg(test)]
    pub(crate) fn execute_session_turn_for_test(
        &self,
        request: SessionTurnExecutionRequest,
    ) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
        self.execute_session_turn_with_policy(request, TurnTerminalCommitPolicy::Executor)
    }

    /// 执行不创建 TaskStore 的 Conversation Turn。
    ///
    /// 上下文、Provider 和历史装配仍复用同一 dispatcher，但终态提交策略显式交给
    /// SessionTurnCoordinator，避免普通对话同时由执行器和 API finalizer 收口。
    pub fn execute_conversation_turn(
        &self,
        request: SessionTurnExecutionRequest,
    ) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
        self.execute_session_turn_with_policy(request, TurnTerminalCommitPolicy::Coordinator)
    }

    pub fn track_conversation_turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> ConversationTurnActivityGuard {
        ConversationTurnActivityGuard {
            _inner: self.conversation_quiesce.begin(TaskId::new(
                serde_json::to_string(&(session_id, turn_id)).expect("conversation identity"),
            )),
        }
    }

    pub async fn wait_for_conversation_turn_quiesce(&self, session_id: &SessionId, turn_id: &str) {
        self.conversation_quiesce
            .wait_for_root(&TaskId::new(
                serde_json::to_string(&(session_id, turn_id)).expect("conversation identity"),
            ))
            .await;
    }

    /// 仅等待指定任务的执行线程；由阻塞工具执行器调用，不能等待包含调用者的 root。
    /// 超时仅报告结算未完成，不取消等待中的线程或假造资源退出。
    pub fn wait_for_task_dispatches_blocking(
        &self,
        task_ids: &[TaskId],
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        if self
            .dispatch_quiesce
            .wait_for_tasks_blocking(task_ids, timeout)
        {
            Ok(())
        } else {
            Err("代理执行线程尚未退出，不能确认资源已结算".into())
        }
    }

    fn execute_session_turn_with_policy(
        &self,
        request: SessionTurnExecutionRequest,
        #[cfg(test)] terminal_policy: TurnTerminalCommitPolicy,
        #[cfg(not(test))] _terminal_policy: TurnTerminalCommitPolicy,
    ) -> Result<SessionTurnExecutionOutput, SessionTurnExecutionError> {
        let plan_store =
            magi_plan::PlanStore::new(self.session_store.clone(), request.session_id.clone());
        let execution_settings_snapshot = self.execution_settings_snapshot();
        let execution_settings =
            self.execution_settings_or_live(execution_settings_snapshot.as_ref());
        let client: Arc<dyn ModelBridgeClient> = if matches!(
            request.command,
            Some(crate::session_turn_execution::SessionTurnCommand::Plugin { .. })
        ) {
            Arc::new(PluginCommandPlaceholderClient)
        } else {
            match self.resolve_model_client_for_task(
                execution_settings,
                None,
                None,
                Some(&request.session_id),
            ) {
                Ok(client) => client,
                Err(error) => {
                    // 解析失败的原文只进日志（用户可见信息仍走下面的公共文案）：
                    // 没有它「模型服务请求失败」无法定位到底是缺引擎、缺绑定还是缺工厂。
                    tracing::warn!(
                        session_id = %request.session_id,
                        turn_id = %request.turn_id,
                        %error,
                        "会话主模型客户端解析失败"
                    );
                    let owns_active_plan = self
                        .session_store
                        .active_plan_for_execution_owner(&request.session_id, &request.turn_id)
                        .is_some();
                    let stopped_goal = self
                        .session_store
                        .active_goal_for_execution_owner(&request.session_id, &request.turn_id)
                        .map(|goal| {
                            self.session_store.stop_goal_for_runtime_failure(
                                &request.session_id,
                                &goal.goal_id,
                                None,
                                &request.turn_id,
                                "model_configuration_unavailable",
                            )
                        })
                        .transpose();
                    let plan_result = match stopped_goal {
                        Ok(Some(_)) => Ok(self.session_store.plan(&request.session_id)),
                        Ok(None) if owns_active_plan => plan_store.pause(),
                        Ok(None) => Ok(None),
                        Err(error) => Err(magi_plan::PlanUpdateError::Store(error.to_string())),
                    };
                    match plan_result {
                        Ok(Some(plan)) => magi_plan::publish_plan_event(
                            &self.event_bus,
                            magi_plan::plan_event_type(&plan),
                            &plan,
                            request.workspace_id.as_ref(),
                            None,
                            None,
                        ),
                        Ok(None) => {}
                        Err(error) => tracing::warn!(
                            session_id = %request.session_id,
                            %error,
                            "模型配置解析失败后停止 Goal/Plan 失败"
                        ),
                    }
                    return Err(SessionTurnExecutionError {
                        reason: crate::session_turn_execution::SessionTurnFailureReason::ModelInvocationFailed,
                        diagnostic_code: "model_configuration_unavailable".to_string(),
                        public_message: crate::model_error::PUBLIC_MODEL_INVOCATION_FAILURE_MESSAGE
                            .to_string(),
                        model_failure: Some(Box::new(
                            crate::model_error::ModelFailureDiagnostic::configuration_unavailable(),
                        )),
                    });
                }
            }
        };

        let prompt = request.prompt.clone();
        let safety_gate = self.build_safety_gate(execution_settings);
        let knowledge_context_prompt = self.context_runtime.as_ref().and_then(|runtime| {
            let selection = runtime.select_knowledge_on_demand(KnowledgeContextRequest {
                consumer: KnowledgeConsumer::Mainline,
                workspace_id: request.workspace_id.clone(),
                query: request.prompt.clone(),
            });
            self.publish_knowledge_context_diagnostic(
                &selection,
                &request.session_id,
                request.workspace_id.as_ref(),
                None,
                None,
            );
            selection.render_for_prompt()
        });
        #[cfg(test)]
        let run = match terminal_policy {
            TurnTerminalCommitPolicy::Executor => run_session_turn_execution_for_test,
            TurnTerminalCommitPolicy::Coordinator => run_session_turn_execution,
        };
        #[cfg(not(test))]
        let run = run_session_turn_execution;
        run(SessionTurnExecutionRuntime {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            conversation_registry: self.conversation_registry.as_ref(),
            plan_store: &plan_store,
            settings_store: execution_settings,
            safety_gate: safety_gate.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            request,
            prompt,
            knowledge_context_prompt,
            persist_session_state: self.session_state_persist_callback.as_deref(),
            live_settings_store: self.settings_store.clone(),
            plugin_command_executor: self.plugin_command_executor.clone(),
        })
    }

    fn invoke_llm_with_tools(
        &self,
        input: TaskLlmInvocationInput<'_>,
    ) -> (TaskOutcome, Option<ExecutionContextSummary>) {
        if input.task.kind == TaskKind::LocalWorkflow {
            return self.invoke_workflow_once(WorkflowInvocation::from(&input));
        }
        let TaskLlmInvocationInput {
            task,
            lease_id,
            session_id,
            workspace_id,
            execution_root,
            use_tools,
            skill_name,
            images,
            usage_binding,
            streaming_entry_id,
            is_sidechain,
            worker_id,
            execution_role_id,
            thread_id,
            system_prompt,
            execution_settings_snapshot,
        } = input;
        let task_id = &task.task_id;
        let execution_settings =
            self.execution_settings_or_live(execution_settings_snapshot.as_ref());
        let role_for_model = if is_sidechain {
            execution_role_id
        } else {
            None
        };
        let client = match self.resolve_model_client_for_task(
            execution_settings,
            Some(task),
            role_for_model,
            Some(session_id),
        ) {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(task_id = %task.task_id, error = %error, "invoke_llm_with_tools: model bridge client resolve failed");
                return (
                    TaskOutcome::Failed {
                        error: format!("模型配置不可用: {error}"),
                    },
                    None,
                );
            }
        };

        if task.recovery_checkpoint().is_some()
            && client.executor_capabilities().resume
                != magi_bridge_client::ExecutorResumeMode::Checkpoint
        {
            return (TaskOutcome::Failed {
                error: "executor_resume_unsupported: 当前执行器不支持 Magi 检查点恢复，请核对远端进展后提交新的明确指令".into(),
            }, None);
        }

        let skill_name = match self.resolve_registered_skill_id(skill_name.as_deref()) {
            Ok(skill_name) => skill_name,
            Err(error) => return (TaskOutcome::Failed { error }, None),
        };
        let (prompt, task_context_messages, context_summary) =
            self.assemble_prompt(execution_settings, task, session_id, workspace_id);
        let developer_prompt = compose_developer_instructions(
            system_prompt.as_deref(),
            self.resolve_user_rules_prompt(execution_settings)
                .as_deref(),
            self.resolve_safeguard_prompt(execution_settings).as_deref(),
        );
        let workspace_identity_root_path = workspace_id
            .as_ref()
            .and_then(|_| self.resolve_workspace_root_path(session_id, workspace_id));
        let prepare_workspace = || {
            self.prepare_task_workspace_execution(
                task,
                lease_id,
                session_id,
                workspace_id,
                execution_root,
                is_sidechain,
                worker_id,
            )
        };
        // ExecutorOwned 的首个请求内部就可能执行本地工具，必须在交付请求前走
        // 同一准备入口；原生模型仍到首次本地工具边界才准备资源。
        let executor_owns_tools = client.executor_capabilities().tools
            == magi_bridge_client::ExecutorToolMode::ExecutorOwned;
        let prepared = if executor_owns_tools {
            if !use_tools {
                return (TaskOutcome::Failed {
                    error: "executor_policy_unsupported: 当前任务禁止使用工具，不能启动拥有独立工具循环的执行器".into(),
                }, None);
            }
            match prepare_workspace() {
                Ok(prepared) => Some(prepared),
                Err(error) => {
                    return (
                        TaskOutcome::Failed {
                            error: format!("外部执行器工作区准备失败，调用尚未发送：{error}"),
                        },
                        None,
                    );
                }
            }
        } else {
            None
        };
        let workspace_root_path = match prepared.as_ref() {
            Some(prepared) => prepared.root.clone(),
            None => execution_root
                .cloned()
                .or_else(|| self.resolve_workspace_root_path(session_id, workspace_id)),
        };
        let execution_task = prepared
            .as_ref()
            .map(|prepared| prepared.task.clone())
            .unwrap_or_else(|| {
                Self::task_for_execution_root(
                    task,
                    workspace_identity_root_path.as_ref(),
                    workspace_root_path.as_ref(),
                )
            });

        let tools = if use_tools && !executor_owns_tools {
            let access_profile = execution_task
                .policy_snapshot
                .as_ref()
                .map(magi_core::TaskPolicy::effective_access_profile)
                .unwrap_or_default();
            let tool_defs = self.build_tool_definitions(
                &execution_task,
                skill_name.as_deref(),
                access_profile,
                workspace_id.as_ref(),
            );
            if tool_defs.is_empty() {
                None
            } else {
                Some(tool_defs)
            }
        } else {
            None
        };

        let safety_gate = self.build_safety_gate(execution_settings);
        let plan_store = magi_plan::PlanStore::new(self.session_store.clone(), session_id.clone());
        let project_memory = workspace_identity_root_path.as_ref().and_then(|path| {
            let workspace_root = magi_core::WorkspaceRootPath::new(path.to_string_lossy());
            match self.project_memory_registry.get_or_open(&workspace_root) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, workspace_root = %path.display(), "ProjectMemory: 打开失败，本次 Turn 不注入项目记忆");
                    None
                }
            }
        });
        let snapshot_session = match prepared {
            Some(prepared) => prepared.snapshot,
            None => self.snapshot_manager.as_ref().and_then(|manager| {
                workspace_root_path
                    .as_ref()
                    .and_then(|root| manager.get_session_for_workspace(session_id.as_str(), root))
            }),
        };
        let refresh_tools = |skill_id: Option<&str>| {
            self.build_tool_definitions(
                &execution_task,
                skill_id,
                execution_task
                    .policy_snapshot
                    .as_ref()
                    .map(magi_core::TaskPolicy::effective_access_profile)
                    .unwrap_or_default(),
                workspace_id.as_ref(),
            )
        };
        conversation_loop::run_conversation_loop(ConversationLoopRequest {
            client: client.as_ref(),
            event_bus: self.event_bus.as_ref(),
            session_store: self.session_store.as_ref(),
            settings_store: execution_settings,
            live_settings_store: self.settings_store.as_ref(),
            tool_registry: self.tool_registry.as_ref(),
            skill_runtime: self.skill_runtime.as_deref(),
            skill_dispatch_runtime: self.skill_dispatch_runtime.as_deref(),
            skill_name: skill_name.clone(),
            task_store: self.pipeline.execution_runtime.task_store(),
            execution_registry: &self.execution_registry,
            conversation_registry: self.conversation_registry.as_ref(),
            agent_role_registry: self.agent_role_registry.as_ref(),
            safety_gate: safety_gate.as_ref(),
            plan_store: &plan_store,
            project_memory: project_memory.as_deref(),
            task: &execution_task,
            task_id,
            lease_id,
            session_id,
            workspace_id,
            prompt,
            task_context_messages,
            images,
            tools,
            refresh_tool_definitions: (use_tools && !executor_owns_tools)
                .then_some(&refresh_tools as &conversation_loop::RefreshToolDefinitions<'_>),
            usage_binding,
            streaming_entry_id,
            is_sidechain,
            worker_id,
            thread_id,
            context_summary,
            system_prompt: developer_prompt,
            workspace_root_path,
            snapshot_session,
            prepare_workspace: (use_tools && !executor_owns_tools)
                .then_some(&prepare_workspace as &crate::conversation_loop::PrepareWorkspace),
            execution_group_id: Some(task.mission_id.to_string()),
            persist_session_state: self.session_state_persist_callback.as_deref(),
        })
    }

    /// Synchronous inner dispatch logic; invoked either directly or inside
    /// `tokio::task::spawn_blocking` so the LLM call does not starve the
    /// async runtime (design §1.3).
    fn dispatch_inner(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_orchestrator::task_store::TaskLease,
    ) -> Result<(), String> {
        let Some(plan) = self.execution_registry.get(&task.task_id) else {
            let error = format!(
                "任务 {} 缺少结构化执行计划，已拒绝无计划执行路径",
                task.task_id
            );
            tracing::error!(
                task_id = %task.task_id,
                mission_id = %task.mission_id,
                worker_id = %worker.worker_id,
                "task dispatch missing execution plan"
            );
            self.notify_task_result(
                &task.task_id,
                &lease.lease_id,
                TaskOutcome::Failed { error },
            );
            return Ok(());
        };
        let mut plan = plan;
        {
            let TaskExecutionPlan::Dispatch {
                session_id,
                turn_id,
                thread_id,
                is_primary,
                skill_name,
                ..
            } = &mut plan;
            let parent_owned = self
                .session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)
                .and_then(|turn| turn.agent_turn_binding().ok().flatten())
                .is_some();
            let retained = task.agent_activation().is_some_and(|activation| {
                !matches!(activation.lifetime, magi_core::AgentLifetime::Turn)
            });
            let own_existing = if !*is_primary {
                self.session_store
                    .agent_turn_for_task(session_id, &task.task_id)
                    .map_err(|error| error.to_string())?
            } else {
                None
            };
            if !*is_primary && (retained || parent_owned || own_existing.is_some()) {
                let source_turn_id = turn_id.clone();
                let parent_turn_id = if task.parent_task_id.as_ref() != Some(&task.root_task_id) {
                    let parent = task
                        .parent_task_id
                        .as_ref()
                        .ok_or("嵌套 Agent 缺少父任务")?;
                    let parent = self
                        .session_store
                        .agent_turn_for_task(session_id, parent)
                        .map_err(|error| error.to_string())?
                        .ok_or("嵌套 Agent 缺少父任务的独立 Turn")?;
                    if !self
                        .session_store
                        .turn_execution_is_active(session_id, &parent.turn_id)
                    {
                        return Err("嵌套 Agent 的父 Turn 不可执行".into());
                    }
                    parent.turn_id
                } else {
                    source_turn_id.clone()
                };
                let result = if let Some(existing) = own_existing {
                    if existing.turn_id == source_turn_id {
                        Ok(existing)
                    } else {
                        self.session_store
                            .create_agent_turn(task, &parent_turn_id, thread_id)
                    }
                } else {
                    self.session_store
                        .create_agent_turn(task, &parent_turn_id, thread_id)
                };
                let owned = match result {
                    Ok(turn) => turn,
                    Err(error) => {
                        self.notify_task_result(
                            &task.task_id,
                            &lease.lease_id,
                            TaskOutcome::Failed {
                                error: format!("接纳 Agent 独立执行 Turn 失败: {error}"),
                            },
                        );
                        return Ok(());
                    }
                };
                *turn_id = owned.turn_id.clone();
                if let Some(skill) = owned
                    .agent_active_skill_id()
                    .map_err(|error| error.to_string())?
                {
                    *skill_name = Some(skill);
                }
                // 只修改同一 Task 的进程内计划投影。持久来源已由 canonical 事务提交。
                self.execution_registry
                    .bind_agent_turn(&task.task_id, &source_turn_id, &owned)?;
            }
        }
        let plan_turn_id = match &plan {
            TaskExecutionPlan::Dispatch { turn_id, .. } => turn_id.clone(),
        };
        let mut plan_cleanup = ExecutionPlanCleanup {
            registry: &self.execution_registry,
            task_id: &task.task_id,
            turn_id: plan_turn_id,
            retained: false,
        };

        match plan {
            TaskExecutionPlan::Dispatch {
                target: _,
                worker_id,
                thread_id,
                is_primary,
                session_id,
                turn_id,
                workspace_id,
                execution_root,
                writebacks,
                use_tools,
                skill_name,
                images,
                execution_settings_snapshot,
                ..
            } => {
                if let Some(notifier) = self.completion_notifier.as_ref() {
                    notifier.bind_lease_context(
                        &task.task_id,
                        lease.lease_id.as_str(),
                        Some(session_id.clone()),
                        Some(turn_id.clone()),
                    );
                }
                self.publish_task_dispatched_event(TaskDispatchedEventInput {
                    task_id: &task.task_id,
                    mission_id: &task.mission_id,
                    worker,
                    lease_id: &lease.lease_id,
                    kind: task.kind,
                    session_id: Some(&session_id),
                    workspace_id: workspace_id.as_ref(),
                });
                let outcome = self.execute_dispatch_plan(DispatchPlanExecutionInput {
                    task,
                    lease_id: &lease.lease_id,
                    session_id,
                    turn_id,
                    workspace_id,
                    execution_root,
                    writebacks,
                    use_tools,
                    skill_name,
                    images,
                    usage_binding: model_usage_binding_for_worker_with_settings(
                        worker,
                        is_primary,
                        self.execution_settings_or_live(execution_settings_snapshot.as_ref()),
                    ),
                    is_sidechain: !is_primary,
                    worker_id,
                    worker_role: worker.role.clone(),
                    thread_id,
                    system_prompt: match worker.system_prompt_template.as_deref() {
                        Some(role_prompt) => Some(
                            self.agent_role_registry
                                .compose_system_prompt_for_worker_snapshot(
                                    &worker.role,
                                    role_prompt,
                                    task.executor_binding_capability_ids(),
                                )
                                .map_err(|error| {
                                    format!("组合代理角色与专业能力提示词失败: {error}")
                                })?,
                        ),
                        None if !task.executor_binding_capability_ids().is_empty() => {
                            return Err(format!(
                                "代理角色 {} 缺少基础提示词，无法激活专业能力: {}",
                                worker.role,
                                task.executor_binding_capability_ids().join(", ")
                            ));
                        }
                        None => None,
                    },
                    execution_settings_snapshot,
                });
                // 完成通知可能立即触发 session 队列出队。先释放当前任务的
                // execution plan，再提交 TaskStore 终态，避免下一轮观察到旧计划。
                plan_cleanup.retained = matches!(&outcome, TaskOutcome::Yielded);
                drop(plan_cleanup);
                self.notify_task_result(&task.task_id, &lease.lease_id, outcome);
            }
        }

        Ok(())
    }
}

fn recent_turn_source_label(source: RecentTurnSource) -> &'static str {
    match source {
        RecentTurnSource::Session => "session",
        RecentTurnSource::Project => "project",
        RecentTurnSource::Provided => "provided",
    }
}

fn knowledge_kind_label(kind: KnowledgeKind) -> &'static str {
    match kind {
        KnowledgeKind::Adr => "adr",
        KnowledgeKind::Faq => "faq",
        KnowledgeKind::Learning => "learning",
        KnowledgeKind::CodeIndex => "code_index",
    }
}

struct LearningCandidate {
    content: String,
    context: Option<String>,
    tags: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LearningExtractionFailure {
    InvocationFailed,
    MissingContent,
}

impl LearningExtractionFailure {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvocationFailed => "model_invocation_failed",
            Self::MissingContent => "missing_content",
        }
    }
}

/// 会话记忆水位线（粗略 token 估算）。自上一次抽取以来新增 timeline 文本
/// 估算 token 数超过该阈值才会触发新一轮辅助模型调用。
const SESSION_MEMORY_WATERLINE_TOKENS: u64 = 3_000;
const SESSION_MEMORY_SOURCE_PREFIX: &str = "session-memory://";

fn estimate_session_memory_tokens(text: &str) -> u64 {
    estimate_text_tokens(text) as u64
}

/// 与 TS 版 `session-memory-extraction-service` 5 段契约对齐的结构化记忆切片。
struct SessionMemorySlice {
    category: &'static str,
    content: String,
}

struct AuxiliaryModelExtractionContext<'a> {
    client: Arc<dyn ModelBridgeClient>,
    event_bus: &'a InMemoryEventBus,
    session_store: &'a SessionStore,
    settings_store: Option<&'a Arc<SettingsStore>>,
    session_id: &'a SessionId,
    workspace_id: &'a Option<WorkspaceId>,
    expected_turn_id: Option<&'a str>,
}

/// 利用辅助模型从会话片段中识别"经验/结论/教训"。
///
/// 与会话标题精修（`magi-api` 的 `session_title`）保持同一套约定：
/// - 辅助模型未配置时调用方应在外层短路（缺失则不会进入本函数）。
/// - 模型返回失败、`ok=false` 等异常一律 `tracing::debug!` 并返回明确失败原因，
///   由上层发布诊断事件；不降级到 marker 路径。
fn extract_learnings_via_auxiliary(
    context: AuxiliaryModelExtractionContext<'_>,
    text: &str,
) -> Result<Option<Vec<LearningCandidate>>, LearningExtractionFailure> {
    let AuxiliaryModelExtractionContext {
        client,
        event_bus,
        session_store,
        settings_store,
        session_id,
        workspace_id,
        expected_turn_id,
    } = context;
    let prompt = build_knowledge_extraction_prompt(text);
    let request = ModelInvocationRequest {
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt,
        messages: None,
        tools: None,
        tool_choice: None,
    };
    let call_id = format!(
        "auxiliary-knowledge-extraction-{}-{}",
        session_id,
        UtcMillis::now().0
    );
    let response = match invoke_auxiliary_model_with_usage(
        client,
        request,
        AuxiliaryModelUsageContext {
            event_bus,
            session_store,
            settings_store,
            session_id: Some(session_id),
            workspace_id,
            call_id,
            phase: UsagePhase::Integration,
            expected_turn_id,
        },
    ) {
        Ok(resp) => resp,
        Err(err) => {
            tracing::debug!(error = %err, "辅助模型调用失败，跳过知识抽取");
            return Err(LearningExtractionFailure::InvocationFailed);
        }
    };
    let raw = response
        .content
        .ok_or(LearningExtractionFailure::MissingContent)?;
    Ok(parse_learning_candidates(&raw))
}

fn build_knowledge_extraction_prompt(text: &str) -> String {
    format!(
        "请从下面的会话片段中提取最多 3 条可复用的“经验/结论/教训”。\n\n\
         输出要求：\n\
         - 严格 JSON 数组，每项形如 {{\"content\": \"...\", \"tags\": [\"...\"]}}\n\
         - content 必须是完整成句的一句话陈述，10-200 字之间\n\
         - 不要复述具体的任务上下文，只保留有跨场景复用价值的结论\n\
         - 不要输出“先调用某工具、再运行某命令”这类纯工具操作流水\n\
         - 没有可提取的内容时直接输出 []\n\
         - 不要任何 markdown、代码块包装、解释性前后缀\n\n\
         会话片段：\n{text}"
    )
}

fn parse_learning_candidates(raw: &str) -> Option<Vec<LearningCandidate>> {
    #[derive(serde::Deserialize)]
    struct Wire {
        content: String,
        #[serde(default)]
        tags: Vec<String>,
    }
    let trimmed = raw.trim();
    let list: Vec<Wire> = serde_json::from_str(trimmed).ok()?;
    let mut out = Vec::new();
    for item in list {
        let cnt = item.content.chars().count();
        if !(10..=600).contains(&cnt) || is_pure_tool_sequence(&item.content) {
            continue;
        }
        let mut tags = item.tags;
        tags.push("auto".to_string());
        tags.push("learning".to_string());
        out.push(LearningCandidate {
            content: item.content,
            context: None,
            tags,
        });
        if out.len() == 3 {
            break;
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn is_pure_tool_sequence(content: &str) -> bool {
    let normalized = content.to_ascii_lowercase();
    let tool_markers = [
        "file_read",
        "file_write",
        "apply_patch",
        "shell_exec",
        "cargo test",
        "npm run",
        "git status",
        "git diff",
    ]
    .iter()
    .filter(|marker| normalized.contains(**marker))
    .count();
    let sequence_markers = ["先", "然后", "再", "最后", "调用", "运行", "执行"]
        .iter()
        .filter(|marker| normalized.contains(**marker))
        .count();
    let has_reusable_rationale = [
        "必须", "应该", "应当", "需要", "避免", "确保", "不能", "禁止", "否则", "因为", "原因",
        "原则", "约束",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));

    tool_markers >= 2 && sequence_markers >= 2 && !has_reusable_rationale
}

/// 调用辅助模型生成 5 类会话记忆切片。
///
/// 调用约定与 `extract_learnings_via_auxiliary` 一致：失败 / `ok=false` /
/// JSON 解析异常一律 `tracing::debug!` 后返回 `None`。调用方需先确保辅助模型
/// 已配置（外层使用 `resolve_target_for_role(.., RoleTarget::Auxiliary)` 短路）。
fn extract_session_memory_via_auxiliary(
    context: AuxiliaryModelExtractionContext<'_>,
    text: &str,
) -> Option<Vec<SessionMemorySlice>> {
    let AuxiliaryModelExtractionContext {
        client,
        event_bus,
        session_store,
        settings_store,
        session_id,
        workspace_id,
        expected_turn_id,
    } = context;
    let prompt = build_session_memory_prompt(text);
    let request = ModelInvocationRequest {
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt,
        messages: None,
        tools: None,
        tool_choice: None,
    };
    let call_id = format!(
        "auxiliary-session-memory-{}-{}",
        session_id,
        UtcMillis::now().0
    );
    let response = match invoke_auxiliary_model_with_usage(
        client,
        request,
        AuxiliaryModelUsageContext {
            event_bus,
            session_store,
            settings_store,
            session_id: Some(session_id),
            workspace_id,
            call_id,
            phase: UsagePhase::Integration,
            expected_turn_id,
        },
    ) {
        Ok(resp) => resp,
        Err(err) => {
            tracing::debug!(error = %err, "辅助模型调用失败，跳过会话记忆抽取");
            return None;
        }
    };
    let raw = response.content?;
    parse_session_memory_slices(&raw)
}

fn build_session_memory_prompt(text: &str) -> String {
    format!(
        "请把下面这段会话压缩成 5 类结构化记忆，便于在后续轮次复用。\n\n\
         输出要求：\n\
         - 严格 JSON 对象，键固定为：currentWork、decisions、importantContext、pendingIssues、nextSteps\n\
         - 每个键的值是一句完整中文陈述，30-300 字之间；没有可写内容时填空字符串\n\
         - currentWork：当前正在做的事 / 当前焦点\n\
         - decisions：本段已确定的关键决策或结论\n\
         - importantContext：影响后续判断的重要背景（约束、偏好、外部事实）\n\
         - pendingIssues：尚未解决或仍存疑的问题\n\
         - nextSteps：下一步明确动作\n\
         - 不要复述完整对话，提炼能跨轮使用的信号即可\n\
         - 不要任何 markdown、代码块包装、解释性前后缀\n\n\
         会话片段：\n{text}"
    )
}

fn parse_session_memory_slices(raw: &str) -> Option<Vec<SessionMemorySlice>> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default, rename = "currentWork")]
        current_work: String,
        #[serde(default)]
        decisions: String,
        #[serde(default, rename = "importantContext")]
        important_context: String,
        #[serde(default, rename = "pendingIssues")]
        pending_issues: String,
        #[serde(default, rename = "nextSteps")]
        next_steps: String,
    }
    let wire: Wire = serde_json::from_str(raw.trim()).ok()?;
    let candidates: [(&'static str, String); 5] = [
        ("currentWork", wire.current_work),
        ("decisions", wire.decisions),
        ("importantContext", wire.important_context),
        ("pendingIssues", wire.pending_issues),
        ("nextSteps", wire.next_steps),
    ];
    let mut out = Vec::with_capacity(5);
    for (category, content) in candidates {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let len = trimmed.chars().count();
        if !(10..=600).contains(&len) {
            continue;
        }
        out.push(SessionMemorySlice {
            category,
            content: trimmed.to_string(),
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn normalized_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn knowledge_duplicate(
    existing: &[KnowledgeRecord],
    kind: KnowledgeKind,
    workspace_id: Option<&WorkspaceId>,
    content: &str,
) -> bool {
    let normalized = normalized_text(content);
    existing.iter().any(|record| {
        record.kind == kind && record.workspace_id.as_ref() == workspace_id && {
            let record_text = normalized_text(&record.content);
            record_text == normalized
                || record_text.contains(&normalized)
                || normalized.contains(&record_text)
                || magi_knowledge_store::business_text_similarity(&record.content, content) >= 0.35
        }
    })
}

fn title_from_learning_content(content: &str) -> String {
    let mut title = content.chars().take(80).collect::<String>();
    if content.chars().count() > 80 {
        title.push('…');
    }
    title
}

impl TaskDispatcher for LlmTaskDispatcher {
    fn dispatch(
        &self,
        task: &magi_core::Task,
        worker: &WorkerInfo,
        lease: &magi_orchestrator::task_store::TaskLease,
        admission_permit: ExecutionAdmissionPermit,
    ) -> Result<(), String> {
        let dispatcher = self.clone();
        let task = task.clone();
        let worker = worker.clone();
        let lease = lease.clone();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let task_id = task.task_id.clone();
            let lease_id = lease.lease_id.clone();
            let join_observer = self.clone();
            let _dispatch_quiesce = self
                .dispatch_quiesce
                .begin_task(task.root_task_id.clone(), task.task_id.clone());
            let join = handle.spawn_blocking(move || {
                let _dispatch_quiesce = _dispatch_quiesce;
                let _admission_permit = admission_permit;
                if let Err(err) = dispatcher.dispatch_inner(&task, &worker, &lease) {
                    tracing::error!("dispatch_inner failed: {}", err);
                    dispatcher.notify_task_result(
                        &task.task_id,
                        &lease.lease_id,
                        TaskOutcome::Failed {
                            error: format!("dispatch failed: {}", err),
                        },
                    );
                }
            });
            handle.spawn(async move {
                record_dispatch_join_outcome(join_observer, task_id, lease_id, join).await;
            });
            Ok(())
        } else {
            // 不在 tokio 运行时中（例如同步测试环境），直接同步执行。
            let _dispatch_quiesce = self
                .dispatch_quiesce
                .begin_task(task.root_task_id.clone(), task.task_id.clone());
            let _admission_permit = admission_permit;
            self.dispatch_inner(&task, &worker, &lease)
        }
    }

    fn wait_for_quiesce<'a>(
        &'a self,
        root_task_id: &'a TaskId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let state = Arc::clone(&self.dispatch_quiesce);
        let root_task_id = root_task_id.clone();
        let store = self.pipeline.execution_runtime.task_store();
        Box::pin(async move {
            state.wait_for_execution_scope(store, &root_task_id).await;
        })
    }

    fn wait_for_audit_tree_quiesce<'a>(
        &'a self,
        root_task_id: &'a TaskId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let state = Arc::clone(&self.dispatch_quiesce);
        let root = self
            .pipeline
            .execution_runtime
            .task_store()
            .get_task(root_task_id)
            .map_or_else(|| root_task_id.clone(), |task| task.root_task_id);
        Box::pin(async move {
            state.wait_for_root(&root).await;
        })
    }
}

async fn record_dispatch_join_outcome(
    dispatcher: LlmTaskDispatcher,
    task_id: TaskId,
    lease_id: LeaseId,
    join: tokio::task::JoinHandle<()>,
) {
    if let Err(error) = join.await {
        tracing::error!(task_id = %task_id, lease_id = %lease_id, ?error, "dispatch spawn_blocking panicked");
        let direct_error = public_runtime_excerpt(&format!("模型执行线程异常退出: {error}"), 4096);
        dispatcher.notify_task_result(
            &task_id,
            &lease_id,
            TaskOutcome::Failed {
                error: direct_error,
            },
        );
    }
}

/// S8：依据 settings 快照构造 SafetyGate（用户规则与内置默认规则合并）。
/// Task 执行与外部（MCP）调用共用，保证两条入口的拦截规则一致。
pub fn safety_gate_from_settings(
    settings_store: Option<&Arc<SettingsStore>>,
) -> Option<magi_safety_gate::SafetyGate> {
    let settings_rules = settings_store
        .map(|store| store.get_section("safeguardConfig"))
        .and_then(|raw| {
            raw.get("rules")
                .map(magi_safety_gate::rules_from_settings_value)
        })
        .unwrap_or_default();
    let rules = magi_safety_gate::merge_rules_with_builtin_defaults(settings_rules);
    if rules.is_empty() {
        None
    } else {
        Some(magi_safety_gate::SafetyGate::new(rules))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CanonicalTurnEventSink;
    use crate::model_config::{
        merge_orchestrator_session_override, resolve_orchestrator_model_config,
    };
    use magi_core::{
        MissionId, Task, TaskCompletionAttempt, TaskPolicy, TaskRuntimePayload, TaskTier,
    };
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn retired_root_detection_decodes_windows_paths_and_allows_the_allocated_subdirectory() {
        let old = std::path::Path::new(r"C:\project");
        let new = std::path::Path::new(r"C:\project\.magi\isolated");
        assert!(arguments_reference_retired_root(
            "file_read",
            &serde_json::json!({"paths": [r"C:\project\src\main.rs"]}).to_string(),
            old,
            Some(new),
        ));
        assert!(!arguments_reference_retired_root(
            "shell_exec",
            &serde_json::json!({"command": r"cat C:\project\.magi\isolated\README.md"}).to_string(),
            old,
            Some(new),
        ));
        assert!(arguments_reference_retired_root(
            "shell_exec",
            &serde_json::json!({"command": r"cp C:\project\a.txt C:\project\.magi\isolated\a.txt"})
                .to_string(),
            old,
            Some(new),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn retired_root_detection_covers_path_aliases_in_file_and_shell_tools() {
        let fixture = tempfile::tempdir().unwrap();
        let original = fixture.path().join("original");
        let alias = fixture.path().join("alias");
        let current = original.join("isolated");
        std::fs::create_dir_all(&current).unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        assert!(
            !arguments_reference_retired_root(
                "file_write",
                &serde_json::json!({"path": alias.join("new.txt"), "content":"a"}).to_string(),
                &original,
                Some(&alias),
            ),
            "相同目录的别名不属于执行根切换"
        );
        assert_eq!(
            project_policy_paths_to_execution_root(
                &[alias.join("private").to_string_lossy().into_owned()],
                &original,
                &current,
            ),
            vec![current.join("private").to_string_lossy().into_owned()],
            "通过别名声明的禁止/只读子目录也必须投影到隔离目录"
        );
        for (tool, arguments) in [
            (
                "file_write",
                serde_json::json!({"path": alias.join("new.txt"), "content":"a"}),
            ),
            (
                "shell_exec",
                serde_json::json!({"command": format!("cat '{}'", alias.join("old.txt").display())}),
            ),
        ] {
            assert!(arguments_reference_retired_root(
                tool,
                &arguments.to_string(),
                &original,
                Some(&current)
            ));
        }
        assert!(!arguments_reference_retired_root(
            "file_write",
            &serde_json::json!({
                "path": alias.join("isolated/new.txt"), "content":"a"
            })
            .to_string(),
            &original,
            Some(&current)
        ));
    }

    /// 只记录输入并返回固定 client 的测试工厂（不断言真实浏览器行为）。
    struct RecordingEngineFactory {
        specs: std::sync::Mutex<Vec<magi_plugin_system::SessionEngineInvocationSpec>>,
    }

    impl RecordingEngineFactory {
        fn new() -> Self {
            Self {
                specs: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn specs(&self) -> Vec<magi_plugin_system::SessionEngineInvocationSpec> {
            self.specs.lock().expect("spec 锁").clone()
        }
    }

    impl magi_plugin_system::SessionEngineFactory for RecordingEngineFactory {
        fn supports(&self, engine_id: &str) -> bool {
            engine_id.starts_with("plugin/")
        }

        fn build_session_engine(
            &self,
            spec: magi_plugin_system::SessionEngineInvocationSpec,
        ) -> Result<Arc<dyn ModelBridgeClient>, String> {
            self.specs.lock().expect("spec 锁").push(spec);
            Ok(Arc::new(FailingAuxiliaryClient))
        }
    }

    /// 会话绑定 GPT Web 固定入口的 settings store（不需要任何引擎注册表条目）。
    fn web_engine_store(session_id: &SessionId) -> Arc<SettingsStore> {
        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "apiProtocol": "openai_chat",
                }),
            )
            .expect("写入 orchestrator");
        store
            .set_session_section(
                session_id,
                "orchestrator",
                serde_json::json!({
                    "engineId": "plugin/openai.chatgpt-web/default",
                    "reasoningEffort": "high",
                }),
            )
            .expect("写入会话级覆盖");
        store
    }

    #[test]
    fn web_engine_binding_builds_client_from_the_web_factory_with_the_session_binding() {
        let session_id = SessionId::new("session-web");
        let store = web_engine_store(&session_id);
        store
            .set_session_section(
                &session_id,
                crate::model_config::SESSION_WEB_CONVERSATION_SECTION,
                serde_json::to_value(magi_web_model::WebConversationBinding::saved("conv-9"))
                    .unwrap(),
            )
            .unwrap();
        let factory = Arc::new(RecordingEngineFactory::new());
        let context = SessionEngineResolutionContext {
            factory: Arc::clone(&factory) as Arc<dyn magi_plugin_system::SessionEngineFactory>,
            thread_id: ORCHESTRATOR_THREAD_ID.to_string(),
            project_id: session_id.to_string(),
        };
        let resolved = resolve_target_for_role_with_engine(
            Some(&store),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
            Some(&context),
        )
        .expect("Web 引擎应当解析成功");
        assert!(
            resolved.is_some(),
            "绑定 Web 引擎时不得回退到 default_client"
        );
        let specs = factory.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].engine_id, "plugin/openai.chatgpt-web/default");
        assert_eq!(specs[0].session_id, "session-web");
        assert_eq!(specs[0].thread_id, ORCHESTRATOR_THREAD_ID);
        let binding: magi_web_model::WebConversationBinding =
            serde_json::from_value(specs[0].binding.clone()).unwrap();
        assert_eq!(binding.mode, magi_web_model::WebConversationMode::Saved);
        assert_eq!(binding.remote_conversation_id.as_deref(), Some("conv-9"));
    }

    #[test]
    fn generic_plugin_engine_does_not_receive_web_conversation_binding() {
        let session_id = SessionId::new("session-plugin");
        let store = Arc::new(SettingsStore::new());
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                serde_json::json!({
                    "engineId": "plugin/acme.tools/engine",
                    "model": "plugin/acme.tools/engine",
                    "reasoningEffort": "high",
                }),
            )
            .expect("写入插件引擎绑定");
        store
            .set_session_section(
                &session_id,
                crate::model_config::SESSION_WEB_CONVERSATION_SECTION,
                serde_json::to_value(magi_web_model::WebConversationBinding::saved("web-conv"))
                    .unwrap(),
            )
            .unwrap();
        let factory = Arc::new(RecordingEngineFactory::new());
        let context = SessionEngineResolutionContext {
            factory: Arc::clone(&factory) as Arc<dyn magi_plugin_system::SessionEngineFactory>,
            thread_id: ORCHESTRATOR_THREAD_ID.to_string(),
            project_id: "workspace-plugin".to_string(),
        };
        let resolved = resolve_target_for_role_with_engine(
            Some(&store),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
            Some(&context),
        )
        .expect("插件引擎应当解析成功");
        assert!(resolved.is_some());
        let specs = factory.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].engine_id, "plugin/acme.tools/engine");
        assert_eq!(specs[0].binding, serde_json::Value::Null);
    }

    #[test]
    fn gpt_web_tool_scope_uses_only_the_session_workspace() {
        let store = SessionStore::default();
        let personal_session_id = SessionId::new("session-personal-web");
        store
            .create_session_for_workspace(personal_session_id.clone(), "personal GPT Web", None)
            .unwrap();
        let workspace_session_id = SessionId::new("session-workspace-web");
        store
            .create_session_for_workspace(
                workspace_session_id.clone(),
                "workspace GPT Web",
                Some("workspace-project".to_string()),
            )
            .unwrap();

        assert_eq!(
            web_project_id_for_session(&store, Some(&personal_session_id)),
            "",
            "个人会话不能把 session id 冒充为工作区路径根"
        );
        assert_eq!(
            web_project_id_for_session(&store, Some(&workspace_session_id)),
            "workspace-project"
        );
        assert_eq!(web_project_id_for_session(&store, None), "");
    }

    #[test]
    fn a_role_bound_to_the_web_entry_is_rejected_for_subagents() {
        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "registryAgents",
                serde_json::json!([{
                    "templateId": "role-web",
                    "engineId": "plugin/openai.chatgpt-web/default",
                    "enabled": true,
                    "bindingRevision": 1,
                    "order": 0,
                }]),
            )
            .ok();
        let factory = Arc::new(RecordingEngineFactory::new());
        let context = SessionEngineResolutionContext {
            factory: Arc::clone(&factory) as Arc<dyn magi_plugin_system::SessionEngineFactory>,
            thread_id: "task-1".to_string(),
            project_id: "project".to_string(),
        };
        let result = resolve_target_for_role_with_engine(
            Some(&store),
            None,
            RoleTarget::Agent {
                role_id: "role-web",
            },
            Some(&SessionId::new("session-x")),
            Some(&context),
        );
        // 角色绑定解析依赖注册表装配；无论走哪条失败路径，都不得为子代理构造 Web client。
        assert!(factory.specs().is_empty(), "子代理不得构造 Web client");
        let _ = result;
    }

    #[test]
    fn web_engine_binding_fails_closed_without_a_factory() {
        let session_id = SessionId::new("session-web");
        let store = web_engine_store(&session_id);
        let error = match resolve_target_for_role_with_engine(
            Some(&store),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
            None,
        ) {
            Ok(_) => panic!("缺少工厂时命中 Web 引擎必须失败关闭"),
            Err(error) => error,
        };
        assert!(
            error.contains("不允许退回 HTTP 模型"),
            "错误必须说明失败关闭而不是静默降级：{error}"
        );
    }

    #[test]
    fn http_engine_binding_is_unaffected_by_the_web_path() {
        let session_id = SessionId::new("session-http");
        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "engines",
                serde_json::json!([
                    {
                        "id": "sonnet-4-5",
                        "displayName": "Sonnet 4.5",
                        "llm": {
                            "baseUrl": "https://api.example.com/v1",
                            "apiKey": "sk-engine",
                            "model": "claude-sonnet-4-5",
                            "apiProtocol": "openai_chat",
                        },
                    }
                ]),
            )
            .expect("写入 engines");
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                serde_json::json!({ "engineId": "sonnet-4-5" }),
            )
            .expect("写入会话级覆盖");
        let resolved = resolve_target_for_role_with_engine(
            Some(&store),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
            None,
        )
        .expect("HTTP 引擎绑定应当可用");
        assert!(resolved.is_some());
    }

    struct FailingAuxiliaryClient;

    impl ModelBridgeClient for FailingAuxiliaryClient {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }

        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError>
        {
            Err(magi_bridge_client::BridgeClientError::CallFailed {
                layer: magi_bridge_client::BridgeErrorLayer::Transport,
                code: None,
                message: "test failure".to_string(),
            })
        }

        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&magi_bridge_client::ModelStreamingDelta),
        ) -> Result<magi_bridge_client::ModelResponse, magi_bridge_client::BridgeClientError>
        {
            self.invoke(request)
        }
    }

    fn task_with_role(role: &str, task_tier: TaskTier) -> Task {
        let now = UtcMillis(1_000);
        let background_allowed = false;
        let task_id = TaskId::new(format!("task-{role}"));
        Task {
            task_id: task_id.clone(),
            mission_id: MissionId::new("mission-tool-scope"),
            root_task_id: task_id,
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: format!("task {role}"),
            goal: format!("run as {role}"),
            status: magi_core::TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: Some(TaskPolicy {
                autonomy_level: "Autonomous".to_string(),
                access_profile: magi_core::AccessProfile::Restricted,
                allowed_tools: Vec::new(),
                denied_tools: Vec::new(),
                allowed_paths: Vec::new(),
                denied_paths: Vec::new(),
                read_only_paths: Vec::new(),
                network_mode: "full".to_string(),
                command_mode: "full".to_string(),
                retry_limit: 1,
                validation_profile: None,
                checkpoint_mode: "turn".to_string(),
                task_tier,
                background_allowed,
                escalation_conditions: Vec::new(),
            }),
            executor_binding: Some(magi_core::TaskExecutorBinding::for_role(role)),
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
        }
    }

    fn dispatcher_with_default_tool_surface() -> LlmTaskDispatcher {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(magi_governance::GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        dispatcher_with_tool_surface(event_bus, tool_registry, "dispatcher-default-tool-surface")
    }

    fn dispatcher_with_ready_browser_tool_surface() -> LlmTaskDispatcher {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(magi_governance::GovernanceService::default());
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus))
            .with_browser_automation(
                Arc::new(|_, tool, _, _| {
                    (
                        serde_json::json!({ "tool": tool, "status": "succeeded" }).to_string(),
                        magi_core::ExecutionResultStatus::Succeeded,
                    )
                }),
                Arc::new(|| magi_browser_authority::BrowserCapabilitySnapshot {
                    revision: 11,
                    in_app_browser_enabled: true,
                    browser_use_enabled: true,
                    devtools_enabled: true,
                    lan_access_enabled: false,
                    host_status: magi_browser_authority::BrowserHostStatus::Ready,
                    host_protocol_compatible: true,
                }),
            );
        tool_registry.register_default_builtins();
        dispatcher_with_tool_surface(event_bus, tool_registry, "dispatcher-browser-tool-surface")
    }

    fn dispatcher_with_tool_surface(
        event_bus: Arc<InMemoryEventBus>,
        tool_registry: ToolRegistry,
        state_label: &str,
    ) -> LlmTaskDispatcher {
        let execution_runtime =
            OrchestratedExecutionRuntime::new(magi_worker_runtime::WorkerRuntime::new());
        let pipeline = ExecutionPipeline {
            execution_runtime,
            memory_store: MemoryStore::new(),
        };

        LlmTaskDispatcher::new(
            Arc::clone(&event_bus),
            pipeline,
            LlmTaskDispatcherDependencies {
                session_store: Arc::new(SessionStore::new()),
                execution_registry: TaskExecutionRegistry::default(),
                completion_sink: Arc::new(|_| panic!("此夹具不应派发 Task 终态")),
                conversation_registry: Arc::new(ConversationRegistry::new()),
                agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            },
            test_mission_state_root(state_label),
        )
        .with_tool_registry(tool_registry)
    }

    #[test]
    fn post_completion_enrichment_does_not_block_terminal_result() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind auxiliary test server");
        let address = listener.local_addr().expect("auxiliary test address");
        let (request_started_tx, request_started_rx) = mpsc::channel();
        let (release_response_tx, release_response_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept auxiliary request");
            request_started_tx
                .send(())
                .expect("signal auxiliary request start");
            release_response_rx
                .recv()
                .expect("wait for auxiliary response release");
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"[]"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write auxiliary response");
        });

        let settings = Arc::new(SettingsStore::new());
        settings
            .set_section(
                "auxiliary",
                serde_json::json!({
                    "baseUrl": format!("http://{address}/v1"),
                    "model": "auxiliary-enrichment-test",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat"
                }),
            )
            .expect("auxiliary settings should save");
        let dispatcher = dispatcher_with_default_tool_surface()
            .with_settings_store(settings)
            .with_knowledge_store(Arc::new(KnowledgeStore::new()));
        let (scheduled_tx, scheduled_rx) = mpsc::channel();
        let schedule_call = std::thread::spawn(move || {
            dispatcher.schedule_post_completion_enrichment(
                SessionId::new("session-nonblocking-enrichment"),
                "turn-nonblocking-enrichment".to_string(),
                Some(WorkspaceId::new("workspace-nonblocking-enrichment")),
                TaskOutcome::Completed {
                    attempt: TaskCompletionAttempt {
                        output_refs: vec!["已完成真实任务输出，等待后台知识抽取".to_string()],
                        final_response: Some("任务已完成".to_string()),
                        evidence: Vec::new(),
                    },
                },
                None,
            );
            scheduled_tx.send(()).expect("signal scheduling return");
        });

        let returned_before_auxiliary_release =
            scheduled_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        let auxiliary_request_started = request_started_rx
            .recv_timeout(Duration::from_secs(2))
            .is_ok();
        if !auxiliary_request_started {
            let _ = TcpStream::connect(address);
        }
        release_response_tx
            .send(())
            .expect("release auxiliary response");
        schedule_call
            .join()
            .expect("join enrichment scheduling call");
        server.join().expect("join auxiliary test server");

        assert!(
            returned_before_auxiliary_release,
            "任务终态不能等待后台知识/记忆抽取返回"
        );
        assert!(
            auxiliary_request_started,
            "后台知识抽取必须真实启动，测试不能通过跳过辅助模型伪造非阻塞"
        );
    }

    fn git_fixture(path: &std::path::Path, args: &[&str]) -> String {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git fixture command should start");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn sidechain_projects_workspace_policy_paths_to_agent_execution_root() {
        let identity_root = PathBuf::from("/workspace/project");
        let execution_root = PathBuf::from("/runtime/agent-worktree");
        let mut task = task_with_role("explorer", TaskTier::ExecutionChain);
        let policy = task.policy_snapshot.as_mut().expect("task policy");
        policy.allowed_paths = vec![
            identity_root.to_string_lossy().into_owned(),
            "/shared/reference".to_string(),
        ];
        policy.denied_paths = vec![identity_root.join("private").to_string_lossy().into_owned()];
        policy.read_only_paths = vec![identity_root.join("vendor").to_string_lossy().into_owned()];

        let projected = LlmTaskDispatcher::task_for_execution_root(
            &task,
            Some(&identity_root),
            Some(&execution_root),
        );
        let projected_policy = projected.policy_snapshot.expect("projected policy");

        assert_eq!(
            projected_policy.allowed_paths,
            vec![
                execution_root.to_string_lossy().into_owned(),
                "/shared/reference".to_string(),
            ]
        );
        assert_eq!(
            projected_policy.denied_paths,
            vec![
                execution_root
                    .join("private")
                    .to_string_lossy()
                    .into_owned()
            ]
        );
        assert_eq!(
            projected_policy.read_only_paths,
            vec![execution_root.join("vendor").to_string_lossy().into_owned()]
        );
        assert_eq!(
            task.policy_snapshot
                .as_ref()
                .expect("source policy")
                .allowed_paths[0],
            identity_root.to_string_lossy()
        );
        let decision = crate::tool_batch::access_profile_tool_decision(
            crate::tool_batch::AccessProfileToolDecisionInput {
                access_profile: projected_policy.access_profile,
                command_mode: &projected_policy.command_mode,
                allowed_tools: &projected_policy.allowed_tools,
                denied_tools: &projected_policy.denied_tools,
                allowed_paths: &projected_policy.allowed_paths,
                denied_paths: &projected_policy.denied_paths,
                read_only_paths: &projected_policy.read_only_paths,
                requested_tool_name: BuiltinToolName::FileRead.as_str(),
                arguments: r#"{"path":"crates/magi-api/src/routes/sessions.rs"}"#,
                workspace_root_path: Some(&execution_root),
            },
        );
        assert!(
            decision.is_none(),
            "子代理相对执行根目录读取工作区文件不应被继承的逻辑路径策略拒绝"
        );
    }

    struct ScopeProbeExecutor {
        scopes: Arc<Mutex<Vec<magi_bridge_client::ModelExecutionScope>>>,
    }

    impl ModelBridgeClient for ScopeProbeExecutor {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            let mut caps = magi_bridge_client::text_executor_capabilities();
            caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
            caps
        }

        fn bind_execution_scope(
            &self,
            scope: magi_bridge_client::ModelExecutionScope,
        ) -> Result<Option<Arc<dyn ModelBridgeClient>>, BridgeClientError> {
            self.scopes.lock().unwrap().push(scope);
            Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: "executor_policy_unsupported: probe stops before external execution"
                    .into(),
            })
        }

        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            panic!("scope probe must stop before executing")
        }

        fn invoke_streaming(
            &self,
            _: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("scope probe must stop before executing")
        }
    }

    #[test]
    fn executor_owned_child_prepares_isolated_directory_before_scope_binding() {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("repo");
        std::fs::create_dir(&repository).unwrap();
        git_fixture(&repository, &["init", "-b", "main"]);
        git_fixture(&repository, &["config", "user.name", "Magi Test"]);
        git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
        std::fs::write(repository.join("README.md"), "base\n").unwrap();
        git_fixture(&repository, &["add", "README.md"]);
        git_fixture(&repository, &["commit", "-m", "base"]);
        std::fs::write(repository.join("README.md"), "parent uncommitted\n").unwrap();
        let service = Arc::new(magi_git::GitService::new());
        let observation = block_on_git(service.observe(&repository)).unwrap();
        let contexts = magi_git::SessionCodeContextRegistry::default();
        let session = SessionId::new("session-external-scope");
        let workspace = Some(WorkspaceId::new("workspace-external-scope"));
        contexts.accept(
            session.as_str(),
            workspace.as_ref().unwrap().as_str(),
            vec![repository.clone()],
            &observation,
        );
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = dispatcher_with_default_tool_surface()
            .with_git_context_runtime(service, contexts.clone())
            .with_model_bridge_client(Arc::new(ScopeProbeExecutor {
                scopes: scopes.clone(),
            }));
        dispatcher.agent_worktree_root = fixture.path().join("agent-worktrees");
        let mut parent = task_with_role("coordinator", TaskTier::ExecutionChain);
        parent.status = magi_core::TaskStatus::Running;
        let mut child = task_with_role("explorer", TaskTier::ExecutionChain);
        child.status = magi_core::TaskStatus::Running;
        child.root_task_id = parent.task_id.clone();
        child.parent_task_id = Some(parent.task_id.clone());
        let policy = child.policy_snapshot.as_mut().unwrap();
        policy.access_profile = AccessProfile::ReadOnly;
        policy.allowed_paths = vec![repository.to_string_lossy().into_owned()];
        let store = dispatcher.pipeline.execution_runtime.task_store();
        store.insert_task(parent).unwrap();
        store.insert_task(child.clone()).unwrap();
        let worker = WorkerId::new("worker-external-scope");
        let lease = store
            .grant_lease(
                &child.task_id,
                &child.root_task_id,
                &worker,
                "explorer",
                60_000,
            )
            .unwrap();
        dispatcher
            .session_store
            .create_session(session.clone(), "scope probe")
            .unwrap();
        dispatcher
            .session_store
            .ensure_session_mission(&session, UtcMillis::now(), || child.mission_id.clone());
        let thread = crate::session_thread::ensure_thread_for_role(
            &dispatcher.session_store,
            &session,
            &child.mission_id,
            "explorer",
            &worker,
            &child.task_id,
            UtcMillis::now(),
        )
        .unwrap();
        let usage = crate::usage_recording::session_turn_model_usage_binding(true);
        let invoke = |use_tools| {
            dispatcher.invoke_llm_with_tools(TaskLlmInvocationInput {
                task: &child,
                lease_id: &lease.lease_id,
                session_id: &session,
                workspace_id: &workspace,
                execution_root: Some(&repository),
                use_tools,
                skill_name: None,
                images: vec![],
                usage_binding: &usage,
                streaming_entry_id: None,
                is_sidechain: true,
                worker_id: Some(&worker),
                execution_role_id: Some("explorer"),
                thread_id: &thread,
                system_prompt: None,
                execution_settings_snapshot: None,
            })
        };
        let (outcome, _) = invoke(false);
        assert!(matches!(outcome, TaskOutcome::Failed { .. }));
        assert!(scopes.lock().unwrap().is_empty());
        assert!(
            contexts
                .get(session.as_str())
                .unwrap()
                .agent_worktrees
                .is_empty()
        );
        let (outcome, _) = invoke(true);
        assert!(
            matches!(outcome, TaskOutcome::Failed { .. }),
            "probe rejects binding: {outcome:?}"
        );
        let observed = scopes.lock().unwrap();
        assert_eq!(
            observed.len(),
            1,
            "must reach scope binding after preparation: {outcome:?}"
        );
        let root = observed[0].working_dir.as_ref().unwrap();
        assert_ne!(root, &repository);
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "parent uncommitted\n"
        );
        assert_eq!(
            observed[0].policy.allowed_paths,
            vec![root.to_string_lossy()]
        );
        let context = contexts.get(session.as_str()).unwrap();
        assert_eq!(context.agent_worktrees.len(), 1);
        assert_eq!(
            context.agent_worktrees[0].mode,
            magi_git::AgentWorktreeMode::ReadOnly
        );
        drop(observed);
        dispatcher.finalize_agent_worktree(&child, &lease.lease_id, &session, &workspace, true);
        assert!(!context.agent_worktrees[0].path.exists());
    }

    #[test]
    fn recovered_agent_worktree_keeps_original_files_across_new_runtime_workers_and_cleanup() {
        for failed_checkpoint in [
            None,
            Some("agent_worktree_reattached"),
            Some("agent_worktree_result_saved"),
        ] {
            let fail_persist = failed_checkpoint == Some("agent_worktree_reattached");
            let fixture = tempfile::tempdir().unwrap();
            let repository = fixture.path().join("repo");
            std::fs::create_dir(&repository).unwrap();
            for args in [
                vec!["init", "-b", "main"],
                vec!["config", "user.name", "Magi Test"],
                vec!["config", "user.email", "test@magi.invalid"],
            ] {
                git_fixture(&repository, &args);
            }
            std::fs::write(repository.join("README.md"), "original baseline").unwrap();
            git_fixture(&repository, &["add", "."]);
            git_fixture(&repository, &["commit", "-m", "base"]);
            let git = Arc::new(magi_git::GitService::new());
            let observation = block_on_git(git.observe(&repository)).unwrap();
            let contexts = magi_git::SessionCodeContextRegistry::default();
            let session = SessionId::new("resource-recovery");
            let workspace = Some(WorkspaceId::new("resource-recovery"));
            contexts.accept(
                session.as_str(),
                workspace.as_ref().unwrap().as_str(),
                vec![],
                &observation,
            );
            let mut dispatcher = dispatcher_with_default_tool_surface()
                .with_git_context_runtime(git, contexts.clone());
            dispatcher.agent_worktree_root = fixture.path().join("worktrees");
            let mut parent = task_with_role("coordinator", TaskTier::ExecutionChain);
            parent.status = magi_core::TaskStatus::Running;
            let mut child = task_with_role("executor", TaskTier::ExecutionChain);
            child.status = magi_core::TaskStatus::Running;
            child.parent_task_id = Some(parent.task_id.clone());
            child.root_task_id = parent.task_id.clone();
            child.executor_binding.as_mut().unwrap().agent_activation =
                Some(magi_core::AgentActivation {
                    agent_id: child.task_id.clone(),
                    session_id: session.clone(),
                    lifetime: magi_core::AgentLifetime::Session,
                    generation: 1,
                    previous_task_id: None,
                });
            let store = dispatcher.pipeline.execution_runtime.task_store();
            store.insert_task(parent).unwrap();
            store.insert_task(child.clone()).unwrap();
            let logical_worker = WorkerId::new("persistent-thread-worker");
            let first = store
                .grant_lease(
                    &child.task_id,
                    &child.root_task_id,
                    &WorkerId::new("old-runtime-worker"),
                    "executor",
                    60_000,
                )
                .unwrap();
            let path = dispatcher
                .resolve_task_execution_root(
                    &child,
                    &first.lease_id,
                    &session,
                    &workspace,
                    None,
                    true,
                    Some(&logical_worker),
                )
                .unwrap()
                .unwrap();
            store
                .transfer_agent_subtree(
                    &child.root_task_id,
                    &child.task_id,
                    magi_core::AgentRunTransfer {
                        operation_id: "resource-recovery-transfer".into(),
                        session_id: session.clone(),
                        source_turn_id: "original-parent-turn".into(),
                        transferred_by: child.root_task_id.clone(),
                        activation_generation: 1,
                        owner: magi_core::AgentRunOwner::Session,
                        transferred_at: UtcMillis::now(),
                    },
                )
                .unwrap();
            child = store.get_task(&child.task_id).unwrap();
            std::fs::write(path.join("retained.txt"), "original agent output").unwrap();
            assert!(store.revoke_lease(&child.task_id, &first.lease_id));
            contexts.restore_after_restart(contexts.all());
            std::fs::write(repository.join("README.md"), "new mainline baseline").unwrap();
            let second = store
                .grant_lease(
                    &child.task_id,
                    &child.root_task_id,
                    &WorkerId::new("new-runtime-worker"),
                    "executor",
                    60_000,
                )
                .unwrap();
            dispatcher.session_state_persist_callback = Some(Arc::new(move |checkpoint| {
                if Some(checkpoint) == failed_checkpoint {
                    Err("injected resource flush failure".into())
                } else {
                    Ok(())
                }
            }));
            let resumed = dispatcher.resolve_task_execution_root(
                &child,
                &second.lease_id,
                &session,
                &workspace,
                None,
                true,
                Some(&logical_worker),
            );
            assert_eq!(resumed.is_err(), fail_persist, "{resumed:?}");
            assert_eq!(
                std::fs::read_to_string(path.join("README.md")).unwrap(),
                "original baseline"
            );
            assert_eq!(
                std::fs::read_to_string(path.join("retained.txt")).unwrap(),
                "original agent output"
            );
            assert_eq!(
                contexts
                    .get(session.as_str())
                    .unwrap()
                    .agent_worktrees
                    .len(),
                1
            );
            if fail_persist {
                assert!(!contexts.get(session.as_str()).unwrap().agent_worktrees[0].active);
                assert!(
                    path.exists(),
                    "保存失败仍保留原文件供核对，不能执行清理或换新副本"
                );
            } else {
                assert_eq!(
                    std::fs::canonicalize(resumed.unwrap().unwrap()).unwrap(),
                    std::fs::canonicalize(&path).unwrap()
                );
                assert!(
                    dispatcher
                        .finalize_agent_worktree(
                            &child,
                            &first.lease_id,
                            &session,
                            &workspace,
                            true
                        )
                        .is_none()
                );
                assert!(
                    contexts.get(session.as_str()).unwrap().agent_worktrees[0].active,
                    "旧 lease 的迟到清理不能释放新执行"
                );
                dispatcher
                    .finalize_agent_worktree(&child, &second.lease_id, &session, &workspace, true)
                    .unwrap();
                assert_eq!(
                    path.exists(),
                    failed_checkpoint == Some("agent_worktree_result_saved"),
                    "清理前保存失败须保留原目录；正常清理之后从持久结果恢复"
                );
                assert!(store.revoke_lease(&child.task_id, &second.lease_id));
                let third = store
                    .grant_lease(
                        &child.task_id,
                        &child.root_task_id,
                        &WorkerId::new("third-runtime-worker"),
                        "executor",
                        60_000,
                    )
                    .unwrap();
                assert_eq!(
                    std::fs::canonicalize(
                        dispatcher
                            .resolve_task_execution_root(
                                &child,
                                &third.lease_id,
                                &session,
                                &workspace,
                                None,
                                true,
                                Some(&logical_worker)
                            )
                            .unwrap()
                            .unwrap()
                    )
                    .unwrap(),
                    std::fs::canonicalize(&path).unwrap()
                );
                assert_eq!(
                    std::fs::read_to_string(path.join("retained.txt")).unwrap(),
                    "original agent output",
                    "已清理目录从原结果重建，不能从当前主线建立新基线"
                );
            }
            contexts.remove(session.as_str());
            assert!(
                dispatcher
                    .resolve_task_execution_root(
                        &child,
                        &second.lease_id,
                        &session,
                        &workspace,
                        None,
                        true,
                        Some(&logical_worker)
                    )
                    .is_err(),
                "丢失原目录上下文时，不能以当前主线作为转交代理的恢复目录"
            );
            assert!(!repository.join("retained.txt").exists());
        }
    }

    #[test]
    fn sidechain_inherits_session_base_in_independent_worktree() {
        let fixture = tempfile::tempdir().expect("fixture root");
        let repository = fixture.path().join("repo");
        std::fs::create_dir_all(&repository).expect("repo directory");
        git_fixture(&repository, &["init", "-b", "main"]);
        git_fixture(&repository, &["config", "user.name", "Magi Test"]);
        git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
        std::fs::write(repository.join("README.md"), "base\n").expect("fixture file");
        git_fixture(&repository, &["add", "README.md"]);
        git_fixture(&repository, &["commit", "-m", "base"]);

        let git_service = Arc::new(magi_git::GitService::new());
        let observation = block_on_git(git_service.observe(&repository)).expect("observe repo");
        let contexts = magi_git::SessionCodeContextRegistry::default();
        contexts.accept(
            "session-git-agent",
            "workspace-git-agent",
            vec![repository.clone()],
            &observation,
        );
        let mut dispatcher = dispatcher_with_default_tool_surface()
            .with_git_context_runtime(git_service, contexts.clone());
        dispatcher.agent_worktree_root = fixture.path().join("agent-worktrees");
        let task = task_with_role("executor", TaskTier::ExecutionChain);
        let lease_id = LeaseId::new("lease-git-agent");
        let execution_root = dispatcher
            .resolve_task_execution_root(
                &task,
                &lease_id,
                &SessionId::new("session-git-agent"),
                &Some(WorkspaceId::new("workspace-git-agent")),
                None,
                true,
                Some(&WorkerId::new("worker-git-agent")),
            )
            .expect("agent root")
            .expect("agent path");

        assert_ne!(execution_root, repository);
        let agent_head = magi_process::std_command("git")
            .arg("-C")
            .arg(&execution_root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("agent head");
        assert!(agent_head.status.success());
        assert_eq!(
            String::from_utf8_lossy(&agent_head.stdout).trim(),
            observation.head.as_deref().expect("base head")
        );
        let context = contexts.get("session-git-agent").expect("session context");
        assert_eq!(context.agent_worktrees.len(), 1);
        assert_eq!(
            context.agent_worktrees[0].base_head,
            observation.head.expect("head")
        );
        assert_eq!(
            context.agent_worktrees[0].mode,
            magi_git::AgentWorktreeMode::Writable
        );
        let agent_branch = context.agent_worktrees[0]
            .branch
            .clone()
            .expect("writable agent branch");

        dispatcher.finalize_agent_worktree(
            &task,
            &lease_id,
            &SessionId::new("session-git-agent"),
            &Some(WorkspaceId::new("workspace-git-agent")),
            true,
        );

        assert!(
            !execution_root.exists(),
            "clean agent worktree should be removed"
        );
        let context = contexts.get("session-git-agent").expect("released context");
        assert!(!context.agent_worktrees[0].active);
        assert!(
            !context
                .runtime_workspace_roots
                .iter()
                .any(|root| root == &execution_root)
        );
        git_fixture(
            &repository,
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{agent_branch}"),
            ],
        );
    }

    #[test]
    fn writable_sidechain_starts_from_main_worktree_and_commits_its_output() {
        let fixture = tempfile::tempdir().expect("fixture root");
        let repository = fixture.path().join("repo");
        std::fs::create_dir_all(&repository).expect("repo directory");
        git_fixture(&repository, &["init", "-b", "main"]);
        git_fixture(&repository, &["config", "user.name", "Magi Test"]);
        git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
        std::fs::write(repository.join("README.md"), "base\n").expect("fixture file");
        git_fixture(&repository, &["add", "README.md"]);
        git_fixture(&repository, &["commit", "-m", "base"]);

        // 主线有未提交改动：子代理必须从包含它的快照出发。
        std::fs::write(repository.join("README.md"), "main uncommitted\n").expect("main edit");
        let git_service = Arc::new(magi_git::GitService::new());
        let observation = block_on_git(git_service.observe(&repository)).expect("observe repo");
        let contexts = magi_git::SessionCodeContextRegistry::default();
        contexts.accept(
            "session-git-agent-dirty",
            "workspace-git-agent-dirty",
            vec![repository.clone()],
            &observation,
        );
        let mut dispatcher = dispatcher_with_default_tool_surface()
            .with_git_context_runtime(git_service, contexts.clone());
        dispatcher.agent_worktree_root = fixture.path().join("agent-worktrees");
        let task = task_with_role("executor", TaskTier::ExecutionChain);
        let lease_id = LeaseId::new("lease-git-agent-dirty");
        let execution_root = dispatcher
            .resolve_task_execution_root(
                &task,
                &lease_id,
                &SessionId::new("session-git-agent-dirty"),
                &Some(WorkspaceId::new("workspace-git-agent-dirty")),
                None,
                true,
                Some(&WorkerId::new("worker-git-agent-dirty")),
            )
            .expect("agent root")
            .expect("agent path");
        assert_eq!(
            std::fs::read_to_string(execution_root.join("README.md")).unwrap(),
            "main uncommitted\n",
            "子代理应看到主线未提交的改动"
        );
        std::fs::write(execution_root.join("uncommitted.txt"), "agent output\n")
            .expect("dirty agent output");

        let workspace_output = dispatcher.finalize_agent_worktree(
            &task,
            &lease_id,
            &SessionId::new("session-git-agent-dirty"),
            &Some(WorkspaceId::new("workspace-git-agent-dirty")),
            true,
        );

        let workspace_output = workspace_output.expect("可写代理有改动时应返回产出");
        assert_eq!(
            workspace_output["agent_workspace"]["changed_paths"],
            serde_json::json!(["uncommitted.txt"]),
            "主线快照里的改动不算代理产出"
        );
        assert!(!execution_root.exists(), "提交后的干净 worktree 应移除");
        let context = contexts
            .get("session-git-agent-dirty")
            .expect("released context");
        let allocation = &context.agent_worktrees[0];
        assert!(!allocation.active);
        let head = allocation.result_head.clone().expect("应记录代理分支 HEAD");
        assert_eq!(
            allocation.changed_paths,
            vec!["uncommitted.txt".to_string()]
        );
        assert_eq!(
            git_fixture(&repository, &["show", &format!("{head}:uncommitted.txt")]),
            "agent output"
        );
        assert!(
            !context
                .runtime_workspace_roots
                .iter()
                .any(|root| root == &execution_root)
        );
        assert_eq!(
            std::fs::read_to_string(repository.join("README.md")).unwrap(),
            "main uncommitted\n",
            "主线工作树不受影响"
        );
    }

    #[test]
    fn session_turn_model_configuration_failure_pauses_plan() {
        let dispatcher = dispatcher_with_default_tool_surface();
        let session_id = SessionId::new("session-model-config-failure");
        let accepted_at = UtcMillis(2_000);
        dispatcher
            .session_store
            .create_session(session_id.clone(), "model config failure")
            .expect("session should create");
        let (_mission_id, orchestrator_thread_id) = dispatcher
            .session_store
            .ensure_session_mission(&session_id, accepted_at, || {
                MissionId::new("mission-model-config-failure")
            });
        let turn_id = "turn-model-config-failure";
        let attempt = match dispatcher
            .conversation_registry
            .turn_coordinator()
            .execute_command(
                &session_id,
                crate::TurnCommand::Start(crate::TurnAdmission {
                    turn_id: turn_id.to_string(),
                    request_id: "request-model-config-failure".to_string(),
                    request_fingerprint: "fingerprint-model-config-failure".to_string(),
                    profile: crate::ExecutionProfile::Conversation,
                }),
            )
            .expect("model config failure Turn should start")
        {
            crate::CoordinatorCommandResult::Admission(crate::CoordinatorAdmission::Accepted(
                attempt,
            )) => attempt,
            other => panic!("unexpected model config failure admission: {other:?}"),
        };
        CanonicalTurnEventSink::for_store(&dispatcher.session_store, None)
            .accept_conversation_turn_with_timeline_entry(
                session_id.clone(),
                None,
                magi_session_store::TimelineEntryInput::new(
                    "timeline-model-config-failure",
                    magi_session_store::TimelineEntryKind::UserMessage,
                    "继续执行",
                    accepted_at,
                ),
                magi_session_store::ActiveExecutionTurn {
                    turn_id: turn_id.to_string(),
                    turn_seq: accepted_at.0,
                    accepted_at,
                    completed_at: None,
                    status: "accepted".to_string(),
                    user_message: Some("继续执行".to_string()),
                    items: vec![magi_session_store::ActiveExecutionTurnItem {
                        item_id: "user-model-config-failure".to_string(),
                        item_seq: 1,
                        kind: "user_message".to_string(),
                        status: "completed".to_string(),
                        source: "orchestrator".to_string(),
                        title: None,
                        content: Some("继续执行".to_string()),
                        task_id: None,
                        worker_id: None,
                        role_id: None,
                        tool_call_id: None,
                        tool_name: None,
                        tool_status: None,
                        tool_arguments: None,
                        tool_result: None,
                        tool_error: None,
                        request_id: Some("request-model-config-failure".to_string()),
                        user_message_id: None,
                        placeholder_message_id: None,
                        metadata: Default::default(),
                        timeline_entry_id: Some("timeline-model-config-failure".to_string()),
                        source_thread_id: orchestrator_thread_id,
                    }],
                },
            )
            .expect("current Turn should persist through sink");
        dispatcher
            .conversation_registry
            .turn_coordinator()
            .execute_command(
                &session_id,
                crate::TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: crate::CoordinatorTurnStatus::Preparing,
                },
            )
            .expect("model config failure Turn should prepare");
        dispatcher
            .conversation_registry
            .turn_coordinator()
            .execute_command(
                &session_id,
                crate::TurnCommand::SetStatus {
                    attempt,
                    status: crate::CoordinatorTurnStatus::Running,
                },
            )
            .expect("model config failure Turn should run");
        CanonicalTurnEventSink::for_store(&dispatcher.session_store, None)
            .set_status_domain(&session_id, Some(turn_id), "running")
            .expect("current Turn running status should persist through sink");
        let plan_store =
            magi_plan::PlanStore::new(dispatcher.session_store.clone(), session_id.clone());
        plan_store
            .update(magi_plan::UpdatePlanInput {
                plan_id: None,
                expected_revision: Some(0),
                expected_goal_id: None,
                expected_goal_control_revision: None,
                language: "zh-CN".to_string(),
                explanation: None,
                plan: vec![magi_plan::UpdatePlanItemInput {
                    item_id: Some("execute-current-step".to_string()),
                    step: "执行当前步骤".to_string(),
                    status: magi_core::PlanItemStatus::InProgress,
                }],
            })
            .expect("plan should persist");

        let result = dispatcher.execute_session_turn_for_test(SessionTurnExecutionRequest {
            session_id,
            turn_id: "turn-model-config-failure".to_string(),
            workspace_id: None,
            prompt: "继续执行".to_string(),
            images: Vec::new(),
            context_references: Vec::new(),
            access_profile: magi_core::AccessProfile::Restricted,
            skill_name: None,
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            command: None,
        });

        assert!(result.is_err());
        let plan = plan_store.snapshot().expect("plan should remain visible");
        assert_eq!(plan.state, magi_core::PlanState::Paused);
        assert_eq!(plan.items[0].status, magi_core::PlanItemStatus::InProgress);
    }

    #[tokio::test]
    async fn dispatch_thread_panic_is_reported_as_task_failure() {
        let mut dispatcher = dispatcher_with_default_tool_surface();
        let (sender, receiver) = std::sync::mpsc::channel();
        dispatcher.completion_sink = Arc::new(move |result| sender.send(result).unwrap());
        let task_id = TaskId::new("task-dispatch-thread-panic");
        let lease_id = LeaseId::new("lease-dispatch-thread-panic");
        let join = tokio::task::spawn_blocking(|| panic!("模拟模型执行线程 panic"));

        record_dispatch_join_outcome(dispatcher, task_id.clone(), lease_id.clone(), join).await;

        let results = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, task_id);
        assert_eq!(results[0].lease_id, lease_id);
        match &results[0].outcome {
            TaskOutcome::Failed { error } => {
                assert!(error.contains("模型执行线程异常退出"));
                assert!(error.contains("模拟模型执行线程 panic"));
            }
            TaskOutcome::Completed { .. } => panic!("panic 不得被记录为成功"),
            TaskOutcome::Yielded => panic!("panic 不得被记录为等待用户"),
            TaskOutcome::Cancelled => panic!("panic 不得被记录为用户取消"),
        }
    }

    fn test_mission_state_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("magi-{label}-{}", UtcMillis::now().0));
        std::fs::create_dir_all(&root).expect("test mission state root should create");
        root
    }

    #[test]
    fn subagent_task_has_own_streaming_entry_without_writebacks() {
        let worker_task = task_with_role("executor", TaskTier::ExecutionChain);

        assert_eq!(
            task_streaming_entry_id(&worker_task),
            format!("timeline-streaming-{}", worker_task.task_id)
        );
    }

    #[test]
    fn assemble_prompt_for_subagent_uses_package_and_current_file_facts_only() {
        let session_id = SessionId::new("session-context-prompt");
        let workspace_id = WorkspaceId::new("workspace-context-prompt");
        let session_store = SessionStore::from_state(magi_session_store::SessionStoreState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![magi_session_store::SessionRecord {
                session_id: session_id.clone(),
                title: "Context prompt session".to_string(),
                status: magi_core::SessionLifecycleStatus::Active,
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
                message_count: None,
                workspace_id: Some(workspace_id.to_string()),
                last_completed_at: None,
                last_viewed_at: None,
                kind: Default::default(),
            }],
            timeline: vec![magi_session_store::TimelineEntry {
                entry_id: "timeline-context-prompt".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::SystemNote,
                message: "prior session fact for runtime context".to_string(),
                occurred_at: UtcMillis(10),
            }],
            notifications: vec![],
            canonical_turns: vec![],
            inputs: vec![],
            goals: vec![],
            plans: vec![],
            thread_context_checkpoints: vec![],
            thread_registry: vec![],
            execution_sidecar_store: magi_session_store::SessionExecutionSidecarStoreState {
                runtime_sidecars: vec![],
            },
        });

        let file_summary_store = magi_context_runtime::FileSummaryStore::default();

        let context_runtime = ContextRuntime::with_runtime_sources(
            KnowledgeStore::new(),
            MemoryStore::new(),
            session_store,
            magi_context_runtime::SharedContextPool::default(),
            file_summary_store,
            magi_context_runtime::ProjectRecentTurnStore::default(),
        );
        let dispatcher = dispatcher_with_default_tool_surface()
            .with_context_runtime(Arc::new(context_runtime))
            .with_context_budget(ContextBudget {
                max_turns: 1,
                max_knowledge: 0,
                max_memory: 0,
                max_shared_items: 0,
                max_file_summaries: 1,
            });
        dispatcher
            .session_store
            .create_session(session_id.clone(), "Context prompt session")
            .expect("dispatcher session should create");
        let (_, thread_id) =
            dispatcher
                .session_store
                .ensure_session_mission(&session_id, UtcMillis(15), || {
                    MissionId::new("mission-context-prompt")
                });
        let directory = tempfile::tempdir().expect("tempdir");
        let file_path = directory.path().join("src-lib.rs");
        std::fs::write(&file_path, "Important file summary from current workspace.")
            .expect("write file fact");
        dispatcher.session_store.append_thread_messages(
            &thread_id,
            vec![magi_session_store::ThreadChatMessage {
                input_request_id: None,
                role: "tool".to_string(),
                content: Some(
                    serde_json::json!({
                        "tool": "file_read",
                        "status": "succeeded",
                        "path": file_path,
                        "content_hash": magi_snapshot::path_content_hash(&file_path)
                            .expect("hash file fact"),
                        "content": "Important file summary from current workspace."
                    })
                    .to_string(),
                ),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-file-summary".to_string()),
                provider_context: Vec::new(),
            }],
            UtcMillis(20),
        );
        let mut task = task_with_role("executor", TaskTier::ExecutionChain);
        task.runtime_payload = magi_core::TaskRuntimePayload::AgentContext {
            package: Box::new(magi_core::AgentContextPackage {
                package_id: "agent-context-prompt".to_string(),
                revision: 1,
                parent_task_id: TaskId::new("task-parent-context-prompt"),
                summary: "只检查当前任务包".to_string(),
                constraints: vec!["不得自动读取主对话".to_string()],
                expected_output: "输出检查结论".to_string(),
                references: Vec::new(),
                fork: None,
                supplements: Vec::new(),
                created_at: UtcMillis(1),
                updated_at: UtcMillis(1),
            }),
            accesses: Vec::new(),
        };

        let (prompt, context_messages, summary) =
            dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id.clone()));
        assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
        let context = context_messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(context.contains("[agent-context-package]"));
        assert!(context.contains("只检查当前任务包"));
        assert!(!context.contains("prior session fact for runtime context"));
        assert!(context.contains("Important file summary from current workspace"));
        let summary = summary.expect("context summary");
        assert_eq!(summary.used_turns, 0);
        assert_eq!(summary.used_file_summaries, 1);

        std::fs::write(&file_path, "Externally changed content.")
            .expect("change projected file fact");
        let (prompt, context_messages, summary) =
            dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id));
        assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
        let context = context_messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!context.contains("Important file summary from current workspace"));
        assert_eq!(summary.expect("context summary").used_file_summaries, 0);
    }

    #[test]
    fn assemble_prompt_without_workspace_does_not_query_default_workspace_context() {
        let session_id = SessionId::new("session-no-workspace-context");
        let knowledge_store = KnowledgeStore::new();
        knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "kb-default-workspace".to_string(),
            kind: KnowledgeKind::Learning,
            title: "Default workspace note".to_string(),
            content: "This note must not leak into workspace-less task prompts.".to_string(),
            tags: vec!["leak-check".to_string()],
            workspace_id: Some(WorkspaceId::new("default")),
            source_ref: Some("memory/default.md".to_string()),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        });
        let dispatcher = dispatcher_with_default_tool_surface()
            .with_context_runtime(Arc::new(ContextRuntime::new(
                knowledge_store,
                MemoryStore::new(),
            )))
            .with_context_budget(ContextBudget {
                max_turns: 0,
                max_knowledge: 4,
                max_memory: 0,
                max_shared_items: 0,
                max_file_summaries: 0,
            });
        let task = task_with_role("executor", TaskTier::ExecutionChain);

        let (prompt, context_messages, summary) =
            dispatcher.assemble_prompt(None, &task, &session_id, &None);
        assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
        let context = context_messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !context.contains("Default workspace note"),
            "缺少 workspace 时不得伪造 default workspace 并注入知识库内容"
        );
        assert!(summary.is_none());
    }

    #[test]
    fn assemble_prompt_skips_matching_knowledge_when_task_has_no_knowledge_intent() {
        let session_id = SessionId::new("session-task-no-knowledge-intent");
        let workspace_id = WorkspaceId::new("workspace-task-no-knowledge-intent");
        let knowledge_store = KnowledgeStore::new();
        knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "learning-read-readme".to_string(),
            kind: KnowledgeKind::Learning,
            title: "读取 README 文件".to_string(),
            content: "读取 README 文件时使用 file_read。".to_string(),
            tags: vec!["readme".to_string()],
            workspace_id: Some(workspace_id.clone()),
            source_ref: Some("session:test".to_string()),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        });
        let dispatcher = dispatcher_with_default_tool_surface()
            .with_context_runtime(Arc::new(ContextRuntime::new(
                knowledge_store,
                MemoryStore::new(),
            )))
            .with_context_budget(ContextBudget {
                max_turns: 0,
                max_knowledge: 4,
                max_memory: 0,
                max_shared_items: 0,
                max_file_summaries: 0,
            });
        let mut task = task_with_role("executor", TaskTier::ExecutionChain);
        task.title = "读取 README 文件".to_string();
        task.goal = "读取 README 文件".to_string();

        let (prompt, context_messages, summary) =
            dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id));
        assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
        let context = context_messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!context.contains("读取 README 文件时使用 file_read"));
        assert_eq!(summary.expect("context summary").used_knowledge, 0);
        let events = dispatcher.event_bus.snapshot().recent_events;
        let diagnostic = events
            .iter()
            .find(|event| event.event_type == "knowledge.context.selected")
            .expect("knowledge diagnostic event");
        assert_eq!(diagnostic.payload["consumer"], "task_execution");
        assert_eq!(diagnostic.payload["decision"], "not_needed");
        assert_eq!(diagnostic.payload["injected_count"], 0);
    }

    #[test]
    fn assemble_prompt_injects_full_relevant_adr_and_records_knowledge_id() {
        let session_id = SessionId::new("session-task-adr-context");
        let workspace_id = WorkspaceId::new("workspace-task-adr-context");
        let knowledge_store = KnowledgeStore::new();
        let adr_content = format!(
            "运行态采用单一事实源，事件事实负责写入，只读投影负责展示。{}最终约束是禁止多个状态源互相覆盖。",
            "背景信息。".repeat(24)
        );
        knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "adr-single-source".to_string(),
            kind: KnowledgeKind::Adr,
            title: "为什么运行态采用单一事实源架构".to_string(),
            content: adr_content,
            tags: vec!["架构".to_string(), "单一事实源".to_string()],
            workspace_id: Some(workspace_id.clone()),
            source_ref: Some("adr:runtime-state".to_string()),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        });
        let dispatcher = dispatcher_with_default_tool_surface()
            .with_context_runtime(Arc::new(ContextRuntime::new(
                knowledge_store,
                MemoryStore::new(),
            )))
            .with_context_budget(ContextBudget {
                max_turns: 0,
                max_knowledge: 4,
                max_memory: 0,
                max_shared_items: 0,
                max_file_summaries: 0,
            });
        let mut task = task_with_role("executor", TaskTier::ExecutionChain);
        task.title = "分析运行态架构决策".to_string();
        task.goal = "说明为什么运行态采用单一事实源架构".to_string();

        let (prompt, context_messages, summary) =
            dispatcher.assemble_prompt(None, &task, &session_id, &Some(workspace_id));
        assert_eq!(prompt, format!("{}\n\n{}", task.title, task.goal));
        let context = context_messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(context.contains("[reference:knowledge:adr]"));
        assert!(context.contains("最终约束是禁止多个状态源互相覆盖"));
        let summary = summary.expect("context summary");
        assert_eq!(summary.used_knowledge, 1);
        assert_eq!(summary.knowledge_ids, vec!["adr-single-source".to_string()]);
        let events = dispatcher.event_bus.snapshot().recent_events;
        let diagnostic = events
            .iter()
            .find(|event| event.event_type == "knowledge.context.selected")
            .expect("knowledge diagnostic event");
        assert_eq!(diagnostic.payload["decision"], "injected");
        assert_eq!(
            diagnostic.payload["knowledge_ids"],
            serde_json::json!(["adr-single-source"])
        );
        assert_eq!(
            diagnostic.payload["result_kinds"],
            serde_json::json!(["adr"])
        );
        assert!(diagnostic.payload.get("content").is_none());
    }

    #[test]
    fn tool_visibility_is_filtered_by_role_and_task_tier() {
        let registry = magi_agent_role::AgentRoleRegistry::load_default();
        let worker_task = task_with_role("executor", TaskTier::ExecutionChain);
        let coordinator_task = task_with_role("coordinator", TaskTier::ExecutionChain);

        assert!(!task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::AgentSpawn
        ));
        assert!(!task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::AgentWait
        ));
        assert!(task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::ContextSearch
        ));
        assert!(task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::ContextRead
        ));
        assert!(task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::ContextRequest
        ));
        assert!(task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::AgentSpawn
        ));
        assert!(task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::AgentWait
        ));
        assert!(task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::AgentSend
        ));
        assert!(!task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::ContextSearch
        ));
        assert!(task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::MemoryWrite
        ));
        assert!(task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::CreateGoal
        ));
        let mut nested_coordinator = coordinator_task.clone();
        nested_coordinator.parent_task_id = Some(TaskId::new("task-parent"));
        nested_coordinator.root_task_id = coordinator_task.task_id.clone();
        assert!(!task_can_see_builtin_tool(
            Some(&nested_coordinator),
            Some(&registry),
            BuiltinToolName::AgentSpawn
        ));
        let mut detached_coordinator = coordinator_task.clone();
        detached_coordinator.root_task_id = TaskId::new("task-other-root");
        assert!(!task_can_see_builtin_tool(
            Some(&detached_coordinator),
            Some(&registry),
            BuiltinToolName::AgentWait
        ));
        assert!(!task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::CreateGoal
        ));
        assert!(!task_can_see_builtin_tool(
            None,
            Some(&registry),
            BuiltinToolName::AgentSpawn
        ));
        assert!(!task_can_see_builtin_tool(
            None,
            Some(&registry),
            BuiltinToolName::AgentWait
        ));
        assert!(
            task_can_see_builtin_tool(None, Some(&registry), BuiltinToolName::CreateGoal),
            "主线 session turn 没有 Task 包装时也必须能调用 Goal 工具"
        );
        assert!(task_can_see_builtin_tool(
            None,
            Some(&registry),
            BuiltinToolName::UpdateGoal
        ));
        assert!(task_can_see_builtin_tool(
            None,
            Some(&registry),
            BuiltinToolName::GitBranchSwitch
        ));
        assert!(task_can_see_builtin_tool(
            Some(&coordinator_task),
            Some(&registry),
            BuiltinToolName::GitMerge
        ));
        assert!(!task_can_see_builtin_tool(
            Some(&worker_task),
            Some(&registry),
            BuiltinToolName::GitBranchSwitch
        ));
        assert!(
            task_can_see_builtin_tool(
                Some(&worker_task),
                Some(&registry),
                BuiltinToolName::AskUserQuestion,
            ),
            "子代理可以请求用户澄清，但不能因此取得协调权限"
        );
    }

    #[tokio::test]
    async fn dispatch_quiesce_waits_for_every_in_flight_dispatch_of_root() {
        let state = Arc::new(DispatchQuiesceState::default());
        let root_task_id = TaskId::new("root-dispatch-quiesce");
        let first = state.begin(root_task_id.clone());
        let second = state.begin(root_task_id.clone());

        let waiting = tokio::spawn({
            let state = Arc::clone(&state);
            let root_task_id = root_task_id.clone();
            async move { state.wait_for_root(&root_task_id).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), waiting)
                .await
                .is_err(),
            "仍有 dispatch 时 quiesce 不应提前返回"
        );

        drop(first);
        let waiting = tokio::spawn({
            let state = Arc::clone(&state);
            let root_task_id = root_task_id.clone();
            async move { state.wait_for_root(&root_task_id).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), waiting)
                .await
                .is_err(),
            "仍有第二个 dispatch 时 quiesce 不应提前返回"
        );

        drop(second);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.wait_for_root(&root_task_id),
        )
        .await
        .expect("全部 dispatch 退出后 quiesce 应返回");
    }

    #[test]
    fn dispatch_quiesce_task_scope_does_not_wait_for_its_caller_or_siblings() {
        let state = Arc::new(DispatchQuiesceState::default());
        let root = TaskId::new("scope-root");
        let child = TaskId::new("scope-child");
        let leaf = TaskId::new("scope-leaf");
        let _caller = state.begin_task(root.clone(), root.clone());
        let _sibling = state.begin_task(root.clone(), TaskId::new("scope-sibling"));
        let first = state.begin_task(root.clone(), child.clone());
        let second = state.begin_task(root.clone(), child.clone());
        let descendant = state.begin_task(root, leaf.clone());
        let ids = vec![child, leaf];
        assert!(!state.wait_for_tasks_blocking(&ids, std::time::Duration::ZERO));
        drop(first);
        assert!(!state.wait_for_tasks_blocking(&ids, std::time::Duration::ZERO));
        drop(second);
        assert!(!state.wait_for_tasks_blocking(&ids, std::time::Duration::ZERO));
        let (ready, waiting) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn({
            let state = state.clone();
            move || {
                ready.send(()).unwrap();
                state.wait_for_tasks_blocking(&ids, std::time::Duration::from_secs(2))
            }
        });
        waiting.recv().unwrap();
        drop(descendant);
        assert!(
            waiter.join().unwrap(),
            "子树退出后不能再等仍在运行的父任务或兄弟"
        );
    }

    #[tokio::test]
    async fn dispatch_quiesce_uses_current_transfer_owner_without_retagging_in_flight_calls() {
        let store = magi_orchestrator::task_store::TaskStore::new();
        let mut root = task_with_role("coordinator", TaskTier::ExecutionChain);
        root.status = magi_core::TaskStatus::Running;
        let mut child = task_with_role("executor", TaskTier::ExecutionChain);
        child.root_task_id = root.task_id.clone();
        child.parent_task_id = Some(root.task_id.clone());
        child.executor_binding.as_mut().unwrap().agent_activation =
            Some(magi_core::AgentActivation {
                agent_id: child.task_id.clone(),
                session_id: SessionId::new("quiesce-transfer"),
                lifetime: magi_core::AgentLifetime::Session,
                generation: 1,
                previous_task_id: None,
            });
        store.insert_task(root.clone()).unwrap();
        store.insert_task(child.clone()).unwrap();
        let state = Arc::new(DispatchQuiesceState::default());
        let source = state.begin_task(root.task_id.clone(), root.task_id.clone());
        let work = state.begin_task(root.task_id.clone(), child.task_id.clone());
        drop(source);
        let waiting = state.wait_for_execution_scope(&store, &root.task_id);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        store
            .transfer_agent_subtree(
                &root.task_id,
                &child.task_id,
                magi_core::AgentRunTransfer {
                    operation_id: "quiesce-handoff".into(),
                    session_id: SessionId::new("quiesce-transfer"),
                    source_turn_id: "source-turn".into(),
                    transferred_by: root.task_id.clone(),
                    activation_generation: 1,
                    owner: magi_core::AgentRunOwner::Session,
                    transferred_at: UtcMillis::now(),
                },
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut waiting)
            .await
            .expect("原拥有方不能等待已转交执行");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                state.wait_for_execution_scope(&store, &child.task_id)
            )
            .await
            .is_err()
        );
        drop(work);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.wait_for_execution_scope(&store, &child.task_id),
        )
        .await
        .expect("新拥有方须等待原线程真实退出");
    }

    #[test]
    fn agent_mode_prompt_matches_role_and_current_tool_surface() {
        let dispatcher = dispatcher_with_default_tool_surface();
        let session_id = SessionId::new("session-multi-agent-mode");
        let workspace_id = None;

        let coordinator_task = task_with_role("coordinator", TaskTier::ExecutionChain);
        let prompt_for_task = |task: &Task, skill_name: Option<&str>| {
            let tools = dispatcher.build_tool_definitions(
                task,
                skill_name,
                AccessProfile::FullAccess,
                workspace_id.as_ref(),
            );
            crate::prompt_utils::current_agent_mode_prompt(
                task,
                dispatcher.execution_registry.execution_admission().limits(),
                &dispatcher.agent_role_registry,
                &tools,
            )
            .unwrap()
        };
        let (_, coordinator_context, _) =
            dispatcher.assemble_prompt(None, &coordinator_task, &session_id, &workspace_id);
        assert!(
            coordinator_context.iter().all(|message| {
                !message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .contains("多代理协作（root coordinator 必须遵守）")
            }),
            "运行时派发规则不能混入任务事实"
        );
        let coordinator_prompt = prompt_for_task(&coordinator_task, None);
        assert!(
            coordinator_prompt.contains("多代理协作（root coordinator 必须遵守）"),
            "root coordinator prompt 必须包含多代理协作规则: {coordinator_prompt}"
        );
        assert!(
            coordinator_prompt.contains("明确要求真实代理执行具体工作")
                && coordinator_prompt.contains("必须通过 agent_spawn 创建真实代理"),
            "明确 subagent 请求必须被约束为真实 agent_spawn"
        );
        assert!(
            coordinator_prompt.contains("runtime_internal=true")
                && coordinator_prompt.contains("可派发角色"),
            "root coordinator 必须知道代理工具可直接调用以及可派发角色"
        );

        let worker_task = task_with_role("executor", TaskTier::ExecutionChain);
        let worker_prompt = prompt_for_task(&worker_task, None);
        assert!(
            worker_prompt.contains("子代理模式（当前模式：worker；worker 必须遵守）")
                && worker_prompt.contains("不要继续创建代理"),
            "worker prompt 必须说明自身不能继续分派"
        );
        assert!(
            !worker_prompt.contains("多代理协作（root coordinator 必须遵守）"),
            "worker prompt 不能收到 root coordinator 派发策略"
        );

        for tool_policy in ["no_tools", "deny_spawn", "allow_file_read"] {
            let mut restricted = coordinator_task.clone();
            let policy = restricted.policy_snapshot.as_mut().unwrap();
            match tool_policy {
                "no_tools" => policy.command_mode = "no_tools".into(),
                "deny_spawn" => policy.denied_tools = vec!["agent_spawn".into()],
                _ => policy.allowed_tools = vec!["file_read".into()],
            }
            let prompt = prompt_for_task(&restricted, None);
            assert!(
                prompt.contains("本轮 tools 未提供 agent_spawn"),
                "{tool_policy}: {prompt}"
            );
            assert!(!prompt.contains("必须通过 agent_spawn 创建真实代理"));
            assert!(!prompt.contains("可派发角色"));
        }

        // 使用真实内置角色，检查角色提示、运行时提示和工具面不会相互矛盾。
        for role_id in ["architect", "executor", "reviewer", "explorer", "tester"] {
            let mut delegated = task_with_role(role_id, TaskTier::ExecutionChain);
            delegated.parent_task_id = Some(coordinator_task.task_id.clone());
            delegated.root_task_id = coordinator_task.task_id.clone();
            delegated
                .executor_binding
                .as_mut()
                .unwrap()
                .delegation_allowed = true;
            let prompt = prompt_for_task(&delegated, None);
            let role = dispatcher.agent_role_registry.get(role_id).unwrap();
            assert!(prompt.contains("受委派代理模式"), "{role_id}: {prompt}");
            assert!(prompt.contains("获授委派能力不要求创建子代理"));
            assert!(!prompt.contains("不要继续创建代理"));
            assert!(role.system_prompt.contains("未获授权时不能创建更多子代理"));
            let names = dispatcher
                .build_tool_definitions(
                    &delegated,
                    None,
                    magi_core::AccessProfile::Restricted,
                    Some(&WorkspaceId::new("delegation-prompt-workspace")),
                )
                .into_iter()
                .map(|tool| tool.function.name)
                .collect::<Vec<_>>();
            for tool in [
                "agent_spawn",
                "agent_wait",
                "agent_cancel",
                "agent_send",
                "agent_apply",
                "ask_user_question",
            ] {
                assert!(
                    names.iter().any(|name| name == tool),
                    "{role_id} 缺少 {tool}"
                );
            }
            for tool in [
                "create_goal",
                "update_plan",
                "memory_write",
                "git_branch_switch",
            ] {
                assert!(
                    !names.iter().any(|name| name == tool),
                    "{role_id} 不应拥有 {tool}"
                );
            }
        }
    }

    #[test]
    fn read_only_coordinator_surface_keeps_internal_coordination_tools() {
        let dispatcher = dispatcher_with_default_tool_surface();
        let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
        task.policy_snapshot
            .as_mut()
            .expect("policy")
            .access_profile = magi_core::AccessProfile::ReadOnly;

        let names = dispatcher
            .build_tool_definitions(
                &task,
                None,
                magi_core::AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        for hidden in [
            "file_write",
            "file_patch",
            "apply_patch",
            "file_remove",
            "file_mkdir",
            "file_copy",
            "file_move",
            "memory_write",
        ] {
            assert!(
                !names.iter().any(|name| name == hidden),
                "read-only 工具面不应暴露写工具 {hidden}: {names:?}"
            );
        }
        assert!(names.iter().any(|name| name == "file_read"));
        assert!(names.iter().any(|name| name == "search_text"));
        assert!(names.iter().any(|name| name == "shell_exec"));
        assert!(names.iter().any(|name| name == "agent_spawn"));
        assert!(names.iter().any(|name| name == "agent_wait"));
        for internal in ["get_goal", "create_goal", "update_goal", "update_plan"] {
            assert!(
                names.iter().any(|name| name == internal),
                "只读访问只限制外部副作用，不能屏蔽内部协调工具 {internal}: {names:?}"
            );
        }
    }

    #[test]
    fn full_access_with_empty_allowed_tools_keeps_shell_exec_visible() {
        let dispatcher = dispatcher_with_default_tool_surface();
        let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
        let policy = task.policy_snapshot.as_mut().expect("policy");
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy.command_mode = "full".to_string();
        policy.allowed_tools.clear();
        policy.denied_tools.clear();

        let names = dispatcher
            .build_tool_definitions(
                &task,
                None,
                magi_core::AccessProfile::FullAccess,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "shell_exec"));
        assert!(names.iter().any(|name| name == "file_write"));
        assert!(names.iter().any(|name| name == "apply_patch"));
    }

    #[test]
    fn tool_surface_separates_coordinator_worker_and_delegation_grant() {
        let root = task_with_role("coordinator", TaskTier::ExecutionChain);
        let mut worker = root.clone();
        worker.parent_task_id = Some(root.task_id.clone());
        worker.task_id = TaskId::new("worker-surface");
        let mut delegator = worker.clone();
        delegator
            .executor_binding
            .as_mut()
            .unwrap()
            .delegation_allowed = true;
        let dispatcher = dispatcher_with_default_tool_surface();
        for (task, can_spawn, can_plan) in [
            (&root, true, true),
            (&worker, false, false),
            (&delegator, true, false),
            (&worker, false, false),
        ] {
            let definitions =
                dispatcher.build_tool_definitions(task, None, AccessProfile::FullAccess, None);
            assert_eq!(
                definitions
                    .iter()
                    .any(|tool| tool.function.name == "agent_spawn"),
                can_spawn
            );
            assert_eq!(
                definitions
                    .iter()
                    .any(|tool| tool.function.name == "update_plan"),
                can_plan
            );
        }
    }

    #[test]
    fn coordinator_model_tool_surface_contains_agent_contracts() {
        let dispatcher = dispatcher_with_default_tool_surface();
        let task = task_with_role("coordinator", TaskTier::ExecutionChain);
        let definitions = dispatcher.build_tool_definitions(
            &task,
            None,
            magi_core::AccessProfile::Restricted,
            Some(&WorkspaceId::new("test-workspace")),
        );

        for name in ["agent_spawn", "agent_send", "agent_wait"] {
            let definition = definitions
                .iter()
                .find(|definition| definition.function.name == name)
                .unwrap_or_else(|| panic!("coordinator model request must contain {name}"));
            assert_eq!(definition.kind, "function");
            assert!(
                !definition.function.description.trim().is_empty(),
                "{name} definition must be model-facing"
            );
            assert_eq!(definition.function.parameters["type"], "object");
        }
    }

    #[test]
    fn access_profile_tool_surface_distinguishes_coordinator_and_subagent_roles() {
        let dispatcher = dispatcher_with_default_tool_surface();

        for access_profile in [
            magi_core::AccessProfile::ReadOnly,
            magi_core::AccessProfile::Restricted,
            magi_core::AccessProfile::FullAccess,
        ] {
            let mut coordinator = task_with_role("coordinator", TaskTier::ExecutionChain);
            coordinator
                .policy_snapshot
                .as_mut()
                .expect("coordinator policy")
                .access_profile = access_profile;
            let coordinator_names = dispatcher
                .build_tool_definitions(
                    &coordinator,
                    None,
                    access_profile,
                    Some(&WorkspaceId::new("test-workspace")),
                )
                .into_iter()
                .map(|definition| definition.function.name)
                .collect::<Vec<_>>();
            assert!(
                coordinator_names.iter().any(|name| name == "agent_spawn"),
                "主线在 {access_profile:?} 模式下都必须能够创建继承同模式的子代理"
            );

            let mut worker = task_with_role("executor", TaskTier::ExecutionChain);
            worker
                .policy_snapshot
                .as_mut()
                .expect("worker policy")
                .access_profile = access_profile;
            let worker_names = dispatcher
                .build_tool_definitions(
                    &worker,
                    None,
                    access_profile,
                    Some(&WorkspaceId::new("test-workspace")),
                )
                .into_iter()
                .map(|definition| definition.function.name)
                .collect::<Vec<_>>();
            assert!(worker_names.iter().any(|name| name == "file_read"));
            assert!(worker_names.iter().any(|name| name == "shell_exec"));
            assert!(
                worker_names.iter().any(|name| name == "ask_user_question"),
                "子代理应在 {access_profile:?} 模式下看到自己的提问入口"
            );
            assert!(
                !worker_names.iter().any(|name| name == "agent_spawn"),
                "子代理在 {access_profile:?} 模式下都不能递归创建代理"
            );
            assert_eq!(
                worker_names.iter().any(|name| name == "file_write"),
                access_profile != magi_core::AccessProfile::ReadOnly,
                "子代理文件写入工具可见性必须与访问模式一致"
            );
        }
    }

    #[test]
    fn ready_browser_host_is_exposed_to_regular_execution_worker() {
        let dispatcher = dispatcher_with_ready_browser_tool_surface();
        let worker = task_with_role("executor", TaskTier::ExecutionChain);
        let names = dispatcher
            .build_tool_definitions(
                &worker,
                None,
                magi_core::AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "shell_exec"));
        for browser_tool in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_press",
            "browser_scroll",
            "browser_screenshot",
            "browser_tabs",
            "browser_viewport",
        ] {
            assert!(
                names.iter().any(|name| name == browser_tool),
                "普通执行 Worker 未发现 {browser_tool}: {names:?}"
            );
        }
    }

    #[test]
    fn task_tool_surface_hides_workspace_tools_without_workspace() {
        let dispatcher = dispatcher_with_ready_browser_tool_surface();
        let task = task_with_role("executor", TaskTier::ExecutionChain);
        let names = dispatcher
            .build_tool_definitions(&task, None, magi_core::AccessProfile::Restricted, None)
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        for hidden in ["file_read", "shell_exec", "git_status", "search_text"] {
            assert!(
                !names.iter().any(|name| name == hidden),
                "无 workspace 的任务工具目录不能暴露 {hidden}: {names:?}"
            );
        }
        assert!(
            names.iter().any(|name| name == "browser_navigate"),
            "无 workspace 的任务工具目录必须保留浏览器工具: {names:?}"
        );
    }

    #[test]
    fn read_only_command_mode_hides_write_tools_even_with_full_access_profile() {
        let dispatcher = dispatcher_with_default_tool_surface();
        let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
        let policy = task.policy_snapshot.as_mut().expect("policy");
        policy.access_profile = magi_core::AccessProfile::FullAccess;
        policy.command_mode = "read_only".to_string();

        let names = dispatcher
            .build_tool_definitions(
                &task,
                None,
                magi_core::AccessProfile::FullAccess,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "file_read"));
        assert!(!names.iter().any(|name| name == "file_write"));
        assert!(names.iter().any(|name| name == "agent_spawn"));
    }

    #[test]
    fn active_skill_custom_bindings_are_exposed_in_tool_surface() {
        let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
            let registry = magi_skill_runtime::SkillRegistry::new();
            registry.register(magi_skill_runtime::SkillDefinition {
                skill_id: "code-review".to_string(),
                title: "代码审查".to_string(),
                instruction: "检查稳定性风险。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "quality".to_string(),
                    tags: vec!["review".to_string()],
                },
                restrict_standard_tools: true,
                allowed_tools: vec![],
                custom_tool_bindings: vec![magi_skill_runtime::CustomToolBinding {
                    binding_id: "review-mcp".to_string(),
                    tool_name: "echo.describe".to_string(),
                    description: "回显描述".to_string(),
                    bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp,
                    dispatch_action: magi_bridge_client::BridgeDispatchAction::McpToolCall,
                    bridge_target: "loopback-mcp".to_string(),
                }],
                prompt_priority: 50,
            });
            magi_skill_runtime::SkillRuntime::new(registry)
        }));
        let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
        let workspace = WorkspaceId::new("test-workspace");
        let names = dispatcher
            .build_tool_definitions(
                &task,
                Some("code-review"),
                magi_core::AccessProfile::Restricted,
                Some(&workspace),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(
            names
                .iter()
                .any(|name| name == "skill__code-review__review-mcp"),
            "active skill custom binding should surface as callable tool"
        );
        assert!(!names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
        assert!(
            !names.iter().any(|name| name == "file_read"),
            "active skill 没有声明 allowed_tools 时不应暴露普通内置工具"
        );
        assert!(
            !names.iter().any(|name| name == "shell_exec"),
            "active skill 没有声明 allowed_tools 时不应暴露 shell_exec"
        );
        let skills = dispatcher.skill_runtime.as_ref().unwrap().registry();
        let original = skills.get("code-review").unwrap();
        let mut tightened = original.clone();
        tightened.custom_tool_bindings.clear();
        skills.register(tightened);
        let tightened_tools = dispatcher.build_tool_definitions(
            &task,
            Some("code-review"),
            AccessProfile::Restricted,
            Some(&workspace),
        );
        assert_eq!(
            tightened_tools
                .iter()
                .map(|definition| definition.function.name.as_str())
                .collect::<Vec<_>>(),
            ["update_execution_steps"],
            "同名 Skill 收紧为封闭空集合后不能继续发布缓存的自定义工具，但执行步骤工具仍须可用"
        );
        skills.register(original);
        for denied in [false, true] {
            let policy = task.policy_snapshot.as_mut().unwrap();
            policy.allowed_tools = if denied {
                vec![]
            } else {
                vec!["file_read".into()]
            };
            policy.denied_tools = if denied {
                vec!["skill__code-review__review-mcp".into()]
            } else {
                vec![]
            };
            let tools = dispatcher.build_tool_definitions(
                &task,
                Some("code-review"),
                AccessProfile::Restricted,
                None,
            );
            assert!(
                !tools
                    .iter()
                    .any(|tool| tool.function.name == "skill__code-review__review-mcp"),
                "Task 白名单和明确禁止必须约束 Skill 自定义工具，不能只在执行时拒绝"
            );
        }
    }

    #[test]
    fn active_skill_is_injected_directly_without_reexposing_skill_apply() {
        let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
            let registry = magi_skill_runtime::SkillRegistry::new();
            registry.register(magi_skill_runtime::SkillDefinition {
                skill_id: "direct-skill".to_string(),
                title: "直接 Skill".to_string(),
                instruction: "直接注入。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "workflow".to_string(),
                    tags: vec![],
                },
                restrict_standard_tools: false,
                allowed_tools: vec![],
                custom_tool_bindings: vec![],
                prompt_priority: 50,
            });
            magi_skill_runtime::SkillRuntime::new(registry)
        }));

        let names = dispatcher
            .build_tool_definitions(
                &task_with_role("coordinator", TaskTier::ExecutionChain),
                Some("direct-skill"),
                magi_core::AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(!names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
        assert!(
            names.iter().any(|name| name == "file_read"),
            "未声明 allowed_tools 的 prompt-only Skill 应继承标准只读工具"
        );
    }

    #[test]
    fn live_mcp_tools_are_exposed_on_the_model_tool_surface() {
        let mut dispatcher = dispatcher_with_default_tool_surface();
        let registry = dispatcher
            .tool_registry
            .take()
            .expect("test dispatcher should own a tool registry")
            .with_external_tool_catalog_provider(Arc::new(|| {
                magi_tool_runtime::ExternalToolCatalogSnapshot {
                    instruction_skill_count: 0,
                    mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                        server_id: "repo-tools".to_string(),
                        server_name: "Repository Tools".to_string(),
                        model_tool_name: "mcp__repo-tools__inspect".to_string(),
                        tool_name: "inspect".to_string(),
                        description: "Inspect repository".to_string(),
                        read_only: false,
                        input_schema: serde_json::json!({
                            "type": "object",
                            "properties": { "path": { "type": "string" } }
                        }),
                    }],
                    ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
                }
            }));
        dispatcher.tool_registry = Some(registry);

        let definitions = dispatcher.build_tool_definitions(
            &task_with_role("coordinator", TaskTier::ExecutionChain),
            None,
            magi_core::AccessProfile::Restricted,
            Some(&WorkspaceId::new("test-workspace")),
        );
        let mcp = definitions
            .iter()
            .find(|definition| definition.function.name == "mcp__repo-tools__inspect");
        assert!(
            mcp.is_none(),
            "MCP schema 默认延迟加载，首轮不能占用完整工具面"
        );
    }

    #[test]
    fn prompt_only_skill_keeps_live_mcp_tools() {
        let mut dispatcher = dispatcher_with_default_tool_surface();
        let registry = dispatcher
            .tool_registry
            .take()
            .expect("test dispatcher should own a tool registry")
            .with_external_tool_catalog_provider(Arc::new(|| {
                magi_tool_runtime::ExternalToolCatalogSnapshot {
                    instruction_skill_count: 0,
                    mcp_tools: vec![magi_tool_runtime::ExternalMcpToolCatalogEntry {
                        server_id: "repo-tools".to_string(),
                        server_name: "Repository Tools".to_string(),
                        model_tool_name: "mcp__repo-tools__inspect".to_string(),
                        tool_name: "inspect".to_string(),
                        description: "Inspect repository".to_string(),
                        read_only: false,
                        input_schema: serde_json::json!({ "type": "object" }),
                    }],
                    ..magi_tool_runtime::ExternalToolCatalogSnapshot::default()
                }
            }));
        dispatcher.tool_registry = Some(registry);
        dispatcher = dispatcher.with_skill_runtime(Arc::new({
            let registry = magi_skill_runtime::SkillRegistry::new();
            registry.register(magi_skill_runtime::SkillDefinition {
                skill_id: "prompt-only".to_string(),
                title: "Prompt Only".to_string(),
                instruction: "先取证，再回答。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "workflow".to_string(),
                    tags: vec![],
                },
                restrict_standard_tools: false,
                allowed_tools: vec![],
                custom_tool_bindings: vec![],
                prompt_priority: 50,
            });
            magi_skill_runtime::SkillRuntime::new(registry)
        }));

        let names = dispatcher
            .build_tool_definitions(
                &task_with_role("coordinator", TaskTier::ExecutionChain),
                Some("prompt-only"),
                magi_core::AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "file_read"));
        assert!(!names.iter().any(|name| name == "mcp__repo-tools__inspect"));
        assert!(!names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
    }

    #[test]
    fn selected_skill_short_name_resolves_before_prompt_and_tool_surface_build() {
        let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
            let registry = magi_skill_runtime::SkillRegistry::new();
            registry.register(magi_skill_runtime::SkillDefinition {
                skill_id: "owner/repo/skills/code-review".to_string(),
                title: "代码审查".to_string(),
                instruction: "检查稳定性。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "quality".to_string(),
                    tags: vec![],
                },
                restrict_standard_tools: true,
                allowed_tools: vec!["file_read".to_string()],
                custom_tool_bindings: vec![],
                prompt_priority: 50,
            });
            magi_skill_runtime::SkillRuntime::new(registry)
        }));

        assert_eq!(
            dispatcher.resolve_registered_skill_id(Some("code-review")),
            Ok(Some("owner/repo/skills/code-review".to_string()))
        );
        let registry = dispatcher.skill_runtime.as_ref().unwrap().registry();
        let mut duplicate = registry.get("owner/repo/skills/code-review").unwrap();
        duplicate.skill_id = "another/repo/code-review".into();
        registry.register(duplicate);
        assert!(
            dispatcher
                .resolve_registered_skill_id(Some("code-review"))
                .unwrap_err()
                .contains("skill_ambiguous")
        );
        registry.replace([]);
        assert!(
            dispatcher
                .resolve_registered_skill_id(Some("owner/repo/skills/code-review"))
                .unwrap_err()
                .contains("skill_unavailable")
        );
        let dispatcher = dispatcher_with_default_tool_surface();
        assert!(
            dispatcher
                .resolve_registered_skill_id(Some("code-review"))
                .unwrap_err()
                .contains("skill_unavailable")
        );
        for absent in [None, Some(" ")] {
            assert_eq!(dispatcher.resolve_registered_skill_id(absent), Ok(None));
        }
    }

    #[test]
    fn read_only_tool_surface_hides_mcp_skill_bindings() {
        let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
            let registry = magi_skill_runtime::SkillRegistry::new();
            registry.register(magi_skill_runtime::SkillDefinition {
                skill_id: "mixed-skill".to_string(),
                title: "混合 Skill".to_string(),
                instruction: "提供只读模型辅助和 MCP 外接工具。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "quality".to_string(),
                    tags: vec!["mixed".to_string()],
                },
                restrict_standard_tools: true,
                allowed_tools: vec![],
                custom_tool_bindings: vec![
                    magi_skill_runtime::CustomToolBinding {
                        binding_id: "mcp-tool".to_string(),
                        tool_name: "echo.inspect".to_string(),
                        description: "MCP 检查".to_string(),
                        bridge_kind: magi_bridge_client::BridgeBindingKind::Mcp,
                        dispatch_action: magi_bridge_client::BridgeDispatchAction::McpToolCall,
                        bridge_target: "loopback-mcp".to_string(),
                    },
                    magi_skill_runtime::CustomToolBinding {
                        binding_id: "model-tool".to_string(),
                        tool_name: "model.summarize".to_string(),
                        description: "模型摘要".to_string(),
                        bridge_kind: magi_bridge_client::BridgeBindingKind::Model,
                        dispatch_action: magi_bridge_client::BridgeDispatchAction::ModelPrompt,
                        bridge_target: "loopback-model".to_string(),
                    },
                ],
                prompt_priority: 50,
            });
            magi_skill_runtime::SkillRuntime::new(registry)
        }));
        let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
        task.policy_snapshot
            .as_mut()
            .expect("policy")
            .access_profile = magi_core::AccessProfile::ReadOnly;

        let names = dispatcher
            .build_tool_definitions(
                &task,
                Some("mixed-skill"),
                magi_core::AccessProfile::ReadOnly,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(
            !names
                .iter()
                .any(|name| name == "skill__mixed-skill__mcp-tool"),
            "read-only 工具面不应暴露 MCP Skill 工具: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|name| name == "skill__mixed-skill__model-tool"),
            "read-only 工具面仍可保留模型型 Skill 工具: {names:?}"
        );
    }

    #[test]
    fn agent_spawn_requires_wait_and_cancel_in_the_effective_tool_surface() {
        for missing in ["agent_wait", "agent_cancel"] {
            let dispatcher = dispatcher_with_default_tool_surface();
            let mut task = task_with_role("coordinator", TaskTier::ExecutionChain);
            task.policy_snapshot.as_mut().unwrap().denied_tools = vec![missing.into()];
            let tools =
                dispatcher.build_tool_definitions(&task, None, AccessProfile::Restricted, None);
            assert!(
                !tools.iter().any(|tool| tool.function.name == "agent_spawn"),
                "missing {missing} must prevent creation of an unmanageable child"
            );
            assert!(tools.iter().any(|tool| tool.function.name == "agent_send"));
            let prompt = crate::prompt_utils::current_agent_mode_prompt(
                &task,
                dispatcher.execution_registry.execution_admission().limits(),
                &dispatcher.agent_role_registry,
                &tools,
            )
            .unwrap();
            assert!(prompt.contains("本轮 tools 未提供 agent_spawn"));
            assert!(!prompt.contains("可派发角色"));
        }
    }

    #[test]
    fn active_skill_allowed_tools_filter_builtin_tool_surface() {
        let dispatcher = dispatcher_with_default_tool_surface().with_skill_runtime(Arc::new({
            let registry = magi_skill_runtime::SkillRegistry::new();
            registry.register(magi_skill_runtime::SkillDefinition {
                skill_id: "read-only-skill".to_string(),
                title: "只读 Skill".to_string(),
                instruction: "只能读取文件。".to_string(),
                metadata: magi_skill_runtime::SkillMetadata {
                    category: "quality".to_string(),
                    tags: vec!["read".to_string()],
                },
                restrict_standard_tools: true,
                allowed_tools: vec!["file_read".to_string()],
                custom_tool_bindings: vec![],
                prompt_priority: 50,
            });
            magi_skill_runtime::SkillRuntime::new(registry)
        }));
        let task = task_with_role("coordinator", TaskTier::ExecutionChain);

        let names = dispatcher
            .build_tool_definitions(
                &task,
                Some("read-only-skill"),
                magi_core::AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            )
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "file_read"));
        assert!(!names.iter().any(|name| name == SKILL_APPLY_TOOL_NAME));
        assert!(!names.iter().any(|name| name == "search_text"));
        assert!(!names.iter().any(|name| name == "shell_exec"));
        assert!(!names.iter().any(|name| name == "agent_spawn"));
        // Goal 生命周期工具继续由宿主拥有；重建目录不能被 Skill 隐藏，
        // Task 的显式禁止仍须生效。
        let mut goal_task = task.clone();
        goal_task.executor_binding.as_mut().unwrap().goal_mode = true;
        goal_task
            .policy_snapshot
            .as_mut()
            .unwrap()
            .denied_tools
            .push("update_plan".into());
        let goal_tools = dispatcher.build_tool_definitions(
            &goal_task,
            Some("read-only-skill"),
            AccessProfile::Restricted,
            Some(&WorkspaceId::new("test-workspace")),
        );
        for name in ["get_goal", "create_goal", "update_goal", "file_read"] {
            assert!(goal_tools.iter().any(|tool| tool.function.name == name));
        }
        assert!(
            !goal_tools
                .iter()
                .any(|tool| tool.function.name == "update_plan")
        );
        let prompt = crate::prompt_utils::current_agent_mode_prompt(
            &task,
            dispatcher.execution_registry.execution_admission().limits(),
            &dispatcher.agent_role_registry,
            &dispatcher.build_tool_definitions(
                &task,
                Some("read-only-skill"),
                AccessProfile::Restricted,
                Some(&WorkspaceId::new("test-workspace")),
            ),
        )
        .unwrap();
        assert!(prompt.contains("本轮 tools 未提供 agent_spawn"));
        assert!(!prompt.contains("必须通过 agent_spawn 创建真实代理"));

        // 同名配置重载及移除必须重新解析范围，不能因名称相同复用旧目录。
        let runtime = dispatcher.skill_runtime.as_ref().unwrap();
        let registry = runtime.registry();
        let mut skill = registry.get("read-only-skill").unwrap();
        skill.allowed_tools = vec!["search_text".into()];
        registry.register(skill);
        let tools = || {
            dispatcher
                .build_tool_definitions(
                    &task,
                    Some("read-only-skill"),
                    AccessProfile::Restricted,
                    Some(&WorkspaceId::new("test-workspace")),
                )
                .into_iter()
                .map(|definition| definition.function.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(tools(), ["search_text", "update_execution_steps"]);
        let available =
            dispatcher.build_tool_definitions(&task, None, AccessProfile::Restricted, None);
        assert!(
            available
                .iter()
                .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
                .unwrap()
                .function
                .description
                .contains("read-only-skill")
        );
        registry.replace([]);
        assert_eq!(tools(), ["update_execution_steps"]);
        let available =
            dispatcher.build_tool_definitions(&task, None, AccessProfile::Restricted, None);
        assert!(
            !available
                .iter()
                .find(|tool| tool.function.name == SKILL_APPLY_TOOL_NAME)
                .unwrap()
                .function
                .description
                .contains("read-only-skill")
        );
    }

    #[test]
    fn parse_learning_candidates_accepts_well_formed_array() {
        let raw = r#"[
            {"content": "在 magi 中辅助模型必须配置 base_url 才会启用", "tags": ["config"]},
            {"content": "extract_learnings_via_auxiliary 失败时直接跳过，不退化", "tags": []}
        ]"#;
        let result = parse_learning_candidates(raw).expect("应解析成功");
        assert_eq!(result.len(), 2);
        assert!(result[0].tags.iter().any(|t| t == "auto"));
        assert!(result[0].tags.iter().any(|t| t == "learning"));
    }

    #[test]
    fn auxiliary_learning_extraction_exposes_model_invocation_failure() {
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let settings_store = Arc::new(SettingsStore::new());
        settings_store
            .set_section(
                "auxiliary",
                serde_json::json!({
                    "baseUrl": "https://example.test",
                    "apiProtocol": "openai_chat",
                    "apiKey": "secret-auxiliary-test-key",
                    "model": "auxiliary-test-model"
                }),
            )
            .expect("auxiliary config should save");
        let session_id = SessionId::new("session-learning-extraction-failure");
        let workspace_id = Some(WorkspaceId::new("workspace-learning-extraction-failure"));
        let result = extract_learnings_via_auxiliary(
            AuxiliaryModelExtractionContext {
                client: Arc::new(FailingAuxiliaryClient),
                event_bus: &event_bus,
                session_store: &session_store,
                settings_store: Some(&settings_store),
                session_id: &session_id,
                workspace_id: &workspace_id,
                expected_turn_id: None,
            },
            "这是一段用于验证辅助模型失败诊断的会话内容",
        );

        assert!(matches!(
            result,
            Err(LearningExtractionFailure::InvocationFailed)
        ));
        let usage_event = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "model.usage.recorded")
            .expect("auxiliary failure should be recorded");
        assert_eq!(usage_event.payload["executionBinding"]["role"], "auxiliary");
        assert_eq!(
            usage_event.payload["modelConfig"]["model"],
            "auxiliary-test-model"
        );

        let dispatcher = dispatcher_with_default_tool_surface();
        let workspace_id = WorkspaceId::new("workspace-learning-extraction-failure");
        dispatcher.publish_learning_extraction_diagnostic(
            &session_id,
            Some(&workspace_id),
            "failed",
            Some(LearningExtractionFailure::InvocationFailed.as_str()),
            0,
            0,
        );
        let event = dispatcher
            .event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .find(|event| event.event_type == "knowledge.learning.extraction")
            .expect("learning extraction diagnostic");
        assert_eq!(event.payload["status"], "failed");
        assert_eq!(event.payload["failure_reason"], "model_invocation_failed");
        assert!(event.payload.get("content").is_none());
    }

    #[test]
    fn parse_learning_candidates_drops_out_of_range_content() {
        let raw = r#"[
            {"content": "太短", "tags": []},
            {"content": "在 magi 中辅助模型必须配置 base_url 才会启用", "tags": []}
        ]"#;
        let result = parse_learning_candidates(raw).expect("仍有一项命中");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_learning_candidates_returns_none_when_all_filtered() {
        let raw = r#"[{"content": "短", "tags": []}]"#;
        assert!(parse_learning_candidates(raw).is_none());
    }

    #[test]
    fn parse_learning_candidates_rejects_non_json_payload() {
        let raw = "这不是 JSON，模型胡乱输出";
        assert!(parse_learning_candidates(raw).is_none());
    }

    #[test]
    fn parse_learning_candidates_caps_at_three_items() {
        let mut parts = Vec::new();
        for i in 0..8 {
            parts.push(format!(
                r#"{{"content": "条目编号 {i}：这是一条长度足够的占位内容用于通过过滤", "tags": []}}"#
            ));
        }
        let raw = format!("[{}]", parts.join(","));
        let result = parse_learning_candidates(&raw).expect("应解析成功");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn parse_learning_candidates_filters_pure_tool_sequences() {
        let raw = r#"[
            {"content": "先调用 file_read，然后调用 apply_patch，最后运行 cargo test", "tags": []},
            {"content": "修改状态逻辑后必须同时验证事件事实和只读投影，避免多个状态源互相覆盖", "tags": []}
        ]"#;

        let result = parse_learning_candidates(raw).expect("应保留可复用经验");

        assert_eq!(result.len(), 1);
        assert!(result[0].content.contains("事件事实"));
    }

    #[test]
    fn knowledge_duplicate_recognizes_chinese_paraphrases() {
        let workspace_id = WorkspaceId::new("workspace-learning-duplicate");
        let existing = vec![KnowledgeRecord {
            knowledge_id: "learning-existing".to_string(),
            kind: KnowledgeKind::Learning,
            title: "状态逻辑验证".to_string(),
            content: "修改状态逻辑后必须验证事件投影".to_string(),
            tags: vec![],
            workspace_id: Some(workspace_id.clone()),
            source_ref: Some("session:existing".to_string()),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }];

        assert!(knowledge_duplicate(
            &existing,
            KnowledgeKind::Learning,
            Some(&workspace_id),
            "状态逻辑改动后需要检查事件投影"
        ));
    }

    #[test]
    fn parse_session_memory_slices_accepts_full_object() {
        let raw = r#"{
            "currentWork": "正在收敛辅助模型介入路径，把会话记忆抽取并入主流程",
            "decisions": "决定复用现有 MemoryStore.apply_extraction 落库，不新增 schema",
            "importantContext": "用户强调 cn-engineering-standard，不允许并行实现或兜底",
            "pendingIssues": "Prompt 增强和配置错位修复尚未启动，会在后续两步中处理",
            "nextSteps": "先跑 cargo check 验证，再切入 B 步骤的 Prompt 增强路径"
        }"#;
        let result = parse_session_memory_slices(raw).expect("应解析成功");
        let categories: Vec<&str> = result.iter().map(|s| s.category).collect();
        assert_eq!(
            categories,
            vec![
                "currentWork",
                "decisions",
                "importantContext",
                "pendingIssues",
                "nextSteps"
            ]
        );
    }

    #[test]
    fn parse_session_memory_slices_skips_empty_or_short_categories() {
        let raw = r#"{
            "currentWork": "正在调试会话记忆水位线触发的核心条件，避免每轮都跑",
            "decisions": "",
            "importantContext": "短",
            "pendingIssues": "保留与 TS 版 5 段契约一致，便于未来跨端共用提示词",
            "nextSteps": ""
        }"#;
        let result = parse_session_memory_slices(raw).expect("仍有命中");
        let categories: Vec<&str> = result.iter().map(|s| s.category).collect();
        assert_eq!(categories, vec!["currentWork", "pendingIssues"]);
    }

    #[test]
    fn parse_session_memory_slices_rejects_non_json_payload() {
        assert!(parse_session_memory_slices("not json at all").is_none());
    }

    #[test]
    fn parse_session_memory_slices_returns_none_when_all_filtered() {
        let raw = r#"{
            "currentWork": "",
            "decisions": "短",
            "importantContext": "",
            "pendingIssues": "",
            "nextSteps": ""
        }"#;
        assert!(parse_session_memory_slices(raw).is_none());
    }

    #[test]
    fn resolve_target_for_role_orchestrator_requires_session_model_override() {
        use magi_settings_store::SettingsStore;

        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();

        let resolved = resolve_target_for_role(Some(&store), None, RoleTarget::Orchestrator, None)
            .expect("orchestrator 段构造不应失败");
        assert!(
            resolved.is_none(),
            "全局 orchestrator 只承载连接配置，旧 model/reasoningEffort 不得生成主对话 client"
        );
    }

    #[test]
    fn merge_orchestrator_session_override_applies_model_and_effort() {
        // 全局 base 只提供连接凭据，会话覆盖提供 model + reasoningEffort。
        let mut base = serde_json::json!({
            "baseUrl": "https://api.example.com/v1",
            "apiKey": "sk-orch",
            "model": "global-default-model",
            "urlMode": "standard",
            "apiProtocol": "openai_chat",
            "reasoningEffort": "medium",
        });
        let override_section = serde_json::json!({
            "model": "session-only-model",
            "reasoningEffort": "xhigh",
        });
        merge_orchestrator_session_override(&mut base, &override_section);

        let normalized = NormalizedModelConfig::from_settings_value(&base)
            .expect("合并后的模型配置应符合当前协议");
        assert_eq!(
            normalized.require_model().expect("model 必须存在"),
            "session-only-model",
            "会话覆盖的 model 必须生效"
        );
        assert_eq!(
            normalized.require_base_url().expect("baseUrl 必须存在"),
            "https://api.example.com/v1",
            "凭据仍来自全局 base"
        );
        assert_eq!(
            base.get("reasoningEffort")
                .and_then(serde_json::Value::as_str),
            Some("xhigh"),
            "会话覆盖的 reasoningEffort 必须生效"
        );
    }

    #[test]
    fn merge_orchestrator_session_override_ignores_credentials_and_empty() {
        // 会话覆盖即便误带 baseUrl/apiKey 也不得替换全局连接凭据。
        let mut base = serde_json::json!({
            "baseUrl": "https://api.example.com/v1",
            "apiKey": "sk-orch",
            "model": "global-default-model",
            "urlMode": "standard",
            "apiProtocol": "openai_chat",
        });
        let override_section = serde_json::json!({
            "baseUrl": "https://evil.example.com/v1",
            "apiKey": "sk-evil",
            "model": "session-only-model",
        });
        merge_orchestrator_session_override(&mut base, &override_section);
        assert_eq!(
            base.get("baseUrl").and_then(serde_json::Value::as_str),
            Some("https://api.example.com/v1"),
            "会话覆盖不得替换全局 baseUrl"
        );
        assert_eq!(
            base.get("apiKey").and_then(serde_json::Value::as_str),
            Some("sk-orch"),
            "会话覆盖不得替换全局 apiKey"
        );
        assert_eq!(
            base.get("model").and_then(serde_json::Value::as_str),
            Some("session-only-model"),
        );

        // 空覆盖：base 完全不变。
        let mut base2 = serde_json::json!({ "model": "keep-me" });
        merge_orchestrator_session_override(&mut base2, &serde_json::json!({}));
        assert_eq!(
            base2.get("model").and_then(serde_json::Value::as_str),
            Some("keep-me"),
        );
    }

    #[test]
    fn merge_orchestrator_session_override_null_effort_restores_medium_default() {
        // reasoningEffort 显式为 null 时恢复产品默认值，运行期不允许出现空强度。
        let mut base = serde_json::json!({
            "baseUrl": "https://api.example.com/v1",
            "apiKey": "sk-orch",
            "model": "global-default-model",
            "reasoningEffort": "high",
        });
        merge_orchestrator_session_override(
            &mut base,
            &serde_json::json!({ "reasoningEffort": serde_json::Value::Null }),
        );
        assert_eq!(
            base.get("reasoningEffort")
                .and_then(serde_json::Value::as_str),
            Some("medium"),
            "null 覆盖必须恢复中等 reasoningEffort"
        );
    }

    #[test]
    fn resolve_orchestrator_model_config_defaults_reasoning_effort_to_medium() {
        use magi_settings_store::SettingsStore;

        let store = SettingsStore::new();
        store
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();
        let session_id = SessionId::new("session-default-reasoning-effort");
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                serde_json::json!({ "model": "session-model" }),
            )
            .unwrap();

        let config = resolve_orchestrator_model_config(&store, Some(&session_id))
            .expect("主线模型配置应完成默认值归一化");
        assert_eq!(
            config
                .to_usage_llm_config()
                .and_then(|config| config.reasoning_effort),
            Some(magi_usage_authority::ReasoningEffort::Medium),
        );
    }

    #[test]
    fn resolve_orchestrator_model_config_uses_user_defaults_without_session_override() {
        use magi_settings_store::{ORCHESTRATOR_SESSION_DEFAULTS_SECTION, SettingsStore};

        let store = SettingsStore::new();
        store
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();
        store
            .set_section(
                ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({
                    "model": "model-last-used",
                    "reasoningEffort": "high"
                }),
            )
            .unwrap();
        let session_without_model_override = SessionId::new("session-without-model-override");

        let config =
            resolve_orchestrator_model_config(&store, Some(&session_without_model_override))
                .expect("没有模型覆盖的会话应继承权威的用户默认模型");
        assert_eq!(
            config.require_model().expect("默认模型必须可执行"),
            "model-last-used"
        );
        assert_eq!(
            config
                .to_usage_llm_config()
                .and_then(|config| config.reasoning_effort),
            Some(magi_usage_authority::ReasoningEffort::High),
        );
    }

    #[test]
    fn resolve_target_for_role_orchestrator_threads_session_override() {
        use magi_settings_store::SettingsStore;

        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();
        let session_id = SessionId::new("session-model-scope");
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                serde_json::json!({
                    "model": "session-only-model",
                    "reasoningEffort": "xhigh",
                }),
            )
            .unwrap();

        // 会话级覆盖存在时，主路径必须返回业务模型 client。
        let resolved = resolve_target_for_role(
            Some(&store),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
        )
        .expect("orchestrator 段解析不应失败");
        assert!(
            resolved.is_some(),
            "会话级覆盖存在时必须返回业务模型 client"
        );

        // 不带 session_id 时不能使用全局旧模型字段构造。
        let global = resolve_target_for_role(Some(&store), None, RoleTarget::Orchestrator, None)
            .expect("orchestrator 段解析不应失败");
        assert!(
            global.is_none(),
            "全局 orchestrator base 不得返回隐藏默认模型 client"
        );
    }

    #[test]
    fn build_safety_gate_honors_settings_override_for_builtin_rules() {
        use magi_settings_store::SettingsStore;

        let dispatcher = dispatcher_with_default_tool_surface();
        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "safeguardConfig",
                serde_json::json!({
                    "rules": [
                        {
                            "pattern": "rm -rf",
                            "enabled": false,
                            "category": "bulk_delete",
                            "action": "require_approval_in_restricted"
                        }
                    ]
                }),
            )
            .unwrap();

        let gate = dispatcher
            .build_safety_gate(Some(&store))
            .expect("settings override 后仍应构造 SafetyGate");
        let args = serde_json::json!({ "command": "rm -rf /tmp/demo" }).to_string();

        assert_eq!(
            gate.evaluate("shell_exec", &args),
            magi_safety_gate::SafetyDecision::Allow,
            "用户在设置页禁用内置规则后，运行期必须尊重同一份 settings"
        );
        let force_push =
            serde_json::json!({ "command": "git push --force origin main" }).to_string();
        assert!(
            gate.evaluate("shell_exec", &force_push)
                .is_require_approval(),
            "settings 未覆盖的内置规则仍应自动补齐"
        );
    }

    #[test]
    fn resolve_safeguard_prompt_renders_runtime_rule_actions() {
        use magi_settings_store::SettingsStore;

        let dispatcher = dispatcher_with_default_tool_surface();
        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                "safeguardConfig",
                serde_json::json!({
                    "rules": [
                        {
                            "pattern": "custom hard block",
                            "enabled": true,
                            "category": "custom",
                            "action": "hard_block"
                        },
                        {
                            "pattern": "custom approval command",
                            "enabled": true,
                            "category": "custom",
                            "action": "require_approval_in_restricted"
                        },
                        {
                            "pattern": "custom audit command",
                            "enabled": true,
                            "category": "custom",
                            "action": "audit_only"
                        },
                        {
                            "pattern": "disabled hard block",
                            "enabled": false,
                            "category": "custom",
                            "action": "hard_block"
                        }
                    ]
                }),
            )
            .unwrap();

        let prompt = dispatcher
            .resolve_safeguard_prompt(Some(&store))
            .expect("安全防护基线必须始终注入");

        assert!(
            prompt.contains(
                "- [硬阻断] custom hard block：任何访问模式下都不得执行，也不得请求授权绕过。"
            ),
            "HardBlock 必须明确表达为不可审批绕过的阻断"
        );
        assert!(
            prompt.contains(
                "- [需要授权] custom approval command：受限访问下允许模型发起调用，运行时暂停并创建用户授权请求；批准后继续执行，拒绝后不要重复同一调用；完全访问下按当前授权执行并保留风险说明。"
            ),
            "RequireApprovalInRestricted 必须表达访问模式差异"
        );
        assert!(
            prompt.contains("- [审计] custom audit command：允许执行，但必须如实说明影响。"),
            "AuditOnly 必须表达为审计而非审批"
        );
        assert!(
            !prompt.contains("disabled hard block"),
            "禁用规则不能进入模型可见提示"
        );
        assert!(
            !prompt.contains("如果命中以下危险模式，必须先向用户确认"),
            "提示不能再把所有 SafetyGate 动作统一描述为审批"
        );
    }

    #[test]
    fn resolve_target_for_role_reads_execution_snapshot_not_live_settings() {
        use magi_settings_store::SettingsStore;

        let live_store = Arc::new(SettingsStore::new());
        live_store
            .set_section(
                "orchestrator",
                serde_json::json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-old",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();
        let session_id = SessionId::new("session-snapshot-model");
        live_store
            .set_session_section(
                &session_id,
                "orchestrator",
                serde_json::json!({
                    "model": "model-snapshot",
                }),
            )
            .unwrap();
        let snapshot = Arc::new(live_store.execution_snapshot());

        live_store.remove_section("orchestrator").unwrap();
        live_store
            .remove_session_section(&session_id, "orchestrator")
            .unwrap();

        let snapshot_client = resolve_target_for_role(
            Some(&snapshot),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
        )
        .expect("快照内的 orchestrator 配置应可解析");
        assert!(
            snapshot_client.is_some(),
            "执行快照必须保留任务接受时的会话模型配置"
        );

        let live_client = resolve_target_for_role(
            Some(&live_store),
            None,
            RoleTarget::Orchestrator,
            Some(&session_id),
        )
        .expect("实时 settings 查询不应失败");
        assert!(
            live_client.is_none(),
            "实时 settings 已删除 orchestrator，不应影响既有执行快照"
        );
    }
}
