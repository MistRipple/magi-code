use crate::dto::{
    AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeProvider, BridgeCutoverSmokeSnapshotDto,
    BridgeCutoverSmokeSnapshotProvider, BridgePreflightProvider, BridgePreflightSnapshotDto,
    BridgePreflightSnapshotProvider, BridgeProbeSnapshotProvider, BridgeServicesSnapshotDto,
    BridgeSnapshotProvider, DirectHttpModelProbeConfig, HealthDto, RuntimeReadModelDto,
    ServiceInfo, VersionHandshakeDto, runtime_read_model_dto,
};
use crate::errors::ApiError;
use crate::routes::settings::{
    builtin_role_templates, enabled_registry_agent_roles, load_registry_engines,
    resolve_registry_agents,
};
use crate::settings_store::SettingsStore;
use crate::skill_loader;
use crate::task_execution::{ShadowTaskDispatcher, ShadowTaskExecutionRegistry};
use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient, McpServerConfig,
    ModelBridgeClient, StdioMcpBridgeClient,
};
use magi_core::{SessionId, TaskId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_memory_store::MemoryStore;
use magi_orchestrator::{
    OrchestratedExecutionRuntime, OrchestratorService,
    task_runner::{
        EventBasedResultReceiver, EventBasedTaskDispatcher, RunCycleOutcome, TaskDispatcher,
        TaskResultReceiver, TaskRunner, WorkerExecutionDispatcher, WorkerInfo,
    },
    task_store::TaskStore,
    task_worker_catalog::build_worker_catalog_for_roles,
};
use magi_session_store::{SessionRecord, SessionStore};
use magi_tool_runtime::ToolRegistry;
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
    /// 后台 runner 循环是否仍未退出，用于避免暂停/停止中的任务链被重复启动。
    pub active: Arc<AtomicBool>,
    /// Number of cycles executed so far.
    pub cycle_count: Arc<AtomicU64>,
    /// 当前 runner 展示状态："running"、"blocked"、"stopped"、"completed"、"error"。
    pub status: Arc<Mutex<String>>,
    /// Last error message, if any.
    pub last_error: Arc<Mutex<Option<String>>>,
}

/// Manages active Runner instances keyed by root_task_id.
#[derive(Clone)]
pub struct RunnerManager {
    runners: Arc<Mutex<HashMap<String, Arc<RunnerHandle>>>>,
    task_store: Arc<TaskStore>,
    worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
    dispatcher: Option<Arc<dyn TaskDispatcher>>,
    event_bus: Option<Arc<InMemoryEventBus>>,
    /// Shared result receiver that collects task completion/failure results
    /// pushed from the TaskStore's status-change callback.
    result_receiver: Arc<EventBasedResultReceiver>,
    /// Optional path for periodic task-store checkpoints.
    checkpoint_path: Option<PathBuf>,
    /// Maps a session to the root task IDs whose runners should be cancelled
    /// when the session is closed (design 1.5: Session-Runner linkage).
    session_runner_index: Arc<Mutex<HashMap<SessionId, Vec<String>>>>,
}

/// Number of runner cycles between periodic checkpoints.
const CHECKPOINT_INTERVAL_CYCLES: u64 = 5;

impl RunnerManager {
    pub fn new(task_store: Arc<TaskStore>, workers: Vec<WorkerInfo>) -> Self {
        let worker_catalog = Arc::new(move || workers.clone());
        Self::with_worker_catalog(task_store, worker_catalog)
    }

    pub fn with_worker_catalog(
        task_store: Arc<TaskStore>,
        worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
    ) -> Self {
        Self {
            runners: Arc::new(Mutex::new(HashMap::new())),
            task_store,
            worker_catalog,
            dispatcher: None,
            event_bus: None,
            result_receiver: Arc::new(EventBasedResultReceiver::new()),
            checkpoint_path: None,
            session_runner_index: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a RunnerManager wired to a real event bus so that dispatched
    /// tasks publish `task.dispatched` domain events and the TaskStore
    /// publishes `task.status.changed` events.
    pub fn with_event_bus(
        task_store: Arc<TaskStore>,
        workers: Vec<WorkerInfo>,
        event_bus: Arc<InMemoryEventBus>,
        result_receiver: Arc<EventBasedResultReceiver>,
    ) -> Self {
        let worker_catalog = Arc::new(move || workers.clone());
        Self::with_event_bus_and_worker_catalog(
            task_store,
            worker_catalog,
            event_bus,
            result_receiver,
        )
    }

    pub fn with_event_bus_and_worker_catalog(
        task_store: Arc<TaskStore>,
        worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
        event_bus: Arc<InMemoryEventBus>,
        result_receiver: Arc<EventBasedResultReceiver>,
    ) -> Self {
        Self {
            runners: Arc::new(Mutex::new(HashMap::new())),
            task_store,
            worker_catalog,
            dispatcher: None,
            event_bus: Some(event_bus),
            result_receiver,
            checkpoint_path: None,
            session_runner_index: Arc::new(Mutex::new(HashMap::new())),
        }
    }

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
            event_bus: None,
            result_receiver,
            checkpoint_path: None,
            session_runner_index: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_worker_execution(
        task_store: Arc<TaskStore>,
        worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
        worker_runtime: magi_worker_runtime::WorkerRuntime,
        event_bus: Option<Arc<InMemoryEventBus>>,
    ) -> Self {
        let result_receiver = Arc::new(EventBasedResultReceiver::new());
        let mut dispatcher =
            WorkerExecutionDispatcher::new(worker_runtime, Arc::clone(&result_receiver));
        if let Some(ref bus) = event_bus {
            dispatcher = dispatcher.with_event_bus(Arc::clone(bus));
        }
        Self {
            runners: Arc::new(Mutex::new(HashMap::new())),
            task_store,
            worker_catalog,
            dispatcher: Some(Arc::new(dispatcher)),
            event_bus,
            result_receiver,
            checkpoint_path: None,
            session_runner_index: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn resolved_workers(&self) -> Vec<WorkerInfo> {
        (self.worker_catalog)()
    }

    fn build_task_runner(&self) -> TaskRunner {
        let workers = self.resolved_workers();
        let runner = if let Some(ref dispatcher) = self.dispatcher {
            TaskRunner::with_dispatcher(
                Arc::clone(&self.task_store),
                workers,
                Arc::clone(dispatcher),
                Arc::clone(&self.result_receiver) as Arc<dyn TaskResultReceiver>,
            )
        } else if let Some(ref event_bus) = self.event_bus {
            TaskRunner::with_dispatcher(
                Arc::clone(&self.task_store),
                workers,
                Arc::new(EventBasedTaskDispatcher::new(Arc::clone(event_bus))),
                Arc::clone(&self.result_receiver) as Arc<dyn TaskResultReceiver>,
            )
        } else {
            TaskRunner::new(Arc::clone(&self.task_store), workers)
        };
        if let Some(ref event_bus) = self.event_bus {
            runner.with_event_bus(Arc::clone(event_bus))
        } else {
            runner
        }
    }

    /// Set the file path used for periodic task-store checkpoints.
    pub fn with_checkpoint_path(mut self, path: PathBuf) -> Self {
        self.checkpoint_path = Some(path);
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
        tokio::spawn(async move {
            let mut blocked_streak = 0u32;
            let max_blocked_streak = 20u32;
            loop {
                if bg_handle.cancel.load(Ordering::Relaxed) {
                    let mut status = bg_handle.status.lock().expect("status lock should hold");
                    *status = "stopped".to_string();
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
                        blocked_streak = 0;
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    RunCycleOutcome::AllComplete => {
                        if let Some(ref path) = bg_checkpoint_path {
                            let _ = bg_task_store.checkpoint_to_file(path);
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = "completed".to_string();
                        bg_active.store(false, Ordering::Relaxed);
                        break;
                    }
                    RunCycleOutcome::Blocked(_) => {
                        blocked_streak += 1;
                        if blocked_streak >= max_blocked_streak {
                            if let Some(ref path) = bg_checkpoint_path {
                                let _ = bg_task_store.checkpoint_to_file(path);
                            }
                            let mut status =
                                bg_handle.status.lock().expect("status lock should hold");
                            *status = "blocked".to_string();
                            bg_active.store(false, Ordering::Relaxed);
                            break;
                        }
                        let backoff_ms = 200u64.saturating_mul(blocked_streak as u64).min(2_000);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
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
        *status = "stopped".to_string();
        Ok(())
    }

    /// Bind a session to a root task so that when the session closes the
    /// runner is automatically cancelled (design 1.5).
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

    #[cfg(test)]
    pub(crate) fn set_status_for_test(&self, root_task_id: &str, status: &str) {
        let handle = Arc::new(RunnerHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicBool::new(status == "running")),
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status.to_string())),
            last_error: Arc::new(Mutex::new(None)),
        });
        self.runners
            .lock()
            .expect("runners lock should hold")
            .insert(root_task_id.to_string(), handle);
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

    pub fn pause_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.build_task_runner().pause_task(&tid)?;
        self.set_runner_status_if_present(root_task_id, "blocked");
        Ok(())
    }

    pub fn pause_task(&self, task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(task_id);
        let task = self
            .task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", task_id))?;
        self.build_task_runner().pause_task(&tid)?;
        self.set_runner_status_if_present(task.root_task_id.as_str(), "blocked");
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
pub struct ShadowExecutionPipeline {
    pub orchestrator: OrchestratorService,
    pub execution_runtime: OrchestratedExecutionRuntime,
    pub memory_store: MemoryStore,
}

impl ShadowExecutionPipeline {}

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
    bridge_probe_snapshot_provider: BridgeProbeSnapshotProvider,
    bridge_preflight_snapshot_provider: BridgePreflightSnapshotProvider,
    bridge_cutover_smoke_provider: BridgeCutoverSmokeSnapshotProvider,
    bridge_snapshot_provider: Option<Arc<dyn BridgeSnapshotProvider>>,
    shadow_execution_pipeline: Option<ShadowExecutionPipeline>,
    shadow_task_execution_registry: ShadowTaskExecutionRegistry,
    task_store: Option<Arc<TaskStore>>,
    runner_manager: Option<RunnerManager>,
    session_turn_dispatcher: Option<Arc<ShadowTaskDispatcher>>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    task_planning_model_client: Option<Arc<dyn ModelBridgeClient>>,
    model_bridge_client_is_real: bool,
    tool_registry: Option<ToolRegistry>,
    pub skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    pub tunnel_manager: crate::tunnel::TunnelManager,
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
                api_version: "v0-shadow".to_string(),
            },
            runtime_epoch: format!("runtime-{}", UtcMillis::now().0),
            event_bus,
            session_store,
            workspace_registry,
            governance,
            knowledge_store: Arc::new(KnowledgeStore::new()),
            settings_store: Arc::new(SettingsStore::new()),
            runtime_persistence: None,
            bridge_probe_snapshot_provider: BridgeProbeSnapshotProvider::default(),
            bridge_preflight_snapshot_provider: BridgePreflightSnapshotProvider::default(),
            bridge_cutover_smoke_provider: BridgeCutoverSmokeSnapshotProvider::default(),
            bridge_snapshot_provider: None,
            shadow_execution_pipeline: None,
            shadow_task_execution_registry: ShadowTaskExecutionRegistry::default(),
            task_store: None,
            runner_manager: None,
            session_turn_dispatcher: None,
            mcp_connections: Arc::new(RwLock::new(HashMap::new())),
            model_bridge_client: None,
            task_planning_model_client: None,
            model_bridge_client_is_real: false,
            tool_registry: None,
            skill_runtime: None,
            tunnel_manager: crate::tunnel::TunnelManager::new(38123),
        }
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
        build_worker_catalog_for_roles(enabled_registry_agent_roles(self))
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

    pub fn with_shadow_execution_pipeline(
        mut self,
        orchestrator: OrchestratorService,
        execution_runtime: OrchestratedExecutionRuntime,
        memory_store: MemoryStore,
    ) -> Self {
        self.shadow_execution_pipeline = Some(ShadowExecutionPipeline {
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

    pub fn runtime_persistence(&self) -> Option<&RuntimeStatePersistence> {
        self.runtime_persistence.as_deref()
    }

    pub fn health_dto(&self) -> HealthDto {
        HealthDto::from_service_info(&self.service_info)
    }

    pub fn runtime_epoch(&self) -> &str {
        &self.runtime_epoch
    }

    pub fn bootstrap_dto(&self) -> BootstrapDto {
        BootstrapDto::from_state(self)
    }

    pub fn bootstrap_dto_for_session(
        &self,
        requested_session_id: Option<&SessionId>,
    ) -> BootstrapDto {
        BootstrapDto::from_state_with_selected_session(self, requested_session_id)
    }

    pub fn bootstrap_dto_for_workspace_session(
        &self,
        workspace_id: Option<&str>,
        requested_session_id: Option<&SessionId>,
    ) -> BootstrapDto {
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
            .cloned();
        projection.current_session_id = selected_session_id.clone();
        if let Some(session_id) = selected_session_id.as_ref() {
            projection
                .timeline
                .retain(|entry| entry.session_id == *session_id);
            projection
                .notifications
                .retain(|notification| notification.session_id == *session_id);
        } else {
            projection.timeline.clear();
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
            return self.session_store.sessions();
        };
        self.session_store
            .sessions()
            .into_iter()
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

    pub fn runtime_read_model_dto(&self) -> RuntimeReadModelDto {
        runtime_read_model_dto(
            self.event_bus.runtime_read_model_input(),
            &self.session_store.execution_sidecar_exports(),
            &self.workspace_registry.recovery_sidecar_exports(),
            self.audit_usage_ledger_dto(),
            self.task_store(),
        )
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

    pub fn shadow_execution_pipeline(&self) -> Option<&ShadowExecutionPipeline> {
        self.shadow_execution_pipeline.as_ref()
    }

    pub fn shadow_task_execution_registry(&self) -> &ShadowTaskExecutionRegistry {
        &self.shadow_task_execution_registry
    }

    pub fn settings_snapshot_json(&self) -> serde_json::Value {
        self.settings_snapshot_json_with_mcp_hydration(true)
    }

    pub fn settings_snapshot_json_with_mcp_hydration(
        &self,
        hydrate_mcp_servers: bool,
    ) -> serde_json::Value {
        let mut snapshot = self.settings_store.public_snapshot();
        normalize_settings_snapshot_sections(&mut snapshot);
        if hydrate_mcp_servers {
            self.enrich_mcp_servers_with_connection_status(&mut snapshot);
        }
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workerConfigs"),
            "orchestratorConfig": object_section(&snapshot, "orchestratorConfig"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliaryConfig"),
            "userRulesConfig": object_section(&snapshot, "userRulesConfig"),
            "skillsConfig": object_section(&snapshot, "skillsConfig"),
            "safeguardConfig": object_section(&snapshot, "safeguardConfig"),
            "repositories": array_section(&snapshot, "repositories"),
            "mcpServers": array_section(&snapshot, "mcpServers"),
            "builtinTools": self.builtin_tools_json(),
            "workerStatuses": object_section(&snapshot, "workerStatuses"),
            "runtimeSettings": runtime_settings_from_snapshot(&snapshot),
            "roleTemplates": builtin_role_templates(),
            "registryEngines": load_registry_engines(self),
            "registryAgents": resolve_registry_agents(self),
            "bootstrapScope": if hydrate_mcp_servers { "full" } else { "core" },
            "mcpServersHydrated": hydrate_mcp_servers,
        })
    }

    fn builtin_tools_json(&self) -> serde_json::Value {
        let Some(registry) = &self.tool_registry else {
            return serde_json::Value::Array(Vec::new());
        };
        let mut tools = registry
            .builtin_specs()
            .into_iter()
            .map(|spec| {
                let access_mode = registry
                    .builtin_access_mode(&spec.name)
                    .map(|mode| mode.as_str())
                    .unwrap_or("read_only");
                serde_json::json!({
                    "name": spec.name,
                    "riskLevel": spec.risk_level,
                    "approvalRequirement": spec.approval_requirement,
                    "accessMode": access_mode,
                    "enabled": true,
                })
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| {
            left.get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )
        });
        serde_json::Value::Array(tools)
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
                            let mut pool = self
                                .mcp_connections
                                .write()
                                .expect("mcp connections write lock poisoned");
                            pool.insert(server_id, Arc::new(client));
                        }
                        Err(_) => {
                            entry["connected"] = serde_json::json!(false);
                            entry["health"] = serde_json::json!("disconnected");
                        }
                    }
                } else {
                    entry["connected"] = serde_json::json!(false);
                    entry["health"] = serde_json::json!("disconnected");
                }
            } else {
                entry["connected"] = serde_json::json!(false);
                entry["health"] = serde_json::json!("disconnected");
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

    pub fn task_store(&self) -> Option<&TaskStore> {
        self.task_store.as_deref()
    }

    pub fn with_runner_manager(mut self, manager: RunnerManager) -> Self {
        self.runner_manager = Some(manager);
        self
    }

    pub fn with_session_turn_dispatcher(mut self, dispatcher: Arc<ShadowTaskDispatcher>) -> Self {
        self.session_turn_dispatcher = Some(dispatcher);
        self
    }

    pub fn session_turn_dispatcher(&self) -> Option<&Arc<ShadowTaskDispatcher>> {
        self.session_turn_dispatcher.as_ref()
    }

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_task_planning_model_bridge_client(
        mut self,
        client: Arc<dyn ModelBridgeClient>,
    ) -> Self {
        self.task_planning_model_client = Some(client);
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

    pub fn runner_manager(&self) -> Option<&RunnerManager> {
        self.runner_manager.as_ref()
    }

    pub fn mcp_connections(&self) -> &Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>> {
        &self.mcp_connections
    }

    pub fn model_bridge_client(&self) -> Option<&Arc<dyn ModelBridgeClient>> {
        self.model_bridge_client.as_ref()
    }

    pub fn task_planning_model_client(&self) -> Option<&Arc<dyn ModelBridgeClient>> {
        self.task_planning_model_client.as_ref()
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
    let deep_task = runtime
        .and_then(|value| value.get("deepTask"))
        .and_then(|value| value.as_bool())
        .or_else(|| snapshot.get("deepTask").and_then(|value| value.as_bool()))
        .unwrap_or(false);
    serde_json::json!({
        "locale": locale,
        "deepTask": deep_task,
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

fn builtin_safeguard_rules() -> Vec<serde_json::Value> {
    let rules: &[(&str, &str)] = &[
        ("git push --force", "git_history"),
        ("git push -f", "git_history"),
        ("git rebase", "git_history"),
        ("git reset --hard", "git_history"),
        ("git commit --amend", "git_history"),
        ("git checkout --", "git_discard"),
        ("git restore", "git_discard"),
        ("git clean", "git_discard"),
        ("git stash drop", "git_discard"),
        ("npm publish", "package_publish"),
        ("cargo publish", "package_publish"),
        ("yarn publish", "package_publish"),
        ("pip upload", "package_publish"),
        ("rm -rf", "bulk_delete"),
        ("rimraf", "bulk_delete"),
    ];
    rules
        .iter()
        .map(|(pattern, category)| {
            serde_json::json!({
                "pattern": pattern,
                "enabled": true,
                "category": category,
            })
        })
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

fn normalize_mcp_server_snapshot_entry(entry: &serde_json::Value) -> Option<serde_json::Value> {
    let raw = entry
        .get("server")
        .or_else(|| entry.get("updates"))
        .cloned()
        .unwrap_or_else(|| entry.clone());
    let mut object = raw.as_object().cloned()?;
    let server_id = object
        .get("id")
        .and_then(|value| value.as_str())
        .or_else(|| object.get("serverId").and_then(|value| value.as_str()))
        .or_else(|| entry.get("serverId").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    object.insert("id".to_string(), serde_json::json!(server_id));
    object.insert("serverId".to_string(), serde_json::json!(server_id));
    if object
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        object.insert("name".to_string(), serde_json::json!(server_id));
    }
    Some(serde_json::Value::Object(object))
}

pub(crate) fn build_mcp_config_from_entry(entry: &serde_json::Value) -> Option<McpServerConfig> {
    let command = entry.get("command")?.as_str()?.to_string();
    if command.is_empty() {
        return None;
    }
    let args: Vec<String> = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let working_directory = entry
        .get("workingDirectory")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    let env: std::collections::BTreeMap<String, String> = entry
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(McpServerConfig {
        command,
        args,
        working_directory,
        env,
    })
}
