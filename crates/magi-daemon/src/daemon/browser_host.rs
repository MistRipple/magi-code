use std::{env, fmt, time::Duration};

use magi_api::{ApiState, BrowserHostConnectionConfig, BrowserHostStatusSnapshot};
use magi_browser_authority::{
    BrowserHostClient, BrowserHostClientError, BrowserHostCommand, BrowserHostEvent,
    BrowserHostHandshake, BrowserHostIncomingEvent, BrowserHostStatus, BrowserLeaseEndReason,
    BrowserSessionLifecycle,
};
use magi_core::{BrowserTabId, EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
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

pub(super) fn start_controller(state: &ApiState) {
    if let Err(error) = restore_browser_sessions(state) {
        tracing::error!(%error, "恢复浏览器逻辑会话失败");
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
    handle.spawn(monitor_desktop_parent_process(state.clone()));
    handle.spawn(async move { run_desktop_browser_controller(state).await });
}

/// Electron owns the daemon in Desktop mode. The connection socket alone is
/// not sufficient as a lifecycle boundary: a hard Electron crash can leave a
/// daemon listening on the development port with stale browser leases. Keep a
/// process-level watchdog tied to the registered parent PID and terminate the
/// daemon after synchronously closing its execution resources.
async fn monitor_desktop_parent_process(state: ApiState) {
    let mut system = System::new();
    loop {
        let Some(config) = state.browser_host_connection_config() else {
            tokio::time::sleep(DESKTOP_PARENT_PROCESS_POLL_INTERVAL).await;
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
                interrupt_browser_tasks_for_runtime_failure(&state);
                let cancelled_process_count = ToolRegistry::cancel_all_active_processes();
                let cancelled_managed_process_count =
                    magi_process::terminate_all_managed_processes();
                tracing::info!(
                    cancelled_process_count,
                    cancelled_managed_process_count,
                    "Desktop parent death 清理已完成，daemon 即将退出"
                );
                std::process::exit(0);
            }
        }
        tokio::time::sleep(DESKTOP_PARENT_PROCESS_POLL_INTERVAL).await;
    }
}

fn is_process_alive(system: &mut System, pid: Pid) -> bool {
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some()
}

async fn run_desktop_browser_controller(state: ApiState) {
    let mut reconnecting = false;
    let mut active_config: Option<DesktopBrowserConnectionConfig> = None;
    let mut config_rx = state.browser_host_connection_receiver();
    loop {
        let Some(config) = state
            .browser_host_connection_config()
            .and_then(|config| DesktopBrowserConnectionConfig::from_runtime(config).ok())
        else {
            active_config = None;
            reconnecting = false;
            set_waiting_status(&state);
            if config_rx.changed().await.is_err() {
                return;
            }
            continue;
        };
        if active_config.as_ref() != Some(&config) {
            active_config = Some(config.clone());
            reconnecting = false;
        }
        let Some((client, handshake)) =
            connect_with_retries(&state, &config, reconnecting, &mut config_rx).await
        else {
            continue;
        };
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
        let mut events = client.subscribe();
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

        let disconnect =
            monitor_desktop_connection(&state, &client, &mut events, generation, &mut config_rx)
                .await;
        if state.browser_host_generation() == generation {
            state.set_browser_host_client(None);
        }
        client.close().await;
        tracing::warn!(reason = disconnect, "Electron Desktop 浏览器控制连接中断");

        if disconnect == "configuration_changed" {
            // 清理连接也可能由 Worker 崩溃触发。配置通道变化意味着旧
            // Desktop 运行边界已经失效，必须先撤销所有 Agent Lease，
            // 再等待新的 Worker/Host 注册，避免旧 Lease 跨代残留。
            interrupt_browser_tasks_for_runtime_failure(&state);
            reconnecting = false;
            continue;
        }

        // Desktop 页面由 Electron Main 持有。daemon 断线只撤销 Agent 控制边界，
        // 不关闭逻辑 Tab、不销毁 Surface，也不改变页面当前状态。
        interrupt_browser_tasks_for_runtime_failure(&state);
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
) -> Option<(BrowserHostClient, BrowserHostHandshake)> {
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
) -> &'static str {
    loop {
        tokio::select! {
            event = tokio::time::timeout(DESKTOP_HEARTBEAT_TIMEOUT, events.recv()) => {
                match event {
                    Ok(Ok(event)) => handle_host_event(state, event, generation),
                    Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                        tracing::warn!(skipped, "Electron Desktop 浏览器事件接收滞后");
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => return "event_stream_closed",
                    Err(_) => {
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
                let revoked = authority.set_primary_surface(binding.clone(), UtcMillis::now())?;
                let accepted = authority
                    .primary_surface(&binding.tab_id)
                    .is_some_and(|surface| surface == &binding);
                Ok((revoked, accepted))
            });
            match result {
                Ok((revoked, true)) => {
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
                Ok((_, false)) => tracing::debug!(
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
            suggested_filename,
            state: download_state,
            byte_length,
            error,
        } => {
            publish_tab_event(
                state,
                "browser.download.updated",
                browser_tab_context(state, &tab_id),
                serde_json::json!({
                    "tab_id": tab_id,
                    "suggested_filename": suggested_filename,
                    "state": download_state,
                    "byte_length": byte_length,
                    "error": error.map(|value| magi_core::public_runtime_excerpt(&value, 1024)),
                }),
            );
        }
        BrowserHostEvent::FileChooser { tab_id } => {
            publish_tab_event(
                state,
                "browser.file_chooser.cancelled",
                browser_tab_context(state, &tab_id),
                serde_json::json!({ "tab_id": tab_id }),
            );
        }
        BrowserHostEvent::PopupBlocked { tab_id } => {
            publish_tab_event(
                state,
                "browser.popup.blocked",
                browser_tab_context(state, &tab_id),
                serde_json::json!({ "tab_id": tab_id }),
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

fn interrupt_browser_tasks_for_runtime_failure(state: &ApiState) {
    let sessions = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .sessions
        .into_iter()
        .filter(|session| session.lifecycle.is_recoverable())
        .collect::<Vec<_>>();

    for browser_session in sessions {
        state.cancel_execution_resources(
            Some(&browser_session.session_id),
            browser_session.workspace_id.as_ref(),
            None,
            BrowserLeaseEndReason::RuntimeUnavailable,
        );
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
                "Desktop 浏览器失效后终止执行树失败"
            );
        }
        match state
            .session_store
            .interrupt_current_turn_by_daemon_restart(&browser_session.session_id)
        {
            Ok(Some(_)) => {
                state
                    .conversation_registry
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
                            "Desktop 浏览器失效后暂停 Goal 与计划失败"
                        ),
                    }
                }
                if let Err(error) =
                    state.persist_session_state_checkpoint("browser_desktop_disconnected_session")
                {
                    tracing::warn!(
                        session_id = %browser_session.session_id,
                        ?error,
                        "Desktop 浏览器失效后的 session 状态持久化失败"
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
                            "workspace_id": browser_session.workspace_id,
                            "turn_id": current_turn.turn_id,
                            "interrupted": true,
                            "reason": "browser_host_unavailable",
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
                "Desktop 浏览器失效后的 session Turn 收敛失败"
            ),
        }
    }
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
    use std::sync::Arc;

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
    use magi_session_store::SessionStore;
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
                        workspace_id: Some(workspace_id),
                        session_id: Some(session_id),
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

        interrupt_browser_tasks_for_runtime_failure(&state);

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
    }
}
