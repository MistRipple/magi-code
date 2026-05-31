use crate::dto::{
    AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeProvider, BridgeCutoverSmokeSnapshotDto,
    BridgeCutoverSmokeSnapshotProvider, BridgePreflightProvider, BridgePreflightSnapshotDto,
    BridgePreflightSnapshotProvider, BridgeProbeSnapshotProvider, BridgeServicesSnapshotDto,
    BridgeSnapshotProvider, DirectHttpModelProbeConfig, HealthDto, MissionAggregateExport,
    RuntimeReadModelDto, ServiceInfo, VersionHandshakeDto, runtime_read_model_dto,
};
use crate::errors::ApiError;
use crate::mcp_config::{build_mcp_config_from_entry, normalize_mcp_server_snapshot_entry};
use crate::routes::settings::{
    builtin_role_templates, load_registry_engines, registered_role_template_ids,
    resolve_registry_agents,
};
use crate::settings_store::SettingsStore;
use crate::skill_loader;
use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient, ModelBridgeClient,
    StdioMcpBridgeClient,
};
use magi_conversation_runtime::{
    ConversationRegistry,
    task_execution_dispatcher::{ExecutionPipeline, LlmTaskDispatcher},
    task_execution_registry::TaskExecutionRegistry,
    task_runner::TaskRunner,
    task_runner_bridge::{
        EventBasedResultReceiver, RunCycleOutcome, TaskDispatchGate, TaskDispatcher,
        TaskResultReceiver,
    },
};
use magi_core::{
    SessionId, SessionLifecycleStatus, TaskId, TaskStatus, UtcMillis, WorkspaceId,
    WorkspaceRootPath,
};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_memory_store::MemoryStore;
use magi_mission::{enumerate_resumable_missions, resume_mission};
use magi_orchestrator::{
    OrchestratedExecutionRuntime, OrchestratorService,
    task_store::TaskStore,
    task_worker_catalog::{WorkerInfo, build_worker_catalog_for_roles},
};
use magi_session_store::{SessionLifecycleObserver, SessionRecord, SessionStore};
use magi_snapshot::{SnapshotManager, SnapshotSession};
use magi_tool_runtime::{
    RuntimeCapabilityDependencyEntry, RuntimeCapabilityDependencyProvider, ToolExecutionContext,
    ToolExecutionContextQuery, ToolRegistry,
};
use magi_workspace::WorkspaceStore;
use std::collections::HashMap;
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
}

type RunnerTerminalObserver = Arc<dyn Fn(TaskId, Option<SessionId>, String) + Send + Sync>;
pub type SessionStateCheckpointPersist = Arc<dyn Fn(&str) -> Result<(), ApiError> + Send + Sync>;

pub(crate) fn session_has_user_content(session: &SessionRecord) -> bool {
    session.message_count.unwrap_or(0) > 0
}

/// Manages active Runner instances keyed by root_task_id.
#[derive(Clone)]
pub struct RunnerManager {
    runners: Arc<Mutex<HashMap<String, Arc<RunnerHandle>>>>,
    task_store: Arc<TaskStore>,
    worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
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
        if let Some(existing) = runners.get(root_task_id) {
            if existing.active.load(Ordering::Relaxed) {
                return Err(RunnerStartError::AlreadyRunning);
            }
        }

        let handle = Arc::new(RunnerHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(true)),
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new("running".to_string())),
            last_error: Arc::new(Mutex::new(None)),
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
        tokio::spawn(async move {
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
                                    _ => cycle % CHECKPOINT_INTERVAL_CYCLES == 0,
                                }
                            } else {
                                cycle % CHECKPOINT_INTERVAL_CYCLES == 0
                            }
                        } else {
                            cycle % CHECKPOINT_INTERVAL_CYCLES == 0
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

        Ok(handle)
    }

    /// Signal a runner to stop.
    pub fn stop(&self, root_task_id: &str) -> Result<(), RunnerStopError> {
        let runners = self.runners.lock().expect("runners lock should hold");
        let handle = runners.get(root_task_id).ok_or(RunnerStopError::NotFound)?;
        let mut status = handle.status.lock().expect("status lock should hold");
        if *status != "running" {
            return Err(RunnerStopError::NotRunning);
        }
        handle.cancel.store(true, Ordering::Relaxed);
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
    pub fn unbind_session(&self, session_id: &SessionId) {
        let root_task_ids = {
            let mut index = self
                .session_runner_index
                .lock()
                .expect("session_runner_index lock should hold");
            index.remove(session_id).unwrap_or_default()
        };
        for root_task_id in root_task_ids {
            let _ = self.stop(&root_task_id);
        }
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
}

#[derive(Clone, Debug)]
pub struct RuntimeStatePersistence {
    session_path: PathBuf,
    workspace_path: PathBuf,
    knowledge_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

static RUNTIME_PERSISTENCE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
        let temp_path = temp_path_for(path);
        let payload = serde_json::to_vec_pretty(value)
            .map_err(|error| ApiError::internal_assembly("序列化运行态持久化数据失败", error))?;
        fs::write(&temp_path, payload)
            .map_err(|error| ApiError::internal_assembly("写入运行态持久化临时文件失败", error))?;
        fs::rename(&temp_path, path)
            .map_err(|error| ApiError::internal_assembly("提交运行态持久化文件失败", error))?;
        Ok(())
    }

    fn save_workspace_store(&self, store: &WorkspaceStore) -> Result<(), ApiError> {
        self.save_json(&self.workspace_path, &store.durable_state())
    }

    fn save_knowledge_store(&self, store: &KnowledgeStore) -> Result<(), ApiError> {
        self.save_json(&self.knowledge_path, &store.export_state())
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "runtime-state.json".to_string());
    let nonce = RUNTIME_PERSISTENCE_TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    file_name.push_str(&format!(".{nonce}.tmp"));
    path.with_file_name(file_name)
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
    let has_workspace_root = context
        .workspace_id
        .as_ref()
        .and_then(|workspace_id| {
            workspace_root_path_from_registry(workspace_registry, workspace_id)
        })
        .or_else(|| context.working_directory.clone())
        .is_some();
    let snapshot_active = session_id
        .as_deref()
        .is_some_and(|session_id| snapshot_manager.get_session(session_id).is_some());
    let status = if session_id.is_none() || workspace_id.is_none() {
        "missing_context"
    } else if snapshot_active {
        "ready"
    } else if has_workspace_root {
        "not_ready"
    } else {
        "unavailable"
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
        }
    }

    /// 安装 SessionLifecycleObserver，把 session 创建/归档/删除事件桥接到 SnapshotManager。
    pub fn install_snapshot_lifecycle_observer(&self) {
        let observer = Arc::new(crate::snapshot_lifecycle::SnapshotLifecycleObserver::new(
            self.snapshot_manager.clone(),
            self.workspace_registry.clone(),
        ));
        self.session_store.set_lifecycle_observer(observer.clone());
        for session in self.session_store.sessions() {
            if session.status == SessionLifecycleStatus::Active {
                observer.on_session_created(&session.session_id, session.workspace_id.as_deref());
            }
        }
    }

    /// 同步取 session 对应的 SnapshotSession。未装载表示生命周期接线异常，调用方应显式报错。
    pub fn snapshot_session(&self, session_id: &SessionId) -> Option<Arc<SnapshotSession>> {
        self.snapshot_manager.get_session(session_id.as_str())
    }

    pub(crate) async fn ensure_snapshot_session(
        &self,
        session_id: &SessionId,
        workspace_root: &Path,
    ) -> Result<Arc<SnapshotSession>, ApiError> {
        if let Some(session) = self.snapshot_session(session_id) {
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
            projection
                .notifications
                .retain(|notification| notification.session_id == *session_id);
        } else {
            projection.timeline.clear();
            projection.canonical_turns.clear();
            projection.notifications.clear();
        }
        BootstrapDto::from_state_with_session_projection(self, projection)
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
        if let Some(workspace_id) = requested_workspace_id {
            if self
                .workspace_root_path(&Some(workspace_id.clone()))
                .is_some()
            {
                return Some(workspace_id);
            }
            return self.workspace_id_for_root_path(requested_workspace_path);
        }
        self.workspace_id_for_root_path(requested_workspace_path)
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
        let mission_aggregate_exports = self.collect_mission_aggregate_exports();
        runtime_read_model_dto(
            self.event_bus.runtime_read_model_input(),
            &self.session_store.execution_sidecar_exports(),
            &self.workspace_registry.recovery_sidecar_exports(),
            self.audit_usage_ledger_dto(),
            self.task_store(),
            &mission_aggregate_exports,
        )
    }

    /// 跨 workspace 枚举所有可恢复 mission,组装派生属性导出。
    ///
    /// 反孤儿:`MissionAggregate::lifecycle_phase()` / `metrics()` 在 Phase A
    /// 落地后必须有真实消费方,本函数是 read-model 路径上的入口。
    ///
    /// 单点失败容错:任一 mission resume 失败(charter-draft、checkpoint 缺失等)
    /// `warn-and-skip` 而非整体 503;debug 级日志避免污染 ops 视图。
    pub(crate) fn collect_mission_aggregate_exports(&self) -> Vec<MissionAggregateExport> {
        let Some(magi_home) = self
            .runtime_persistence
            .as_ref()
            .and_then(|p| p.state_root().map(|r| r.to_path_buf()))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for workspace in self.workspace_registry.workspaces() {
            let workspace_root = WorkspaceRootPath::from(workspace.root_path.as_str());
            let mids = match enumerate_resumable_missions(&workspace_root, &magi_home) {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!(
                        workspace = %workspace.root_path.as_str(),
                        error = %err,
                        "enumerate_resumable_missions 失败,跳过此 workspace"
                    );
                    continue;
                }
            };
            for mid in mids {
                let aggregate = match resume_mission(&mid, &workspace_root, &magi_home) {
                    Ok(a) => a,
                    Err(err) => {
                        tracing::debug!(
                            mission_id = %mid.as_str(),
                            error = %err,
                            "resume_mission 跳过(charter-draft 等预期错误)"
                        );
                        continue;
                    }
                };
                let lifecycle_phase = match aggregate.lifecycle_phase() {
                    Ok(p) => p,
                    Err(err) => {
                        tracing::debug!(
                            mission_id = %mid.as_str(),
                            error = %err,
                            "lifecycle_phase 计算失败,跳过"
                        );
                        continue;
                    }
                };
                let metrics = aggregate.metrics().ok().flatten();
                out.push(MissionAggregateExport {
                    mission_id: mid.as_str().to_string(),
                    lifecycle_phase,
                    metrics,
                });
            }
        }
        out
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
        let tool_catalog = self.settings_tool_catalog_json(tool_context);
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workerConfigs"),
            "orchestratorConfig": object_section(&snapshot, "orchestratorConfig"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliaryConfig"),
            "userRulesConfig": object_section(&snapshot, "userRulesConfig"),
            "skillsConfig": object_section(&snapshot, "skillsConfig"),
            "safeguardConfig": object_section(&snapshot, "safeguardConfig"),
            "repositories": array_section(&snapshot, "repositories"),
            "mcpServers": array_section(&snapshot, "mcpServers"),
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

    fn settings_tool_catalog_json(&self, tool_context: &ToolExecutionContext) -> serde_json::Value {
        self.tool_catalog_json(
            r#"{"includeExternal":false,"includeMcpServers":false,"includeAgentRoles":false}"#,
            tool_context,
        )
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
            .into_iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.get("name").cloned().unwrap_or(serde_json::Value::Null),
                    "riskLevel": tool.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
                    "approvalRequirement": tool.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "accessMode": tool.get("access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
                    "runtimeStatus": tool.get("runtime_status").cloned().unwrap_or(serde_json::Value::String("ready".to_string())),
                    "runtimeWarnings": tool.get("runtime_warnings").cloned().unwrap_or_else(|| serde_json::json!([])),
                    "schemaStatus": tool.get("schema_status").cloned().unwrap_or(serde_json::Value::String("ok".to_string())),
                    "schemaWarnings": tool.get("schema_warnings").cloned().unwrap_or_else(|| serde_json::json!([])),
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
            let enabled = entry
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let already_connected = {
                let pool = self
                    .mcp_connections
                    .read()
                    .expect("mcp connections read lock poisoned");
                pool.contains_key(&server_id)
            };

            if already_connected {
                let pool = self
                    .mcp_connections
                    .read()
                    .expect("mcp connections read lock poisoned");
                if let Some(client) = pool.get(&server_id) {
                    entry["connected"] = serde_json::json!(true);
                    entry["health"] = serde_json::json!("connected");
                    entry.as_object_mut().map(|m| m.remove("error"));
                    if let Ok(tools) = client.list_tools() {
                        entry["toolCount"] = serde_json::json!(tools.len());
                    }
                }
            } else if enabled {
                if let Some(config) = build_mcp_config_from_entry(entry) {
                    let client = StdioMcpBridgeClient::new(config);
                    match client.list_tools() {
                        Ok(tools) => {
                            entry["connected"] = serde_json::json!(true);
                            entry["health"] = serde_json::json!("connected");
                            entry["toolCount"] = serde_json::json!(tools.len());
                            entry.as_object_mut().map(|m| m.remove("error"));
                            let mut pool = self
                                .mcp_connections
                                .write()
                                .expect("mcp connections write lock poisoned");
                            pool.insert(server_id, Arc::new(client));
                        }
                        Err(err) => {
                            tracing::warn!(
                                server_id = %server_id,
                                error = ?err,
                                "MCP server health check failed"
                            );
                            entry["connected"] = serde_json::json!(false);
                            entry["health"] = serde_json::json!("disconnected");
                            entry["error"] = serde_json::json!("mcp_connection_failed");
                        }
                    }
                } else {
                    entry["connected"] = serde_json::json!(false);
                    entry["health"] = serde_json::json!("disconnected");
                    entry["error"] = serde_json::json!("mcp_invalid_config");
                }
            } else {
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disconnected");
                entry.as_object_mut().map(|m| m.remove("error"));
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
        let (mut global_state, mut workspace_states) = durable.partition_by_workspace();
        let workspaces = self.workspace_registry.workspaces();
        for workspace in &workspaces {
            let ws_id = workspace.workspace_id.to_string();
            let ws_state = workspace_states.remove(&ws_id).unwrap_or_default();
            let magi_dir = std::path::Path::new(workspace.root_path.as_str()).join(".magi");
            let session_path = magi_dir.join("sessions.json");
            persistence.save_json(&session_path, &ws_state)?;
        }

        for (_, orphan_state) in workspace_states {
            global_state.append_state(orphan_state);
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

    pub fn persist_workspace_durable_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_workspace_store(&self.workspace_registry)
    }

    pub fn persist_knowledge_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_knowledge_store(&self.knowledge_store)
    }

    pub fn persist_runtime_durable_state(&self) -> Result<(), ApiError> {
        self.persist_session_durable_state()?;
        self.persist_workspace_durable_state()?;
        self.persist_knowledge_state()?;
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
    for key in [
        "orchestrator",
        "orchestratorConfig",
        "auxiliary",
        "auxiliaryConfig",
        "userRulesConfig",
        "safeguard",
        "safeguardConfig",
    ] {
        if let Some(value) = snapshot.get_mut(key) {
            normalize_wrapped_section_value(value);
        }
    }
    skill_loader::normalize_skills_config_sections(snapshot);
    seed_user_rules_config(snapshot);
    normalize_workers_section(snapshot);
    normalize_mcp_servers_section(snapshot);
    seed_default_safeguard_rules(snapshot);
    normalize_safeguard_config_section(snapshot);
    alias_snapshot_keys(snapshot);
}

fn alias_snapshot_keys(snapshot: &mut HashMap<String, serde_json::Value>) {
    let aliases: &[(&str, &str)] = &[
        ("workers", "workerConfigs"),
        ("orchestrator", "orchestratorConfig"),
        ("auxiliary", "auxiliaryConfig"),
    ];
    for (from, to) in aliases {
        if !snapshot.contains_key(*to) {
            if let Some(value) = snapshot.get(*from).cloned() {
                snapshot.insert(to.to_string(), value);
            }
        }
    }
}

fn normalize_capability_dependency_json(raw: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "status": raw.get("status").cloned().unwrap_or(serde_json::Value::String("unknown".to_string())),
        "requiredBy": capability_dependency_field(raw, "requiredBy", "required_by")
            .unwrap_or_else(|| serde_json::json!([])),
        "workspaceId": capability_dependency_field(raw, "workspaceId", "workspace_id"),
        "sessionId": capability_dependency_field(raw, "sessionId", "session_id"),
        "fileCount": capability_dependency_field(raw, "fileCount", "file_count"),
        "lastIndexed": capability_dependency_field(raw, "lastIndexed", "last_indexed"),
        "roleCount": capability_dependency_field(raw, "roleCount", "role_count"),
        "spawnableRoleCount": capability_dependency_field(raw, "spawnableRoleCount", "spawnable_role_count"),
        "snapshotActive": capability_dependency_field(raw, "snapshotActive", "snapshot_active"),
        "configuredCount": capability_dependency_field(raw, "configuredCount", "configured_count"),
        "enabledCount": capability_dependency_field(raw, "enabledCount", "enabled_count"),
        "readyCount": capability_dependency_field(raw, "readyCount", "ready_count"),
        "toolCount": capability_dependency_field(raw, "toolCount", "tool_count"),
    })
}

fn capability_dependency_field(
    raw: &serde_json::Value,
    camel_key: &str,
    snake_key: &str,
) -> Option<serde_json::Value> {
    raw.get(camel_key)
        .or_else(|| raw.get(snake_key))
        .cloned()
        .filter(|value| !value.is_null())
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

fn normalize_wrapped_section_value(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object() {
        let nested = object.get("config").or_else(|| object.get("data")).cloned();
        if let Some(nested) = nested {
            *value = nested;
        }
    }
    strip_scope_binding_fields(value);
}

fn strip_scope_binding_fields(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object_mut() {
        for key in [
            "workspaceId",
            "workspace_id",
            "workspacePath",
            "workspace_path",
            "sessionId",
            "session_id",
        ] {
            object.remove(key);
        }
    }
}

fn seed_user_rules_config(snapshot: &mut HashMap<String, serde_json::Value>) {
    let raw = snapshot
        .remove("userRulesConfig")
        .or_else(|| snapshot.remove("userRules"))
        .unwrap_or_else(|| serde_json::json!({}));
    snapshot.insert(
        "userRulesConfig".to_string(),
        normalize_user_rules_config_value(raw),
    );
}

fn normalize_user_rules_config_value(mut value: serde_json::Value) -> serde_json::Value {
    normalize_wrapped_section_value(&mut value);
    match value {
        serde_json::Value::String(user_rules) => serde_json::json!({ "userRules": user_rules }),
        serde_json::Value::Object(_) => value,
        _ => serde_json::json!({}),
    }
}

pub(crate) fn normalize_safeguard_config_value(mut value: serde_json::Value) -> serde_json::Value {
    normalize_wrapped_section_value(&mut value);
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
        let legacy = snapshot
            .remove("safeguard")
            .unwrap_or(serde_json::json!({}));
        snapshot.insert("safeguardConfig".to_string(), legacy);
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

fn normalize_workers_section(snapshot: &mut HashMap<String, serde_json::Value>) {
    let Some(workers) = snapshot.get_mut("workers") else {
        return;
    };
    let Some(object) = workers.as_object() else {
        return;
    };
    let worker_id = object
        .get("worker")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let worker_config = object.get("config").cloned();
    if let (Some(worker_id), Some(worker_config)) = (worker_id, worker_config) {
        *workers = serde_json::json!({ worker_id: worker_config });
    }
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
    use magi_core::{AbsolutePath, MissionId, Task, TaskKind};
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
            Some(workspace_id)
        );
    }
}
