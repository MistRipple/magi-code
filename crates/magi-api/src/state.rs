use crate::dto::{
    runtime_read_model_dto, AuditUsageLedgerDto, BootstrapDto, BridgeCutoverSmokeProvider,
    BridgeCutoverSmokeSnapshotDto, BridgeCutoverSmokeSnapshotProvider,
    BridgePreflightSnapshotDto, BridgePreflightProvider, BridgePreflightSnapshotProvider,
    BridgeProbeSnapshotProvider, BridgeServicesSnapshotDto, BridgeSnapshotProvider,
    DirectHttpModelProbeConfig, HealthDto,
    RuntimeReadModelDto, ServiceInfo, VersionHandshakeDto,
};
use crate::errors::ApiError;
use crate::routes::settings::{
    builtin_role_templates, enabled_registry_agent_roles, load_registry_engines,
    resolve_registry_agents,
};
use crate::settings_store::SettingsStore;
use crate::task_execution::ShadowTaskExecutionRegistry;
use magi_bridge_client::{BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient, McpServerConfig, ModelBridgeClient, StdioMcpBridgeClient};
use magi_core::{TaskId, WorkerId};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_memory_store::MemoryStore;
use magi_orchestrator::{
    ExecutionWritebackPlans,
    OrchestratedExecutionRuntime, OrchestratorCommandError, OrchestratorService,
    RecoveryExecutionResult,
    task_worker_catalog::build_worker_catalog_for_roles,
    task_runner::{EventBasedResultReceiver, EventBasedTaskDispatcher, RunCycleOutcome, TaskDispatcher, TaskResultReceiver, TaskRunner, WorkerExecutionDispatcher, WorkerInfo},
    task_store::TaskStore,
};
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, atomic::{AtomicBool, AtomicU64, Ordering}};

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
        let workers = self.resolved_workers();
        let task_runner = if let Some(ref dispatcher) = self.dispatcher {
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
                        let mut status =
                            bg_handle.status.lock().expect("status lock should hold");
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
                        let mut status =
                            bg_handle.status.lock().expect("status lock should hold");
                        *status = "error".to_string();
                        let mut last_error =
                            bg_handle.last_error.lock().expect("last_error lock should hold");
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
        let handle = runners
            .get(root_task_id)
            .ok_or(RunnerStopError::NotFound)?;
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
            let status = handle.status.lock().expect("status lock should hold").clone();
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
        let workers = self.resolved_workers();
        let task_runner = if let Some(ref dispatcher) = self.dispatcher {
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
        Ok(task_runner.run_cycle(&tid))
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

impl ShadowExecutionPipeline {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn execute_recovery_with_writebacks(
        &self,
        input: magi_core::RecoveryResumeInput,
        worker_id: WorkerId,
        writebacks: ExecutionWritebackPlans,
    ) -> Result<RecoveryExecutionResult, OrchestratorCommandError> {
        self.execution_runtime.execute_recovery_with_writebacks(
            input,
            worker_id,
            None,
            self.memory_store.clone(),
            writebacks,
        )
    }

}

#[derive(Clone)]
pub struct ApiState {
    pub service_info: ServiceInfo,
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
    mcp_connections: Arc<RwLock<HashMap<String, Arc<StdioMcpBridgeClient>>>>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    pub skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
}

#[derive(Clone, Debug)]
pub struct RuntimeStatePersistence {
    session_path: PathBuf,
    workspace_path: PathBuf,
    knowledge_path: PathBuf,
}

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
        }
    }

    pub fn state_root(&self) -> Option<&Path> {
        self.session_path.parent()
    }

    fn save_json<T>(&self, path: &Path, value: &T) -> Result<(), ApiError>
    where
        T: serde::Serialize,
    {
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

    fn save_session_store(&self, store: &SessionStore) -> Result<(), ApiError> {
        self.save_json(&self.session_path, &store.durable_state())
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
    file_name.push_str(".tmp");
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
            mcp_connections: Arc::new(RwLock::new(HashMap::new())),
            model_bridge_client: None,
            skill_runtime: None,
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

    pub fn bootstrap_dto(&self) -> BootstrapDto {
        BootstrapDto::from_state(self)
    }

    pub fn runtime_read_model_dto(&self) -> RuntimeReadModelDto {
        runtime_read_model_dto(
            self.event_bus.runtime_read_model_input(),
            &self.session_store.execution_sidecar_exports(),
            &self.workspace_registry.recovery_sidecar_exports(),
            self.audit_usage_ledger_dto(),
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
        let mut snapshot = self.settings_store.snapshot();
        normalize_settings_snapshot_sections(&mut snapshot);
        snapshot.insert(
            "roleTemplates".to_string(),
            serde_json::Value::Array(builtin_role_templates()),
        );
        snapshot.insert(
            "engines".to_string(),
            serde_json::Value::Array(load_registry_engines(self)),
        );
        snapshot.insert(
            "agents".to_string(),
            serde_json::Value::Array(resolve_registry_agents(self)),
        );
        self.enrich_mcp_servers_with_connection_status(&mut snapshot);
        serde_json::json!(snapshot)
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

    pub fn with_runtime_persistence(
        mut self,
        persistence: Arc<RuntimeStatePersistence>,
    ) -> Self {
        self.runtime_persistence = Some(persistence);
        self
    }

    pub fn persist_session_durable_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        persistence.save_session_store(&self.session_store)
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

    pub fn with_model_bridge_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_bridge_client = Some(client);
        self
    }

    pub fn with_skill_runtime(mut self, skill_runtime: Arc<magi_skill_runtime::SkillRuntime>) -> Self {
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
}

fn normalize_settings_snapshot_sections(snapshot: &mut HashMap<String, serde_json::Value>) {
    for key in ["orchestrator", "auxiliary", "userRules", "safeguard", "skillsConfig"] {
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

fn normalize_wrapped_section_value(value: &mut serde_json::Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    let nested = object
        .get("config")
        .or_else(|| object.get("data"))
        .cloned();
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
        let legacy = snapshot.remove("safeguard").unwrap_or(serde_json::json!({}));
        snapshot.insert("safeguardConfig".to_string(), legacy);
    }

    let safeguard = snapshot
        .get_mut("safeguardConfig")
        .expect("safeguardConfig just inserted");

    let existing_rules = safeguard
        .get("rules")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let has_builtin = existing_rules
        .iter()
        .any(|r| r.get("category").and_then(|v| v.as_str()).is_some_and(|c| c != "custom"));

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

#[cfg(test)]
mod tests {
    use super::ShadowExecutionPipeline;
    use magi_core::{AbsolutePath, ExecutionOwnership, MissionId, SessionId, TaskId, WorkerId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::{ExecutionWritebackPlans, OrchestratorCommand, OrchestratorService};
    use magi_session_store::SessionStore;
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    #[test]
    fn recovery_writeback_pipeline_persists_recovery_memory_extraction_on_success() {
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator =
            OrchestratorService::with_governance(Arc::clone(&event_bus), Arc::clone(&governance));
        let control = orchestrator.control_plane();
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mission_id = MissionId::new("mission-recovery-pipeline");
        let assignment_id = magi_core::AssignmentId::new("assignment-recovery-pipeline");
        let task_id = TaskId::new("task-recovery-pipeline");
        let session_id = SessionId::new("session-recovery-pipeline");
        let workspace_id = WorkspaceId::new("workspace-recovery-pipeline");
        let worker_id = WorkerId::new("worker-recovery-pipeline");

        session_store
            .create_session(session_id.clone(), "session")
            .expect("session should be creatable");
        session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(mission_id.clone()),
                task_id: Some(task_id.clone()),
                worker_id: Some(worker_id.clone()),
                execution_chain_ref: Some("chain-recovery-pipeline".to_string()),
            },
        );

        workspace_store
            .register(workspace_id.clone(), AbsolutePath::new("/Users/xie/code/magi"))
            .expect("workspace should be creatable");
        let recovery_handle = workspace_store.prepare_recovery_entry(
            workspace_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(mission_id.clone()),
                task_id: Some(task_id.clone()),
                worker_id: Some(worker_id.clone()),
                execution_chain_ref: Some("chain-recovery-pipeline".to_string()),
            },
            "snapshot-recovery-pipeline",
            "recovery-recovery-pipeline",
            Some("resume parser after crash".to_string()),
        );
        workspace_store
            .mark_recovery_ready(&recovery_handle.recovery_id)
            .expect("recovery should be ready");

        let _ = control.execute(OrchestratorCommand::CreateMission {
            mission_id: mission_id.clone(),
            title: "mission".to_string(),
        });
        let _ = control.execute(OrchestratorCommand::AddAssignment {
            mission_id: mission_id.clone(),
            assignment_id: assignment_id.clone(),
            title: "assignment".to_string(),
        });
        let _ = control.execute(OrchestratorCommand::CreateTask {
            mission_id: mission_id.clone(),
            assignment_id: assignment_id,
            task_id: task_id.clone(),
            title: "todo".to_string(),
        });

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime_with_recovery_support(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
        );
        let pipeline = ShadowExecutionPipeline {
            orchestrator,
            execution_runtime,
            memory_store: memory_store.clone(),
        };

        let recovery_input = workspace_store
            .build_recovery_resume_input(&recovery_handle.recovery_id)
            .expect("recovery input should be buildable");
        let writebacks = ExecutionWritebackPlans::from_recovery_resume_input(&recovery_input);
        let result = pipeline
            .execute_recovery_with_writebacks(recovery_input, worker_id, writebacks)
            .expect("recovery should execute");

        assert_eq!(result.decision.task_id, task_id);
        let verification = memory_store
            .verify_extraction_linkage("extract-recovery-recovery-recovery-pipeline")
            .expect("recovery writeback should persist extraction linkage");
        assert!(verification.is_consistent);
        let linkage = memory_store
            .extraction_linkage("extract-recovery-recovery-recovery-pipeline")
            .expect("recovery extraction linkage should exist");
        assert_eq!(
            linkage.extraction.source_ref.as_deref(),
            Some("recovery://recovery-recovery-pipeline/snapshot/snapshot-recovery-pipeline")
        );
        assert_eq!(linkage.produced_records[0].content, "resume parser after crash");
    }
}

fn build_mcp_config_from_entry(entry: &serde_json::Value) -> Option<McpServerConfig> {
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
