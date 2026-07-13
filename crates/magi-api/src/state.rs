use crate::dto::{
    AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeProvider, BridgeCutoverSmokeSnapshotDto,
    BridgeCutoverSmokeSnapshotProvider, BridgePreflightProvider, BridgePreflightSnapshotDto,
    BridgePreflightSnapshotProvider, BridgeProbeSnapshotProvider, BridgeServicesSnapshotDto,
    BridgeSnapshotProvider, DirectHttpModelProbeConfig, HealthDto, RuntimeReadModelDto,
    ServiceInfo, SessionTurnRequestDto, SessionTurnRouteDto, VersionHandshakeDto,
    runtime_read_model_dto_with_usage,
};
use crate::errors::ApiError;
use crate::mcp_config::{
    build_mcp_config_from_entry, mcp_server_entry_enabled, normalize_mcp_server_snapshot_entry,
    redact_mcp_server_public_entry,
};
use crate::routes::settings::{
    builtin_role_templates, load_registry_engines, registered_role_template_ids,
    resolve_registry_agents,
};
use crate::scope_binding::strip_scope_binding_fields;
use crate::skill_loader;
use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient, ModelBridgeClient,
    StdioMcpBridgeClient,
};
use magi_conversation_runtime::{
    ConversationRegistry,
    session_images::SessionTurnImage,
    task_execution_dispatcher::{ExecutionPipeline, LlmTaskDispatcher},
    task_execution_registry::TaskExecutionRegistry,
    task_runner::TaskRunner,
    task_runner_bridge::{
        EventBasedResultReceiver, RunCycleOutcome, TaskDispatchGate, TaskDispatcher,
        TaskResultReceiver,
    },
};
use magi_core::{
    SessionId, SessionLifecycleStatus, TaskId, TaskStatus, TaskTier, UtcMillis, WorkspaceId,
};
use magi_event_bus::{InMemoryEventBus, latest_usage_observations_from_ledger};
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_memory_store::MemoryStore;
use magi_orchestrator::{
    OrchestratedExecutionRuntime, OrchestratorService,
    task_store::TaskStore,
    task_worker_catalog::{WorkerInfo, build_worker_catalog_for_roles},
};
use magi_session_store::{SessionLifecycleObserver, SessionRecord, SessionStore};
use magi_settings_store::SettingsStore;
use magi_snapshot::{SnapshotManager, SnapshotSession};
use magi_tool_runtime::{
    RuntimeCapabilityDependencyEntry, RuntimeCapabilityDependencyProvider, ToolExecutionContext,
    ToolExecutionContextQuery, ToolRegistry,
};
use magi_workspace::WorkspaceStore;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

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

type RunnerTerminalObserver = Arc<dyn Fn(TaskId, Option<SessionId>, String) + Send + Sync>;
pub type SessionStateCheckpointPersist = Arc<dyn Fn(&str) -> Result<(), ApiError> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SessionTurnQueueKey {
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
}

impl SessionTurnQueueKey {
    fn new(session_id: &SessionId, workspace_id: Option<&WorkspaceId>) -> Self {
        Self {
            session_id: session_id.clone(),
            workspace_id: workspace_id.cloned(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedRegularSessionTurn {
    pub request: SessionTurnRequestDto,
    pub images: Vec<SessionTurnImage>,
    pub requested_workspace_id: WorkspaceId,
    pub accepted_at: UtcMillis,
    pub route: SessionTurnRouteDto,
    pub task_title: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    pub tool_intent: Option<String>,
    pub forced_tool_name: Option<String>,
    pub required_tool_chain: Vec<String>,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub queue_id: String,
}

pub(crate) fn session_has_user_content(session: &SessionRecord) -> bool {
    session.message_count.unwrap_or(0) > 0
}

/// Manages active Runner instances keyed by root_task_id.
#[derive(Clone)]
pub struct RunnerManager {
    runners: Arc<Mutex<HashMap<String, Arc<RunnerHandle>>>>,
    task_store: Arc<TaskStore>,
    worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
    agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
    dispatcher: Option<Arc<dyn TaskDispatcher>>,
    dispatch_gate: Option<Arc<TaskDispatchGate>>,
    /// Shared result receiver that collects task completion/failure results
    /// pushed from the TaskStore's status-change callback.
    result_receiver: Arc<EventBasedResultReceiver>,
    /// Optional path for periodic task-store checkpoints.
    checkpoint_path: Option<PathBuf>,
    /// Maps a session to the root task IDs whose runners should be killed
    /// when the session is closed (design 1.5: Session-Runner linkage).
    session_runner_index: Arc<Mutex<HashMap<SessionId, Vec<String>>>>,
    terminal_observer: Option<RunnerTerminalObserver>,
}

/// Number of runner cycles between periodic checkpoints.
const CHECKPOINT_INTERVAL_CYCLES: u64 = 5;

impl RunnerManager {
    pub fn with_dispatcher_and_worker_catalog(
        task_store: Arc<TaskStore>,
        worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
        dispatcher: Arc<dyn TaskDispatcher>,
        result_receiver: Arc<EventBasedResultReceiver>,
    ) -> Self {
        Self {
            runners: Arc::new(Mutex::new(HashMap::new())),
            task_store,
            worker_catalog,
            agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            dispatcher: Some(dispatcher),
            dispatch_gate: None,
            result_receiver,
            checkpoint_path: None,
            session_runner_index: Arc::new(Mutex::new(HashMap::new())),
            terminal_observer: None,
        }
    }

    fn resolved_workers(&self) -> Vec<WorkerInfo> {
        (self.worker_catalog)()
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

    fn build_task_runner(&self) -> TaskRunner {
        let workers = self.resolved_workers();
        let dispatcher = self
            .dispatcher
            .as_ref()
            .expect("RunnerManager 缺少 LLM dispatcher");
        let mut runner = TaskRunner::with_dispatcher(
            Arc::clone(&self.task_store),
            workers,
            Arc::clone(dispatcher),
            Arc::clone(&self.result_receiver) as Arc<dyn TaskResultReceiver>,
        );
        runner = runner.with_agent_role_registry((*self.agent_role_registry).clone());
        if let Some(gate) = &self.dispatch_gate {
            runner = runner.with_dispatch_gate(Arc::clone(gate));
        }
        runner
    }

    /// Set the file path used for periodic task-store checkpoints.
    pub fn with_checkpoint_path(mut self, path: PathBuf) -> Self {
        self.checkpoint_path = Some(path);
        self
    }

    pub fn with_terminal_observer(
        mut self,
        observer: impl Fn(TaskId, Option<SessionId>, String) + Send + Sync + 'static,
    ) -> Self {
        self.terminal_observer = Some(Arc::new(observer));
        self
    }

    /// Get a reference to the shared result receiver.
    ///
    /// This is used by the daemon to wire the TaskStore's status-change
    /// callback so that terminal status transitions push results into the
    /// receiver for the Runner to pick up.
    pub fn result_receiver(&self) -> &Arc<EventBasedResultReceiver> {
        &self.result_receiver
    }

    /// Attempt to start a runner for the given root task.
    /// Returns Err if the root task doesn't exist or a runner is already active.
    pub fn start(
        &self,
        root_task_id: &str,
        session_id: Option<SessionId>,
    ) -> Result<Arc<RunnerHandle>, RunnerStartError> {
        let tid = TaskId::new(root_task_id);
        // Verify the root task exists.
        self.task_store
            .get_task(&tid)
            .ok_or(RunnerStartError::NotFound)?;

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

        runners.insert(root_task_id.to_string(), Arc::clone(&handle));
        drop(runners);

        let observer_session_id = session_id.clone();
        if let Some(session_id) = session_id {
            self.bind_session(session_id, root_task_id);
        }

        // Spawn the background loop.
        let task_runner = self.build_task_runner();
        let root_id = tid;
        let bg_handle = Arc::clone(&handle);
        let bg_active = Arc::clone(&handle.active);
        let bg_task_store = Arc::clone(&self.task_store);
        let bg_checkpoint_path = self.checkpoint_path.clone();
        let terminal_observer = self.terminal_observer.clone();
        let join_handle = tokio::spawn(async move {
            let mut stalled_streak = 0u32;
            let max_stalled_streak = 20u32;
            loop {
                if bg_handle.cancel.load(Ordering::Relaxed) {
                    let mut status = bg_handle.status.lock().expect("status lock should hold");
                    *status = "killed".to_string();
                    bg_active.store(false, Ordering::Relaxed);
                    break;
                }

                let outcome = task_runner.run_cycle(&root_id);
                let cycle = bg_handle.cycle_count.fetch_add(1, Ordering::Relaxed) + 1;

                // Checkpoint policy consumption (design 3.2).
                if let Some(ref path) = bg_checkpoint_path {
                    let should_checkpoint =
                        if let Some(root_task) = bg_task_store.get_task(&root_id) {
                            if let Some(ref policy) = root_task.policy_snapshot {
                                match policy.checkpoint_mode.as_str() {
                                    "turn" => true,
                                    "task_or_phase" => task_runner.take_checkpoint_signal(),
                                    _ => cycle.is_multiple_of(CHECKPOINT_INTERVAL_CYCLES),
                                }
                            } else {
                                cycle.is_multiple_of(CHECKPOINT_INTERVAL_CYCLES)
                            }
                        } else {
                            cycle.is_multiple_of(CHECKPOINT_INTERVAL_CYCLES)
                        };
                    if should_checkpoint {
                        let _ = bg_task_store.checkpoint_to_file(path);
                    }
                }

                match outcome {
                    RunCycleOutcome::Continue => {
                        stalled_streak = 0;
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
                        if let Some(ref path) = bg_checkpoint_path {
                            let _ = bg_task_store.checkpoint_to_file(path);
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = "completed".to_string();
                        bg_active.store(false, Ordering::Relaxed);
                        if let Some(observer) = terminal_observer.as_ref() {
                            observer(
                                root_id.clone(),
                                observer_session_id.clone(),
                                "completed".to_string(),
                            );
                        }
                        break;
                    }
                    RunCycleOutcome::Stalled(stalled_ids) => {
                        stalled_streak += 1;
                        let should_finalize_stalled = stalled_streak >= max_stalled_streak
                            || stalled_outcome_is_terminally_unrunnable(
                                &bg_task_store,
                                &stalled_ids,
                            );
                        if should_finalize_stalled {
                            let runner_status = match task_runner
                                .finalize_stalled_outcome(&root_id, &stalled_ids)
                            {
                                Ok(_) => "error",
                                Err(err) => {
                                    let mut last_error = bg_handle
                                        .last_error
                                        .lock()
                                        .expect("last_error lock should hold");
                                    *last_error = Some(err);
                                    "error"
                                }
                            };
                            if let Some(ref path) = bg_checkpoint_path {
                                let _ = bg_task_store.checkpoint_to_file(path);
                            }
                            let mut status =
                                bg_handle.status.lock().expect("status lock should hold");
                            *status = runner_status.to_string();
                            bg_active.store(false, Ordering::Relaxed);
                            if let Some(observer) = terminal_observer.as_ref() {
                                observer(
                                    root_id.clone(),
                                    observer_session_id.clone(),
                                    runner_status.to_string(),
                                );
                            }
                            break;
                        }
                        let backoff_ms = 200u64.saturating_mul(stalled_streak as u64).min(2_000);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                    RunCycleOutcome::Blocked { reason, .. } => {
                        stalled_streak = 0;
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
                        if let Some(ref path) = bg_checkpoint_path {
                            let _ = bg_task_store.checkpoint_to_file(path);
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = "error".to_string();
                        let mut last_error = bg_handle
                            .last_error
                            .lock()
                            .expect("last_error lock should hold");
                        *last_error = Some(err);
                        bg_active.store(false, Ordering::Relaxed);
                        if let Some(observer) = terminal_observer.as_ref() {
                            observer(
                                root_id.clone(),
                                observer_session_id.clone(),
                                "error".to_string(),
                            );
                        }
                        break;
                    }
                }
            }
        });
        *handle
            .join_handle
            .lock()
            .expect("runner join handle lock should hold") = Some(join_handle);

        Ok(handle)
    }

    /// Signal a runner to stop.
    pub fn stop(&self, root_task_id: &str) -> Result<(), RunnerStopError> {
        let runners = self.runners.lock().expect("runners lock should hold");
        let handle = runners.get(root_task_id).ok_or(RunnerStopError::NotFound)?;
        if !handle.active.load(Ordering::Relaxed) {
            return Err(RunnerStopError::NotRunning);
        }
        handle.cancel.store(true, Ordering::Relaxed);
        let mut status = handle.status.lock().expect("status lock should hold");
        *status = "killed".to_string();
        Ok(())
    }

    /// Bind a session to a root task so that when the session closes the
    /// runner is automatically killed.
    pub fn bind_session(&self, session_id: SessionId, root_task_id: &str) {
        let mut index = self
            .session_runner_index
            .lock()
            .expect("session_runner_index lock should hold");
        index
            .entry(session_id)
            .or_default()
            .push(root_task_id.to_string());
    }

    /// Cancel all runners bound to the given session and remove the binding.
    /// Called when a session is closed.
    pub async fn unbind_session(&self, session_id: &SessionId) -> usize {
        let root_task_ids = {
            let mut index = self
                .session_runner_index
                .lock()
                .expect("session_runner_index lock should hold");
            index.remove(session_id).unwrap_or_default()
        };
        let mut joins = Vec::new();
        {
            let runners = self.runners.lock().expect("runners lock should hold");
            for root_task_id in &root_task_ids {
                let Some(handle) = runners.get(root_task_id) else {
                    continue;
                };
                handle.cancel.store(true, Ordering::Relaxed);
                if handle.active.load(Ordering::Relaxed) {
                    *handle.status.lock().expect("status lock should hold") = "killed".to_string();
                }
                if let Some(join) = handle
                    .join_handle
                    .lock()
                    .expect("runner join handle lock should hold")
                    .take()
                {
                    joins.push(join);
                }
            }
        }
        for join in joins {
            let _ = join.await;
        }
        let mut runners = self.runners.lock().expect("runners lock should hold");
        for root_task_id in &root_task_ids {
            runners.remove(root_task_id);
        }
        root_task_ids.len()
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

    /// Run a single cycle synchronously (for testing / manual trigger).
    pub fn run_single_cycle(&self, root_task_id: &str) -> Result<RunCycleOutcome, String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        let task_runner = self.build_task_runner();
        Ok(task_runner.run_cycle(&tid))
    }

    pub fn kill_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.build_task_runner().kill_tree(&tid)?;
        self.set_runner_status_if_present(root_task_id, "killed");
        Ok(())
    }

    pub fn kill_task(&self, task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(task_id);
        let task = self
            .task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", task_id))?;
        self.build_task_runner().kill_task(&tid)?;
        self.set_runner_status_if_present(task.root_task_id.as_str(), "killed");
        Ok(())
    }

    pub fn resume_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.build_task_runner().resume_task(&tid)
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

fn stalled_outcome_is_terminally_unrunnable(
    task_store: &TaskStore,
    stalled_task_ids: &[TaskId],
) -> bool {
    stalled_task_ids.iter().any(|task_id| {
        task_store
            .get_task(task_id)
            .is_some_and(|task| matches!(task.status, TaskStatus::Failed | TaskStatus::Killed))
    })
}

#[derive(Debug)]
pub enum RunnerStartError {
    NotFound,
    AlreadyRunning,
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

#[derive(Clone)]
pub struct ApiState {
    pub service_info: ServiceInfo,
    runtime_epoch: String,
    pub event_bus: Arc<InMemoryEventBus>,
    pub session_store: Arc<SessionStore>,
    pub workspace_registry: Arc<WorkspaceStore>,
    pub governance: Arc<GovernanceService>,
    pub knowledge_store: Arc<KnowledgeStore>,
    pub settings_store: Arc<SettingsStore>,
    runtime_persistence: Option<Arc<RuntimeStatePersistence>>,
    session_state_checkpoint_persist: Option<SessionStateCheckpointPersist>,
    bridge_probe_snapshot_provider: BridgeProbeSnapshotProvider,
    bridge_preflight_snapshot_provider: BridgePreflightSnapshotProvider,
    bridge_cutover_smoke_provider: BridgeCutoverSmokeSnapshotProvider,
    bridge_snapshot_provider: Option<Arc<dyn BridgeSnapshotProvider>>,
    execution_pipeline: Option<ExecutionPipeline>,
    task_execution_registry: TaskExecutionRegistry,
    task_store: Option<Arc<TaskStore>>,
    runner_manager: Option<RunnerManager>,
    session_turn_dispatcher: Option<Arc<LlmTaskDispatcher>>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    model_bridge_client_is_real: bool,
    tool_registry: Option<ToolRegistry>,
    pub skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    pub skill_dispatch_runtime: Option<Arc<magi_skill_runtime::SkillDispatchRuntime>>,
    pub tunnel_manager: crate::tunnel::TunnelManager,
    pub snapshot_manager: Arc<SnapshotManager>,
    pub conversation_registry: Arc<ConversationRegistry>,
    /// 任务系统：AgentRole 注册表（替代 task_worker_catalog 硬编码 prompt）。
    /// 加载策略：`~/.magi/roles/*.json` 优先，回落到 crate 内置 builtin 集。
    pub agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
    /// 任务系统 — L5：父子任务关系图，作为 task_dispatch 中
    /// "parent_task_id 散落查询"的统一上层。同一进程共享。
    pub spawn_graph: Arc<Mutex<magi_spawn_graph::SpawnGraph>>,
    session_turn_queue:
        Arc<Mutex<HashMap<SessionTurnQueueKey, VecDeque<QueuedRegularSessionTurn>>>>,
}

#[derive(Clone, Debug)]
pub struct RuntimeStatePersistence {
    session_path: PathBuf,
    workspace_path: PathBuf,
    knowledge_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

const SESSION_PERSISTENCE_PUBLIC_ERROR: &str = "会话状态暂不可保存，请稍后重试";
const WORKSPACE_PERSISTENCE_PUBLIC_ERROR: &str = "工作区状态暂不可保存，请稍后重试";
const KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR: &str = "知识库状态暂不可保存，请稍后重试";

impl RuntimeStatePersistence {
    pub fn new(
        session_path: impl Into<PathBuf>,
        workspace_path: impl Into<PathBuf>,
        knowledge_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            session_path: session_path.into(),
            workspace_path: workspace_path.into(),
            knowledge_path: knowledge_path.into(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn state_root(&self) -> Option<&Path> {
        self.session_path.parent()
    }

    pub(crate) fn save_json<T>(&self, path: &Path, value: &T) -> Result<(), ApiError>
    where
        T: serde::Serialize,
    {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("runtime persistence write lock poisoned");
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
        self.save_json(&self.workspace_path, &store.durable_state())
    }

    fn save_knowledge_store(&self, store: &KnowledgeStore) -> Result<(), ApiError> {
        self.save_json(&self.knowledge_path, &store.export_state())
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
    snapshot_manager: Arc<SnapshotManager>,
    workspace_registry: Arc<WorkspaceStore>,
    context_runtime_available: bool,
) -> RuntimeCapabilityDependencyProvider {
    Arc::new(move |context| {
        vec![
            context_runtime_capability_dependency(context, context_runtime_available),
            file_snapshot_capability_dependency(
                snapshot_manager.as_ref(),
                workspace_registry.as_ref(),
                context,
            ),
        ]
    })
}

fn context_runtime_capability_dependency(
    context: &ToolExecutionContext,
    context_runtime_available: bool,
) -> RuntimeCapabilityDependencyEntry {
    let session_id = context.session_id.as_ref().map(ToString::to_string);
    let workspace_id = context.workspace_id.as_ref().map(ToString::to_string);
    let status = if !context_runtime_available {
        "unavailable"
    } else if session_id.is_none() || workspace_id.is_none() {
        "missing_context"
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
        workspace_id,
        session_id,
        file_count: None,
        last_indexed: None,
        cache_status: None,
        role_count: None,
        spawnable_role_count: None,
        snapshot_active: None,
        configured_count: None,
        enabled_count: None,
        ready_count: None,
        tool_count: None,
    }
}

fn file_snapshot_capability_dependency(
    snapshot_manager: &SnapshotManager,
    workspace_registry: &WorkspaceStore,
    context: &ToolExecutionContext,
) -> RuntimeCapabilityDependencyEntry {
    let session_id = context.session_id.as_ref().map(ToString::to_string);
    let workspace_id = context.workspace_id.as_ref().map(ToString::to_string);
    let workspace_root = match context.workspace_id.as_ref() {
        Some(workspace_id) => workspace_root_path_from_registry(workspace_registry, workspace_id),
        None => context.working_directory.clone(),
    };
    let workspace_root_available = workspace_root
        .as_deref()
        .is_some_and(|workspace_root| workspace_root.is_absolute() && workspace_root.is_dir());
    let snapshot_active = session_id.as_deref().is_some_and(|session_id| {
        workspace_root.as_ref().is_some_and(|workspace_root| {
            snapshot_manager
                .get_session_for_workspace(session_id, workspace_root)
                .is_some()
        })
    });
    let status = if session_id.is_none() || workspace_id.is_none() {
        "missing_context"
    } else if !workspace_root_available {
        "unavailable"
    } else {
        "ready"
    };

    RuntimeCapabilityDependencyEntry {
        name: "file_snapshot".to_string(),
        status: status.to_string(),
        required_by: vec![
            "changes/diff".to_string(),
            "changes/approve".to_string(),
            "changes/revert".to_string(),
        ],
        workspace_id,
        session_id,
        file_count: None,
        last_indexed: None,
        cache_status: None,
        role_count: None,
        spawnable_role_count: None,
        snapshot_active: Some(snapshot_active),
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
        .map(|workspace| PathBuf::from(workspace.root_path.as_str()))
}

fn canonicalize_path_for_workspace_match(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

impl ApiState {
    pub fn new(
        service_name: impl Into<String>,
        event_bus: Arc<InMemoryEventBus>,
        session_store: Arc<SessionStore>,
        workspace_registry: Arc<WorkspaceStore>,
        governance: Arc<GovernanceService>,
    ) -> Self {
        Self {
            service_info: ServiceInfo {
                service_name: service_name.into(),
                api_version: "v0".to_string(),
            },
            runtime_epoch: format!("runtime-{}", UtcMillis::now().0),
            event_bus,
            session_store,
            workspace_registry,
            governance,
            knowledge_store: Arc::new(KnowledgeStore::new()),
            settings_store: Arc::new(SettingsStore::new()),
            runtime_persistence: None,
            session_state_checkpoint_persist: None,
            bridge_probe_snapshot_provider: BridgeProbeSnapshotProvider::default(),
            bridge_preflight_snapshot_provider: BridgePreflightSnapshotProvider::default(),
            bridge_cutover_smoke_provider: BridgeCutoverSmokeSnapshotProvider::default(),
            bridge_snapshot_provider: None,
            execution_pipeline: None,
            task_execution_registry: TaskExecutionRegistry::default(),
            task_store: None,
            runner_manager: None,
            session_turn_dispatcher: None,
            mcp_connections: Arc::new(RwLock::new(HashMap::new())),
            model_bridge_client: None,
            model_bridge_client_is_real: false,
            tool_registry: None,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            tunnel_manager: crate::tunnel::TunnelManager::new(38123),
            snapshot_manager: Arc::new(SnapshotManager::new()),
            conversation_registry: Arc::new(ConversationRegistry::new()),
            agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            spawn_graph: Arc::new(Mutex::new(magi_spawn_graph::SpawnGraph::new())),
            session_turn_queue: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 安装 SessionLifecycleObserver，把 session 创建/归档/删除事件桥接到 SnapshotManager。
    pub fn install_snapshot_lifecycle_observer(&self) {
        let observer = Arc::new(crate::snapshot_lifecycle::SnapshotLifecycleObserver::new(
            self.snapshot_manager.clone(),
            self.workspace_registry.clone(),
        ));
        self.session_store.set_lifecycle_observer(observer.clone());
        let registered_workspace_ids = self
            .workspace_registry
            .workspaces()
            .into_iter()
            .map(|workspace| workspace.workspace_id.to_string())
            .collect::<HashSet<_>>();
        let mut skipped_orphan_workspace_sessions = 0usize;
        for session in self.session_store.sessions() {
            if session.status != SessionLifecycleStatus::Active {
                continue;
            }
            if let Some(workspace_id) = session.workspace_id.as_deref()
                && !registered_workspace_ids.contains(workspace_id)
            {
                skipped_orphan_workspace_sessions += 1;
                continue;
            }
            observer.on_session_created(&session.session_id, session.workspace_id.as_deref());
        }
        if skipped_orphan_workspace_sessions > 0 {
            tracing::warn!(
                skipped_orphan_workspace_sessions,
                "snapshot lifecycle: 启动重放跳过未注册 workspace 的历史 session"
            );
        }
    }

    /// 同步取 session + workspace 对应的 SnapshotSession。未装载表示生命周期接线异常，
    /// 调用方应显式报错或触发 lazy start。
    pub fn snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Option<Arc<SnapshotSession>> {
        self.snapshot_manager
            .get_session_for_workspace(session_id.as_str(), workspace_root)
    }

    pub(crate) async fn ensure_snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Result<Arc<SnapshotSession>, ApiError> {
        if let Some(session) = self.snapshot_session(session_id, workspace_root) {
            return Ok(session);
        }
        self.snapshot_manager
            .start_session(
                session_id.as_str().to_string(),
                workspace_root.to_path_buf(),
            )
            .await
            .map_err(|error| ApiError::internal_assembly("启动会话快照账本失败", error))
    }

    pub(crate) async fn ensure_snapshot_session_for_workspace_id(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<Option<Arc<SnapshotSession>>, ApiError> {
        let Some(workspace_id) = workspace_id else {
            return Ok(None);
        };
        let workspace_root = self
            .workspace_root_path(&Some(workspace_id.clone()))
            .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace_id.as_str()))?;
        self.ensure_snapshot_session(session_id, &workspace_root)
            .await
            .map(Some)
    }

    pub fn with_tunnel_port(mut self, port: u16) -> Self {
        self.tunnel_manager = crate::tunnel::TunnelManager::new(port);
        self
    }

    pub fn with_bridge_probe_transport(
        mut self,
        server_kind: BridgeServerKind,
        transport: Arc<dyn BridgeTransport>,
    ) -> Self {
        self.bridge_probe_snapshot_provider
            .register_transport(server_kind, transport.clone());
        self.bridge_preflight_snapshot_provider
            .register_transport(server_kind, transport.clone());
        self.bridge_cutover_smoke_provider
            .register_transport(server_kind, transport);
        self
    }

    pub fn task_worker_catalog(&self) -> Vec<WorkerInfo> {
        build_worker_catalog_for_roles(
            &self.agent_role_registry,
            registered_role_template_ids(self),
        )
    }

    pub fn with_bridge_probe(
        mut self,
        server_kind: BridgeServerKind,
        probe: JsonRpcBridgeServerProbeClient,
    ) -> Self {
        self.bridge_probe_snapshot_provider
            .register_probe(server_kind, probe);
        self
    }

    pub fn with_direct_http_model_probe(mut self, config: DirectHttpModelProbeConfig) -> Self {
        self.bridge_cutover_smoke_provider
            .register_direct_http_probe(config);
        self
    }

    pub fn with_bridge_snapshot_provider(
        mut self,
        provider: Arc<dyn BridgeSnapshotProvider>,
    ) -> Self {
        self.bridge_snapshot_provider = Some(provider);
        self
    }

    pub fn with_execution_pipeline(
        mut self,
        orchestrator: OrchestratorService,
        execution_runtime: OrchestratedExecutionRuntime,
        memory_store: MemoryStore,
    ) -> Self {
        self.execution_pipeline = Some(ExecutionPipeline {
            orchestrator,
            execution_runtime,
            memory_store,
        });
        self
    }

    pub fn with_tool_registry(mut self, tool_registry: ToolRegistry) -> Self {
        self.tool_registry = Some(tool_registry);
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

    pub fn cancel_active_tool_executions(
        &self,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
        task_id: Option<&TaskId>,
    ) -> usize {
        let Some(registry) = &self.tool_registry else {
            return 0;
        };
        registry.cancel_active_shell_execs(&ToolExecutionContextQuery {
            session_id: session_id.cloned(),
            workspace_id: workspace_id.cloned(),
            task_id: task_id.cloned(),
            worker_id: None,
        })
    }

    pub fn runtime_persistence(&self) -> Option<&RuntimeStatePersistence> {
        self.runtime_persistence.as_deref()
    }

    pub fn health_dto(&self) -> HealthDto {
        HealthDto::from_service_info(&self.service_info)
    }

    pub fn runtime_epoch(&self) -> &str {
        &self.runtime_epoch
    }

    pub fn bootstrap_dto(&self) -> Result<BootstrapDto, ApiError> {
        BootstrapDto::from_state(self)
    }

    pub fn bootstrap_dto_for_session(
        &self,
        requested_session_id: Option<&SessionId>,
    ) -> Result<BootstrapDto, ApiError> {
        BootstrapDto::from_state_with_selected_session(self, requested_session_id)
    }

    pub fn bootstrap_dto_for_workspace_session(
        &self,
        workspace_id: Option<&str>,
        requested_session_id: Option<&SessionId>,
    ) -> Result<BootstrapDto, ApiError> {
        let Some(ws_id) = workspace_id else {
            return BootstrapDto::from_state_with_selected_session(self, requested_session_id);
        };
        let event_snapshot = self.event_bus.snapshot();
        let mut projection = self.session_store.projection_input();
        projection.sessions = self.session_records_for_workspace(Some(ws_id));
        let selected_session_id = requested_session_id
            .filter(|session_id| {
                projection
                    .sessions
                    .iter()
                    .any(|session| session.session_id == **session_id)
            })
            .cloned()
            .or_else(|| {
                projection
                    .sessions
                    .first()
                    .map(|session| session.session_id.clone())
            });
        projection.current_session_id = selected_session_id.clone();
        if let Some(session_id) = selected_session_id.as_ref() {
            projection
                .timeline
                .retain(|entry| entry.session_id == *session_id);
            projection
                .canonical_turns
                .retain(|turn| turn.session_id == *session_id);
        } else {
            projection.timeline.clear();
            projection.canonical_turns.clear();
        }
        projection.notifications = self
            .session_store
            .notifications_for_context(ws_id, selected_session_id.as_ref());
        BootstrapDto::from_state_with_session_projection(self, projection, event_snapshot)
    }

    pub(crate) fn session_records_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Vec<SessionRecord> {
        let Some(workspace_id) = workspace_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return self
                .session_store
                .sessions()
                .into_iter()
                .filter(session_has_user_content)
                .collect();
        };
        self.session_store
            .sessions()
            .into_iter()
            .filter(session_has_user_content)
            .filter(|session| {
                self.session_workspace_id(session)
                    .as_ref()
                    .map(|bound_workspace_id| bound_workspace_id.as_str())
                    == Some(workspace_id)
            })
            .collect()
    }

    pub(crate) fn session_workspace_id(&self, session: &SessionRecord) -> Option<WorkspaceId> {
        session.workspace_id.as_deref().map(WorkspaceId::new)
    }

    pub(crate) fn workspace_root_path(
        &self,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        let workspace_id = workspace_id.as_ref()?;
        workspace_root_path_from_registry(self.workspace_registry.as_ref(), workspace_id)
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
            .map(PathBuf::from)?;
        let requested_path = canonicalize_path_for_workspace_match(&requested_path);
        self.workspace_registry
            .workspaces()
            .into_iter()
            .find(|workspace| {
                let stored_path = PathBuf::from(workspace.root_path.as_str());
                canonicalize_path_for_workspace_match(&stored_path) == requested_path
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

    /// 从已恢复的审计/用量账本回放每会话最近一次用量观测值。
    ///
    /// 重启容错:守护进程重启后 event-bus 的 `recent_events` 缓冲区为空,只有持久化
    /// 账本里仍保有 `model.usage.recorded`。DTO 装配用这份观测值回填按 sidecar 重建
    /// 的会话,使预算在重启后不至于整体丢失。
    pub fn ledger_usage_observations(
        &self,
    ) -> std::collections::BTreeMap<String, magi_event_bus::SessionRuntimeUsageObservation> {
        let snapshot = self.event_bus.audit_usage_ledger_snapshot();
        latest_usage_observations_from_ledger(&snapshot.usage_entries)
    }

    pub fn audit_usage_ledger_dto(&self) -> AuditUsageLedgerDto {
        self.event_bus.runtime_ledger_summary()
    }

    pub fn bridge_services_dto(&self) -> BridgeServicesSnapshotDto {
        self.bridge_snapshot_provider
            .as_ref()
            .map(|provider| provider.services_snapshot())
            .unwrap_or_else(|| self.bridge_probe_snapshot_provider.services_snapshot())
    }

    pub fn bridge_preflight_dto(&self) -> BridgePreflightSnapshotDto {
        self.bridge_preflight_snapshot_provider.preflight_snapshot()
    }

    pub fn bridge_cutover_smoke_dto(&self) -> BridgeCutoverSmokeSnapshotDto {
        self.bridge_cutover_smoke_provider.cutover_smoke_snapshot()
    }

    pub fn version_handshake_dto(&self) -> VersionHandshakeDto {
        VersionHandshakeDto::from_service_info(&self.service_info)
    }

    pub fn execution_pipeline(&self) -> Option<&ExecutionPipeline> {
        self.execution_pipeline.as_ref()
    }

    pub fn task_execution_registry(&self) -> &TaskExecutionRegistry {
        &self.task_execution_registry
    }

    pub fn settings_snapshot_json(&self) -> serde_json::Value {
        self.settings_snapshot_json_with_mcp_hydration(true)
    }

    pub fn settings_snapshot_json_with_mcp_hydration(
        &self,
        hydrate_mcp_servers: bool,
    ) -> serde_json::Value {
        self.settings_snapshot_json_with_mcp_hydration_and_tool_context(
            hydrate_mcp_servers,
            &ToolExecutionContext::default(),
        )
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
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workers"),
            "orchestratorConfig": object_section(&snapshot, "orchestrator"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliary"),
            "userRulesConfig": object_section(&snapshot, "userRulesConfig"),
            "skillsConfig": skills_config,
            "safeguardConfig": object_section(&snapshot, "safeguardConfig"),
            "repositories": array_section(&snapshot, "repositories"),
            "mcpServers": public_mcp_servers,
            "builtinTools": self.builtin_tools_json(&tool_catalog),
            "capabilityDependencies": self.capability_dependencies_json(&tool_catalog),
            "workerStatuses": object_section(&snapshot, "workerStatuses"),
            "runtimeSettings": runtime_settings_from_snapshot(&snapshot),
            "roleTemplates": builtin_role_templates(),
            "registryEngines": load_registry_engines(self),
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
            r#"{"includeExternal":true,"includeMcpServers":true,"includeAgentRoles":false}"#
        } else {
            r#"{"includeExternal":false,"includeMcpServers":false,"includeAgentRoles":false}"#
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
                    "riskLevel": tool.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
                    "approvalRequirement": tool.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "effectiveApprovalPolicy": tool.get("effective_approval_policy").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "accessProfileBehavior": tool.get("access_profile_behavior").cloned().unwrap_or(serde_json::Value::String("restricted_allowed".to_string())),
                    "accessMode": tool.get("access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
                    "policyScope": tool.get("policy_scope").cloned().unwrap_or(serde_json::Value::String("fixed".to_string())),
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
            let Some(server_id) = entry
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| entry.get("serverId").and_then(|v| v.as_str()))
                .map(str::to_string)
            else {
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
        self.settings_store = store;
        self
    }

    pub fn with_runtime_persistence(mut self, persistence: Arc<RuntimeStatePersistence>) -> Self {
        self.runtime_persistence = Some(persistence);
        self
    }

    pub fn with_session_state_checkpoint_persist(
        mut self,
        persist: SessionStateCheckpointPersist,
    ) -> Self {
        self.session_state_checkpoint_persist = Some(persist);
        self
    }

    pub fn persist_session_state_checkpoint(&self, checkpoint: &str) -> Result<(), ApiError> {
        if let Some(persist) = &self.session_state_checkpoint_persist {
            persist(checkpoint)?;
        }
        self.persist_session_durable_state()
    }

    pub fn persist_session_durable_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };

        let durable = self.session_store.durable_state();
        let (global_state, mut workspace_states) = durable.partition_by_workspace();
        let workspaces = self.workspace_registry.workspaces();
        for workspace in &workspaces {
            let ws_id = workspace.workspace_id.to_string();
            let ws_state = workspace_states.remove(&ws_id).unwrap_or_default();
            let magi_dir = std::path::Path::new(workspace.root_path.as_str()).join(".magi");
            let session_path = magi_dir.join("sessions.json");
            persistence.save_json(&session_path, &ws_state)?;
        }

        let orphan_session_count: usize = workspace_states
            .values()
            .map(|state| state.sessions.len())
            .sum();
        if orphan_session_count > 0 {
            tracing::warn!(
                orphan_session_count,
                "跳过未注册 workspace 的会话持久化；workspace 绑定会话必须写入对应工作区状态"
            );
        }

        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        let global_session_path = state_root.join("sessions.json");
        if global_state.is_empty() {
            match fs::remove_file(&global_session_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ApiError::internal_assembly("删除全局会话状态失败", error));
                }
            }
        } else {
            persistence.save_json(&global_session_path, &global_state)?;
        }

        Ok(())
    }

    pub fn persist_session_durable_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_session_durable_state().map_err(|error| {
            public_runtime_persistence_error("session", SESSION_PERSISTENCE_PUBLIC_ERROR, error)
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

    pub fn persist_knowledge_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_knowledge_state().map_err(|error| {
            public_runtime_persistence_error("knowledge", KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR, error)
        })
    }

    pub fn persist_runtime_durable_state(&self) -> Result<(), ApiError> {
        self.persist_session_durable_state()?;
        self.persist_workspace_durable_state()?;
        self.persist_knowledge_state()?;
        Ok(())
    }

    pub fn persist_runtime_durable_state_for_api(&self) -> Result<(), ApiError> {
        self.persist_session_durable_state_for_api()?;
        self.persist_workspace_durable_state_for_api()?;
        self.persist_knowledge_state_for_api()?;
        Ok(())
    }

    pub fn with_task_store(mut self, store: Arc<TaskStore>) -> Self {
        self.task_store = Some(store);
        self
    }

    pub fn with_spawn_graph(mut self, graph: Arc<Mutex<magi_spawn_graph::SpawnGraph>>) -> Self {
        self.spawn_graph = graph;
        self
    }

    pub fn task_store(&self) -> Option<&TaskStore> {
        self.task_store.as_deref()
    }

    pub fn with_runner_manager(mut self, manager: RunnerManager) -> Self {
        self.runner_manager = Some(manager);
        self
    }

    pub fn with_session_turn_dispatcher(mut self, dispatcher: Arc<LlmTaskDispatcher>) -> Self {
        self.session_turn_dispatcher = Some(dispatcher);
        self
    }

    pub fn session_turn_dispatcher(&self) -> Option<&Arc<LlmTaskDispatcher>> {
        self.session_turn_dispatcher.as_ref()
    }

    pub(crate) fn enqueue_regular_session_turn(&self, turn: QueuedRegularSessionTurn) -> usize {
        let key = SessionTurnQueueKey::new(&turn.session_id, turn.workspace_id.as_ref());
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let queue = queues.entry(key).or_default();
        queue.push_back(turn);
        queue.len()
    }

    pub(crate) fn pop_next_regular_session_turn(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Option<QueuedRegularSessionTurn> {
        let key = SessionTurnQueueKey::new(session_id, workspace_id);
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let queue = queues.get_mut(&key)?;
        let next = queue.pop_front();
        if queue.is_empty() {
            queues.remove(&key);
        }
        next
    }

    pub(crate) fn queued_regular_session_turn_count(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
    ) -> usize {
        let key = SessionTurnQueueKey::new(session_id, workspace_id);
        self.session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned")
            .get(&key)
            .map(VecDeque::len)
            .unwrap_or(0)
    }

    pub(crate) fn clear_regular_session_turn_queue(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
    ) -> usize {
        let key = SessionTurnQueueKey::new(session_id, workspace_id);
        self.session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned")
            .remove(&key)
            .map(|queue| queue.len())
            .unwrap_or(0)
    }

    /// 删除 session 时清空该 session 的全部队列键。不能只按当前 workspace 键删除，
    /// 否则历史错误绑定或无 workspace 的排队消息会成为孤儿。
    pub(crate) fn clear_all_regular_session_turn_queues(&self, session_id: &SessionId) -> usize {
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let mut removed = 0usize;
        queues.retain(|key, queue| {
            if &key.session_id == session_id {
                removed = removed.saturating_add(queue.len());
                false
            } else {
                true
            }
        });
        removed
    }

    /// 会话删除的唯一资源回收入口。先停止并等待后台 runner，再删除所有运行态与
    /// 持久化事实，最后删除 SessionStore 主记录，避免任何组件保留孤儿状态。
    pub async fn delete_session_and_resources(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ApiError> {
        if let Some(manager) = self.runner_manager() {
            manager.unbind_session(session_id).await;
        }
        self.settings_store
            .remove_session(session_id)
            .map_err(crate::errors::settings_persistence_error)?;
        self.clear_all_regular_session_turn_queues(session_id);

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
        task_ids.extend(
            self.task_execution_registry
                .remove_session(session_id)
                .into_iter(),
        );
        if let Some(task_store) = self.task_store() {
            for task_id in task_ids.clone() {
                if let Some(task) = task_store.get_task(&task_id) {
                    mission_ids.insert(task.mission_id);
                }
            }
            for mission_id in mission_ids {
                task_ids.extend(
                    task_store
                        .remove_tasks_by_mission(&mission_id)
                        .into_iter()
                        .map(|task| task.task_id),
                );
            }
            for task_id in task_ids.clone() {
                let _ = task_store.remove_task(&task_id);
            }
        }
        self.spawn_graph
            .lock()
            .map_err(|error| ApiError::internal_assembly("清理会话 SpawnGraph 失败", error))?
            .remove_tasks(&task_ids);
        self.conversation_registry.remove_session(session_id);
        self.session_store
            .delete_session(session_id)
            .map_err(|error| ApiError::internal_assembly("删除会话失败", error))
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_real_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self.model_bridge_client_is_real = true;
        self
    }

    pub fn with_skill_runtime(
        mut self,
        skill_runtime: Arc<magi_skill_runtime::SkillRuntime>,
    ) -> Self {
        self.skill_runtime = Some(skill_runtime);
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
        mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
    ) -> Self {
        self.mcp_connections = mcp_connections;
        self
    }

    pub fn runner_manager(&self) -> Option<&RunnerManager> {
        self.runner_manager.as_ref()
    }

    pub fn mcp_connections(&self) -> &Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>> {
        &self.mcp_connections
    }

    pub fn model_bridge_client(&self) -> Option<&Arc<dyn ModelBridgeClient>> {
        self.model_bridge_client.as_ref()
    }

    pub fn model_bridge_client_is_real(&self) -> bool {
        self.model_bridge_client_is_real
    }
}

fn normalize_settings_snapshot_sections(snapshot: &mut HashMap<String, serde_json::Value>) {
    for key in ["orchestrator", "auxiliary", "safeguardConfig"] {
        if let Some(value) = snapshot.get_mut(key) {
            strip_scope_binding_fields(value);
            if key == "orchestrator" {
                strip_orchestrator_session_owned_fields(value);
            }
        }
    }
    skill_loader::normalize_skills_config_sections(snapshot);
    seed_user_rules_config(snapshot);
    normalize_mcp_servers_section(snapshot);
    seed_default_safeguard_rules(snapshot);
    normalize_safeguard_config_section(snapshot);
}

fn strip_orchestrator_session_owned_fields(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("model");
    object.remove("reasoningEffort");
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
    serde_json::json!({
        "name": raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "status": raw.get("status").cloned().unwrap_or(serde_json::Value::String("unknown".to_string())),
        "requiredBy": capability_dependency_field(raw, "required_by")
            .unwrap_or_else(|| serde_json::json!([])),
        "workspaceId": capability_dependency_field(raw, "workspace_id"),
        "sessionId": capability_dependency_field(raw, "session_id"),
        "fileCount": capability_dependency_field(raw, "file_count"),
        "lastIndexed": capability_dependency_field(raw, "last_indexed"),
        "cacheStatus": capability_dependency_field(raw, "cache_status"),
        "roleCount": capability_dependency_field(raw, "role_count"),
        "spawnableRoleCount": capability_dependency_field(raw, "spawnable_role_count"),
        "snapshotActive": capability_dependency_field(raw, "snapshot_active"),
        "configuredCount": capability_dependency_field(raw, "configured_count"),
        "enabledCount": capability_dependency_field(raw, "enabled_count"),
        "readyCount": capability_dependency_field(raw, "ready_count"),
        "enabledToolCount": capability_dependency_field(raw, "enabled_tool_count"),
        "readyToolCount": capability_dependency_field(raw, "ready_tool_count"),
        "toolCount": capability_dependency_field(raw, "tool_count"),
    })
}

fn normalize_public_tool_catalog_item_json(raw: &serde_json::Value) -> serde_json::Value {
    let mut item = serde_json::json!({
        "name": raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "category": raw.get("category").cloned().unwrap_or(serde_json::Value::Null),
        "public": raw.get("public").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "runtimeInternal": raw.get("runtime_internal").cloned().unwrap_or(serde_json::Value::Bool(false)),
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
    item
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
    serde_json::json!({
        "serverId": raw.get("server_id").cloned().unwrap_or(serde_json::Value::Null),
        "serverName": raw.get("server_name").cloned().unwrap_or(serde_json::Value::Null),
        "modelToolName": raw.get("model_tool_name").cloned().unwrap_or(serde_json::Value::Null),
        "toolName": raw.get("tool_name").cloned().unwrap_or(serde_json::Value::Null),
        "description": raw.get("description").cloned().unwrap_or(serde_json::Value::String(String::new())),
        "readOnly": raw.get("read_only").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "inputSchema": raw.get("input_schema").cloned().unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} })),
    })
}

fn normalize_public_agent_role_catalog_json(raw: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "roleId": raw.get("role_id").cloned().unwrap_or(serde_json::Value::Null),
        "spawnable": raw.get("spawnable").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "coordinatorMode": raw.get("coordinator_mode").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "supportedKinds": raw.get("supported_kinds").cloned().unwrap_or_else(|| serde_json::json!([])),
        "parallelismLimit": raw.get("parallelism_limit").cloned().unwrap_or(serde_json::Value::Null),
        "status": raw.get("status").cloned().unwrap_or(serde_json::Value::String("unknown".to_string())),
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
    snapshot
        .get("mcpServers")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            serde_json::Value::Array(
                items
                    .iter()
                    .cloned()
                    .map(redact_mcp_server_public_entry)
                    .collect(),
            )
        })
        .unwrap_or_else(|| serde_json::json!([]))
}

fn runtime_settings_from_snapshot(
    snapshot: &HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    let runtime = snapshot
        .get("runtimeSettings")
        .and_then(|value| value.as_object());
    let locale = runtime
        .and_then(|value| value.get("locale"))
        .and_then(|value| value.as_str())
        .or_else(|| snapshot.get("locale").and_then(|value| value.as_str()))
        .filter(|value| matches!(*value, "zh-CN" | "en-US"))
        .unwrap_or("zh-CN");
    serde_json::json!({
        "locale": locale,
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

fn normalize_user_rules_config_value(mut value: serde_json::Value) -> serde_json::Value {
    strip_scope_binding_fields(&mut value);
    match value {
        serde_json::Value::String(user_rules) => serde_json::json!({ "userRules": user_rules }),
        serde_json::Value::Object(_) => value,
        _ => serde_json::json!({}),
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
    object.insert(
        "rules".to_string(),
        serde_json::Value::Array(normalized_rules),
    );
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

fn builtin_safeguard_rules() -> Vec<serde_json::Value> {
    // 单一事实源：magi-safety-gate::builtin_rules() 持有内置危险模式集合。
    // 这里只做"规则结构 → settings JSON 形态"的转换，便于前端读取与编辑。
    magi_safety_gate::builtin_rules()
        .into_iter()
        .map(safeguard_rule_json)
        .collect()
}

fn seed_default_safeguard_rules(snapshot: &mut HashMap<String, serde_json::Value>) {
    if !snapshot.contains_key("safeguardConfig") {
        snapshot.insert("safeguardConfig".to_string(), serde_json::json!({}));
    }

    let safeguard = snapshot
        .get_mut("safeguardConfig")
        .expect("safeguardConfig just inserted");
    if !safeguard.is_object() {
        *safeguard = serde_json::json!({});
    }

    let existing_rules = safeguard
        .get("rules")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let has_builtin = existing_rules.iter().any(|r| {
        r.get("category")
            .and_then(|v| v.as_str())
            .is_some_and(|c| c != "custom")
    });

    if has_builtin {
        return;
    }

    let mut all_rules = builtin_safeguard_rules();
    all_rules.extend(existing_rules);
    safeguard["rules"] = serde_json::Value::Array(all_rules);
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

#[cfg(test)]
mod tests {
    use super::*;
    use magi_agent_role::{AgentRole, AgentRoleRegistry, TaskKindLabel};
    use magi_core::{AbsolutePath, MissionId, Task, TaskKind, WorkerId};
    use magi_orchestrator::task_store::TaskLease;
    use std::collections::HashMap;
    use std::time::Duration;

    fn task_with_status(task_id: &str, status: TaskStatus) -> Task {
        let now = UtcMillis::now();
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-stable-waiting-state"),
            root_task_id: TaskId::new("task-root-stable-waiting-state"),
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "等待确认".to_string(),
            goal: "等待用户确认后继续".to_string(),
            status,
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
            created_at: now,
            updated_at: now,
        }
    }

    struct RecordingDispatcher {
        observed_role: Arc<Mutex<Option<String>>>,
    }

    impl TaskDispatcher for RecordingDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            worker: &WorkerInfo,
            _lease: &TaskLease,
        ) -> Result<(), String> {
            *self
                .observed_role
                .lock()
                .expect("observed role lock should not poison") = Some(worker.role.clone());
            Ok(())
        }
    }

    fn test_agent_role(id: &str) -> AgentRole {
        AgentRole {
            id: id.to_string(),
            system_prompt: format!("{id} prompt"),
            supported_kinds: vec![TaskKindLabel::LocalAgent],
            parallelism_limit: None,
            coordinator_mode: false,
            version: 1,
        }
    }

    #[test]
    fn runner_manager_uses_injected_agent_role_registry_for_worker_matching() {
        let store = Arc::new(TaskStore::new());
        let mut task = task_with_status("task-custom-agent-role", TaskStatus::Pending);
        task.root_task_id = task.task_id.clone();
        task.executor_binding = Some(magi_core::TaskExecutorBinding::for_role("auditor"));
        let root_task_id = task.root_task_id.clone();
        store.insert_task(task);

        let observed_role = Arc::new(Mutex::new(None));
        let dispatcher = Arc::new(RecordingDispatcher {
            observed_role: observed_role.clone(),
        });
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store,
            Arc::new(|| {
                vec![
                    WorkerInfo {
                        worker_id: WorkerId::new("worker-executor"),
                        role: "executor".to_string(),
                        supported_kinds: vec![TaskKind::LocalAgent],
                        parallelism_limit: None,
                        system_prompt_template: None,
                    },
                    WorkerInfo {
                        worker_id: WorkerId::new("worker-auditor"),
                        role: "auditor".to_string(),
                        supported_kinds: vec![TaskKind::LocalAgent],
                        parallelism_limit: None,
                        system_prompt_template: None,
                    },
                ]
            }),
            dispatcher,
            Arc::new(EventBasedResultReceiver::new()),
        )
        .with_agent_role_registry(Arc::new(AgentRoleRegistry::from_map(HashMap::from([
            ("executor".to_string(), test_agent_role("executor")),
            ("auditor".to_string(), test_agent_role("auditor")),
        ]))));

        let outcome = manager.build_task_runner().run_cycle(&root_task_id);

        assert_eq!(outcome, RunCycleOutcome::Continue);
        assert_eq!(
            observed_role
                .lock()
                .expect("observed role lock should not poison")
                .as_deref(),
            Some("auditor")
        );
    }

    #[tokio::test]
    async fn unbind_session_waits_for_blocked_runner_and_removes_handle() {
        let store = Arc::new(TaskStore::new());
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store,
            Arc::new(Vec::new),
            Arc::new(RecordingDispatcher {
                observed_role: Arc::new(Mutex::new(None)),
            }),
            Arc::new(EventBasedResultReceiver::new()),
        );
        let session_id = SessionId::new("session-blocked-runner-cleanup");
        let root_task_id = "task-blocked-runner-cleanup";
        let cancel = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(true));
        let background_cancel = cancel.clone();
        let background_active = active.clone();
        let join_handle = tokio::spawn(async move {
            while !background_cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            background_active.store(false, Ordering::Relaxed);
        });
        manager.runners.lock().expect("runners should lock").insert(
            root_task_id.to_string(),
            Arc::new(RunnerHandle {
                cancel,
                active,
                cycle_count: Arc::new(AtomicU64::new(0)),
                status: Arc::new(Mutex::new("blocked".to_string())),
                last_error: Arc::new(Mutex::new(Some("等待输入".to_string()))),
                join_handle: Mutex::new(Some(join_handle)),
            }),
        );
        manager.bind_session(session_id.clone(), root_task_id);

        assert_eq!(manager.unbind_session(&session_id).await, 1);
        assert!(manager.status(root_task_id).is_none());
    }

    #[test]
    fn builtin_tools_json_does_not_assume_missing_runtime_status_ready() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let catalog = serde_json::json!({
            "tools": [
                {
                    "name": "file_read",
                    "public": true,
                    "runtime_status": "ready"
                },
                {
                    "name": "tool_catalog",
                    "public": true
                },
                {
                    "name": "shell_exec",
                    "public": true,
                    "runtime_status": " "
                }
            ]
        });

        let tools = state.builtin_tools_json(&catalog);
        let tools = tools.as_array().expect("builtin tools should be an array");

        assert_eq!(tools[0]["runtimeStatus"], serde_json::json!("ready"));
        assert_eq!(tools[1]["runtimeStatus"], serde_json::json!("unknown"));
        assert_eq!(tools[2]["runtimeStatus"], serde_json::json!("unknown"));
    }

    #[tokio::test]
    async fn snapshot_lifecycle_replay_skips_unregistered_workspace_sessions() {
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let workspace_root =
            std::env::temp_dir().join(format!("magi-api-snapshot-replay-{}", UtcMillis::now().0));
        std::fs::create_dir_all(&workspace_root).expect("workspace root should create");
        let registered_workspace_id = WorkspaceId::new("workspace-snapshot-replay-known");
        workspace_store
            .register(
                registered_workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
        let known_session_id = SessionId::new("session-snapshot-replay-known");
        let orphan_session_id = SessionId::new("session-snapshot-replay-orphan");
        session_store
            .create_session_for_workspace(
                known_session_id.clone(),
                "known",
                Some(registered_workspace_id.to_string()),
            )
            .expect("known session should create");
        session_store
            .create_session_for_workspace(
                orphan_session_id.clone(),
                "orphan",
                Some("workspace-snapshot-replay-missing".to_string()),
            )
            .expect("orphan session should create");
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        );

        state.install_snapshot_lifecycle_observer();
        tokio::task::yield_now().await;

        assert!(
            state
                .snapshot_manager
                .get_session(known_session_id.as_str())
                .is_some(),
            "registered workspace session should replay into snapshot lifecycle"
        );
        assert!(
            state
                .snapshot_manager
                .get_session(orphan_session_id.as_str())
                .is_none(),
            "unregistered workspace session should not start a stale snapshot lifecycle"
        );
        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn stalled_outcome_with_failed_task_is_terminally_unrunnable() {
        let store = TaskStore::new();
        let failed_id = TaskId::new("task-failed");
        store.insert_task(task_with_status(failed_id.as_str(), TaskStatus::Failed));

        assert!(stalled_outcome_is_terminally_unrunnable(
            &store,
            &[failed_id]
        ));
    }

    #[test]
    fn stalled_outcome_with_pending_task_still_uses_debounce() {
        let store = TaskStore::new();
        let pending_id = TaskId::new("task-pending-unmatched");
        store.insert_task(task_with_status(pending_id.as_str(), TaskStatus::Pending));

        assert!(!stalled_outcome_is_terminally_unrunnable(
            &store,
            &[pending_id]
        ));
    }

    #[test]
    fn public_skills_config_section_hides_directory_paths() {
        let temp = tempfile::tempdir().expect("temp skill dir should create");
        std::fs::write(
            temp.path().join("SKILL.md"),
            "# local-skill\n\n请输出 local-skill。\n",
        )
        .expect("skill markdown should write");
        let missing_dir = temp.path().join("missing-skill");
        let public = public_skills_config_section(serde_json::json!({
            "instructionSkills": [
                {
                    "name": "local-skill",
                    "skillId": "local-skill",
                    "directoryPath": temp.path().to_string_lossy().to_string(),
                    "description": "desc"
                },
                {
                    "name": "missing-skill",
                    "skillId": "missing-skill",
                    "directoryPath": missing_dir.to_string_lossy().to_string(),
                    "description": "stale"
                }
            ],
            "customTools": [
                {
                    "name": "custom-tool"
                }
            ]
        }));

        let skill = public["instructionSkills"][0]
            .as_object()
            .expect("skill should stay object");
        assert_eq!(skill["name"], serde_json::json!("local-skill"));
        assert!(!skill.contains_key("directoryPath"));
        assert_eq!(
            public["instructionSkills"].as_array().map(Vec::len),
            Some(1),
            "unavailable local skills should not be exposed as selectable instructions"
        );
        assert_eq!(public["customTools"][0]["name"], "custom-tool");
    }

    #[test]
    fn session_state_checkpoint_runs_callback_and_persists_durable_state() {
        let state_root = std::env::temp_dir().join(format!(
            "magi-api-session-checkpoint-{}",
            UtcMillis::now().0
        ));
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let session_id = SessionId::new("session-checkpoint-durable");
        session_store
            .create_session(session_id.clone(), "checkpoint durable")
            .expect("session should create");
        let observed_checkpoints = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed_for_callback = observed_checkpoints.clone();

        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        )
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            state_root.join("sessions.json"),
            state_root.join("workspaces.json"),
            state_root.join("knowledge.json"),
        )))
        .with_session_state_checkpoint_persist(Arc::new(move |checkpoint| {
            observed_for_callback
                .lock()
                .expect("checkpoint observer lock should not poison")
                .push(checkpoint.to_string());
            Ok(())
        }));

        state
            .persist_session_state_checkpoint("checkpoint-test")
            .expect("checkpoint should persist");

        assert_eq!(
            observed_checkpoints
                .lock()
                .expect("checkpoint observer lock should not poison")
                .as_slice(),
            ["checkpoint-test"]
        );
        let persisted = std::fs::read_to_string(state_root.join("sessions.json"))
            .expect("global session durable state should be written");
        assert!(persisted.contains(session_id.as_str()));
    }

    #[test]
    fn session_durable_persistence_drops_orphan_workspace_sessions() {
        let state_root = std::env::temp_dir().join(format!(
            "magi-api-orphan-session-persistence-{}",
            UtcMillis::now().0
        ));
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let session_id = SessionId::new("session-orphan-workspace-current");
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "orphan workspace",
                Some("workspace-missing-current".to_string()),
            )
            .expect("session should create");

        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        )
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            state_root.join("sessions.json"),
            state_root.join("workspaces.json"),
            state_root.join("knowledge.json"),
        )));

        state
            .persist_session_durable_state()
            .expect("session durable state should persist");
        assert!(
            !state_root.join("sessions.json").exists(),
            "未注册 workspace 的绑定会话不能写回全局 sessions.json"
        );
        let _ = std::fs::remove_dir_all(state_root);
    }

    #[test]
    fn api_persistence_wrappers_redact_runtime_errors() {
        let state_root = std::env::temp_dir().join(format!(
            "magi-api-redacted-persistence-{}",
            UtcMillis::now().0
        ));
        let session_path = state_root.join("sessions.json");
        let workspace_path = state_root.join("workspaces.json");
        let knowledge_path = state_root.join("knowledge.json");
        std::fs::create_dir_all(&session_path).expect("session conflict dir should create");
        std::fs::create_dir_all(&workspace_path).expect("workspace conflict dir should create");
        std::fs::create_dir_all(&knowledge_path).expect("knowledge conflict dir should create");

        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
            session_path,
            workspace_path,
            knowledge_path,
        )));

        assert_public_persistence_error(
            state
                .persist_session_durable_state_for_api()
                .expect_err("session persistence should fail"),
            SESSION_PERSISTENCE_PUBLIC_ERROR,
        );
        assert_public_persistence_error(
            state
                .persist_workspace_durable_state_for_api()
                .expect_err("workspace persistence should fail"),
            WORKSPACE_PERSISTENCE_PUBLIC_ERROR,
        );
        assert_public_persistence_error(
            state
                .persist_knowledge_state_for_api()
                .expect_err("knowledge persistence should fail"),
            KNOWLEDGE_PERSISTENCE_PUBLIC_ERROR,
        );

        let _ = std::fs::remove_dir_all(state_root);
    }

    fn assert_public_persistence_error(error: ApiError, expected_message: &str) {
        let ApiError::InternalAssemblyError(message) = error else {
            panic!("expected internal assembly error");
        };
        assert_eq!(message, expected_message);
        assert!(!message.contains("os error"));
        assert!(!message.contains("Is a directory"));
        assert!(!message.contains("Permission denied"));
        assert!(!message.contains(".json"));
    }

    #[tokio::test]
    async fn bootstrap_workspace_session_selects_latest_visible_history() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let workspace_root = std::env::temp_dir().join(format!(
            "magi-api-bootstrap-default-history-{}",
            UtcMillis::now().0
        ));
        std::fs::create_dir_all(&workspace_root).expect("workspace root should create");
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store.clone(),
            workspace_store.clone(),
            governance,
        );
        let workspace_id = WorkspaceId::new("workspace-bootstrap-default-history");
        workspace_store
            .register(
                workspace_id.clone(),
                AbsolutePath::new(workspace_root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
        session_store
            .create_session_for_workspace(
                SessionId::new("session-empty-bootstrap-history"),
                "空白会话",
                Some(workspace_id.to_string()),
            )
            .expect("empty session should create");

        let older_session_id = SessionId::new("session-bootstrap-older");
        session_store
            .create_session_for_workspace(
                older_session_id.clone(),
                "较早历史",
                Some(workspace_id.to_string()),
            )
            .expect("older session should create");
        session_store.append_timeline_entry(
            older_session_id,
            magi_session_store::TimelineEntryKind::UserMessage,
            "较早消息",
        );
        std::thread::sleep(Duration::from_millis(2));

        let newer_session_id = SessionId::new("session-bootstrap-newer");
        session_store
            .create_session_for_workspace(
                newer_session_id.clone(),
                "较新历史",
                Some(workspace_id.to_string()),
            )
            .expect("newer session should create");
        session_store.append_timeline_entry(
            newer_session_id.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "较新消息",
        );
        state
            .snapshot_manager
            .start_session(
                newer_session_id.as_str().to_string(),
                workspace_root.clone(),
            )
            .await
            .expect("selected session snapshot should start");

        let bootstrap = state
            .bootstrap_dto_for_workspace_session(Some(workspace_id.as_str()), None)
            .expect("bootstrap should build");

        assert_eq!(
            bootstrap
                .current_session
                .as_ref()
                .map(|session| session.session_id.clone()),
            Some(newer_session_id)
        );
        assert_eq!(bootstrap.sessions.len(), 2);
        assert!(
            bootstrap
                .sessions
                .iter()
                .all(|session| session.message_count.unwrap_or(0) > 0)
        );
        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn bootstrap_workspace_session_ignores_foreign_requested_session() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_a = WorkspaceId::new("workspace-bootstrap-a");
        let workspace_b = WorkspaceId::new("workspace-bootstrap-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-workspace-a"),
            )
            .expect("workspace A should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-workspace-b"),
            )
            .expect("workspace B should register");

        let session_a = SessionId::new("session-bootstrap-workspace-a");
        state
            .session_store
            .create_session_for_workspace(
                session_a.clone(),
                "A 会话",
                Some(workspace_a.to_string()),
            )
            .expect("session A should create");
        state.session_store.append_timeline_entry(
            session_a.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "A 消息",
        );
        let session_b = SessionId::new("session-bootstrap-workspace-b");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "B 会话",
                Some(workspace_b.to_string()),
            )
            .expect("session B should create");
        state.session_store.append_timeline_entry(
            session_b.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "B 消息",
        );

        let bootstrap = state
            .bootstrap_dto_for_workspace_session(Some(workspace_a.as_str()), Some(&session_b))
            .expect("bootstrap should build");

        assert_eq!(
            bootstrap
                .current_session
                .as_ref()
                .map(|session| session.session_id.clone()),
            Some(session_a.clone())
        );
        assert_eq!(bootstrap.sessions.len(), 1);
        assert_eq!(bootstrap.sessions[0].session_id, session_a);
        assert!(
            bootstrap
                .timeline
                .iter()
                .all(|entry| entry.session_id == session_a)
        );
    }

    #[test]
    fn resolve_workspace_id_from_request_rejects_unknown_stale_workspace_id() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-known-from-path");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-known-from-path"),
            )
            .expect("workspace should register");
        state
            .workspace_registry
            .register(
                WorkspaceId::new("workspace-stale-registered-url"),
                AbsolutePath::new("/tmp/magi-stale-registered-url"),
            )
            .expect("stale workspace should register");

        assert_eq!(
            state.resolve_workspace_id_from_request(
                Some(WorkspaceId::new("workspace-stale-url")),
                None,
            ),
            None
        );
        assert_eq!(
            state.resolve_workspace_id_from_request(
                Some(WorkspaceId::new("workspace-stale-url")),
                Some("/tmp/magi-known-from-path"),
            ),
            Some(workspace_id.clone())
        );
        assert_eq!(
            state.resolve_workspace_id_from_request(
                Some(WorkspaceId::new("workspace-stale-registered-url")),
                Some("/tmp/magi-known-from-path"),
            ),
            Some(workspace_id)
        );
    }

    #[test]
    fn file_snapshot_dependency_does_not_trust_working_directory_for_unregistered_workspace_id() {
        let snapshot_manager = SnapshotManager::new();
        let workspace_store = WorkspaceStore::default();
        let workspace_root = std::env::temp_dir().join(format!(
            "magi-api-file-snapshot-unregistered-{}",
            UtcMillis::now().0
        ));
        std::fs::create_dir_all(&workspace_root).expect("workspace root should create");

        let entry = file_snapshot_capability_dependency(
            &snapshot_manager,
            &workspace_store,
            &ToolExecutionContext {
                session_id: Some(SessionId::new("session-file-snapshot-unregistered")),
                workspace_id: Some(WorkspaceId::new("workspace-file-snapshot-unregistered")),
                working_directory: Some(workspace_root.clone()),
                ..ToolExecutionContext::default()
            },
        );

        assert_eq!(entry.status, "unavailable");
        assert_eq!(
            entry.workspace_id.as_deref(),
            Some("workspace-file-snapshot-unregistered")
        );
        assert_eq!(
            entry.session_id.as_deref(),
            Some("session-file-snapshot-unregistered")
        );
        assert_eq!(entry.snapshot_active, Some(false));

        let _ = std::fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn capability_dependency_json_preserves_mcp_tool_count_semantics() {
        let raw = serde_json::json!({
            "name": "mcp_servers",
            "status": "not_ready",
            "required_by": ["mcp custom tools"],
            "configured_count": 1,
            "enabled_count": 1,
            "ready_count": 0,
            "enabled_tool_count": 7,
            "ready_tool_count": 0,
            "tool_count": 0,
        });

        let normalized = normalize_capability_dependency_json(&raw);

        assert_eq!(normalized["enabledToolCount"], serde_json::json!(7));
        assert_eq!(normalized["readyToolCount"], serde_json::json!(0));
        assert_eq!(
            normalized["toolCount"],
            serde_json::json!(0),
            "toolCount must remain the ready/usable tool count in settings bootstrap"
        );
    }

    #[test]
    fn public_tool_catalog_response_uses_camel_case_boundary() {
        let raw = serde_json::json!({
            "tool": "tool_catalog",
            "status": "succeeded",
            "catalog_access_mode": "read_only",
            "current_access_profile": "full_access",
            "runtime_dependencies": [{
                "name": "mcp_servers",
                "status": "ready",
                "required_by": ["mcp custom tools"],
                "enabled_tool_count": 2,
                "tool_count": 2
            }],
            "tools": [{
                "name": "shell_exec",
                "category": "builtin",
                "public": true,
                "runtime_internal": false,
                "access_mode": "explicit_write",
                "policy_scope": "input_sensitive",
                "input_sensitive_policy": true,
                "policy_summary": "summary",
                "risk_level": "high",
                "approval_requirement": "required",
                "effective_approval_policy": "none",
                "access_profile_behavior": "full_access_allowed",
                "schema_status": "ok",
                "schema_warnings": [],
                "runtime_status": "ready",
                "runtime_warnings": [],
                "parameters_schema": {"type": "object", "properties": {"old_string": {"type": "string"}}}
            }],
            "skill_tools": [{
                "source": "skill",
                "skill_id": "skill-1",
                "binding_id": "binding-1",
                "name": "skill.tool",
                "description": "tool",
                "bridge_kind": "skill",
                "dispatch_action": "run",
                "bridge_target": "skill-1",
                "access_profile_behavior": "restricted_allowed",
                "risk_level": "low",
                "approval_requirement": "none",
                "status": "available"
            }],
            "mcp_servers": [{
                "server_id": "mcp-1",
                "name": "mcp",
                "enabled": true,
                "connected": true,
                "health": "connected",
                "tool_count": 3,
                "error": null
            }],
            "mcp_tools": [{
                "server_id": "mcp-1",
                "server_name": "mcp",
                "model_tool_name": "mcp__mcp-1__inspect",
                "tool_name": "inspect",
                "description": "inspect",
                "read_only": true,
                "input_schema": {"type": "object", "properties": {}}
            }],
            "agent_roles": [{
                "role_id": "executor",
                "spawnable": true,
                "coordinator_mode": false,
                "supported_kinds": ["local_agent"],
                "parallelism_limit": 2,
                "status": "ready"
            }]
        });

        let public = public_tool_catalog_response_json(&raw);

        assert!(public.get("runtime_dependencies").is_none());
        assert!(public.get("catalog_access_mode").is_none());
        assert_eq!(public["catalogAccessMode"], serde_json::json!("read_only"));
        assert_eq!(
            public["currentAccessProfile"],
            serde_json::json!("full_access")
        );
        assert_eq!(
            public["runtimeDependencies"][0]["requiredBy"][0],
            "mcp custom tools"
        );
        assert_eq!(public["runtimeDependencies"][0]["enabledToolCount"], 2);
        assert_eq!(public["tools"][0]["effectiveApprovalPolicy"], "none");
        assert_eq!(
            public["tools"][0]["parametersSchema"]["properties"]["old_string"]["type"],
            "string"
        );
        assert_eq!(public["skillTools"][0]["skillId"], "skill-1");
        assert_eq!(
            public["skillTools"][0]["accessProfileBehavior"],
            "restricted_allowed"
        );
        assert_eq!(public["mcpServers"][0]["serverId"], "mcp-1");
        assert_eq!(public["mcpServers"][0]["toolCount"], 3);
        assert_eq!(public["mcpTools"][0]["readOnly"], true);
        assert_eq!(public["agentRoles"][0]["roleId"], "executor");
        assert_eq!(public["agentRoles"][0]["parallelismLimit"], 2);
    }
}
