//! GPT Web 工具通道在 daemon 内的装配（最终开发基线 §8.3）。
//!
//! GPT Web 的工具能力完全由 Magi MCP 服务提供，本模块只做三件事：
//! 1. **持有推理通道的共享引用**：单槽位表与运行态投影，让 daemon 路由能读取它们；
//! 2. **托管 OpenAI Tunnel 客户端**：按需启动 / 停止 `openai/tunnel-client`，由它以 stdio 拉起
//!    `magi-mcp --stdio --slot`（连接 daemon 的槽位端点）；凭据**只按文件引用**，本模块从不读取
//!    凭据内容，也不把凭据写进日志、诊断或命令行以外的任何地方；
//! 3. **把通道就绪状态写进共享单元**：`web_tunnel_unavailable` 的判据与“具体缺哪一项”。
//!
//! 不落盘的内容：通道运行状态与槽位。唯一落盘的是「凭据引用」——用户私有凭据文件的路径、
//! Tunnel id 与连接器权限档，写在既有 settings 里；API 密钥值始终只在用户自己的文件里。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use magi_web_model::{
    TunnelClientStatus, TunnelManager, TunnelRuntimeConfig, WebModelChannelState,
    WebModelChannelStatus, WebSlotTable, status_from_tunnel,
};
use serde::{Deserialize, Serialize};

use crate::state::ApiState;

/// 凭据引用在 settings 里的段名（只存引用，不存密钥值）。
pub const WEB_MODEL_TUNNEL_SECTION: &str = "webModelTunnel";

/// 用户在「设置 → 浏览器 → GPT Web」提供的通道配置。
///
/// 来源划分：Tunnel id 与运行时 API 密钥由用户在 OpenAI 平台创建（Magi 无法替用户创建，
/// 创建需要管理员密钥），在 Magi 设置里粘贴；`tunnel-client` 由 Magi 自行下载校验；权限档是
/// Magi 自己的设置；槽位端点、profile 目录、别名全部由 Magi 生成。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebModelTunnelConfig {
    /// 用户在自己的 OpenAI 账号创建的 Tunnel id。
    #[serde(default)]
    pub tunnel_id: String,
    /// 连接器权限档：`read_only` / `edit`（默认）/ `edit_trusted`。**没有 `exec`**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    /// 需要授权的工具调用怎么处理：`ask`（默认，每次询问）/ `always`（始终授权）/ `deny`（拒绝）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<String>,
}

/// GPT Web 工具调用的授权方式。只作用于 GPT Web 槽位发起、且按权限档需要人工确认的调用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WebApprovalMode {
    #[default]
    Ask,
    Always,
    Deny,
}

impl WebApprovalMode {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("always") => Self::Always,
            Some("deny") => Self::Deny,
            _ => Self::Ask,
        }
    }
}

impl WebModelTunnelConfig {
    pub fn is_configured(&self) -> bool {
        !self.tunnel_id.trim().is_empty()
    }
}

/// 运行时 API 密钥在 state root 下的私有文件：Magi 自己管理，只存在于这里。
pub(crate) fn api_key_path(state_root: &std::path::Path) -> PathBuf {
    state_root.join("web-model").join("tunnel-api-key")
}

/// 写入密钥文件（用户私有权限、原子写）。密钥原文不进日志、settings 或任何返回值。
fn write_api_key(state_root: &std::path::Path, api_key: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_whitespace) {
        return Err("API 密钥格式不正确".to_string());
    }
    let path = api_key_path(state_root);
    let parent = path.parent().ok_or("密钥路径无效")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("无法创建密钥目录：{error}"))?;
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("无法保存 API 密钥：{error}"))?;
        file.write_all(key.as_bytes())
            .map_err(|error| format!("无法保存 API 密钥：{error}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|error| format!("无法保存 API 密钥：{error}"))
}

/// 通道状态投影（HTTP 响应形状；不含任何凭据内容）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebModelTunnelStatus {
    /// 当前生效通道：`openai_tunnel` / `none`。
    pub channel: &'static str,
    pub ready: bool,
    pub code: String,
    pub detail: String,
    pub tunnel_id: String,
    /// 是否已保存运行时 API 密钥。
    pub has_api_key: bool,
    /// 已保存密钥的遮罩预览（只含末 4 位）。完整密钥只经桌面端专用的「显示」接口取得。
    pub api_key_preview: Option<String>,
    pub tool_profile: String,
    /// 授权方式：`ask` / `always` / `deny`。
    pub approval_mode: String,
    /// GPT Web 槽位端点（本地 socket，仅当前 OS 用户可访问）。
    pub slot_endpoint: Option<String>,
}

struct ChannelInner {
    tunnel: Option<Arc<TunnelManager>>,
    config: Option<WebModelTunnelConfig>,
    /// 状态监视只在托管的 tunnel 运行期间存在；取消句柄与任务一起保存，
    /// 重新配置不会让旧监视器用过期状态覆盖新的通道状态。
    monitor_stop: Option<tokio::sync::oneshot::Sender<()>>,
    monitor_handle: Option<tokio::task::JoinHandle<()>>,
    slot_endpoint: Option<PathBuf>,
    /// 启动前的准备阶段（安装组件等）。有值时优先于 tunnel 自身状态展示。
    phase: Option<(String, String)>,
}

/// daemon 内的 GPT Web 工具通道运行时。
pub struct WebModelChannelRuntime {
    channel: Arc<WebModelChannelState>,
    inner: Mutex<ChannelInner>,
    /// 串行化通道生命周期变更；状态锁不跨 await 持有。
    lifecycle: tokio::sync::Mutex<()>,
    /// `spawn_blocking` 一旦开始就无法取消：状态探测在发布结果前必须核对代次，
    /// 以免用停止 / 替换前的结果覆盖新状态。
    monitor_generation: Arc<AtomicU64>,
    /// 后台启动任务的代次：重新配置 / 停止会使进行中的启动作废。
    launch_generation: AtomicU64,
    /// 推理通道的单槽位表（与 `WebModelHostFactory` 共享同一份）。
    slots: Mutex<Option<Arc<WebSlotTable>>>,
    /// 推理通道的运行态投影（阶段 / 账号消息计数）。
    runtime: Mutex<Option<Arc<magi_web_model::WebModelRuntimeRegistry>>>,
    /// 宿主页面驱动（与推理通道共用）：已保存对话列表 / 删除、连接器配置都经它进入唯一的 WebView。
    driver: Mutex<Option<Arc<dyn magi_web_model::WebModelPageDriver>>>,
    /// 最近一次配置时的 state root（读取密钥是否存在用）。
    state_root: Mutex<Option<PathBuf>>,
}

impl WebModelChannelRuntime {
    pub fn new() -> Self {
        Self {
            channel: Arc::new(WebModelChannelState::new()),
            inner: Mutex::new(ChannelInner {
                tunnel: None,
                config: None,
                monitor_stop: None,
                monitor_handle: None,
                slot_endpoint: None,
                phase: None,
            }),
            lifecycle: tokio::sync::Mutex::new(()),
            monitor_generation: Arc::new(AtomicU64::new(0)),
            launch_generation: AtomicU64::new(0),
            slots: Mutex::new(None),
            runtime: Mutex::new(None),
            driver: Mutex::new(None),
            state_root: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ChannelInner> {
        self.inner.lock().expect("web model channel lock poisoned")
    }

    /// 装配单槽位表（与 `WebModelHostFactory` 共享同一份）。
    pub fn set_bindings(&self, slots: Arc<WebSlotTable>) {
        *self.slots.lock().expect("web model slots lock poisoned") = Some(slots);
    }

    /// 单槽位表；尚未装配时返回 `None`（调用方必须显式失败）。
    pub fn bindings(&self) -> Option<Arc<WebSlotTable>> {
        self.slots
            .lock()
            .expect("web model slots lock poisoned")
            .clone()
    }

    pub fn set_runtime(&self, runtime: Arc<magi_web_model::WebModelRuntimeRegistry>) {
        *self
            .runtime
            .lock()
            .expect("web model runtime lock poisoned") = Some(runtime);
    }

    /// 运行态投影表；尚未装配时返回 `None`（按「无在飞 turn」处理）。
    pub fn runtime(&self) -> Option<Arc<magi_web_model::WebModelRuntimeRegistry>> {
        self.runtime
            .lock()
            .expect("web model runtime lock poisoned")
            .clone()
    }

    pub fn set_driver(&self, driver: Arc<dyn magi_web_model::WebModelPageDriver>) {
        *self.driver.lock().expect("web model driver lock poisoned") = Some(driver);
    }

    /// 宿主页面驱动；尚未装配时返回 `None`（调用方必须显式失败）。
    pub fn driver(&self) -> Option<Arc<dyn magi_web_model::WebModelPageDriver>> {
        self.driver
            .lock()
            .expect("web model driver lock poisoned")
            .clone()
    }

    /// 通道就绪状态的共享单元。
    pub fn channel(&self) -> Arc<WebModelChannelState> {
        Arc::clone(&self.channel)
    }

    pub fn configured(&self) -> Option<WebModelTunnelConfig> {
        self.lock().config.clone()
    }

    pub fn status(&self) -> WebModelTunnelStatus {
        let (tunnel, config, slot_endpoint, phase) = {
            let guard = self.lock();
            (
                guard.tunnel.clone(),
                guard.config.clone(),
                guard
                    .slot_endpoint
                    .as_ref()
                    .map(|path| path.display().to_string()),
                guard.phase.clone(),
            )
        };
        // 不在持有状态锁时调用外部 tunnel-client 的状态命令：它是控制面探测，
        // 耗时足以阻塞无关的 daemon 路由。
        let tunnel_status = tunnel.as_ref().map(|tunnel| tunnel.status());
        let channel_status = match phase {
            Some((code, detail)) => WebModelChannelStatus::unavailable(code, detail),
            None => status_from_tunnel(tunnel_status.as_ref()),
        };
        let status = WebModelTunnelStatus {
            channel: if tunnel.is_some() || config.is_some() {
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
            has_api_key: self.has_api_key(),
            api_key_preview: self.api_key_preview(),
            tool_profile: config
                .as_ref()
                .and_then(|config| config.tool_profile.clone())
                .unwrap_or_else(|| "edit".to_string()),
            approval_mode: match WebApprovalMode::parse(
                config
                    .as_ref()
                    .and_then(|config| config.approval_mode.as_deref()),
            ) {
                WebApprovalMode::Ask => "ask",
                WebApprovalMode::Always => "always",
                WebApprovalMode::Deny => "deny",
            }
            .to_string(),
            slot_endpoint,
        };
        self.channel.set(channel_status);
        status
    }

    /// 当前授权方式（未配置时每次询问）。
    pub fn approval_mode(&self) -> WebApprovalMode {
        WebApprovalMode::parse(
            self.lock()
                .config
                .as_ref()
                .and_then(|config| config.approval_mode.as_deref()),
        )
    }

    /// 只改权限档 / 授权方式：下一次工具调用就生效，不重启通道。返回要落盘的完整配置。
    pub fn update_policy(
        &self,
        tool_profile: Option<String>,
        approval_mode: Option<String>,
    ) -> WebModelTunnelConfig {
        let mut guard = self.lock();
        let config = guard
            .config
            .get_or_insert_with(WebModelTunnelConfig::default);
        if let Some(profile) = tool_profile {
            config.tool_profile = Some(profile);
        }
        if let Some(mode) = approval_mode {
            config.approval_mode = Some(mode);
        }
        config.clone()
    }

    fn has_api_key(&self) -> bool {
        self.state_root
            .lock()
            .expect("web model state root lock poisoned")
            .as_deref()
            .is_some_and(|root| api_key_path(root).is_file())
    }

    /// 读取已保存的完整密钥：只给桌面端的「显示」按钮用，其余路径都只用遮罩预览。
    pub fn reveal_api_key(&self) -> Option<String> {
        let root = self
            .state_root
            .lock()
            .expect("web model state root lock poisoned")
            .clone()?;
        std::fs::read_to_string(api_key_path(&root))
            .ok()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
    }

    fn api_key_preview(&self) -> Option<String> {
        let key = self.reveal_api_key()?;
        let tail: String = key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Some(format!("••••••••{tail}"))
    }

    fn set_phase(&self, phase: Option<(&str, String)>) {
        self.lock().phase = phase.map(|(code, detail)| (code.to_string(), detail));
        self.status();
    }

    /// 应用通道配置：保存密钥（如有）、重建托管，并在**后台**完成组件安装与启动。
    ///
    /// 返回时通道通常还在准备中（`client_installing` / 启动中）；界面轮询状态即可。
    /// `api_key` 为 `None` 表示沿用已保存的密钥。
    pub async fn configure(
        self: &Arc<Self>,
        state: &ApiState,
        config: Option<WebModelTunnelConfig>,
        api_key: Option<String>,
    ) -> Result<WebModelTunnelStatus, String> {
        let state_root = state
            .runtime_persistence()
            .and_then(|persistence| persistence.state_root().map(std::path::Path::to_path_buf))
            .ok_or_else(|| "没有 state root，无法配置 GPT Web 工具通道".to_string())?;
        *self
            .state_root
            .lock()
            .expect("web model state root lock poisoned") = Some(state_root.clone());
        let launch = {
            let _lifecycle = self.lifecycle.lock().await;
            self.stop_tunnel_inner(state).await;
            let Some(config) = config else {
                // 显式清除：连密钥文件一起删除。
                let _ = std::fs::remove_file(api_key_path(&state_root));
                self.lock().config = None;
                self.refresh_channel_status();
                return Ok(self.status());
            };
            if let Some(api_key) = api_key.filter(|key| !key.trim().is_empty()) {
                write_api_key(&state_root, &api_key)?;
            }
            self.lock().config = Some(config.clone());
            self.launch_generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        // 密钥或 Tunnel id 不全：不下载任何东西，直接报告缺哪一项。
        let config = self
            .configured()
            .filter(WebModelTunnelConfig::is_configured);
        if config.is_none() || !self.has_api_key() {
            self.refresh_channel_status();
            return Ok(self.status());
        }
        self.set_phase(Some((
            "client_installing",
            "正在准备通道组件（首次启用需要下载 tunnel-client）".to_string(),
        )));
        let runtime = Arc::clone(self);
        let state = state.clone();
        tokio::spawn(async move {
            runtime.launch(&state, state_root, launch).await;
        });
        Ok(self.status())
    }

    /// 后台启动：安装组件 → 校验 → 打开槽位端点 → 启动 tunnel-client。
    async fn launch(&self, state: &ApiState, state_root: PathBuf, generation: u64) {
        let binary = match std::env::var_os("MAGI_TUNNEL_CLIENT_BINARY") {
            Some(explicit) => PathBuf::from(explicit),
            None => match crate::tunnel_client_install::install(&state_root).await {
                Ok(path) => path,
                Err(error) => {
                    if self.launch_generation.load(Ordering::SeqCst) == generation {
                        tracing::warn!(%error, "tunnel-client 安装失败，GPT Web 没有项目工具");
                        self.set_phase(Some(("client_install_failed", error)));
                    }
                    return;
                }
            },
        };
        let _lifecycle = self.lifecycle.lock().await;
        if self.launch_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        self.lock().phase = None;
        let Some(config) = self
            .configured()
            .filter(WebModelTunnelConfig::is_configured)
        else {
            self.refresh_channel_status();
            return;
        };
        // 先解析并校验全部本地输入，再打开 daemon 的槽位端点：错误的客户端、校验值、
        // Tunnel id 或凭据引用不能留下一个看起来可用的 socket。
        let mcp_binary = match resolve_mcp_binary() {
            Ok(binary) => binary,
            Err(error) => {
                self.set_phase(Some(("mcp_unavailable", error)));
                return;
            }
        };
        let slot_endpoint = crate::mcp_runtime::slot_endpoint_for(&state_root);
        let tunnel = Arc::new(TunnelManager::new(TunnelRuntimeConfig {
            client_binary: binary,
            expected_client_sha256: None,
            credential_file: api_key_path(&state_root),
            tunnel_id: config.tunnel_id.trim().to_string(),
            mcp_binary,
            local_endpoint: slot_endpoint,
            profile_dir: state_root.join("web-model").join("tunnel-profile"),
            profile_name: "magi-web-model".to_string(),
            alias: "magi-web-model".to_string(),
            launch_args: TunnelRuntimeConfig::default_launch_args(),
        }));
        self.lock().tunnel = Some(Arc::clone(&tunnel));
        if tunnel.preflight_status().is_some() {
            self.refresh_channel_status();
            return;
        }
        match state.mcp_service.start_slot_gateway(state).await {
            Ok(endpoint) => self.lock().slot_endpoint = Some(endpoint),
            Err(error) => {
                self.stop_tunnel_inner(state).await;
                self.set_phase(Some(("mcp_unavailable", error.to_string())));
                return;
            }
        }
        let status = tunnel.start().await;
        if matches!(
            status,
            TunnelClientStatus::Running { .. } | TunnelClientStatus::NotReady { .. }
        ) {
            // `runtimes connect` 可能在托管进程健康但本地 ready 探测尚未翻转时返回：
            // 保持端点，由监视器发布最终的就绪状态，而不是拆掉一个仍在启动的运行时。
            self.start_status_monitor(Arc::clone(&tunnel));
        } else {
            state.mcp_service.stop_slot_gateway().await;
            self.lock().slot_endpoint = None;
            tracing::warn!(
                code = status.code(),
                detail = %status.detail(),
                "OpenAI Tunnel 未能启动，GPT Web 没有项目工具"
            );
        }
        self.refresh_channel_status();
    }

    pub async fn stop_tunnel(&self, state: &ApiState) {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_tunnel_inner(state).await;
    }

    async fn stop_tunnel_inner(&self, state: &ApiState) {
        self.launch_generation.fetch_add(1, Ordering::SeqCst);
        self.lock().phase = None;
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        let (monitor_stop, monitor_handle) = {
            let mut guard = self.lock();
            (guard.monitor_stop.take(), guard.monitor_handle.take())
        };
        if let Some(stop) = monitor_stop {
            let _ = stop.send(());
        }
        if let Some(handle) = monitor_handle {
            // 监视器不拥有 tunnel 进程，先停止观察再发停止命令是安全的。
            handle.abort();
        }
        let tunnel = self.lock().tunnel.take();
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        state.mcp_service.stop_slot_gateway().await;
        self.lock().slot_endpoint = None;
        self.refresh_channel_status();
    }

    /// 应用退出：停通道、停槽位端点。
    pub async fn shutdown(&self, state: &ApiState) {
        self.stop_tunnel(state).await;
    }

    pub fn refresh_channel_status(&self) {
        let tunnel = self.lock().tunnel.clone();
        let tunnel_status = tunnel.as_ref().map(|tunnel| tunnel.status());
        self.channel.set(status_from_tunnel(tunnel_status.as_ref()));
    }

    /// 在 daemon 的异步 worker 线程之外轮询 tunnel-client 控制面。
    /// `TunnelManager::status` 刻意使用厂商 CLI 的同步状态命令；没有这个监视器时，启动后退出的
    /// tunnel 会让共享通道一直停在 `ready=true`。
    fn start_status_monitor(&self, tunnel: Arc<TunnelManager>) {
        let generation = self.monitor_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let monitor_generation = Arc::clone(&self.monitor_generation);
        let (stop, mut receiver) = tokio::sync::oneshot::channel();
        let channel = Arc::clone(&self.channel);
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
                            TunnelClientStatus::Running { .. } | TunnelClientStatus::NotReady { .. }
                        );
                        channel.set(status_from_tunnel(Some(&status)));
                        if !active {
                            break;
                        }
                    }
                }
            }
        });
        let mut guard = self.lock();
        guard.monitor_stop = Some(stop);
        guard.monitor_handle = Some(handle);
    }
}

impl Default for WebModelChannelRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// 中继就是 daemon 自己的可执行文件（`mcp-relay` 子命令），不依赖另一个需要打包的二进制。
fn resolve_mcp_binary() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("无法定位 daemon 可执行文件：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_only_needs_the_tunnel_id() {
        let mut config = WebModelTunnelConfig::default();
        assert!(!config.is_configured());
        config.tunnel_id = "tunnel_123".to_string();
        assert!(config.is_configured());
    }

    #[tokio::test]
    async fn an_unconfigured_channel_reports_the_missing_piece_without_credentials() {
        let runtime = WebModelChannelRuntime::new();
        let status = runtime.status();
        assert!(!status.ready);
        assert_eq!(status.channel, "none");
        assert_eq!(status.code, "tunnel_not_configured");
        assert_eq!(status.tool_profile, "edit", "连接器权限档默认是 edit");
        assert!(!status.has_api_key);
        assert!(status.api_key_preview.is_none());
        assert!(status.slot_endpoint.is_none());
    }

    #[test]
    fn the_persisted_config_holds_no_credential_material() {
        let config = WebModelTunnelConfig {
            tunnel_id: "t".to_string(),
            tool_profile: Some("read_only".to_string()),
            approval_mode: Some("always".to_string()),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["toolProfile"], "read_only");
        assert!(json.get("apiKey").is_none() && json.get("credentialFile").is_none());
    }

    #[test]
    fn the_api_key_is_stored_privately_and_malformed_keys_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_api_key(dir.path(), "  ").is_err());
        assert!(write_api_key(dir.path(), "sk-has space").is_err());
        assert!(write_api_key(dir.path(), "sk-proj-abc123").is_ok());
        let path = api_key_path(dir.path());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sk-proj-abc123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn approval_mode_defaults_to_asking_and_policy_updates_apply_without_a_restart() {
        assert_eq!(WebApprovalMode::parse(None), WebApprovalMode::Ask);
        assert_eq!(
            WebApprovalMode::parse(Some("whatever")),
            WebApprovalMode::Ask
        );
        assert_eq!(
            WebApprovalMode::parse(Some("always")),
            WebApprovalMode::Always
        );
        assert_eq!(WebApprovalMode::parse(Some("deny")), WebApprovalMode::Deny);

        let runtime = WebModelChannelRuntime::new();
        assert_eq!(runtime.approval_mode(), WebApprovalMode::Ask);
        assert_eq!(runtime.status().approval_mode, "ask");
        let config = runtime.update_policy(None, Some("always".to_string()));
        assert_eq!(config.approval_mode.as_deref(), Some("always"));
        assert_eq!(runtime.approval_mode(), WebApprovalMode::Always);
        assert_eq!(runtime.status().approval_mode, "always");
        // 只改权限档不动授权方式。
        let config = runtime.update_policy(Some("read_only".to_string()), None);
        assert_eq!(config.tool_profile.as_deref(), Some("read_only"));
        assert_eq!(config.approval_mode.as_deref(), Some("always"));
        runtime.update_policy(None, Some("deny".to_string()));
        assert_eq!(runtime.approval_mode(), WebApprovalMode::Deny);
    }
}
