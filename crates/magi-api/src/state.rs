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
};
use crate::routes::settings::{
    load_registry_engines, registered_role_template_ids, resolve_registry_agents,
    role_templates_for_registry,
};
use crate::scope_binding::strip_scope_binding_fields;
use crate::skill_loader;
use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, JsonRpcBridgeServerProbeClient, McpServerClient,
    ModelBridgeClient,
};
use magi_browser_runtime::{
    BrowserAuthority, BrowserAuthorityError, BrowserCapabilitySnapshot, BrowserDurableState,
    BrowserHostClient, BrowserHostCommand, BrowserHostCommandOutcome, BrowserHostCommandResult,
    BrowserHostControlMode, BrowserLeaseEndReason, BrowserLeaseSelector, BrowserProfile,
    BrowserProfileControlMode, BrowserProfileControlSnapshot, BrowserProfileKind,
    BrowserRuntimeComponentStatus, BrowserRuntimeControlClient, BrowserRuntimeUpdateLevel,
    BrowserScreencastFormat,
};
use magi_conversation_runtime::{
    ConversationRegistry,
    execution_admission::{ExecutionAdmissionController, ExecutionAdmissionSnapshot},
    task_execution_dispatcher::{ExecutionPipeline, LlmTaskDispatcher},
    task_execution_registry::TaskExecutionRegistry,
    task_runner::TaskRunner,
    task_runner_bridge::{
        EventBasedResultReceiver, RunCycleOutcome, TaskDispatchGate, TaskDispatcher,
        TaskResultReceiver,
    },
};
use magi_core::{
    AccessProfile, BrowserProfileId, BrowserTabId, SessionId, SessionLifecycleStatus, TaskId,
    TaskTier, UtcMillis, WorkspaceId, public_runtime_excerpt,
};
use magi_event_bus::{
    EventContext, EventEnvelope, InMemoryEventBus, latest_usage_observations_from_ledger,
};
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
use magi_snapshot::{BaselinePatchEntry, SnapshotManager, SnapshotSession};
use magi_tool_runtime::{
    RuntimeCapabilityDependencyEntry, RuntimeCapabilityDependencyProvider, ToolExecutionContext,
    ToolExecutionContextQuery, ToolRegistry,
};
use magi_workspace::WorkspaceStore;
use serde::{Deserialize, Serialize};
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
    pub request: SessionTurnRequestDto,
    pub requested_workspace_id: WorkspaceId,
    pub accepted_at: UtcMillis,
    pub route: SessionTurnRouteDto,
    pub task_title: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    pub tool_intent: Option<String>,
    pub forced_tool_name: Option<String>,
    #[serde(default)]
    pub goal_mode: bool,
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
    fn normalize_identity(&mut self) {
        if self.request.request_id().is_none() {
            self.request.request_id = Some(format!("request-{}", self.queue_id));
        }
        if self.request.user_message_id().is_none() {
            self.request.user_message_id = Some(format!("turn-item-user-{}", self.queue_id));
        }
    }
}

pub(crate) fn session_has_user_content(session: &SessionRecord) -> bool {
    session.message_count.unwrap_or(0) > 0
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
        session_store: Arc<SessionStore>,
        worker_catalog: Arc<dyn Fn() -> Vec<WorkerInfo> + Send + Sync>,
        dispatcher: Arc<dyn TaskDispatcher>,
        result_receiver: Arc<EventBasedResultReceiver>,
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
        runner = runner.with_execution_admission(Arc::clone(&self.execution_admission), session_id);
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

    /// 串行化 session 与 root task 生命周期后启动 runner。
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

        let observer_session_id = session_id.clone();
        let task_runner = Arc::new(self.build_task_runner(session_id.clone()));
        let root_id = tid;
        let bg_handle = Arc::clone(&handle);
        let bg_active = Arc::clone(&handle.active);
        let bg_task_store = Arc::clone(&self.task_store);
        let bg_checkpoint_path = self.checkpoint_path.clone();
        let terminal_observer = self.terminal_observer.clone();
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
                    RunCycleOutcome::Waiting => {
                        waiting_streak = waiting_streak.saturating_add(1);
                        let backoff_ms = 200u64.saturating_mul(waiting_streak as u64).min(2_000);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                    RunCycleOutcome::Unrunnable(task_ids) => {
                        let runner_status =
                            match task_runner.finalize_unrunnable_outcome(&root_id, &task_ids) {
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
                        let mut status = bg_handle.status.lock().expect("status lock should hold");
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

    pub async fn lock_for_restart(&self, root_task_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .restart_locks
                .lock()
                .expect("runner restart locks should hold");
            locks
                .entry(root_task_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    pub async fn quiesce_for_restart(&self, root_task_id: &str) {
        let existing = {
            self.runners
                .lock()
                .expect("runners lock should hold")
                .get(root_task_id)
                .cloned()
        };
        let Some(handle) = existing else {
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
    pub async fn unbind_session(&self, session_id: &SessionId) -> usize {
        let _session_guard = self.lock_session_lifecycle(session_id).await;
        self.unbind_session_after_lifecycle_lock(session_id).await
    }

    pub async fn unbind_session_after_lifecycle_lock(&self, session_id: &SessionId) -> usize {
        let mut root_task_ids = {
            let mut index = self
                .session_runner_index
                .lock()
                .expect("session_runner_index lock should hold");
            index.remove(session_id).unwrap_or_default()
        };
        if let Some(chain) = self.session_store.active_execution_chain(session_id)
            && !root_task_ids
                .iter()
                .any(|root_task_id| root_task_id == chain.root_task_id.as_str())
        {
            root_task_ids.push(chain.root_task_id.to_string());
        }
        for root_task_id in &root_task_ids {
            let _restart_guard = self.lock_for_restart(root_task_id).await;
            self.quiesce_for_restart(root_task_id).await;
        }
        self.execution_admission.remove_queued_session(session_id);
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
        let task_runner = self.build_task_runner(None);
        Ok(task_runner.run_cycle(&tid))
    }

    pub fn kill_tree(&self, root_task_id: &str) -> Result<(), String> {
        let tid = TaskId::new(root_task_id);
        self.task_store
            .get_task(&tid)
            .ok_or_else(|| format!("任务不存在: {}", root_task_id))?;
        self.signal_stop_if_present(root_task_id);
        self.build_task_runner(None).kill_tree(&tid)?;
        self.set_runner_status_if_present(root_task_id, "killed");
        Ok(())
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserRuntimeStatusSnapshot {
    pub revision: u64,
    pub in_app_browser_enabled: bool,
    pub browser_use_enabled: bool,
    pub component_status: BrowserRuntimeComponentStatus,
    pub runtime_mode: String,
    pub host_status: String,
    pub host_protocol_compatible: bool,
    pub runtime_version: Option<String>,
    pub host_version: Option<String>,
    pub playwright_version: Option<String>,
    pub chromium_version: Option<String>,
    pub available_runtime_version: Option<String>,
    pub required_magi_version: Option<String>,
    pub update_level: Option<BrowserRuntimeUpdateLevel>,
    pub component_management_available: bool,
    pub last_error_code: Option<String>,
}

impl Default for BrowserRuntimeStatusSnapshot {
    fn default() -> Self {
        Self {
            revision: 1,
            in_app_browser_enabled: true,
            browser_use_enabled: true,
            component_status: BrowserRuntimeComponentStatus::NotInstalled,
            runtime_mode: "unavailable".to_string(),
            host_status: "stopped".to_string(),
            host_protocol_compatible: false,
            runtime_version: None,
            host_version: None,
            playwright_version: None,
            chromium_version: None,
            available_runtime_version: None,
            required_magi_version: None,
            update_level: None,
            component_management_available: false,
            last_error_code: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowserScreencastSubscription {
    tab_id: BrowserTabId,
    host_generation: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BrowserScreencastOptions {
    pub format: BrowserScreencastFormat,
    pub quality: u8,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Default)]
struct BrowserScreencastEntry {
    host_generation: u64,
    subscriber_count: usize,
    active: bool,
}

#[derive(Debug, Default)]
pub(crate) struct BrowserScreencastCoordinator {
    host_generation: AtomicU64,
    entries: Mutex<HashMap<BrowserTabId, Arc<tokio::sync::Mutex<BrowserScreencastEntry>>>>,
}

impl BrowserScreencastCoordinator {
    fn advance_host_generation(&self) {
        self.host_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn host_generation(&self) -> u64 {
        self.host_generation.load(Ordering::Acquire)
    }

    fn entry(&self, tab_id: &BrowserTabId) -> Arc<tokio::sync::Mutex<BrowserScreencastEntry>> {
        self.entries
            .lock()
            .expect("browser screencast registry lock poisoned")
            .entry(tab_id.clone())
            .or_default()
            .clone()
    }

    pub(crate) async fn subscribe(
        &self,
        client: &BrowserHostClient,
        host_generation: u64,
        tab_id: &BrowserTabId,
        options: BrowserScreencastOptions,
    ) -> Result<BrowserScreencastSubscription, String> {
        if self.host_generation() != host_generation {
            return Err("浏览器 Host 已切换，无法建立画面订阅".to_string());
        }
        let entry = self.entry(tab_id);
        let mut current = entry.lock().await;
        if self.host_generation() != host_generation {
            return Err("浏览器 Host 已切换，无法建立画面订阅".to_string());
        }
        if current.host_generation != host_generation {
            *current = BrowserScreencastEntry {
                host_generation,
                ..BrowserScreencastEntry::default()
            };
        }
        let reply = client
            .request(BrowserHostCommand::StartScreencast {
                tab_id: tab_id.clone(),
                format: options.format,
                quality: options.quality,
                max_width: options.max_width,
                max_height: options.max_height,
            })
            .await
            .map_err(|error| format!("启动浏览器画面流失败: {error}"))?;
        if self.host_generation() != host_generation {
            return Err("浏览器 Host 在画面流启动期间发生切换".to_string());
        }
        if !matches!(
            reply.response.outcome,
            BrowserHostCommandOutcome::Succeeded(ref result)
                if matches!(result.as_ref(), BrowserHostCommandResult::Empty)
        ) {
            return Err(format!(
                "启动浏览器画面流失败: {:?}",
                reply.response.outcome
            ));
        }
        current.active = true;
        current.subscriber_count = current.subscriber_count.saturating_add(1);
        Ok(BrowserScreencastSubscription {
            tab_id: tab_id.clone(),
            host_generation,
        })
    }

    pub(crate) async fn unsubscribe(
        &self,
        client: &BrowserHostClient,
        subscription: BrowserScreencastSubscription,
    ) {
        if self.host_generation() != subscription.host_generation {
            return;
        }
        let entry = self.entry(&subscription.tab_id);
        let mut current = entry.lock().await;
        if current.host_generation != subscription.host_generation || current.subscriber_count == 0
        {
            return;
        }
        current.subscriber_count -= 1;
        if current.subscriber_count > 0 || !current.active {
            return;
        }
        current.active = false;
        let result = client
            .request(BrowserHostCommand::StopScreencast {
                tab_id: subscription.tab_id.clone(),
            })
            .await;
        if !matches!(
            result,
            Ok(ref reply) if matches!(
                reply.response.outcome,
                BrowserHostCommandOutcome::Succeeded(ref result)
                    if matches!(result.as_ref(), BrowserHostCommandResult::Empty)
            )
        ) {
            tracing::warn!(
                tab_id = %subscription.tab_id,
                ?result,
                "停止浏览器画面流失败"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionResourceCancellationReport {
    pub process_count: usize,
    pub browser_lease_count: usize,
}

impl ExecutionResourceCancellationReport {
    pub fn total(self) -> usize {
        self.process_count.saturating_add(self.browser_lease_count)
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
        let process_count = self
            .tool_registry
            .read()
            .expect("execution resource tool registry lock poisoned")
            .as_ref()
            .map_or(0, |registry| registry.cancel_active_processes(&query));
        let selector = BrowserLeaseSelector {
            session_id: query.session_id.clone(),
            workspace_id: query.workspace_id.clone(),
            task_id: query.task_id.clone(),
            worker_id: query.worker_id.clone(),
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
            let profile_ids = revoked
                .iter()
                .map(|lease| lease.profile_id.clone())
                .collect::<HashSet<_>>();
            let controls = profile_ids
                .iter()
                .filter_map(|profile_id| authority.profile_control_snapshot(profile_id).ok())
                .collect::<Vec<_>>();
            (revoked, controls)
        };
        self.synchronize_browser_controls(controls);
        for lease in &revoked {
            self.event_bus.publish(
                EventEnvelope::domain(
                    magi_core::EventId::new(format!(
                        "event-browser-lease-revoked-{}-{}",
                        lease.lease_id, now.0
                    )),
                    "browser.lease.revoked",
                    serde_json::json!({
                        "lease_id": lease.lease_id,
                        "browser_session_id": lease.browser_session_id,
                        "profile_id": lease.profile_id,
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
        ExecutionResourceCancellationReport {
            process_count,
            browser_lease_count: revoked.len(),
        }
    }

    fn synchronize_browser_controls(&self, controls: Vec<BrowserProfileControlSnapshot>) {
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

fn spawn_browser_control_sync(client: BrowserHostClient, control: BrowserProfileControlSnapshot) {
    let future = async move {
        let result = client
            .request(BrowserHostCommand::UpdateControl {
                fence: control.fence,
                mode: match control.mode {
                    BrowserProfileControlMode::Agent => BrowserHostControlMode::Agent,
                    BrowserProfileControlMode::User => BrowserHostControlMode::User,
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
                profile_id = %control.profile_id,
                fence = control.fence,
                outcome = ?reply.response.outcome,
                "同步已撤销 Browser Lease 的 Host fence 失败"
            ),
            Err(error) => tracing::warn!(
                profile_id = %control.profile_id,
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

#[derive(Clone)]
pub struct ApiState {
    pub service_info: ServiceInfo,
    runtime_epoch: String,
    pub event_bus: Arc<InMemoryEventBus>,
    pub session_store: Arc<SessionStore>,
    pub workspace_registry: Arc<WorkspaceStore>,
    /// Git branch/worktree 的唯一结构化执行服务。repository mutex 在所有 session 间共享。
    pub git_service: Arc<magi_git::GitService>,
    /// 主对话 session 与代码/Git 上下文的正交绑定，不承载 conversation fork 或任务分支。
    pub session_code_contexts: magi_git::SessionCodeContextRegistry,
    /// turn/worker 与 Git mutation 的 workspace 级 lease 协调器，消除“检查后立即竞态”。
    pub workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator,
    pub governance: Arc<GovernanceService>,
    pub knowledge_store: Arc<KnowledgeStore>,
    pub settings_store: Arc<SettingsStore>,
    pub appearance_library: Arc<magi_appearance::AppearanceLibrary>,
    runtime_persistence: Option<Arc<RuntimeStatePersistence>>,
    session_state_checkpoint_persist: Option<SessionStateCheckpointPersist>,
    bridge_probe_snapshot_provider: BridgeProbeSnapshotProvider,
    bridge_preflight_snapshot_provider: BridgePreflightSnapshotProvider,
    bridge_cutover_smoke_provider: BridgeCutoverSmokeSnapshotProvider,
    bridge_snapshot_provider: Option<Arc<dyn BridgeSnapshotProvider>>,
    execution_pipeline: Option<ExecutionPipeline>,
    task_execution_registry: TaskExecutionRegistry,
    task_store: Option<Arc<TaskStore>>,
    runner_manager: Option<Arc<RunnerManager>>,
    session_turn_dispatcher: Option<Arc<LlmTaskDispatcher>>,
    mcp_connections: Arc<RwLock<HashMap<String, Arc<McpServerClient>>>>,
    model_bridge_client: Option<Arc<dyn ModelBridgeClient>>,
    model_bridge_client_is_real: bool,
    tool_registry: Option<ToolRegistry>,
    pub browser_authority: Arc<Mutex<BrowserAuthority>>,
    browser_write_lock: Arc<Mutex<()>>,
    pub(crate) browser_control_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) browser_viewport_controllers: Arc<Mutex<HashMap<BrowserTabId, String>>>,
    pub(crate) browser_screencasts: Arc<BrowserScreencastCoordinator>,
    browser_state_writable: Arc<AtomicBool>,
    browser_runtime_status: Arc<RwLock<BrowserRuntimeStatusSnapshot>>,
    browser_host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    browser_runtime_control: Arc<RwLock<Option<BrowserRuntimeControlClient>>>,
    execution_resources: ExecutionResourceCoordinator,
    pub skill_runtime: Option<Arc<magi_skill_runtime::SkillRuntime>>,
    pub skill_dispatch_runtime: Option<Arc<magi_skill_runtime::SkillDispatchRuntime>>,
    pub tunnel_manager: crate::tunnel::TunnelManager,
    pub snapshot_manager: Arc<SnapshotManager>,
    pub conversation_registry: Arc<ConversationRegistry>,
    pub(crate) terminal_sessions: crate::terminal_runtime::TerminalSessionManager,
    /// 任务系统：AgentRole 注册表（替代 task_worker_catalog 硬编码 prompt）。
    /// 加载策略：`~/.magi/roles/*.json` 优先，回落到 crate 内置 builtin 集。
    pub agent_role_registry: Arc<magi_agent_role::AgentRoleRegistry>,
    /// 任务系统 — L5：父子任务关系图，作为 task_dispatch 中
    /// "parent_task_id 散落查询"的统一上层。同一进程共享。
    pub spawn_graph: Arc<Mutex<magi_spawn_graph::SpawnGraph>>,
    session_turn_queue: Arc<Mutex<HashMap<SessionId, VecDeque<QueuedRegularSessionTurn>>>>,
    session_turn_locks: Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>,
    session_change_sync_locks: Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>,
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
const BROWSER_PERSISTENCE_PUBLIC_ERROR: &str = "浏览器状态暂不可保存，请稍后重试";
const DEFAULT_BROWSER_PROFILE_ID: &str = "browser-profile-default";

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

    pub(crate) fn save_knowledge_store(&self, store: &KnowledgeStore) -> Result<(), ApiError> {
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
    let durable: BrowserDurableState = serde_json::from_slice(&bytes)?;
    let mut authority = BrowserAuthority::restore_durable(durable, UtcMillis::now())?;
    if authority
        .profile(&BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID))
        .is_none()
    {
        authority.register_profile(BrowserProfile {
            profile_id: BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID),
            kind: BrowserProfileKind::ManagedDefault,
            data_path: state_root.join("browser/profiles/default"),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        })?;
    }
    Ok((authority, false))
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
        | BrowserAuthorityError::UserHasControl(_)
        | BrowserAuthorityError::LeaseConflict { .. }
        | BrowserAuthorityError::LeaseNotHeld(_)
        | BrowserAuthorityError::LeaseExpired(_)
        | BrowserAuthorityError::LeaseFenceMismatch { .. }
        | BrowserAuthorityError::LeaseSessionMismatch { .. }
        | BrowserAuthorityError::GoalBindingMismatch
        | BrowserAuthorityError::LeaseOwnerMismatch
        | BrowserAuthorityError::LeaseTurnMismatch
        | BrowserAuthorityError::SnapshotRevisionMismatch { .. }
        | BrowserAuthorityError::FrameSequenceMismatch { .. }
        | BrowserAuthorityError::NavigationRevisionMismatch { .. }
        | BrowserAuthorityError::NavigationRevisionRegression { .. } => {
            ApiError::Conflict(error.to_string())
        }
        BrowserAuthorityError::InvalidSnapshot(_) => {
            ApiError::InternalAssemblyError(error.to_string())
        }
        _ => ApiError::InvalidInput(error.to_string()),
    }
}

impl ApiState {
    pub fn new(
        service_name: impl Into<String>,
        event_bus: Arc<InMemoryEventBus>,
        session_store: Arc<SessionStore>,
        workspace_registry: Arc<WorkspaceStore>,
        governance: Arc<GovernanceService>,
    ) -> Self {
        let browser_authority = Arc::new(Mutex::new(BrowserAuthority::new()));
        let browser_write_lock = Arc::new(Mutex::new(()));
        let browser_control_lock = Arc::new(tokio::sync::Mutex::new(()));
        let browser_viewport_controllers = Arc::new(Mutex::new(HashMap::new()));
        let browser_screencasts = Arc::new(BrowserScreencastCoordinator::default());
        let browser_host_client = Arc::new(RwLock::new(None));
        let execution_resources = ExecutionResourceCoordinator::new(
            Arc::clone(&browser_authority),
            Arc::clone(&browser_write_lock),
            Arc::clone(&browser_host_client),
            Arc::clone(&event_bus),
        );
        Self {
            service_info: ServiceInfo {
                service_name: service_name.into(),
                api_version: "v0".to_string(),
            },
            runtime_epoch: format!("runtime-{}", UtcMillis::now().0),
            event_bus,
            session_store,
            workspace_registry,
            git_service: Arc::new(magi_git::GitService::new()),
            session_code_contexts: magi_git::SessionCodeContextRegistry::default(),
            workspace_git_coordinator: magi_git::WorkspaceGitOperationCoordinator::default(),
            governance,
            knowledge_store: Arc::new(KnowledgeStore::new()),
            settings_store: Arc::new(SettingsStore::new()),
            appearance_library: Arc::new(magi_appearance::AppearanceLibrary::in_memory()),
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
            browser_authority,
            browser_write_lock,
            browser_control_lock,
            browser_viewport_controllers,
            browser_screencasts,
            browser_state_writable: Arc::new(AtomicBool::new(true)),
            browser_runtime_status: Arc::new(RwLock::new(BrowserRuntimeStatusSnapshot::default())),
            browser_host_client,
            browser_runtime_control: Arc::new(RwLock::new(None)),
            execution_resources,
            skill_runtime: None,
            skill_dispatch_runtime: None,
            tunnel_manager: crate::tunnel::TunnelManager::new(38123),
            snapshot_manager: Arc::new(SnapshotManager::new()),
            conversation_registry: Arc::new(ConversationRegistry::new()),
            terminal_sessions: crate::terminal_runtime::TerminalSessionManager::default(),
            agent_role_registry: Arc::new(magi_agent_role::AgentRoleRegistry::load_default()),
            spawn_graph: Arc::new(Mutex::new(magi_spawn_graph::SpawnGraph::new())),
            session_turn_queue: Arc::new(Mutex::new(HashMap::new())),
            session_turn_locks: Arc::new(Mutex::new(HashMap::new())),
            session_change_sync_locks: Arc::new(Mutex::new(HashMap::new())),
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
        if force_reconcile {
            snapshot
                .reconcile()
                .map_err(|error| ApiError::internal_assembly("刷新磁盘变更状态失败", error))?;
        }

        let Some(existing_context) = self.session_code_contexts.get(session_id.as_str()) else {
            match self.git_service.observe(workspace_root).await {
                Ok(observation) => {
                    self.align_snapshot_baseline_to_observation_head(
                        workspace_root,
                        &observation,
                        &snapshot,
                    )
                    .await?;
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
        snapshot: &SnapshotSession,
    ) -> Result<(), ApiError> {
        let previous_head = existing_context.git.base_head.as_deref().ok_or_else(|| {
            ApiError::InternalAssemblyError("原 Git baseline HEAD 缺失".to_string())
        })?;
        let current_head = observation.head.as_deref().ok_or_else(|| {
            ApiError::InternalAssemblyError("新 Git baseline HEAD 缺失".to_string())
        })?;
        snapshot
            .reconcile()
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
        snapshot: &SnapshotSession,
    ) -> Result<(), ApiError> {
        let Some(head) = observation.head.as_deref() else {
            return Ok(());
        };
        snapshot.reconcile().map_err(|error| {
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
        self.session_code_contexts = session_code_contexts;
        self.workspace_git_coordinator = workspace_git_coordinator;
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
        self.cancel_execution_resources(
            session_id,
            workspace_id,
            task_id,
            BrowserLeaseEndReason::TurnStopped,
        )
        .total()
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
            },
            reason,
            UtcMillis::now(),
        )
    }

    pub fn execution_resource_coordinator(&self) -> &ExecutionResourceCoordinator {
        &self.execution_resources
    }

    pub fn browser_runtime_status(&self) -> BrowserRuntimeStatusSnapshot {
        self.browser_runtime_status
            .read()
            .expect("browser runtime status lock poisoned")
            .clone()
    }

    pub fn set_browser_runtime_status(&self, mut status: BrowserRuntimeStatusSnapshot) {
        let mut current = self
            .browser_runtime_status
            .write()
            .expect("browser runtime status lock poisoned");
        status.revision = current.revision.saturating_add(1);
        status.in_app_browser_enabled = current.in_app_browser_enabled;
        status.browser_use_enabled = current.browser_use_enabled;
        *current = status;
    }

    pub fn update_browser_capability_settings(
        &self,
        in_app_browser_enabled: bool,
        browser_use_enabled: bool,
    ) -> Result<(), ApiError> {
        self.settings_store
            .set_section(
                "browser",
                serde_json::json!({
                    "inAppBrowserEnabled": in_app_browser_enabled,
                    "browserUseEnabled": browser_use_enabled,
                }),
            )
            .map_err(crate::errors::settings_persistence_error)?;
        let mut current = self
            .browser_runtime_status
            .write()
            .expect("browser runtime status lock poisoned");
        current.in_app_browser_enabled = in_app_browser_enabled;
        current.browser_use_enabled = browser_use_enabled;
        current.revision = current.revision.saturating_add(1);
        Ok(())
    }

    pub fn set_browser_host_client(&self, client: Option<BrowserHostClient>) {
        let mut current = self
            .browser_host_client
            .write()
            .expect("browser Host client lock poisoned");
        *current = client;
        self.browser_screencasts.advance_host_generation();
    }

    pub fn browser_host_client(&self) -> Option<BrowserHostClient> {
        self.browser_host_client
            .read()
            .expect("browser Host client lock poisoned")
            .clone()
    }

    pub(crate) fn browser_host_client_with_generation(&self) -> Option<(BrowserHostClient, u64)> {
        let current = self
            .browser_host_client
            .read()
            .expect("browser Host client lock poisoned");
        current
            .clone()
            .map(|client| (client, self.browser_screencasts.host_generation()))
    }

    pub fn set_browser_runtime_control(&self, control: BrowserRuntimeControlClient) {
        *self
            .browser_runtime_control
            .write()
            .expect("browser runtime control lock poisoned") = Some(control);
    }

    pub fn browser_runtime_control(&self) -> Option<BrowserRuntimeControlClient> {
        self.browser_runtime_control
            .read()
            .expect("browser runtime control lock poisoned")
            .clone()
    }

    pub fn browser_tool_runtime_dependencies(&self) -> crate::BrowserToolRuntimeDependencies {
        crate::BrowserToolRuntimeDependencies {
            authority: Arc::clone(&self.browser_authority),
            write_lock: Arc::clone(&self.browser_write_lock),
            control_lock: Arc::clone(&self.browser_control_lock),
            state_writable: Arc::clone(&self.browser_state_writable),
            runtime_status: Arc::clone(&self.browser_runtime_status),
            host_client: Arc::clone(&self.browser_host_client),
            event_bus: Arc::clone(&self.event_bus),
            session_store: Arc::clone(&self.session_store),
            persistence: self.runtime_persistence.clone(),
        }
    }

    pub fn browser_runtime_component_root(&self) -> Option<PathBuf> {
        self.runtime_persistence
            .as_ref()
            .and_then(|persistence| persistence.state_root())
            .map(|root| root.join("runtimes/browser"))
    }

    pub fn browser_capability_snapshot(
        &self,
        session_id: Option<&SessionId>,
    ) -> BrowserCapabilitySnapshot {
        let runtime = self.browser_runtime_status();
        let access_profile = session_id
            .and_then(|session_id| self.session_store.active_goal(session_id))
            .map_or(AccessProfile::Restricted, |goal| goal.access_profile);
        BrowserCapabilitySnapshot {
            revision: runtime.revision,
            in_app_browser_enabled: runtime.in_app_browser_enabled,
            browser_use_enabled: runtime.browser_use_enabled,
            runtime_status: runtime.component_status,
            host_protocol_compatible: runtime.host_protocol_compatible,
            access_profile,
        }
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
        let mut projection = self
            .session_store
            .projection_input_for_workspace_session(ws_id, requested_session_id);
        let selected_session_id = projection.current_session_id.clone();
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
            .sessions_for_workspace(workspace_id)
            .into_iter()
            .filter(session_has_user_content)
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
                let stored_path = workspace.native_root_path();
                canonicalize_path_for_workspace_match(&stored_path) == requested_path
            })
            .map(|workspace| workspace.workspace_id)
    }

    pub fn runtime_read_model_dto(&self) -> RuntimeReadModelDto {
        let mut dto = runtime_read_model_dto_with_usage(
            self.event_bus.runtime_read_model_input(),
            &self.session_store.execution_sidecar_exports(),
            &self.workspace_registry.recovery_sidecar_exports(),
            self.audit_usage_ledger_dto(),
            self.task_store(),
            &self.ledger_usage_observations(),
        );
        crate::dto::apply_configured_model_context_windows(&mut dto, &self.settings_store);
        dto
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
        let audit_ledger = self.audit_usage_ledger_dto();
        let safeguard_audit_count = self
            .event_bus
            .audit_usage_ledger_snapshot()
            .audit_entries
            .iter()
            .filter(|entry| entry.event_type == "security.safety.evaluated")
            .count();
        serde_json::json!({
            "workerConfigs": object_section(&snapshot, "workers"),
            "orchestratorConfig": object_section(&snapshot, "orchestrator"),
            "auxiliaryConfig": object_section(&snapshot, "auxiliary"),
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
                    "category": tool.get("category").cloned().unwrap_or(serde_json::Value::String("uncategorized".to_string())),
                    "riskLevel": tool.get("risk_level").cloned().unwrap_or(serde_json::Value::String("low".to_string())),
                    "approvalRequirement": tool.get("approval_requirement").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "effectiveApprovalPolicy": tool.get("effective_approval_policy").cloned().unwrap_or(serde_json::Value::String("none".to_string())),
                    "accessProfileBehavior": tool.get("access_profile_behavior").cloned().unwrap_or(serde_json::Value::String("restricted_allowed".to_string())),
                    "accessMode": tool.get("access_mode").cloned().unwrap_or(serde_json::Value::String("read_only".to_string())),
                    "policyScope": tool.get("policy_scope").cloned().unwrap_or(serde_json::Value::String("fixed".to_string())),
                    "modelCallScope": tool.get("model_call_scope").cloned().unwrap_or(serde_json::Value::String("session_or_task".to_string())),
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
        let browser = store.get_section("browser");
        let mut runtime = self
            .browser_runtime_status
            .write()
            .expect("browser runtime status lock poisoned");
        runtime.in_app_browser_enabled = browser
            .get("inAppBrowserEnabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        runtime.browser_use_enabled = browser
            .get("browserUseEnabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
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
            let path = state_root.join("session-git-contexts.json");
            match fs::read(&path) {
                Ok(bytes) => {
                    match serde_json::from_slice::<Vec<magi_git::SessionCodeContext>>(&bytes) {
                        Ok(contexts) => self.session_code_contexts.replace_all(contexts),
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
        self.runtime_persistence = Some(persistence);
        match browser_load {
            Ok(Some((authority, is_new))) => {
                *self
                    .browser_authority
                    .lock()
                    .expect("browser authority lock poisoned") = authority;
                if is_new && let Err(error) = self.persist_browser_durable_state() {
                    self.browser_state_writable.store(false, Ordering::Release);
                    self.set_browser_runtime_status(BrowserRuntimeStatusSnapshot {
                        component_status: BrowserRuntimeComponentStatus::Failed,
                        last_error_code: Some("browser_state_persist_failed".to_string()),
                        ..self.browser_runtime_status()
                    });
                    tracing::error!(?error, "初始化浏览器持久状态失败");
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.browser_state_writable.store(false, Ordering::Release);
                self.set_browser_runtime_status(BrowserRuntimeStatusSnapshot {
                    component_status: BrowserRuntimeComponentStatus::Failed,
                    host_protocol_compatible: false,
                    runtime_version: None,
                    host_status: "failed".to_string(),
                    last_error_code: Some("browser_state_invalid".to_string()),
                    revision: 0,
                    ..BrowserRuntimeStatusSnapshot::default()
                });
                tracing::error!(error = %error, "浏览器持久状态无效，禁止覆盖原文件");
            }
        }
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
        let durable = self
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .durable_state();
        persistence.save_json(&state_root.join("browser/state.json"), &durable)
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
        if !self.browser_state_writable.load(Ordering::Acquire) {
            return Err(ApiError::Conflict(
                "浏览器状态文件无效，修复前不能修改浏览器状态".to_string(),
            ));
        }
        let _write_guard = self
            .browser_write_lock
            .lock()
            .expect("browser authority write lock poisoned");
        let current = self
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .clone();
        let mut candidate = current;
        let output = mutation(&mut candidate).map_err(browser_authority_api_error)?;
        if let Some(persistence) = &self.runtime_persistence
            && let Some(state_root) = persistence.state_root()
        {
            persistence.save_json(
                &state_root.join("browser/state.json"),
                &candidate.durable_state(),
            )?;
        }
        *self
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned") = candidate;
        Ok(output)
    }

    pub(crate) fn accept_browser_viewport_controller(
        &self,
        tab_id: &BrowserTabId,
        controller_id: &str,
        claim: bool,
    ) -> bool {
        let mut controllers = self
            .browser_viewport_controllers
            .lock()
            .expect("browser viewport controller lock poisoned");
        match controllers.get(tab_id) {
            Some(current) if current == controller_id => true,
            Some(_) if !claim => false,
            _ => {
                controllers.insert(tab_id.clone(), controller_id.to_string());
                true
            }
        }
    }

    pub(crate) fn clear_browser_viewport_controller(&self, tab_id: &BrowserTabId) {
        self.browser_viewport_controllers
            .lock()
            .expect("browser viewport controller lock poisoned")
            .remove(tab_id);
    }

    pub fn record_browser_frame(
        &self,
        tab_id: &magi_core::BrowserTabId,
        frame_sequence: u64,
        now: UtcMillis,
    ) -> Result<(), ApiError> {
        let _write_guard = self
            .browser_write_lock
            .lock()
            .expect("browser authority write lock poisoned");
        self.browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .record_frame(tab_id, frame_sequence, now)
            .map_err(browser_authority_api_error)
    }

    pub fn restore_regular_session_turn_queues(&self) -> Result<usize, ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(0);
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(0);
        };
        let path = state_root.join("session-turn-queue.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(ApiError::internal_assembly(
                    "读取 session turn 排队状态失败",
                    error,
                ));
            }
        };
        let turns =
            serde_json::from_slice::<Vec<QueuedRegularSessionTurn>>(&bytes).map_err(|error| {
                ApiError::internal_assembly("解析 session turn 排队状态失败", error)
            })?;
        let mut queues = HashMap::<SessionId, VecDeque<QueuedRegularSessionTurn>>::new();
        for mut turn in turns {
            turn.normalize_identity();
            let session_is_active = self
                .session_store
                .session(&turn.session_id)
                .is_some_and(|session| session.status == SessionLifecycleStatus::Active);
            let route_is_queueable = matches!(
                turn.route,
                SessionTurnRouteDto::Chat
                    | SessionTurnRouteDto::Execute
                    | SessionTurnRouteDto::Task
            );
            if session_is_active && route_is_queueable {
                queues
                    .entry(turn.session_id.clone())
                    .or_default()
                    .push_back(turn);
            }
        }
        let restored_count = queues.values().map(VecDeque::len).sum();
        self.persist_regular_session_turn_queues(&queues)?;
        *self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned") = queues;
        Ok(restored_count)
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

    pub fn persist_session_state_checkpoint(&self, checkpoint: &str) -> Result<(), ApiError> {
        if let Some(persist) = &self.session_state_checkpoint_persist {
            persist(checkpoint)?;
        }
        self.persist_session_durable_state()?;
        self.persist_session_git_contexts()
    }

    /// 每轮执行前重新观测 session 绑定的 Git branch/HEAD/worktree。
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
        let workspace_root = self
            .workspace_root_path(&Some(workspace_id.clone()))
            .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace_id.as_str()))?;
        let _change_sync_guard = self.lock_session_change_sync(session_id).await;
        let session_key = session_id.as_str();
        let existing_context = self.session_code_contexts.get(session_key);
        if let Some(existing_context) = existing_context.as_ref() {
            self.workspace_git_coordinator
                .begin_execution(session_key, &existing_context.git.git_common_dir)
                .map_err(|error| ApiError::Conflict(error.to_string()))?;
        }
        let observation = match self.git_service.observe(&workspace_root).await {
            Ok(observation) => observation,
            Err(magi_git::GitError::NotRepository { .. }) => {
                self.release_session_git_execution_lease(session_id);
                return Ok(None);
            }
            Err(error) => {
                self.release_session_git_execution_lease(session_id);
                return Err(ApiError::InternalAssemblyError(format!(
                    "每轮执行前校验 Git context 失败: {error}"
                )));
            }
        };
        if existing_context.is_none() {
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
        if existing_context.is_none()
            && let Err(error) = self
                .workspace_git_coordinator
                .begin_execution(session_key, &context.git.git_common_dir)
        {
            return Err(ApiError::Conflict(error.to_string()));
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

    fn publish_session_git_fast_forward(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        context: &magi_git::SessionCodeContext,
        previous_head: &str,
    ) {
        let now = UtcMillis::now();
        self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::new(format!(
                    "workspace-git-context-changed-{workspace_id}-{}",
                    now.0
                )),
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
        let now = UtcMillis::now();
        self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::new(format!(
                    "workspace-git-context-changed-{workspace_id}-{}",
                    now.0
                )),
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

    /// 删除 session 时只回收干净的 agent worktree；任何未提交改动都保留目录与 context，
    /// 禁止用 `--force` 静默丢失代理产物。
    async fn cleanup_session_git_resources(&self, session_id: &SessionId) {
        self.release_session_git_execution_lease(session_id);
        let Some(context) = self.session_code_contexts.get(session_id.as_str()) else {
            return;
        };
        let mut cleanup_failed = false;
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
        if !cleanup_failed {
            self.session_code_contexts.remove(session_id.as_str());
        }
        if let Err(error) = self.persist_session_git_contexts() {
            tracing::warn!(session_id = %session_id, ?error, "持久化 session Git 资源清理失败");
        }
    }

    pub fn persist_session_durable_state(&self) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };

        self.session_store.persist_durable_state_with(|durable| {
            let (mut global_state, mut workspace_states) = durable.partition_by_workspace();
            let workspaces = self.workspace_registry.workspaces();
            for workspace in &workspaces {
                let ws_id = workspace.workspace_id.to_string();
                let ws_state = workspace_states.remove(&ws_id).unwrap_or_default();
                let magi_dir = workspace.native_root_path().join(".magi");
                let session_path = magi_dir.join("sessions.json");
                persistence.save_json(&session_path, &ws_state)?;
            }

            let orphan_session_count: usize = workspace_states
                .values()
                .map(|state| state.sessions.len())
                .sum();
            if orphan_session_count > 0 {
                global_state.clear_current_session_if_owned_by_workspace_states(&workspace_states);
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
        })
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

    pub(crate) fn schedule_workspace_code_index(
        &self,
        workspace_id: WorkspaceId,
        workspace_root: PathBuf,
    ) -> bool {
        if !self
            .knowledge_store
            .begin_workspace_index_build(&workspace_id)
        {
            return false;
        }

        let state = self.clone();
        tokio::spawn(async move {
            let build_state = state.clone();
            let build_workspace_id = workspace_id.clone();
            let build_result = tokio::task::spawn_blocking(move || {
                build_state
                    .knowledge_store
                    .build_workspace_index(&build_workspace_id, &workspace_root)
            })
            .await;
            let cancelled_before_persist = state
                .knowledge_store
                .workspace_index_build_cancelled(&workspace_id);
            match build_result {
                Ok(_) => {
                    if !cancelled_before_persist && let Err(error) = state.persist_knowledge_state()
                    {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            error = ?error,
                            "后台代码索引持久化失败"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        workspace_id = %workspace_id,
                        error = %error,
                        "后台代码索引构建任务失败"
                    );
                }
            }
            if state
                .knowledge_store
                .finish_workspace_index_build(&workspace_id)
                && let Err(error) = state.persist_knowledge_state()
            {
                tracing::warn!(
                    workspace_id = %workspace_id,
                    error = ?error,
                    "已取消的后台代码索引清理结果持久化失败"
                );
            }
        });
        true
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
        self.persist_session_git_contexts()?;
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
        self.runner_manager = Some(Arc::new(manager));
        self
    }

    pub fn with_shared_runner_manager(mut self, manager: Arc<RunnerManager>) -> Self {
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

    async fn lock_session_change_sync(
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

    fn persist_regular_session_turn_queues(
        &self,
        queues: &HashMap<SessionId, VecDeque<QueuedRegularSessionTurn>>,
    ) -> Result<(), ApiError> {
        let Some(persistence) = &self.runtime_persistence else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        let turns = queues
            .values()
            .flat_map(|queue| queue.iter().cloned())
            .collect::<Vec<_>>();
        persistence.save_json(&state_root.join("session-turn-queue.json"), &turns)
    }

    pub(crate) fn enqueue_regular_session_turn(
        &self,
        mut turn: QueuedRegularSessionTurn,
    ) -> Result<usize, ApiError> {
        turn.normalize_identity();
        let session_id = turn.session_id.clone();
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let queue = queues.entry(session_id.clone()).or_default();
        queue.push_back(turn);
        let queue_len = queue.len();
        if let Err(error) = self.persist_regular_session_turn_queues(&queues) {
            let queue = queues
                .get_mut(&session_id)
                .expect("刚入队的 session 队列必须存在");
            queue.pop_back();
            if queue.is_empty() {
                queues.remove(&session_id);
            }
            return Err(error);
        }
        Ok(queue_len)
    }

    pub(crate) fn peek_next_regular_session_turn(
        &self,
        session_id: &SessionId,
    ) -> Option<QueuedRegularSessionTurn> {
        self.session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned")
            .get(session_id)
            .and_then(|queue| queue.front())
            .cloned()
    }

    pub(crate) fn queued_regular_session_turns(
        &self,
        session_id: &SessionId,
    ) -> Vec<QueuedRegularSessionTurn> {
        self.session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned")
            .get(session_id)
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn remove_regular_session_turn(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<bool, ApiError> {
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let Some(queue) = queues.get_mut(session_id) else {
            return Ok(false);
        };
        let Some(position) = queue.iter().position(|turn| turn.queue_id == queue_id) else {
            return Ok(false);
        };
        let removed = queue
            .remove(position)
            .expect("已定位的 session turn 队列项必须存在");
        if queue.is_empty() {
            queues.remove(session_id);
        }
        if let Err(error) = self.persist_regular_session_turn_queues(&queues) {
            let queue = queues.entry(session_id.clone()).or_default();
            queue.insert(position.min(queue.len()), removed);
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) fn acknowledge_regular_session_turn(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<(), ApiError> {
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let Some(queue) = queues.get_mut(session_id) else {
            return Ok(());
        };
        if !queue.front().is_some_and(|turn| turn.queue_id == queue_id) {
            return Err(ApiError::internal_assembly(
                "确认 session turn 队列消费失败",
                "队首消息已变化",
            ));
        }
        let acknowledged = queue.pop_front().expect("已确认队首存在");
        if queue.is_empty() {
            queues.remove(session_id);
        }
        if let Err(error) = self.persist_regular_session_turn_queues(&queues) {
            queues
                .entry(session_id.clone())
                .or_default()
                .push_front(acknowledged);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn record_regular_session_turn_retry(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<u8, ApiError> {
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let queue = queues.get_mut(session_id).ok_or_else(|| {
            ApiError::internal_assembly("记录 session turn 重试失败", "session 队列不存在")
        })?;
        let turn = queue.front_mut().ok_or_else(|| {
            ApiError::internal_assembly("记录 session turn 重试失败", "session 队列为空")
        })?;
        if turn.queue_id != queue_id {
            return Err(ApiError::internal_assembly(
                "记录 session turn 重试失败",
                "队首消息已变化",
            ));
        }
        let previous_retry_count = turn.retry_count;
        turn.retry_count = turn.retry_count.saturating_add(1);
        let retry_count = turn.retry_count;
        if let Err(error) = self.persist_regular_session_turn_queues(&queues) {
            queues
                .get_mut(session_id)
                .and_then(|queue| queue.front_mut())
                .expect("重试计数更新期间队首必须存在")
                .retry_count = previous_retry_count;
            return Err(error);
        }
        Ok(retry_count)
    }

    pub(crate) fn queued_regular_session_turn_count(&self, session_id: &SessionId) -> usize {
        self.session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned")
            .get(session_id)
            .map(VecDeque::len)
            .unwrap_or(0)
    }

    pub(crate) fn queued_regular_session_ids(&self) -> Vec<SessionId> {
        self.session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn clear_regular_session_turn_queue_for_lifecycle(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, ApiError> {
        let mut queues = self
            .session_turn_queue
            .lock()
            .expect("session turn queue lock poisoned");
        let removed = queues.remove(session_id).unwrap_or_default();
        let removed_count = removed.len();
        if let Err(error) = self.persist_regular_session_turn_queues(&queues) {
            if !removed.is_empty() {
                queues.insert(session_id.clone(), removed);
            }
            return Err(error);
        }
        Ok(removed_count)
    }

    /// 会话删除的唯一资源回收入口。先停止并等待后台 runner，再删除所有运行态与
    /// 持久化事实，最后删除 SessionStore 主记录，避免任何组件保留孤儿状态。
    pub async fn delete_session_and_resources(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ApiError> {
        let _session_turn_guard = self.lock_session_turn(session_id).await;
        let manager = self.runner_manager();
        let _session_lifecycle_guard = match manager {
            Some(manager) => Some(manager.lock_session_lifecycle(session_id).await),
            None => None,
        };
        if let Some(manager) = manager {
            manager
                .unbind_session_after_lifecycle_lock(session_id)
                .await;
        }
        self.terminal_sessions
            .close_for_session(session_id.as_str());
        self.cleanup_session_git_resources(session_id).await;
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
            .map_err(|error| ApiError::internal_assembly("删除会话失败", error))?;
        drop(_session_lifecycle_guard);
        drop(_session_turn_guard);
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

    pub fn model_bridge_client_is_real(&self) -> bool {
        self.model_bridge_client_is_real
    }
}

fn normalize_settings_snapshot_sections(snapshot: &mut HashMap<String, serde_json::Value>) {
    for key in [
        "orchestrator",
        "auxiliary",
        "imageGeneration",
        "safeguardConfig",
    ] {
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
    array_section(snapshot, "mcpServers")
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
    use magi_core::{
        AbsolutePath, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
        ExecutionOwnership, MissionId, Task, TaskKind, TaskStatus, WorkerId,
    };
    use magi_orchestrator::task_store::TaskLease;
    use magi_session_store::{ActiveExecutionChain, ActiveExecutionDispatchContext};
    use std::collections::HashMap;
    use std::time::Duration;

    fn git_fixture(path: &Path, args: &[&str]) {
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
    }

    fn queued_turn_fixture(
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        queue_id: &str,
        accepted_at: u64,
    ) -> QueuedRegularSessionTurn {
        QueuedRegularSessionTurn {
            request: SessionTurnRequestDto {
                session_id: Some(session_id.to_string()),
                workspace_id: Some(workspace_id.to_string()),
                workspace_path: None,
                text: Some(format!("queued {queue_id}")),
                skill_name: None,
                locale: None,
                goal_mode: false,
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                access_profile: None,
                orchestrator_session_config: None,
                request_id: Some(format!("request-{queue_id}")),
                user_message_id: Some(format!("user-{queue_id}")),
                placeholder_message_id: Some(format!("assistant-{queue_id}")),
                steer_current_turn: false,
                expected_turn_id: None,
                replace_turn_id: None,
            },
            requested_workspace_id: workspace_id.clone(),
            accepted_at: UtcMillis(accepted_at),
            route: SessionTurnRouteDto::Chat,
            task_title: None,
            execution_goal: None,
            task_tier: TaskTier::ExecutionChain,
            tool_intent: None,
            forced_tool_name: None,
            goal_mode: false,
            required_tool_chain: Vec::new(),
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            session_id: session_id.clone(),
            workspace_id: Some(workspace_id.clone()),
            queue_id: queue_id.to_string(),
            retry_count: 0,
        }
    }

    #[test]
    fn browser_viewport_controller_enforces_single_writer_until_explicit_claim() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let tab_id = BrowserTabId::new("browser-tab-viewport-controller");

        assert!(state.accept_browser_viewport_controller(&tab_id, "controller-a", false));
        assert!(state.accept_browser_viewport_controller(&tab_id, "controller-a", false));
        assert!(!state.accept_browser_viewport_controller(&tab_id, "controller-b", false));
        assert!(state.accept_browser_viewport_controller(&tab_id, "controller-b", true));
        assert!(!state.accept_browser_viewport_controller(&tab_id, "controller-a", false));

        state.clear_browser_viewport_controller(&tab_id);
        assert!(state.accept_browser_viewport_controller(&tab_id, "controller-a", false));
    }

    #[test]
    fn execution_resource_coordinator_revokes_browser_lease_by_session_scope() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-resource-coordinator");
        let session_id = SessionId::new("session-resource-coordinator");
        let browser_session_id = BrowserSessionId::new("browser-session-resource-coordinator");
        let profile_id = BrowserProfileId::new("browser-profile-resource-coordinator");
        let tab_id = BrowserTabId::new("browser-tab-resource-coordinator");
        let lease_id = BrowserLeaseId::new("browser-lease-resource-coordinator");
        let now = UtcMillis(100);

        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(magi_browser_runtime::BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: magi_browser_runtime::BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("browser profile fixture should create")
                        .keep(),
                    created_at: now,
                    updated_at: now,
                })?;
                authority.create_session(magi_browser_runtime::CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: workspace_id.clone(),
                    session_id: session_id.clone(),
                    profile_id: profile_id.clone(),
                    now,
                })?;
                authority.transition_session(
                    &browser_session_id,
                    magi_browser_runtime::BrowserSessionLifecycle::Ready,
                    now,
                )?;
                authority.create_tab(magi_browser_runtime::CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "about:blank".to_string(),
                    viewport: magi_browser_runtime::BrowserViewport::default(),
                    now,
                })?;
                authority.transition_tab(
                    &tab_id,
                    magi_browser_runtime::BrowserTabLifecycle::Ready,
                    now,
                )?;
                authority.acquire_lease(magi_browser_runtime::AcquireBrowserLease {
                    lease_id: lease_id.clone(),
                    profile_id: profile_id.clone(),
                    browser_session_id,
                    owner: ExecutionOwnership {
                        session_id: Some(session_id.clone()),
                        workspace_id: Some(workspace_id.clone()),
                        task_id: Some(TaskId::new("task-resource-coordinator")),
                        ..ExecutionOwnership::default()
                    },
                    turn_id: "turn-resource-coordinator".to_string(),
                    goal_binding: None,
                    acquired_at: now,
                    expires_at: UtcMillis(1_000),
                })?;
                Ok(())
            })
            .expect("browser resource fixture should initialize");

        let report = state.cancel_execution_resources(
            Some(&session_id),
            None,
            None,
            magi_browser_runtime::BrowserLeaseEndReason::GoalPaused,
        );
        assert_eq!(report.browser_lease_count, 1);
        assert_eq!(report.total(), 1);
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should not poison");
        assert_eq!(
            authority.lease(&lease_id).map(|lease| lease.lifecycle),
            Some(magi_browser_runtime::BrowserLeaseLifecycle::Revoked)
        );
        assert_eq!(
            authority
                .lease(&lease_id)
                .and_then(|lease| lease.end_reason),
            Some(magi_browser_runtime::BrowserLeaseEndReason::GoalPaused)
        );
    }

    #[tokio::test]
    async fn session_cleanup_preserves_inactive_dirty_agent_worktree_context() {
        let fixture = tempfile::tempdir().expect("fixture root");
        let repository = fixture.path().join("repo");
        std::fs::create_dir_all(&repository).expect("repo directory");
        git_fixture(&repository, &["init", "-b", "main"]);
        git_fixture(&repository, &["config", "user.name", "Magi Test"]);
        git_fixture(&repository, &["config", "user.email", "magi@example.test"]);
        std::fs::write(repository.join("README.md"), "base\n").expect("fixture file");
        git_fixture(&repository, &["add", "README.md"]);
        git_fixture(&repository, &["commit", "-m", "base"]);

        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let session_id = SessionId::new("session-dirty-agent-cleanup");
        let observation = state
            .git_service
            .observe(&repository)
            .await
            .expect("observe repository");
        let context = state.session_code_contexts.accept(
            session_id.as_str(),
            "workspace-dirty-agent-cleanup",
            vec![repository.clone()],
            &observation,
        );
        let worktree_path = fixture.path().join("agent-worktree");
        let created = state
            .git_service
            .worktree_create(
                &repository,
                magi_git::WorktreeCreateOptions {
                    path: worktree_path.clone(),
                    base: observation.head.clone().expect("base head"),
                    branch: Some("magi/agent/dirty-cleanup".to_string()),
                    create_branch: true,
                    detached: false,
                    precondition: context.precondition(),
                },
            )
            .await
            .expect("create agent worktree");
        state
            .session_code_contexts
            .register_agent_worktree(
                session_id.as_str(),
                magi_git::AgentWorktreeContext {
                    task_id: "task-dirty-agent-cleanup".to_string(),
                    worker_id: "worker-dirty-agent-cleanup".to_string(),
                    path: created.path,
                    mode: magi_git::AgentWorktreeMode::Writable,
                    base_head: observation.head.expect("base head"),
                    branch: created.branch,
                    active: true,
                },
            )
            .expect("register agent worktree");
        std::fs::write(worktree_path.join("uncommitted.txt"), "preserve me\n")
            .expect("dirty agent output");
        state
            .session_code_contexts
            .release_agent_worktree(session_id.as_str(), "task-dirty-agent-cleanup")
            .expect("release agent worktree");

        state.cleanup_session_git_resources(&session_id).await;

        assert!(worktree_path.join("uncommitted.txt").is_file());
        let retained = state
            .session_code_contexts
            .get(session_id.as_str())
            .expect("dirty worktree context must remain recoverable");
        assert!(!retained.agent_worktrees[0].active);
    }

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
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
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
            _admission_permit: magi_conversation_runtime::execution_admission::ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            *self
                .observed_role
                .lock()
                .expect("observed role lock should not poison") = Some(worker.role.clone());
            Ok(())
        }
    }

    struct PanickingDispatcher;

    impl TaskDispatcher for PanickingDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            _worker: &WorkerInfo,
            _lease: &TaskLease,
            _admission_permit: magi_conversation_runtime::execution_admission::ExecutionAdmissionPermit,
        ) -> Result<(), String> {
            panic!("模拟 Runner 派发 panic");
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
            Arc::new(SessionStore::new()),
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

        let outcome = manager.build_task_runner(None).run_cycle(&root_task_id);

        assert_eq!(outcome, RunCycleOutcome::Continue);
        assert_eq!(
            observed_role
                .lock()
                .expect("observed role lock should not poison")
                .as_deref(),
            Some("auditor")
        );
    }

    #[test]
    fn kill_tree_signals_active_runner_before_updating_task_tree() {
        let store = Arc::new(TaskStore::new());
        let mut root_task = task_with_status("task-runner-interrupt", TaskStatus::Running);
        root_task.root_task_id = root_task.task_id.clone();
        store.insert_task(root_task);
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store.clone(),
            Arc::new(SessionStore::new()),
            Arc::new(Vec::new),
            Arc::new(RecordingDispatcher {
                observed_role: Arc::new(Mutex::new(None)),
            }),
            Arc::new(EventBasedResultReceiver::new()),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(true));
        manager.runners.lock().expect("runners should lock").insert(
            "task-runner-interrupt".to_string(),
            Arc::new(RunnerHandle {
                cancel: cancel.clone(),
                active,
                cycle_count: Arc::new(AtomicU64::new(1)),
                status: Arc::new(Mutex::new("running".to_string())),
                last_error: Arc::new(Mutex::new(None)),
                join_handle: Mutex::new(None),
            }),
        );

        manager
            .kill_tree("task-runner-interrupt")
            .expect("kill tree should succeed");

        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(
            manager
                .status("task-runner-interrupt")
                .expect("runner status should remain observable")
                .status,
            "killed"
        );
        assert_eq!(
            store
                .get_task(&TaskId::new("task-runner-interrupt"))
                .expect("root task should remain")
                .status,
            TaskStatus::Killed
        );
    }

    #[tokio::test]
    async fn unbind_session_waits_for_blocked_runner_and_removes_handle() {
        let store = Arc::new(TaskStore::new());
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store,
            Arc::new(SessionStore::new()),
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

    #[tokio::test]
    async fn unbind_session_stops_active_chain_runner_before_session_binding_exists() {
        let store = Arc::new(TaskStore::new());
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-active-chain-runner-cleanup");
        let root_task_id = TaskId::new("task-active-chain-runner-cleanup");
        session_store
            .create_session(session_id.clone(), "active chain cleanup")
            .expect("session should create");
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: MissionId::new("mission-active-chain-runner-cleanup"),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-active-runner-cleanup".to_string(),
                    workspace_id: None,
                    active_branch_task_ids: Vec::new(),
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: UtcMillis::now(),
                        entry_id: "entry-active-runner-cleanup".to_string(),
                        trimmed_text: None,
                        skill_name: None,
                    },
                    current_turn: None,
                },
            )
            .expect("active chain should persist");
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store,
            session_store,
            Arc::new(Vec::new),
            Arc::new(RecordingDispatcher {
                observed_role: Arc::new(Mutex::new(None)),
            }),
            Arc::new(EventBasedResultReceiver::new()),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let background_cancel = cancel.clone();
        let join_handle = tokio::spawn(async move {
            while !background_cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        manager.runners.lock().expect("runners should lock").insert(
            root_task_id.to_string(),
            Arc::new(RunnerHandle {
                cancel,
                active: Arc::new(AtomicBool::new(true)),
                cycle_count: Arc::new(AtomicU64::new(0)),
                status: Arc::new(Mutex::new("running".to_string())),
                last_error: Arc::new(Mutex::new(None)),
                join_handle: Mutex::new(Some(join_handle)),
            }),
        );

        assert_eq!(manager.unbind_session(&session_id).await, 1);
        assert!(manager.status(root_task_id.as_str()).is_none());
    }

    #[tokio::test]
    async fn quiesce_runner_waits_for_exit_and_removes_handle_before_restart() {
        let store = Arc::new(TaskStore::new());
        let root_task_id = "task-runner-restart-race";
        let mut root_task = task_with_status(root_task_id, TaskStatus::Running);
        root_task.root_task_id = root_task.task_id.clone();
        store.insert_task(root_task);
        let session_store = Arc::new(SessionStore::new());
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store,
            session_store,
            Arc::new(Vec::new),
            Arc::new(RecordingDispatcher {
                observed_role: Arc::new(Mutex::new(None)),
            }),
            Arc::new(EventBasedResultReceiver::new()),
        );
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
                active: active.clone(),
                cycle_count: Arc::new(AtomicU64::new(1)),
                status: Arc::new(Mutex::new("error".to_string())),
                last_error: Arc::new(Mutex::new(Some("旧执行轮即将退出".to_string()))),
                join_handle: Mutex::new(Some(join_handle)),
            }),
        );

        let _restart_guard = manager.lock_for_restart(root_task_id).await;
        manager.quiesce_for_restart(root_task_id).await;

        assert!(!active.load(Ordering::Relaxed));
        assert!(manager.status(root_task_id).is_none());
        assert!(manager.start_after_quiesce(root_task_id, None).is_ok());
    }

    #[tokio::test]
    async fn quiesce_runner_cleans_handle_when_old_join_panics() {
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            Arc::new(TaskStore::new()),
            Arc::new(SessionStore::new()),
            Arc::new(Vec::new),
            Arc::new(RecordingDispatcher {
                observed_role: Arc::new(Mutex::new(None)),
            }),
            Arc::new(EventBasedResultReceiver::new()),
        );
        let root_task_id = "task-runner-join-panic";
        let join_handle = tokio::spawn(async move {
            panic!("模拟旧 runner panic");
        });
        manager.runners.lock().expect("runners should lock").insert(
            root_task_id.to_string(),
            Arc::new(RunnerHandle {
                cancel: Arc::new(AtomicBool::new(false)),
                active: Arc::new(AtomicBool::new(true)),
                cycle_count: Arc::new(AtomicU64::new(0)),
                status: Arc::new(Mutex::new("running".to_string())),
                last_error: Arc::new(Mutex::new(None)),
                join_handle: Mutex::new(Some(join_handle)),
            }),
        );

        manager.quiesce_for_restart(root_task_id).await;

        assert!(manager.status(root_task_id).is_none());
    }

    #[tokio::test]
    async fn runner_start_rejects_archived_session() {
        let store = Arc::new(TaskStore::new());
        let root_task_id = "task-archived-session-runner";
        let mut root_task = task_with_status(root_task_id, TaskStatus::Pending);
        root_task.root_task_id = root_task.task_id.clone();
        store.insert_task(root_task);
        let session_store = Arc::new(SessionStore::new());
        let session_id = SessionId::new("session-archived-runner");
        session_store
            .create_session(session_id.clone(), "archived")
            .expect("session should create");
        session_store
            .archive_session(&session_id)
            .expect("session should archive");
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store,
            session_store,
            Arc::new(Vec::new),
            Arc::new(RecordingDispatcher {
                observed_role: Arc::new(Mutex::new(None)),
            }),
            Arc::new(EventBasedResultReceiver::new()),
        );

        assert!(matches!(
            manager.start(root_task_id, Some(session_id)).await,
            Err(RunnerStartError::SessionUnavailable)
        ));
        assert!(manager.status(root_task_id).is_none());
    }

    #[tokio::test]
    async fn runner_panic_fails_root_task_and_notifies_terminal_observer() {
        let store = Arc::new(TaskStore::new());
        let root_task_id = "task-runner-cycle-panic";
        let mut root_task = task_with_status(root_task_id, TaskStatus::Pending);
        root_task.root_task_id = root_task.task_id.clone();
        store.insert_task(root_task);
        let observed_status = Arc::new(Mutex::new(None));
        let observed_status_for_observer = observed_status.clone();
        let manager = RunnerManager::with_dispatcher_and_worker_catalog(
            store.clone(),
            Arc::new(SessionStore::new()),
            Arc::new(|| {
                vec![WorkerInfo {
                    worker_id: WorkerId::new("worker-runner-cycle-panic"),
                    role: "executor".to_string(),
                    supported_kinds: vec![TaskKind::LocalAgent],
                    parallelism_limit: None,
                    system_prompt_template: None,
                }]
            }),
            Arc::new(PanickingDispatcher),
            Arc::new(EventBasedResultReceiver::new()),
        )
        .with_terminal_observer(move |_task_id, _session_id, status| {
            *observed_status_for_observer
                .lock()
                .expect("observer status lock should not poison") = Some(status);
        });

        manager
            .start_after_quiesce(root_task_id, None)
            .expect("runner should start");
        for _ in 0..20 {
            if observed_status
                .lock()
                .expect("observer status lock should not poison")
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            observed_status
                .lock()
                .expect("observer status lock should not poison")
                .as_deref(),
            Some("error")
        );
        assert_eq!(
            store
                .get_task(&TaskId::new(root_task_id))
                .expect("root task should remain available")
                .status,
            TaskStatus::Failed
        );
        let failure_outputs = store
            .get_task(&TaskId::new(root_task_id))
            .expect("root task should remain available")
            .output_refs;
        assert_eq!(failure_outputs.len(), 1);
        assert!(failure_outputs[0].contains("任务 Runner 执行线程异常退出"));
        assert!(failure_outputs[0].contains("模拟 Runner 派发 panic"));
        assert_eq!(
            manager
                .status(root_task_id)
                .expect("runner status should remain inspectable")
                .status,
            "error"
        );
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

    #[test]
    fn session_git_context_round_trips_through_runtime_persistence() {
        let root = tempfile::tempdir().expect("state root");
        let persistence = || {
            Arc::new(RuntimeStatePersistence::new(
                root.path().join("sessions.json"),
                root.path().join("workspaces.json"),
                root.path().join("knowledge.json"),
            ))
        };
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        state.session_code_contexts.accept(
            "session-git-persist",
            "workspace-git-persist",
            vec![PathBuf::from("/repo")],
            &magi_git::GitObservation {
                repository_root: PathBuf::from("/repo"),
                git_common_dir: PathBuf::from("/repo/.git"),
                worktree_path: PathBuf::from("/repo"),
                worktree_git_dir: PathBuf::from("/repo/.git"),
                branch: Some("main".to_string()),
                head: Some("abc123".to_string()),
                upstream: Some("origin/main".to_string()),
                origin_url: Some("https://example.test/repo.git".to_string()),
                ahead: 0,
                behind: 0,
                dirty: magi_git::GitDirtySummary::default(),
            },
        );
        state
            .persist_session_git_contexts()
            .expect("persist Git context");

        let reloaded = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        let context = reloaded
            .session_code_contexts
            .get("session-git-persist")
            .expect("reloaded Git context");
        assert_eq!(context.git.desired_ref.as_deref(), Some("main"));
        assert_eq!(context.git.base_head.as_deref(), Some("abc123"));
        assert_eq!(context.context_revision, 1);
    }

    #[test]
    fn regular_session_turn_queue_round_trips_with_acknowledge_and_retry_state() {
        let root = tempfile::tempdir().expect("state root");
        let persistence = || {
            Arc::new(RuntimeStatePersistence::new(
                root.path().join("sessions.json"),
                root.path().join("workspaces.json"),
                root.path().join("knowledge.json"),
            ))
        };
        let session_store = Arc::new(SessionStore::default());
        let session_id = SessionId::new("session-turn-queue-persistence");
        let workspace_id = WorkspaceId::new("workspace-turn-queue-persistence");
        session_store
            .create_session(session_id.clone(), "queue persistence")
            .expect("session should create");
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        state
            .enqueue_regular_session_turn(queued_turn_fixture(
                &session_id,
                &workspace_id,
                "queue-first",
                11,
            ))
            .expect("first turn should persist");
        state
            .enqueue_regular_session_turn(queued_turn_fixture(
                &session_id,
                &workspace_id,
                "queue-second",
                12,
            ))
            .expect("second turn should persist");

        let restored = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        assert_eq!(restored.restore_regular_session_turn_queues().unwrap(), 2);
        assert_eq!(restored.queued_regular_session_turns(&session_id).len(), 2);
        assert!(
            restored
                .remove_regular_session_turn(&session_id, "queue-second")
                .expect("queued turn removal should persist")
        );
        assert_eq!(restored.queued_regular_session_turns(&session_id).len(), 1);
        restored
            .enqueue_regular_session_turn(queued_turn_fixture(
                &session_id,
                &workspace_id,
                "queue-second",
                12,
            ))
            .expect("removed turn should be re-enqueued for retry coverage");
        assert_eq!(
            restored
                .peek_next_regular_session_turn(&session_id)
                .expect("restored queue should have a head")
                .queue_id,
            "queue-first"
        );
        restored
            .acknowledge_regular_session_turn(&session_id, "queue-first")
            .expect("acknowledgement should persist");
        assert_eq!(
            restored
                .record_regular_session_turn_retry(&session_id, "queue-second")
                .expect("retry should persist"),
            1
        );

        let reloaded = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            session_store,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        assert_eq!(reloaded.restore_regular_session_turn_queues().unwrap(), 1);
        let remaining = reloaded
            .peek_next_regular_session_turn(&session_id)
            .expect("second turn should remain persisted");
        assert_eq!(remaining.queue_id, "queue-second");
        assert_eq!(remaining.retry_count, 1);
    }

    #[test]
    fn regular_session_turn_queue_restore_removes_archived_session_entries_from_disk() {
        let root = tempfile::tempdir().expect("state root");
        let persistence = || {
            Arc::new(RuntimeStatePersistence::new(
                root.path().join("sessions.json"),
                root.path().join("workspaces.json"),
                root.path().join("knowledge.json"),
            ))
        };
        let session_store = Arc::new(SessionStore::default());
        let session_id = SessionId::new("session-turn-queue-archived");
        let workspace_id = WorkspaceId::new("workspace-turn-queue-archived");
        session_store
            .create_session(session_id.clone(), "archived queue")
            .expect("session should create");
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        state
            .enqueue_regular_session_turn(queued_turn_fixture(
                &session_id,
                &workspace_id,
                "queue-archived",
                21,
            ))
            .expect("queued turn should persist");
        session_store
            .archive_session(&session_id)
            .expect("session should archive");

        let restored = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            session_store,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_runtime_persistence(persistence());
        assert_eq!(restored.restore_regular_session_turn_queues().unwrap(), 0);
        let persisted: Vec<QueuedRegularSessionTurn> = serde_json::from_slice(
            &fs::read(root.path().join("session-turn-queue.json"))
                .expect("cleaned queue file should exist"),
        )
        .expect("cleaned queue file should parse");
        assert!(persisted.is_empty());
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

    #[tokio::test]
    async fn bootstrap_workspace_session_prefers_stored_selection_over_list_order() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-bootstrap-stored-selection");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-bootstrap-stored-selection"),
            )
            .expect("workspace should register");
        let selected_session_id = SessionId::new("session-bootstrap-stored-selected");
        state
            .session_store
            .create_session_for_workspace(
                selected_session_id.clone(),
                "选择会话",
                Some(workspace_id.to_string()),
            )
            .expect("selected session should create");
        state.session_store.append_timeline_entry(
            selected_session_id.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "选择会话消息",
        );
        std::thread::sleep(Duration::from_millis(2));
        let newer_session_id = SessionId::new("session-bootstrap-stored-newer");
        state
            .session_store
            .create_session_for_workspace(
                newer_session_id.clone(),
                "更新会话",
                Some(workspace_id.to_string()),
            )
            .expect("newer session should create");
        state.session_store.append_timeline_entry(
            newer_session_id.clone(),
            magi_session_store::TimelineEntryKind::UserMessage,
            "更新会话消息",
        );
        state
            .session_store
            .select_current_session(&selected_session_id)
            .expect("older session should be selected");

        let bootstrap = state
            .bootstrap_dto_for_workspace_session(Some(workspace_id.as_str()), None)
            .expect("bootstrap should build");

        assert_eq!(
            bootstrap
                .current_session
                .as_ref()
                .map(|session| session.session_id.clone()),
            Some(selected_session_id)
        );
        assert_eq!(bootstrap.sessions[0].session_id, newer_session_id);
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
    fn product_capability_dependencies_do_not_require_workspace_or_session_context() {
        let provider = build_runtime_capability_dependency_provider(true);

        let entries = provider();
        let context_runtime = entries
            .iter()
            .find(|entry| entry.name == "context_runtime")
            .expect("context runtime dependency should be listed");
        let file_snapshot = entries
            .iter()
            .find(|entry| entry.name == "file_snapshot")
            .expect("file snapshot dependency should be listed");

        assert_eq!(context_runtime.status, "ready");
        assert_eq!(file_snapshot.status, "ready");
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
                "model_call_scope": "session_or_task",
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
        assert_eq!(public["tools"][0]["modelCallScope"], "session_or_task");
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
