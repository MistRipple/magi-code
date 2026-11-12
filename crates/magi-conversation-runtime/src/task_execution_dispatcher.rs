//! 任务系统 — conversation dispatcher runtime.
//!
//! Owns the production task dispatch implementation for session turns and conversation loops.

mod extension_services;
mod plugin_commands;
mod plugin_models;

use crate::{
    ConversationRegistry, SKILL_APPLY_TOOL_NAME, active_skill_tool_execution_policy,
    build_skill_custom_tool_definitions,
    context_authority::current_session_file_facts,
    conversation_loop::{self, ConversationLoopRequest},
    execution_admission::ExecutionAdmissionPermit,
    model_config::{
        NormalizedModelConfig, configured_role_engine_model_config,
        resolve_orchestrator_model_config,
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
        RefreshLiveExternalToolDefinitionsInput, constrain_task_tool_definitions,
        refresh_live_external_tool_definitions,
    },
    usage_recording::{
        AuxiliaryModelUsageContext, ModelUsageBinding, invoke_auxiliary_model_with_usage,
        model_usage_binding_for_worker_with_settings,
    },
};
use magi_bridge_client::{ChatToolDefinition, ModelBridgeClient, ModelInvocationRequest};
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
    ExecutionContextSummary, ExecutionWritebackPlans,
    task_store::TaskStore, task_worker_catalog::WorkerInfo,
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
#[cfg(test)]
use magi_bridge_client::{BridgeClientError, BridgeErrorLayer, ModelResponse, ModelStreamingDelta};

#[derive(Clone)]
pub struct ExecutionPipeline {
    pub task_store: Arc<TaskStore>,
    pub memory_store: MemoryStore,
}

/// 在首次本地工具调用前准备会话资源。实现由 daemon 的应用层注入；运行时不复制
/// Git 基线、执行租约或变更账本的所有权。
pub type WorkspaceExecutionPreparer = dyn Fn(
        &SessionId,
        &Option<WorkspaceId>,
        &str,
        Option<&magi_core::ProjectDirectoryBinding>,
        crate::workspace_execution::WorkspacePreparationMode,
    ) -> Result<crate::workspace_execution::WorkspaceExecutionResources, String>
    + Send
    + Sync;

/// 任务标题是目标开头的截断副本（`执行: <目标前缀>`）时返回 true。
fn task_title_is_derived_from_goal(title: &str, goal: &str) -> bool {
    let title = title.trim();
    let stem = title
        .strip_prefix("执行:")
        .or_else(|| title.strip_prefix("执行："))
        .unwrap_or(title)
        .trim()
        .trim_end_matches(['…', '.']);
    !stem.is_empty() && goal.trim_start().starts_with(stem)
}

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
                .is_some_and(|tool| tool.requires_execution_directory()))
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

struct TaskInvocationInput<'a> {
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
    /// 按设置事实源代际复用角色模型客户端。HTTP 连接池由 bridge-client 继续统一持有，
    /// 这里只避免每个 Turn 重复解析配置和构造同一角色包装器。
    model_client_cache: Arc<Mutex<HashMap<String, Arc<dyn ModelBridgeClient>>>>,
    plugin_model_calls: Arc<Mutex<HashMap<String, plugin_models::ModelCall>>>,
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
    extension_hooks: Option<Arc<dyn crate::extension_hooks::ExtensionHooks>>,
    plugin_command_executor: Option<Arc<dyn crate::plugin_command::PluginCommandExecutor>>,
    plugin_session_message_admitter:
        Option<Arc<crate::plugin_command::PluginSessionMessageAdmitter>>,
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
                for context in registry.for_session(self.session_id.as_str()) {
                    let _ = registry.release_agent_worktree_for_lease_for_root(
                        self.session_id.as_str(),
                        &context.execution_root,
                        self.task.task_id.as_str(),
                        self.lease_id.as_str(),
                    );
                }
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

pub const ORCHESTRATOR_THREAD_ID: &str = "orchestrator";

pub fn resolve_target_for_role(
    settings_store: Option<&Arc<SettingsStore>>,
    default_client: Option<Arc<dyn ModelBridgeClient>>,
    target: RoleTarget<'_>,
    session_id: Option<&SessionId>,
) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
    match target {
        RoleTarget::Orchestrator => {
            if let Some(store) = settings_store
                && let Some(client) = build_orchestrator_client(store, session_id)?
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

            let Some(role_model) = configured_role_engine_model_config(store, role_id)? else {
                return Ok(None);
            };
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
    let normalized = NormalizedModelConfig::from_settings_with_catalog(settings_store, &config)
        .map_err(|error| format!("{section} 模型配置无效：{error}"))?;
    Ok(normalized
        .to_http_model_client()
        .map(|client| Arc::new(client) as Arc<dyn ModelBridgeClient>))
}

/// 构造业务主对话（orchestrator）的 client。
///
/// 主对话由数据源目录（`orchestrator.sources`）+ 会话的 `sourceId`/`model` 选择解析；
/// 目录里没有任何数据源即“未配置”，返回 `None` 由宿主注入的默认客户端承接（开发/测试/嵌入场景）；
/// 已配置但选择无效是错误，不静默回退。
fn build_orchestrator_client(
    settings_store: &Arc<SettingsStore>,
    session_id: Option<&SessionId>,
) -> Result<Option<Arc<dyn ModelBridgeClient>>, String> {
    // 用户没有配置任何数据源、会话也没有选择过来源时才视为“未配置”；
    // 一旦选择了来源（含插件来源），就必须按目录解析，来源不可用即报错，不回退到默认客户端。
    if !crate::model_config::has_orchestrator_model_config(settings_store, session_id)? {
        return Ok(None);
    }
    let normalized = resolve_orchestrator_model_config(settings_store, session_id)?;
    let client = normalized
        .to_http_model_client()
        .ok_or_else(|| "主对话模型连接不可用".to_string())?;
    Ok(Some(Arc::new(client)))
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
        let store = self.pipeline.task_store.as_ref();
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
        let root = self
            .execution_registry
            .get(task_id)
            .and_then(|plan| match plan {
                crate::task_execution_registry::TaskExecutionPlan::Dispatch {
                    execution_root,
                    ..
                } => execution_root,
            })
            .or_else(|| self.resolve_task_source_root(&task, session_id, workspace_id));
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
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        mode: crate::workspace_execution::WorkspacePreparationMode,
    ) -> Result<crate::workspace_execution::WorkspaceExecutionResources, String> {
        let Some(prepare) = self.workspace_execution_preparer.as_ref() else {
            return Ok(Default::default());
        };
        let turn_id = self
            .execution_registry
            .turn_id(&task.task_id)
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
        prepare(
            session_id,
            workspace_id,
            &source,
            task.executor_binding
                .as_ref()
                .and_then(|binding| binding.project_directories.as_ref()),
            mode,
        )
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
            self.pipeline.task_store.as_ref(),
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
        if let Some(binding) = task
            .executor_binding
            .as_ref()
            .and_then(|executor| executor.project_directories.as_ref())
        {
            if workspace_id.as_ref() != Some(&binding.workspace_id) {
                return Err("Task 项目目录绑定与执行归属不匹配".into());
            }
            if let Some(registry) = self.workspace_registry.as_ref() {
                registry
                    .validate_directory_binding(binding)
                    .map_err(|error| error.to_string())?;
            }
        }
        // 仓库准备可能等另一个任务释放资源，不能同时占住它继续执行所需的模型名额。
        let waiting = self
            .execution_registry
            .execution_admission()
            .suspend_while_waiting(&task.task_id);
        let prepared = (|| -> Result<conversation_loop::PreparedWorkspaceExecution, String> {
            let identity_root = self.resolve_task_source_root(task, session_id, workspace_id);
            let transferred = self
                .pipeline
                .task_store.as_ref()
                .agent_run_transfer_for_task(&task.task_id)
                .map_err(|error| error.to_string())?
                .is_some();
            let mut resources = if transferred {
                // 转交根的独立副本已在交接前准备；嵌套任务从直接父副本出发。
                // 主线可能已结束或换到下一轮，不能再准备/改动它的工作区状态。
                self.prepare_workspace_execution(
                    task,
                    session_id,
                    workspace_id,
                    crate::workspace_execution::WorkspacePreparationMode::Mapping,
                )?
            } else {
                self.prepare_workspace_execution(
                    task,
                    session_id,
                    workspace_id,
                    crate::workspace_execution::WorkspacePreparationMode::Execution,
                )?
            };
            let main_root = resources.default_root()?;
            if !execution_is_active() {
                return Err("准备执行资源期间任务或父 Turn 已停止".into());
            }
            let root = if let Some(binding) = task
                .executor_binding
                .as_ref()
                .and_then(|executor| executor.project_directories.as_ref())
            {
                let original = resources
                    .directories
                    .as_ref()
                    .ok_or("项目资源准备缺少完整目录映射")?;
                original
                    .validate_for_binding(binding)
                    .map_err(|error| error.to_string())?;
                let projected =
                    magi_core::ProjectDirectoryExecutionMap::from_binding(binding, |grant| {
                        let parent_root = original
                            .root(Some(&grant.source_folder_id))?
                            .execution_root()?;
                        self.resolve_task_execution_root(
                            task,
                            lease_id,
                            session_id,
                            workspace_id,
                            Some(&parent_root),
                            is_sidechain,
                            worker_id,
                        )
                        .map_err(|message| magi_core::DomainError::Validation { message })?
                        .ok_or_else(|| magi_core::DomainError::Validation {
                            message: "项目目录缺少实际执行根".into(),
                        })
                        .and_then(|root| {
                            std::fs::canonicalize(root).map_err(|error| {
                                magi_core::DomainError::Validation {
                                    message: format!("执行目录不可用: {error}"),
                                }
                            })
                        })
                    })
                    .map_err(|error| error.to_string())?;
                let root = projected
                    .root(None)
                    .and_then(|root| root.execution_root())
                    .map_err(|error| error.to_string())?;
                resources.snapshots.retain(|snapshot| {
                    projected.roots.iter().any(|root| {
                        root.execution_root().is_ok_and(|root| {
                            std::fs::canonicalize(snapshot.workspace_root())
                                .is_ok_and(|snapshot| root == snapshot)
                        })
                    })
                });
                resources.directories = Some(projected);
                Some(root)
            } else {
                let root = self.resolve_task_execution_root(
                    task,
                    lease_id,
                    session_id,
                    workspace_id,
                    main_root.as_ref().or(execution_root),
                    is_sidechain,
                    worker_id,
                )?;
                resources
                    .snapshots
                    .retain(|snapshot| root.as_deref() == Some(snapshot.workspace_root()));
                root
            };
            if !execution_is_active() {
                return Err("准备执行资源期间任务租约已失效".into());
            }
            Ok(conversation_loop::PreparedWorkspaceExecution {
                task: if resources.directories.is_some() {
                    task.clone()
                } else {
                    Self::task_for_execution_root(task, identity_root.as_ref(), root.as_ref())
                },
                // Git 子代理的变更属于代理分支；不能让其相对路径写入主会话账本。
                resources,
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
            plugin_command_executor: None,
            plugin_session_message_admitter: None,
            extension_hooks: None,
            plugin_model_calls: Arc::default(),
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
        let mut runtime = self.execution_registry.agent_spawn_preflight_runtime();
        runtime.workspace_registry = Some(registry.clone());
        self.execution_registry = self
            .execution_registry
            .clone()
            .with_agent_spawn_preflight_runtime(runtime);
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

    /// 接纳插件业务结果，复用 API 的原生 Input 提交端口。
    pub fn with_plugin_session_message_admitter(
        mut self,
        admitter: Arc<crate::plugin_command::PluginSessionMessageAdmitter>,
    ) -> Self {
        self.plugin_session_message_admitter = Some(admitter);
        self
    }

    pub fn with_plugin_command_executor(
        mut self,
        executor: Arc<dyn crate::plugin_command::PluginCommandExecutor>,
    ) -> Self {
        self.plugin_command_executor = Some(executor);
        self
    }

    pub fn with_extension_hooks(
        mut self,
        hooks: Arc<dyn crate::extension_hooks::ExtensionHooks>,
    ) -> Self {
        self.extension_hooks = Some(hooks);
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
        let invocation = TaskInvocationInput {
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
        };
        let (mut outcome, context_summary) = match self.execute_plugin_task_command(&invocation) {
            Some(outcome) => (outcome, None),
            None => self.invoke_llm_with_tools(invocation),
        };
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
            refresh_live_external_tool_definitions(RefreshLiveExternalToolDefinitionsInput {
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
                loaded_tool_names: &std::collections::BTreeSet::new(),
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
                        // 持久化配置类工具只在声明它的 Skill 激活时进入模型工具面。
                        && (!tool.requires_skill_declaration()
                            || active_skill_allowed_tools.is_some_and(|allowed| {
                                allowed.iter().any(|name| name == tool.as_str())
                            }))
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
        if self.skill_runtime.is_some() {
            let mut definition = skill_apply_tool_definition(
                self.skill_runtime
                    .as_deref()
                    .expect("skill runtime checked above"),
            );
            if skill_name.is_some() {
                definition.function.parameters["properties"]["mode"] =
                    serde_json::json!({"type":"string","enum":["reference"]});
                definition.function.parameters["required"] =
                    serde_json::json!(["skill_name", "mode"]);
            }
            definitions.push(definition);
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
    fn resolve_task_source_root(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        if let Some(binding) = task
            .executor_binding
            .as_ref()
            .and_then(|executor| executor.project_directories.as_ref())
        {
            return binding
                .grant(None)
                .ok()
                .and_then(|grant| magi_core::HostPath::from_path_ref(&grant.source_path_ref).ok())
                .map(magi_core::HostPath::into_path_buf);
        }
        workspace_id
            .as_ref()
            .and_then(|_| self.resolve_workspace_root_path(session_id, workspace_id))
    }

    fn resolve_workspace_root_path(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        let source_root = workspace_id.as_ref().and_then(|workspace_id| {
            self.workspace_registry
                .as_ref()?
                .resolve_source_folder(workspace_id, None)
                .ok()
                .map(|(_, root)| root)
        });
        if workspace_id.is_some() && self.workspace_registry.is_some() && source_root.is_none() {
            return None;
        }
        if let Some(context) =
            self.session_code_contexts
                .as_ref()
                .and_then(|registry| match source_root.as_deref() {
                    Some(root) => registry.get_for_path(session_id.as_str(), root),
                    None => registry.get(session_id.as_str()),
                })
        {
            return Some(context.execution_root);
        }
        source_root
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
            .or_else(|| self.resolve_task_source_root(task, session_id, workspace_id));
        if workspace_id.is_some() && main_root.is_none() {
            return Err("项目执行缺少明确源文件夹绑定，不能选择任意仓库".into());
        }
        if !is_sidechain {
            return Ok(main_root);
        }
        // 个人会话没有项目 Git 注册表；子代理和显式转交沿原执行计划继承目录。
        if workspace_id.is_none() && main_root.is_some() {
            return Ok(main_root);
        }
        let context = self.session_code_contexts.as_ref().and_then(|registry| {
            main_root
                .as_deref()
                .and_then(|root| registry.get_for_path(session_id.as_str(), root))
        });
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
                let store = self.pipeline.task_store.as_ref();
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
                            .task_store.as_ref()
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
                    .reattach_agent_worktree_for_root(
                        session_id.as_str(),
                        &context.execution_root,
                        previous,
                        lease_id.as_str(),
                        worker.as_str(),
                    )
                    .map_err(|error| format!("恢复代理 worktree 记录失败: {error}"))?;
                if let Some(persist) = self.session_state_persist_callback.as_deref()
                    && let Err(error) = persist("agent_worktree_reattached")
                {
                    let _ = registry.release_agent_worktree_for_lease_for_root(
                        session_id.as_str(),
                        &context.execution_root,
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
        use sha2::{Digest, Sha256};
        let repository_key = magi_core::HostPath::from_path(context.execution_root.clone());
        let repository_key = magi_core::hex_lower(Sha256::digest(
            repository_key.to_path_ref().as_str().as_bytes(),
        ));
        let path = self
            .agent_worktree_root
            .join(workspace_key)
            .join(session_key)
            .join(&repository_key)
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
        let branch = (mode == magi_git::AgentWorktreeMode::Writable).then(|| {
            format!(
                "magi/agent/{task_key}-{}-{allocation_nonce}",
                &repository_key[..12]
            )
        });
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
            let store = self.pipeline.task_store.as_ref();
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
            .register_agent_worktree_for_root(
                session_id.as_str(),
                &context.execution_root,
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
        let registry = self.session_code_contexts.as_ref()?;
        let contexts = registry.for_session(session_id.as_str());
        let mut outputs = Vec::new();
        for context in contexts {
            if let Some(output) = self.finalize_agent_worktree_for_context(
                task,
                lease_id,
                session_id,
                workspace_id,
                &context,
            ) {
                outputs.push(output);
            }
        }
        match outputs.len() {
            0 => None,
            1 => outputs.pop(),
            _ => Some(serde_json::json!({
                "agent_workspaces": outputs.into_iter().map(|output| output["agent_workspace"].clone()).collect::<Vec<_>>()
            })),
        }
    }

    fn finalize_agent_worktree_for_context(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        context: &magi_git::SessionCodeContext,
    ) -> Option<serde_json::Value> {
        let registry = self.session_code_contexts.as_ref()?;
        let git_service = self.git_service.as_ref()?;

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
            match self.commit_agent_worktree_output(
                task,
                lease_id,
                session_id,
                &context.execution_root,
                &allocation,
            ) {
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
                            .record_agent_worktree_result_for_root(
                                session_id.as_str(),
                                &context.execution_root,
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

        if let Err(error) = registry.release_agent_worktree_for_lease_for_root(
            session_id.as_str(),
            &context.execution_root,
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
        repository_root: &Path,
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
            .record_agent_worktree_result_for_root(
                session_id.as_str(),
                repository_root,
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
                "repository_root": repository_root,
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
                self.pipeline.task_store.as_ref(), &self.session_store, &session_id, task,
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
                .and_then(|id| self.pipeline.task_store.as_ref().get_task(id))
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

        let task_store = self.pipeline.task_store.as_ref();
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

    /// `main_thread` 为真时，模型请求本来就带着完整的会话线程历史：近期回合和文件读取摘要
    /// 只是历史的投影副本，每轮重复放在请求尾部会多发数千 token 且永远命不中缓存，因此不再装配。
    fn assemble_prompt(
        &self,
        _settings_store: Option<&Arc<SettingsStore>>,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        main_thread: bool,
    ) -> (
        String,
        Vec<magi_bridge_client::ChatMessage>,
        Option<ExecutionContextSummary>,
    ) {
        let base_prompt = if task.goal.is_empty() {
            task.title.clone()
        } else if task_title_is_derived_from_goal(&task.title, &task.goal) {
            // 用户回合的任务标题只是目标的截断副本；把它再拼一遍会让模型看到的输入与
            // thread 历史里的原始用户消息不同，回合边界重建后缓存前缀在此处失效。
            task.goal.clone()
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
            source_folder_id: task
                .executor_binding
                .as_ref()
                .and_then(|binding| binding.project_directories.as_ref())
                .map(|directories| directories.default_source_folder_id.clone()),
            execution_root: (task.kind != TaskKind::LocalAgent)
                .then(|| {
                    self.prepare_workspace_execution(
                        task,
                        session_id,
                        workspace_id,
                        crate::workspace_execution::WorkspacePreparationMode::Mapping,
                    )
                    .ok()
                    .and_then(|resources| resources.directories)
                    .and_then(|map| {
                        map.root(None)
                            .ok()
                            .map(|root| root.execution_root_ref.clone())
                    })
                })
                .flatten(),
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
        if main_thread {
            context_budget.max_turns = 0;
            context_budget.max_file_summaries = 0;
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
        let Some(settings_store) = settings_store else {
            return resolve_target_for_role(
                settings_store,
                self.model_bridge_client.clone(),
                target,
                session_id,
            );
        };
        let target_label = match target {
            RoleTarget::Orchestrator => "orchestrator".to_string(),
            RoleTarget::Auxiliary => "auxiliary".to_string(),
            RoleTarget::Agent { role_id } => format!("agent:{role_id}"),
        };
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
        let resolved = resolve_target_for_role(
            Some(settings_store),
            self.model_bridge_client.clone(),
            target,
            session_id,
        )?;
        if let Some(client) = resolved.as_ref() {
            self.model_client_cache
                .lock()
                .expect("model client cache lock poisoned")
                .insert(cache_key, client.clone());
        }
        Ok(resolved)
    }

    /// 扩展调用的模型必须与 `resolve_model_client_for_task` 选中的角色一致，并拒绝插件主对话数据源。
    pub(crate) fn ensure_extension_model_source(
        &self,
        task: Option<&magi_core::Task>,
        execution_role_id: Option<&str>,
        session_id: &SessionId,
    ) -> Result<(), String> {
        let Some(store) = self.settings_store.as_ref() else {
            return Ok(());
        };
        if task
            .and_then(|task| task.executor_binding.as_ref())
            .and_then(|binding| binding.model_selection.as_ref())
            .is_some()
        {
            return Ok(());
        }
        let role_id = execution_role_id
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .or_else(|| task.and_then(|task| task_role_id(Some(task))));
        crate::model_config::ensure_extension_model_source(store, role_id, session_id)
    }

    fn resolve_model_client_for_task(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        task: Option<&magi_core::Task>,
        execution_role_id: Option<&str>,
        session_id: Option<&SessionId>,
    ) -> Result<Arc<dyn ModelBridgeClient>, String> {
        if let Some(selection) = task
            .and_then(|task| task.executor_binding.as_ref())
            .and_then(|binding| binding.model_selection.as_ref())
        {
            let store = settings_store.ok_or_else(|| "恢复子代理缺少模型连接配置".to_string())?;
            let config = crate::model_config::frozen_agent_model_config(store, selection)?;
            return config
                .to_http_model_client()
                .map(|client| Arc::new(client) as Arc<dyn ModelBridgeClient>)
                .ok_or_else(|| "子代理原模型连接不可用".to_string());
        }
        let role_id = execution_role_id
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .or_else(|| task.and_then(|task| task_role_id(Some(task))));

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
        if let Some(binding) = request.project_directories.as_ref()
            && let Some(registry) = self.workspace_registry.as_ref()
        {
            let valid = request.workspace_id.as_ref() == Some(&binding.workspace_id);
            if !valid || registry.validate_directory_membership(binding).is_err() {
                return Err(SessionTurnExecutionError {
                    reason:
                        crate::session_turn_execution::SessionTurnFailureReason::RuntimeInvalidState,
                    diagnostic_code: "project_directory_binding_invalid".into(),
                    public_message: "原输入的项目源文件夹授权已失效".into(),
                    model_failure: None,
                });
            }
        }
        let plan_store =
            magi_plan::PlanStore::new(self.session_store.clone(), request.session_id.clone());
        let execution_settings_snapshot = self.execution_settings_snapshot();
        let execution_settings =
            self.execution_settings_or_live(execution_settings_snapshot.as_ref());
        let client = match self.resolve_model_client_for_task(
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
        };

        let prompt = request.prompt.clone();
        let safety_gate = self.build_safety_gate(execution_settings);
        let knowledge_context_prompt = self.context_runtime.as_ref().and_then(|runtime| {
            let selection = runtime.select_knowledge_on_demand(KnowledgeContextRequest {
                consumer: KnowledgeConsumer::Mainline,
                workspace_id: request.workspace_id.clone(),
                source_folder_id: request
                    .project_directories
                    .as_ref()
                    .map(|binding| binding.default_source_folder_id.clone()),
                execution_root: self
                    .workspace_execution_preparer
                    .as_ref()
                    .and_then(|prepare| {
                        prepare(
                            &request.session_id,
                            &request.workspace_id,
                            &request.turn_id,
                            request.project_directories.as_ref(),
                            crate::workspace_execution::WorkspacePreparationMode::Mapping,
                        )
                        .ok()
                    })
                    .and_then(|resources| resources.directories)
                    .and_then(|map| {
                        map.root(None)
                            .ok()
                            .map(|root| root.execution_root_ref.clone())
                    }),
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
            extension_hooks: self.extension_hooks.as_deref(),
        })
    }

    fn invoke_llm_with_tools(
        &self,
        input: TaskInvocationInput<'_>,
    ) -> (TaskOutcome, Option<ExecutionContextSummary>) {
        let TaskInvocationInput {
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
        if use_tools
            && let Some(binding) = task
                .executor_binding
                .as_ref()
                .and_then(|executor| executor.project_directories.as_ref())
            && let Some(registry) = self.workspace_registry.as_ref()
            && let Err(error) = registry.validate_directory_binding(binding)
        {
            return (
                TaskOutcome::Failed {
                    error: format!("项目源文件夹授权已失效，工具执行未开始：{error:?}"),
                },
                None,
            );
        }
        let (prompt, task_context_messages, context_summary) = self.assemble_prompt(
            execution_settings,
            task,
            session_id,
            workspace_id,
            !is_sidechain,
        );
        let developer_prompt = compose_developer_instructions(
            system_prompt.as_deref(),
            self.resolve_user_rules_prompt(execution_settings)
                .as_deref(),
            self.resolve_safeguard_prompt(execution_settings).as_deref(),
        );
        let workspace_identity_root_path =
            self.resolve_task_source_root(task, session_id, workspace_id);
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
        let project_memory = workspace_id.as_ref().and_then(|workspace_id| {
            match self.project_memory_registry.get_or_open(workspace_id) {
                Ok(store) => Some(store),
                Err(err) => {
                    tracing::warn!(error = %err, %workspace_id, "ProjectMemory: 打开失败，本次 Turn 不注入项目记忆");
                    None
                }
            }
        });
        let workspace_resources = match prepared {
            Some(prepared) => Some(prepared.resources),
            None if use_tools
                && task
                    .executor_binding
                    .as_ref()
                    .is_some_and(|executor| executor.project_directories.is_some()) =>
            {
                match self.prepare_workspace_execution(
                    task,
                    session_id,
                    workspace_id,
                    crate::workspace_execution::WorkspacePreparationMode::Mapping,
                ) {
                    Ok(resources) => Some(resources),
                    Err(error) => return (TaskOutcome::Failed { error }, None),
                }
            }
            None => self.snapshot_manager.as_ref().and_then(|manager| {
                workspace_root_path
                    .as_ref()
                    .and_then(|root| manager.get_session_for_workspace(session_id.as_str(), root))
                    .map(crate::workspace_execution::WorkspaceExecutionResources::from_snapshot)
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
            extension_hooks: self.extension_hooks.as_deref(),
            skill_name: skill_name.clone(),
            task_store: self.pipeline.task_store.as_ref(),
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
            workspace_resources,
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
                    )
                    .with_task_model(
                        task,
                        self.execution_settings_or_live(execution_settings_snapshot.as_ref()),
                    )?,
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
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt,
        messages: None,
        tools: None,
        tool_choice: None,
    };
    let call_id =
        magi_core::EventId::unique(format!("auxiliary-knowledge-extraction-{session_id}"))
            .to_string();
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
            purpose: crate::usage_recording::AuxiliaryModelPurpose::KnowledgeExtraction,
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
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt,
        messages: None,
        tools: None,
        tool_choice: None,
    };
    let call_id =
        magi_core::EventId::unique(format!("auxiliary-session-memory-{session_id}")).to_string();
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
            purpose: crate::usage_recording::AuxiliaryModelPurpose::SessionMemoryExtraction,
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
        let store = self.pipeline.task_store.as_ref();
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
            .task_store.as_ref()
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
mod tests;
