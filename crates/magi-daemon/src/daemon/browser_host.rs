use std::{env, fmt, time::Duration};

use magi_api::{ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot};
use magi_browser_authority::{
    BrowserHostClient, BrowserHostClientError, BrowserHostCommand, BrowserHostEvent,
    BrowserHostHandshake, BrowserHostIncomingEvent, BrowserHostStatus, BrowserLeaseEndReason,
    BrowserSession, BrowserSessionLifecycle,
};
use magi_core::{BrowserTabId, EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::ActiveExecutionTurn;
use magi_tool_runtime::ToolRegistry;
use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::sync::{broadcast, watch};

const DESKTOP_CONTROL_SOCKET_ENV: &str = "MAGI_DESKTOP_CONTROL_SOCKET";
const DESKTOP_CONTROL_TOKEN_ENV: &str = "MAGI_DESKTOP_CONTROL_TOKEN";
const DESKTOP_EPOCH_ENV: &str = "MAGI_DESKTOP_EPOCH";
const DESKTOP_PARENT_PID_ENV: &str = "MAGI_DESKTOP_PARENT_PID";
const DESKTOP_CONNECT_ATTEMPTS: usize = 3;
const DESKTOP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DESKTOP_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(8);
const DESKTOP_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const DESKTOP_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
const DESKTOP_PARENT_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub(super) struct BrowserHostControllerLifecycle {
    shutdown_tx: watch::Sender<bool>,
}

impl BrowserHostControllerLifecycle {
    pub(super) fn new() -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self { shutdown_tx }
    }

    pub(super) fn request_shutdown(&self) {
        self.shutdown_tx.send_replace(true);
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DesktopBrowserConnectionConfig {
    socket_path: String,
    auth_token: String,
    desktop_epoch: String,
    parent_pid: u32,
    generation: u64,
}

impl DesktopBrowserConnectionConfig {
    fn from_env() -> Result<Self, DesktopBrowserConfigError> {
        Self::from_values(
            required_env(DESKTOP_CONTROL_SOCKET_ENV)?,
            required_env(DESKTOP_CONTROL_TOKEN_ENV)?,
            required_env(DESKTOP_EPOCH_ENV)?,
            required_env(DESKTOP_PARENT_PID_ENV)?,
        )
    }

    fn from_values(
        socket_path: String,
        auth_token: String,
        desktop_epoch: String,
        parent_pid: String,
    ) -> Result<Self, DesktopBrowserConfigError> {
        let socket_path = non_empty(DESKTOP_CONTROL_SOCKET_ENV, socket_path)?;
        let auth_token = non_empty(DESKTOP_CONTROL_TOKEN_ENV, auth_token)?;
        let desktop_epoch = non_empty(DESKTOP_EPOCH_ENV, desktop_epoch)?;
        let parent_pid = parent_pid.trim().parse::<u32>().map_err(|_| {
            DesktopBrowserConfigError::Invalid(DESKTOP_PARENT_PID_ENV, parent_pid.clone())
        })?;
        if parent_pid == 0 {
            return Err(DesktopBrowserConfigError::Invalid(
                DESKTOP_PARENT_PID_ENV,
                parent_pid.to_string(),
            ));
        }
        Ok(Self {
            socket_path,
            auth_token,
            desktop_epoch,
            parent_pid,
            generation: 0,
        })
    }

    fn from_runtime(
        config: BrowserHostConnectionConfig,
    ) -> Result<Self, DesktopBrowserConfigError> {
        let mut runtime = Self::from_values(
            config.socket_path,
            config.auth_token,
            config.desktop_epoch,
            config.parent_pid.to_string(),
        )?;
        runtime.generation = config.generation;
        Ok(runtime)
    }

    fn public(&self) -> BrowserHostConnectionConfig {
        BrowserHostConnectionConfig {
            socket_path: self.socket_path.clone(),
            auth_token: self.auth_token.clone(),
            desktop_epoch: self.desktop_epoch.clone(),
            parent_pid: self.parent_pid,
            generation: self.generation,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DesktopBrowserConfigError {
    Missing(&'static str),
    Invalid(&'static str, String),
}

impl DesktopBrowserConfigError {
    fn code(&self) -> &'static str {
        match self {
            Self::Missing(_) => "browser_desktop_context_missing",
            Self::Invalid(_, _) => "browser_desktop_context_invalid",
        }
    }
}

impl fmt::Display for DesktopBrowserConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(formatter, "缺少桌面浏览器环境变量 {name}"),
            Self::Invalid(name, value) => {
                write!(formatter, "桌面浏览器环境变量 {name} 无效: {value}")
            }
        }
    }
}

fn required_env(name: &'static str) -> Result<String, DesktopBrowserConfigError> {
    env::var(name).map_err(|_| DesktopBrowserConfigError::Missing(name))
}

fn non_empty(name: &'static str, value: String) -> Result<String, DesktopBrowserConfigError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(DesktopBrowserConfigError::Invalid(name, value));
    }
    Ok(trimmed.to_string())
}

pub(super) fn start_controller(state: &ApiState, lifecycle: &BrowserHostControllerLifecycle) {
    if let Err(error) = restore_browser_sessions(state) {
        tracing::error!(%error, "恢复浏览器逻辑会话失败");
        set_host_status(
            state,
            BrowserHostStatus::Failed,
            "failed",
            false,
            Some("browser_session_restore_failed".to_string()),
            None,
        );
        publish_host_status(state);
    }

    match DesktopBrowserConnectionConfig::from_env() {
        Ok(config) => state.set_browser_host_connection_config(Some(config.public())),
        Err(error) => {
            tracing::warn!(%error, "Electron Desktop 浏览器控制上下文不可用");
            state.set_browser_host_connection_config(None);
            set_host_status(
                state,
                BrowserHostStatus::Failed,
                "failed",
                false,
                Some(error.code().to_string()),
                None,
            );
            publish_host_status(state);
        }
    }

    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        set_host_status(
            state,
            BrowserHostStatus::Failed,
            "failed",
            false,
            Some("browser_controller_runtime_unavailable".to_string()),
            None,
        );
        publish_host_status(state);
        tracing::error!("当前线程没有 Tokio runtime，无法连接 Electron Desktop 浏览器");
        return;
    };

    let state = state.clone();
    handle.spawn(monitor_desktop_parent_process(
        state.clone(),
        lifecycle.subscribe(),
    ));
    let shutdown_rx = lifecycle.subscribe();
    handle.spawn(async move { run_desktop_browser_controller(state, shutdown_rx).await });
}

/// Electron owns the daemon in Desktop mode. The connection socket alone is
/// not sufficient as a lifecycle boundary: a hard Electron crash can leave a
/// daemon listening on the development port with stale browser leases. Keep a
/// process-level watchdog tied to the registered parent PID and terminate the
/// daemon after synchronously closing its execution resources.
async fn monitor_desktop_parent_process(state: ApiState, mut shutdown_rx: watch::Receiver<bool>) {
    let mut system = System::new();
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        let Some(config) = state.browser_host_connection_config() else {
            tokio::select! {
                _ = tokio::time::sleep(DESKTOP_PARENT_PROCESS_POLL_INTERVAL) => {}
                _ = shutdown_rx.changed() => return,
            }
            continue;
        };
        let parent_pid = Pid::from_u32(config.parent_pid);
        if !is_process_alive(&mut system, parent_pid) {
            let still_owned = state
                .browser_host_connection_config()
                .is_some_and(|current| {
                    current.parent_pid == config.parent_pid
                        && current.desktop_epoch == config.desktop_epoch
                        && current.generation == config.generation
                });
            if still_owned {
                tracing::error!(
                    parent_pid = config.parent_pid,
                    desktop_epoch = %config.desktop_epoch,
                    "Electron Desktop 已退出，daemon 正在收口浏览器运行资源"
                );
                interrupt_all_tasks_for_daemon_shutdown(&state);
                let cancelled_process_count = ToolRegistry::cancel_all_active_processes();
                let cancelled_managed_process_count = state.terminate_managed_processes();
                tracing::info!(
                    cancelled_process_count,
                    cancelled_managed_process_count,
                    "Desktop parent death 清理已完成，daemon 即将退出"
                );
                std::process::exit(0);
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(DESKTOP_PARENT_PROCESS_POLL_INTERVAL) => {}
            _ = shutdown_rx.changed() => return,
        }
    }
}

fn is_process_alive(system: &mut System, pid: Pid) -> bool {
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some()
}

async fn run_desktop_browser_controller(state: ApiState, mut shutdown_rx: watch::Receiver<bool>) {
    let mut reconnecting = false;
    let mut active_config: Option<DesktopBrowserConnectionConfig> = None;
    let mut config_rx = state.browser_host_connection_receiver();
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        let Some(config) = state
            .browser_host_connection_config()
            .and_then(|config| DesktopBrowserConnectionConfig::from_runtime(config).ok())
        else {
            active_config = None;
            reconnecting = false;
            set_waiting_status(&state);
            tokio::select! {
                changed = config_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                _ = shutdown_rx.changed() => return,
            }
            continue;
        };
        if active_config.as_ref() != Some(&config) {
            active_config = Some(config.clone());
            reconnecting = false;
        }
        let Some(connection) = connect_with_retries(
            &state,
            &config,
            reconnecting,
            &mut config_rx,
            &mut shutdown_rx,
        )
        .await
        else {
            continue;
        };
        let magi_browser_authority::BrowserHostConnection {
            client,
            handshake,
            mut events,
        } = connection;
        if state
            .browser_host_connection_config()
            .and_then(|current| DesktopBrowserConnectionConfig::from_runtime(current).ok())
            .as_ref()
            != Some(&config)
        {
            client.close().await;
            continue;
        }
        state
            .mutate_browser_authority(|authority| {
                authority.accept_desktop_epoch(handshake.desktop_epoch.clone(), UtcMillis::now());
                Ok(())
            })
            .expect("Desktop epoch should be accepted by BrowserAuthority");
        if let Err(error) = resume_interrupted_browser_sessions(&state) {
            tracing::error!(%error, "Electron Desktop 浏览器重连后恢复 Browser Session 失败");
            client.close().await;
            set_host_status(
                &state,
                BrowserHostStatus::Failed,
                "failed",
                true,
                Some("browser_session_restore_failed".to_string()),
                Some(&handshake),
            );
            publish_host_status(&state);
            reconnecting = true;
            continue;
        }
        let generation = state.set_browser_host_client(Some(client.clone()));
        set_host_status(
            &state,
            BrowserHostStatus::Ready,
            "running",
            true,
            None,
            Some(&handshake),
        );
        publish_host_status(&state);

        let disconnect = monitor_desktop_connection(
            &state,
            &client,
            &mut events,
            generation,
            &mut config_rx,
            &mut shutdown_rx,
        )
        .await;
        if state.browser_host_generation() == generation {
            state.set_browser_host_client(None);
        }
        client.close().await;
        tracing::warn!(reason = disconnect, "Electron Desktop 浏览器控制连接中断");

        if disconnect == "daemon_shutdown" {
            interrupt_all_tasks_for_daemon_shutdown(&state);
            set_host_status(
                &state,
                BrowserHostStatus::Stopped,
                "stopped",
                false,
                None,
                None,
            );
            publish_host_status(&state);
            return;
        }

        if disconnect == "configuration_changed" {
            // 清理连接也可能由 Worker 崩溃触发。配置通道变化意味着旧
            // Desktop 运行边界已经失效，必须先撤销所有 Agent Lease，
            // 再等待新的 Worker/Host 注册，避免旧 Lease 跨代残留。
            suspend_browser_sessions_for_host_disconnect(&state);
            reconnecting = false;
            continue;
        }

        // Desktop 页面由 Electron Main 持有。daemon 断线只撤销 Agent 控制边界，
        // 不关闭逻辑 Tab、不销毁 Surface，也不改变页面当前状态。
        suspend_browser_sessions_for_host_disconnect(&state);
        set_host_status(
            &state,
            BrowserHostStatus::Reconnecting,
            "reconnecting",
            false,
            Some("browser_desktop_disconnected".to_string()),
            None,
        );
        publish_host_status(&state);
        reconnecting = true;
    }
}

fn set_waiting_status(state: &ApiState) {
    let current = state.browser_host_status();
    if current.status == BrowserHostStatus::Stopped && current.last_error_code.is_none() {
        return;
    }
    if current.status == BrowserHostStatus::Failed
        && current.last_error_code.as_deref() == Some("browser_desktop_context_missing")
    {
        return;
    }
    set_host_status(
        state,
        BrowserHostStatus::Failed,
        "failed",
        false,
        Some("browser_desktop_context_missing".to_string()),
        None,
    );
    publish_host_status(state);
}

async fn connect_with_retries(
    state: &ApiState,
    config: &DesktopBrowserConnectionConfig,
    reconnecting: bool,
    config_rx: &mut watch::Receiver<Option<BrowserHostConnectionConfig>>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Option<magi_browser_authority::BrowserHostConnection> {
    let mut last_error = None;
    for attempt in 1..=DESKTOP_CONNECT_ATTEMPTS {
        let host_status = if reconnecting || attempt > 1 {
            "reconnecting"
        } else {
            "starting"
        };
        set_host_status(
            state,
            if reconnecting || attempt > 1 {
                BrowserHostStatus::Reconnecting
            } else {
                BrowserHostStatus::Starting
            },
            host_status,
            false,
            None,
            None,
        );
        publish_host_status(state);

        let connection = tokio::select! {
            result = BrowserHostClient::connect_desktop_socket(
                &config.socket_path,
                &config.auth_token,
                &config.desktop_epoch,
                config.parent_pid,
                DESKTOP_HANDSHAKE_TIMEOUT,
            ) => result,
            changed = config_rx.changed() => {
                if changed.is_ok() {
                    return None;
                }
                return None;
            }
            _ = shutdown_rx.changed() => return None,
        };
        match connection {
            Ok(connection) => return Some(connection),
            Err(error) => {
                let error_code = desktop_connection_error_code(&error);
                tracing::warn!(attempt, error = %error, error_code, "连接 Electron Desktop 浏览器失败");
                last_error = Some((error_code, error));
            }
        }

        if attempt < DESKTOP_CONNECT_ATTEMPTS {
            tokio::select! {
                _ = tokio::time::sleep(DESKTOP_RETRY_BASE_DELAY * attempt as u32) => {}
                changed = config_rx.changed() => {
                    if changed.is_ok() {
                        return None;
                    }
                    return None;
                }
                _ = shutdown_rx.changed() => return None,
            }
        }
    }

    let error_code = last_error
        .as_ref()
        .map_or("browser_not_ready", |(code, _)| *code);
    set_host_status(
        state,
        BrowserHostStatus::Reconnecting,
        "reconnecting",
        false,
        Some(error_code.to_string()),
        None,
    );
    publish_host_status(state);
    if let Some((_, error)) = last_error {
        tracing::error!(error = %error, "Electron Desktop 浏览器有限重连已耗尽");
    }
    tokio::select! {
        _ = tokio::time::sleep(DESKTOP_RECONNECT_BACKOFF) => {}
        changed = config_rx.changed() => {
            if changed.is_err() {
                return None;
            }
        }
        _ = shutdown_rx.changed() => return None,
    }
    None
}

fn desktop_connection_error_code(error: &BrowserHostClientError) -> &'static str {
    match error {
        BrowserHostClientError::ProtocolIncompatible { .. } => "browser_protocol_incompatible",
        BrowserHostClientError::DesktopEpochMismatch { .. }
        | BrowserHostClientError::DesktopProcessMismatch { .. } => {
            "browser_desktop_identity_mismatch"
        }
        BrowserHostClientError::InvalidConfiguration(_) => "browser_desktop_context_invalid",
        BrowserHostClientError::HandshakeTimeout => "browser_desktop_handshake_timeout",
        BrowserHostClientError::Connect(_)
        | BrowserHostClientError::Transport(_)
        | BrowserHostClientError::Json(_)
        | BrowserHostClientError::Disconnected
        | BrowserHostClientError::RequestTimeout(_)
        | BrowserHostClientError::RequestIndeterminate(_)
        | BrowserHostClientError::UnexpectedResponse(_)
        | BrowserHostClientError::UnexpectedBinaryPayload
        | BrowserHostClientError::BinarySizeMismatch { .. }
        | BrowserHostClientError::BinaryHashMismatch => "browser_not_ready",
    }
}

async fn monitor_desktop_connection(
    state: &ApiState,
    client: &BrowserHostClient,
    events: &mut broadcast::Receiver<BrowserHostIncomingEvent>,
    generation: u64,
    config_rx: &mut watch::Receiver<Option<BrowserHostConnectionConfig>>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> &'static str {
    loop {
        tokio::select! {
            event = tokio::time::timeout(DESKTOP_HEARTBEAT_TIMEOUT, events.recv()) => {
                match event {
                    Ok(Ok(event)) => {
                        tracing::debug!(generation, event = ?event.envelope.event, "收到 Electron Desktop 浏览器事件");
                        handle_host_event(state, event, generation)
                    }
                    Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                        tracing::warn!(skipped, "Electron Desktop 浏览器事件接收滞后");
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => {
                        tracing::warn!(generation, "Electron Desktop 浏览器事件流关闭");
                        return "event_stream_closed";
                    }
                    Err(_) => {
                        tracing::warn!(generation, "Electron Desktop 浏览器心跳超时");
                        client.close().await;
                        return "heartbeat_timeout";
                    }
                }
            }
            changed = config_rx.changed() => {
                if changed.is_ok() {
                    client.close().await;
                    return "configuration_changed";
                }
                client.close().await;
                return "configuration_channel_closed";
            }
            _ = shutdown_rx.changed() => {
                client.close().await;
                return "daemon_shutdown";
            }
        }
    }
}

fn restore_browser_sessions(state: &ApiState) -> Result<(), String> {
    let sessions = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .sessions
        .into_iter()
        .filter(|session| session.lifecycle == BrowserSessionLifecycle::Recovering)
        .collect::<Vec<_>>();

    for session in sessions {
        let restored = state
            .mutate_browser_authority(|authority| {
                authority.transition_session(
                    &session.browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis::now(),
                )
            })
            .map_err(|error| format!("恢复 Browser Session 权威状态失败: {error:?}"))?;

        // Tab 保持 Suspended，直到右侧面板首次激活时由 Electron 物化真实 Surface。
        state.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-browser-session-recovered-{}-{}",
                    restored.browser_session_id,
                    UtcMillis::now().0
                )),
                "browser.session.recovered",
                serde_json::json!({
                    "browser_session_id": restored.browser_session_id,
                    "runtime_epoch": restored.runtime_epoch,
                    "materialized": false,
                }),
            )
            .with_context(EventContext {
                workspace_id: restored.workspace_id,
                session_id: Some(restored.session_id),
                ..EventContext::default()
            }),
        );
    }
    Ok(())
}

/// Browser Host 重连后，恢复本次连接中被标记为 Interrupted 的逻辑会话。
///
/// 断线时只撤销控制租约并把会话标记为 Interrupted，避免把真实 Chromium
/// 页面当作已关闭。连接恢复后必须先经过 Recovering 再回到 Ready，才能让
/// 后续的 Tab 激活、工具调用与当前 Host 代次重新建立一致的运行边界。
fn resume_interrupted_browser_sessions(state: &ApiState) -> Result<(), String> {
    let resumed = state
        .mutate_browser_authority(|authority| {
            Ok(authority.resume_interrupted_sessions(UtcMillis::now()))
        })
        .map_err(|error| format!("投入浏览器会话恢复失败: {error:?}"))?;
    if !resumed.is_empty() {
        restore_browser_sessions(state)?;
    }
    Ok(())
}

#[derive(Clone)]
struct BrowserTabContext {
    workspace_id: Option<WorkspaceId>,
    session_id: SessionId,
}

fn handle_host_event(state: &ApiState, event: BrowserHostIncomingEvent, generation: u64) {
    if state.browser_host_generation() != generation {
        tracing::debug!(generation, "忽略旧 Electron Desktop 浏览器代次事件");
        return;
    }

    match event.envelope.event {
        BrowserHostEvent::PrimarySurfaceChanged { binding } => {
            let context = browser_tab_context(state, &binding.tab_id);
            let result = state.mutate_browser_authority(|authority| {
                authority.accept_primary_surface(binding.clone(), UtcMillis::now())
            });
            match result {
                Ok((true, tab, revoked)) => {
                    // Host 重连时 Control Server 只会重放当前 Primary Surface；
                    // 之前已经完成加载的页面不会再次发送 page_updated。标记
                    // 投影必须在 Primary 确认后立即由 Authority 重新下发，
                    // 否则服务或 Electron 重启后持久化标记会永久消失。
                    schedule_browser_annotation_sync(state, &binding.tab_id);
                    publish_tab_event(
                        state,
                        "browser.tab.status_changed",
                        context.clone(),
                        serde_json::json!({
                            "tab_id": tab.tab_id,
                            "lifecycle": tab.lifecycle,
                            "surface_id": binding.surface_id,
                            "surface_revision": binding.surface_revision,
                            "navigation_revision": tab.navigation_revision,
                        }),
                    );
                    publish_tab_event(
                        state,
                        "browser.surface.primary_changed",
                        context,
                        serde_json::json!({ "binding": binding }),
                    );
                    if !revoked.is_empty() {
                        publish_tab_event(
                            state,
                            "browser.control.revoked",
                            browser_tab_context(state, &binding.tab_id),
                            serde_json::json!({
                                "tab_id": binding.tab_id,
                                "reason": "primary_surface_changed",
                                "revoked_lease_count": revoked.len(),
                            }),
                        );
                    }
                }
                Ok((false, _, _)) => tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    surface_revision = binding.surface_revision,
                    "忽略旧 Browser Surface 主页面事件"
                ),
                Err(error) => tracing::debug!(
                    tab_id = %binding.tab_id,
                    ?error,
                    "忽略无法收敛的 Browser Surface 主页面状态"
                ),
            }
        }
        BrowserHostEvent::PrimarySurfaceClosed { binding } => {
            let context = browser_tab_context(state, &binding.tab_id);
            match state.mutate_browser_authority(|authority| {
                authority.clear_primary_surface(&binding, UtcMillis::now())
            }) {
                Ok((true, tab, revoked)) => {
                    publish_tab_event(
                        state,
                        "browser.tab.status_changed",
                        context.clone(),
                        serde_json::json!({
                            "tab_id": tab.tab_id,
                            "lifecycle": tab.lifecycle,
                            "surface_id": null,
                            "surface_revision": binding.surface_revision,
                            "navigation_revision": tab.navigation_revision,
                        }),
                    );
                    publish_tab_event(
                        state,
                        "browser.surface.primary_closed",
                        context,
                        serde_json::json!({ "binding": binding }),
                    );
                    if !revoked.is_empty() {
                        publish_tab_event(
                            state,
                            "browser.control.revoked",
                            browser_tab_context(state, &binding.tab_id),
                            serde_json::json!({
                                "tab_id": binding.tab_id,
                                "reason": "primary_surface_closed",
                                "revoked_lease_count": revoked.len(),
                            }),
                        );
                    }
                }
                Ok((false, _, _)) => tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    "忽略非当前 Primary Surface 的关闭事件"
                ),
                Err(error) => tracing::warn!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    ?error,
                    "处理 Primary Surface 关闭事件失败"
                ),
            }
        }
        BrowserHostEvent::UserTakeover { binding } => {
            revoke_surface_control(
                state,
                &binding,
                BrowserLeaseEndReason::UserTakeover,
                "user_takeover",
            );
        }
        BrowserHostEvent::ControlRevoked { binding, reason } => {
            let lease_reason = control_revocation_reason(&reason);
            revoke_surface_control(state, &binding, lease_reason, &reason);
        }
        BrowserHostEvent::PageUpdated {
            binding,
            page_state,
        } => {
            if page_state.tab_id != binding.tab_id
                || page_state.navigation_revision != binding.navigation_revision
            {
                tracing::debug!(
                    tab_id = %binding.tab_id,
                    "忽略页面状态与 Surface binding 不一致的事件"
                );
                return;
            }
            let context = browser_tab_context(state, &binding.tab_id);
            match state.mutate_browser_authority(|authority| {
                let (accepted, revoked) =
                    authority.accept_page_binding(&binding, UtcMillis::now())?;
                if !accepted {
                    return Ok((None, revoked));
                }
                authority
                    .apply_host_page_state(
                        &page_state.tab_id,
                        page_state.navigation_revision,
                        page_state.url.clone(),
                        page_state.origin.clone(),
                        page_state.title.clone(),
                        UtcMillis::now(),
                    )
                    .map(|tab| (Some(tab), revoked))
            }) {
                Ok((Some(_), revoked)) => {
                    schedule_browser_annotation_sync(state, &binding.tab_id);
                    publish_tab_event(
                        state,
                        "browser.tab.updated",
                        context.clone(),
                        serde_json::json!({
                            "tab_id": page_state.tab_id,
                            "url": page_state.url,
                            "title": page_state.title,
                            "navigation_revision": page_state.navigation_revision,
                            "binding": binding,
                        }),
                    );
                    if !revoked.is_empty() {
                        publish_tab_event(
                            state,
                            "browser.control.revoked",
                            context,
                            serde_json::json!({
                                "tab_id": binding.tab_id,
                                "binding": binding,
                                "reason": "navigation_changed",
                                "revoked_lease_count": revoked.len(),
                            }),
                        );
                    }
                }
                Ok((None, _)) => tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    surface_revision = binding.surface_revision,
                    "忽略旧 Browser Surface 页面事件"
                ),
                Err(error) => tracing::debug!(
                    tab_id = %binding.tab_id,
                    ?error,
                    "忽略无法应用的旧页面状态"
                ),
            }
        }
        BrowserHostEvent::PageFailed { binding, reason } => {
            if !is_current_or_advanced_primary_binding(state, &binding) {
                tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    navigation_revision = binding.navigation_revision,
                    "忽略已失效 Browser Surface 的页面失败事件"
                );
                return;
            }
            publish_tab_event(
                state,
                "browser.automation.page_failed",
                browser_tab_context(state, &binding.tab_id),
                serde_json::json!({
                    "tab_id": binding.tab_id,
                    "binding": binding,
                    "reason": magi_core::public_runtime_excerpt(&reason, 1024),
                }),
            );
        }
        BrowserHostEvent::LoadingChanged { binding, loading } => {
            if !is_current_or_advanced_primary_binding(state, &binding) {
                tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    navigation_revision = binding.navigation_revision,
                    "忽略已失效 Browser Surface 的加载事件"
                );
                return;
            }
            publish_tab_event(
                state,
                "browser.tab.loading_changed",
                browser_tab_context(state, &binding.tab_id),
                serde_json::json!({
                    "tab_id": binding.tab_id,
                    "binding": binding,
                    "loading": loading,
                }),
            );
        }
        BrowserHostEvent::PageCrashed {
            binding,
            diagnostic,
        } => {
            if !is_current_primary_binding(state, &binding) {
                tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    surface_revision = binding.surface_revision,
                    "忽略已失效 Browser Surface 的崩溃事件"
                );
                return;
            }
            let context = browser_tab_context(state, &binding.tab_id);
            let lifecycle = state.mutate_browser_authority(|authority| {
                authority.transition_tab(
                    &binding.tab_id,
                    magi_browser_authority::BrowserTabLifecycle::Crashed,
                    UtcMillis::now(),
                )
            });
            match lifecycle {
                Ok(tab) => {
                    publish_tab_event(
                        state,
                        "browser.tab.status_changed",
                        context.clone(),
                        serde_json::json!({
                            "tab_id": tab.tab_id,
                            "lifecycle": tab.lifecycle,
                            "surface_id": binding.surface_id,
                            "surface_revision": binding.surface_revision,
                        }),
                    );
                    publish_tab_event(
                        state,
                        "browser.automation.page_crashed",
                        context,
                        serde_json::json!({
                            "tab_id": binding.tab_id,
                            "binding": binding,
                            "diagnostic": diagnostic.map(|value| magi_core::public_runtime_excerpt(&value, 1024)),
                        }),
                    );
                }
                Err(error) => tracing::debug!(
                    tab_id = %binding.tab_id,
                    ?error,
                    "忽略无法收敛的 Browser Surface 崩溃状态"
                ),
            }
        }
        BrowserHostEvent::Dialog {
            tab_id,
            dialog_id,
            dialog_type,
            message,
        } => {
            publish_tab_event(
                state,
                "browser.dialog.opened",
                browser_tab_context(state, &tab_id),
                serde_json::json!({
                    "tab_id": tab_id,
                    "dialog_id": dialog_id,
                    "dialog_type": dialog_type,
                    "message": magi_core::public_runtime_excerpt(&message, 1024),
                }),
            );
        }
        BrowserHostEvent::Download {
            tab_id,
            download_id,
            suggested_filename,
            state: download_state,
            received_bytes,
            total_bytes,
            byte_length,
            error,
        } => {
            publish_tab_event(
                state,
                "browser.download.updated",
                browser_tab_context(state, &tab_id),
                serde_json::json!({
                    "tab_id": tab_id,
                    "download_id": download_id,
                    "suggested_filename": suggested_filename,
                    "state": download_state,
                    "received_bytes": received_bytes,
                    "total_bytes": total_bytes,
                    "byte_length": byte_length,
                    "error": error.map(|value| magi_core::public_runtime_excerpt(&value, 1024)),
                }),
            );
        }
        BrowserHostEvent::PopupBlocked {
            binding,
            url,
            reason,
        } => {
            if !is_current_or_advanced_primary_binding(state, &binding) {
                tracing::debug!(
                    tab_id = %binding.tab_id,
                    surface_id = %binding.surface_id,
                    navigation_revision = binding.navigation_revision,
                    "忽略已失效 Browser Surface 的弹窗事件"
                );
                return;
            }
            publish_tab_event(
                state,
                "browser.popup.blocked",
                browser_tab_context(state, &binding.tab_id),
                serde_json::json!({
                    "tab_id": binding.tab_id,
                    "binding": binding,
                    "url": url,
                    "reason": reason,
                }),
            );
        }
        BrowserHostEvent::NodeSelection(selection) => {
            if !is_current_node_selection(state, &selection) {
                tracing::debug!(
                    tab_id = %selection.tab_id,
                    surface_id = %selection.surface_id,
                    navigation_revision = selection.navigation_revision,
                    "忽略已失效 Browser Surface 的节点选择事件"
                );
                return;
            }
            let payload = serde_json::to_value(&selection)
                .expect("BrowserNodeSelection must serialize as a structured event");
            publish_tab_event(
                state,
                "browser.node.selected",
                browser_tab_context(state, &selection.tab_id),
                magi_conversation_runtime::context_reference::sanitize_browser_node_selection(
                    &payload,
                ),
            );
        }
        BrowserHostEvent::Ready(_)
        | BrowserHostEvent::Console { .. }
        | BrowserHostEvent::AgentCursor(_)
        | BrowserHostEvent::BinaryPayloadReady(_)
        | BrowserHostEvent::Heartbeat { .. } => {}
    }
}

fn control_revocation_reason(reason: &str) -> BrowserLeaseEndReason {
    match reason {
        "user_takeover" => BrowserLeaseEndReason::UserTakeover,
        "task_finished" => BrowserLeaseEndReason::TaskFinished,
        "goal_paused" => BrowserLeaseEndReason::GoalPaused,
        "runtime_unavailable" | "worker_failed" => BrowserLeaseEndReason::RuntimeUnavailable,
        _ => BrowserLeaseEndReason::TurnStopped,
    }
}

fn revoke_surface_control(
    state: &ApiState,
    binding: &magi_browser_authority::BrowserSurfaceBinding,
    lease_reason: BrowserLeaseEndReason,
    reason: &str,
) {
    if !is_current_primary_binding(state, binding) {
        tracing::debug!(
            tab_id = %binding.tab_id,
            surface_id = %binding.surface_id,
            surface_revision = binding.surface_revision,
            "忽略已失效 Browser Surface 的控制权撤销事件"
        );
        return;
    }
    let context = browser_tab_context(state, &binding.tab_id);
    let Some(context) = context.clone() else {
        tracing::debug!(tab_id = %binding.tab_id, "忽略未知 Browser Surface 的控制权撤销事件");
        return;
    };
    let revoked =
        state.cancel_browser_surface_control(&binding.tab_id, &binding.surface_id, lease_reason);
    publish_tab_event(
        state,
        "browser.control.revoked",
        Some(context),
        serde_json::json!({
            "tab_id": binding.tab_id,
            "binding": binding,
            "reason": reason,
            "revoked_lease_count": revoked.len(),
        }),
    );
}

fn browser_tab_context(state: &ApiState, tab_id: &BrowserTabId) -> Option<BrowserTabContext> {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let tab = authority.tab(tab_id)?;
    let session = authority.session(&tab.browser_session_id)?;
    Some(BrowserTabContext {
        workspace_id: session.workspace_id.clone(),
        session_id: session.session_id.clone(),
    })
}

fn is_current_primary_binding(
    state: &ApiState,
    binding: &magi_browser_authority::BrowserSurfaceBinding,
) -> bool {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    authority.is_current_surface_binding(binding)
}

fn is_current_or_advanced_primary_binding(
    state: &ApiState,
    binding: &magi_browser_authority::BrowserSurfaceBinding,
) -> bool {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    authority
        .primary_surface(&binding.tab_id)
        .is_some_and(|current| {
            current.desktop_epoch == binding.desktop_epoch
                && current.window_id == binding.window_id
                && current.surface_id == binding.surface_id
                && current.surface_revision == binding.surface_revision
                && current.tab_id == binding.tab_id
                && current.web_contents_id == binding.web_contents_id
                && current.target_id == binding.target_id
                && current.browser_context_id == binding.browser_context_id
                && binding.navigation_revision >= current.navigation_revision
        })
}

fn is_current_node_selection(
    state: &ApiState,
    selection: &magi_browser_authority::BrowserNodeSelection,
) -> bool {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    authority
        .primary_surface(&selection.tab_id)
        .is_some_and(|binding| {
            binding.surface_id == selection.surface_id
                && authority
                    .tab(&selection.tab_id)
                    .is_some_and(|tab| tab.browser_session_id == selection.browser_session_id)
                && binding.navigation_revision == selection.navigation_revision
        })
}

fn schedule_browser_annotation_sync(state: &ApiState, tab_id: &BrowserTabId) {
    let Some(client) = state.browser_host_client() else {
        return;
    };
    let annotations = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotations_for_tab(tab_id)
        .into_iter()
        .filter_map(|annotation| serde_json::to_value(annotation).ok())
        .collect::<Vec<_>>();
    let tab_id = tab_id.clone();
    tokio::spawn(async move {
        if let Err(error) = client
            .request(BrowserHostCommand::SetAnnotations {
                tab_id: tab_id.clone(),
                annotations,
            })
            .await
        {
            tracing::debug!(%tab_id, ?error, "页面状态更新后同步浏览器标记失败");
        }
    });
}

fn publish_tab_event(
    state: &ApiState,
    event_type: &str,
    context: Option<BrowserTabContext>,
    payload: serde_json::Value,
) {
    let Some(context) = context else {
        return;
    };
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-{}-{}",
                event_type.replace('.', "-"),
                UtcMillis::now().0
            )),
            event_type,
            payload,
        )
        .with_context(EventContext {
            workspace_id: context.workspace_id,
            session_id: Some(context.session_id),
            ..EventContext::default()
        }),
    );
}

/// 仅收口 Browser Host 资源。Host 控制连接属于浏览器运行时，不是普通会话
/// Turn 的执行边界；断线时必须释放 Browser Lease，但不能中断对话、Goal 或
/// 其他工具执行。
fn suspend_browser_sessions_for_host_disconnect(state: &ApiState) -> Vec<BrowserSession> {
    let sessions = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .sessions
        .into_iter()
        .filter(|session| session.lifecycle.is_recoverable())
        .collect::<Vec<_>>();

    for browser_session in &sessions {
        let interrupted_session = match state.mutate_browser_authority(|authority| {
            authority.transition_session(
                &browser_session.browser_session_id,
                BrowserSessionLifecycle::Interrupted,
                UtcMillis::now(),
            )
        }) {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    browser_session_id = %browser_session.browser_session_id,
                    ?error,
                    "Desktop 浏览器失效后收敛 Browser Session 状态失败"
                );
                continue;
            }
        };
        state.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-browser-session-interrupted-{}-{}",
                    interrupted_session.browser_session_id,
                    UtcMillis::now().0
                )),
                "browser.session.status_changed",
                serde_json::json!({
                    "browser_session_id": interrupted_session.browser_session_id,
                    "session_id": interrupted_session.session_id,
                    "workspace_id": interrupted_session.workspace_id,
                    "lifecycle": interrupted_session.lifecycle,
                    "reason": "browser_host_unavailable",
                    "revision": interrupted_session.revision,
                }),
            )
            .with_context(EventContext {
                workspace_id: interrupted_session.workspace_id.clone(),
                session_id: Some(interrupted_session.session_id.clone()),
                ..EventContext::default()
            }),
        );
        state.revoke_browser_execution_resources(
            Some(&browser_session.session_id),
            browser_session.workspace_id.as_ref(),
            None,
            BrowserLeaseEndReason::RuntimeUnavailable,
        );
    }
    sessions
}

/// daemon 真正关闭时才允许全局中断执行。该路径由 daemon_shutdown 或父进程
/// 已退出触发，和 Browser Host 的普通断线严格分离。
fn interrupt_all_tasks_for_daemon_shutdown(state: &ApiState) {
    let sessions = suspend_browser_sessions_for_host_disconnect(state);
    for browser_session in sessions {
        let Some(current_turn) = state
            .session_store
            .runtime_sidecar(&browser_session.session_id)
            .and_then(|sidecar| sidecar.current_turn)
        else {
            continue;
        };
        if matches!(
            current_turn.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            continue;
        }
        let owned_goal = state
            .session_store
            .active_goal_for_execution_owner(&browser_session.session_id, &current_turn.turn_id)
            .is_some();
        let request_id = request_id_for_turn(&current_turn);
        if let Some(chain) = state
            .session_store
            .active_execution_chain(&browser_session.session_id)
            && let Some(manager) = state.runner_manager()
            && let Err(error) = manager.kill_tree(chain.root_task_id.as_str())
        {
            tracing::warn!(
                session_id = %browser_session.session_id,
                task_id = %chain.root_task_id,
                ?error,
                "daemon 关闭时终止浏览器执行树失败"
            );
        }
        match state
            .turn_event_sink()
            .interrupt_turn_by_daemon_restart(&browser_session.session_id)
        {
            Ok(Some(_)) => {
                state
                    .turn_coordinator()
                    .close_session_turn_input(&browser_session.session_id, &current_turn.turn_id);
                if owned_goal {
                    match state
                        .session_store
                        .pause_active_goal_for_diversion(&browser_session.session_id)
                    {
                        Ok(Some((_goal, Some(plan)))) => magi_plan::publish_plan_event(
                            &state.event_bus,
                            magi_plan::plan_event_type(&plan),
                            &plan,
                            browser_session.workspace_id.as_ref(),
                            None,
                            None,
                        ),
                        Ok(Some((_, None))) | Ok(None) => {}
                        Err(error) => tracing::warn!(
                            session_id = %browser_session.session_id,
                            ?error,
                            "daemon 关闭时暂停 Goal 与计划失败"
                        ),
                    }
                }
                if let Err(error) = state.persist_session_state_checkpoint("daemon_shutdown") {
                    tracing::warn!(
                        session_id = %browser_session.session_id,
                        ?error,
                        "daemon 关闭时 session 状态持久化失败"
                    );
                }
                state.event_bus.publish(
                    EventEnvelope::domain(
                        EventId::new(format!(
                            "event-browser-turn-interrupted-{}-{}",
                            browser_session.session_id,
                            UtcMillis::now().0
                        )),
                        "session.turn.interrupted",
                        serde_json::json!({
                            "session_id": browser_session.session_id,
                            "request_id": request_id,
                            "workspace_id": browser_session.workspace_id,
                            "turn_id": current_turn.turn_id,
                            "interrupted": true,
                            "reason": "daemon_shutdown",
                        }),
                    )
                    .with_context(EventContext {
                        workspace_id: browser_session.workspace_id.clone(),
                        session_id: Some(browser_session.session_id.clone()),
                        ..EventContext::default()
                    }),
                );
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                session_id = %browser_session.session_id,
                ?error,
                "daemon 关闭时中断活动 Turn 失败"
            ),
        }
    }
}

fn request_id_for_turn(turn: &ActiveExecutionTurn) -> String {
    turn.items
        .iter()
        .find_map(|item| {
            item.request_id.clone().or_else(|| {
                item.metadata
                    .get("request_id")
                    .or_else(|| item.metadata.get("requestId"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        })
        .filter(|request_id| !request_id.trim().is_empty())
        .unwrap_or_else(|| turn.turn_id.clone())
}

fn set_host_status(
    state: &ApiState,
    status: BrowserHostStatus,
    _host_status: &str,
    protocol_compatible: bool,
    error_code: Option<String>,
    _handshake: Option<&BrowserHostHandshake>,
) {
    let previous = state.browser_host_status();
    state.set_browser_host_status(BrowserHostStatusSnapshot {
        revision: 0,
        in_app_browser_enabled: previous.in_app_browser_enabled,
        browser_use_enabled: previous.browser_use_enabled,
        status,
        protocol_compatible,
        last_error_code: error_code,
    });
}

fn publish_host_status(state: &ApiState) {
    let runtime = state.browser_host_status();
    state.event_bus.publish(EventEnvelope::system(
        EventId::new(format!(
            "event-browser-host-status-{:?}-{}",
            runtime.status,
            UtcMillis::now().0
        )),
        "browser.host.status_changed",
        serde_json::json!({
            "host_status": runtime.status,
            "protocol_compatible": runtime.protocol_compatible,
            "error_code": runtime.last_error_code,
            "revision": runtime.revision,
        }),
    ));
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use magi_api::ApiState;
    use magi_browser_authority::{
        AcquireBrowserLease, BrowserHostEventEnvelope, BrowserHostPageState,
        BrowserHostProtocolVersion, BrowserLeaseLifecycle, BrowserProfile, BrowserProfileKind,
        BrowserSessionLifecycle, BrowserTabLifecycle, CreateBrowserSession, CreateBrowserTab,
    };
    use magi_core::{
        BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId, ExecutionOwnership,
        SessionId, TaskId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::{ActiveExecutionTurn, SessionStore};
    use magi_workspace::WorkspaceStore;

    use super::*;

    fn test_state() -> ApiState {
        ApiState::new(
            "desktop-browser-controller-test",
            Arc::new(InMemoryEventBus::new(64)),
            Arc::new(SessionStore::new()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn node_selection_fixture() -> (ApiState, BrowserTabId) {
        let state = test_state();
        let profile_id = BrowserProfileId::new("browser-profile-node-selection");
        let browser_session_id = BrowserSessionId::new("browser-session-node-selection");
        let tab_id = BrowserTabId::new("browser-tab-node-selection");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: std::env::temp_dir().join("magi-browser-node-selection-test"),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: None,
                    session_id: SessionId::new("session-node-selection"),
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id,
                    url: "https://example.com".to_string(),
                    now: UtcMillis(2),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
                authority.set_primary_surface(
                    magi_browser_authority::BrowserSurfaceBinding {
                        desktop_epoch: "desktop-node-selection".to_string(),
                        window_id: "window-node-selection".to_string(),
                        surface_id: "surface-node-selection".to_string(),
                        surface_revision: 1,
                        tab_id: tab_id.clone(),
                        web_contents_id: 23,
                        target_id: "target-node-selection".to_string(),
                        browser_context_id: "context-node-selection".to_string(),
                        navigation_revision: 4,
                    },
                    UtcMillis(3),
                )?;
                Ok(())
            })
            .expect("node selection fixture should create");
        (state, tab_id)
    }

    fn node_selection_event(
        tab_id: BrowserTabId,
        surface_id: &str,
        navigation_revision: u64,
    ) -> BrowserHostEvent {
        BrowserHostEvent::NodeSelection(magi_browser_authority::BrowserNodeSelection {
            tab_id,
            surface_id: surface_id.to_string(),
            navigation_revision,
            browser_session_id: BrowserSessionId::new("browser-session-node-selection"),
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            frame_id: Some("frame-1".to_string()),
            backend_dom_node_id: 101,
            dom_node_id: Some(202),
            node_name: "BUTTON".to_string(),
            attributes: BTreeMap::from([("data-testid".to_string(), "submit".to_string())]),
            text_excerpt: "Submit".to_string(),
            outer_html: "<button>Submit</button>".to_string(),
            outer_html_truncated: false,
            aria_role: Some("button".to_string()),
            aria_name: Some("Submit".to_string()),
            bounds: Some(magi_browser_authority::BrowserHostRect {
                x: 10.0,
                y: 20.0,
                width: 120.0,
                height: 40.0,
            }),
        })
    }

    #[test]
    fn current_node_selection_is_published_as_a_structured_browser_event() {
        let (state, tab_id) = node_selection_fixture();
        let mut events = state.event_bus.subscribe();

        handle_host_event(
            &state,
            BrowserHostIncomingEvent {
                envelope: BrowserHostEventEnvelope {
                    protocol_version: BrowserHostProtocolVersion::CURRENT,
                    sequence: 1,
                    event: node_selection_event(tab_id, "surface-node-selection", 4),
                },
                binary: None,
            },
            0,
        );

        let event = events
            .try_recv()
            .expect("current node selection should be published");
        assert_eq!(event.event_type, "browser.node.selected");
        assert_eq!(event.payload["tab_id"], "browser-tab-node-selection");
        assert_eq!(event.payload["surface_id"], "surface-node-selection");
        assert_eq!(event.payload["navigation_revision"], 4);
        assert_eq!(event.payload["node_name"], "BUTTON");
    }

    #[test]
    fn stale_node_selection_is_dropped_before_it_reaches_the_event_bus() {
        let (state, tab_id) = node_selection_fixture();
        let mut events = state.event_bus.subscribe();

        for (surface_id, navigation_revision) in
            [("surface-node-selection", 3), ("replaced-surface", 4)]
        {
            handle_host_event(
                &state,
                BrowserHostIncomingEvent {
                    envelope: BrowserHostEventEnvelope {
                        protocol_version: BrowserHostProtocolVersion::CURRENT,
                        sequence: 1,
                        event: node_selection_event(
                            tab_id.clone(),
                            surface_id,
                            navigation_revision,
                        ),
                    },
                    binary: None,
                },
                0,
            );

            assert!(events.try_recv().is_err());
        }
    }

    #[test]
    fn desktop_connection_config_requires_all_identity_fields() {
        assert_eq!(
            DesktopBrowserConnectionConfig::from_values(
                "/tmp/magi.sock".to_string(),
                "token".to_string(),
                "desktop-epoch".to_string(),
                "0".to_string(),
            ),
            Err(DesktopBrowserConfigError::Invalid(
                DESKTOP_PARENT_PID_ENV,
                "0".to_string()
            ))
        );
        assert!(
            DesktopBrowserConnectionConfig::from_values(
                "/tmp/magi.sock".to_string(),
                "token".to_string(),
                "desktop-epoch".to_string(),
                "42".to_string(),
            )
            .is_ok()
        );
    }

    #[test]
    fn desktop_parent_liveness_check_distinguishes_live_and_missing_processes() {
        let mut system = System::new();
        assert!(is_process_alive(
            &mut system,
            Pid::from_u32(std::process::id())
        ));
        assert!(!is_process_alive(&mut system, Pid::from_u32(u32::MAX)));
    }

    #[test]
    fn restoring_session_keeps_tabs_unmaterialized() {
        let state = test_state();
        let profile_id = BrowserProfileId::new("browser-profile-restore");
        let browser_session_id = BrowserSessionId::new("browser-session-restore");
        let tab_id = BrowserTabId::new("browser-tab-restore");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: std::env::temp_dir().join("magi-browser-restore-test"),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: None,
                    session_id: SessionId::new("session-restore"),
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com".to_string(),
                    now: UtcMillis(2),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Suspended, UtcMillis(3))?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Recovering,
                    UtcMillis(3),
                )?;
                Ok(())
            })
            .expect("browser recovery fixture should create");

        restore_browser_sessions(&state).expect("logical browser session should restore");

        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority
                .session(&browser_session_id)
                .map(|session| session.lifecycle),
            Some(BrowserSessionLifecycle::Ready)
        );
        assert_eq!(
            authority.tab(&tab_id).map(|tab| tab.lifecycle),
            Some(BrowserTabLifecycle::Suspended)
        );
    }

    #[test]
    fn reconnect_restores_interrupted_browser_sessions_before_tab_activation() {
        let state = test_state();
        let profile_id = BrowserProfileId::new("browser-profile-reconnect");
        let browser_session_id = BrowserSessionId::new("browser-session-reconnect");
        let tab_id = BrowserTabId::new("browser-tab-reconnect");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: std::env::temp_dir().join("magi-browser-reconnect-test"),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: None,
                    session_id: SessionId::new("session-reconnect"),
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com".to_string(),
                    now: UtcMillis(2),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Interrupted,
                    UtcMillis(3),
                )?;
                Ok(())
            })
            .expect("interrupted browser session fixture should create");

        resume_interrupted_browser_sessions(&state)
            .expect("reconnected Browser Host should restore interrupted session");

        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority
                .session(&browser_session_id)
                .map(|session| session.lifecycle),
            Some(BrowserSessionLifecycle::Ready)
        );
        assert_eq!(
            authority.tab(&tab_id).map(|tab| tab.lifecycle),
            Some(BrowserTabLifecycle::Suspended)
        );
    }

    #[test]
    fn page_updated_does_not_drop_the_current_primary_surface() {
        let state = test_state();
        let profile_id = BrowserProfileId::new("browser-profile-page-update");
        let browser_session_id = BrowserSessionId::new("browser-session-page-update");
        let tab_id = BrowserTabId::new("browser-tab-page-update");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: std::env::temp_dir().join("magi-browser-page-update-test"),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: None,
                    session_id: SessionId::new("session-page-update"),
                    profile_id,
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id,
                    url: "https://example.com/old".to_string(),
                    now: UtcMillis(2),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
                authority.set_primary_surface(
                    magi_browser_authority::BrowserSurfaceBinding {
                        desktop_epoch: "desktop-page-update".to_string(),
                        window_id: "window-page-update".to_string(),
                        surface_id: "surface-page-update".to_string(),
                        surface_revision: 4,
                        tab_id: tab_id.clone(),
                        web_contents_id: 23,
                        target_id: "target-page-update".to_string(),
                        browser_context_id: "context-page-update".to_string(),
                        navigation_revision: 0,
                    },
                    UtcMillis(3),
                )?;
                Ok(())
            })
            .expect("page update fixture should create");

        handle_host_event(
            &state,
            BrowserHostIncomingEvent {
                envelope: BrowserHostEventEnvelope {
                    protocol_version: BrowserHostProtocolVersion::CURRENT,
                    sequence: 1,
                    event: BrowserHostEvent::PageUpdated {
                        binding: magi_browser_authority::BrowserSurfaceBinding {
                            desktop_epoch: "desktop-page-update".to_string(),
                            window_id: "window-page-update".to_string(),
                            surface_id: "surface-page-update".to_string(),
                            surface_revision: 4,
                            tab_id: tab_id.clone(),
                            web_contents_id: 23,
                            target_id: "target-page-update".to_string(),
                            browser_context_id: "context-page-update".to_string(),
                            navigation_revision: 1,
                        },
                        page_state: BrowserHostPageState {
                            tab_id: tab_id.clone(),
                            url: "https://example.com/new".to_string(),
                            origin: Some("https://example.com".to_string()),
                            title: "New page".to_string(),
                            navigation_revision: 1,
                        },
                    },
                },
                binary: None,
            },
            0,
        );

        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority
                .primary_surface(&tab_id)
                .map(|surface| surface.surface_id.as_str()),
            Some("surface-page-update")
        );
        assert_eq!(
            authority.tab(&tab_id).map(|tab| tab.url.as_str()),
            Some("https://example.com/new")
        );
    }

    #[test]
    fn desktop_disconnect_revokes_lease_without_closing_logical_tab() {
        let state = test_state();
        let profile_id = BrowserProfileId::new("browser-profile-disconnect");
        let browser_session_id = BrowserSessionId::new("browser-session-disconnect");
        let tab_id = BrowserTabId::new("browser-tab-disconnect");
        let lease_id = BrowserLeaseId::new("browser-lease-disconnect");
        let workspace_id = WorkspaceId::new("workspace-disconnect");
        let session_id = SessionId::new("session-disconnect");
        state
            .session_store
            .create_session_for_workspace_at(
                session_id.clone(),
                "Browser disconnect session",
                Some(workspace_id.to_string()),
                UtcMillis(1),
            )
            .expect("session fixture should create");
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-disconnect".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("普通会话 Turn 应保持运行".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("running turn fixture should create");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(BrowserProfile {
                    profile_id: profile_id.clone(),
                    kind: BrowserProfileKind::ManagedDefault,
                    data_path: std::env::temp_dir().join("magi-browser-disconnect-test"),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: Some(workspace_id.clone()),
                    session_id: session_id.clone(),
                    profile_id: profile_id.clone(),
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis(2),
                )?;
                authority.create_tab(CreateBrowserTab {
                    tab_id: tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "https://example.com".to_string(),
                    now: UtcMillis(2),
                })?;
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis(2))?;
                authority.set_primary_surface(
                    magi_browser_authority::BrowserSurfaceBinding {
                        desktop_epoch: "desktop-disconnect".to_string(),
                        window_id: "window-disconnect".to_string(),
                        surface_id: "surface-disconnect".to_string(),
                        surface_revision: 1,
                        tab_id: tab_id.clone(),
                        web_contents_id: 23,
                        target_id: "target-disconnect".to_string(),
                        browser_context_id: "context-disconnect".to_string(),
                        navigation_revision: 0,
                    },
                    UtcMillis(3),
                )?;
                authority.acquire_lease(AcquireBrowserLease {
                    lease_id: lease_id.clone(),
                    tab_id: tab_id.clone(),
                    surface_id: "surface-disconnect".to_string(),
                    owner: ExecutionOwnership {
                        workspace_id: Some(workspace_id.clone()),
                        session_id: Some(session_id.clone()),
                        task_id: Some(TaskId::new("task-disconnect")),
                        ..ExecutionOwnership::default()
                    },
                    turn_id: "turn-disconnect".to_string(),
                    goal_binding: None,
                    acquired_at: UtcMillis(3),
                    expires_at: UtcMillis(30_000),
                })?;
                Ok(())
            })
            .expect("browser disconnect fixture should create");

        let mut events = state.event_bus.subscribe();
        suspend_browser_sessions_for_host_disconnect(&state);

        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority.tab(&tab_id).map(|tab| tab.lifecycle),
            Some(BrowserTabLifecycle::Ready)
        );
        assert_eq!(
            authority.lease(&lease_id).map(|lease| lease.lifecycle),
            Some(BrowserLeaseLifecycle::Revoked)
        );
        assert_eq!(
            authority
                .lease(&lease_id)
                .and_then(|lease| lease.end_reason),
            Some(BrowserLeaseEndReason::RuntimeUnavailable)
        );
        assert_eq!(
            authority
                .session(&browser_session_id)
                .map(|session| session.lifecycle),
            Some(BrowserSessionLifecycle::Interrupted)
        );
        drop(authority);
        assert_eq!(
            state
                .session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .map(|turn| turn.status),
            Some("running".to_string()),
            "Browser Host 断线不得中断普通会话 Turn"
        );

        let event = events
            .try_recv()
            .expect("Browser Session 中断后应发布状态终态事件");
        assert_eq!(event.event_type, "browser.session.status_changed");
        assert_eq!(
            event.payload["browser_session_id"],
            browser_session_id.to_string()
        );
        assert_eq!(event.payload["session_id"], session_id.to_string());
        assert_eq!(event.payload["workspace_id"], workspace_id.to_string());
        assert_eq!(event.payload["lifecycle"], "interrupted");
    }
}
