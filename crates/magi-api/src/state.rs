use crate::dto::{
    AuditUsageLedgerDto, BootstrapDto, DaemonIdentity, HealthDto, RuntimeReadModelDto, ServiceInfo,
    SessionTurnRequestDto, SessionTurnRouteDto, VersionHandshakeDto,
    runtime_read_model_dto_with_usage,
};
use crate::errors::ApiError;
use crate::mcp_config::{
    build_mcp_config_from_entry, mcp_server_entry_enabled, mcp_server_entry_id,
    normalize_mcp_server_snapshot_entry,
};
use crate::model_config::{
    registered_role_template_ids, resolve_registry_agents, role_templates_for_registry,
};
use crate::scope_binding::strip_scope_binding_fields;
use crate::skill_loader;
use magi_bridge_client::{McpServerClient, ModelBridgeClient};
use magi_browser_authority::{
    BrowserAuthority, BrowserAuthorityError, BrowserCapabilitySnapshot, BrowserControlLease,
    BrowserDurableState, BrowserHostClient, BrowserHostCommand, BrowserHostCommandOutcome,
    BrowserHostControlUpdate, BrowserHostStatus, BrowserLeaseEndReason, BrowserLeaseSelector,
    BrowserProfile, BrowserProfileKind, BrowserSessionLifecycle, BrowserSurfaceControlSnapshot,
};
use magi_conversation_runtime::{
    CanonicalTurnEventSink, ConversationRegistry, CoordinatorCommandResult, SessionTurnCoordinator,
    TaskCompletionNotifier,
    execution_admission::{ExecutionAdmissionController, ExecutionAdmissionSnapshot},
    task_execution_dispatcher::{ExecutionPipeline, LlmTaskDispatcher},
    task_execution_registry::TaskExecutionRegistry,
    task_runner::TaskRunner,
    task_runner_bridge::{RunCycleOutcome, TaskDispatchGate, TaskDispatcher},
};
use magi_core::{
    BrowserProfileId, BrowserTabId, DomainError, DomainResult, SessionId, SessionLifecycleStatus,
    TaskId, UtcMillis, WorkspaceId, public_runtime_excerpt,
};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, latest_usage_observations_from_ledger,
};
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_memory_store::MemoryStore;
use magi_orchestrator::{
    task_store::{TaskStore, TaskStoreSnapshot},
    task_worker_catalog::{WorkerInfo, build_worker_catalog_for_roles},
};
use magi_process::ManagedProcessGroup;
use magi_session_store::{
    NotificationContext, SessionDurableState, SessionExecutionSidecarStoreState, SessionRecord,
    SessionStore,
};
use magi_settings_store::SettingsStore;
use magi_snapshot::{BaselinePatchEntry, SnapshotManager, SnapshotSession};
use magi_tool_runtime::{
    RuntimeCapabilityDependencyEntry, RuntimeCapabilityDependencyProvider, ToolExecutionContext,
    ToolExecutionContextQuery, ToolRegistry,
};
use magi_workspace::WorkspaceStore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::{OwnedMutexGuard, watch};

static RUNTIME_EPOCH_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct AppServerRequestLockEntry {
    lock: Arc<tokio::sync::Mutex<()>>,
    references: usize,
}

/// 以显式业务 requestId 为粒度串行化 App Server Turn 的“查找并提交”事务。
///
/// 该锁只服务 App Server，不改变 HTTP 路由的并发模型。引用计数让空闲 requestId
/// 不会永久留在注册表中，同时在有等待者时保证后续调用仍复用同一把锁。
pub(crate) struct AppServerRequestLockGuard {
    registry: Arc<Mutex<HashMap<String, AppServerRequestLockEntry>>>,
    request_id: String,
    _guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for AppServerRequestLockGuard {
    fn drop(&mut self) {
        // 先释放底层锁，再删除注册表条目。否则新请求可能在条目删除和
        // OwnedMutexGuard 自动析构之间创建第二把锁，破坏 requestId 串行性。
        self._guard.take();
        let mut registry = self
            .registry
            .lock()
            .expect("app server request lock registry poisoned");
        let Some(entry) = registry.get_mut(&self.request_id) else {
            return;
        };
        if entry.references <= 1 {
            registry.remove(&self.request_id);
        } else {
            entry.references -= 1;
        }
    }
}

/// Tracks the state of a single running Runner instance.
pub struct RunnerHandle {
    /// Whether the runner has been signalled to stop.
    pub cancel: Arc<AtomicBool>,
    /// 后台 runner 循环是否仍未退出，用于避免已中断任务被重复启动。
    pub active: Arc<AtomicBool>,
    /// Number of cycles executed so far.
    pub cycle_count: Arc<AtomicU64>,
    /// 当前 runner 展示状态："running"、"killed"、"completed"、"error"。
    pub status: Arc<Mutex<String>>,
    /// Last error message, if any.
    pub last_error: Arc<Mutex<Option<String>>>,
    /// 后台循环的 join handle。会话删除必须等待循环退出后才能清理 TaskStore，
    /// 防止 in-flight runner 在删除完成后反写已回收任务。
    join_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub type TaskCheckpointPersist = Arc<dyn Fn(&TaskStoreSnapshot) -> DomainResult<()> + Send + Sync>;
pub type CanonicalEventNextSequenceProvider =
    Arc<dyn Fn(&SessionId) -> Result<u64, String> + Send + Sync>;
/// 返回值表示回调是否已经把 durable session projection 一并写入。
pub type SessionStateCheckpointPersist = Arc<dyn Fn(&str) -> Result<bool, ApiError> + Send + Sync>;
pub type WorkspaceHistoryRestore =
    Arc<dyn Fn(&magi_workspace::WorkspaceRecord) -> Result<(), ApiError> + Send + Sync>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionProjectionPersistMode {
    Full,
    Navigation {
        target_session_id: Option<SessionId>,
    },
    /// 只有这些会话的内容发生了变化（新建 / 重命名 / 删除后的替代会话 / 通知 / 续跑……）：
    /// 只重新序列化并提交它们，其余会话沿用已落盘的版本；不再保留的会话由持久化层按会话列表清理。
    Sessions {
        session_ids: Vec<SessionId>,
    },
}

/// 删除一个启动时无法加载的会话的持久化文件；由 daemon 的状态仓库实现。
pub type UnavailableSessionDiscard = Arc<
    dyn Fn(&SessionId) -> Result<magi_session_store::UnavailableSession, ApiError> + Send + Sync,
>;

pub type SessionProjectionPersist = Arc<
    dyn Fn(
            &SessionDurableState,
            &SessionExecutionSidecarStoreState,
            SessionProjectionPersistMode,
        ) -> Result<(), ApiError>
        + Send
        + Sync,
>;

fn snapshot_baseline_patch(
    entries: Vec<magi_git::GitTreeBaselineEntry>,
) -> Vec<BaselinePatchEntry> {
    entries
        .into_iter()
        .map(|entry| match entry {
            magi_git::GitTreeBaselineEntry::Deleted { path } => {
                BaselinePatchEntry::Deleted { path }
            }
            magi_git::GitTreeBaselineEntry::RegularFile { path, content } => {
                BaselinePatchEntry::RegularFile { path, content }
            }
            magi_git::GitTreeBaselineEntry::Symlink { path, target } => {
                BaselinePatchEntry::Symlink { path, target }
            }
            magi_git::GitTreeBaselineEntry::Gitlink { path, object_id } => {
                BaselinePatchEntry::Gitlink { path, object_id }
            }
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct QueuedRegularSessionTurn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_message: Option<magi_app_server_protocol::generated::PluginSessionMessageOrigin>,
    pub request: SessionTurnRequestDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_directories: Option<magi_core::ProjectDirectoryBinding>,
    /// 用户提交请求的完整幂等指纹。服务端补齐 requestId/userMessageId 前计算，
    /// 重启恢复后只允许同一份原始请求重放，不再依赖被 normalize 的 request 做猜测。
    #[serde(default)]
    pub request_fingerprint: Option<String>,
    /// 请求所属会话作用域。`None` 明确表示个人会话，不能在出队时补成当前项目。
    pub requested_workspace_id: Option<WorkspaceId>,
    pub accepted_at: UtcMillis,
    pub route: SessionTurnRouteDto,
    pub task_title: Option<String>,
    pub execution_goal: Option<String>,
    pub required_tool_chain: Vec<String>,
    #[serde(default)]
    pub completion_contract: magi_core::TaskCompletionContract,
    #[serde(default)]
    pub recovery_checkpoint: Option<magi_core::TaskRecoveryCheckpoint>,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub queue_id: String,
    #[serde(default)]
    pub retry_count: u8,
}

impl QueuedRegularSessionTurn {
    fn requires_execution(&self) -> bool {
        !self.plugin_message.as_ref().is_some_and(|origin| {
            origin.delivery
                == magi_app_server_protocol::generated::PluginSessionMessageDelivery::Context
        })
    }
}

/// 为一个用户发起的 session turn 生成并规范化唯一请求身份。
///
/// 入口允许客户端省略这些字段，但一旦进入 accepted、queued 或 preparing，
/// 两个用户身份必须同时存在且不能再因为空白差异改变。`identity_seed` 必须来自
/// 当前提交事实（例如 acceptedAt 或 queueId），这样排队持久化和 daemon 重启
/// 恢复可以继续使用同一对身份；目标自动续跑不经过此函数，因为它没有用户消息。
pub(crate) fn normalize_session_turn_identity(
    request_id: Option<String>,
    user_message_id: Option<String>,
    identity_seed: &str,
) -> (String, String) {
    let identity_seed = identity_seed.trim();
    debug_assert!(!identity_seed.is_empty());
    let request_id = request_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("request-{identity_seed}"));
    let user_message_id = user_message_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("turn-item-user-{identity_seed}"));
    (request_id, user_message_id)
}

impl QueuedRegularSessionTurn {
    fn normalize_identity(&mut self) {
        let (request_id, user_message_id) = normalize_session_turn_identity(
            self.request.request_id.take(),
            self.request.user_message_id.take(),
            &self.queue_id,
        );
        self.request.request_id = Some(request_id);
        self.request.user_message_id = Some(user_message_id);
    }
}

pub(crate) fn session_has_user_content(session: &SessionRecord) -> bool {
    session.message_count.unwrap_or(0) > 0
}

/// 会话执行生命周期提交的统一锁顺序。
///
/// Session Turn 和 Runner 属于一个会话的执行资源，必须先取得 per-session Turn 锁，
/// 再取得 Runner 生命周期锁。当前会话指针是独立的导航事实，不属于执行生命周期；
/// 因此执行准备、Runner 启动、模型执行和终态收口都不能持有全局导航锁。
pub(crate) struct SessionLifecycleCommitGuard {
    _session_turn_guard: tokio::sync::OwnedMutexGuard<()>,
    _runner_lifecycle_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

/// 会话 Turn 提交锁只保护指定会话的 Turn 状态，不串行化无关会话的导航。
///
/// 导航指针由 `lock_session_navigation` 单独保护；只有创建新会话或提交当前指针
/// 的路径才需要显式取得那把全局锁。这样一个会话的后台执行不会阻塞另一个会话切换。
pub(crate) struct SessionTurnCommitGuard {
    _session_turn_guard: tokio::sync::OwnedMutexGuard<()>,
}

/// Manages active Runner instances keyed by root_task_id.
#[derive(Clone)]
pub struct RunnerManager {
    runners: Arc<Mutex<HashMap<String, Arc<RunnerHandle>>>>,
    restart_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    session_lifecycle_locks: Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>,
    task_store: Arc<TaskStore>,
    session_store: Arc<SessionStore>,
    worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
    agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
    dispatcher: Option<Arc<dyn TaskDispatcher>>,
    dispatch_gate: Option<Arc<TaskDispatchGate>>,
    execution_admission: Arc<ExecutionAdmissionController>,
    /// 任务快照提交回调。daemon 在这里把 manifest 提交与 accepted WAL 收敛绑定为
    /// 同一个持久化事务，Runner 不允许绕过该入口直接写 projection。
    checkpoint_persist: Option<TaskCheckpointPersist>,
    /// Maps a session to the root task IDs whose runners should be killed
    /// when the session is closed (design 1.5: Session-Runner linkage).
    session_runner_index: Arc<Mutex<HashMap<SessionId, Vec<String>>>>,
}

/// Number of runner cycles between periodic checkpoints.
const CHECKPOINT_INTERVAL_CYCLES: u64 = 5;

fn task_checkpoint_is_already_durable(task_store: &TaskStore, root_task_id: &TaskId) -> bool {
    task_store.has_checkpoint_callback()
        && task_store
            .get_task(root_task_id)
            .and_then(|task| task.policy_snapshot)
            .is_some_and(|policy| {
                matches!(policy.checkpoint_mode.as_str(), "turn" | "task_or_phase")
            })
}

fn fail_runner_for_checkpoint_error(
    handle: &RunnerHandle,
    active: &AtomicBool,
    error: &DomainError,
) {
    let message = format!("任务 checkpoint 持久化失败: {error}");
    *handle.status.lock().expect("status lock should hold") = "error".to_string();
    *handle
        .last_error
        .lock()
        .expect("last_error lock should hold") = Some(message);
    active.store(false, Ordering::Relaxed);
}

impl RunnerManager {
    pub fn with_dispatcher_and_worker_catalog(
        task_store: Arc<TaskStore>,
        session_store: Arc<SessionStore>,
        worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
        dispatcher: Arc<dyn TaskDispatcher>,
    ) -> Self {
        Self {
            runners: Arc::new(Mutex::new(HashMap::new())),
            restart_locks: Arc::new(Mutex::new(HashMap::new())),
            session_lifecycle_locks: Arc::new(Mutex::new(HashMap::new())),
            task_store,
            session_store,
            worker_catalog,
            agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            dispatcher: Some(dispatcher),
            dispatch_gate: None,
            execution_admission: Arc::new(ExecutionAdmissionController::default()),
            checkpoint_persist: None,
            session_runner_index: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_dispatch_gate(mut self, gate: Arc<TaskDispatchGate>) -> Self {
        self.dispatch_gate = Some(gate);
        self
    }

    pub fn with_agent_role_registry(
        mut self,
        registry: Arc<magi_agent_role::AgentRoleRegistry>,
    ) -> Self {
        self.agent_role_registry = registry;
        self
    }

    pub fn with_execution_admission(
        mut self,
        execution_admission: Arc<ExecutionAdmissionController>,
    ) -> Self {
        self.execution_admission = execution_admission;
        self
    }

    pub fn execution_admission_snapshot(&self) -> ExecutionAdmissionSnapshot {
        self.execution_admission.snapshot()
    }

    fn build_task_runner(&self, session_id: Option<SessionId>) -> TaskRunner {
        let dispatcher = self
            .dispatcher
            .as_ref()
            .expect("RunnerManager 缺少 LLM dispatcher");
        let mut runner = TaskRunner::with_dispatcher_and_worker_catalog(
            Arc::clone(&self.task_store),
            Arc::clone(&self.worker_catalog),
            Arc::clone(dispatcher),
        );
        runner = runner.with_agent_role_registry((*self.agent_role_registry).clone());
        runner = runner.with_execution_admission(Arc::clone(&self.execution_admission), session_id);
        if let Some(gate) = &self.dispatch_gate {
            runner = runner.with_dispatch_gate(Arc::clone(gate));
        }
        runner
    }

    /// 设置 Runner 周期性 checkpoint 的唯一提交入口。
    pub fn with_checkpoint_persist(mut self, persist: TaskCheckpointPersist) -> Self {
        self.checkpoint_persist = Some(persist);
        self
    }

    /// 串行化指定 session 与 root task 生命周期后启动 runner。
    pub async fn start(
        &self,
        root_task_id: &str,
        session_id: Option<SessionId>,
    ) -> Result<Arc<RunnerHandle>, RunnerStartError> {
        let _session_guard = match session_id.as_ref() {
            Some(session_id) => Some(self.lock_session_lifecycle(session_id).await),
            None => None,
        };
        let _restart_guard = self.lock_for_restart(root_task_id).await;
        self.start_after_quiesce(root_task_id, session_id)
    }

    /// 调用方已持有 session 生命周期锁与 root restart 锁时启动 runner。
    pub(crate) fn start_after_quiesce(
        &self,
        root_task_id: &str,
        session_id: Option<SessionId>,
    ) -> Result<Arc<RunnerHandle>, RunnerStartError> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or(RunnerStartError::NotFound)?;
        if let Some(session_id) = session_id.as_ref() {
            let session = self
                .session_store
                .session(session_id)
                .ok_or(RunnerStartError::SessionUnavailable)?;
            if session.status != SessionLifecycleStatus::Active {
                return Err(RunnerStartError::SessionUnavailable);
            }
        }

        let mut runners = self.runners.lock().expect("runners lock should hold");
        if let Some(existing) = runners.get(root_task_id)
            && existing.active.load(Ordering::Relaxed)
        {
            return Err(RunnerStartError::AlreadyRunning);
        }

        let handle = Arc::new(RunnerHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(true)),
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new("running".to_string())),
            last_error: Arc::new(Mutex::new(None)),
            join_handle: Mutex::new(None),
        });

        let task_runner = Arc::new(self.build_task_runner(session_id.clone()));
        let root_id = tid;
        let bg_handle = Arc::clone(&handle);
        let bg_active = Arc::clone(&handle.active);
        let bg_task_store = Arc::clone(&self.task_store);
        let bg_checkpoint_persist = self.checkpoint_persist.clone();
        let join_handle = tokio::spawn(async move {
            let mut waiting_streak = 0u32;
            loop {
                if bg_handle.cancel.load(Ordering::Relaxed) {
                    let mut status = bg_handle.status.lock().expect("status lock should hold");
                    *status = "killed".to_string();
                    bg_active.store(false, Ordering::Relaxed);
                    break;
                }

                let cycle_runner = Arc::clone(&task_runner);
                let cycle_root_id = root_id.clone();
                let outcome = match tokio::task::spawn_blocking(move || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        cycle_runner.run_cycle(&cycle_root_id)
                    }))
                })
                .await
                {
                    Ok(Ok(outcome)) => outcome,
                    Ok(Err(panic_payload)) => {
                        let panic_message =
                            if let Some(message) = panic_payload.downcast_ref::<&str>() {
                                (*message).to_string()
                            } else if let Some(message) = panic_payload.downcast_ref::<String>() {
                                message.clone()
                            } else {
                                "任务 Runner 执行线程异常退出".to_string()
                            };
                        tracing::error!(
                            root_task_id = %root_id,
                            panic_message = %panic_message,
                            "任务 Runner 执行线程发生 panic，开始收口任务树"
                        );
                        let direct_error = public_runtime_excerpt(
                            &format!("任务 Runner 执行线程异常退出: {panic_message}"),
                            4096,
                        );
                        if let Err(error) =
                            task_runner.finalize_unexpected_failure(&root_id, &direct_error)
                        {
                            tracing::error!(
                                root_task_id = %root_id,
                                ?error,
                                "任务 Runner panic 后任务树收口失败"
                            );
                        }
                        RunCycleOutcome::Error(direct_error)
                    }
                    Err(error) => {
                        RunCycleOutcome::Error(format!("任务 Runner 阻塞执行线程异常退出: {error}"))
                    }
                };
                let cycle = bg_handle.cycle_count.fetch_add(1, Ordering::Relaxed) + 1;
                let mut checkpointed_this_cycle = false;

                // Checkpoint policy consumption (design 3.2).
                if let Some(ref persist) = bg_checkpoint_persist {
                    let should_checkpoint = if let Some(root_task) =
                        bg_task_store.get_task(&root_id)
                    {
                        if let Some(ref policy) = root_task.policy_snapshot {
                            match policy.checkpoint_mode.as_str() {
                                // TaskStore 的每次状态变更已经在提交前触发 checkpoint；
                                // Runner 只需消费变更信号，不能按 100ms 调度周期重复
                                // 重写整个任务投影。
                                "turn" | "task_or_phase" => task_runner.take_checkpoint_signal(),
                                _ => cycle.is_multiple_of(CHECKPOINT_INTERVAL_CYCLES),
                            }
                        } else {
                            cycle.is_multiple_of(CHECKPOINT_INTERVAL_CYCLES)
                        }
                    } else {
                        cycle.is_multiple_of(CHECKPOINT_INTERVAL_CYCLES)
                    };
                    if should_checkpoint {
                        // TaskStore 的 turn/task_or_phase 变更已经在状态提交前通过
                        // checkpoint callback 增量落盘。这里只消费 Runner 信号，避免
                        // AllComplete 分支再次序列化全部历史 root；没有该 callback
                        // 的嵌入/测试运行时继续使用 Runner checkpoint 兜底。
                        let checkpoint_is_already_durable =
                            task_checkpoint_is_already_durable(&bg_task_store, &root_id);
                        if !checkpoint_is_already_durable
                            && let Err(error) = persist(&bg_task_store.snapshot())
                        {
                            fail_runner_for_checkpoint_error(
                                bg_handle.as_ref(),
                                bg_active.as_ref(),
                                &error,
                            );
                            break;
                        }
                        checkpointed_this_cycle = true;
                    }
                }

                match outcome {
                    RunCycleOutcome::Continue => {
                        waiting_streak = 0;
                        {
                            let mut status =
                                bg_handle.status.lock().expect("status lock should hold");
                            if status.as_str() == "blocked" {
                                *status = "running".to_string();
                                let mut last_error = bg_handle
                                    .last_error
                                    .lock()
                                    .expect("last_error lock should hold");
                                *last_error = None;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    RunCycleOutcome::AllComplete => {
                        if !checkpointed_this_cycle
                            && !task_checkpoint_is_already_durable(&bg_task_store, &root_id)
                            && let Some(ref persist) = bg_checkpoint_persist
                            && let Err(error) = persist(&bg_task_store.snapshot())
                        {
                            fail_runner_for_checkpoint_error(
                                bg_handle.as_ref(),
                                bg_active.as_ref(),
                                &error,
                            );
                            break;
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = "completed".to_string();
                        bg_active.store(false, Ordering::Relaxed);
                        break;
                    }
                    RunCycleOutcome::Waiting => {
                        waiting_streak = waiting_streak.saturating_add(1);
                        let backoff_ms = 200u64.saturating_mul(waiting_streak as u64).min(2_000);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                    RunCycleOutcome::Unrunnable(task_ids) => {
                        let runner_status =
                            match task_runner.finalize_unrunnable_outcome(&root_id, &task_ids) {
                                // 候选可能已被别的循环领取、完成或转交；未提交失败就继续观察。
                                Ok(false) => continue,
                                Ok(true) => "error",
                                Err(err) => {
                                    let mut last_error = bg_handle
                                        .last_error
                                        .lock()
                                        .expect("last_error lock should hold");
                                    *last_error = Some(err);
                                    "error"
                                }
                            };
                        if !checkpointed_this_cycle
                            && let Some(ref persist) = bg_checkpoint_persist
                            && let Err(error) = persist(&bg_task_store.snapshot())
                        {
                            fail_runner_for_checkpoint_error(
                                bg_handle.as_ref(),
                                bg_active.as_ref(),
                                &error,
                            );
                            break;
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = runner_status.to_string();
                        bg_active.store(false, Ordering::Relaxed);
                        break;
                    }
                    RunCycleOutcome::Blocked { reason, .. } => {
                        waiting_streak = 0;
                        {
                            let mut status =
                                bg_handle.status.lock().expect("status lock should hold");
                            *status = "blocked".to_string();
                        }
                        {
                            let mut last_error = bg_handle
                                .last_error
                                .lock()
                                .expect("last_error lock should hold");
                            *last_error = Some(reason);
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    RunCycleOutcome::Error(err) => {
                        // A cycle can stop because a child task failed or no
                        // worker could proceed while the root itself is still
                        // Pending/Running.  Persist that root failure through
                        // TaskStore before notifying the Turn side; otherwise
                        // removing Runner's terminal observer would leave a
                        // non-terminal task with no durable completion fact.
                        if bg_task_store.get_task(&root_id).is_some_and(|task| {
                            matches!(
                                task.status,
                                magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                            )
                        }) && let Err(close_error) =
                            task_runner.finalize_unexpected_failure(&root_id, &err)
                        {
                            tracing::error!(
                                root_task_id = %root_id,
                                %close_error,
                                "Runner error 后根任务收口失败"
                            );
                        }
                        if !checkpointed_this_cycle
                            && let Some(ref persist) = bg_checkpoint_persist
                            && let Err(error) = persist(&bg_task_store.snapshot())
                        {
                            fail_runner_for_checkpoint_error(
                                bg_handle.as_ref(),
                                bg_active.as_ref(),
                                &error,
                            );
                            break;
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = "error".to_string();
                        let mut last_error = bg_handle
                            .last_error
                            .lock()
                            .expect("last_error lock should hold");
                        *last_error = Some(err);
                        bg_active.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            }
        });
        *handle
            .join_handle
            .lock()
            .expect("runner join handle lock should hold") = Some(join_handle);
        runners.insert(root_task_id.to_string(), Arc::clone(&handle));
        drop(runners);

        if let Some(session_id) = session_id {
            self.bind_session(session_id, root_task_id);
        }

        Ok(handle)
    }

    pub async fn lock_session_lifecycle(
        &self,
        session_id: &SessionId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .session_lifecycle_locks
                .lock()
                .expect("session lifecycle locks should hold");
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    fn restart_lock(&self, root_task_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.restart_locks
            .lock()
            .expect("runner restart locks should hold")
            .entry(root_task_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub async fn lock_for_restart(&self, root_task_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.restart_lock(root_task_id).lock_owned().await
    }

    /// 只在 TaskStore 提交后的普通回调线程使用；不能在 Tokio worker 或 mutation guard 内调用。
    /// 终态投影与显式 Continue 共用原 scope 锁，避免校验后重开导致旧回调清理新资源。
    pub fn lock_for_task_status_change(
        &self,
        task: &magi_core::Task,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        loop {
            let scope = self
                .task_store
                .execution_scope_root(&task.task_id)
                .unwrap_or_else(|_| task.root_task_id.clone());
            let guard = self.restart_lock(scope.as_str()).blocking_lock_owned();
            if self
                .task_store
                .execution_scope_root(&task.task_id)
                .unwrap_or_else(|_| task.root_task_id.clone())
                == scope
            {
                return guard;
            }
            // 等待期间可能刚完成转交；必须改用当前独立范围的同一把锁。
        }
    }

    pub async fn quiesce_for_restart(&self, root_task_id: &str) {
        let dispatcher = self.dispatcher.clone();
        let existing = {
            self.runners
                .lock()
                .expect("runners lock should hold")
                .get(root_task_id)
                .cloned()
        };
        let Some(handle) = existing else {
            if let Some(dispatcher) = dispatcher.as_ref() {
                dispatcher
                    .wait_for_quiesce(&TaskId::new(root_task_id))
                    .await;
            }
            return;
        };

        handle.cancel.store(true, Ordering::Relaxed);
        let join = handle
            .join_handle
            .lock()
            .expect("runner join handle lock should hold")
            .take();
        if let Some(join) = join
            && let Err(error) = join.await
        {
            *handle.status.lock().expect("status lock should hold") = "error".to_string();
            *handle
                .last_error
                .lock()
                .expect("last_error lock should hold") =
                Some(format!("旧 runner 异常退出: {error}"));
            tracing::error!(
                root_task_id,
                ?error,
                "旧 runner join 异常，已完成生命周期清理"
            );
        }
        if let Some(dispatcher) = dispatcher.as_ref() {
            dispatcher
                .wait_for_quiesce(&TaskId::new(root_task_id))
                .await;
        }
        handle.active.store(false, Ordering::Relaxed);

        let mut runners = self.runners.lock().expect("runners lock should hold");
        if runners
            .get(root_task_id)
            .is_some_and(|current| Arc::ptr_eq(current, &handle))
        {
            runners.remove(root_task_id);
        }
    }

    /// Signal a runner to stop.
    pub fn stop(&self, root_task_id: &str) -> Result<(), RunnerStopError> {
        let handle = self
            .runners
            .lock()
            .expect("runners lock should hold")
            .get(root_task_id)
            .cloned()
            .ok_or(RunnerStopError::NotFound)?;
        if !handle.active.load(Ordering::Relaxed) {
            return Err(RunnerStopError::NotRunning);
        }
        self.signal_runner_stop(&handle);
        Ok(())
    }

    fn signal_stop_if_present(&self, root_task_id: &str) -> bool {
        let handle = self
            .runners
            .lock()
            .expect("runners lock should hold")
            .get(root_task_id)
            .cloned();
        let Some(handle) = handle else {
            return false;
        };
        self.signal_runner_stop(&handle);
        true
    }

    fn signal_runner_stop(&self, handle: &RunnerHandle) {
        handle.cancel.store(true, Ordering::Relaxed);
        let mut status = handle.status.lock().expect("status lock should hold");
        *status = "killed".to_string();
    }

    /// Bind a session to a root task so that when the session closes the
    /// runner is automatically killed.
    pub fn bind_session(&self, session_id: SessionId, root_task_id: &str) {
        let mut index = self
            .session_runner_index
            .lock()
            .expect("session_runner_index lock should hold");
        let roots = index.entry(session_id).or_default();
        if !roots.iter().any(|existing| existing == root_task_id) {
            roots.push(root_task_id.to_string());
        }
    }

    /// Cancel all runners bound to the given session and remove the binding.
    /// Called when a session is closed.
    pub async fn unbind_session(&self, session_id: &SessionId) -> Result<usize, String> {
        let _session_guard = self.lock_session_lifecycle(session_id).await;
        self.unbind_session_after_lifecycle_lock(session_id).await
    }

    pub async fn unbind_session_after_lifecycle_lock(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, String> {
        let mut root_task_ids = {
            let index = self
                .session_runner_index
                .lock()
                .expect("session_runner_index lock should hold");
            index.get(session_id).cloned().unwrap_or_default()
        };
        if let Some(chain) = self.session_store.active_execution_chain(session_id)
            && !root_task_ids
                .iter()
                .any(|root_task_id| root_task_id == chain.root_task_id.as_str())
        {
            root_task_ids.push(chain.root_task_id.to_string());
        }
        // 重启后或注册交接期间可能尚无进程内绑定；从既有 Task/Agent 与 canonical
        // 归属补全审计 root，不能只依赖当前主线指针或 Runner 内存索引。
        let mut owned_tasks = self
            .session_store
            .execution_task_ids_for_session(session_id);
        owned_tasks.extend(
            self.task_store
                .all_tasks()
                .into_iter()
                .filter(|task| {
                    task.agent_activation()
                        .is_some_and(|activation| &activation.session_id == session_id)
                })
                .map(|task| task.task_id),
        );
        for task_id in owned_tasks {
            if let Some(task) = self.task_store.get_task(&task_id)
                && !root_task_ids
                    .iter()
                    .any(|id| id == task.root_task_id.as_str())
            {
                root_task_ids.push(task.root_task_id.to_string());
            }
        }
        for root_task_id in &root_task_ids {
            let _restart_guard = self.lock_for_restart(root_task_id).await;
            // Session 销毁跨过转交边界；先撤销全部 Task/lease，再等执行线程退出，
            // 不能只停止 Runner 循环后等仍有效的已转交执行自然结束。
            if self
                .task_store
                .get_task(&TaskId::new(root_task_id))
                .is_some()
            {
                self.kill_tree(root_task_id)?;
            }
            self.quiesce_for_restart(root_task_id).await;
            if let Some(dispatcher) = self.dispatcher.as_ref() {
                dispatcher
                    .wait_for_audit_tree_quiesce(&TaskId::new(root_task_id))
                    .await;
            }
        }
        self.execution_admission.release_pending_session(session_id);
        self.session_runner_index
            .lock()
            .expect("session runner index lock poisoned")
            .remove(session_id);
        Ok(root_task_ids.len())
    }

    /// Get the status of a runner.
    pub fn status(&self, root_task_id: &str) -> Option<RunnerStatusSnapshot> {
        let runners = self.runners.lock().expect("runners lock should hold");
        runners.get(root_task_id).map(|handle| {
            let status = handle
                .status
                .lock()
                .expect("status lock should hold")
                .clone();
            let cycle_count = handle.cycle_count.load(Ordering::Relaxed);
            let last_error = handle
                .last_error
                .lock()
                .expect("last_error lock should hold")
                .clone();
            RunnerStatusSnapshot {
                root_task_id: root_task_id.to_string(),
                status,
                cycle_count,
                last_error,
            }
        })
    }

    pub fn kill_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        // 整体销毁也停止子树内独立拥有方的 Runner；只停审计 root 会留下空转调度器。
        let scope_roots = self.task_store.collect_subtree_ids(&tid);
        for scope in &scope_roots {
            self.signal_stop_if_present(scope.as_str());
        }
        let result = self.build_task_runner(None).kill_tree(&tid);
        for scope in scope_roots {
            self.set_runner_status_if_present(scope.as_str(), "killed");
        }
        result
    }

    pub(crate) fn kill_execution_scope(&self, scope_root: &str) -> Result<(), String> {
        let tid = TaskId::new(scope_root);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {scope_root}"))?;
        self.signal_stop_if_present(scope_root);
        let result = self.build_task_runner(None).kill_execution_scope(&tid);
        self.set_runner_status_if_present(scope_root, "killed");
        result
    }

    pub fn kill_task(&self, task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(task_id);
        let task = self
            .task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", task_id))?;
        self.build_task_runner(None).kill_task(&tid)?;
        self.set_runner_status_if_present(task.root_task_id.as_str(), "killed");
        Ok(())
    }

    pub fn resume_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.build_task_runner(None).resume_task(&tid)
    }

    fn set_runner_status_if_present(&self, root_task_id: &str, status: &str) {
        let runners = self.runners.lock().expect("runners lock should hold");
        let Some(handle) = runners.get(root_task_id) else {
            return;
        };
        let mut current = handle.status.lock().expect("status lock should hold");
        *current = status.to_string();
    }
}

#[derive(Debug)]
pub enum RunnerStartError {
    NotFound,
    AlreadyRunning,
    SessionUnavailable,
}

#[derive(Debug)]
pub enum RunnerStopError {
    NotFound,
    NotRunning,
}

#[derive(Clone, Debug)]
pub struct RunnerStatusSnapshot {
    pub root_task_id: String,
    pub status: String,
    pub cycle_count: u64,
    pub last_error: Option<String>,
}

/// 用户在设置里控制的浏览器能力开关。daemon 是它们的唯一所有者：
/// 持久化在设置存储的 `browser` 分区，运行时随状态快照一起读取。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCapabilitySettings {
    pub in_app_browser_enabled: bool,
    pub browser_use_enabled: bool,
    /// 开发者工具集（性能、Lighthouse、堆快照等低频诊断）对模型可见。
    pub devtools_enabled: bool,
    /// 浏览器允许访问局域网私有网段；本机回环地址始终允许。
    pub lan_access_enabled: bool,
}

impl Default for BrowserCapabilitySettings {
    fn default() -> Self {
        Self {
            in_app_browser_enabled: true,
            browser_use_enabled: true,
            devtools_enabled: false,
            lan_access_enabled: false,
        }
    }
}

impl BrowserCapabilitySettings {
    pub fn from_section(section: &serde_json::Value) -> Self {
        let defaults = Self::default();
        let flag = |key: &str, default: bool| {
            section
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default)
        };
        Self {
            in_app_browser_enabled: flag("inAppBrowserEnabled", defaults.in_app_browser_enabled),
            browser_use_enabled: flag("browserUseEnabled", defaults.browser_use_enabled),
            devtools_enabled: flag("devtoolsEnabled", defaults.devtools_enabled),
            lan_access_enabled: flag("lanAccessEnabled", defaults.lan_access_enabled),
        }
    }

    pub fn to_section(self) -> serde_json::Value {
        serde_json::json!({
            "inAppBrowserEnabled": self.in_app_browser_enabled,
            "browserUseEnabled": self.browser_use_enabled,
            "devtoolsEnabled": self.devtools_enabled,
            "lanAccessEnabled": self.lan_access_enabled,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserHostStatusSnapshot {
    pub revision: u64,
    #[serde(flatten)]
    pub settings: BrowserCapabilitySettings,
    pub status: BrowserHostStatus,
    pub protocol_compatible: bool,
    pub last_error_code: Option<String>,
}

/// Electron Desktop 控制通道的运行时注册信息。
///
/// 正式包由 Electron 启动 daemon 时通过环境变量提供；开发模式先启动的
/// daemon 则由 Electron 在启动后注册。两条路径最终都收敛到同一个状态，
/// 不再要求 daemon 重启或启动第二个端口实例。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostConnectionConfig {
    pub socket_path: String,
    pub auth_token: String,
    pub desktop_epoch: String,
    pub parent_pid: u32,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserHostOwner {
    desktop_epoch: String,
    parent_pid: u32,
}

impl From<&BrowserHostConnectionConfig> for BrowserHostOwner {
    fn from(config: &BrowserHostConnectionConfig) -> Self {
        Self {
            desktop_epoch: config.desktop_epoch.clone(),
            parent_pid: config.parent_pid,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserHostConnectionRegistrationError {
    Conflict { current_generation: u64 },
    OwnerConflict { current_generation: u64 },
}

impl Default for BrowserHostStatusSnapshot {
    fn default() -> Self {
        Self {
            revision: 1,
            settings: BrowserCapabilitySettings::default(),
            status: BrowserHostStatus::Stopped,
            protocol_compatible: false,
            last_error_code: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionResourceCancellationReport {
    pub tool_execution_count: usize,
    pub browser_lease_count: usize,
}

impl ExecutionResourceCancellationReport {
    pub fn total(self) -> usize {
        self.tool_execution_count
            .saturating_add(self.browser_lease_count)
    }
}

#[derive(Clone)]
pub struct ExecutionResourceCoordinator {
    tool_registry: Arc<RwLock<Option<ToolRegistry>>>,
    browser_authority: Arc<Mutex<BrowserAuthority>>,
    browser_write_lock: Arc<Mutex<()>>,
    browser_host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    event_bus: Arc<InMemoryEventBus>,
}

impl ExecutionResourceCoordinator {
    fn new(
        browser_authority: Arc<Mutex<BrowserAuthority>>,
        browser_write_lock: Arc<Mutex<()>>,
        browser_host_client: Arc<RwLock<Option<BrowserHostClient>>>,
        event_bus: Arc<InMemoryEventBus>,
    ) -> Self {
        Self {
            tool_registry: Arc::new(RwLock::new(None)),
            browser_authority,
            browser_write_lock,
            browser_host_client,
            event_bus,
        }
    }

    fn set_tool_registry(&self, registry: ToolRegistry) {
        *self
            .tool_registry
            .write()
            .expect("execution resource tool registry lock poisoned") = Some(registry);
    }

    pub fn cancel(
        &self,
        query: ToolExecutionContextQuery,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> ExecutionResourceCancellationReport {
        let tool_execution_count = self
            .tool_registry
            .read()
            .expect("execution resource tool registry lock poisoned")
            .as_ref()
            .map_or(0, |registry| registry.cancel_active_executions(&query));
        let browser_lease_count = self.revoke_browser_leases(&query, reason, now);
        ExecutionResourceCancellationReport {
            tool_execution_count,
            browser_lease_count,
        }
    }

    /// 只撤销 Chromium 自动化控制租约，不影响同一会话中仍可继续运行的
    /// shell、文件或其他非浏览器工具。
    pub fn revoke_browser_leases(
        &self,
        query: &ToolExecutionContextQuery,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> usize {
        let selector = BrowserLeaseSelector {
            session_id: query.session_id.clone(),
            workspace_id: query.workspace_id.clone(),
            task_id: query.task_id.clone(),
            worker_id: query.worker_id.clone(),
            taskless_only: query.taskless_only,
            ..BrowserLeaseSelector::default()
        };
        let (revoked, controls) = {
            let _write_guard = self
                .browser_write_lock
                .lock()
                .expect("browser authority write lock poisoned");
            let mut authority = self
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned");
            let revoked = authority.revoke_leases(&selector, reason, now);
            let controls = revoked
                .iter()
                .filter_map(|lease| {
                    authority
                        .surface_control_snapshot(&lease.tab_id, &lease.surface_id)
                        .ok()
                })
                .collect::<Vec<_>>();
            (revoked, controls)
        };
        self.synchronize_browser_controls(controls);
        for lease in &revoked {
            self.event_bus.publish(
                EventEnvelope::domain(
                    magi_core::EventId::unique(format!(
                        "event-browser-lease-revoked-{}",
                        lease.lease_id
                    )),
                    "browser.lease.revoked",
                    serde_json::json!({
                        "lease_id": lease.lease_id,
                        "tab_id": lease.tab_id,
                        "surface_id": lease.surface_id,
                        "reason": reason,
                        "fence": lease.fence,
                    }),
                )
                .with_context(EventContext {
                    workspace_id: lease.owner.workspace_id.clone(),
                    session_id: lease.owner.session_id.clone(),
                    mission_id: lease.owner.mission_id.clone(),
                    task_id: lease.owner.task_id.clone(),
                    ..EventContext::default()
                }),
            );
        }
        revoked.len()
    }

    fn cancel_surface(
        &self,
        tab_id: &BrowserTabId,
        surface_id: &str,
        reason: BrowserLeaseEndReason,
        now: UtcMillis,
    ) -> Vec<BrowserControlLease> {
        let selector = BrowserLeaseSelector {
            tab_id: Some(tab_id.clone()),
            surface_id: Some(surface_id.to_string()),
            ..BrowserLeaseSelector::default()
        };
        let (revoked, controls) = {
            let _write_guard = self
                .browser_write_lock
                .lock()
                .expect("browser authority write lock poisoned");
            let mut authority = self
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned");
            let revoked = authority.revoke_leases(&selector, reason, now);
            let controls = revoked
                .iter()
                .filter_map(|lease| {
                    authority
                        .surface_control_snapshot(&lease.tab_id, &lease.surface_id)
                        .ok()
                })
                .collect::<Vec<_>>();
            (revoked, controls)
        };
        self.synchronize_browser_controls(controls);
        revoked
    }

    fn synchronize_browser_controls(&self, controls: Vec<BrowserSurfaceControlSnapshot>) {
        let client = self
            .browser_host_client
            .read()
            .expect("browser Host client lock poisoned")
            .clone();
        let Some(client) = client else {
            return;
        };
        for control in controls {
            spawn_browser_control_sync(client.clone(), control);
        }
    }
}

fn spawn_browser_control_sync(client: BrowserHostClient, control: BrowserSurfaceControlSnapshot) {
    let future = async move {
        let result = client
            .request(BrowserHostCommand::UpdateControl {
                tab_id: control.tab_id.clone(),
                surface_id: control.surface_id.clone(),
                control: BrowserHostControlUpdate::Released {
                    fence: control.fence,
                },
            })
            .await;
        match result {
            Ok(reply)
                if matches!(
                    reply.response.outcome,
                    BrowserHostCommandOutcome::Succeeded(_)
                ) => {}
            Ok(reply) => tracing::warn!(
                tab_id = %control.tab_id,
                surface_id = %control.surface_id,
                fence = control.fence,
                outcome = ?reply.response.outcome,
                "同步已撤销 Browser Lease 的 Host fence 失败"
            ),
            Err(error) => tracing::warn!(
                tab_id = %control.tab_id,
                surface_id = %control.surface_id,
                fence = control.fence,
                ?error,
                "同步已撤销 Browser Lease 的 Host fence 失败"
            ),
        }
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    } else {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                runtime.block_on(future);
            }
        });
    }
}

/// 在后台执行一段需要 session Turn 锁的异步工作。
///
/// TaskStore 状态回调可能运行在普通的提交后线程上，不能在没有 runtime 的线程里直接
/// `tokio::spawn`：有 runtime 时挂到当前 runtime，否则使用进程级后台 runtime。
/// 不能为每项工作创建临时 runtime：finalizer 派生的队列/Goal 续跑会在父 future
/// 返回时随临时 runtime 一起取消，留下永远没有下一次唤醒的持久 Pending 输入。
pub(crate) fn spawn_session_turn_work(
    thread_name: &str,
    work: impl std::future::Future<Output = ()> + Send + 'static,
) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(work);
    } else {
        static BACKGROUND_RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
            std::sync::OnceLock::new();
        match BACKGROUND_RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("magi-session-background")
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        }) {
            Ok(runtime) => {
                runtime.spawn(work);
            }
            Err(error) => tracing::error!(%error, thread_name, "启动 session Turn 后台运行时失败"),
        }
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub service_info: ServiceInfo,
    daemon_identity: DaemonIdentity,
    runtime_epoch: String,
    pub event_bus: Arc<InMemoryEventBus>,
    pub session_store: Arc<SessionStore>,
    pub workspace_registry: Arc<WorkspaceStore>,
    /// Git branch/worktree 的唯一结构化执行服务。repository mutex 在所有 session 间共享。
    pub git_service: Arc<magi_git::GitService>,
    /// 主对话 session 与代码/Git 上下文的正交绑定，不承载 conversation fork 或任务分支。
    pub session_code_contexts: magi_git::SessionCodeContextRegistry,
    /// 运行在隔离工作副本里的会话。隔离会话不占用主工作区的执行租约，也没有主工作区的 Git 上下文。
    pub session_isolations: magi_session_isolation::SessionIsolationRegistry,
    /// 正在等别的会话用完工作区的会话（会话 → 占用工作区的会话）。事件只通知变化，
    /// 页面刷新或重连后靠这份快照还原「正在等谁」。
    pub session_workspace_waits: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// 目标模式「起始轮」的原始请求（根任务 → 请求）。目标由模型在这一轮里创建，轮次在创建前就因
    /// 暂态故障失败时还没有目标可续跑，只能重提原请求；终态收口时取走，只在内存里保留。
    pub goal_start_requests: Arc<Mutex<HashMap<String, SessionTurnRequestDto>>>,
    /// turn/worker 与 Git mutation 的 workspace 级 lease 协调器，消除“检查后立即竞态”。
    pub workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator,
    pub governance: Arc<GovernanceService>,
    pub knowledge_store: Arc<KnowledgeStore>,
    pub settings_store: Arc<SettingsStore>,
    pub appearance_library: Arc<magi_appearance::AppearanceLibrary>,
    runtime_persistence: Option<Arc<RuntimeStatePersistence>>,
    canonical_event_next_sequence_provider: Option<CanonicalEventNextSequenceProvider>,
    session_state_checkpoint_persist: Option<SessionStateCheckpointPersist>,
    session_projection_persist: Option<SessionProjectionPersist>,
    unavailable_session_discard: Option<UnavailableSessionDiscard>,
    workspace_history_restore: Option<WorkspaceHistoryRestore>,
    execution_pipeline: Option<ExecutionPipeline>,
    task_execution_registry: TaskExecutionRegistry,
    task_store: Option<Arc<TaskStore>>,
    /// TaskStore terminal transitions are routed through this notifier before
    /// they reach the Turn finalizer.  It is optional for lightweight test
    /// states that do not construct a task runtime.
    task_completion_notifier: Option<Arc<TaskCompletionNotifier>>,
    runner_manager: Option<Arc<RunnerManager>>,
    session_turn_dispatcher: Option<Arc<LlmTaskDispatcher>>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    tool_registry: Option<ToolRegistry>,
    pub browser_authority: Arc<Mutex<BrowserAuthority>>,
    browser_write_lock: Arc<Mutex<()>>,
    pub(crate) browser_control_locks: BrowserControlLocks,
    pub(crate) browser_downloads: Arc<crate::browser_downloads::BrowserDownloadRegistry>,
    app_server_request_locks: Arc<Mutex<HashMap<String, AppServerRequestLockEntry>>>,
    browser_state_writable: Arc<AtomicBool>,
    browser_host_status: Arc<RwLock<BrowserHostStatusSnapshot>>,
    browser_host_connection: Arc<watch::Sender<Option<BrowserHostConnectionConfig>>>,
    browser_host_connection_generation: Arc<AtomicU64>,
    browser_host_owner: Arc<Mutex<Option<BrowserHostOwner>>>,
    browser_host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    browser_host_generation: Arc<AtomicU64>,
    managed_process_group: ManagedProcessGroup,
    execution_resources: ExecutionResourceCoordinator,
    pub skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    pub skill_dispatch_runtime: Option<Arc<magi_skill_runtime::SkillDispatchRuntime>>,
    /// 受信扩展宿主的运行状态来源；None 表示没有扩展宿主（例如纯 API 测试）。
    pub(crate) extension_status: Option<Arc<dyn ExtensionStatusSource>>,
    /// 插件视图后端路由的执行端口；None 表示没有可执行扩展。
    pub(crate) plugin_route_executor: Option<Arc<dyn PluginRouteExecutor>>,
    /// 上游生态插件的翻译工具链（随包 Node 与兼容运行时）；None 表示本机不能翻译上游插件。
    pub(crate) plugin_toolchain: Option<PluginToolchain>,
    /// 扩展 `input` 钩子端口；None 表示没有可执行扩展。
    pub(crate) input_guard: Option<Arc<dyn magi_conversation_runtime::extension_hooks::InputGuard>>,
    pub tunnel_manager: crate::tunnel::TunnelManager,
    /// 插件安装、授权和生命周期的唯一 daemon 领域服务。
    pub plugin_manager: Arc<Mutex<magi_plugin_system::PluginManager>>,
    pub(crate) plugin_jobs: Arc<crate::plugin_jobs::PluginJobs>,
    pub(crate) plugin_installations: Arc<crate::plugin_installations::PluginInstallations>,
    /// 升级时执行插件数据格式转换的 Worker 执行者；未装配时需要转换的升级被拒绝。
    pub(crate) mcp_service: Arc<crate::mcp_runtime::McpServiceRuntime>,
    pub snapshot_manager: Arc<SnapshotManager>,
    pub conversation_registry: Arc<ConversationRegistry>,
    /// Session Turn 的单一生命周期所有者。只保存轻量身份与 attempt，不复制正文。
    pub turn_coordinator: Arc<magi_conversation_runtime::SessionTurnCoordinator>,
    pub(crate) terminal_sessions: crate::terminal_runtime::TerminalSessionManager,
    /// 任务系统：AgentRole 注册表（替代 task_worker_catalog 硬编码 prompt）。
    /// 加载策略：`~/.magi/roles/*.md` 与 crate 内置 builtin 集合并加载。
    pub agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
    /// 角色定义与绑定共享同一把进程内事务锁，避免删除角色时并发写入悬空绑定。
    pub(crate) role_configuration_lock: Arc<Mutex<()>>,
    /// 任务系统 — L5：父子任务关系图，作为 task_dispatch 中
    /// "parent_task_id 散落查询"的统一上层。同一进程共享。
    session_turn_locks: Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>,
    session_change_sync_locks: Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>,
    session_navigation_lock: Arc<tokio::sync::Mutex<()>>,
    /// 项目管理与工具/终端准入的短事务锁；不持有成员或授权事实。
    project_directory_locks: Arc<Mutex<HashMap<WorkspaceId, Arc<tokio::sync::RwLock<()>>>>>,
}

#[derive(Clone, Debug)]
pub struct RuntimeStatePersistence {
    root_path: PathBuf,
    workspace_path: PathBuf,
    knowledge_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
    knowledge_save_scheduled: Arc<AtomicBool>,
    knowledge_save_dirty: Arc<AtomicBool>,
}

const SESSION_PERSISTENCE_PUBLIC_ERROR: &str = "会话状态暂不可保存，请稍后重试";
const WORKSPACE_PERSISTENCE_PUBLIC_ERROR: &str = "工作区状态暂不可保存，请稍后重试";
const KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR: &str = "知识库状态暂不可保存，请稍后重试";
const BROWSER_PERSISTENCE_PUBLIC_ERROR: &str = "浏览器状态暂不可保存，请稍后重试";
const DEFAULT_BROWSER_PROFILE_ID: &str = "browser-profile-default";
const ROLE_DELETE_JOURNAL_FILE: &str = "role-delete-transaction.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RoleDeleteTransaction {
    role_id: String,
    role_file_contents: Option<String>,
    agents_section: Option<serde_json::Value>,
    phase: RoleDeleteTransactionPhase,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RoleDeleteTransactionPhase {
    Prepared,
    Committed,
}

fn role_delete_journal_path(state_root: &Path) -> PathBuf {
    state_root.join(ROLE_DELETE_JOURNAL_FILE)
}

fn role_delete_role_path(state_root: &Path, role_id: &str) -> Result<PathBuf, String> {
    magi_agent_role::validate_role_id(role_id)
        .map_err(|error| format!("角色删除事务中的 role_id 无效: {error}"))?;
    Ok(state_root.join("roles").join(format!("{role_id}.md")))
}

pub(crate) fn prepare_role_delete_transaction(
    state_root: &Path,
    role_id: &str,
    role_file_contents: Option<String>,
    agents_section: Option<serde_json::Value>,
) -> Result<(), String> {
    let _ = role_delete_role_path(state_root, role_id)?;
    fs::create_dir_all(state_root).map_err(|error| format!("创建角色删除事务目录失败: {error}"))?;
    let journal = RoleDeleteTransaction {
        role_id: role_id.to_string(),
        role_file_contents,
        agents_section,
        phase: RoleDeleteTransactionPhase::Prepared,
    };
    let content =
        serde_json::to_vec(&journal).map_err(|error| format!("序列化角色删除事务失败: {error}"))?;
    magi_core::fs_atomic::write_atomic(&role_delete_journal_path(state_root), content)
        .map_err(|error| format!("写入角色删除事务失败: {error}"))
}

pub(crate) fn mark_role_delete_transaction_committed(state_root: &Path) -> Result<(), String> {
    let path = role_delete_journal_path(state_root);
    let content = fs::read(&path).map_err(|error| format!("读取角色删除事务失败: {error}"))?;
    let mut journal: RoleDeleteTransaction = serde_json::from_slice(&content)
        .map_err(|error| format!("解析角色删除事务失败: {error}"))?;
    journal.phase = RoleDeleteTransactionPhase::Committed;
    let content =
        serde_json::to_vec(&journal).map_err(|error| format!("序列化角色删除事务失败: {error}"))?;
    magi_core::fs_atomic::write_atomic(&path, content)
        .map_err(|error| format!("提交角色删除事务失败: {error}"))
}

pub(crate) fn clear_role_delete_transaction(state_root: &Path) -> Result<(), String> {
    let path = role_delete_journal_path(state_root);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("清理角色删除事务失败: {error}")),
    }
}

fn restore_role_delete_transaction(
    state_root: &Path,
    settings_store: &SettingsStore,
    journal: &RoleDeleteTransaction,
) -> Result<(), String> {
    let role_path = role_delete_role_path(state_root, &journal.role_id)?;
    match &journal.role_file_contents {
        Some(contents) => {
            let parent = role_path
                .parent()
                .ok_or_else(|| "角色文件缺少父目录".to_string())?;
            fs::create_dir_all(parent).map_err(|error| format!("恢复角色目录失败: {error}"))?;
            magi_core::fs_atomic::write_atomic(&role_path, contents.as_bytes())
                .map_err(|error| format!("恢复角色文件失败: {error}"))?;
        }
        None => match fs::remove_file(&role_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("清理角色文件失败: {error}")),
        },
    }
    match &journal.agents_section {
        Some(value) => settings_store
            .set_section("agents", value.clone())
            .map_err(|error| format!("恢复角色绑定失败: {error}"))?,
        None => settings_store
            .remove_section("agents")
            .map_err(|error| format!("清理角色绑定失败: {error}"))?,
    }
    Ok(())
}

/// 在 daemon 启动时恢复未完成的角色删除事务。
///
/// `prepared` 表示删除尚未形成可恢复的提交点，必须完整恢复删除前像；
/// `committed` 表示角色文件与绑定都已删除，只需清理事务记录。
pub fn recover_role_delete_transaction(
    state_root: &Path,
    settings_store: &SettingsStore,
) -> Result<(), String> {
    let path = role_delete_journal_path(state_root);
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取角色删除事务失败: {error}")),
    };
    let journal: RoleDeleteTransaction = serde_json::from_slice(&content)
        .map_err(|error| format!("解析角色删除事务失败: {error}"))?;
    if journal.phase == RoleDeleteTransactionPhase::Prepared {
        restore_role_delete_transaction(state_root, settings_store, &journal)?;
    }
    clear_role_delete_transaction(state_root)
}

impl RuntimeStatePersistence {
    pub fn new(
        root_path: impl Into<PathBuf>,
        workspace_path: impl Into<PathBuf>,
        knowledge_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            root_path: root_path.into(),
            workspace_path: workspace_path.into(),
            knowledge_path: knowledge_path.into(),
            write_lock: Arc::new(Mutex::new(())),
            knowledge_save_scheduled: Arc::new(AtomicBool::new(false)),
            knowledge_save_dirty: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn state_root(&self) -> Option<&Path> {
        Some(self.root_path.as_path())
    }

    /// daemon 的恢复/迁移与运行中持久化必须串行写入同一状态根。
    pub fn with_shared_write_lock(mut self, write_lock: Arc<Mutex<()>>) -> Self {
        self.write_lock = write_lock;
        self
    }

    pub(crate) fn save_json<T>(&self, path: &Path, value: &T) -> Result<(), ApiError>
    where
        T: serde::Serialize,
    {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("runtime persistence write lock poisoned");
        Self::write_json(path, value)
    }

    fn write_json<T>(path: &Path, value: &T) -> Result<(), ApiError>
    where
        T: serde::Serialize,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ApiError::internal_assembly("创建运行态持久化目录失败", error))?;
        }
        let payload = serde_json::to_vec_pretty(value)
            .map_err(|error| ApiError::internal_assembly("序列化运行态持久化数据失败", error))?;
        magi_core::fs_atomic::write_atomic(path, payload)
            .map_err(|error| ApiError::internal_assembly("写入运行态持久化文件失败", error))?;
        Ok(())
    }

    fn save_workspace_store(&self, store: &WorkspaceStore) -> Result<(), ApiError> {
        store.with_durable_state(|snapshot| self.save_json(&self.workspace_path, snapshot))
    }

    pub(crate) fn save_knowledge_store(&self, store: &KnowledgeStore) -> Result<(), ApiError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("runtime persistence write lock poisoned");
        let snapshot = store.export_state();
        Self::write_json(&self.knowledge_path, &snapshot)
    }

    pub(crate) fn schedule_knowledge_store_save(&self, store: KnowledgeStore) {
        self.knowledge_save_dirty.store(true, Ordering::Release);
        if self
            .knowledge_save_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let persistence = self.clone();
        std::thread::spawn(move || {
            loop {
                persistence
                    .knowledge_save_dirty
                    .store(false, Ordering::Release);
                if let Err(error) = persistence.save_knowledge_store(&store) {
                    tracing::warn!(?error, "异步持久化索引维护后的知识状态失败");
                }
                if persistence.knowledge_save_dirty.load(Ordering::Acquire) {
                    continue;
                }
                if persistence
                    .knowledge_save_scheduled
                    .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    continue;
                }
                if persistence.knowledge_save_dirty.load(Ordering::Acquire)
                    && persistence
                        .knowledge_save_scheduled
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                break;
            }
        });
    }
}

fn public_runtime_persistence_error(
    domain: &'static str,
    public_message: &'static str,
    error: ApiError,
) -> ApiError {
    tracing::warn!(domain, error = ?error, "runtime state persistence failed");
    ApiError::InternalAssemblyError(public_message.to_string())
}

pub fn build_runtime_capability_dependency_provider(
    context_runtime_available: bool,
) -> RuntimeCapabilityDependencyProvider {
    Arc::new(move || {
        vec![
            context_runtime_capability_dependency(context_runtime_available),
            file_snapshot_capability_dependency(),
        ]
    })
}

fn context_runtime_capability_dependency(
    context_runtime_available: bool,
) -> RuntimeCapabilityDependencyEntry {
    let status = if !context_runtime_available {
        "unavailable"
    } else {
        "ready"
    };

    RuntimeCapabilityDependencyEntry {
        name: "context_runtime".to_string(),
        status: status.to_string(),
        required_by: vec![
            "task_execution".to_string(),
            "conversation_context".to_string(),
            "knowledge_memory_selection".to_string(),
        ],
        role_count: None,
        spawnable_role_count: None,
        configured_count: None,
        enabled_count: None,
        ready_count: None,
        tool_count: None,
    }
}

fn file_snapshot_capability_dependency() -> RuntimeCapabilityDependencyEntry {
    RuntimeCapabilityDependencyEntry {
        name: "file_snapshot".to_string(),
        status: "ready".to_string(),
        required_by: vec![
            "changes/diff".to_string(),
            "changes/approve".to_string(),
            "changes/revert".to_string(),
        ],
        role_count: None,
        spawnable_role_count: None,
        configured_count: None,
        enabled_count: None,
        ready_count: None,
        tool_count: None,
    }
}

fn workspace_root_path_from_registry(
    workspace_registry: &WorkspaceStore,
    workspace_id: &WorkspaceId,
) -> Option<PathBuf> {
    workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace.workspace_id == *workspace_id)
        .map(|workspace| workspace.native_root_path())
}

fn canonicalize_path_for_workspace_match(path: &Path) -> PathBuf {
    magi_core::HostPath::canonicalize(path)
        .map(magi_core::HostPath::into_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn default_browser_authority(state_root: &Path) -> Result<BrowserAuthority, BrowserAuthorityError> {
    let now = UtcMillis::now();
    let mut authority = BrowserAuthority::new();
    authority.register_profile(BrowserProfile {
        profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
        kind: BrowserProfileKind::ManagedDefault,
        data_path: state_root.join("browser/profiles/default"),
        created_at: now,
        updated_at: now,
    })?;
    Ok(authority)
}

/// 浏览器物理控制的串行化按 Tab 进行：同一页面上的接管、导航、关闭与代理控制同步
/// 互斥，不同页面互不阻塞，一条慢的 Host 命令不会拖住其他会话。
#[derive(Clone, Default)]
pub struct BrowserControlLocks {
    locks: Arc<Mutex<HashMap<BrowserTabId, Arc<tokio::sync::Mutex<()>>>>>,
}

impl BrowserControlLocks {
    fn lock_handle(&self, tab_id: &BrowserTabId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().expect("browser control locks poisoned");
        // 没有持有者的锁可以回收，登记表规模只取决于正在被控制的页面数。
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(
            locks
                .entry(tab_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    pub async fn lock_tab(&self, tab_id: &BrowserTabId) -> tokio::sync::OwnedMutexGuard<()> {
        self.lock_handle(tab_id).lock_owned().await
    }

    /// 同时控制多个 Tab 时按固定顺序加锁，避免互相等待。
    pub async fn lock_tabs(
        &self,
        tab_ids: &[BrowserTabId],
    ) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
        let mut ordered = tab_ids.to_vec();
        ordered.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ordered.dedup();
        let mut guards = Vec::with_capacity(ordered.len());
        for tab_id in &ordered {
            guards.push(self.lock_tab(tab_id).await);
        }
        guards
    }
}

pub(crate) enum BrowserAuthorityCommitError {
    NotWritable,
    Rejected(BrowserAuthorityError),
    Persist(ApiError),
}

/// BrowserAuthority 的唯一提交路径（API 路由与浏览器工具共用）：在副本上执行变更，
/// 持久化快照确有变化时才写盘，成功后再替换内存中的权威状态。
pub(crate) fn commit_browser_authority_mutation<T>(
    authority: &Mutex<BrowserAuthority>,
    write_lock: &Mutex<()>,
    state_writable: &AtomicBool,
    persistence: Option<&RuntimeStatePersistence>,
    mutation: impl FnOnce(&mut BrowserAuthority) -> Result<T, BrowserAuthorityError>,
) -> Result<T, BrowserAuthorityCommitError> {
    if !state_writable.load(Ordering::Acquire) {
        return Err(BrowserAuthorityCommitError::NotWritable);
    }
    let _write_guard = write_lock
        .lock()
        .expect("browser authority write lock poisoned");
    let mut candidate = authority
        .lock()
        .expect("browser authority lock poisoned")
        .clone();
    let before = browser_durable_state_for_persistence(&candidate);
    let output = mutation(&mut candidate).map_err(BrowserAuthorityCommitError::Rejected)?;
    let after = browser_durable_state_for_persistence(&candidate);
    // 租约、当前 Tab 等运行态变化不进入持久化快照，不必重写 state.json。
    if after != before
        && let Some(persistence) = persistence
        && let Some(state_root) = persistence.state_root()
    {
        persistence
            .save_json(&state_root.join("browser/state.json"), &after)
            .map_err(BrowserAuthorityCommitError::Persist)?;
    }
    *authority.lock().expect("browser authority lock poisoned") = candidate;
    Ok(output)
}

fn browser_durable_state_for_persistence(authority: &BrowserAuthority) -> BrowserDurableState {
    authority.durable_state()
}

fn parse_browser_durable_state(bytes: &[u8]) -> Result<BrowserDurableState, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "browser durable state 必须是 JSON object".to_string())?;
    // BrowserDurableState 的顶层结构故意在这里做严格校验。BrowserAuthority 的
    // domain DTO 复用于多条协议边界，不能把 serde 默认忽略未知字段的行为当成
    // A19 的兼容策略；未知结构必须落入「无状态重建」，不能半恢复半丢字段。
    const KNOWN_FIELDS: [&str; 6] = [
        "schema_version",
        "revision",
        "profiles",
        "sessions",
        "tabs",
        "annotations",
    ];
    if let Some(unknown) = object
        .keys()
        .find(|key| !KNOWN_FIELDS.iter().any(|known| known == key))
    {
        return Err(format!("unknown browser durable state field: {unknown}"));
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// 加载浏览器 durable state。
///
/// 按 A19 约定，这是**全函数**：浏览器状态只是可重建的 Tab 指针，文件结构不认识、
/// schema 版本不认识或内容非法时一律按「无状态」重建并覆盖写回（返回 `true`），
/// 不让 daemon 起不来，也不把浏览器能力置于 Failed。Electron partition 里的
/// 登录态不受影响，用户重新打开页面即可。
fn load_browser_authority(
    state_root: &Path,
) -> Result<(BrowserAuthority, bool), Box<dyn std::error::Error + Send + Sync>> {
    let path = state_root.join("browser/state.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((default_browser_authority(state_root)?, true));
        }
        Err(error) => return Err(Box::new(error)),
    };
    let (restored, stripped_transient_tabs) = match parse_browser_durable_state(&bytes) {
        Ok(durable) => (
            BrowserAuthority::restore_durable(durable, UtcMillis::now()),
            false,
        ),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "浏览器持久状态无法解析，按无状态重建并覆盖写回"
            );
            return Ok((default_browser_authority(state_root)?, true));
        }
    };
    let mut authority = match restored {
        Ok(authority) => authority,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "浏览器持久状态无法恢复，按无状态重建并覆盖写回"
            );
            return Ok((default_browser_authority(state_root)?, true));
        }
    };
    let mut needs_persist = stripped_transient_tabs;
    if authority
        .profile(&BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID))
        .is_none()
    {
        needs_persist = true;
        authority.register_profile(BrowserProfile {
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: state_root.join("browser/profiles/default"),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        })?;
    }
    Ok((authority, needs_persist))
}

fn browser_authority_api_error(error: BrowserAuthorityError) -> ApiError {
    match error {
        BrowserAuthorityError::UnknownProfile(_)
        | BrowserAuthorityError::UnknownSession(_)
        | BrowserAuthorityError::UnknownTab(_)
        | BrowserAuthorityError::UnknownLease(_) => ApiError::NotFound(error.to_string()),
        BrowserAuthorityError::ProfileAlreadyExists(_)
        | BrowserAuthorityError::SessionAlreadyExists(_)
        | BrowserAuthorityError::OpenSessionAlreadyExists { .. }
        | BrowserAuthorityError::TabAlreadyExists(_)
        | BrowserAuthorityError::LeaseAlreadyExists(_)
        | BrowserAuthorityError::LeaseConflict { .. }
        | BrowserAuthorityError::LeaseNotHeld(_)
        | BrowserAuthorityError::LeaseExpired(_)
        | BrowserAuthorityError::LeaseFenceMismatch { .. }
        | BrowserAuthorityError::TabSessionMismatch { .. }
        | BrowserAuthorityError::GoalBindingMismatch
        | BrowserAuthorityError::LeaseOwnerMismatch
        | BrowserAuthorityError::LeaseTurnMismatch
        | BrowserAuthorityError::SessionMagiSessionMismatch { .. }
        | BrowserAuthorityError::PrimarySurfaceUnavailable(_)
        | BrowserAuthorityError::SurfaceNotPrimary { .. }
        | BrowserAuthorityError::NodeSelectionPageMismatch { .. }
        | BrowserAuthorityError::SnapshotRevisionMismatch { .. }
        | BrowserAuthorityError::NavigationRevisionMismatch { .. }
        | BrowserAuthorityError::NavigationRevisionRegression { .. } => {
            ApiError::Conflict(error.to_string())
        }
        BrowserAuthorityError::SessionTabLimitReached { .. } => {
            ApiError::BrowserSessionTabLimitReached
        }
        BrowserAuthorityError::GlobalTabLimitReached { .. } => {
            ApiError::BrowserGlobalTabLimitReached
        }
        BrowserAuthorityError::InvalidSnapshot(_) => {
            ApiError::InternalAssemblyError(error.to_string())
        }
        _ => ApiError::InvalidInput(error.to_string()),
    }
}

/// 受信扩展宿主的监管状态与生命周期端口（daemon 的 ExtensionRuntime 实现它）。
/// 状态属于宿主监管，插件管理器与 API 都不持有第二份。
pub trait ExtensionStatusSource: Send + Sync {
    /// 沿唯一监管器收敛已提交的激活意图；不更改安装、启停或授权事实。
    fn reconcile(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        Box::pin(async { Err("扩展宿主未提供运行目录收敛端口".into()) })
    }
    fn validate_settings(
        &self,
        _plugin_id: String,
        _value: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        Box::pin(async { Err("扩展宿主未提供设置校验端口".into()) })
    }
    fn status(
        &self,
        plugin_id: &str,
    ) -> Option<magi_app_server_protocol::generated::PluginRuntimeStatus>;
    /// 生命周期切换前关闭宿主准入并等待进程树退出；guard 登记保持到提交后的收敛。
    fn quiesce(
        &self,
        _plugin_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        Box::pin(async { Err("扩展宿主未提供生命周期停止端口".into()) })
    }
}

/// 翻译上游生态插件所需的本机工具链：随包 Node 与兼容运行时目录（含 `cli.mjs`）。
/// 与扩展宿主使用同一份解析结果，API 只据此启动翻译进程，不自行寻找 Node。
#[derive(Clone, Debug)]
pub struct PluginToolchain {
    pub node: std::path::PathBuf,
    pub compat: std::path::PathBuf,
}

/// 插件视图后端路由的执行端口（daemon 的 PluginServices 实现）。
/// 路由处理器运行在受信扩展宿主里；API 只转交请求，不持有插件状态，也不解释路由语义。
pub trait PluginRouteExecutor: Send + Sync {
    /// 把视图请求交给该插件的宿主。调用方（HTTP 请求）被丢弃时，实现应取消宿主侧的调用。
    fn call(
        &self,
        plugin_id: String,
        request: magi_app_server_protocol::generated::PluginRouteCallRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        magi_app_server_protocol::generated::PluginRouteCallResult,
                        ApiError,
                    >,
                > + Send,
        >,
    >;
}

impl ApiState {
    pub(crate) async fn lock_app_server_request(
        &self,
        request_id: &str,
    ) -> AppServerRequestLockGuard {
        let request_id = request_id.trim().to_string();
        debug_assert!(!request_id.is_empty());
        let lock = {
            let mut registry = self
                .app_server_request_locks
                .lock()
                .expect("app server request lock registry poisoned");
            let entry =
                registry
                    .entry(request_id.clone())
                    .or_insert_with(|| AppServerRequestLockEntry {
                        lock: Arc::new(tokio::sync::Mutex::new(())),
                        references: 0,
                    });
            entry.references += 1;
            Arc::clone(&entry.lock)
        };
        let mut lease = AppServerRequestLockGuard {
            registry: Arc::clone(&self.app_server_request_locks),
            request_id,
            _guard: None,
        };
        // lease 在等待期间已经持有引用；若这个 future 被取消，Drop 会回收
        // 引用计数，不会把已取消的 requestId 永久留在注册表中。
        lease._guard = Some(lock.lock_owned().await);
        lease
    }

    pub fn new(
        service_name: impl Into<String>,
        event_bus: Arc<InMemoryEventBus>,
        session_store: Arc<SessionStore>,
        workspace_registry: Arc<WorkspaceStore>,
        governance: Arc<GovernanceService>,
    ) -> Self {
        let plugin_manager =
            magi_plugin_system::PluginManager::open(std::env::temp_dir().join(format!(
                "magi-api-plugin-state-{}-{}",
                std::process::id(),
                UtcMillis::now().0
            )))
            .expect("ApiState plugin manager should initialize");
        let plugin_installations = Arc::new(
            crate::plugin_installations::PluginInstallations::open(&plugin_manager),
        );
        let plugin_manager = Arc::new(Mutex::new(plugin_manager));
        let browser_authority = Arc::new(Mutex::new(BrowserAuthority::new()));
        let browser_write_lock = Arc::new(Mutex::new(()));
        let browser_host_client = Arc::new(RwLock::new(None));
        let execution_resources = ExecutionResourceCoordinator::new(
            Arc::clone(&browser_authority),
            Arc::clone(&browser_write_lock),
            Arc::clone(&browser_host_client),
            Arc::clone(&event_bus),
        );
        let input_event_bus = Arc::clone(&event_bus);
        let canonical_item_event_bus = Arc::clone(&event_bus);
        session_store.install_canonical_item_commit_observer(Arc::new(move |notice| {
            magi_conversation_runtime::session_writeback::publish_committed_canonical_item_event(
                &canonical_item_event_bus,
                notice,
            );
        }));
        session_store.install_input_commit_observer(Arc::new(move |notice| {
            input_event_bus.publish(EventEnvelope::domain(
                magi_core::EventId::unique("session-inbox"), "session.inbox.changed",
                serde_json::json!({ "cursor": magi_app_server_protocol::generated::SessionInboxCursor {
                    epoch: notice.epoch, through_sequence: notice.through_sequence,
                }}),
            ).with_context(EventContext { session_id: Some(notice.session_id),
                workspace_id: notice.workspace_id.map(WorkspaceId::new), ..EventContext::default() }));
        }));
        let model_step_event_bus = Arc::clone(&event_bus);
        session_store.install_model_step_commit_observer(Arc::new(move |notice| {
            magi_conversation_runtime::canonical_usage::publish_model_step_usage(
                &model_step_event_bus,
                &notice.session_id,
                &notice.turn.turn_id,
                &notice.step,
            );
            let context = EventContext {
                session_id: Some(notice.session_id.clone()),
                workspace_id: notice.workspace_id.map(WorkspaceId::new),
                ..EventContext::default()
            };
            model_step_event_bus.publish(EventEnvelope::domain(
                magi_core::EventId::unique("session-model-step"),
                "session.turn.step",
                serde_json::json!({
                    "session_id": notice.session_id,
                    "canonical_turn": notice.turn,
                    "canonical_schema_version": magi_session_store::CANONICAL_TURN_SCHEMA_VERSION,
                    "canonical_event_kind": "model_step_updated",
                    "canonical_model_step": notice.step,
                }),
            ).with_context(context.clone()));
            let Some(goal_id) = notice.step.goal_id else {
                return;
            };
            model_step_event_bus.publish(
                EventEnvelope::domain(
                    magi_core::EventId::unique("session-goal-usage"),
                    "session.goal.usage.changed",
                    serde_json::to_value(
                        magi_app_server_protocol::generated::SessionGoalUsageChanged {
                            goal_id: goal_id.to_string(),
                            step_id: notice.step.step_id,
                        },
                    )
                    .expect("Goal usage notice contains only strings"),
                )
                .with_context(context),
            );
        }));
        let service_name = service_name.into();
        let turn_coordinator = Arc::new(SessionTurnCoordinator::new());
        let conversation_registry = Arc::new(ConversationRegistry::with_turn_coordinator(
            Arc::clone(&turn_coordinator),
        ));
        Self {
            service_info: ServiceInfo {
                service_name,
                api_version: "v0".to_string(),
            },
            daemon_identity: DaemonIdentity::unknown(),
            runtime_epoch: format!(
                "runtime-{}-{}-{}",
                UtcMillis::now().0,
                std::process::id(),
                RUNTIME_EPOCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            event_bus,
            session_store,
            workspace_registry,
            git_service: Arc::new(magi_git::GitService::new()),
            session_code_contexts: magi_git::SessionCodeContextRegistry::default(),
            session_isolations: magi_session_isolation::SessionIsolationRegistry::default(),
            session_workspace_waits: Arc::new(Mutex::new(HashMap::new())),
            goal_start_requests: Arc::new(Mutex::new(HashMap::new())),
            workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator::default(),
            governance,
            knowledge_store: Arc::new(KnowledgeStore::new()),
            settings_store: Arc::new(SettingsStore::new()),
            appearance_library: Arc::new(magi_appearance::AppearanceLibrary::in_memory()),
            runtime_persistence: None,
            canonical_event_next_sequence_provider: None,
            session_state_checkpoint_persist: None,
            session_projection_persist: None,
            unavailable_session_discard: None,
            workspace_history_restore: None,
            execution_pipeline: None,
            task_execution_registry: TaskExecutionRegistry::default(),
            task_store: None,
            task_completion_notifier: None,
            runner_manager: None,
            session_turn_dispatcher: None,
            mcp_connections: Arc::new(RwLock::new(HashMap::new())),
            model_bridge_client: None,
            tool_registry: None,
            browser_authority,
            browser_write_lock,
            browser_control_locks: BrowserControlLocks::default(),
            browser_downloads: Arc::default(),
            app_server_request_locks: Arc::new(Mutex::new(HashMap::new())),
            browser_state_writable: Arc::new(AtomicBool::new(true)),
            browser_host_status: Arc::new(RwLock::new(BrowserHostStatusSnapshot::default())),
            browser_host_connection: Arc::new(watch::channel(None).0),
            browser_host_connection_generation: Arc::new(AtomicU64::new(0)),
            browser_host_owner: Arc::new(Mutex::new(None)),
            browser_host_client,
            browser_host_generation: Arc::new(AtomicU64::new(0)),
            managed_process_group: ManagedProcessGroup::new(),
            execution_resources,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            extension_status: None,
            plugin_route_executor: None,
            plugin_toolchain: None,
            plugin_installations,
            input_guard: None,
            tunnel_manager: crate::tunnel::TunnelManager::new(38123),
            plugin_manager,
            mcp_service: Arc::new(crate::mcp_runtime::McpServiceRuntime::in_memory()),
            plugin_jobs: Arc::new(crate::plugin_jobs::PluginJobs::open(
                std::env::temp_dir().join(format!(
                    "magi-jobs-{}.json",
                    magi_core::EventId::unique("test")
                )),
            )),
            snapshot_manager: Arc::new(SnapshotManager::new()),
            conversation_registry,
            turn_coordinator,
            terminal_sessions: crate::terminal_runtime::TerminalSessionManager::default(),
            agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            role_configuration_lock: Arc::new(Mutex::new(())),
            session_turn_locks: Arc::new(Mutex::new(HashMap::new())),
            session_change_sync_locks: Arc::new(Mutex::new(HashMap::new())),
            session_navigation_lock: Arc::new(tokio::sync::Mutex::new(())),
            project_directory_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 安装 SessionLifecycleObserver，把 session 创建/归档/删除事件桥接到 SnapshotManager。
    ///
    /// 历史 session 不在启动阶段批量建立快照。快照的整树初始化属于变更视图的
    /// 按需资源，启动时批量恢复会把所有 workspace 扫描排进同一条生命周期队列，
    /// 既浪费 IO，也会让用户第一次导航等待与当前会话无关的历史任务。实际访问
    /// 变更视图或执行需要快照的操作时，由 ensure_snapshot_session 懒加载。
    pub fn install_snapshot_lifecycle_observer(&self) {
        let observer = Arc::new(crate::snapshot_lifecycle::SnapshotLifecycleObserver::new(
            self.snapshot_manager.clone(),
        ));
        self.session_store.set_lifecycle_observer(observer);
    }

    /// 同步取 session + workspace 对应的 SnapshotSession。按需资源尚未装载时返回 None；
    /// 需要变更账本的操作必须先经 ensure 入口准备。
    pub fn snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Option<Arc<SnapshotSession>> {
        let workspace_root = self.snapshot_root_for_session(session_id, workspace_root);
        self.snapshot_manager
            .get_session_for_workspace(session_id.as_str(), &workspace_root)
    }

    /// 隔离会话的变更账本建立在隔离副本上；调用方按习惯传入主工作区路径时在这里换成副本路径，
    /// 这样变更面板、MCP、Git 面板等所有按「会话 + 工作区」取账本的入口都自然指向副本。
    pub(crate) fn snapshot_root_for_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> PathBuf {
        let Some(isolation) = self
            .session_isolations
            .get_for_source(session_id.as_str(), workspace_root)
        else {
            return workspace_root.to_path_buf();
        };
        let canonical = std::fs::canonicalize(workspace_root).ok();
        if workspace_root == isolation.source_root
            || canonical.as_deref() == Some(isolation.source_root.as_path())
        {
            isolation.root
        } else {
            workspace_root.to_path_buf()
        }
    }

    pub(crate) async fn ensure_snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Result<Arc<SnapshotSession>, ApiError> {
        let _ = self.session_execution_root_for_source(session_id, workspace_root)?;
        if let Some(session) = self.snapshot_session(session_id, workspace_root) {
            return Ok(session);
        }
        let workspace_root = self.snapshot_root_for_session(session_id, workspace_root);
        self.snapshot_manager
            .start_session(session_id.as_str().to_string(), workspace_root)
            .await
            .map_err(|error| ApiError::internal_assembly("启动会话快照账本失败", error))
    }

    /// 同步 session 变更面板依赖的磁盘状态与 Git baseline。
    ///
    /// `force_reconcile` 用于页面首次打开、窗口重新聚焦及执行变更操作前的强一致读取；
    /// 常规轮询依赖 watcher 增量，只做轻量 Git ref 指纹检查。同分支快进只应用
    /// old HEAD→new HEAD 的 tree patch，绝不把整个 dirty worktree 直接提升为 baseline。
    pub(crate) async fn synchronize_session_changes(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
        force_reconcile: bool,
    ) -> Result<Arc<SnapshotSession>, ApiError> {
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        let snapshot = self
            .ensure_snapshot_session(session_id, workspace_root)
            .await?;
        tracing::info!(%session_id, stage = "changes_snapshot_ready", "workspace execution preparation");
        if force_reconcile {
            snapshot
                .reconcile_async()
                .await
                .map_err(|error| ApiError::internal_assembly("刷新磁盘变更状态失败", error))?;
        }
        tracing::info!(%session_id, stage = "changes_reconciled", "workspace execution preparation");
        // 隔离副本有自己独立的 Git 状态，不与主工作区的 branch/HEAD 对齐。
        if self
            .session_isolations
            .get_for_root(session_id.as_str(), workspace_root)
            .is_some()
        {
            return Ok(snapshot);
        }

        let Some(existing_context) = self
            .session_code_contexts
            .get_for_path(session_id.as_str(), workspace_root)
        else {
            match self.git_service.observe(workspace_root).await {
                Ok(observation) => {
                    tracing::info!(%session_id, stage = "changes_git_observed", "workspace execution preparation");
                    self.align_snapshot_baseline_to_observation_head(
                        workspace_root,
                        &observation,
                        &snapshot,
                    )
                    .await?;
                    tracing::info!(%session_id, stage = "changes_baseline_aligned", "workspace execution preparation");
                    self.session_code_contexts.accept(
                        session_id.as_str(),
                        workspace_id.as_str(),
                        vec![workspace_root.to_path_buf()],
                        &observation,
                    );
                    self.persist_session_git_contexts()?;
                }
                Err(magi_git::GitError::NotRepository { .. }) => {}
                Err(error) => {
                    return Err(ApiError::internal_assembly(
                        "初始化变更面板 Git context 失败",
                        error,
                    ));
                }
            }
            return Ok(snapshot);
        };

        let ref_observation = match self.git_service.observe_ref(workspace_root).await {
            Ok(observation) => observation,
            Err(magi_git::GitError::NotRepository { .. }) => return Ok(snapshot),
            Err(error) => {
                return Err(ApiError::internal_assembly(
                    "刷新变更面板 Git ref 失败",
                    error,
                ));
            }
        };
        let observed_ref_changed = existing_context.git.observed_branch != ref_observation.branch
            || existing_context.git.observed_head != ref_observation.head;
        if !existing_context.has_external_drift() && !observed_ref_changed {
            return Ok(snapshot);
        }

        let observation = self
            .git_service
            .observe(workspace_root)
            .await
            .map_err(|error| ApiError::internal_assembly("刷新变更面板 Git context 失败", error))?;
        let observed_context = self.session_code_contexts.observe(
            session_id.as_str(),
            workspace_id.as_str(),
            existing_context.runtime_workspace_roots.clone(),
            &observation,
        );
        if !observed_context.has_external_drift() {
            self.persist_session_git_contexts()?;
            return Ok(snapshot);
        }

        let safe_fast_forward = self
            .git_service
            .is_session_context_fast_forward(&existing_context, &observation)
            .await
            .map_err(|error| {
                ApiError::internal_assembly("判断变更面板 Git context 是否安全快进失败", error)
            })?;
        if !safe_fast_forward
            || self
                .workspace_git_coordinator
                .session_holds_execution(session_id.as_str(), &existing_context.git.git_common_dir)
        {
            self.persist_session_git_contexts()?;
            if observed_ref_changed {
                self.publish_session_git_drift(
                    session_id,
                    workspace_id,
                    &observed_context,
                    existing_context.git.observed_head.as_deref(),
                    if safe_fast_forward {
                        "fast_forward_pending"
                    } else {
                        "external_drift_detected"
                    },
                );
            }
            return Ok(snapshot);
        }

        self.advance_snapshot_baseline_for_fast_forward(
            session_id,
            workspace_root,
            &existing_context,
            &observation,
            &snapshot,
        )
        .await?;
        let context = self.session_code_contexts.accept(
            session_id.as_str(),
            workspace_id.as_str(),
            existing_context.runtime_workspace_roots.clone(),
            &observation,
        );
        self.persist_session_git_contexts()?;
        if let Some(previous_head) = existing_context.git.base_head.as_deref() {
            self.publish_session_git_fast_forward(
                session_id,
                workspace_id,
                &context,
                previous_head,
            );
        }
        self.schedule_workspace_code_index(workspace_id.clone(), workspace_root.to_path_buf());
        Ok(snapshot)
    }

    async fn advance_snapshot_baseline_for_fast_forward(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
        existing_context: &magi_git::SessionCodeContext,
        observation: &magi_git::GitObservation,
        snapshot: &Arc<SnapshotSession>,
    ) -> Result<(), ApiError> {
        let previous_head = existing_context.git.base_head.as_deref().ok_or_else(|| {
            ApiError::InternalAssemblyError("原 Git baseline HEAD 缺失".to_string())
        })?;
        let current_head = observation.head.as_deref().ok_or_else(|| {
            ApiError::InternalAssemblyError("新 Git baseline HEAD 缺失".to_string())
        })?;
        snapshot
            .reconcile_async()
            .await
            .map_err(|error| ApiError::internal_assembly("Git 快进前刷新磁盘状态失败", error))?;
        let git_patch = self
            .git_service
            .tree_baseline_patch(workspace_root, previous_head, current_head)
            .await
            .map_err(|error| {
                ApiError::internal_assembly("读取 Git 快进 baseline 补丁失败", error)
            })?;
        let snapshot_patch = snapshot_baseline_patch(git_patch);
        snapshot
            .apply_baseline_patch(&snapshot_patch)
            .map_err(|error| {
                tracing::error!(session_id = %session_id, ?error, "推进 session Git baseline 失败");
                ApiError::internal_assembly("推进 session Git baseline 失败", error)
            })?;
        Ok(())
    }

    async fn align_snapshot_baseline_to_observation_head(
        &self,
        workspace_root: &Path,
        observation: &magi_git::GitObservation,
        snapshot: &Arc<SnapshotSession>,
    ) -> Result<(), ApiError> {
        let Some(head) = observation.head.as_deref() else {
            return Ok(());
        };
        snapshot.reconcile_async().await.map_err(|error| {
            ApiError::internal_assembly("Git baseline 校准前刷新磁盘失败", error)
        })?;
        let mut paths = snapshot
            .pending_changes()
            .map_err(|error| ApiError::internal_assembly("读取待校准变更失败", error))?
            .into_iter()
            .flat_map(|change| {
                change
                    .old_path
                    .into_iter()
                    .chain(std::iter::once(change.path))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return Ok(());
        }
        let git_entries = self
            .git_service
            .tree_baseline_entries_for_paths(workspace_root, head, &paths)
            .await
            .map_err(|error| ApiError::internal_assembly("读取 Git HEAD baseline 失败", error))?;
        snapshot
            .apply_baseline_patch(&snapshot_baseline_patch(git_entries))
            .map_err(|error| {
                ApiError::internal_assembly("校准 snapshot Git baseline 失败", error)
            })?;
        Ok(())
    }

    pub fn with_tunnel_port(mut self, port: u16) -> Self {
        self.tunnel_manager = crate::tunnel::TunnelManager::new(port);
        self
    }

    pub fn with_plugin_manager(
        mut self,
        manager: Arc<Mutex<magi_plugin_system::PluginManager>>,
    ) -> Self {
        self.plugin_installations =
            Arc::new(crate::plugin_installations::PluginInstallations::open(
                &manager.lock().expect("plugin manager assembly"),
            ));
        self.plugin_manager = manager;
        // Builder 的装配顺序不能让实时 Skill provider 永久捕获临时 manager。
        if let Some(runtime) = self.skill_runtime.clone() {
            self = self.with_skill_runtime(runtime);
        }
        self
    }

    pub fn with_managed_process_group(mut self, process_group: ManagedProcessGroup) -> Self {
        self.managed_process_group = process_group;
        self
    }

    pub fn terminate_managed_processes(&self) -> usize {
        self.managed_process_group.terminate_all()
    }

    pub fn task_worker_catalog(&self) -> Vec<WorkerInfo> {
        // Runner 同时承接主线 coordinator task 和 agent_spawn 创建的 Worker task。
        // registry 对外的 role template 列表只包含可被 agent_spawn 派发的角色，
        // 因此这里必须显式补入唯一的内部 coordinator，不能依赖 TaskRunner 的
        // 角色不匹配时继续补入唯一 coordinator，否则目标续跑会被错误标记为不可运行。
        let mut role_ids = registered_role_template_ids(self);
        if let Some(coordinator) = self.agent_role_registry.default_coordinator() {
            role_ids.push(coordinator.id);
        }
        build_worker_catalog_for_roles(&self.agent_role_registry, role_ids)
    }

    pub fn with_execution_pipeline(
        mut self,
        task_store: Arc<TaskStore>,
        memory_store: MemoryStore,
    ) -> Self {
        self.execution_pipeline = Some(ExecutionPipeline {
            task_store,
            memory_store,
        });
        self
    }

    pub fn with_tool_registry(mut self, tool_registry: ToolRegistry) -> Self {
        self.execution_resources
            .set_tool_registry(tool_registry.clone());
        self.tool_registry = Some(tool_registry);
        self
    }

    pub fn with_git_context_runtime(
        mut self,
        git_service: Arc<magi_git::GitService>,
        session_code_contexts: magi_git::SessionCodeContextRegistry,
        workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator,
    ) -> Self {
        self.git_service = git_service;
        self.session_code_contexts = session_code_contexts.clone();
        self.workspace_git_coordinator = workspace_git_coordinator.clone();
        self.conversation_registry
            .human_wait()
            .set_hook(Arc::new(HumanWaitGitLeaseHook {
                coordinator: workspace_git_coordinator,
            }));
        self
    }

    pub fn with_snapshot_manager(mut self, snapshot_manager: Arc<SnapshotManager>) -> Self {
        self.snapshot_manager = snapshot_manager;
        self
    }

    pub fn with_agent_role_registry(
        mut self,
        registry: Arc<magi_agent_role::AgentRoleRegistry>,
    ) -> Self {
        self.agent_role_registry = registry;
        self
    }

    /// 任务（含子任务）进入终态：释放它自己的进程、浏览器租约与默认浏览器目标，
    /// 不等整轮结束，避免子代理结束后仍占着页面阻塞主代理。
    pub fn release_finished_task_resources(&self, session_id: &SessionId, task_id: &TaskId) {
        // 已排队但从未进入模型循环的代理也会收到 TaskStore 终态通知，
        // 不能只依赖执行栈退出时回收其初始 assignment。
        self.conversation_registry
            .close_task_signal_channel(session_id, task_id);
        self.cancel_execution_resources(
            Some(session_id),
            None,
            Some(task_id),
            BrowserLeaseEndReason::TaskFinished,
        );
        let executor = crate::browser_tool_runtime::browser_executor_key_for_task(task_id);
        let created_tabs = match self
            .mutate_browser_authority(|authority| Ok(authority.forget_agent_tabs(&executor)))
        {
            Ok(created_tabs) => created_tabs,
            Err(error) => {
                tracing::warn!(%session_id, %task_id, ?error, "清理任务浏览器默认目标失败");
                return;
            }
        };
        // 子代理新开的页面是中间产物，随子任务结束关闭，避免标签页累积到上限；
        // 主线的页面属于用户可继续查看的结果，保留到用户显式关闭。
        let is_child_task = self
            .task_store()
            .and_then(|task_store| task_store.get_task(task_id))
            .is_some_and(|task| task.parent_task_id.is_some());
        if !is_child_task || created_tabs.is_empty() {
            return;
        }
        let state = self.clone();
        spawn_session_turn_work("magi-browser-child-tab-close", async move {
            for tab_id in created_tabs {
                if let Err(error) =
                    crate::browser_tool_runtime::close_browser_tab(&state, &tab_id).await
                {
                    tracing::warn!(%tab_id, ?error, "关闭子任务新开的浏览器标签页失败");
                }
            }
        });
    }

    pub fn cancel_execution_resources(
        &self,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
        task_id: Option<&TaskId>,
        reason: BrowserLeaseEndReason,
    ) -> ExecutionResourceCancellationReport {
        self.execution_resources.cancel(
            ToolExecutionContextQuery {
                session_id: session_id.cloned(),
                workspace_id: workspace_id.cloned(),
                task_id: task_id.cloned(),
                worker_id: None,
                taskless_only: false,
            },
            reason,
            UtcMillis::now(),
        )
    }

    pub(crate) fn cancel_conversation_execution_resources(
        &self,
        session_id: &SessionId,
        reason: BrowserLeaseEndReason,
    ) -> ExecutionResourceCancellationReport {
        self.execution_resources.cancel(
            ToolExecutionContextQuery::for_taskless_session(session_id),
            reason,
            UtcMillis::now(),
        )
    }

    /// 按 TaskStore 当前拥有方的执行范围取消资源；已转交子树及同会话的其他执行
    /// 由各自拥有方结算。Session 关闭使用 session 级资源入口，覆盖全部拥有方。
    /// 即使原 Task 已被收口或移除，仍结算调用方明确指定的 root 身份。
    pub(crate) fn cancel_task_execution_scope_resources(
        &self,
        session_id: &SessionId,
        root_task_id: &TaskId,
        reason: BrowserLeaseEndReason,
    ) -> ExecutionResourceCancellationReport {
        let mut task_ids = self
            .task_store()
            .map(|store| store.collect_execution_subtree_ids(root_task_id))
            .unwrap_or_default();
        if !task_ids.contains(root_task_id) {
            task_ids.push(root_task_id.clone());
        }
        let mut report = ExecutionResourceCancellationReport::default();
        for task_id in task_ids {
            let cancelled =
                self.cancel_execution_resources(Some(session_id), None, Some(&task_id), reason);
            report.tool_execution_count = report
                .tool_execution_count
                .saturating_add(cancelled.tool_execution_count);
            report.browser_lease_count = report
                .browser_lease_count
                .saturating_add(cancelled.browser_lease_count);
        }
        report
    }

    /// Goal 控制版本是现有执行撤销事实。修改/暂停/清除后，主动结算其已转交的
    /// 执行范围与资源；不触及 Session 拥有的分支，也不从主轮当前指针猜测归属。
    pub(crate) async fn settle_revoked_goal_agent_runs(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, ApiError> {
        let Some(store) = self.task_store.clone() else {
            return Ok(0);
        };
        let goals = self.session_store.goals_for_session(session_id);
        let roots = TaskExecutionRegistry::revoked_goal_agent_roots(
            &store,
            &self.session_store,
            session_id,
        );
        let mut errors = Vec::new();
        for root in &roots {
            let manager = self.runner_manager();
            let _restart_guard = match manager {
                Some(manager) => Some(manager.lock_for_restart(root.task_id.as_str()).await),
                None => None,
            };
            let goal_is_paused = root.agent_run_transfer().is_some_and(|transfer| {
                matches!(&transfer.owner, magi_core::AgentRunOwner::Goal { goal_id, .. }
                    if goals.iter().any(|goal| &goal.goal_id == goal_id && goal.status == magi_session_store::GoalStatus::Paused))
            });
            let end_reason = if goal_is_paused {
                BrowserLeaseEndReason::GoalPaused
            } else {
                BrowserLeaseEndReason::OwnerReleased
            };
            self.cancel_task_execution_scope_resources(session_id, &root.task_id, end_reason);
            let registry = self.task_execution_registry.clone();
            let execution_store = store.clone();
            let execution_session = session_id.clone();
            let execution_root = root.task_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                registry.settle_revoked_goal_agent_run(
                    &execution_store,
                    &execution_session,
                    &execution_root,
                )
            })
            .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => errors.push(error),
                Err(error) => errors.push(format!("目标代理结算线程退出：{error}")),
            }

            if let Some(manager) = manager {
                manager.quiesce_for_restart(root.task_id.as_str()).await;
            } else if let Some(dispatcher) = self.session_turn_dispatcher() {
                dispatcher.wait_for_quiesce(&root.task_id).await;
            }
            let ids = store.collect_execution_subtree_ids(&root.task_id);
            if ids.iter().filter_map(|id| store.get_task(id)).any(|task| {
                matches!(
                    task.status,
                    magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                )
            }) || self.tool_registry().is_some_and(|registry| {
                ids.iter().any(|id| {
                    registry.has_active_executions(&ToolExecutionContextQuery {
                        session_id: Some(session_id.clone()),
                        task_id: Some(id.clone()),
                        ..Default::default()
                    })
                })
            }) {
                errors.push(format!("目标代理 {} 尚未完成资源结算", root.task_id));
            }
        }
        if errors.is_empty() {
            Ok(roots.len())
        } else {
            Err(ApiError::Conflict(format!(
                "目标已更新，部分代理结算失败：{}",
                errors.join("；")
            )))
        }
    }

    /// Browser Host 连接断开时的窄域清理。连接断开只代表浏览器自动化暂时
    /// 不可用，不能把它升级为整个会话执行被取消。
    pub fn revoke_browser_execution_resources(
        &self,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
        task_id: Option<&TaskId>,
        reason: BrowserLeaseEndReason,
    ) -> usize {
        self.execution_resources.revoke_browser_leases(
            &ToolExecutionContextQuery {
                session_id: session_id.cloned(),
                workspace_id: workspace_id.cloned(),
                task_id: task_id.cloned(),
                worker_id: None,
                taskless_only: false,
            },
            reason,
            UtcMillis::now(),
        )
    }

    pub fn cancel_browser_surface_control(
        &self,
        tab_id: &BrowserTabId,
        surface_id: &str,
        reason: BrowserLeaseEndReason,
    ) -> Vec<BrowserControlLease> {
        self.execution_resources
            .cancel_surface(tab_id, surface_id, reason, UtcMillis::now())
    }

    pub fn browser_host_status(&self) -> BrowserHostStatusSnapshot {
        self.browser_host_status
            .read()
            .expect("browser host status lock poisoned")
            .clone()
    }

    /// 返回 BrowserAuthority 当前记录的唯一主 Surface。
    ///
    /// Surface 属于桌面运行态，不进入持久化状态；所有 API 和 Host 事件
    /// 都必须从 Authority 读取，不能在各层维护自己的 Surface 缓存。
    pub fn browser_primary_surface_id(&self, tab_id: &BrowserTabId) -> Option<String> {
        self.browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .primary_surface(tab_id)
            .map(|surface| surface.surface_id.clone())
    }

    pub fn browser_host_connection_config(&self) -> Option<BrowserHostConnectionConfig> {
        self.browser_host_connection.borrow().clone()
    }

    pub fn set_browser_host_connection_config(&self, config: Option<BrowserHostConnectionConfig>) {
        if let Some(config) = config.as_ref() {
            let candidate_owner = BrowserHostOwner::from(config);
            let mut owner = self
                .browser_host_owner
                .lock()
                .expect("browser Host owner lock poisoned");
            match owner.as_ref() {
                Some(current) => assert_eq!(
                    current, &candidate_owner,
                    "同一 daemon 生命周期不能切换 Electron Desktop 所有者"
                ),
                None => *owner = Some(candidate_owner),
            }
            self.browser_host_connection_generation
                .fetch_max(config.generation, Ordering::AcqRel);
        } else {
            // 清除连接同时释放 Desktop 所有权。开发 daemon 会被多个 Electron
            // 生命周期复用；如果正常退出只清了 config 却保留 owner，下一次
            // Desktop 永远会收到 409，浏览器能力会永久停在 stopped。
            self.browser_host_owner
                .lock()
                .expect("browser Host owner lock poisoned")
                .take();
        }
        self.browser_host_connection.send_replace(config);
    }

    pub fn browser_host_connection_receiver(
        &self,
    ) -> watch::Receiver<Option<BrowserHostConnectionConfig>> {
        self.browser_host_connection.subscribe()
    }

    pub fn browser_host_connection_generation(&self) -> u64 {
        self.browser_host_connection_generation
            .load(Ordering::Acquire)
    }

    pub fn register_browser_host_connection(
        &self,
        candidate: BrowserHostConnectionConfig,
        expected_generation: u64,
    ) -> Result<(BrowserHostConnectionConfig, bool), BrowserHostConnectionRegistrationError> {
        let candidate_owner = BrowserHostOwner::from(&candidate);
        let mut owner = self
            .browser_host_owner
            .lock()
            .expect("browser Host owner lock poisoned");
        if owner
            .as_ref()
            .is_some_and(|current| current != &candidate_owner)
        {
            return Err(BrowserHostConnectionRegistrationError::OwnerConflict {
                current_generation: self.browser_host_connection_generation(),
            });
        }
        let mut outcome = Ok((candidate.clone(), false));
        self.browser_host_connection.send_if_modified(|current| {
            let next_generation = || {
                self.browser_host_connection_generation
                    .fetch_add(1, Ordering::AcqRel)
                    .saturating_add(1)
            };
            match current {
                Some(existing)
                    if existing.desktop_epoch == candidate.desktop_epoch
                        && existing.parent_pid == candidate.parent_pid
                        && existing.socket_path == candidate.socket_path
                        && existing.auth_token == candidate.auth_token
                        && expected_generation == existing.generation
                        && existing.generation != 0 =>
                {
                    outcome = Ok((existing.clone(), false));
                    false
                }
                Some(existing) if expected_generation == existing.generation => {
                    let mut accepted = candidate.clone();
                    accepted.generation = next_generation();
                    outcome = Ok((accepted.clone(), true));
                    *current = Some(accepted);
                    true
                }
                Some(existing) => {
                    outcome = Err(BrowserHostConnectionRegistrationError::Conflict {
                        current_generation: existing.generation,
                    });
                    false
                }
                None if expected_generation == self.browser_host_connection_generation() => {
                    let mut accepted = candidate.clone();
                    accepted.generation = next_generation();
                    outcome = Ok((accepted.clone(), true));
                    *current = Some(accepted);
                    true
                }
                None => {
                    outcome = Err(BrowserHostConnectionRegistrationError::Conflict {
                        current_generation: self.browser_host_connection_generation(),
                    });
                    false
                }
            }
        });
        if outcome.is_ok() && owner.is_none() {
            *owner = Some(candidate_owner);
        }
        outcome
    }

    pub fn set_browser_host_status(&self, mut status: BrowserHostStatusSnapshot) {
        let mut current = self
            .browser_host_status
            .write()
            .expect("browser host status lock poisoned");
        status.revision = current.revision.saturating_add(1);
        status.settings = current.settings;
        *current = status;
    }

    pub fn update_browser_capability_settings(
        &self,
        settings: BrowserCapabilitySettings,
    ) -> Result<(), ApiError> {
        self.settings_store
            .set_section("browser", settings.to_section())
            .map_err(crate::errors::settings_persistence_error)?;
        let mut current = self
            .browser_host_status
            .write()
            .expect("browser host status lock poisoned");
        current.settings = settings;
        current.revision = current.revision.saturating_add(1);
        Ok(())
    }

    pub fn set_browser_host_client(&self, client: Option<BrowserHostClient>) -> u64 {
        let mut current = self
            .browser_host_client
            .write()
            .expect("browser Host client lock poisoned");
        *current = client;
        self.browser_host_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn browser_host_client(&self) -> Option<BrowserHostClient> {
        self.browser_host_client
            .read()
            .expect("browser Host client lock poisoned")
            .clone()
    }

    /// 清理孤儿 Browser Session。
    ///
    /// 异常退出等情况下，Browser Authority 的持久化状态可能早于对应 Magi 会话被删除或归档，
    /// 这些状态不能继续参与恢复和全局 Tab 容量计算。只保留当前 SessionStore
    /// 中仍为 Active 的 Magi 会话，所有其他打开状态一次性转为 Closed。
    pub fn reconcile_browser_sessions_with_session_store(&self) -> Result<usize, ApiError> {
        let active_session_ids = self
            .session_store
            .sessions()
            .into_iter()
            .filter(|session| session.status == SessionLifecycleStatus::Active)
            .map(|session| session.session_id)
            .collect::<HashSet<_>>();
        let orphan_browser_session_ids = self
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .snapshot()
            .sessions
            .into_iter()
            // App 级会话不属于任何 Magi 会话，只按 owner 的 Session 分支参与孤儿判定
            // （设计基线 §5.3）。
            .filter(|session| {
                session.lifecycle.is_open()
                    && session
                        .magi_session_id()
                        .is_some_and(|session_id| !active_session_ids.contains(session_id))
            })
            .map(|session| session.browser_session_id)
            .collect::<Vec<_>>();
        if orphan_browser_session_ids.is_empty() {
            return Ok(0);
        }

        let now = UtcMillis::now();
        self.mutate_browser_authority(|authority| {
            for browser_session_id in &orphan_browser_session_ids {
                authority.transition_session(
                    browser_session_id,
                    BrowserSessionLifecycle::Closed,
                    now,
                )?;
            }
            Ok(orphan_browser_session_ids.len())
        })
    }

    pub fn browser_host_generation(&self) -> u64 {
        self.browser_host_generation.load(Ordering::Acquire)
    }

    pub fn browser_downloads(&self) -> &Arc<crate::browser_downloads::BrowserDownloadRegistry> {
        &self.browser_downloads
    }

    pub fn browser_tool_runtime_dependencies(&self) -> crate::BrowserToolRuntimeDependencies {
        crate::BrowserToolRuntimeDependencies {
            authority: Arc::clone(&self.browser_authority),
            write_lock: Arc::clone(&self.browser_write_lock),
            control_locks: self.browser_control_locks.clone(),
            downloads: Arc::clone(&self.browser_downloads),
            state_writable: Arc::clone(&self.browser_state_writable),
            host_status: Arc::clone(&self.browser_host_status),
            host_client: Arc::clone(&self.browser_host_client),
            event_bus: Arc::clone(&self.event_bus),
            session_store: Arc::clone(&self.session_store),
            persistence: self.runtime_persistence.clone(),
        }
    }

    pub fn browser_capability_snapshot(&self) -> BrowserCapabilitySnapshot {
        let host = self.browser_host_status();
        BrowserCapabilitySnapshot {
            revision: host.revision,
            in_app_browser_enabled: host.settings.in_app_browser_enabled,
            browser_use_enabled: host.settings.browser_use_enabled,
            devtools_enabled: host.settings.devtools_enabled,
            lan_access_enabled: host.settings.lan_access_enabled,
            host_status: host.status,
            host_protocol_compatible: host.protocol_compatible,
        }
    }

    pub fn runtime_persistence(&self) -> Option<&RuntimeStatePersistence> {
        self.runtime_persistence.as_deref()
    }

    pub fn health_dto(&self) -> HealthDto {
        HealthDto::from_service_info(
            &self.service_info,
            &self.runtime_epoch,
            &self.daemon_identity,
        )
    }

    pub fn runtime_epoch(&self) -> &str {
        &self.runtime_epoch
    }

    pub(crate) fn canonical_event_next_sequence_for(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Option<u64>, ApiError> {
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let Some(provider) = self.canonical_event_next_sequence_provider.as_ref() else {
            return Ok(None);
        };
        provider(session_id)
            .map(Some)
            .map_err(|error| ApiError::internal_assembly("读取 canonical event 游标失败", error))
    }

    pub fn bootstrap_dto_for_workspace_session(
        &self,
        workspace_id: Option<&str>,
        requested_session_id: Option<&SessionId>,
    ) -> Result<BootstrapDto, ApiError> {
        let Some(ws_id) = workspace_id else {
            let event_snapshot = self.event_bus.snapshot();
            let mut projection = self
                .session_store
                .projection_input_for_personal_session(requested_session_id);
            let selected_session_id = projection.current_session_id.clone();
            projection.notifications = self
                .session_store
                .notifications_for_context(&NotificationContext::personal(selected_session_id));
            let mut dto =
                BootstrapDto::from_state_with_session_projection(self, projection, event_snapshot)?;
            dto.scope = crate::dto::SessionScopeKindDto::Personal;
            dto.workspace_id = None;
            dto.workspace_path = None;
            return Ok(dto);
        };
        let event_snapshot = self.event_bus.snapshot();
        let mut projection = self
            .session_store
            .projection_input_for_workspace_session(ws_id, requested_session_id);
        let selected_session_id = projection.current_session_id.clone();
        projection.notifications = self
            .session_store
            .notifications_for_context(&NotificationContext::workspace(ws_id, selected_session_id));
        let mut dto =
            BootstrapDto::from_state_with_session_projection(self, projection, event_snapshot)?;
        dto.scope = crate::dto::SessionScopeKindDto::Workspace;
        dto.workspace_id = Some(ws_id.to_string());
        dto.workspace_path = self
            .workspace_root_path(&Some(WorkspaceId::new(ws_id)))
            .map(|path| path.to_string_lossy().to_string());
        Ok(dto)
    }

    pub(crate) fn session_records_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Vec<SessionRecord> {
        let workspace_id = workspace_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        self.session_store
            .sessions()
            .into_iter()
            .filter(|session| session.workspace_id.as_deref() == workspace_id)
            // 外部工具会话（外部 MCP 客户端）有自己的侧栏入口，不进用户的会话列表。
            .filter(|session| session.kind.is_user())
            .filter(session_has_user_content)
            .collect()
    }

    pub(crate) fn session_workspace_id(&self, session: &SessionRecord) -> Option<WorkspaceId> {
        session.workspace_id.as_deref().map(WorkspaceId::new)
    }

    /// 外部（MCP）工具调用复用与会话内 agent 相同的工具注册表。
    pub(crate) fn tool_registry(&self) -> Option<&ToolRegistry> {
        self.tool_registry.as_ref()
    }

    pub(crate) fn workspace_root_path(
        &self,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        let workspace_id = workspace_id.as_ref()?;
        workspace_root_path_from_registry(self.workspace_registry.as_ref(), workspace_id)
    }

    /// 未绑定项目的个人会话使用 Magi 管理的私有执行目录。
    ///
    /// 这是工具 cwd/权限根目录，不属于项目注册表；文件入口按会话访问它，不继承 Git、变更或项目知识。
    pub(crate) fn personal_session_execution_root_path(&self, session_id: &SessionId) -> PathBuf {
        let state_root = self
            .runtime_persistence()
            .and_then(RuntimeStatePersistence::state_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::temp_dir().join("magi"));
        // 在唯一目录入口解析状态根的物理身份，尚未创建的会话目录只拼接。
        // 接纳、映射和实际工具执行必须共用路径，不能把 /var 别名当作目录替换。
        magi_tool_runtime::canonicalize_tool_permission_path(&state_root)
            .join("personal-sessions")
            .join(session_id.as_str())
    }

    pub(crate) fn personal_session_execution_root(
        &self,
        session_id: &SessionId,
    ) -> Result<PathBuf, ApiError> {
        let root = self.personal_session_execution_root_path(session_id);
        std::fs::create_dir_all(&root)
            .map_err(|error| ApiError::internal_assembly("创建个人会话执行目录失败", error))?;
        Ok(root)
    }

    pub(crate) fn resolve_workspace_id_from_request(
        &self,
        requested_workspace_id: Option<WorkspaceId>,
        requested_workspace_path: Option<&str>,
    ) -> Option<WorkspaceId> {
        if let Some(workspace_id) = self.workspace_id_for_root_path(requested_workspace_path) {
            return Some(workspace_id);
        }
        if let Some(workspace_id) = requested_workspace_id
            && self
                .workspace_root_path(&Some(workspace_id.clone()))
                .is_some()
        {
            return Some(workspace_id);
        }
        None
    }

    pub(crate) fn workspace_id_for_root_path(
        &self,
        requested_workspace_path: Option<&str>,
    ) -> Option<WorkspaceId> {
        let requested_path = requested_workspace_path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .and_then(|path| {
                magi_core::HostPath::resolve_native_input(
                    path,
                    std::env::current_dir().ok().as_deref(),
                    dirs::home_dir().as_deref(),
                )
                .ok()
            })
            .map(magi_core::HostPath::into_path_buf)?;
        let requested_path = canonicalize_path_for_workspace_match(&requested_path);
        self.workspace_registry
            .workspaces()
            .into_iter()
            .find(|workspace| {
                // 旧 workspacePath 在导航和事件订阅中只标识项目，必须识别所有
                // 当前源目录；执行目录仍由 sourceFolderId 和冻结绑定分别校验。
                workspace
                    .source_folder_roots()
                    .into_iter()
                    .any(|stored_path| {
                        canonicalize_path_for_workspace_match(&stored_path) == requested_path
                    })
            })
            .map(|workspace| workspace.workspace_id)
    }

    pub fn runtime_read_model_dto(&self) -> RuntimeReadModelDto {
        runtime_read_model_dto_with_usage(
            self.event_bus.runtime_read_model_input(),
            &self.session_store.execution_sidecar_exports(),
            &self.workspace_registry.recovery_sidecar_exports(),
            self.audit_usage_ledger_dto(),
            self.task_store(),
            &self.ledger_usage_observations(),
        )
    }

    /// 当前 daemon 的执行准入状态。未装配任务 Runner 的最小化 API 状态不会伪造
    /// 运行指标，调用方应将 `None` 视为执行运行时尚未初始化。
    pub fn execution_admission_snapshot(&self) -> Option<ExecutionAdmissionSnapshot> {
        self.runner_manager
            .as_deref()
            .map(RunnerManager::execution_admission_snapshot)
    }

    /// 从已恢复的审计/用量账本回放每会话最近一次用量观测值。
    ///
    /// 重启容错:守护进程重启后 event-bus 的 `recent_events` 缓冲区为空,只有持久化
    /// 账本里仍保有 `model.usage.recorded`。DTO 装配用这份观测值回填按 sidecar 重建
    /// 的会话,使预算在重启后不至于整体丢失。
    pub fn ledger_usage_observations(
        &self,
    ) -> std::collections::BTreeMap<String, magi_event_bus::SessionRuntimeUsageObservation> {
        // 回放本身按 (observed_at, sequence) 取最新，与条目顺序和去重无关，
        // 所以直接在读锁内扫描，不必克隆整本账本（bootstrap 每次都会走这里）。
        self.event_bus.with_audit_usage_ledger(|ledger| {
            latest_usage_observations_from_ledger(&ledger.usage_entries)
        })
    }

    pub fn audit_usage_ledger_dto(&self) -> AuditUsageLedgerDto {
        self.event_bus.runtime_ledger_summary()
    }

    pub fn version_handshake_dto(&self) -> VersionHandshakeDto {
        VersionHandshakeDto::from_service_info(&self.service_info, &self.daemon_identity)
    }

    pub fn with_daemon_identity(mut self, identity: DaemonIdentity) -> Self {
        self.daemon_identity = identity;
        self
    }

    pub fn execution_pipeline(&self) -> Option<&ExecutionPipeline> {
        self.execution_pipeline.as_ref()
    }

    pub fn task_execution_registry(&self) -> &TaskExecutionRegistry {
        &self.task_execution_registry
    }

    pub fn settings_snapshot_json_with_mcp_hydration_and_tool_context(
        &self,
        hydrate_mcp_servers: bool,
        tool_context: &ToolExecutionContext,
    ) -> serde_json::Value {
        let mut snapshot = self.settings_store.public_snapshot();
        normalize_settings_snapshot_sections(&mut snapshot);
        if hydrate_mcp_servers {
            self.enrich_mcp_servers_with_connection_status(&mut snapshot);
        }
        let tool_catalog = self.settings_tool_catalog_json(hydrate_mcp_servers, tool_context);
        let skills_config = public_skills_config_section(object_section(&snapshot, "skillsConfig"));
        let public_mcp_servers = public_mcp_servers_section(&snapshot);
        let audit_ledger = self.audit_usage_ledger_dto();
        // 只统计数量：读锁内直接扫描，不克隆整本账本。
        let safeguard_audit_count = crate::safeguard_audit::visible_count(&self.event_bus);
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workers"),
            "orchestratorConfig": object_section(&snapshot, "orchestrator"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliary"),
            "visionConfig": object_section(&snapshot, "vision"),
            "imageGenerationConfig": object_section(&snapshot, "imageGeneration"),
            "modelContextWindows": object_section(&snapshot, "modelContextWindows"),
            "userRulesConfig": object_section(&snapshot, "userRulesConfig"),
            "skillsConfig": skills_config,
            "safeguardConfig": object_section(&snapshot, "safeguardConfig"),
            "safeguardAudit": {
                "auditCount": safeguard_audit_count,
                "persistenceHealthy": audit_ledger.is_persist_healthy,
                "pendingFlush": audit_ledger.pending_flush,
            },
            "repositories": array_section(&snapshot, "repositories"),
            "mcpServers": public_mcp_servers,
            "builtinTools": self.builtin_tools_json(&tool_catalog),
            "capabilityDependencies": self.capability_dependencies_json(&tool_catalog),
            "workerStatuses": object_section(&snapshot, "workerStatuses"),
            "runtimeSettings": runtime_settings_from_snapshot(&snapshot),
            "roleTemplates": role_templates_for_registry(self.agent_role_registry.as_ref()),
            "registryEngines": crate::model_config::load_registry_engines(self),
            "registryAgents": resolve_registry_agents(self),
            "bootstrapScope": if hydrate_mcp_servers { "full" } else { "core" },
            "mcpServersHydrated": hydrate_mcp_servers,
        })
    }

    fn settings_tool_catalog_json(
        &self,
        include_external_dependencies: bool,
        tool_context: &ToolExecutionContext,
    ) -> serde_json::Value {
        let input = if include_external_dependencies {
            r#"{"include_external":true,"include_mcp_servers":true,"include_agent_roles":false}"#
        } else {
            r#"{"include_external":false,"include_mcp_servers":false,"include_agent_roles":false}"#
        };
        self.tool_catalog_json(input, tool_context)
            .unwrap_or(serde_json::Value::Null)
    }

    pub(crate) fn tool_catalog_json(
        &self,
        input: &str,
        tool_context: &ToolExecutionContext,
    ) -> Result<serde_json::Value, ApiError> {
        let Some(registry) = &self.tool_registry else {
            return Err(ApiError::not_found("工具注册表未配置", "tool_registry"));
        };
        Ok(registry.tool_catalog_value(input, tool_context))
    }

    pub(crate) fn public_tool_catalog_json(
        &self,
        input: &str,
        tool_context: &ToolExecutionContext,
    ) -> Result<serde_json::Value, ApiError> {
        self.tool_catalog_json(input, tool_context)
            .map(|catalog| public_tool_catalog_response_json(&catalog))
    }

    fn builtin_tools_json(&self, tool_catalog: &serde_json::Value) -> serde_json::Value {
        if tool_catalog.is_null() {
            return serde_json::Value::Array(Vec::new());
        }
        let tools = tool_catalog
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|tool| tool.get("public").and_then(serde_json::Value::as_bool) == Some(true))
            .map(|tool| {
                serde_json::json!({
                    "name": tool.get("name").cloned().unwrap_or(serde_json::Value::Null),
                    "category": tool.get("category").cloned().unwrap_or(serde_json::Value::String("uncategorized".to_string())),
                    "riskLevel": tool.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
                    "approvalRequirement": tool.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "effectiveApprovalPolicy": tool.get("effective_approval_policy").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "accessProfileBehavior": tool.get("access_profile_behavior").cloned().unwrap_or(serde_json::Value::String("restricted_allowed".to_string())),
                    "accessMode": tool.get("access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
                    "policyScope": tool.get("policy_scope").cloned().unwrap_or(serde_json::Value::String("fixed".to_string())),
                    "modelCallScope": tool.get("model_call_scope").cloned().unwrap_or(serde_json::Value::String("session_or_task".to_string())),
                    "workspaceRequired": tool.get("workspace_required").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "modelAvailable": tool.get("model_available").cloned().unwrap_or(serde_json::Value::Bool(true)),
                    "inputSensitivePolicy": tool.get("input_sensitive_policy").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "policySummary": tool.get("policy_summary").cloned().unwrap_or(serde_json::Value::String("使用工具默认风险策略".to_string())),
                    "runtimeInternal": tool.get("runtime_internal").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "runtimeStatus": normalize_tool_runtime_status(tool.get("runtime_status")),
                    "runtimeWarnings": warning_markers(tool, "runtime_warnings", "runtime_warning"),
                    "schemaStatus": tool.get("schema_status").cloned().unwrap_or(serde_json::Value::String("ok".to_string())),
                    "schemaWarnings": warning_markers(tool, "schema_warnings", "schema_warning"),
                    "enabled": true,
                })
            })
            .collect::<Vec<_>>();
        serde_json::Value::Array(tools)
    }

    fn capability_dependencies_json(&self, tool_catalog: &serde_json::Value) -> serde_json::Value {
        let dependencies = tool_catalog
            .get("runtime_dependencies")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(normalize_capability_dependency_json)
            .collect::<Vec<_>>();
        serde_json::Value::Array(dependencies)
    }

    pub fn settings_runtime_json(&self) -> serde_json::Value {
        let snapshot = self.settings_store.public_snapshot();
        runtime_settings_from_snapshot(&snapshot)
    }

    pub fn runtime_status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": "running",
            "version": self.service_info.api_version,
        })
    }

    fn enrich_mcp_servers_with_connection_status(
        &self,
        snapshot: &mut HashMap<String, serde_json::Value>,
    ) {
        let Some(servers) = snapshot.get_mut("mcpServers") else {
            return;
        };
        let Some(arr) = servers.as_array_mut() else {
            return;
        };
        for entry in arr.iter_mut() {
            let Some(server_id) = mcp_server_entry_id(entry).map(str::to_string) else {
                continue;
            };
            let enabled = mcp_server_entry_enabled(entry);

            if !enabled {
                let mut pool = self
                    .mcp_connections
                    .write()
                    .expect("mcp connections write lock poisoned");
                pool.remove(&server_id);
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disabled");
                entry.as_object_mut().map(|m| m.remove("error"));
                continue;
            }

            let already_connected = {
                let pool = self
                    .mcp_connections
                    .read()
                    .expect("mcp connections read lock poisoned");
                pool.contains_key(&server_id)
            };

            if already_connected {
                entry["connected"] = serde_json::json!(true);
                entry["health"] = serde_json::json!("connected");
                entry.as_object_mut().map(|m| m.remove("error"));
            } else if build_mcp_config_from_entry(entry).is_some() {
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disconnected");
            } else {
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disconnected");
                entry["error"] = serde_json::json!("mcp_invalid_config");
            }
        }
    }

    pub fn with_knowledge_store(mut self, store: Arc<KnowledgeStore>) -> Self {
        self.knowledge_store = store;
        self
    }

    pub fn with_settings_store(mut self, store: Arc<SettingsStore>) -> Self {
        let browser = store.get_section("browser");
        let mut runtime = self
            .browser_host_status
            .write()
            .expect("browser host status lock poisoned");
        runtime.settings = BrowserCapabilitySettings::from_section(&browser);
        drop(runtime);
        self.settings_store = store;
        self
    }

    pub fn with_appearance_library(
        mut self,
        library: Arc<magi_appearance::AppearanceLibrary>,
    ) -> Self {
        self.appearance_library = library;
        self
    }

    pub fn with_runtime_persistence(mut self, persistence: Arc<RuntimeStatePersistence>) -> Self {
        if let Some(state_root) = persistence.state_root() {
            self.restore_session_isolations_from(&persistence);
            let path = state_root.join("session-git-contexts.json");
            match fs::read(&path) {
                Ok(bytes) => {
                    match serde_json::from_slice::<Vec<magi_git::SessionCodeContext>>(&bytes) {
                        Ok(contexts) => {
                            let released =
                                self.session_code_contexts.restore_after_restart(contexts);
                            if released > 0 {
                                tracing::info!(
                                    released,
                                    "已释放上次运行遗留的子代理 worktree 占用标记"
                                );
                            }
                        }
                        Err(error) => tracing::warn!(
                            path = %path.display(),
                            error = %error,
                            "忽略无法解析的 session Git context 持久化状态"
                        ),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "忽略无法读取的 session Git context 持久化状态"
                ),
            }
        }
        let browser_load = persistence
            .state_root()
            .map(load_browser_authority)
            .transpose();
        if let Some(state_root) = persistence.state_root() {
            self.plugin_jobs = Arc::new(crate::plugin_jobs::PluginJobs::open(
                state_root.join("plugin-jobs.json"),
            ));
            self.mcp_service = Arc::new(crate::mcp_runtime::McpServiceRuntime::load(
                state_root.join("mcp-server.json"),
            ));
            if let Err(error) = self
                .mcp_service
                .migrate_legacy_directory_bindings(&self.workspace_registry)
            {
                tracing::error!(%error, "MCP 旧令牌目录授权迁移失败，保留原令牌与文件；缺少持久目录授权的调用拒绝执行");
            }
        }
        self.runtime_persistence = Some(persistence);
        if let Some(persistence) = self.runtime_persistence.clone() {
            let store = self.knowledge_store.clone();
            store.set_index_persistence_callback(move |store| {
                persistence.schedule_knowledge_store_save(store.clone());
            });
        }
        match browser_load {
            Ok(Some((authority, is_new))) => {
                *self
                    .browser_authority
                    .lock()
                    .expect("browser authority lock poisoned") = authority;
                if is_new && let Err(error) = self.persist_browser_durable_state() {
                    self.browser_state_writable.store(false, Ordering::Release);
                    self.set_browser_host_status(BrowserHostStatusSnapshot {
                        status: BrowserHostStatus::Failed,
                        last_error_code: Some("browser_state_persist_failed".to_string()),
                        ..self.browser_host_status()
                    });
                    tracing::error!(?error, "初始化浏览器持久状态失败");
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.browser_state_writable.store(false, Ordering::Release);
                self.set_browser_host_status(BrowserHostStatusSnapshot {
                    status: BrowserHostStatus::Failed,
                    protocol_compatible: false,
                    last_error_code: Some("browser_state_invalid".to_string()),
                    revision: 0,
                    ..BrowserHostStatusSnapshot::default()
                });
                tracing::error!(error = %error, "浏览器持久状态无效，禁止覆盖原文件");
            }
        }
        self
    }

    pub fn start_plugin_jobs(&self) {
        self.plugin_jobs.start(self.clone());
    }

    /// 按原生 MCP 自己持久化的开关恢复监听，不读取或依赖插件安装/激活状态。
    pub fn start_mcp_service_if_enabled(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            state.mcp_service.start_if_enabled(&state).await;
            state.mcp_service.external_turns.start_maintenance(&state);
        });
    }

    pub fn with_canonical_event_next_sequence_provider(
        mut self,
        provider: CanonicalEventNextSequenceProvider,
    ) -> Self {
        self.canonical_event_next_sequence_provider = Some(provider);
        self
    }

    pub fn persist_browser_durable_state(&self) -> Result<(), ApiError> {
        if !self.browser_state_writable.load(Ordering::Acquire) {
            return Err(ApiError::InternalAssemblyError(
                "浏览器持久状态不可写，原状态文件需要修复".to_string(),
            ));
        }
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        let durable = browser_durable_state_for_persistence(
            &self
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned"),
        );
        persistence.save_json(&state_root.join("browser/state.json"), &durable)
    }

    /// 会话删除时一并删除它产生的浏览器截图等 artifact。
    fn remove_browser_artifacts_for_session(&self, session_id: &SessionId) {
        let Some(state_root) = self
            .runtime_persistence
            .as_ref()
            .and_then(|persistence| persistence.state_root())
        else {
            return;
        };
        let directory = state_root
            .join(crate::browser_tool_runtime::BROWSER_ARTIFACT_DIR)
            .join(session_id.as_str());
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                %session_id,
                path = %directory.display(),
                %error,
                "删除会话浏览器 artifact 失败"
            ),
        }
    }

    pub fn persist_browser_durable_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_browser_durable_state().map_err(|error| {
            public_runtime_persistence_error("browser", BROWSER_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    pub fn mutate_browser_authority<T>(
        &self,
        mutation: impl FnOnce(&mut BrowserAuthority) -> Result<T, BrowserAuthorityError>,
    ) -> Result<T, ApiError> {
        commit_browser_authority_mutation(
            &self.browser_authority,
            &self.browser_write_lock,
            &self.browser_state_writable,
            self.runtime_persistence.as_deref(),
            mutation,
        )
        .map_err(|error| match error {
            BrowserAuthorityCommitError::NotWritable => {
                ApiError::Conflict("浏览器状态文件无效，修复前不能修改浏览器状态".to_string())
            }
            BrowserAuthorityCommitError::Rejected(error) => browser_authority_api_error(error),
            BrowserAuthorityCommitError::Persist(error) => error,
        })
    }

    /// 旧文件只读取迁移一次；完成前保留原文件，崩溃重试按稳定 input 身份去重。
    pub fn restore_regular_session_turn_queues(&self) -> Result<usize, ApiError> {
        if let Some(root) = self
            .runtime_persistence
            .as_ref()
            .and_then(|p| p.state_root())
        {
            let path = root.join("session-turn-queue.json");
            match fs::read(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(ApiError::internal_assembly("读取旧输入队列失败", error)),
                Ok(bytes) => {
                    let turns: Vec<QueuedRegularSessionTurn> = serde_json::from_slice(&bytes)
                        .map_err(|error| {
                            ApiError::internal_assembly("解析旧输入队列失败", error)
                        })?;
                    // 先完整校验再迁移，防止损坏文件留下部分输入。
                    let mut normalized = Vec::new();
                    let mut identities = HashSet::new();
                    let mut requests = HashSet::new();
                    for mut turn in turns {
                        turn.normalize_identity();
                        if !identities.insert((turn.session_id.clone(), turn.queue_id.clone())) {
                            return Err(ApiError::Conflict(
                                "旧输入队列存在重复身份，保留原文件供核对".into(),
                            ));
                        }
                        if turn.request_fingerprint.is_none() {
                            turn.request_fingerprint = Some(
                                turn.request
                                    .request_fingerprint()
                                    .map_err(ApiError::InvalidInput)?,
                            );
                        }
                        if !requests.insert(turn.request.request_id().expect("normalized request"))
                        {
                            return Err(ApiError::Conflict(
                                "旧输入队列存在重复 requestId，保留原文件供核对".into(),
                            ));
                        }
                        if !matches!(
                            turn.route,
                            crate::dto::SessionTurnRouteDto::Chat
                                | crate::dto::SessionTurnRouteDto::Execute
                        ) {
                            return Err(ApiError::Conflict(
                                "旧输入队列投递类型无效，保留原文件供核对".into(),
                            ));
                        }
                        if self.session_store.session(&turn.session_id).is_none() {
                            return Err(ApiError::Conflict(format!(
                                "旧排队输入所属会话不存在: {}",
                                turn.session_id
                            )));
                        }
                        normalized.push(turn);
                    }
                    let backup = root.join("session-turn-queue.migrated.json");
                    if backup.exists()
                        && fs::read(&backup).map_err(|error| {
                            ApiError::internal_assembly("读取迁移备份失败", error)
                        })? != bytes
                    {
                        return Err(ApiError::Conflict(
                            "旧队列迁移备份冲突，原文件已保留".into(),
                        ));
                    }
                    for turn in normalized {
                        self.enqueue_regular_session_turn(turn)?;
                    }
                    fs::rename(&path, &backup).map_err(|error| {
                        ApiError::internal_assembly("保留旧队列迁移备份失败", error)
                    })?;
                }
            }
        }
        for input in self.session_store.all_session_inputs() {
            if input.state == magi_session_store::SessionInputState::Pending {
                let turn: QueuedRegularSessionTurn = serde_json::from_value(input.payload.clone())
                    .map_err(|error| ApiError::internal_assembly("持久输入请求损坏", error))?;
                if turn.session_id != input.session_id
                    || turn.queue_id != input.input_id
                    || turn.request.request_id().as_deref() != Some(input.request_id.as_str())
                    || turn.request.user_message_id().as_deref()
                        != Some(input.user_message_id.as_str())
                    || turn.request_fingerprint.as_deref()
                        != Some(input.request_fingerprint.as_str())
                    || turn.request.interrupt_turn_id.as_deref()
                        != input
                            .interruption
                            .as_ref()
                            .map(|value| value.turn_id.as_str())
                {
                    return Err(ApiError::Conflict(
                        "持久输入与原始请求身份不一致，拒绝执行".into(),
                    ));
                }
            }
        }
        let inputs = self.session_store.all_session_inputs();
        for mut input in inputs {
            if input.state == magi_session_store::SessionInputState::Pending
                && !self
                    .session_store
                    .session(&input.session_id)
                    .is_some_and(|s| s.status == SessionLifecycleStatus::Active)
            {
                let revision = input.revision;
                input.revision += 1;
                input.state = magi_session_store::SessionInputState::Cancelled;
                self.session_store
                    .commit_input(input, Some(revision))
                    .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
            }
        }
        Ok(self
            .session_store
            .all_session_inputs()
            .iter()
            .filter(|input| input.state == magi_session_store::SessionInputState::Pending)
            .count())
    }

    pub fn persist_session_git_contexts(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        persistence.save_json(
            &state_root.join("session-git-contexts.json"),
            &self.session_code_contexts.all(),
        )
    }

    pub fn with_session_state_checkpoint_persist(
        mut self,
        persist: SessionStateCheckpointPersist,
    ) -> Self {
        self.session_state_checkpoint_persist = Some(persist);
        self
    }

    pub fn with_session_projection_persist(mut self, persist: SessionProjectionPersist) -> Self {
        self.session_projection_persist = Some(persist);
        self
    }

    pub fn with_unavailable_session_discard(mut self, discard: UnavailableSessionDiscard) -> Self {
        self.unavailable_session_discard = Some(discard);
        self
    }

    pub fn with_workspace_history_restore(mut self, restore: WorkspaceHistoryRestore) -> Self {
        self.workspace_history_restore = Some(restore);
        self
    }

    pub(crate) fn restore_workspace_history_for_api(
        &self,
        project: &magi_workspace::WorkspaceRecord,
    ) -> Result<(), ApiError> {
        if let Some(restore) = &self.workspace_history_restore {
            restore(project)?;
        }
        Ok(())
    }

    pub fn persist_session_state_checkpoint(&self, checkpoint: &str) -> Result<(), ApiError> {
        let durable_persisted = if let Some(persist) = &self.session_state_checkpoint_persist {
            persist(checkpoint)?
        } else {
            false
        };
        if !durable_persisted {
            self.persist_session_projection()?;
        }
        self.persist_session_git_contexts()
    }

    /// 本轮首次工作区执行前重新观测 session 绑定的 Git branch/HEAD/worktree。
    ///
    /// 同仓库、同 worktree、同分支的线性快进会自动成为新基线，保证提交、拉取或其他
    /// 正常协作操作不会中断后续对话。分支切换、HEAD 回退或历史改写仍需用户明确接受。
    pub async fn ensure_session_code_context(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<Option<magi_git::SessionCodeContext>, ApiError> {
        let Some(workspace_id) = workspace_id else {
            return Ok(None);
        };
        // 旧入口无法表达目录身份，多根时必须由调用方指定源目录再执行。
        let (_, workspace_root) = self
            .workspace_registry
            .resolve_source_folder(workspace_id, None)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        self.ensure_session_code_context_for_root(session_id, workspace_id, &workspace_root)
            .await
    }

    pub(crate) async fn ensure_session_code_context_for_root(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        workspace_root: &Path,
    ) -> Result<Option<magi_git::SessionCodeContext>, ApiError> {
        let workspace_root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        if let Some(isolation) = self
            .session_isolations
            .get_for_source(session_id.as_str(), &workspace_root)
        {
            if isolation.source_root != workspace_root || !isolation.root.is_dir() {
                return Err(ApiError::Conflict(
                    "隔离副本与原目录绑定不匹配或已丢失".into(),
                ));
            }
            return Ok(None);
        } else if self.session_isolations.contains(session_id.as_str()) {
            return Err(ApiError::Conflict(
                "源文件夹缺少会话隔离映射，不能回落原目录".into(),
            ));
        }
        let _change_sync_guard = self.lock_session_change_sync(session_id).await;
        tracing::info!(%session_id, stage = "change_sync_acquired", "workspace execution preparation");
        let session_key = session_id.as_str();
        let existing_context = self
            .session_code_contexts
            .get_for_path(session_key, &workspace_root);
        if existing_context.as_ref().is_some_and(|context| {
            std::fs::canonicalize(&context.execution_root)
                .unwrap_or_else(|_| context.execution_root.clone())
                != workspace_root
                && !context.runtime_workspace_roots.iter().any(|path| {
                    std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()) == workspace_root
                })
        }) {
            return Err(ApiError::Conflict(
                "会话 Git 上下文属于另一个源文件夹，不能重用目录绑定".into(),
            ));
        }
        if let Some(existing_context) = existing_context.as_ref() {
            self.begin_session_git_execution(session_id, &existing_context.git.git_common_dir)
                .await?;
        }
        let mut observation = match self.git_service.observe(&workspace_root).await {
            Ok(observation) => observation,
            Err(magi_git::GitError::NotRepository { .. }) => {
                // 普通源目录没有 Git 资源；同会话其它获授仓库的租约仍在使用。
                return Ok(None);
            }
            Err(error) => {
                self.release_session_git_execution_lease(session_id);
                return Err(ApiError::InternalAssemblyError(format!(
                    "每轮执行前校验 Git context 失败: {error}"
                )));
            }
        };
        tracing::info!(%session_id, stage = "git_observed", "workspace execution preparation");
        if existing_context.is_none() {
            // 首次观测只用于确定需要哪把 repository lease。等到执行权后重新观测，
            // 再建立快照基线，不能记录另一个会话尚未完成写入的中间状态。
            self.begin_session_git_execution(session_id, &observation.git_common_dir)
                .await?;
            tracing::info!(%session_id, stage = "git_lease_acquired", "workspace execution preparation");
            let leased_common_dir = observation.git_common_dir.clone();
            observation = match self.git_service.observe(&workspace_root).await {
                Ok(observation) if observation.git_common_dir == leased_common_dir => observation,
                Ok(_) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(ApiError::Conflict(
                        "等待执行期间工作区 Git repository 已变化，请重新核对".into(),
                    ));
                }
                Err(error) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(ApiError::internal_assembly(
                        "取得执行租约后观测 Git 失败",
                        error,
                    ));
                }
            };
            let snapshot = match self
                .ensure_snapshot_session(session_id, &observation.worktree_path)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(error);
                }
            };
            tracing::info!(%session_id, stage = "git_snapshot_loaded", "workspace execution preparation");
            if let Err(error) = self
                .align_snapshot_baseline_to_observation_head(
                    &observation.worktree_path,
                    &observation,
                    &snapshot,
                )
                .await
            {
                self.release_session_git_execution_lease(session_id);
                return Err(error);
            }
        }
        let mut context = if existing_context.is_some() {
            self.session_code_contexts.observe(
                session_key,
                workspace_id.as_str(),
                vec![workspace_root.clone()],
                &observation,
            )
        } else {
            self.session_code_contexts.accept(
                session_key,
                workspace_id.as_str(),
                vec![workspace_root.clone()],
                &observation,
            )
        };
        let mut adopted_fast_forward_from = None;
        if context.has_external_drift()
            && let Some(existing_context) = existing_context.as_ref()
            && match self
                .git_service
                .is_session_context_fast_forward(existing_context, &observation)
                .await
            {
                Ok(is_fast_forward) => is_fast_forward,
                Err(error) => {
                    tracing::warn!(
                        session_id = %existing_context.session_id,
                        previous_head = ?existing_context.git.base_head,
                        current_head = ?observation.head,
                        ?error,
                        "判断 session Git context 是否安全快进失败，保持显式确认"
                    );
                    false
                }
            }
        {
            let snapshot = match self
                .ensure_snapshot_session(session_id, &observation.worktree_path)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.release_session_git_execution_lease(session_id);
                    return Err(error);
                }
            };
            if let Err(error) = self
                .advance_snapshot_baseline_for_fast_forward(
                    session_id,
                    &observation.worktree_path,
                    existing_context,
                    &observation,
                    &snapshot,
                )
                .await
            {
                self.release_session_git_execution_lease(session_id);
                return Err(error);
            }
            context = self.session_code_contexts.accept(
                session_key,
                workspace_id.as_str(),
                context.runtime_workspace_roots.clone(),
                &observation,
            );
            adopted_fast_forward_from = existing_context.git.base_head.clone();
        }
        if context.has_external_drift() {
            self.release_session_git_execution_lease(session_id);
            self.persist_session_git_contexts()?;
            return Err(ApiError::Conflict(format!(
                "Git context 发生高风险变化：期望 branch={:?} HEAD={:?}，实际 branch={:?} HEAD={:?}。分支切换、HEAD 回退或历史改写需要明确接受新基线，或切回原基线后重试",
                context.git.desired_ref,
                context.git.base_head,
                context.git.observed_branch,
                context.git.observed_head
            )));
        }
        if !context.git.dirty.conflicted_paths.is_empty() {
            self.release_session_git_execution_lease(session_id);
            self.persist_session_git_contexts()?;
            return Err(ApiError::Conflict(format!(
                "当前 workspace 存在未解决的 Git merge conflict，必须先解决或 abort 后再执行新一轮：{}",
                context.git.dirty.conflicted_paths.join(", ")
            )));
        }
        if let Err(error) = self.persist_session_git_contexts() {
            self.release_session_git_execution_lease(session_id);
            return Err(error);
        }
        if let Some(previous_head) = adopted_fast_forward_from {
            self.publish_session_git_fast_forward(
                session_id,
                workspace_id,
                &context,
                &previous_head,
            );
            self.schedule_workspace_code_index(
                workspace_id.clone(),
                observation.worktree_path.clone(),
            );
        }
        Ok(Some(context))
    }

    /// 占用 repository 的独占执行租约；仓库正被别的会话执行时排队等待，而不是让这一轮失败。
    ///
    /// 同一 repository 的主 session 共享 live worktree，必须串行执行。这一轮已经被接受，
    /// 若在这里直接报冲突，用户的消息就变成一条失败的 Turn，目标续跑则会被记成运行时
    /// 错误而受阻。只有这个会话自己确有一轮正在启动（current turn 未终态）时才排队等待；
    /// 其它场景（例如用户手动触发的 Git 操作）保持立即失败。等待按先后顺序交接，
    /// 轮次被取消（终态）后放弃等待。
    async fn begin_session_git_execution(
        &self,
        session_id: &SessionId,
        git_common_dir: &std::path::Path,
    ) -> Result<(), ApiError> {
        self.begin_session_git_execution_set(session_id, &[git_common_dir.to_path_buf()], None)
            .await
    }

    pub(crate) async fn begin_session_git_execution_set(
        &self,
        session_id: &SessionId,
        git_common_dirs: &[PathBuf],
        expected_turn_id: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut announced = false;
        let result = loop {
            if let Some(turn_id) = expected_turn_id
                && !self
                    .session_store
                    .runtime_sidecar(session_id)
                    .and_then(|sidecar| sidecar.current_turn)
                    .is_some_and(|turn| {
                        turn.turn_id == turn_id
                            && self
                                .session_store
                                .canonical_turn_for_session_turn_id(session_id, turn_id)
                                .is_some_and(|turn| !turn.status.is_terminal())
                    })
            {
                self.workspace_git_coordinator
                    .cancel_waiter(session_id.as_str());
                break Err(ApiError::Conflict(
                    "工作区准备目标 Turn 已关闭或变化".into(),
                ));
            }
            match self
                .workspace_git_coordinator
                .begin_execution_set(session_id.as_str(), git_common_dirs)
            {
                Ok(()) => break Ok(()),
                Err(
                    error @ (magi_git::GitCoordinationError::ExecutionActive { .. }
                    | magi_git::GitCoordinationError::MutationActive { .. }),
                ) => {
                    let turn_is_starting = matches!(
                        self.session_store
                            .ensure_current_turn_acceptance_available(session_id),
                        Err(DomainError::CurrentTurnConflict { .. })
                    );
                    if !turn_is_starting {
                        self.workspace_git_coordinator
                            .cancel_waiter(session_id.as_str());
                        break Err(ApiError::Conflict(error.to_string()));
                    }
                    for root in git_common_dirs {
                        self.workspace_git_coordinator
                            .register_waiter(session_id.as_str(), root);
                    }
                    if !announced {
                        announced = true;
                        let blocking: Vec<String> = match &error {
                            magi_git::GitCoordinationError::ExecutionActive {
                                active_session_ids,
                                ..
                            } => active_session_ids.clone(),
                            _ => Vec::new(),
                        };
                        tracing::info!(
                            session_id = %session_id,
                            reason = %error,
                            "repository 正被其它 session 执行，本轮排队等待"
                        );
                        self.publish_session_workspace_wait(session_id, true, &blocking);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                Err(error) => {
                    self.workspace_git_coordinator
                        .cancel_waiter(session_id.as_str());
                    break Err(ApiError::Conflict(error.to_string()));
                }
            }
        };
        if announced {
            self.publish_session_workspace_wait(session_id, false, &[]);
        }
        result
    }

    /// 会话的这一轮开始 / 结束等待别的会话释放工作区。前端据此在运行指示里说明「正在等谁」，
    /// 而不是让用户对着一个没有输出的「运行中」干等。
    fn publish_session_workspace_wait(
        &self,
        session_id: &SessionId,
        waiting: bool,
        blocking_session_ids: &[String],
    ) {
        let event_name = if waiting {
            "session.workspace.waiting"
        } else {
            "session.workspace.ready"
        };
        {
            let mut waits = self
                .session_workspace_waits
                .lock()
                .expect("session workspace waits lock poisoned");
            if waiting {
                waits.insert(
                    session_id.as_str().to_string(),
                    blocking_session_ids.to_vec(),
                );
            } else {
                waits.remove(session_id.as_str());
            }
        }
        let workspace_id = self
            .session_store
            .session(session_id)
            .and_then(|session| session.workspace_id)
            .map(WorkspaceId::new);
        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("{event_name}-{session_id}")),
                event_name,
                serde_json::json!({
                    "session_id": session_id,
                    "workspace_id": workspace_id,
                    "blocking_session_ids": blocking_session_ids,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id,
                ..EventContext::default()
            }),
        );
    }

    fn publish_session_git_fast_forward(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        context: &magi_git::SessionCodeContext,
        previous_head: &str,
    ) {
        self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("workspace-git-context-changed-{workspace_id}")),
                "workspace.git.context.changed",
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "session_id": session_id,
                    "repository_root": context.git.repository_root,
                    "worktree_path": context.git.worktree_path,
                    "branch": context.git.observed_branch,
                    "head": context.git.observed_head,
                    "previous_head": previous_head,
                    "context_revision": context.context_revision,
                    "change_kind": "fast_forward_adopted",
                    "refresh_scopes": ["changes", "file_tree", "code_index", "knowledge", "context_cache"]
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    fn publish_session_git_drift(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        context: &magi_git::SessionCodeContext,
        previous_head: Option<&str>,
        change_kind: &str,
    ) {
        self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("workspace-git-context-changed-{workspace_id}")),
                "workspace.git.context.changed",
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "session_id": session_id,
                    "repository_root": context.git.repository_root,
                    "worktree_path": context.git.worktree_path,
                    "branch": context.git.observed_branch,
                    "head": context.git.observed_head,
                    "previous_head": previous_head,
                    "context_revision": context.context_revision,
                    "change_kind": change_kind,
                    "refresh_scopes": ["changes", "file_tree", "code_index", "knowledge", "context_cache"]
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                ..EventContext::default()
            }),
        );
    }

    pub fn release_session_git_execution_lease(&self, session_id: &SessionId) {
        self.workspace_git_coordinator
            .end_execution(session_id.as_str());
    }

    pub fn release_session_git_execution_lease_for_repository(
        &self,
        session_id: &SessionId,
        git_common_dir: &std::path::Path,
    ) {
        self.workspace_git_coordinator
            .end_execution_for_repository(session_id.as_str(), git_common_dir);
    }

    /// 删除 session 时只回收干净的 agent worktree；任何未提交改动都保留目录与 context，
    /// 禁止用 `--force` 静默丢失代理产物。
    async fn cleanup_session_git_resources(&self, session_id: &SessionId) -> bool {
        self.release_session_git_execution_lease(session_id);
        let personal = self
            .session_code_contexts
            .all()
            .into_iter()
            .filter(|context| {
                context.session_id == session_id.as_str() && context.workspace_id.is_empty()
            })
            .collect::<Vec<_>>();
        if !personal.is_empty() {
            let Some(state_root) = self
                .runtime_persistence()
                .and_then(RuntimeStatePersistence::state_root)
            else {
                return false;
            };
            let managed_root = state_root
                .join("worktrees/personal")
                .join(session_id.as_str());
            let managed_root = managed_root.canonicalize().unwrap_or(managed_root);
            for context in personal {
                let Ok(_lease) = self
                    .workspace_git_coordinator
                    .begin_mutation(&context.git.git_common_dir)
                else {
                    tracing::warn!(%session_id, "个人会话仓库正在使用，保留 Git 资源与执行目录");
                    return false;
                };
                let worktrees = match self
                    .git_service
                    .worktree_list(&context.execution_root)
                    .await
                {
                    Ok(worktrees) => worktrees,
                    Err(error) => {
                        tracing::warn!(%session_id, %error, "无法核对个人会话 Git worktree，保留目录");
                        return false;
                    }
                };
                for worktree in worktrees
                    .into_iter()
                    .filter(|tree| tree.path.starts_with(&managed_root))
                {
                    if let Err(error) = self
                        .git_service
                        .worktree_remove(
                            &context.execution_root,
                            magi_git::WorktreeRemoveOptions {
                                path: worktree.path,
                                force: false,
                                confirm_force: false,
                                precondition: magi_git::GitPrecondition::default(),
                            },
                        )
                        .await
                    {
                        tracing::warn!(%session_id, %error, "个人会话删除时保留无法安全回收的 worktree 与仓库目录");
                        return false;
                    }
                }
            }
            self.session_code_contexts.remove(session_id.as_str());
            if let Err(error) = self.persist_session_git_contexts() {
                tracing::warn!(%session_id, ?error, "持久化个人 Git 资源清理失败");
            }
            return true;
        }
        let mut cleanup_failed = false;
        for context in self.session_code_contexts.for_session(session_id.as_str()) {
            for agent in &context.agent_worktrees {
                if !agent.path.exists() {
                    continue;
                }
                if let Err(error) = self
                    .git_service
                    .worktree_remove(
                        &context.git.worktree_path,
                        magi_git::WorktreeRemoveOptions {
                            path: agent.path.clone(),
                            force: false,
                            confirm_force: false,
                            precondition: context.precondition(),
                        },
                    )
                    .await
                {
                    cleanup_failed = true;
                    tracing::warn!(
                        session_id = %session_id,
                        task_id = %agent.task_id,
                        worktree_path = %agent.path.display(),
                        %error,
                        "session 删除时保留无法安全回收的 agent worktree"
                    );
                }
            }
        }
        if !cleanup_failed {
            self.session_code_contexts.remove(session_id.as_str());
        }
        if let Err(error) = self.persist_session_git_contexts() {
            tracing::warn!(session_id = %session_id, ?error, "持久化 session Git 资源清理失败");
        }
        !cleanup_failed
    }

    pub fn persist_session_projection(&self) -> Result<(), ApiError> {
        let Some(persist) = &self.session_projection_persist else {
            return Ok(());
        };
        self.session_store
            .persist_projection_with(|durable, sidecars| {
                persist(durable, sidecars, SessionProjectionPersistMode::Full)
            })
    }

    /// 只提交指定会话的 projection（以及 current 指针等全局小文件）。
    ///
    /// `persist_session_projection` 每次都要重新序列化**全部**会话的历史来比对摘要，会话越多越慢
    /// （本仓库实测新建一个会话要 14 秒），而且是在会话存储的锁内执行，会让整个 daemon 卡住。
    /// 已知只有哪几个会话变了时，都应该用这个。
    pub fn persist_session_projection_for_sessions(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), ApiError> {
        let Some(persist) = &self.session_projection_persist else {
            return Ok(());
        };
        self.session_store
            .persist_sessions_projection_with(session_ids, |durable, sidecars| {
                persist(
                    durable,
                    sidecars,
                    SessionProjectionPersistMode::Sessions {
                        session_ids: session_ids.to_vec(),
                    },
                )
            })
    }

    pub fn persist_session_projection_for_sessions_for_api(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), ApiError> {
        self.persist_session_projection_for_sessions(session_ids)
            .map_err(|error| {
                public_runtime_persistence_error("session", SESSION_PERSISTENCE_PUBLIC_ERROR, error)
            })
    }

    /// 在这个通知上下文里可见的通知，分别属于哪些会话。
    ///
    /// 通知操作（已读 / 清除 / 删除 / 解决）只会改动「在该上下文可见」的通知；会话级通知存放在
    /// 所属会话的 projection 里，应用级和项目级通知存放在各自的 meta 文件里。所以操作前取一次
    /// 这个集合，就知道要重新提交哪些会话，其余会话不用动。
    pub(crate) fn sessions_with_notifications_in_context(
        &self,
        context: &magi_session_store::NotificationContext,
    ) -> Vec<SessionId> {
        let mut session_ids = Vec::new();
        for notification in self.session_store.notifications_for_context(context) {
            if let Some(session_id) = notification.session_id
                && !session_ids.contains(&session_id)
            {
                session_ids.push(session_id);
            }
        }
        session_ids
    }

    pub(crate) fn rename_session_with_persistence_for_api(
        &self,
        session_id: &SessionId,
        title: impl Into<String>,
    ) -> Result<SessionRecord, ApiError> {
        let persist = self.session_projection_persist.clone();
        self.session_store
            .rename_session_with_persistence(session_id, title, |durable, sidecars| {
                if let Some(persist) = &persist {
                    persist(
                        durable,
                        sidecars,
                        SessionProjectionPersistMode::Sessions {
                            session_ids: vec![session_id.clone()],
                        },
                    )
                } else {
                    Ok(())
                }
            })
            .map_err(|error| match error {
                magi_session_store::SessionMutationTransactionError::Domain(
                    magi_core::DomainError::Validation { message },
                ) => ApiError::InvalidInput(message),
                magi_session_store::SessionMutationTransactionError::Domain(error) => {
                    ApiError::internal_assembly("重命名会话失败", error)
                }
                magi_session_store::SessionMutationTransactionError::Persistence(error) => error,
            })
    }

    /// 在 WorkspaceStore 的管理事务中调用，快照由持有写锁的原拥有方提供，
    /// 不重新读取 registry，也不先发布尚未落盘的内存事实。
    pub(crate) fn persist_workspace_snapshot(
        &self,
        snapshot: &magi_workspace::WorkspaceDurableState,
    ) -> magi_core::DomainResult<()> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence
            .save_json(&persistence.workspace_path, snapshot)
            .map_err(|error| magi_core::DomainError::Persistence {
                message: format!("{error:?}"),
            })
    }

    pub fn persist_workspace_durable_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_workspace_store(&self.workspace_registry)
    }

    pub fn persist_workspace_durable_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_workspace_durable_state().map_err(|error| {
            public_runtime_persistence_error("workspace", WORKSPACE_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    pub fn persist_knowledge_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_knowledge_store(&self.knowledge_store)
    }

    pub(crate) fn schedule_workspace_code_index(
        &self,
        workspace_id: WorkspaceId,
        workspace_root: PathBuf,
    ) -> bool {
        let Some(project) = self.workspace_registry.project(&workspace_id) else {
            return false;
        };
        let root = canonicalize_path_for_workspace_match(&workspace_root);
        let source = self
            .session_isolations
            .all()
            .into_iter()
            .find(|isolation| {
                isolation.workspace_id == workspace_id.as_str()
                    && canonicalize_path_for_workspace_match(&isolation.root) == root
            })
            .map(|isolation| isolation.source_root)
            .unwrap_or_else(|| root.clone());
        let Some(folder) = project
            .source_folders
            .iter()
            .find(|folder| canonicalize_path_for_workspace_match(&folder.native_path()) == source)
        else {
            // 不能将无法归属的副本索引写入项目的旧单根槽位。
            return false;
        };
        let isolated = source != root;
        let index_store = if project.source_folders.len() == 1 && !isolated {
            self.knowledge_store.as_ref().clone()
        } else {
            let Ok(index_store) = self
                .knowledge_store
                .for_source_folder(&folder.source_folder_id, &root)
            else {
                return false;
            };
            index_store
        };
        self.schedule_code_index(workspace_id, root, index_store)
    }

    pub(crate) fn schedule_code_index(
        &self,
        workspace_id: WorkspaceId,
        workspace_root: PathBuf,
        index_store: KnowledgeStore,
    ) -> bool {
        if !index_store.begin_workspace_index_build(&workspace_id) {
            return false;
        }
        let state = self.clone();
        tokio::spawn(async move {
            let build_store = index_store.clone();
            let build_workspace_id = workspace_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                build_store.build_workspace_index(&build_workspace_id, &workspace_root)
            })
            .await;
            if let Err(error) = result {
                tracing::warn!(workspace_id = %workspace_id, scope = ?index_store.code_index_scope(), error = %error, "后台代码索引构建失败");
            }
            if index_store.finish_workspace_index_build(&workspace_id)
                && let Err(error) = state.persist_knowledge_state()
            {
                tracing::warn!(workspace_id = %workspace_id, error = ?error, "已取消的代码索引清理结果持久化失败");
            }
        });
        true
    }

    pub fn persist_knowledge_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_knowledge_state().map_err(|error| {
            public_runtime_persistence_error("knowledge", KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    /// 运行态整体持久化：先 workspace 注册事实，再引用它的 session projection（只提交指定会话），
    /// 最后 knowledge。保持 workspace 先于引用它的 session projection 落盘。
    pub fn persist_runtime_durable_state_for_sessions_for_api(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), ApiError> {
        self.persist_workspace_durable_state_for_api()?;
        self.persist_session_projection_for_sessions_for_api(session_ids)?;
        self.persist_knowledge_state_for_api()?;
        Ok(())
    }

    /// 会话导航只改变当前会话指针和工作区选择。
    ///
    /// 导航处于全局串行锁内，不能把无关的 knowledge 快照同步写盘，否则
    /// 本地知识库增长后，普通切换会话会被拖成一次长耗时请求。知识状态有
    /// 独立的生命周期和持久化入口，不属于导航提交事务。
    pub fn persist_session_navigation_state_for_api(
        &self,
        target_session_id: Option<SessionId>,
    ) -> Result<(), ApiError> {
        let Some(persist) = &self.session_projection_persist else {
            self.persist_workspace_durable_state_for_api()?;
            return Ok(());
        };
        // 导航只需要当前指针与目标会话的切片，不能走会整体克隆全部会话历史的通用入口。
        self.session_store
            .persist_navigation_projection_with(target_session_id.as_ref(), |durable, sidecars| {
                persist(
                    durable,
                    sidecars,
                    SessionProjectionPersistMode::Navigation {
                        target_session_id: target_session_id.clone(),
                    },
                )
            })
            .map_err(|error| {
                public_runtime_persistence_error("session", SESSION_PERSISTENCE_PUBLIC_ERROR, error)
            })?;
        self.persist_workspace_durable_state_for_api()?;
        Ok(())
    }

    pub fn with_task_store(mut self, store: Arc<TaskStore>) -> Self {
        let source = Arc::downgrade(&store);
        self.session_store.set_goal_task_activity_resolver(Arc::new(
            move |root, session, goal, tasks| {
                source
                    .upgrade()
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "目标任务状态查询不可用".into(),
                    })?
                    .goal_has_active_bound_tasks(root, session, goal, tasks)
            },
        ));
        let source = Arc::downgrade(&store);
        self.session_store
            .set_agent_run_transfer_resolver(Arc::new(move |task| {
                source
                    .upgrade()
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "Agent 运行拥有方查询不可用".into(),
                    })?
                    .agent_run_transfer_for_task(task)
            }));
        self.task_store = Some(store);
        self
    }

    pub fn with_task_completion_notifier(mut self, notifier: Arc<TaskCompletionNotifier>) -> Self {
        self.task_completion_notifier = Some(notifier);
        self
    }

    pub fn task_completion_notifier(&self) -> Option<&Arc<TaskCompletionNotifier>> {
        self.task_completion_notifier.as_ref()
    }

    pub fn with_task_execution_registry(mut self, registry: TaskExecutionRegistry) -> Self {
        self.task_execution_registry = registry;
        self
    }

    pub fn task_store(&self) -> Option<&TaskStore> {
        self.task_store.as_deref()
    }

    pub(crate) fn has_active_session_tool_execution(&self, session_id: &SessionId) -> bool {
        self.tool_registry.as_ref().is_some_and(|registry| {
            registry.has_active_executions(&ToolExecutionContextQuery {
                session_id: Some(session_id.clone()),
                ..Default::default()
            })
        })
    }

    /// 构造一次性 Turn 事实写回边界。SessionStore 的 canonical mutation 必须
    /// 通过该 sink 完成，事件发布只能发生在 mutation 成功之后。
    pub fn turn_event_sink(&self) -> CanonicalTurnEventSink<'_> {
        CanonicalTurnEventSink::new(
            self.session_store.as_ref(),
            self.event_bus.as_ref(),
            self.task_store(),
        )
    }

    pub fn with_runner_manager(mut self, manager: RunnerManager) -> Self {
        self.runner_manager = Some(Arc::new(manager));
        self
    }

    pub fn with_shared_runner_manager(mut self, manager: Arc<RunnerManager>) -> Self {
        self.runner_manager = Some(manager);
        self
    }

    pub fn with_session_turn_dispatcher(mut self, dispatcher: Arc<LlmTaskDispatcher>) -> Self {
        let execution = Arc::downgrade(&dispatcher);
        let resources = self.execution_resources.clone();
        self.task_execution_registry
            .set_agent_subtree_settler(move |session, tasks| {
                let queries = tasks
                    .iter()
                    .map(|task| ToolExecutionContextQuery {
                        session_id: Some(session.clone()),
                        task_id: Some(task.clone()),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                for query in &queries {
                    resources.cancel(
                        query.clone(),
                        BrowserLeaseEndReason::TaskFinished,
                        UtcMillis::now(),
                    );
                }
                let execution = execution
                    .upgrade()
                    .ok_or("代理执行器已不可用，无法核对线程退出")?;
                // 这是取消结算的观察期限，不限制任务的正常执行时长。
                execution
                    .wait_for_task_dispatches_blocking(tasks, std::time::Duration::from_secs(30))?;
                if resources
                    .tool_registry
                    .read()
                    .expect("execution resource tool registry lock poisoned")
                    .as_ref()
                    .is_some_and(|registry| {
                        queries
                            .iter()
                            .any(|query| registry.has_active_executions(query))
                    })
                {
                    return Err("代理仍有未退出的工具或进程，取消已接纳但资源尚未结算".into());
                }
                Ok(())
            });
        self.session_turn_dispatcher = Some(dispatcher);
        self
    }

    pub fn session_turn_dispatcher(&self) -> Option<&Arc<LlmTaskDispatcher>> {
        self.session_turn_dispatcher.as_ref()
    }

    pub fn turn_coordinator(&self) -> &Arc<magi_conversation_runtime::SessionTurnCoordinator> {
        &self.turn_coordinator
    }

    /// 从 SessionStore 的 canonical Turn projection 恢复 Coordinator 的轻量身份索引。
    ///
    /// canonical event 是重启后的唯一事实来源；这里只恢复 requestId、fingerprint、
    /// profile、attempt 和生命周期状态，不复制 Turn 正文，也不会自动重新发起上游
    /// Provider 请求。活动 Turn 的执行是否可继续由各 profile 的恢复调度器决定。
    pub fn restore_turn_coordinator_from_session_store(&self) -> usize {
        let mut restored = 0;
        for session_id in self.session_store.session_index() {
            let mut turns = self.session_store.canonical_turns_for_session(&session_id);
            // 先恢复最新 Turn：Coordinator 只允许一个 active 槽位，旧的异常
            // 非终态事实不能覆盖当前 canonical 轮次。终态 Turn 仍会全部进入
            // request replay 索引，供重试返回原 receipt。
            turns.sort_by(|left, right| {
                right
                    .turn_seq
                    .cmp(&left.turn_seq)
                    .then_with(|| right.accepted_at.0.cmp(&left.accepted_at.0))
                    .then_with(|| right.turn_id.cmp(&left.turn_id))
            });
            for turn in turns {
                let Some(command) =
                    SessionTurnCoordinator::recover_command_for_canonical_turn(&session_id, &turn)
                else {
                    continue;
                };
                if matches!(
                    self.turn_coordinator.execute_command(&session_id, command),
                    Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
                ) {
                    restored += 1;
                }
            }
        }
        restored
    }

    pub(crate) async fn lock_session_turn(
        &self,
        session_id: &SessionId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .session_turn_locks
                .lock()
                .expect("session turn locks should hold");
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    /// 项目管理同时冻结已有会话的接纳边界。只尝试取得锁，不能在持有导航锁时
    /// 等待执行结算（执行结算可能需要导航），也不能阻塞其它项目的控制面。
    pub(crate) fn try_lock_session_turn_commit(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTurnCommitGuard, ApiError> {
        let lock = self
            .session_turn_locks
            .lock()
            .expect("session turn locks should hold")
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let guard = lock.try_lock_owned().map_err(|_| {
            ApiError::Conflict("项目会话正在提交控制状态，请稍后重试项目修改".into())
        })?;
        Ok(SessionTurnCommitGuard {
            _session_turn_guard: guard,
        })
    }

    pub(crate) async fn lock_session_navigation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.session_navigation_lock.lock().await
    }

    fn project_directory_lock(&self, id: &WorkspaceId) -> Arc<tokio::sync::RwLock<()>> {
        self.project_directory_locks
            .lock()
            .expect("project directory locks")
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
            .clone()
    }

    pub(crate) fn try_project_directory_admission(
        &self,
        id: &WorkspaceId,
    ) -> Result<tokio::sync::OwnedRwLockReadGuard<()>, ApiError> {
        self.project_directory_lock(id)
            .try_read_owned()
            .map_err(|_| ApiError::Conflict("项目源文件夹正在修改，请稍后重试".into()))
    }

    pub(crate) fn try_project_directory_edit(
        &self,
        id: &WorkspaceId,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, ApiError> {
        self.project_directory_lock(id)
            .try_write_owned()
            .map_err(|_| ApiError::Conflict("项目请求正在接纳或目录正在修改，请稍后重试".into()))
    }

    /// 取得指定 session 的 Turn 提交锁。
    ///
    /// 该锁不包含全局导航锁。导航只在修改当前会话指针或创建新会话的短提交中
    /// 显式取得，避免后台执行生命周期把所有会话切换串行化。
    pub(crate) async fn lock_session_turn_commit(
        &self,
        session_id: &SessionId,
    ) -> SessionTurnCommitGuard {
        let session_turn_guard = self.lock_session_turn(session_id).await;
        SessionTurnCommitGuard {
            _session_turn_guard: session_turn_guard,
        }
    }

    /// 在调用方已经持有 session Turn 锁后取得 Runner 生命周期锁。
    ///
    /// 该入口只允许被已持有 session Turn 锁的路径调用，避免 Continue、
    /// 新会话回滚等流程再次获取 Turn 锁而形成自锁。
    pub(crate) async fn lock_runner_lifecycle_after_turn_commit(
        &self,
        session_id: &SessionId,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let manager = self.runner_manager()?.clone();
        Some(manager.lock_session_lifecycle(session_id).await)
    }

    /// 取得完整执行生命周期提交锁：session Turn -> Runner lifecycle。
    ///
    /// 这里明确不取得全局导航锁。导航锁只属于当前会话指针的短提交，不能被
    /// 后台 preparation、Runner 启动或模型执行带出控制面。
    pub(crate) async fn lock_session_lifecycle(
        &self,
        session_id: &SessionId,
    ) -> SessionLifecycleCommitGuard {
        let session_turn_guard = self.lock_session_turn(session_id).await;
        let runner_lifecycle_guard = self
            .runner_manager()
            .cloned()
            .map(|manager| async move { manager.lock_session_lifecycle(session_id).await });
        let runner_lifecycle_guard = match runner_lifecycle_guard {
            Some(lock) => Some(lock.await),
            None => None,
        };
        SessionLifecycleCommitGuard {
            _session_turn_guard: session_turn_guard,
            _runner_lifecycle_guard: runner_lifecycle_guard,
        }
    }

    /// 回滚一个尚未成功接纳首条消息的新会话。
    ///
    /// 调用方必须已经持有 navigation 锁。新会话创建会自动改写 current，
    /// 因此删除资源后必须显式恢复创建前的 current，并将删除与恢复作为同一次
    /// 完整 projection 提交持久化。创建提交可能已经把新 session projection 写入磁盘，
    /// 回滚必须同时清理该 projection。Turn 与 Runner 锁在这里按统一顺序补齐。
    pub(crate) async fn rollback_created_session_after_navigation_lock(
        &self,
        session_id: &SessionId,
        previous_current_session_id: Option<SessionId>,
    ) -> Result<(), ApiError> {
        let _session_turn_guard = self.lock_session_turn(session_id).await;
        let _runner_lifecycle_guard = self
            .lock_runner_lifecycle_after_turn_commit(session_id)
            .await;
        self.delete_session_and_resources_after_lifecycle_lock(
            session_id,
            previous_current_session_id,
        )
        .await
    }

    pub(crate) async fn lock_session_change_sync(
        &self,
        session_id: &SessionId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .session_change_sync_locks
                .lock()
                .expect("session change sync locks should hold");
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    pub(crate) fn enqueue_regular_session_turn(
        &self,
        mut turn: QueuedRegularSessionTurn,
    ) -> Result<usize, ApiError> {
        use magi_session_store::{SessionInput, SessionInputState};
        turn.normalize_identity();
        let fingerprint = match &turn.request_fingerprint {
            Some(fingerprint) => fingerprint.clone(),
            None => turn
                .request
                .request_fingerprint()
                .map_err(ApiError::InvalidInput)?,
        };
        turn.request_fingerprint = Some(fingerprint.clone());
        let request_id = turn.request.request_id().expect("normalized request ID");
        let user_message_id = turn
            .request
            .user_message_id()
            .expect("normalized user item ID");
        let existing = self
            .session_store
            .inputs_for_session(&turn.session_id)
            .into_iter()
            .find(|input| input.input_id == turn.queue_id || input.request_id == request_id);
        if let Some(existing) = existing {
            let compatible_binding = matches!(
                existing.state,
                magi_session_store::SessionInputState::Attached
                    | magi_session_store::SessionInputState::Consumed
            );
            if (existing.input_id != turn.queue_id && !compatible_binding)
                || existing.session_id != turn.session_id
                || existing.request_id != request_id
                || existing.user_message_id != user_message_id
                || existing.request_fingerprint != fingerprint
            {
                return Err(ApiError::Conflict("输入身份或请求指纹冲突".into()));
            }
            return Ok(self.queued_regular_session_turn_count(&turn.session_id));
        }
        let bound = self
            .session_store
            .canonical_turns_for_session(&turn.session_id)
            .into_iter()
            .find(|canonical| {
                crate::turn_service::canonical_turn_matches_queued_regular_turn(canonical, &turn)
            });
        let active = self
            .session_store
            .session(&turn.session_id)
            .is_some_and(|s| s.status == SessionLifecycleStatus::Active);
        let interruption = turn.request.interrupt_turn_id.as_ref().map(|turn_id| {
            magi_session_store::InputInterruption {
                turn_id: turn_id.clone(),
                root_task_id: self
                    .session_store
                    .active_execution_chain(&turn.session_id)
                    .filter(|chain| chain.current_turn_id.as_ref() == Some(turn_id))
                    .map(|chain| chain.root_task_id),
                settled_at: None,
                error: None,
            }
        });
        self.session_store
            .commit_input(
                SessionInput {
                    input_id: turn.queue_id.clone(),
                    session_id: turn.session_id.clone(),
                    request_id,
                    user_message_id,
                    request_fingerprint: fingerprint,
                    accepted_at: turn.accepted_at,
                    revision: 1,
                    state: if bound.is_some() {
                        SessionInputState::Attached
                    } else if active {
                        SessionInputState::Pending
                    } else {
                        SessionInputState::Cancelled
                    },
                    attached_turn_id: bound.map(|turn| turn.turn_id),
                    consumption: None,
                    interruption,
                    payload: serde_json::to_value(&turn)
                        .map_err(|error| ApiError::internal_assembly("编码输入失败", error))?,
                },
                None,
            )
            .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
        Ok(self
            .queued_regular_session_turns(&turn.session_id)
            .iter()
            .position(|queued| queued.queue_id == turn.queue_id)
            .map_or(0, |position| position + 1))
    }

    pub(crate) fn peek_next_regular_session_turn(
        &self,
        session_id: &SessionId,
    ) -> Option<QueuedRegularSessionTurn> {
        self.queued_regular_session_turns(session_id)
            .into_iter()
            .find(QueuedRegularSessionTurn::requires_execution)
    }

    pub(crate) fn queued_regular_session_turns(
        &self,
        session_id: &SessionId,
    ) -> Vec<QueuedRegularSessionTurn> {
        let mut inputs = self.session_store.inbox_snapshot(session_id).0;
        // 明确的停止并改做先于普通下一轮输入；原 FIFO 输入继续保留。
        inputs.sort_by_key(|input| {
            (
                input.interruption.is_none(),
                input.accepted_at,
                input.input_id.clone(),
            )
        });
        inputs
            .into_iter()
            .map(|input| {
                serde_json::from_value(input.payload).expect("validated queued input payload")
            })
            .collect()
    }

    pub(crate) fn queued_regular_session_turn_for_request_id(
        &self,
        request_id: &str,
    ) -> Option<(QueuedRegularSessionTurn, usize)> {
        self.queued_regular_session_ids()
            .into_iter()
            .find_map(|session_id| {
                self.queued_regular_session_turns(&session_id)
                    .into_iter()
                    .enumerate()
                    .find(|(_, turn)| {
                        turn.request.request_id().as_deref() == Some(request_id.trim())
                    })
                    .map(|(index, turn)| (turn, index + 1))
            })
    }

    pub(crate) fn remove_regular_session_turn(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<bool, ApiError> {
        let Some(mut input) = self
            .session_store
            .inputs_for_session(session_id)
            .into_iter()
            .find(|input| input.input_id == queue_id)
        else {
            return Ok(false);
        };
        // 已绑定的输入不能再被取回编辑或删除。
        if matches!(
            input.state,
            magi_session_store::SessionInputState::Attached
                | magi_session_store::SessionInputState::Consumed
        ) {
            return Err(ApiError::Conflict("输入已被领取，不能取回或删除".into()));
        }
        if input.state != magi_session_store::SessionInputState::Pending {
            return Ok(false);
        }
        let revision = input.revision;
        input.revision += 1;
        input.state = magi_session_store::SessionInputState::Cancelled;
        self.session_store
            .commit_input(input, Some(revision))
            .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
        Ok(true)
    }

    pub(crate) fn record_regular_session_turn_retry(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<u8, ApiError> {
        let mut input = self
            .session_store
            .inputs_for_session(session_id)
            .into_iter()
            .find(|input| {
                input.input_id == queue_id
                    && input.state == magi_session_store::SessionInputState::Pending
            })
            .ok_or_else(|| ApiError::Conflict("输入已被领取或撤销".into()))?;
        let mut turn: QueuedRegularSessionTurn = serde_json::from_value(input.payload.clone())
            .map_err(|error| ApiError::internal_assembly("解码输入失败", error))?;
        turn.retry_count = turn.retry_count.saturating_add(1);
        let retry_count = turn.retry_count;
        let revision = input.revision;
        input.revision += 1;
        input.payload = serde_json::to_value(turn)
            .map_err(|error| ApiError::internal_assembly("编码输入失败", error))?;
        self.session_store
            .commit_input(input, Some(revision))
            .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
        Ok(retry_count)
    }

    pub(crate) fn queued_regular_session_turn_count(&self, session_id: &SessionId) -> usize {
        self.queued_regular_session_turns(session_id)
            .iter()
            .filter(|turn| turn.requires_execution())
            .count()
    }
    pub(crate) fn queued_regular_session_ids(&self) -> Vec<SessionId> {
        self.session_store.pending_input_session_ids()
    }

    pub(crate) fn clear_regular_session_turn_queue_for_lifecycle(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, ApiError> {
        let queued = self.queued_regular_session_turns(session_id);
        for turn in &queued {
            self.remove_regular_session_turn(session_id, &turn.queue_id)?;
        }
        Ok(queued.len())
    }

    /// 会话删除的唯一资源回收入口。先停止并等待后台 runner，再删除所有运行态与
    /// 持久化事实，最后删除 SessionStore 主记录，避免任何组件保留孤儿状态。
    pub async fn delete_session_and_resources(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ApiError> {
        let _lifecycle_guard = self.lock_session_lifecycle(session_id).await;
        let replacement = self.replacement_session_for_delete(session_id);
        self.delete_session_and_resources_after_lifecycle_lock(session_id, replacement)
            .await
    }

    /// 在调用方已经持有 `navigation -> Turn -> Runner lifecycle` 后删除 session。
    ///
    /// 该入口让删除路由可以把替代 current 选择和最终持久化继续放在同一个
    /// navigation 临界区内；不能从未持有完整生命周期锁的路径调用。
    pub(crate) async fn delete_session_and_resources_after_lifecycle_lock(
        &self,
        session_id: &SessionId,
        replacement_session_id: Option<SessionId>,
    ) -> Result<(), ApiError> {
        let mut personal_root = self
            .session_store
            .session(session_id)
            .filter(|session| self.session_workspace_id(session).is_none())
            .map(|_| self.personal_session_execution_root_path(session_id));
        let job_outputs = self
            .session_store
            .background_jobs_for_session(session_id)
            .map_err(|error| ApiError::internal_assembly("读取会话 Job 输出归属失败", error))?;
        let manager = self.runner_manager();
        crate::turn_service::cancel_active_session_turn_for_lifecycle(self, session_id);
        if let Some(manager) = manager {
            manager
                .unbind_session_after_lifecycle_lock(session_id)
                .await
                .map_err(|error| ApiError::internal_assembly("删除会话时停止执行失败", error))?;
        }
        self.close_browser_session_for_magi_session(session_id)
            .await?;
        self.remove_browser_artifacts_for_session(session_id);
        self.terminal_sessions
            .close_for_session(session_id.as_str());
        self.cleanup_session_isolation(session_id).await;
        if personal_root.is_none() {
            self.cleanup_session_git_resources(session_id).await;
        }
        self.settings_store
            .remove_session(session_id)
            .map_err(crate::errors::settings_persistence_error)?;
        self.clear_regular_session_turn_queue_for_lifecycle(session_id)?;

        let mut mission_ids = HashSet::new();
        if let Some(thread) = self
            .session_store
            .orchestrator_thread_for_session(session_id)
        {
            mission_ids.insert(thread.mission_id);
        }
        if let Some(ownership) = self.session_store.execution_ownership(session_id)
            && let Some(mission_id) = ownership.mission_id
        {
            mission_ids.insert(mission_id);
        }
        if let Some(sidecar) = self.session_store.runtime_sidecar(session_id)
            && let Some(chain) = sidecar.active_execution_chain
        {
            mission_ids.insert(chain.mission_id);
        }

        let mut task_ids = self
            .session_store
            .execution_task_ids_for_session(session_id)
            .into_iter()
            .collect::<HashSet<_>>();
        task_ids.extend(self.task_execution_registry.remove_session(session_id));
        if let Some(task_store) = self.task_store() {
            for task_id in task_ids.clone() {
                if let Some(task) = task_store.get_task(&task_id) {
                    mission_ids.insert(task.mission_id);
                }
            }
            for mission_id in mission_ids {
                let removed = task_store
                    .remove_tasks_by_mission(&mission_id)
                    .map_err(|error| {
                        ApiError::internal_assembly("删除会话任务 checkpoint 失败", error)
                    })?;
                task_ids.extend(removed.into_iter().map(|task| task.task_id));
            }
            for task_id in task_ids.clone() {
                task_store.remove_task(&task_id).map_err(|error| {
                    ApiError::internal_assembly("删除会话残留任务 checkpoint 失败", error)
                })?;
            }
        }
        self.conversation_registry.remove_session(session_id);
        self.session_store
            .delete_session_with_persistence(
                session_id,
                replacement_session_id.as_ref(),
                |durable, sidecars| {
                    if let Some(persist) = &self.session_projection_persist {
                        persist(
                            durable,
                            sidecars,
                            SessionProjectionPersistMode::Sessions {
                                session_ids: replacement_session_id.iter().cloned().collect(),
                            },
                        )
                    } else {
                        Ok(())
                    }
                },
            )
            .map_err(|error| match error {
                magi_session_store::SessionMutationTransactionError::Domain(error) => {
                    ApiError::internal_assembly("删除会话失败", error)
                }
                magi_session_store::SessionMutationTransactionError::Persistence(error) => error,
            })?;
        if let Some(root) = self
            .runtime_persistence
            .as_ref()
            .and_then(|persistence| persistence.state_root())
        {
            for job in job_outputs {
                let directory = root.join("job-output").join(&job.job_id);
                if job.stdout_path != directory.join("stdout.log")
                    || job.stderr_path != directory.join("stderr.log")
                {
                    continue;
                }
                if let Err(error) = fs::remove_dir_all(&directory)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%session_id, job_id = %job.job_id, %error, "删除会话 Job 输出失败");
                }
            }
        }
        // 只在会话删除已持久提交后回收自有执行目录。失败不能先删除用户成果。
        if personal_root.is_some() && !self.cleanup_session_git_resources(session_id).await {
            personal_root = None;
        }
        if let Some(root) = personal_root {
            let result = tokio::task::spawn_blocking(move || fs::remove_dir_all(root)).await;
            if let Ok(Err(error)) = result
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%session_id, %error, "删除个人会话文件失败");
            }
        }
        self.session_turn_locks
            .lock()
            .expect("session turn locks should hold")
            .remove(session_id);
        self.session_change_sync_locks
            .lock()
            .expect("session change sync locks should hold")
            .remove(session_id);
        Ok(())
    }

    /// 删除一个启动时无法加载的会话。它没有进入会话状态，没有运行态、任务或锁需要回收，
    /// 只需移除持久化文件与会话私有数据；调用方负责持有该会话的生命周期锁。
    pub(crate) async fn discard_unavailable_session_after_lifecycle_lock(
        &self,
        session_id: &SessionId,
    ) -> Result<magi_session_store::UnavailableSession, ApiError> {
        let discard = self.unavailable_session_discard.as_ref().ok_or_else(|| {
            ApiError::internal_assembly("删除不可用会话失败", "daemon 未注入不可用会话清理入口")
        })?;
        let entry = discard(session_id)?;
        self.session_store.forget_unavailable_session(session_id);
        self.settings_store
            .remove_session(session_id)
            .map_err(crate::errors::settings_persistence_error)?;
        // 无项目会话的私有执行目录：与正常删除同一规则，dirty/占用的 Git 资源保留成果。
        if entry.workspace_id.is_none() && self.cleanup_session_git_resources(session_id).await {
            let root = self.personal_session_execution_root_path(session_id);
            let result = tokio::task::spawn_blocking(move || fs::remove_dir_all(root)).await;
            if let Ok(Err(error)) = result
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%session_id, %error, "删除不可用个人会话文件失败");
            }
        }
        Ok(entry)
    }

    fn replacement_session_for_delete(&self, session_id: &SessionId) -> Option<SessionId> {
        if self.session_store.current_session_id().as_ref() != Some(session_id) {
            return None;
        }
        let target_workspace = self
            .session_store
            .session(session_id)
            .and_then(|session| session.workspace_id);
        self.session_store
            .sessions()
            .into_iter()
            .filter(|session| {
                &session.session_id != session_id
                    && session.workspace_id == target_workspace
                    && session.kind.is_user()
            })
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
            })
            .map(|session| session.session_id)
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_skill_runtime(
        mut self,
        skill_runtime: Arc<magi_skill_runtime::SkillRuntime>,
    ) -> Self {
        let manager = self.plugin_manager.clone();
        skill_runtime
            .registry()
            .set_live_provider(Arc::new(move || {
                let manifests = manager
                    .lock()
                    .ok()
                    .and_then(|manager| manager.manifests().ok())
                    .unwrap_or_default();
                manifests
                    .into_iter()
                    .flat_map(|manifest| {
                        let plugin_id = manifest.id;
                        manifest.contributions.skills.into_iter().map(move |skill| {
                            magi_skill_runtime::SkillDefinition {
                                description: skill.description,
                                skill_id: magi_plugin_system::ContributionRef::new(
                                    &plugin_id, skill.id,
                                )
                                .identity(),
                                title: skill.title,
                                instruction: skill.instruction,
                                metadata: magi_skill_runtime::SkillMetadata {
                                    category: "plugin".into(),
                                    tags: vec![plugin_id.clone()],
                                },
                                restrict_standard_tools: false,
                                allowed_tools: vec![],
                                custom_tool_bindings: vec![],
                                prompt_priority: 50,
                            }
                        })
                    })
                    .collect()
            }));
        self.skill_runtime = Some(skill_runtime);
        self
    }

    pub fn with_extension_status(mut self, source: Arc<dyn ExtensionStatusSource>) -> Self {
        self.extension_status = Some(source);
        self
    }

    pub fn with_plugin_toolchain(mut self, toolchain: PluginToolchain) -> Self {
        self.plugin_toolchain = Some(toolchain);
        self
    }

    pub fn with_optional_plugin_toolchain(mut self, toolchain: Option<PluginToolchain>) -> Self {
        self.plugin_toolchain = toolchain;
        self
    }

    pub(crate) fn managed_process_group(&self) -> &ManagedProcessGroup {
        &self.managed_process_group
    }

    pub fn with_plugin_route_executor(mut self, executor: Arc<dyn PluginRouteExecutor>) -> Self {
        self.plugin_route_executor = Some(executor);
        self
    }

    pub fn with_input_guard(
        mut self,
        guard: Arc<dyn magi_conversation_runtime::extension_hooks::InputGuard>,
    ) -> Self {
        self.input_guard = Some(guard);
        self
    }

    pub fn with_skill_dispatch_runtime(
        mut self,
        skill_dispatch_runtime: Arc<magi_skill_runtime::SkillDispatchRuntime>,
    ) -> Self {
        self.skill_dispatch_runtime = Some(skill_dispatch_runtime);
        self
    }

    pub fn with_mcp_connections(
        mut self,
        mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
    ) -> Self {
        self.mcp_connections = mcp_connections;
        self
    }

    pub fn runner_manager(&self) -> Option<&RunnerManager> {
        self.runner_manager.as_deref()
    }

    pub fn mcp_connections(&self) -> &Arc<RwLock<HashMap<String, Arc<McpServerClient>>>> {
        &self.mcp_connections
    }

    pub fn model_bridge_client(&self) -> Option<&Arc<dyn ModelBridgeClient>> {
        self.model_bridge_client.as_ref()
    }

}

fn normalize_settings_snapshot_sections(snapshot: &mut HashMap<String, serde_json::Value>) {
    skill_loader::normalize_skills_config_sections(snapshot);
    seed_user_rules_config(snapshot);
    normalize_mcp_servers_section(snapshot);
    seed_default_safeguard_rules(snapshot);
    normalize_safeguard_config_section(snapshot);
}

fn public_skills_config_section(value: serde_json::Value) -> serde_json::Value {
    let mut config = value.as_object().cloned().unwrap_or_default();
    if let Some(serde_json::Value::Array(skills)) = config.get_mut("instructionSkills") {
        skills.retain(skill_loader::instruction_skill_source_available);
        for skill in skills {
            if let Some(object) = skill.as_object_mut() {
                object.remove("directoryPath");
            }
        }
    }
    serde_json::Value::Object(config)
}

fn normalize_capability_dependency_json(raw: &serde_json::Value) -> serde_json::Value {
    let mut normalized = serde_json::Map::from_iter([
        (
            "name".to_string(),
            raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        ),
        (
            "status".to_string(),
            raw.get("status")
                .cloned()
                .unwrap_or(serde_json::Value::String("unknown".to_string())),
        ),
        (
            "requiredBy".to_string(),
            capability_dependency_field(raw, "required_by")
                .unwrap_or_else(|| serde_json::json!([])),
        ),
    ]);
    for (source, target) in [
        ("role_count", "roleCount"),
        ("spawnable_role_count", "spawnableRoleCount"),
        ("configured_count", "configuredCount"),
        ("enabled_count", "enabledCount"),
        ("ready_count", "readyCount"),
        ("enabled_tool_count", "enabledToolCount"),
        ("ready_tool_count", "readyToolCount"),
        ("tool_count", "toolCount"),
    ] {
        if let Some(value) = capability_dependency_field(raw, source) {
            normalized.insert(target.to_string(), value);
        }
    }
    serde_json::Value::Object(normalized)
}

fn normalize_public_tool_catalog_item_json(raw: &serde_json::Value) -> serde_json::Value {
    let mut item = serde_json::json!({
        "name": raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "category": raw.get("category").cloned().unwrap_or(serde_json::Value::Null),
        "public": raw.get("public").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "runtimeInternal": raw.get("runtime_internal").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "modelCallScope": raw.get("model_call_scope").cloned().unwrap_or(serde_json::Value::String("session_or_task".to_string())),
        "workspaceRequired": raw.get("workspace_required").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "modelAvailable": raw.get("model_available").cloned().unwrap_or(serde_json::Value::Bool(true)),
        "accessMode": raw.get("access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
        "policyScope": raw.get("policy_scope").cloned().unwrap_or(serde_json::Value::String("fixed".to_string())),
        "inputSensitivePolicy": raw.get("input_sensitive_policy").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "policySummary": raw.get("policy_summary").cloned().unwrap_or(serde_json::Value::String("使用工具默认风险策略".to_string())),
        "riskLevel": raw.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
        "approvalRequirement": raw.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
        "effectiveApprovalPolicy": raw.get("effective_approval_policy").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
        "accessProfileBehavior": raw.get("access_profile_behavior").cloned().unwrap_or(serde_json::Value::String("restricted_allowed".to_string())),
        "schemaStatus": raw.get("schema_status").cloned().unwrap_or(serde_json::Value::String("ok".to_string())),
        "schemaWarnings": raw.get("schema_warnings").cloned().unwrap_or_else(|| serde_json::json!([])),
        "runtimeStatus": normalize_tool_runtime_status(raw.get("runtime_status")),
        "runtimeWarnings": raw.get("runtime_warnings").cloned().unwrap_or_else(|| serde_json::json!([])),
    });
    if let Some(schema) = raw.get("parameters_schema") {
        item["parametersSchema"] = schema.clone();
    }
    project_tool_availability_fields(raw, &mut item);
    item
}

fn project_tool_availability_fields(raw: &serde_json::Value, item: &mut serde_json::Value) {
    for (source, target) in [
        ("model_available", "modelAvailable"),
        ("availability", "availability"),
        ("reason_code", "reasonCode"),
        ("schema_included", "schemaIncluded"),
    ] {
        if let Some(value) = raw.get(source) {
            item[target] = value.clone();
        }
    }
}

fn normalize_public_skill_tool_json(raw: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "source": raw.get("source").cloned().unwrap_or(serde_json::Value::Null),
        "skillId": raw.get("skill_id").cloned().unwrap_or(serde_json::Value::Null),
        "bindingId": raw.get("binding_id").cloned().unwrap_or(serde_json::Value::Null),
        "name": raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "description": raw.get("description").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "bridgeKind": raw.get("bridge_kind").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "dispatchAction": raw.get("dispatch_action").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "bridgeTarget": raw.get("bridge_target").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "accessProfileBehavior": raw.get("access_profile_behavior").cloned().unwrap_or(serde_json::Value::String("restricted_allowed".to_string())),
        "riskLevel": raw.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
        "approvalRequirement": raw.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
        "status": raw.get("status").cloned().unwrap_or(serde_json::Value::String("unavailable".to_string())),
    })
}

fn normalize_public_mcp_server_catalog_json(raw: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "serverId": raw.get("server_id").cloned().unwrap_or(serde_json::Value::Null),
        "name": raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "enabled": raw.get("enabled").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "connected": raw.get("connected").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "health": raw.get("health").cloned().unwrap_or(serde_json::Value::String("unknown".to_string())),
        "toolCount": raw.get("tool_count").cloned().unwrap_or(serde_json::Value::Null),
        "error": raw.get("error").cloned().unwrap_or(serde_json::Value::Null),
    })
}

fn normalize_public_mcp_tool_catalog_json(raw: &serde_json::Value) -> serde_json::Value {
    let mut item = serde_json::json!({
        "serverId": raw.get("server_id").cloned().unwrap_or(serde_json::Value::Null),
        "serverName": raw.get("server_name").cloned().unwrap_or(serde_json::Value::Null),
        "modelToolName": raw.get("model_tool_name").cloned().unwrap_or(serde_json::Value::Null),
        "toolName": raw.get("tool_name").cloned().unwrap_or(serde_json::Value::Null),
        "description": raw.get("description").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "readOnly": raw.get("read_only").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "inputSchema": raw.get("input_schema").cloned().unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} })),
    });
    project_tool_availability_fields(raw, &mut item);
    item
}

fn normalize_public_agent_role_catalog_json(raw: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "roleId": raw.get("role_id").cloned().unwrap_or(serde_json::Value::Null),
        "displayName": raw.get("display_name").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "description": raw.get("description").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "spawnable": raw.get("spawnable").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "coordinatorMode": raw.get("coordinator_mode").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "supportedKinds": raw.get("supported_kinds").cloned().unwrap_or_else(|| serde_json::json!([])),
        "capabilityIds": raw.get("capability_ids").cloned().unwrap_or_else(|| serde_json::json!([])),
        "parallelismLimit": raw.get("parallelism_limit").cloned().unwrap_or(serde_json::Value::Null),
        "status": raw.get("status").cloned().unwrap_or(serde_json::Value::String("unknown".to_string())),
        "modelBindingStatus": raw.get("model_binding_status").cloned().unwrap_or(serde_json::Value::String("unconfigured".to_string())),
    })
}

fn public_tool_catalog_array(
    raw: &serde_json::Value,
    source_key: &str,
    item_mapper: fn(&serde_json::Value) -> serde_json::Value,
) -> serde_json::Value {
    let items = raw
        .get(source_key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(item_mapper)
        .collect::<Vec<_>>();
    serde_json::Value::Array(items)
}

fn public_tool_catalog_response_json(raw: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "tool": raw.get("tool").cloned().unwrap_or(serde_json::Value::String("tool_catalog".to_string())),
        "status": raw.get("status").cloned().unwrap_or(serde_json::Value::String("succeeded".to_string())),
        "catalogAccessMode": raw.get("catalog_access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
        "currentAccessProfile": raw.get("current_access_profile").cloned().unwrap_or(serde_json::Value::String("restricted".to_string())),
        "approvalPolicySummary": raw.get("approval_policy_summary").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "summary": raw.get("summary").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "total": raw.get("total").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "builtinTotal": raw.get("builtin_total").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "publicCount": raw.get("public_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "internalCount": raw.get("internal_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "schemaWarningCount": raw.get("schema_warning_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "runtimeWarningCount": raw.get("runtime_warning_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "runtimeDependencies": public_tool_catalog_array(raw, "runtime_dependencies", normalize_capability_dependency_json),
        "externalCatalogStatus": raw.get("external_catalog_status").cloned().unwrap_or(serde_json::Value::String("unavailable".to_string())),
        "skillToolCount": raw.get("skill_tool_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "mcpServerCount": raw.get("mcp_server_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "connectedMcpServerCount": raw.get("connected_mcp_server_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "mcpToolCount": raw.get("mcp_tool_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "agentRoleCatalogStatus": raw.get("agent_role_catalog_status").cloned().unwrap_or(serde_json::Value::String("unavailable".to_string())),
        "agentRoleCount": raw.get("agent_role_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "spawnableAgentRoleCount": raw.get("spawnable_agent_role_count").cloned().unwrap_or(serde_json::Value::Number(0.into())),
        "tools": public_tool_catalog_array(raw, "tools", normalize_public_tool_catalog_item_json),
        "skillTools": public_tool_catalog_array(raw, "skill_tools", normalize_public_skill_tool_json),
        "mcpServers": public_tool_catalog_array(raw, "mcp_servers", normalize_public_mcp_server_catalog_json),
        "mcpTools": public_tool_catalog_array(raw, "mcp_tools", normalize_public_mcp_tool_catalog_json),
        "agentRoles": public_tool_catalog_array(raw, "agent_roles", normalize_public_agent_role_catalog_json),
    })
}

fn warning_markers(
    raw: &serde_json::Value,
    field: &str,
    marker: &'static str,
) -> serde_json::Value {
    let count = raw
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(|warnings| {
            warnings
                .iter()
                .filter(|warning| {
                    warning
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty())
                })
                .count()
        })
        .unwrap_or(0);
    serde_json::Value::Array(
        std::iter::repeat_with(|| serde_json::Value::String(marker.to_string()))
            .take(count)
            .collect(),
    )
}

fn normalize_tool_runtime_status(value: Option<&serde_json::Value>) -> serde_json::Value {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(|status| serde_json::Value::String(status.to_string()))
        .unwrap_or_else(|| serde_json::Value::String("unknown".to_string()))
}

fn capability_dependency_field(
    raw: &serde_json::Value,
    snake_key: &str,
) -> Option<serde_json::Value> {
    raw.get(snake_key).cloned().filter(|value| !value.is_null())
}

fn object_section(snapshot: &HashMap<String, serde_json::Value>, key: &str) -> serde_json::Value {
    snapshot
        .get(key)
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

fn array_section(snapshot: &HashMap<String, serde_json::Value>, key: &str) -> serde_json::Value {
    snapshot
        .get(key)
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]))
}

fn public_mcp_servers_section(snapshot: &HashMap<String, serde_json::Value>) -> serde_json::Value {
    array_section(snapshot, "mcpServers")
}

fn runtime_settings_from_snapshot(
    snapshot: &HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    // 运行时偏好只由 `/settings/update` 写入顶层的 `locale` / `conversationDisplayMode`，
    // 这里只读这一个位置。
    let locale = snapshot
        .get("locale")
        .and_then(|value| value.as_str())
        .filter(|value| crate::locales::is_supported_locale(value))
        .unwrap_or(crate::locales::DEFAULT_LOCALE);
    let conversation_display_mode = snapshot
        .get("conversationDisplayMode")
        .and_then(|value| value.as_str())
        .filter(|value| matches!(*value, "original" | "summary"))
        .unwrap_or("original");
    serde_json::json!({
        "locale": locale,
        "conversationDisplayMode": conversation_display_mode,
    })
}

fn seed_user_rules_config(snapshot: &mut HashMap<String, serde_json::Value>) {
    snapshot.remove("userRulesConfig");
    let raw = snapshot
        .remove("userRules")
        .unwrap_or_else(|| serde_json::json!({}));
    snapshot.insert(
        "userRulesConfig".to_string(),
        normalize_user_rules_config_value(raw),
    );
}

fn normalize_user_rules_config_value(value: serde_json::Value) -> serde_json::Value {
    if value.is_object() {
        value
    } else {
        serde_json::json!({})
    }
}

pub(crate) fn normalize_safeguard_config_value(mut value: serde_json::Value) -> serde_json::Value {
    strip_scope_binding_fields(&mut value);
    let mut object = match value {
        serde_json::Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    let rules = object
        .get("rules")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let normalized_rules = magi_safety_gate::rules_from_settings_value(&rules)
        .into_iter()
        .map(safeguard_rule_json)
        .collect::<Vec<_>>();
    let revision = object
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    object.insert(
        "rules".to_string(),
        serde_json::Value::Array(normalized_rules),
    );
    object.insert("revision".to_string(), serde_json::Value::from(revision));
    serde_json::Value::Object(object)
}

fn normalize_safeguard_config_section(snapshot: &mut HashMap<String, serde_json::Value>) {
    let Some(value) = snapshot.remove("safeguardConfig") else {
        return;
    };
    snapshot.insert(
        "safeguardConfig".to_string(),
        normalize_safeguard_config_value(value),
    );
}

fn safeguard_rule_json(rule: magi_safety_gate::SafetyRule) -> serde_json::Value {
    serde_json::json!({
        "pattern": rule.pattern,
        "enabled": rule.enabled,
        "category": rule.category.as_str(),
        "action": rule.action.as_str(),
    })
}

/// 设置页看到的规则集 = 运行期 SafetyGate 使用的规则集：已保存的规则与内置规则合并，
/// 版本升级新增的内置规则会出现在列表里，已下线的旧内置规则会消失（见 safety-gate 的合并规则）。
fn seed_default_safeguard_rules(snapshot: &mut HashMap<String, serde_json::Value>) {
    let safeguard = snapshot
        .entry("safeguardConfig".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !safeguard.is_object() {
        *safeguard = serde_json::json!({});
    }
    let saved_rules = safeguard
        .get("rules")
        .map(magi_safety_gate::rules_from_settings_value)
        .unwrap_or_default();
    let merged = magi_safety_gate::merge_rules_with_builtin_defaults(saved_rules)
        .into_iter()
        .map(safeguard_rule_json)
        .collect::<Vec<_>>();
    safeguard["rules"] = serde_json::Value::Array(merged);
}

fn normalize_mcp_servers_section(snapshot: &mut HashMap<String, serde_json::Value>) {
    let Some(servers) = snapshot.get_mut("mcpServers") else {
        return;
    };
    let Some(entries) = servers.as_array_mut() else {
        return;
    };
    let normalized_entries = entries
        .iter()
        .filter_map(normalize_mcp_server_snapshot_entry)
        .collect();
    *entries = normalized_entries;
}

/// 会话等人（工具授权、`ask_user_question`）时让出 repository 执行租约，人回应后再抢回。
///
/// 租约只用来保证同一 repository 同时只有一个主 session 在执行；等人不是执行，
/// 不让出的话，用户思考期间同一仓库的其它会话都发不出消息。
struct HumanWaitGitLeaseHook {
    coordinator: magi_git::WorkspaceGitOperationCoordinator,
}

impl magi_conversation_runtime::HumanWaitHook for HumanWaitGitLeaseHook {
    fn suspend(&self, session_id: &SessionId) {
        self.coordinator.suspend_execution(session_id.as_str());
    }

    fn try_resume(&self, session_id: &SessionId) -> bool {
        self.coordinator
            .try_resume_execution(session_id.as_str())
            .is_ok()
    }
}

#[cfg(test)]
mod tests;
