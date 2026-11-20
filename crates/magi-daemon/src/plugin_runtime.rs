//! Daemon coordinator and crash supervisor for the shared DSH host and loading graph.
use crate::{
    plugin_dependencies::materialize_candidate_project_view,
    plugin_management::{
        PluginBundleManagementConfig, PluginManagementIncoming, PluginManagementService,
    },
    plugin_services::{PluginServicesBridge, PluginServicesIncoming},
    plugin_web_gateway::PluginWebGateway,
};
use magi_api::ApiError;
use magi_extension_host::{
    peer::Incoming,
    shared::{SharedHost, SharedLaunchConfig},
};
use magi_plugin_system::{
    LoadingActivation, PluginError, PluginManager,
    loading_runtime::{
        LoadingHostServices, LoadingPreparation, LoadingRuntime, LoadingSourceMonitor,
        LoadingSourceObservation, LoadingWebRoutes,
    },
};
use magi_process::ManagedProcessGroup;
use serde_json::json;
use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

#[derive(Clone)]
pub struct PluginRuntimeConfig {
    pub node: PathBuf,
    pub sdk_project: PathBuf,
    pub endpoint_dir: PathBuf,
    pub public_origin: String,
    pub bundle_management: Option<PluginBundleManagementConfig>,
}

#[derive(Clone, Copy, Debug)]
pub struct SharedHostRestartPolicy {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub crash_window: Duration,
    /// Stop automatic restart once crashes in `crash_window` reach `crash_limit` (default: 3 in 5 min).
    pub crash_limit: usize,
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    pub max_rss_bytes: u64,
}

impl Default for SharedHostRestartPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            crash_window: Duration::from_secs(300),
            crash_limit: 3,
            probe_interval: Duration::from_secs(5),
            probe_timeout: Duration::from_secs(5),
            max_rss_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SharedHostSupervisorState {
    Starting,
    Running {
        host_generation: u64,
    },
    Backoff {
        crashes_in_window: usize,
        reason: String,
        disabled_instances: Vec<String>,
    },
    /// Automatic restart stopped after reaching `crash_limit` in `crash_window` ("插件宿主已停止").
    StoppedByCrashLimit {
        crashes_in_window: usize,
        reason: String,
        activating_plugins: Vec<String>,
        active_plugins: Vec<String>,
        disabled_instances: Vec<String>,
    },
    Stopped,
}

enum SupervisorCommand {
    PluginExit {
        host_generation: u64,
    },
    ManualRestart {
        reply: oneshot::Sender<Result<u64, ApiError>>,
    },
    Stop {
        reply: oneshot::Sender<Result<(), ApiError>>,
    },
}

struct ActiveGeneration {
    host: Arc<SharedHost>,
    loading: Arc<LoadingRuntime>,
    routes: LoadingWebRoutes,
    sources: LoadingSourceMonitor,
    forwarder: JoinHandle<()>,
}

struct CrashWindow {
    policy: SharedHostRestartPolicy,
    crashes: VecDeque<Instant>,
}

impl CrashWindow {
    fn new(policy: SharedHostRestartPolicy) -> Self {
        Self {
            policy,
            crashes: VecDeque::new(),
        }
    }

    fn reset(&mut self) {
        self.crashes.clear();
    }

    fn record(&mut self, now: Instant) -> Result<Duration, usize> {
        while self
            .crashes
            .front()
            .is_some_and(|at| now.duration_since(*at) > self.policy.crash_window)
        {
            self.crashes.pop_front();
        }
        self.crashes.push_back(now);
        let count = self.crashes.len();
        if count >= self.policy.crash_limit {
            return Err(count);
        }
        let exponent = (count.saturating_sub(1) as u32).min(16);
        let delay = self
            .policy
            .initial_backoff
            .saturating_mul(1u32 << exponent)
            .min(self.policy.max_backoff);
        Ok(delay)
    }

    fn count(&self) -> usize {
        self.crashes.len()
    }
}

pub struct PluginRuntime {
    current_host: Arc<RwLock<Option<Arc<SharedHost>>>>,
    current_loading: Arc<RwLock<Option<Arc<LoadingRuntime>>>>,
    management: Option<Arc<PluginManagementService>>,
    services: Arc<PluginServicesBridge>,
    gateway: Arc<PluginWebGateway>,
    observations: watch::Receiver<LoadingSourceObservation>,
    state: watch::Receiver<SharedHostSupervisorState>,
    commands: mpsc::UnboundedSender<SupervisorCommand>,
    task: Option<JoinHandle<()>>,
}

impl PluginRuntime {
    pub async fn launch(
        owner: Arc<Mutex<PluginManager>>,
        group: &ManagedProcessGroup,
        config: PluginRuntimeConfig,
        initial: LoadingPreparation,
        incoming: Arc<dyn Incoming>,
    ) -> Result<Self, ApiError> {
        Self::launch_with_policy(
            owner,
            group,
            config,
            initial,
            incoming,
            SharedHostRestartPolicy::default(),
        )
        .await
    }

    pub async fn launch_with_policy(
        owner: Arc<Mutex<PluginManager>>,
        group: &ManagedProcessGroup,
        config: PluginRuntimeConfig,
        initial: LoadingPreparation,
        incoming: Arc<dyn Incoming>,
        policy: SharedHostRestartPolicy,
    ) -> Result<Self, ApiError> {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (observations_tx, observations_rx) =
            watch::channel(LoadingSourceObservation::Inspecting);
        let (state_tx, state_rx) = watch::channel(SharedHostSupervisorState::Starting);
        let current_host = Arc::new(RwLock::new(None));
        let current_loading = Arc::new(RwLock::new(None));
        let dsh_home = {
            let source_root = owner.lock().map_err(assembly_error)?.loading_source_root();
            source_root
                .parent()
                .unwrap_or(source_root.as_path())
                .join("dsh-home")
        };
        let services = PluginServicesBridge::new(owner.clone(), current_host.clone(), dsh_home)?;
        let management = match &config.bundle_management {
            Some(bm_config) => Some(PluginManagementService::new(
                owner.clone(),
                group.clone(),
                config.node.clone(),
                config.sdk_project.clone(),
                bm_config.clone(),
                current_host.clone(),
                current_loading.clone(),
            )?),
            None => None,
        };

        let launch_env = GenerationLaunchEnv {
            owner: &owner,
            group,
            config: &config,
            initial: &initial,
            incoming: &incoming,
            management: management.as_ref(),
            services: &services,
            commands_tx: &commands_tx,
            observations_tx: &observations_tx,
        };
        let first = launch_generation(&launch_env, None).await?;
        let first_generation = first.host.generation();
        *current_host.write().expect("host lock") = Some(first.host.clone());
        *current_loading.write().expect("loading lock") = Some(first.loading.clone());
        let gateway = Arc::new(PluginWebGateway::from_routes(
            first.routes,
            &config.public_origin,
        )?);
        let first_active = ActiveRunning {
            host: first.host,
            _loading: first.loading,
            sources: first.sources,
            forwarder: first.forwarder,
        };
        state_tx.send_replace(SharedHostSupervisorState::Running {
            host_generation: first_generation,
        });

        let supervisor_task = tokio::spawn(run_supervisor(SupervisorContext {
            owner,
            group: group.clone(),
            config,
            initial,
            incoming,
            management: management.clone(),
            services: services.clone(),
            policy,
            current_host: current_host.clone(),
            current_loading: current_loading.clone(),
            gateway: gateway.clone(),
            observations_tx,
            state_tx,
            commands_tx: commands_tx.clone(),
            commands_rx,
            initial_active: Some(first_active),
        }));

        Ok(Self {
            current_host,
            current_loading,
            management,
            services,
            gateway,
            observations: observations_rx,
            state: state_rx,
            commands: commands_tx,
            task: Some(supervisor_task),
        })
    }

    pub fn loading(&self) -> Arc<LoadingRuntime> {
        self.current_loading
            .read()
            .expect("plugin runtime loading lock")
            .clone()
            .expect("shared plugin host is not currently running")
    }

    pub fn try_loading(&self) -> Result<Arc<LoadingRuntime>, ApiError> {
        self.current_loading
            .read()
            .expect("plugin runtime loading lock")
            .clone()
            .ok_or_else(|| ApiError::Conflict("插件宿主当前未处于运行状态".into()))
    }

    pub fn gateway(&self) -> Arc<PluginWebGateway> {
        self.gateway.clone()
    }

    pub fn management(&self) -> Option<Arc<PluginManagementService>> {
        self.management.clone()
    }

    pub fn services(&self) -> Arc<PluginServicesBridge> {
        self.services.clone()
    }

    pub fn source_observations(&self) -> watch::Receiver<LoadingSourceObservation> {
        self.observations.clone()
    }

    pub fn supervisor_state(&self) -> SharedHostSupervisorState {
        self.state.borrow().clone()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<SharedHostSupervisorState> {
        self.state.clone()
    }

    pub async fn restart(&self) -> Result<u64, ApiError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::ManualRestart { reply: reply_tx })
            .map_err(|_| ApiError::Conflict("插件宿主监管器已关闭".into()))?;
        reply_rx
            .await
            .map_err(|_| ApiError::Conflict("插件宿主监管器已关闭".into()))?
    }

    pub async fn stop(mut self) -> Result<(), ApiError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .commands
            .send(SupervisorCommand::Stop { reply: reply_tx });
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        match reply_rx.await {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        if let Ok(guard) = self.current_host.read()
            && let Some(host) = guard.as_ref()
        {
            host.invalidate();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct ActiveRunning {
    host: Arc<SharedHost>,
    /// Retains the strong Arc backing LoadingHostServices's Weak<LoadingRuntime> while active.
    _loading: Arc<LoadingRuntime>,
    sources: LoadingSourceMonitor,
    forwarder: JoinHandle<()>,
}

impl ActiveRunning {
    async fn stop(self) -> Result<(), ApiError> {
        self.forwarder.abort();
        let (source_result, host_result) = tokio::join!(self.sources.stop(), self.host.stop());
        source_result.map_err(assembly_error)?;
        host_result.map_err(assembly_error)
    }

    async fn cleanup_after_crash(self) {
        self.forwarder.abort();
        let _ = tokio::join!(self.sources.stop(), self.host.stop());
    }
}

struct PendingCrashOffender {
    activation: LoadingActivation,
    reason: String,
}

struct GenerationLaunchEnv<'a> {
    owner: &'a Arc<Mutex<PluginManager>>,
    group: &'a ManagedProcessGroup,
    config: &'a PluginRuntimeConfig,
    initial: &'a LoadingPreparation,
    incoming: &'a Arc<dyn Incoming>,
    management: Option<&'a Arc<PluginManagementService>>,
    services: &'a Arc<PluginServicesBridge>,
    commands_tx: &'a mpsc::UnboundedSender<SupervisorCommand>,
    observations_tx: &'a watch::Sender<LoadingSourceObservation>,
}

async fn launch_generation(
    env: &GenerationLaunchEnv<'_>,
    offender: Option<&PendingCrashOffender>,
) -> Result<ActiveGeneration, ApiError> {
    let (host_generation, source_root, effective_preparation, first_intake) = {
        let mut guard = env.owner.lock().map_err(assembly_error)?;
        let generation = guard
            .allocate_loading_host_generation()
            .map_err(assembly_error)?;
        let current = guard.loading_configuration();
        let prep = current.map_or_else(|| env.initial.clone(), |c| c.preparation.clone());
        (
            generation,
            guard.loading_source_root(),
            prep,
            current.is_none(),
        )
    };
    materialize_candidate_project_view(&source_root, &effective_preparation)?;
    if let Some(mgmt) = env.management {
        mgmt.materialize_npmrc(&source_root)?;
    }
    let svc_incoming: Arc<dyn Incoming> =
        PluginServicesIncoming::new(env.services.clone(), host_generation, env.incoming.clone());
    let mgmt_incoming = env.management.map(|mgmt| {
        PluginManagementIncoming::new(mgmt.clone(), host_generation, svc_incoming.clone())
    });
    let delegate_incoming: Arc<dyn Incoming> = match &mgmt_incoming {
        Some(adapter) => adapter.clone(),
        None => svc_incoming,
    };
    let services = Arc::new(LoadingHostServices::new(delegate_incoming));
    let restart_sender = env.commands_tx.clone();
    services
        .bind_restart(Arc::new(move |generation| {
            restart_sender
                .send(SupervisorCommand::PluginExit {
                    host_generation: generation,
                })
                .map_err(|_| PluginError::Conflict("插件宿主监管器已关闭".into()))
        }))
        .map_err(assembly_error)?;
    let host = Arc::new(
        SharedHost::launch(
            env.group,
            SharedLaunchConfig {
                node: env.config.node.clone(),
                runtime_entry: env.config.sdk_project.join("runtime/entry.mjs"),
                endpoint_dir: env.config.endpoint_dir.clone(),
                host_generation,
                incoming: services.clone(),
            },
        )
        .await
        .map_err(assembly_error)?,
    );
    let materialized = Mutex::new(effective_preparation);
    let management = env.management.cloned();
    let loading = Arc::new(
        LoadingRuntime::new(env.owner.clone(), host.clone()).with_prepare_application(
            move |configuration| {
                let mut project = materialized
                    .lock()
                    .map_err(|_| PluginError::CorruptState("插件安装视图锁已损坏".into()))?;
                if *project != configuration.preparation {
                    materialize_candidate_project_view(
                        &configuration.source_root,
                        &configuration.preparation,
                    )
                    .map_err(|err| PluginError::Conflict(err.message().into()))?;
                    if let Some(management) = &management {
                        management
                            .materialize_npmrc(&configuration.source_root)
                            .map_err(|err| PluginError::Conflict(err.message().into()))?;
                    }
                    *project = configuration.preparation.clone();
                }
                Ok(())
            },
        ),
    );
    services.bind(&loading).map_err(assembly_error)?;
    if let Some(adapter) = &mgmt_incoming {
        adapter.bind(&host, &loading);
        host.open_plugin_manager().await.map_err(assembly_error)?;
    }
    // Plugins can declare required webServer / tools / web / settings / llm /
    // credentials / connection injections in their first apply. Open private
    // web routes and the host service closure before any graph runs.
    let routes = loading
        .open_web_routes(&env.config.public_origin)
        .await
        .map_err(assembly_error)?;
    host.open_services(
        env.services.dsh_home(),
        env.services.build_initial_host_config(),
    )
    .await
    .map_err(assembly_error)?;

    if let Some(offender) = offender {
        disable_crashing_instance(env.owner, &loading, env.incoming, host_generation, offender)
            .await;
    }

    let sources = loading
        .monitor_sources(env.initial.clone())
        .map_err(assembly_error)?;
    let mut rx = sources.observations();
    if first_intake {
        // The first install needs an admitted source revision. Watch the same
        // coordinator that will process later edits before publishing readiness.
        rx.wait_for(|state| !matches!(state, LoadingSourceObservation::Inspecting))
            .await
            .map_err(assembly_error)?;
    }
    env.observations_tx
        .send_replace(rx.borrow_and_update().clone());
    let tx = env.observations_tx.clone();
    let forwarder = tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            tx.send_replace(rx.borrow_and_update().clone());
        }
    });

    Ok(ActiveGeneration {
        host,
        loading,
        routes,
        sources,
        forwarder,
    })
}

async fn disable_crashing_instance(
    owner: &Arc<Mutex<PluginManager>>,
    loading: &LoadingRuntime,
    incoming: &Arc<dyn Incoming>,
    new_host_generation: u64,
    offender: &PendingCrashOffender,
) {
    let target = {
        let Ok(mut guard) = owner.lock() else {
            return;
        };
        if guard.pending_loading_edit().is_some() {
            let _ = guard.finish_loading_edit();
        }
        let Some(revision) = guard.loading_configuration().map(|c| c.revision) else {
            return;
        };
        let Some((tree_id, local_id)) =
            guard.loading_executable_location(&offender.activation.executable.instance_id)
        else {
            return;
        };
        (revision, tree_id, local_id)
    };
    let (revision, tree_id, local_id) = target;
    let operation = json!({
        "kind": "update",
        "treeId": tree_id,
        "id": local_id,
        "options": { "disabled": true },
    });
    match loading.submit_source_edit(revision, operation).await {
        Ok(_) => {
            incoming.notification(
                "host.diagnostic".into(),
                json!({
                    "hostGeneration": new_host_generation,
                    "pluginId": offender.activation.executable.plugin_id,
                    "instanceId": offender.activation.executable.instance_id,
                    "activationGeneration": offender.activation.activation_generation,
                    "type": "error",
                    "message": format!(
                        "插件 {} 在激活过程中导致共享宿主崩溃（{}），已自动停用",
                        offender.activation.executable.plugin_id, offender.reason
                    ),
                }),
            );
        }
        Err(error) => {
            incoming.notification(
                "host.diagnostic".into(),
                json!({
                    "hostGeneration": new_host_generation,
                    "pluginId": offender.activation.executable.plugin_id,
                    "instanceId": offender.activation.executable.instance_id,
                    "type": "error",
                    "message": format!(
                        "自动停用激活崩溃插件 {} 失败：{}",
                        offender.activation.executable.plugin_id, error
                    ),
                }),
            );
        }
    }
}

struct SupervisorContext {
    owner: Arc<Mutex<PluginManager>>,
    group: ManagedProcessGroup,
    config: PluginRuntimeConfig,
    initial: LoadingPreparation,
    incoming: Arc<dyn Incoming>,
    management: Option<Arc<PluginManagementService>>,
    services: Arc<PluginServicesBridge>,
    policy: SharedHostRestartPolicy,
    current_host: Arc<RwLock<Option<Arc<SharedHost>>>>,
    current_loading: Arc<RwLock<Option<Arc<LoadingRuntime>>>>,
    gateway: Arc<PluginWebGateway>,
    observations_tx: watch::Sender<LoadingSourceObservation>,
    state_tx: watch::Sender<SharedHostSupervisorState>,
    commands_tx: mpsc::UnboundedSender<SupervisorCommand>,
    commands_rx: mpsc::UnboundedReceiver<SupervisorCommand>,
    initial_active: Option<ActiveRunning>,
}

enum RunOutcome {
    Stopped(oneshot::Sender<Result<(), ApiError>>),
    PluginRestart,
    ManualRestart(oneshot::Sender<Result<u64, ApiError>>),
    Crashed { reason: String, process_exit: bool },
}

async fn monitor_active(
    active: &ActiveRunning,
    policy: &SharedHostRestartPolicy,
    commands_rx: &mut mpsc::UnboundedReceiver<SupervisorCommand>,
) -> RunOutcome {
    let host_closed = active.host.closed();
    tokio::pin!(host_closed);
    let mut probe = tokio::time::interval(policy.probe_interval);
    probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately; consume it so probes run after probe_interval.
    probe.tick().await;

    loop {
        tokio::select! {
            biased;
            command = commands_rx.recv() => match command {
                Some(SupervisorCommand::Stop { reply }) => return RunOutcome::Stopped(reply),
                Some(SupervisorCommand::ManualRestart { reply }) => return RunOutcome::ManualRestart(reply),
                Some(SupervisorCommand::PluginExit { host_generation }) => {
                    if host_generation == active.host.generation() {
                        return RunOutcome::PluginRestart;
                    }
                }
                None => return RunOutcome::Crashed { reason: "插件宿主监管命令通道已关闭".into(), process_exit: false },
            },
            _ = &mut host_closed => {
                let exit_code = active.host.reported_exit_code(Duration::from_millis(500)).await;
                return RunOutcome::Crashed {
                    reason: exit_code.map_or_else(|| "共享插件宿主异常退出".into(),
                        |code| format!("共享插件宿主异常退出（退出码 {code}）")),
                    process_exit: exit_code.is_some(),
                };
            }
            _ = probe.tick() => {
                match active.host.ping(policy.probe_timeout).await {
                    Ok(rss_bytes) if rss_bytes > policy.max_rss_bytes => {
                        active.host.invalidate();
                        return RunOutcome::Crashed {
                            reason: format!("共享插件宿主内存占用 {rss_bytes} 字节超过上限 {} 字节", policy.max_rss_bytes),
                            process_exit: false,
                        };
                    }
                    Ok(_) => {}
                    Err(_) => {
                        active.host.invalidate();
                        return RunOutcome::Crashed { reason: "共享插件宿主事件循环无响应".into(), process_exit: false };
                    }
                }
            }
        }
    }
}

fn select_crash_offender(
    activating: &[LoadingActivation],
    previously_healthy: &BTreeSet<(String, String)>,
    reason: &str,
) -> Option<PendingCrashOffender> {
    let mut candidates = activating.iter().filter(|item| {
        !previously_healthy.contains(&(
            item.executable.instance_id.clone(),
            item.executable.fingerprint.clone(),
        ))
    });
    let candidate = candidates.next()?;
    candidates.next().is_none().then(|| PendingCrashOffender {
        activation: candidate.clone(),
        reason: reason.to_owned(),
    })
}

async fn run_supervisor(mut ctx: SupervisorContext) {
    let mut active = ctx.initial_active.take();
    let mut crash_window = CrashWindow::new(ctx.policy);
    let mut disabled_instances: Vec<String> = Vec::new();
    let mut previously_healthy = BTreeSet::new();
    let mut pending_offender: Option<PendingCrashOffender> = None;
    let mut manual_launch: Option<oneshot::Sender<Result<u64, ApiError>>> = None;

    loop {
        if let Some(running) = active.take() {
            let generation = running.host.generation();
            let outcome = monitor_active(&running, &ctx.policy, &mut ctx.commands_rx).await;
            match outcome {
                RunOutcome::Stopped(reply) => {
                    *ctx.current_loading.write().expect("loading lock") = None;
                    *ctx.current_host.write().expect("host lock") = None;
                    ctx.services.on_host_stopped();
                    let result = running.stop().await;
                    if let Ok(mut guard) = ctx.owner.lock() {
                        guard.loading_host_ended(generation, "共享插件宿主已关闭".into());
                    }
                    ctx.state_tx
                        .send_replace(SharedHostSupervisorState::Stopped);
                    let _ = reply.send(result);
                    return;
                }
                RunOutcome::PluginRestart => {
                    *ctx.current_loading.write().expect("loading lock") = None;
                    *ctx.current_host.write().expect("host lock") = None;
                    ctx.services.on_host_stopped();
                    let _ = running.stop().await;
                    if let Ok(mut guard) = ctx.owner.lock() {
                        guard.loading_host_ended(
                            generation,
                            "插件通过 loader.exit 请求重启共享宿主".into(),
                        );
                    }
                    ctx.state_tx
                        .send_replace(SharedHostSupervisorState::Starting);
                    let launch_env = GenerationLaunchEnv {
                        owner: &ctx.owner,
                        group: &ctx.group,
                        config: &ctx.config,
                        initial: &ctx.initial,
                        incoming: &ctx.incoming,
                        management: ctx.management.as_ref(),
                        services: &ctx.services,
                        commands_tx: &ctx.commands_tx,
                        observations_tx: &ctx.observations_tx,
                    };
                    match launch_generation(&launch_env, None).await {
                        Ok(next) => {
                            active = Some(install_generation(&ctx, next, &ctx.state_tx));
                            continue;
                        }
                        Err(error) => {
                            match wait_after_crash(
                                &mut ctx,
                                &mut crash_window,
                                error.message().to_owned(),
                                Vec::new(),
                                Vec::new(),
                                &disabled_instances,
                                &mut pending_offender,
                            )
                            .await
                            {
                                WaitDisposition::Stop => return,
                                WaitDisposition::Launch(manual_reply) => {
                                    manual_launch = manual_reply;
                                }
                            }
                        }
                    }
                }
                RunOutcome::ManualRestart(reply) => {
                    *ctx.current_loading.write().expect("loading lock") = None;
                    *ctx.current_host.write().expect("host lock") = None;
                    ctx.services.on_host_stopped();
                    let _ = running.stop().await;
                    if let Ok(mut guard) = ctx.owner.lock() {
                        guard.loading_host_ended(generation, "手动重启共享插件宿主".into());
                    }
                    crash_window.reset();
                    pending_offender = None;
                    manual_launch = Some(reply);
                }
                RunOutcome::Crashed {
                    reason,
                    process_exit,
                } => {
                    *ctx.current_loading.write().expect("loading lock") = None;
                    *ctx.current_host.write().expect("host lock") = None;
                    ctx.services.on_host_stopped();
                    let (activating_instances, active_instances) = {
                        if let Ok(mut guard) = ctx.owner.lock() {
                            guard.loading_host_ended(generation, reason.clone());
                            guard
                                .loading_host_crash(generation)
                                .map(|summary| {
                                    (
                                        summary.activating_instances.clone(),
                                        summary.active_instances.clone(),
                                    )
                                })
                                .unwrap_or_default()
                        } else {
                            (Vec::new(), Vec::new())
                        }
                    };
                    running.cleanup_after_crash().await;

                    if process_exit
                        && let Some(offender) = select_crash_offender(
                            &activating_instances,
                            &previously_healthy,
                            &reason,
                        )
                    {
                        if !disabled_instances.contains(&offender.activation.executable.instance_id)
                        {
                            disabled_instances
                                .push(offender.activation.executable.instance_id.clone());
                        }
                        pending_offender = Some(offender);
                    }
                    previously_healthy.extend(active_instances.iter().map(|item| {
                        (
                            item.executable.instance_id.clone(),
                            item.executable.fingerprint.clone(),
                        )
                    }));
                    let activating_plugins = activating_instances
                        .iter()
                        .map(|a| a.executable.plugin_id.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    let active_plugins = active_instances
                        .iter()
                        .map(|a| a.executable.plugin_id.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();

                    match wait_after_crash(
                        &mut ctx,
                        &mut crash_window,
                        reason,
                        activating_plugins,
                        active_plugins,
                        &disabled_instances,
                        &mut pending_offender,
                    )
                    .await
                    {
                        WaitDisposition::Stop => return,
                        WaitDisposition::Launch(manual_reply) => {
                            manual_launch = manual_reply;
                        }
                    }
                }
            }
        }

        // Launch next generation after backoff or manual resume.
        loop {
            ctx.state_tx
                .send_replace(SharedHostSupervisorState::Starting);
            let offender = pending_offender.take();
            let launch_env = GenerationLaunchEnv {
                owner: &ctx.owner,
                group: &ctx.group,
                config: &ctx.config,
                initial: &ctx.initial,
                incoming: &ctx.incoming,
                management: ctx.management.as_ref(),
                services: &ctx.services,
                commands_tx: &ctx.commands_tx,
                observations_tx: &ctx.observations_tx,
            };
            match launch_generation(&launch_env, offender.as_ref()).await {
                Ok(next) => {
                    let generation = next.host.generation();
                    active = Some(install_generation(&ctx, next, &ctx.state_tx));
                    if let Some(reply) = manual_launch.take() {
                        let _ = reply.send(Ok(generation));
                    }
                    break;
                }
                Err(error) => {
                    let message = error.message().to_owned();
                    if let Some(reply) = manual_launch.take() {
                        let _ = reply.send(Err(error));
                    }
                    match wait_after_crash(
                        &mut ctx,
                        &mut crash_window,
                        message,
                        Vec::new(),
                        Vec::new(),
                        &disabled_instances,
                        &mut pending_offender,
                    )
                    .await
                    {
                        WaitDisposition::Stop => return,
                        WaitDisposition::Launch(manual_reply) => {
                            manual_launch = manual_reply;
                        }
                    }
                }
            }
        }
    }
}

fn install_generation(
    ctx: &SupervisorContext,
    next: ActiveGeneration,
    state_tx: &watch::Sender<SharedHostSupervisorState>,
) -> ActiveRunning {
    let generation = next.host.generation();
    ctx.gateway.retarget(next.routes);
    *ctx.current_host.write().expect("host lock") = Some(next.host.clone());
    *ctx.current_loading.write().expect("loading lock") = Some(next.loading.clone());
    state_tx.send_replace(SharedHostSupervisorState::Running {
        host_generation: generation,
    });
    ActiveRunning {
        host: next.host,
        _loading: next.loading,
        sources: next.sources,
        forwarder: next.forwarder,
    }
}

enum WaitDisposition {
    Stop,
    Launch(Option<oneshot::Sender<Result<u64, ApiError>>>),
}

async fn wait_after_crash(
    ctx: &mut SupervisorContext,
    crash_window: &mut CrashWindow,
    reason: String,
    activating_plugins: Vec<String>,
    active_plugins: Vec<String>,
    disabled_instances: &[String],
    pending_offender: &mut Option<PendingCrashOffender>,
) -> WaitDisposition {
    match crash_window.record(Instant::now()) {
        Ok(delay) => {
            ctx.state_tx
                .send_replace(SharedHostSupervisorState::Backoff {
                    crashes_in_window: crash_window.count(),
                    reason,
                    disabled_instances: disabled_instances.to_vec(),
                });
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    biased;
                    command = ctx.commands_rx.recv() => match command {
                        Some(SupervisorCommand::Stop { reply }) => {
                            ctx.state_tx.send_replace(SharedHostSupervisorState::Stopped);
                            let _ = reply.send(Ok(()));
                            return WaitDisposition::Stop;
                        }
                        Some(SupervisorCommand::ManualRestart { reply }) => {
                            crash_window.reset();
                            *pending_offender = None;
                            return WaitDisposition::Launch(Some(reply));
                        }
                        Some(SupervisorCommand::PluginExit { .. }) => {}
                        None => return WaitDisposition::Stop,
                    },
                    _ = &mut sleep => return WaitDisposition::Launch(None),
                }
            }
        }
        Err(crashes_in_window) => {
            ctx.state_tx
                .send_replace(SharedHostSupervisorState::StoppedByCrashLimit {
                    crashes_in_window,
                    reason,
                    activating_plugins,
                    active_plugins,
                    disabled_instances: disabled_instances.to_vec(),
                });
            loop {
                match ctx.commands_rx.recv().await {
                    Some(SupervisorCommand::Stop { reply }) => {
                        ctx.state_tx
                            .send_replace(SharedHostSupervisorState::Stopped);
                        let _ = reply.send(Ok(()));
                        return WaitDisposition::Stop;
                    }
                    Some(SupervisorCommand::ManualRestart { reply }) => {
                        crash_window.reset();
                        *pending_offender = None;
                        return WaitDisposition::Launch(Some(reply));
                    }
                    Some(SupervisorCommand::PluginExit { .. }) => {}
                    None => return WaitDisposition::Stop,
                }
            }
        }
    }
}

fn assembly_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::internal_assembly("启动或关闭共享插件宿主失败", error)
}
