//! Magi MCP 服务的运行时：开关、固定回环端口、令牌持久化与吊销联动。
//!
//! 持久化放在 `state_root/mcp-server.json`（用户私有权限、原子写），**不放进 settings**：
//! settings 会整体投影给前端并允许前端写回，令牌哈希不应经过那条面。文件读不出来一律视为
//! “没有令牌、服务关闭”，绝不让 daemon 起不来（设计 M11）。令牌原文从不落盘。
//!
//! 端口一经选定就持久化：之后每次启动都用同一个端口，被占用时明确报错，不静默换端口，
//! 这样客户端配置一次即可长期使用。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use magi_conversation_runtime::external_approval::cancel_external_approvals;
use magi_core::UtcMillis;
use magi_mcp_server::http::{HttpConfig, HttpState, router};
use magi_mcp_server::local_socket::{
    LocalSocketServer, local_endpoint_for, serve_local_socket, serve_local_socket_with_auth,
};
use magi_mcp_server::{IssueTokenRequest, IssuedToken, TokenRecord, TokenStore};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::mcp_service::{build_mcp_server, external_session_id};
use crate::mcp_tunnel::{QuickTunnelProvider, TunnelProvider, host_of_public_url};
use crate::state::ApiState;

const LOOPBACK: &str = "127.0.0.1";

#[derive(Debug, thiserror::Error)]
pub(crate) enum McpRuntimeError {
    #[error("无法监听 {LOOPBACK}:{port}（端口被占用？）：{reason}")]
    Bind { port: u16, reason: String },
    #[error("保存 MCP 服务配置失败: {0}")]
    Persist(String),
    #[error("{0}")]
    Token(String),
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedMcp {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    tokens: Vec<TokenRecord>,
}

struct Running {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    /// stdio 中继连接的本地 socket；只要服务在运行就一起提供。
    local_socket: Option<LocalSocketServer>,
    /// 与 HTTP 入口共享的隧道主机名表：网络模式运行期间写入，停止时清空。
    tunnel_hosts: Arc<RwLock<Vec<String>>>,
}

#[derive(Default)]
struct Control {
    enabled: bool,
    port: Option<u16>,
    running: Option<Running>,
}

/// 网络模式（公网隧道）状态。隧道是 Quick Tunnel：**公网地址每次启动都会变**。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkStatus {
    pub enabled: bool,
    /// `stopped` / `installing` / `starting` / `running` / `error`
    pub status: String,
    pub public_url: Option<String>,
    /// 客户端配置用的完整 MCP 地址（`<公网地址>/mcp`）。
    pub mcp_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpServiceStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub url: Option<String>,
    /// stdio 中继连接的本地端点（`magi-mcp --endpoint`）。服务未运行时为空。
    pub stdio_endpoint: Option<String>,
    /// state root（`magi-mcp --state-root`），用于生成 stdio 配置。
    pub state_root: Option<String>,
    pub active_tokens: usize,
    pub network: NetworkStatus,
}

/// 审计日志保留的最大条数。超出后丢弃最旧的。
const AUDIT_MAX_ENTRIES: usize = 2000;

/// 一条调用审计。只含工具与路径摘要；**不含令牌原文、不含文件正文**。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditEntry {
    pub token_id: String,
    pub client_name: String,
    pub workspace_id: String,
    pub tool: String,
    pub requires_approval: bool,
    pub paths: Vec<String>,
    /// `succeeded` / `failed` / `denied`
    pub outcome: String,
    pub detail: Option<String>,
    pub at_ms: u64,
}

/// 有界的调用审计：内存里保留最近若干条，同时追加写到 `mcp-audit.jsonl`（私有权限）。
/// 文件超过上限的 1.5 倍时整体压缩为最近 `AUDIT_MAX_ENTRIES` 条。
pub(crate) struct McpAuditLog {
    path: Option<PathBuf>,
    entries: Mutex<std::collections::VecDeque<AuditEntry>>,
    appended_since_compact: Mutex<usize>,
}

impl McpAuditLog {
    pub(crate) fn in_memory() -> Self {
        Self {
            path: None,
            entries: Mutex::new(Default::default()),
            appended_since_compact: Mutex::new(0),
        }
    }

    pub(crate) fn load(path: PathBuf) -> Self {
        let mut entries = std::collections::VecDeque::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Ok(entry) = serde_json::from_str::<AuditEntry>(line) {
                    entries.push_back(entry);
                    if entries.len() > AUDIT_MAX_ENTRIES {
                        entries.pop_front();
                    }
                }
            }
        }
        Self {
            path: Some(path),
            entries: Mutex::new(entries),
            appended_since_compact: Mutex::new(0),
        }
    }

    pub(crate) fn record(&self, entry: AuditEntry) {
        let mut entries = self.entries.lock().expect("mcp audit poisoned");
        entries.push_back(entry.clone());
        while entries.len() > AUDIT_MAX_ENTRIES {
            entries.pop_front();
        }
        let Some(path) = &self.path else {
            return;
        };
        let mut appended = self
            .appended_since_compact
            .lock()
            .expect("mcp audit poisoned");
        *appended += 1;
        let result = if *appended > AUDIT_MAX_ENTRIES / 2 {
            *appended = 0;
            let mut body = String::new();
            for kept in entries.iter() {
                if let Ok(line) = serde_json::to_string(kept) {
                    body.push_str(&line);
                    body.push('\n');
                }
            }
            write_private_atomic(path, body.as_bytes())
        } else {
            append_private_line(path, &entry)
        };
        if let Err(error) = result {
            tracing::warn!(%error, "MCP 审计写入失败");
        }
    }

    /// 最近的调用，最新的在前；可按令牌过滤。
    pub(crate) fn recent(&self, limit: usize, token_id: Option<&str>) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .expect("mcp audit poisoned")
            .iter()
            .rev()
            .filter(|entry| token_id.is_none_or(|id| entry.token_id == id))
            .take(limit)
            .cloned()
            .collect()
    }
}

fn append_private_line(path: &std::path::Path, entry: &AuditEntry) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let mut line = serde_json::to_string(entry)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    line.push('\n');
    file.write_all(line.as_bytes())
}

pub(crate) struct McpServiceRuntime {
    audit: McpAuditLog,
    tunnel: Arc<dyn TunnelProvider>,
    /// 网络模式是否开启。不持久化：每次都由用户显式开启，daemon 重启后关闭。
    network_enabled: Arc<AtomicBool>,
    network_generation: Arc<AtomicU64>,
    /// GPT Web 槽位端点（本地 socket，无令牌；身份与归属由槽位表决定）。
    slot_socket: AsyncMutex<Option<LocalSocketServer>>,
    path: Option<PathBuf>,
    tokens: Arc<TokenStore>,
    control: AsyncMutex<Control>,
    persist_lock: Mutex<()>,
}

fn now_ms() -> u64 {
    UtcMillis::now().0
}

impl McpServiceRuntime {
    /// 不持久化（测试与轻量状态）。
    pub(crate) fn in_memory() -> Self {
        Self {
            audit: McpAuditLog::in_memory(),
            tunnel: QuickTunnelProvider::shared(),
            network_enabled: Arc::new(AtomicBool::new(false)),
            network_generation: Arc::new(AtomicU64::new(0)),
            slot_socket: AsyncMutex::new(None),
            path: None,
            tokens: Arc::new(TokenStore::new()),
            control: AsyncMutex::new(Control::default()),
            persist_lock: Mutex::new(()),
        }
    }

    /// 从文件恢复。文件不存在或损坏时按“没有令牌、服务关闭”处理。
    pub(crate) fn load(path: PathBuf) -> Self {
        let persisted = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<PersistedMcp>(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "MCP 服务配置无法解析，按未配置处理");
                PersistedMcp::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PersistedMcp::default(),
            Err(error) => {
                tracing::warn!(%error, "MCP 服务配置无法读取，按未配置处理");
                PersistedMcp::default()
            }
        };
        let audit = McpAuditLog::load(path.with_file_name("mcp-audit.jsonl"));
        let tokens = Arc::new(TokenStore::from_records(persisted.tokens));
        Self {
            audit,
            tunnel: QuickTunnelProvider::shared(),
            network_enabled: Arc::new(AtomicBool::new(false)),
            network_generation: Arc::new(AtomicU64::new(0)),
            slot_socket: AsyncMutex::new(None),
            path: Some(path),
            tokens,
            control: AsyncMutex::new(Control {
                enabled: persisted.enabled,
                port: persisted.port,
                running: None,
            }),
            persist_lock: Mutex::new(()),
        }
    }

    /// 配置文件所在目录即 state root（`state_root/mcp-server.json`）。
    fn state_root(&self) -> Option<&std::path::Path> {
        self.path.as_deref().and_then(std::path::Path::parent)
    }

    pub(crate) fn audit(&self) -> &McpAuditLog {
        &self.audit
    }

    fn persist(&self, enabled: bool, port: Option<u16>) -> Result<(), McpRuntimeError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let _guard = self.persist_lock.lock().expect("mcp persist lock poisoned");
        let body = serde_json::to_vec_pretty(&PersistedMcp {
            enabled,
            port,
            tokens: self.tokens.records(),
        })
        .map_err(|error| McpRuntimeError::Persist(error.to_string()))?;
        write_private_atomic(path, &body)
            .map_err(|error| McpRuntimeError::Persist(error.to_string()))
    }

    pub(crate) async fn status(&self) -> McpServiceStatus {
        let control = self.control.lock().await;
        self.status_locked(&control).await
    }

    async fn status_locked(&self, control: &Control) -> McpServiceStatus {
        let now = now_ms();
        let addr = control.running.as_ref().map(|running| running.addr);
        let enabled = self.network_enabled.load(Ordering::SeqCst);
        let snapshot = if enabled {
            self.tunnel.snapshot().await
        } else {
            Default::default()
        };
        let mcp_url = snapshot
            .public_url
            .as_ref()
            .map(|url| format!("{}/mcp", url.trim_end_matches('/')));
        McpServiceStatus {
            enabled: control.enabled,
            running: control.running.is_some(),
            port: addr.map(|addr| addr.port()).or(control.port),
            url: addr.map(|addr| format!("http://{addr}/mcp")),
            stdio_endpoint: control
                .running
                .as_ref()
                .and_then(|running| running.local_socket.as_ref())
                .map(|socket| socket.endpoint.display().to_string()),
            state_root: self.state_root().map(|root| root.display().to_string()),
            active_tokens: self
                .tokens
                .records()
                .iter()
                .filter(|record| record.is_active(now))
                .count(),
            network: NetworkStatus {
                enabled,
                status: if enabled && snapshot.status.is_empty() {
                    "starting".to_string()
                } else if snapshot.status.is_empty() {
                    "stopped".to_string()
                } else {
                    snapshot.status
                },
                public_url: snapshot.public_url,
                mcp_url,
                error: snapshot.error,
            },
        }
    }

    /// 开关服务。开启时绑定固定端口（首次为系统分配并持久化）。
    pub(crate) async fn set_enabled(
        &self,
        state: &ApiState,
        enabled: bool,
    ) -> Result<McpServiceStatus, McpRuntimeError> {
        let mut control = self.control.lock().await;
        if enabled {
            self.start_locked(state, &mut control).await?;
        } else {
            self.stop_locked(&mut control).await;
        }
        control.enabled = enabled;
        self.persist(control.enabled, control.port)?;
        Ok(self.status_locked(&control).await)
    }

    /// daemon 启动时调用：上次是开启的就恢复。失败只记日志，不影响 daemon。
    pub(crate) async fn start_if_enabled(&self, state: &ApiState) {
        let mut control = self.control.lock().await;
        if control.enabled
            && control.running.is_none()
            && let Err(error) = self.start_locked(state, &mut control).await
        {
            tracing::warn!(%error, "MCP 服务启动失败，保持关闭");
        }
    }

    async fn start_locked(
        &self,
        state: &ApiState,
        control: &mut Control,
    ) -> Result<(), McpRuntimeError> {
        if control.running.is_some() {
            return Ok(());
        }
        let port = control.port.unwrap_or(0);
        let listener =
            TcpListener::bind((LOOPBACK, port))
                .await
                .map_err(|error| McpRuntimeError::Bind {
                    port,
                    reason: error.to_string(),
                })?;
        let addr = listener
            .local_addr()
            .map_err(|error| McpRuntimeError::Bind {
                port,
                reason: error.to_string(),
            })?;
        control.port = Some(addr.port());

        let server = build_mcp_server(state.clone(), self.tokens.clone());
        let http_state = HttpState::new(server.clone(), self.tokens.clone(), HttpConfig::default());
        let tunnel_hosts = http_state.tunnel_hosts.clone();
        let app = router(http_state);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        // stdio 中继入口：失败不影响 HTTP 入口，只记日志。
        let local_socket = match self.state_root() {
            Some(root) => {
                match serve_local_socket(server, self.tokens.clone(), local_endpoint_for(root))
                    .await
                {
                    Ok(socket) => Some(socket),
                    Err(error) => {
                        tracing::warn!(%error, "MCP stdio 本地 socket 启动失败");
                        None
                    }
                }
            }
            None => None,
        };
        control.running = Some(Running {
            addr,
            stop: Some(stop),
            task,
            local_socket,
            tunnel_hosts,
        });
        Ok(())
    }

    async fn stop_locked(&self, control: &mut Control) {
        // 服务停止意味着隧道没有东西可转发：先关网络模式。
        self.disable_network_locked(control).await;
        if let Some(mut running) = control.running.take() {
            if let Some(stop) = running.stop.take() {
                let _ = stop.send(());
            }
            if let Some(socket) = running.local_socket.take() {
                socket.stop();
            }
            // 优雅关闭会等在途请求；给一个上限，之后强制结束任务。
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut running.task)
                .await
                .is_err()
            {
                running.task.abort();
            }
        }
    }

    /// 启动 GPT Web 槽位端点（本地 socket）。幂等；不依赖用户是否启用了“MCP 服务”开关。
    ///
    /// 端点只有当前 OS 用户可访问，不接受也不签发令牌；身份与归属完全由槽位表决定。
    pub(crate) async fn start_slot_gateway(
        &self,
        state: &ApiState,
    ) -> Result<PathBuf, McpRuntimeError> {
        let state_root = self
            .state_root()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| McpRuntimeError::Persist("没有 state root，无法创建槽位端点".into()))?;
        let endpoint = slot_endpoint_for(&state_root);
        let mut slot = self.slot_socket.lock().await;
        if slot.is_some() {
            return Ok(endpoint);
        }
        let server = crate::mcp_service::build_slot_mcp_server(state.clone());
        let harness = state.web_model.clone();
        let auth = crate::web_slot_mcp::slot_principal_provider(
            {
                let harness = harness.clone();
                Arc::new(move || harness.bindings())
            },
            Arc::new(move || {
                crate::web_slot_mcp::slot_profile_from_setting(
                    harness
                        .configured()
                        .and_then(|config| config.tool_profile)
                        .as_deref(),
                )
            }),
        );
        let socket = serve_local_socket_with_auth(server, auth, endpoint.clone())
            .await
            .map_err(|error| McpRuntimeError::Bind {
                port: 0,
                reason: error.to_string(),
            })?;
        *slot = Some(socket);
        Ok(endpoint)
    }

    pub(crate) async fn stop_slot_gateway(&self) {
        if let Some(socket) = self.slot_socket.lock().await.take() {
            socket.stop();
        }
    }

    fn has_active_network_token(&self) -> bool {
        let now = now_ms();
        self.tokens
            .records()
            .iter()
            .any(|record| record.network && record.is_active(now))
    }

    /// 开关网络模式（公网隧道）。开启条件：至少有一个有效且允许网络访问的令牌。
    ///
    /// 隧道只在这里按用户的显式操作启动，绝不随 daemon 自动启动；地址是 Quick Tunnel 的随机地址，
    /// 每次开启都会变，客户端配置需要重新填写。
    pub(crate) async fn set_network(
        &self,
        state: &ApiState,
        enabled: bool,
    ) -> Result<McpServiceStatus, McpRuntimeError> {
        let mut control = self.control.lock().await;
        if enabled {
            if !self.has_active_network_token() {
                return Err(McpRuntimeError::Token(
                    "需要至少一个有效且允许网络访问的令牌才能开启网络模式".to_string(),
                ));
            }
            if self.network_enabled.load(Ordering::SeqCst) {
                return Ok(self.status_locked(&control).await);
            }
            // 网络模式依赖本机服务在运行，但不改变用户的“启用”开关。
            self.start_locked(state, &mut control).await?;
            self.persist(control.enabled, control.port)?;
            let port = control.port.expect("服务运行时端口已确定");
            let hosts = control
                .running
                .as_ref()
                .expect("服务已启动")
                .tunnel_hosts
                .clone();
            self.tunnel.start(port).await;
            self.network_enabled.store(true, Ordering::SeqCst);
            let generation = self.network_generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.spawn_network_watcher(hosts, generation);
        } else {
            self.disable_network_locked(&mut control).await;
        }
        Ok(self.status_locked(&control).await)
    }

    async fn disable_network_locked(&self, control: &mut Control) {
        self.network_generation.fetch_add(1, Ordering::SeqCst);
        self.network_enabled.store(false, Ordering::SeqCst);
        self.tunnel.stop().await;
        if let Some(running) = &control.running {
            running
                .tunnel_hosts
                .write()
                .expect("tunnel hosts poisoned")
                .clear();
        }
    }

    /// 跟随隧道状态维护“允许的隧道主机名”，并在条件消失时自动停止：
    /// 隧道断开则立即不再接受隧道请求；没有有效的网络令牌（吊销、过期）则关闭隧道。
    fn spawn_network_watcher(&self, hosts: Arc<RwLock<Vec<String>>>, generation: u64) {
        let tunnel = self.tunnel.clone();
        let tokens = self.tokens.clone();
        let current = self.network_generation.clone();
        let enabled = self.network_enabled.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if current.load(Ordering::SeqCst) != generation {
                    return;
                }
                let now = now_ms();
                let has_network_token = tokens
                    .records()
                    .iter()
                    .any(|record| record.network && record.is_active(now));
                if !has_network_token {
                    enabled.store(false, Ordering::SeqCst);
                    hosts.write().expect("tunnel hosts poisoned").clear();
                    tunnel.stop().await;
                    return;
                }
                let snapshot = tunnel.snapshot().await;
                let host = (snapshot.status == "running")
                    .then(|| snapshot.public_url.as_deref().and_then(host_of_public_url))
                    .flatten();
                let mut guard = hosts.write().expect("tunnel hosts poisoned");
                guard.clear();
                guard.extend(host);
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn with_tunnel_provider(mut self, tunnel: Arc<dyn TunnelProvider>) -> Self {
        self.tunnel = tunnel;
        self
    }

    pub(crate) fn list_tokens(&self) -> Vec<TokenRecord> {
        self.tokens.records()
    }

    /// 创建令牌。原文只在返回值里出现一次。
    pub(crate) async fn issue_token(
        &self,
        request: IssueTokenRequest,
    ) -> Result<IssuedToken, McpRuntimeError> {
        let control = self.control.lock().await;
        let issued = self
            .tokens
            .issue(request, now_ms())
            .map_err(|error| McpRuntimeError::Token(error.to_string()))?;
        if let Err(error) = self.persist(control.enabled, control.port) {
            // 没能落盘就不能让令牌只活在内存里：撤销后报错。
            self.tokens.revoke(&issued.record.token_id, now_ms());
            return Err(error);
        }
        Ok(issued)
    }

    /// 吊销一个令牌，并取消它所有待审批。返回是否存在该令牌。
    pub(crate) async fn revoke(
        &self,
        state: &ApiState,
        token_id: &str,
    ) -> Result<bool, McpRuntimeError> {
        let control = self.control.lock().await;
        let revoked = self.tokens.revoke(token_id, now_ms());
        cancel_pending(state, token_id);
        self.persist(control.enabled, control.port)?;
        Ok(revoked)
    }

    pub(crate) async fn revoke_all(&self, state: &ApiState) -> Result<usize, McpRuntimeError> {
        let control = self.control.lock().await;
        let ids = self
            .tokens
            .records()
            .into_iter()
            .map(|record| record.token_id)
            .collect::<Vec<_>>();
        let count = self.tokens.revoke_all(now_ms());
        for id in &ids {
            cancel_pending(state, id);
        }
        self.persist(control.enabled, control.port)?;
        Ok(count)
    }

    /// 工作区被移除时，其下令牌一并吊销。
    pub(crate) async fn revoke_workspace(
        &self,
        state: &ApiState,
        workspace_id: &str,
    ) -> Result<usize, McpRuntimeError> {
        let control = self.control.lock().await;
        let ids = self
            .tokens
            .records()
            .into_iter()
            .filter(|record| record.workspace_id == workspace_id)
            .map(|record| record.token_id)
            .collect::<Vec<_>>();
        let count = self.tokens.revoke_workspace(workspace_id, now_ms());
        for id in &ids {
            cancel_pending(state, id);
        }
        self.persist(control.enabled, control.port)?;
        Ok(count)
    }
}

fn cancel_pending(state: &ApiState, token_id: &str) {
    cancel_external_approvals(
        state.conversation_registry.tool_approvals(),
        &external_session_id(token_id),
        token_id,
    );
}

/// GPT Web 槽位端点的文件位置（与令牌端点不同的 socket）。
pub(crate) fn slot_endpoint_for(state_root: &std::path::Path) -> PathBuf {
    local_endpoint_for(&state_root.join("web-slot"))
}

/// 用户私有权限的原子写：先写同目录临时文件再改名。
fn write_private_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_mcp_server::{AttributionMode, Profile};
    use magi_session_store::SessionStore;
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;

    fn state() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(16));
        let governance = Arc::new(GovernanceService::default());
        let mut registry = ToolRegistry::new(governance.clone(), event_bus.clone());
        registry.register_default_builtins();
        ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(registry)
    }

    fn request() -> IssueTokenRequest {
        IssueTokenRequest {
            client_name: "cursor".to_string(),
            workspace_id: "ws".to_string(),
            profile: Profile::ReadOnly,
            attribution: AttributionMode::External,
            ttl_ms: None,
            network: false,
        }
    }

    async fn post(url: &str, secret: Option<&str>) -> Result<u16, reqwest::Error> {
        let mut builder = reqwest::Client::new().post(url).json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"} }
        }));
        if let Some(secret) = secret {
            builder = builder.bearer_auth(secret);
        }
        Ok(builder.send().await?.status().as_u16())
    }

    #[tokio::test]
    async fn enable_serves_authenticated_requests_and_disable_stops_listening() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = McpServiceRuntime::load(dir.path().join("mcp-server.json"));
        let state = state();
        let issued = runtime.issue_token(request()).await.unwrap();

        let status = runtime.set_enabled(&state, true).await.unwrap();
        assert!(status.running && status.enabled);
        let url = status.url.clone().unwrap();
        assert!(url.starts_with("http://127.0.0.1:"));
        assert_eq!(post(&url, Some(&issued.secret)).await.unwrap(), 200);
        assert_eq!(post(&url, None).await.unwrap(), 401);
        assert_eq!(post(&url, Some("magi_mcp_wrong")).await.unwrap(), 401);

        assert!(
            runtime
                .revoke(&state, &issued.record.token_id)
                .await
                .unwrap()
        );
        assert_eq!(post(&url, Some(&issued.secret)).await.unwrap(), 401);

        let status = runtime.set_enabled(&state, false).await.unwrap();
        assert!(!status.running && !status.enabled);
        assert!(post(&url, None).await.is_err(), "关闭后端口不应再可达");
    }

    #[tokio::test]
    async fn tokens_port_and_switch_survive_restart_without_persisting_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let state = state();

        let first = McpServiceRuntime::load(path.clone());
        let issued = first.issue_token(request()).await.unwrap();
        let status = first.set_enabled(&state, true).await.unwrap();
        let port = status.port.unwrap();
        first.stop_locked(&mut *first.control.lock().await).await;

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains(&issued.secret), "令牌原文不得落盘");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "配置文件必须是用户私有权限");
        }

        // 并行测试里其它用例可能恰好拿走了刚释放的端口；短暂重试，避免把端口竞态当成失败。
        let second = McpServiceRuntime::load(path);
        for _ in 0..20 {
            second.start_if_enabled(&state).await;
            if second.status().await.running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let status = second.status().await;
        assert!(status.running);
        assert_eq!(status.port, Some(port), "端口必须稳定");
        assert_eq!(
            post(&status.url.unwrap(), Some(&issued.secret))
                .await
                .unwrap(),
            200,
            "重启后原有令牌仍然可用"
        );
        second.set_enabled(&state, false).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_config_is_treated_as_unconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let runtime = McpServiceRuntime::load(path);
        assert!(runtime.list_tokens().is_empty());
        let status = runtime.status().await;
        assert!(!status.enabled && !status.running);
    }

    #[tokio::test]
    async fn occupied_port_is_reported_and_service_stays_off() {
        let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = blocker.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        std::fs::write(
            &path,
            format!(r#"{{"enabled":true,"port":{port},"tokens":[]}}"#),
        )
        .unwrap();
        let runtime = McpServiceRuntime::load(path);
        let state = state();

        assert!(matches!(
            runtime.set_enabled(&state, true).await,
            Err(McpRuntimeError::Bind { .. })
        ));
        assert!(!runtime.status().await.running);
    }

    #[tokio::test]
    async fn revoking_a_workspace_revokes_only_its_tokens() {
        let runtime = McpServiceRuntime::in_memory();
        let state = state();
        let mine = runtime.issue_token(request()).await.unwrap();
        let mut other = request();
        other.workspace_id = "other".to_string();
        let other = runtime.issue_token(other).await.unwrap();

        assert_eq!(runtime.revoke_workspace(&state, "ws").await.unwrap(), 1);
        let now = now_ms();
        let active = |id: &str| {
            runtime
                .list_tokens()
                .iter()
                .find(|record| record.token_id == id)
                .is_some_and(|record| record.is_active(now))
        };
        assert!(!active(&mine.record.token_id));
        assert!(active(&other.record.token_id));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_endpoint_is_served_while_running_and_removed_after_stop() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let runtime = McpServiceRuntime::load(dir.path().join("mcp-server.json"));
        let state = state();
        let issued = runtime.issue_token(request()).await.unwrap();
        let status = runtime.set_enabled(&state, true).await.unwrap();
        let endpoint = std::path::PathBuf::from(status.stdio_endpoint.clone().expect("stdio 端点"));

        let stream = tokio::net::UnixStream::connect(&endpoint).await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        write
            .write_all(format!("{{\"magiMcpToken\":\"{}\"}}\n", issued.secret).as_bytes())
            .await
            .unwrap();
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        let line = BufReader::new(read)
            .lines()
            .next_line()
            .await
            .unwrap()
            .unwrap();
        assert!(line.contains("magi.fs.read"), "{line}");

        runtime.set_enabled(&state, false).await.unwrap();
        assert!(!endpoint.exists(), "停止后本地 socket 文件应被清理");
    }

    // ── 网络模式（Quick Tunnel）─────────────────────────────────────────────────

    const TUNNEL_HOST: &str = "quick-demo.trycloudflare.com";

    fn network_request(profile: Profile) -> IssueTokenRequest {
        IssueTokenRequest {
            client_name: "remote".to_string(),
            workspace_id: "ws".to_string(),
            profile,
            attribution: AttributionMode::External,
            ttl_ms: None,
            network: true,
        }
    }

    /// 像隧道那样访问 MCP 端口：`Host` 是隧道主机名，来源在 `CF-Connecting-IP`。
    async fn via_tunnel(local_url: &str, secret: &str) -> u16 {
        reqwest::Client::new()
            .post(local_url)
            .header("host", TUNNEL_HOST)
            .header("cf-connecting-ip", "203.0.113.7")
            .bearer_auth(secret)
            .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    #[tokio::test]
    async fn network_mode_needs_a_network_token_and_serves_only_network_tokens() {
        let fake = crate::mcp_tunnel::fake::FakeTunnel::new(&format!("https://{TUNNEL_HOST}"));
        let runtime = McpServiceRuntime::in_memory().with_tunnel_provider(fake.clone());
        let state = state();
        let local = runtime.issue_token(request()).await.unwrap();

        // 只有本机令牌时不能开启。
        assert!(matches!(
            runtime.set_network(&state, true).await,
            Err(McpRuntimeError::Token(_))
        ));
        assert!(fake.started_ports.lock().unwrap().is_empty());

        let remote = runtime
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();
        let status = runtime.set_network(&state, true).await.unwrap();
        assert!(
            status.network.enabled && status.running,
            "网络模式会拉起本机服务"
        );
        assert_eq!(
            fake.started_ports.lock().unwrap().as_slice(),
            &[status.port.unwrap()]
        );

        let local_url = status.url.clone().unwrap();
        // 等监视器把隧道主机名登记进 HTTP 入口。
        for _ in 0..100 {
            if via_tunnel(&local_url, &remote.secret).await == 200 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(via_tunnel(&local_url, &remote.secret).await, 200);
        assert_eq!(
            via_tunnel(&local_url, &local.secret).await,
            401,
            "本机令牌经隧道不可用"
        );
        assert_eq!(
            post(&local_url, Some(&local.secret)).await.unwrap(),
            200,
            "本机令牌走回环仍可用"
        );

        let status = runtime.status().await;
        assert_eq!(status.network.status, "running");
        assert_eq!(
            status.network.mcp_url.as_deref(),
            Some(format!("https://{TUNNEL_HOST}/mcp").as_str())
        );
        runtime.set_enabled(&state, false).await.unwrap();
    }

    #[tokio::test]
    async fn revoking_the_last_network_token_closes_the_tunnel() {
        let fake = crate::mcp_tunnel::fake::FakeTunnel::new(&format!("https://{TUNNEL_HOST}"));
        let runtime = McpServiceRuntime::in_memory().with_tunnel_provider(fake.clone());
        let state = state();
        let remote = runtime
            .issue_token(network_request(Profile::ReadOnly))
            .await
            .unwrap();
        let status = runtime.set_network(&state, true).await.unwrap();
        let local_url = status.url.unwrap();
        for _ in 0..100 {
            if via_tunnel(&local_url, &remote.secret).await == 200 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        runtime
            .revoke(&state, &remote.record.token_id)
            .await
            .unwrap();
        for _ in 0..100 {
            if !runtime.status().await.network.enabled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let status = runtime.status().await;
        assert!(
            !status.network.enabled,
            "最后一个网络令牌被吊销后网络模式应自动关闭"
        );
        assert_eq!(fake.snapshot_status().await, "stopped");
        assert_eq!(
            via_tunnel(&local_url, &remote.secret).await,
            403,
            "隧道主机名应已被清空"
        );
        runtime.set_enabled(&state, false).await.unwrap();
    }

    #[tokio::test]
    async fn disabling_network_or_the_service_clears_the_tunnel_and_nothing_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let fake = crate::mcp_tunnel::fake::FakeTunnel::new(&format!("https://{TUNNEL_HOST}"));
        let runtime = McpServiceRuntime::load(path.clone()).with_tunnel_provider(fake.clone());
        let state = state();
        let remote = runtime
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();

        let status = runtime.set_network(&state, true).await.unwrap();
        let local_url = status.url.unwrap();
        for _ in 0..100 {
            if via_tunnel(&local_url, &remote.secret).await == 200 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let status = runtime.set_network(&state, false).await.unwrap();
        assert!(!status.network.enabled);
        assert_eq!(via_tunnel(&local_url, &remote.secret).await, 403);
        assert!(status.running, "只关网络模式，本机服务保持运行");

        runtime.set_network(&state, true).await.unwrap();
        runtime.set_enabled(&state, false).await.unwrap();
        assert!(
            !runtime.status().await.network.enabled,
            "停止服务必须同时关闭网络模式"
        );

        // 网络模式不持久化：重启后关闭。
        let restarted = McpServiceRuntime::load(path);
        assert!(!restarted.status().await.network.enabled);
    }
}
