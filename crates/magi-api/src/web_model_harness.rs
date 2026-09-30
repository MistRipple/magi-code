//! T3 通道在 daemon 内的装配（设计基线 §5.7.3、§5.7.4；实现计划 4.3 / 4.9）。
//!
//! 本模块只做三件事，其余都留在 `magi-web-model`：
//! 1. **启动本机入口**：`magi-web-harness` 的 Streamable HTTP（`127.0.0.1` 随机
//!    高端口 + `state_root` 端口文件）与 stdio 中继要连的本地 socket；两者都只
//!    接固定桥接工具，不暴露 daemon 的任何 `/api/*` 路由；
//! 2. **托管 OpenAI Tunnel 客户端**：按需启动 / 停止 `openai/tunnel-client`，
//!    由它以 stdio 拉起 `magi-web-harness --stdio`；凭据**只按文件引用**，本模块
//!    从不读取凭据内容，也不把凭据写进日志、诊断或命令行以外的任何地方；
//! 3. **把通道就绪状态写进共享单元**：推理通道据此决定 T3 还是降档 T2（A15）。
//!
//! 不落盘的内容：通道运行状态、harness 会话、turn 令牌（A20、§5.12）。唯一落盘的
//! 是新产物「凭据引用」——用户私有凭据文件的路径与 Tunnel id，写在既有 settings
//! 里；API 密钥值始终只在用户自己的文件里。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use magi_web_model::harness::stdio::{LocalSocketServer, local_endpoint_for, serve_local_socket};
use magi_web_model::harness::{
    HarnessHttpServer, HarnessRegistry, TunnelClientStatus, TunnelManager, TunnelRuntimeConfig,
    serve_http,
};
use magi_web_model::{WebModelChannelState, WebSlotTable, status_from_tunnel};
use serde::{Deserialize, Serialize};

/// 凭据引用在 settings 里的段名（只存引用，不存密钥值）。
pub const WEB_MODEL_TUNNEL_SECTION: &str = "webModelTunnel";

/// 用户在「设置 → GPT Web 模型 → T3 通道」提供的凭据引用。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebModelTunnelConfig {
    /// 用户在自己的 OpenAI 账号创建的 Tunnel id。
    #[serde(default)]
    pub tunnel_id: String,
    /// 仅含 Tunnels Read + Use 的 API 密钥文件路径（**只引用，不读取**）。
    #[serde(default)]
    pub credential_file: String,
    /// `openai/tunnel-client` 可执行文件路径；缺省按 daemon 同目录 / 环境变量查找。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_binary: Option<String>,
    /// 期望的 SHA-256（hex）；缺省读同名 `.sha256` 文件，读不到即报告未校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_sha256: Option<String>,
}

impl WebModelTunnelConfig {
    pub fn is_configured(&self) -> bool {
        !self.tunnel_id.trim().is_empty() && !self.credential_file.trim().is_empty()
    }
}

/// 通道状态投影（HTTP 响应形状；不含任何凭据内容）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebModelTunnelStatus {
    /// 当前生效通道：`openai_tunnel` / `none`。Magi Connect 就绪时优先取它。
    pub channel: &'static str,
    pub ready: bool,
    pub code: String,
    pub detail: String,
    pub tunnel_id: String,
    pub credential_file: String,
    /// harness 本机 HTTP 入口（只监听 127.0.0.1）。
    pub listener_url: Option<String>,
    pub port_file: Option<String>,
    pub local_endpoint: Option<String>,
}

struct StartedHarness {
    http: Option<HarnessHttpServer>,
    local_socket: Option<LocalSocketServer>,
    port_file: Option<PathBuf>,
    local_endpoint: PathBuf,
}

struct HarnessInner {
    started: Option<StartedHarness>,
    tunnel: Option<Arc<TunnelManager>>,
    config: Option<WebModelTunnelConfig>,
    /// The status monitor is best-effort and only exists while a managed
    /// tunnel is running. Keep the cancellation handle and task together so
    /// reconfiguration cannot leave an old monitor publishing stale state
    /// over a newly configured tunnel.
    monitor_stop: Option<tokio::sync::oneshot::Sender<()>>,
    monitor_handle: Option<tokio::task::JoinHandle<()>>,
}

/// daemon 内的 T3 通道运行时。
pub struct WebModelHarnessRuntime {
    registry: Arc<HarnessRegistry>,
    channel: Arc<WebModelChannelState>,
    inner: Mutex<HarnessInner>,
    /// Serializes listener/tunnel lifecycle transitions. The state mutex is
    /// intentionally not held across awaits, so without this guard two
    /// concurrent setup calls could leak one HTTP listener or replace a
    /// socket after its port file had already been published.
    lifecycle: tokio::sync::Mutex<()>,
    /// Invalidates an in-flight status probe when the managed tunnel is
    /// stopped or replaced. `JoinHandle::abort` cannot cancel a
    /// `spawn_blocking` call once it has started, so the probe must verify its
    /// generation before publishing a result.
    monitor_generation: Arc<AtomicU64>,
    /// 推理通道的进程内绑定表。
    ///
    /// 表本身仍由 `WebModelHostFactory` 拥有（client 构造时用同一份引用）；
    /// 这里只是让 daemon 侧路由能对某个会话 / 线程推进 epoch——「重置为 Magi
    /// 对话」是显式的产品动作，不能让用户只能靠重开应用才拿到干净对话（§5.8）。
    /// 装配前为 `None`：拿不到绑定表时该动作明确失败，不假装成功。
    bindings: Mutex<Option<Arc<WebSlotTable>>>,
    /// 推理通道的运行态投影表（阶段 / 排队位置 / 账号消息计数）。
    ///
    /// 与绑定表同样由 `WebModelHostFactory` 拥有，这里只持有同一份引用，
    /// 供 daemon 只读投影（§5.10、§5.13）。
    runtime: Mutex<Option<Arc<magi_web_model::WebModelRuntimeRegistry>>>,
}

impl WebModelHarnessRuntime {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(HarnessRegistry::new()),
            channel: Arc::new(WebModelChannelState::new()),
            inner: Mutex::new(HarnessInner {
                started: None,
                tunnel: None,
                config: None,
                monitor_stop: None,
                monitor_handle: None,
            }),
            lifecycle: tokio::sync::Mutex::new(()),
            monitor_generation: Arc::new(AtomicU64::new(0)),
            bindings: Mutex::new(None),
            runtime: Mutex::new(None),
        }
    }

    /// 装配推理通道的绑定表（与 `WebModelHostFactory` 共享同一份）。
    pub fn set_bindings(&self, bindings: Arc<WebSlotTable>) {
        *self
            .bindings
            .lock()
            .expect("web model bindings lock poisoned") = Some(bindings);
    }

    /// 绑定表；尚未装配时返回 `None`（调用方必须显式失败）。
    pub fn bindings(&self) -> Option<Arc<WebSlotTable>> {
        self.bindings
            .lock()
            .expect("web model bindings lock poisoned")
            .clone()
    }

    /// 装配推理通道的运行态投影表（与 `WebModelHostFactory` 共享同一份）。
    pub fn set_runtime(&self, runtime: Arc<magi_web_model::WebModelRuntimeRegistry>) {
        *self
            .runtime
            .lock()
            .expect("web model runtime lock poisoned") = Some(runtime);
    }

    /// 运行态投影表；尚未装配时返回 `None`（调用方按「无在飞 turn」处理）。
    pub fn runtime(&self) -> Option<Arc<magi_web_model::WebModelRuntimeRegistry>> {
        self.runtime
            .lock()
            .expect("web model runtime lock poisoned")
            .clone()
    }

    /// harness 会话表：推理通道在自己的 client 里使用同一份。
    pub fn registry(&self) -> Arc<HarnessRegistry> {
        Arc::clone(&self.registry)
    }

    /// 通道就绪状态的共享单元：推理通道只读它决定档位。
    pub fn channel(&self) -> Arc<WebModelChannelState> {
        Arc::clone(&self.channel)
    }

    /// 端口文件的固定位置（只有 Streamable HTTP 传输需要）。
    pub fn port_file_path(state_root: &Path) -> PathBuf {
        state_root.join("web-model").join("harness.port")
    }

    pub fn configured(&self) -> Option<WebModelTunnelConfig> {
        self.inner
            .lock()
            .expect("web model harness lock poisoned")
            .config
            .clone()
    }

    pub fn status(&self) -> WebModelTunnelStatus {
        let (tunnel, config, listener_url, port_file, local_endpoint) = {
            let guard = self.inner.lock().expect("web model harness lock poisoned");
            (
                guard.tunnel.clone(),
                guard.config.clone(),
                guard
                    .started
                    .as_ref()
                    .and_then(|started| started.http.as_ref().map(HarnessHttpServer::local_url)),
                guard.started.as_ref().and_then(|started| {
                    started
                        .port_file
                        .as_ref()
                        .map(|path| path.display().to_string())
                }),
                guard
                    .started
                    .as_ref()
                    .map(|started| started.local_endpoint.display().to_string()),
            )
        };
        // Do not hold the runtime mutex while invoking the external
        // tunnel-client status command.  The command is a control-plane
        // probe and may take long enough to block unrelated daemon routes.
        let tunnel_status = tunnel.as_ref().map(|tunnel| tunnel.status());
        let channel_status = status_from_tunnel(tunnel_status.as_ref());
        let status = WebModelTunnelStatus {
            channel: if tunnel.is_some() {
                "openai_tunnel"
            } else {
                "none"
            },
            ready: channel_status.ready,
            code: channel_status.code.clone(),
            detail: channel_status.detail.clone(),
            tunnel_id: config
                .as_ref()
                .map(|config| config.tunnel_id.clone())
                .unwrap_or_default(),
            credential_file: config
                .as_ref()
                .map(|config| config.credential_file.clone())
                .unwrap_or_default(),
            listener_url,
            port_file,
            local_endpoint,
        };
        if !channel_status.ready {
            // Tunnel-client 退出或校验失败时，不能让已经挂起的 tools/call 继续
            // 等待；它们必须走超时/回填收口，而不是在已失效通道上悬挂。
            self.registry.revoke_all();
        }
        self.channel.set(channel_status);
        status
    }

    /// 启动本机入口（HTTP + 本地 socket）并写端口文件。幂等。
    ///
    /// 两条传输共用同一份会话表：stdio 中继通过本地 socket 连进来，HTTP 形态由
    /// Connect 的转发进程按端口文件连进来。任何一步失败都返回错误，由上层把 T3
    /// 判为不可用并降档 T2，绝不假装可用。
    pub async fn ensure_started(&self, state_root: &Path) -> Result<String, String> {
        let _lifecycle = self.lifecycle.lock().await;
        self.ensure_started_inner(state_root).await
    }

    async fn ensure_started_inner(&self, state_root: &Path) -> Result<String, String> {
        let directory = state_root.join("web-model");
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("创建 web-model 目录失败：{error}"))?;
        let port_file = Self::port_file_path(state_root);
        let local_endpoint = local_endpoint_for(state_root);
        {
            let guard = self.inner.lock().expect("web model harness lock poisoned");
            if let Some(started) = guard.started.as_ref()
                && (started.local_endpoint != local_endpoint
                    || started
                        .port_file
                        .as_ref()
                        .is_some_and(|path| path != &port_file))
            {
                // A daemon owns one harness endpoint for its state root.  Do
                // not silently reuse a socket/port from another root: that
                // would connect a newly configured tunnel to the old
                // registry while reporting the new state to callers.
                return Err(
                    "web model harness state root changed; restart the harness first".to_string(),
                );
            }
        }
        let need_http = self
            .inner
            .lock()
            .expect("web model harness lock poisoned")
            .started
            .as_ref()
            .and_then(|started| started.http.as_ref())
            .is_none();
        let http = if need_http {
            Some(
                serve_http(Arc::clone(&self.registry))
                    .await
                    .map_err(|error| format!("启动 magi-web-harness HTTP 入口失败：{error}"))?,
            )
        } else {
            None
        };
        let need_local_socket = self
            .inner
            .lock()
            .expect("web model harness lock poisoned")
            .started
            .as_ref()
            .and_then(|started| started.local_socket.as_ref())
            .is_none();
        let local_socket = if need_local_socket {
            match serve_local_socket(Arc::clone(&self.registry), local_endpoint.clone()).await {
                Ok(server) => Some(server),
                Err(error) => {
                    // `http` is newly created in this invocation.  Do not
                    // leak an HTTP listener when the stdio half cannot bind.
                    if let Some(http) = http {
                        http.stop();
                    }
                    return Err(format!("启动 magi-web-harness 本地 socket 失败：{error}"));
                }
            }
        } else {
            None
        };
        let url;
        let mut guard = self.inner.lock().expect("web model harness lock poisoned");
        if let Some(http) = http {
            // 端口文件只含端口号，供 Connect 的本地转发进程读取；stdio 传输
            // 从不创建或读取这个文件（R54）。
            if let Err(error) =
                magi_core::fs_atomic::write_atomic(&port_file, http.port.to_string())
            {
                http.stop();
                if let Some(local_socket) = local_socket {
                    local_socket.stop();
                }
                let _ = std::fs::remove_file(&port_file);
                return Err(format!("写入 harness 端口文件失败：{error}"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&port_file, std::fs::Permissions::from_mode(0o600));
            }
            url = http.local_url();
            let started = guard.started.get_or_insert_with(|| StartedHarness {
                http: None,
                local_socket: None,
                port_file: None,
                local_endpoint: local_endpoint.clone(),
            });
            started.http = Some(http);
            started.port_file = Some(port_file);
        } else {
            url = guard
                .started
                .as_ref()
                .and_then(|started| started.http.as_ref().map(HarnessHttpServer::local_url))
                .ok_or_else(|| "harness HTTP 入口未启动".to_string())?;
        }
        let started = guard.started.get_or_insert_with(|| StartedHarness {
            http: None,
            local_socket: None,
            port_file: None,
            local_endpoint: local_endpoint.clone(),
        });
        if let Some(local_socket) = local_socket {
            started.local_socket = Some(local_socket);
        }
        Ok(url)
    }

    /// OpenAI Tunnel 的 stdio 形态只需要本地 socket；它绝不启动 HTTP 监听器，
    /// 也绝不写 `harness.port`（R54）。
    async fn ensure_stdio_started(&self, state_root: &Path) -> Result<(), String> {
        let local_endpoint = local_endpoint_for(state_root);
        {
            let guard = self.inner.lock().expect("web model harness lock poisoned");
            if let Some(started) = guard.started.as_ref()
                && started.local_endpoint != local_endpoint
            {
                return Err(
                    "web model harness state root changed; restart the harness first".to_string(),
                );
            }
            if started_local_socket_exists(guard.started.as_ref()) {
                return Ok(());
            }
        }
        let local_socket = serve_local_socket(Arc::clone(&self.registry), local_endpoint.clone())
            .await
            .map_err(|error| format!("启动 magi-web-harness 本地 socket 失败：{error}"))?;
        let mut guard = self.inner.lock().expect("web model harness lock poisoned");
        let started = guard.started.get_or_insert_with(|| StartedHarness {
            http: None,
            local_socket: None,
            port_file: None,
            local_endpoint: local_endpoint.clone(),
        });
        started.local_socket = Some(local_socket);
        Ok(())
    }

    /// 应用通道配置：重建 tunnel 托管并在配置完整时启动。
    pub async fn configure(
        &self,
        state_root: &Path,
        config: Option<WebModelTunnelConfig>,
    ) -> Result<WebModelTunnelStatus, String> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_tunnel_inner().await;
        {
            let mut guard = self.inner.lock().expect("web model harness lock poisoned");
            guard.config = config.clone();
        }
        let Some(config) = config else {
            self.refresh_channel_status();
            return Ok(self.status());
        };
        if !config.is_configured() {
            self.refresh_channel_status();
            return Ok(self.status());
        }
        // Resolve and validate all local inputs before opening the daemon's
        // stdio bridge.  A bad client, checksum, tunnel id, or credential
        // reference must not leave a socket behind that looks usable to a
        // subsequently spawned relay.
        let harness_binary = resolve_harness_binary()?;
        let local_endpoint = local_endpoint_for(state_root);
        let tunnel = Arc::new(TunnelManager::new(TunnelRuntimeConfig {
            client_binary: resolve_client_binary(&config),
            expected_client_sha256: config
                .client_sha256
                .clone()
                .filter(|value| !value.trim().is_empty()),
            credential_file: PathBuf::from(config.credential_file.trim()),
            tunnel_id: config.tunnel_id.trim().to_string(),
            harness_binary,
            local_endpoint,
            profile_dir: state_root.join("web-model").join("tunnel-profile"),
            profile_name: "magi-web-model".to_string(),
            alias: "magi-web-model".to_string(),
            launch_args: TunnelRuntimeConfig::default_launch_args(),
        }));
        {
            let mut guard = self.inner.lock().expect("web model harness lock poisoned");
            guard.tunnel = Some(Arc::clone(&tunnel));
        }
        if tunnel.preflight_status().is_some() {
            self.refresh_channel_status();
            return Ok(self.status());
        }
        if let Err(error) = self.ensure_stdio_started(state_root).await {
            // Do not leave a local stdio endpoint after setup failed.  The
            // tunnel manager is also stopped so a later configure call starts
            // from one coherent state.
            // `configure` already owns `lifecycle`; calling the public
            // method here would wait on the same non-reentrant mutex forever.
            self.stop_tunnel_inner().await;
            return Err(error);
        }
        let status = tunnel.start().await;
        if matches!(
            status,
            TunnelClientStatus::Running { .. } | TunnelClientStatus::NotReady { .. }
        ) {
            // `runtimes connect` can return after the managed process is
            // healthy but before its local ready probe flips.  Keep the
            // socket alive and let the monitor publish the eventual ready
            // state instead of tearing down a valid, still-starting runtime.
            self.start_status_monitor(Arc::clone(&tunnel));
        } else {
            self.stop_started_harness();
            tracing::warn!(
                code = status.code(),
                detail = %status.detail(),
                "OpenAI Tunnel 未能启动，T3 降档到 T2"
            );
        }
        self.refresh_channel_status();
        Ok(self.status())
    }

    /// 只关闭 daemon 本机入口，保留失败的 tunnel 状态供状态页展示。
    fn stop_started_harness(&self) {
        let started = self
            .inner
            .lock()
            .expect("web model harness lock poisoned")
            .started
            .take();
        if let Some(started) = started {
            if let Some(http) = started.http {
                http.stop();
            }
            if let Some(local_socket) = started.local_socket {
                local_socket.stop();
            }
            if let Some(port_file) = started.port_file {
                let _ = std::fs::remove_file(port_file);
            }
        }
    }

    pub async fn stop_tunnel(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_tunnel_inner().await;
    }

    async fn stop_tunnel_inner(&self) {
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        let (monitor_stop, monitor_handle) = {
            let mut guard = self.inner.lock().expect("web model harness lock poisoned");
            (guard.monitor_stop.take(), guard.monitor_handle.take())
        };
        if let Some(stop) = monitor_stop {
            let _ = stop.send(());
        }
        if let Some(handle) = monitor_handle {
            // The monitor never owns the tunnel process; it is safe to stop
            // observing it before issuing the control-plane stop command.
            handle.abort();
        }
        let tunnel = {
            let mut guard = self.inner.lock().expect("web model harness lock poisoned");
            guard.tunnel.take()
        };
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        self.stop_started_harness();
        self.registry.revoke_all();
        self.refresh_channel_status();
    }

    /// 应用退出 / 清除数据：停通道、停本机入口、撤销全部挂起调用。
    pub async fn shutdown(&self) {
        self.stop_tunnel().await;
        self.refresh_channel_status();
    }

    /// 撤销全部在飞回复的 harness 会话（清除数据 / 退出登录时先做这一步）。
    pub fn revoke_turns(&self) -> usize {
        self.registry.revoke_all()
    }

    pub fn refresh_channel_status(&self) {
        let tunnel = self
            .inner
            .lock()
            .expect("web model harness lock poisoned")
            .tunnel
            .clone();
        let tunnel_status = tunnel.as_ref().map(|tunnel| tunnel.status());
        let status = status_from_tunnel(tunnel_status.as_ref());
        if !status.ready {
            self.registry.revoke_all();
        }
        self.channel.set(status);
    }

    /// Poll the tunnel-client control plane outside the daemon's async worker
    /// thread. `TunnelManager::status` deliberately uses the vendor CLI's
    /// synchronous status command; without this monitor a tunnel that exits
    /// after startup would leave the shared channel stuck at `ready=true`,
    /// causing new T3 turns to wait until the page timeout instead of falling
    /// back to T2.
    fn start_status_monitor(&self, tunnel: Arc<TunnelManager>) {
        let generation = self.monitor_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let monitor_generation = Arc::clone(&self.monitor_generation);
        let (stop, mut receiver) = tokio::sync::oneshot::channel();
        let channel = Arc::clone(&self.channel);
        let registry = Arc::clone(&self.registry);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tokio::select! {
                    _ = &mut receiver => break,
                    _ = interval.tick() => {
                        let probe = Arc::clone(&tunnel);
                        let status = match tokio::task::spawn_blocking(move || probe.status()).await {
                            Ok(status) => status,
                            Err(_) => TunnelClientStatus::Exited,
                        };
                        if monitor_generation.load(Ordering::SeqCst) != generation {
                            break;
                        }
                        let active = matches!(
                            status,
                            TunnelClientStatus::Running { .. }
                                | TunnelClientStatus::NotReady { .. }
                        );
                        channel.set(status_from_tunnel(Some(&status)));
                        if !active {
                            registry.revoke_all();
                            break;
                        }
                    }
                }
            }
        });
        let mut guard = self.inner.lock().expect("web model harness lock poisoned");
        guard.monitor_stop = Some(stop);
        guard.monitor_handle = Some(handle);
    }
}

fn started_local_socket_exists(started: Option<&StartedHarness>) -> bool {
    started
        .and_then(|started| started.local_socket.as_ref())
        .is_some()
}

impl Default for WebModelHarnessRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// harness 二进制与 daemon 同目录（打包形态），也允许显式指定。
fn resolve_harness_binary() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("MAGI_WEB_HARNESS_BINARY") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "MAGI_WEB_HARNESS_BINARY 指向的文件不存在：{}",
            path.display()
        ));
    }
    let name = if cfg!(windows) {
        "magi-web-harness.exe"
    } else {
        "magi-web-harness"
    };
    let candidate = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .ok_or_else(|| "无法定位 daemon 可执行文件目录".to_string())?;
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!("缺少 magi-web-harness：{}", candidate.display()))
    }
}

/// tunnel-client 位置：显式配置优先，其次环境变量，最后 daemon 同目录。
fn resolve_client_binary(config: &WebModelTunnelConfig) -> PathBuf {
    if let Some(explicit) = config.client_binary.as_ref() {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(from_env) = std::env::var_os("MAGI_TUNNEL_CLIENT_BINARY") {
        return PathBuf::from(from_env);
    }
    let name = if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_needs_both_tunnel_id_and_credential_reference() {
        let mut config = WebModelTunnelConfig::default();
        assert!(!config.is_configured());
        config.tunnel_id = "tunnel_123".to_string();
        assert!(!config.is_configured());
        config.credential_file = "/home/user/.openai/key".to_string();
        assert!(config.is_configured());
    }

    #[tokio::test]
    async fn an_unconfigured_channel_reports_the_missing_piece_without_credentials() {
        let runtime = WebModelHarnessRuntime::new();
        let status = runtime.status();
        assert!(!status.ready);
        assert_eq!(status.channel, "none");
        assert_eq!(status.code, "tunnel_not_configured");
    }

    #[tokio::test]
    async fn starting_the_local_entrance_writes_a_port_file_and_is_idempotent() {
        let runtime = WebModelHarnessRuntime::new();
        let root = std::env::temp_dir().join(format!(
            "magi-harness-test-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        let url = runtime
            .ensure_started(&root)
            .await
            .expect("本机入口应能启动");
        assert!(url.starts_with("http://127.0.0.1:"));
        let port_file = WebModelHarnessRuntime::port_file_path(&root);
        let port: u16 = std::fs::read_to_string(&port_file)
            .expect("端口文件应存在")
            .trim()
            .parse()
            .expect("端口文件应只含端口号");
        assert!(url.ends_with(&format!(":{port}/mcp")));
        let again = runtime
            .ensure_started(&root)
            .await
            .expect("重复调用是幂等的");
        assert_eq!(url, again);
        runtime.shutdown().await;
        assert!(!port_file.exists(), "关闭后端口文件应被清理");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn configuring_without_a_tunnel_id_keeps_t3_disabled() {
        let runtime = WebModelHarnessRuntime::new();
        let root = std::env::temp_dir().join(format!(
            "magi-harness-cfg-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        let status = runtime
            .configure(
                &root,
                Some(WebModelTunnelConfig {
                    tunnel_id: String::new(),
                    credential_file: "/tmp/key".to_string(),
                    ..Default::default()
                }),
            )
            .await
            .expect("半配置不应报错，只是不可用");
        assert!(!status.ready);
        assert_eq!(status.code, "tunnel_not_configured");
        runtime.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }
}
