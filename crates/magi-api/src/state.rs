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
use crate::task_execution::{ShadowTaskDispatcher, ShadowTaskExecutionRegistry};
use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient, McpServerConfig,
    ModelBridgeClient, StdioMcpBridgeClient,
};
use magi_core::{SessionId, TaskId, UtcMillis};
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
use magi_session_store::SessionStore;
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
    /// Number of cycles executed so far.
    pub cycle_count: Arc<AtomicU64>,
    /// Current status: "running", "stopped", "completed", "error".
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
        }
    }

    fn resolved_workers(&self) -> Vec<WorkerInfo> {
        (self.worker_catalog)()
    }

    fn build_task_runner(&self) -> TaskRunner {
        let workers = self.resolved_workers();
        if let Some(ref dispatcher) = self.dispatcher {
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
    pub fn start(&self, root_task_id: &str) -> Result<Arc<RunnerHandle>, RunnerStartError> {
        let tid = TaskId::new(root_task_id);
        // Verify the root task exists.
        self.task_store
            .get_task(&tid)
            .ok_or(RunnerStartError::NotFound)?;

        let mut runners = self.runners.lock().expect("runners lock should hold");
        if let Some(existing) = runners.get(root_task_id) {
            let status = existing.status.lock().expect("status lock should hold");
            if *status == "running" {
                return Err(RunnerStartError::AlreadyRunning);
            }
        }

        let handle = Arc::new(RunnerHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            cycle_count: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new("running".to_string())),
            last_error: Arc::new(Mutex::new(None)),
        });

        runners.insert(root_task_id.to_string(), Arc::clone(&handle));

        // Spawn the background loop.
        let task_runner = self.build_task_runner();
        let root_id = tid;
        let bg_handle = Arc::clone(&handle);
        let bg_task_store = Arc::clone(&self.task_store);
        let bg_checkpoint_path = self.checkpoint_path.clone();
        tokio::spawn(async move {
            loop {
                if bg_handle.cancel.load(Ordering::Relaxed) {
                    let mut status = bg_handle.status.lock().expect("status lock should hold");
                    *status = "stopped".to_string();
                    break;
                }

                let outcome = task_runner.run_cycle(&root_id);
                let cycle = bg_handle.cycle_count.fetch_add(1, Ordering::Relaxed) + 1;

                // Periodic checkpoint every N cycles.
                if let Some(ref path) = bg_checkpoint_path {
                    let should_checkpoint = cycle % CHECKPOINT_INTERVAL_CYCLES == 0;
                    if should_checkpoint {
                        let _ = bg_task_store.checkpoint_to_file(path);
                    }
                }

                match outcome {
                    RunCycleOutcome::Continue => {
                        // Brief yield before next cycle.
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    RunCycleOutcome::AllComplete => {
                        // Checkpoint on terminal state.
                        if let Some(ref path) = bg_checkpoint_path {
                            let _ = bg_task_store.checkpoint_to_file(path);
                        }
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
                        *status = "completed".to_string();
                        break;
                    }
                    RunCycleOutcome::Blocked(_) => {
                        // Wait longer before retrying when blocked.
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    RunCycleOutcome::Error(err) => {
                        // Checkpoint on error so we don't lose state.
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
        let status = handle.status.lock().expect("status lock should hold");
        if *status != "running" {
            return Err(RunnerStopError::NotRunning);
        }
        handle.cancel.store(true, Ordering::Relaxed);
        Ok(())
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

    pub fn pause_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.build_task_runner().pause_task(&tid)
    }

    pub fn resume_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.build_task_runner().resume_task(&tid)
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
    model_bridge_client_is_real: bool,
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
            model_bridge_client_is_real: false,
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
        let effective_session_id = if requested_session_id.is_some() || workspace_id.is_none() {
            requested_session_id.cloned()
        } else {
            let projection = self.session_store.projection_input();
            projection
                .current_session_id
                .as_ref()
                .filter(|session_id| {
                    projection.sessions.iter().any(|session| {
                        &session.session_id == *session_id
                            && session.workspace_id.as_deref() == workspace_id
                    })
                })
                .cloned()
                .or_else(|| {
                    projection
                        .sessions
                        .iter()
                        .find(|session| session.workspace_id.as_deref() == workspace_id)
                        .map(|session| session.session_id.clone())
                })
        };
        let mut dto =
            BootstrapDto::from_state_with_selected_session(self, effective_session_id.as_ref());
        // 按 workspace_id 过滤会话列表
        if let Some(ws_id) = workspace_id {
            dto.sessions = dto
                .sessions
                .into_iter()
                .filter(|s| s.workspace_id.as_deref() == Some(ws_id))
                .collect();
        }
        dto
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
        self.settings_snapshot_json_for_session(None)
    }

    pub fn settings_snapshot_json_for_session(
        &self,
        session_id: Option<&SessionId>,
    ) -> serde_json::Value {
        let mut snapshot = self.settings_store.public_snapshot();
        snapshot.remove("userRules");
        snapshot.remove("safeguard");
        snapshot.remove("safeguardConfig");
        if let Some(session_id) = session_id {
            snapshot.insert(
                "userRulesConfig".to_string(),
                settings_section_or_empty(
                    self.settings_store
                        .get_session_section(session_id, "userRules"),
                ),
            );
            snapshot.insert(
                "safeguardConfig".to_string(),
                settings_section_or_empty(
                    self.settings_store
                        .get_session_section(session_id, "safeguardConfig"),
                ),
            );
        } else {
            snapshot.insert("userRulesConfig".to_string(), serde_json::json!({}));
            snapshot.insert("safeguardConfig".to_string(), serde_json::json!({}));
        }
        normalize_settings_snapshot_sections(&mut snapshot);
        self.enrich_mcp_servers_with_connection_status(&mut snapshot);
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workerConfigs"),
            "orchestratorConfig": object_section(&snapshot, "orchestratorConfig"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliaryConfig"),
            "userRulesConfig": object_section(&snapshot, "userRulesConfig"),
            "skillsConfig": object_section(&snapshot, "skillsConfig"),
            "safeguardConfig": object_section(&snapshot, "safeguardConfig"),
            "repositories": array_section(&snapshot, "repositories"),
            "mcpServers": array_section(&snapshot, "mcpServers"),
            "workerStatuses": object_section(&snapshot, "workerStatuses"),
            "runtimeSettings": runtime_settings_from_snapshot(&snapshot),
            "roleTemplates": builtin_role_templates(),
            "registryEngines": load_registry_engines(self),
            "registryAgents": resolve_registry_agents(self),
            "bootstrapScope": "full",
            "mcpServersHydrated": true,
        })
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
        let (global_state, mut workspace_states) = durable.partition_by_workspace();
        let workspaces = self.workspace_registry.workspaces();
        for workspace in &workspaces {
            let ws_id = workspace.workspace_id.to_string();
            let ws_state = workspace_states.remove(&ws_id).unwrap_or_default();
            let magi_dir = std::path::Path::new(workspace.root_path.as_str()).join(".magi");
            let session_path = magi_dir.join("sessions.json");
            persistence.save_json(&session_path, &ws_state)?;
        }

        if let Some((workspace_id, _)) = workspace_states.into_iter().next() {
            return Err(ApiError::internal_assembly(
                "持久化会话失败",
                format!("检测到未注册工作区的会话状态: {workspace_id}"),
            ));
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
        "skillsConfig",
    ] {
        if let Some(value) = snapshot.get_mut(key) {
            normalize_wrapped_section_value(value);
        }
    }
    merge_legacy_custom_tools_into_skills_config(snapshot);
    merge_legacy_instruction_skills_into_skills_config(snapshot);
    normalize_workers_section(snapshot);
    normalize_mcp_servers_section(snapshot);
    seed_default_safeguard_rules(snapshot);
    alias_snapshot_keys(snapshot);
}

pub(crate) fn normalize_safeguard_config_value(
    safeguard_config: serde_json::Value,
) -> serde_json::Value {
    let mut snapshot = HashMap::new();
    snapshot.insert("safeguardConfig".to_string(), safeguard_config);
    normalize_settings_snapshot_sections(&mut snapshot);
    snapshot
        .remove("safeguardConfig")
        .unwrap_or_else(|| serde_json::json!({}))
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

fn settings_section_or_empty(value: serde_json::Value) -> serde_json::Value {
    if value.is_null() {
        serde_json::json!({})
    } else {
        value
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
    let Some(object) = value.as_object() else {
        return;
    };
    let nested = object.get("config").or_else(|| object.get("data")).cloned();
    if let Some(nested) = nested {
        *value = nested;
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
    for entry in entries.iter_mut() {
        let raw = entry
            .get("server")
            .or_else(|| entry.get("updates"))
            .cloned()
            .unwrap_or_else(|| entry.clone());
        let Some(mut object) = raw.as_object().cloned() else {
            continue;
        };
        let server_id = object
            .get("id")
            .and_then(|value| value.as_str())
            .or_else(|| object.get("serverId").and_then(|value| value.as_str()))
            .or_else(|| entry.get("serverId").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        if server_id.is_empty() {
            continue;
        }
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
        *entry = serde_json::Value::Object(object);
    }
}

fn merge_legacy_custom_tools_into_skills_config(snapshot: &mut HashMap<String, serde_json::Value>) {
    let legacy_custom_tools = snapshot
        .get("customTools")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if legacy_custom_tools.is_empty() {
        return;
    }
    let skills_config = snapshot
        .entry("skillsConfig".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(config) = skills_config.as_object_mut() else {
        return;
    };
    let custom_tools = config
        .entry("customTools".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(custom_tools_array) = custom_tools.as_array_mut() else {
        return;
    };
    for entry in legacy_custom_tools {
        let Some(mut object) = entry.as_object().cloned() else {
            continue;
        };
        let tool_name = object
            .get("name")
            .and_then(|value| value.as_str())
            .or_else(|| object.get("toolName").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        if tool_name.is_empty() {
            continue;
        }
        object.insert("name".to_string(), serde_json::json!(tool_name));
        object.insert("toolName".to_string(), serde_json::json!(tool_name));
        if let Some(position) = custom_tools_array.iter().position(|item| {
            ["toolName", "name"].iter().any(|field| {
                item.get(*field)
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == tool_name)
            })
        }) {
            custom_tools_array[position] = serde_json::Value::Object(object);
        } else {
            custom_tools_array.push(serde_json::Value::Object(object));
        }
    }
}

fn merge_legacy_instruction_skills_into_skills_config(
    snapshot: &mut HashMap<String, serde_json::Value>,
) {
    let legacy_skills = snapshot
        .get("skills")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if legacy_skills.is_empty() {
        return;
    }
    let skills_config = snapshot
        .entry("skillsConfig".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(config) = skills_config.as_object_mut() else {
        return;
    };
    let instruction_skills = config
        .entry("instructionSkills".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(instruction_skills_array) = instruction_skills.as_array_mut() else {
        return;
    };
    for entry in legacy_skills {
        let Some(mut object) = entry.as_object().cloned() else {
            continue;
        };
        let skill_name = object
            .get("name")
            .and_then(|value| value.as_str())
            .or_else(|| object.get("skillName").and_then(|value| value.as_str()))
            .or_else(|| object.get("skillId").and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        if skill_name.is_empty() {
            continue;
        }
        object.insert("name".to_string(), serde_json::json!(skill_name));
        object.insert("skillName".to_string(), serde_json::json!(skill_name));
        object.insert("skillId".to_string(), serde_json::json!(skill_name));
        if object
            .get("fullName")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            object.insert("fullName".to_string(), serde_json::json!(skill_name));
        }
        if let Some(position) = instruction_skills_array.iter().position(|item| {
            ["skillId", "skillName", "name"].iter().any(|field| {
                item.get(*field)
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value == skill_name)
            })
        }) {
            instruction_skills_array[position] = serde_json::Value::Object(object);
        } else {
            instruction_skills_array.push(serde_json::Value::Object(object));
        }
    }
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
