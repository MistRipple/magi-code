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
use magi_mcp_server::{
    IssueTokenRequest, IssuedToken, Profile, TokenPatch, TokenRecord, TokenStore,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::mcp_direct::{normalize_public_hosts, parse_bind_host};
use crate::mcp_service::{build_mcp_server, external_session_id};
use crate::mcp_tunnel::{
    NamedTunnel, QuickTunnelProvider, TunnelProvider, host_of_public_url, parse_named_tunnel,
};
use crate::state::ApiState;

const LOOPBACK: &str = "127.0.0.1";

/// 令牌原文文件名（与 `mcp-server.json` 同目录）。
const NAMED_TUNNEL_FILE: &str = "mcp-named-tunnel.json";
const SECRETS_FILE: &str = "mcp-token-secrets.json";

#[derive(Debug, thiserror::Error)]
pub(crate) enum McpRuntimeError {
    #[error("无法监听 {host}:{port}（端口被占用，或该地址不属于本机？）：{reason}")]
    Bind {
        host: String,
        port: u16,
        reason: String,
    },
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
    /// 网络模式上次是否开启。只对命名隧道有意义（固定域名），重启后据此恢复；Quick Tunnel 永不恢复。
    #[serde(default)]
    network: bool,
    /// 直接对公网开放（监听公网地址）的配置；开着时重启后恢复。
    #[serde(default)]
    direct: DirectAccess,
}

/// 直接对公网开放：HTTP 入口改为监听 `bind_host`（如 `0.0.0.0`），不经隧道。
///
/// 来自非回环地址的连接一律按网络请求处理（只接受允许网络使用的令牌、限流与失败退避、
/// `Host` 必须是 `public_hosts` 之一）。传输是明文 HTTP，令牌在网络上不加密；
/// 需要加密时在前面放一个终止 TLS 的反向代理。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectAccess {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
    /// 客户端实际使用的公网域名 / IP（不含端口），同时是 `Host` 白名单。
    #[serde(default)]
    pub public_hosts: Vec<String>,
}

fn default_bind_host() -> String {
    "0.0.0.0".to_string()
}

impl Default for DirectAccess {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_host: default_bind_host(),
            public_hosts: Vec::new(),
        }
    }
}

/// 设置页提交的直连配置。
pub(crate) struct DirectAccessRequest {
    pub enabled: bool,
    pub bind_host: String,
    pub public_hosts: Vec<String>,
    /// 固定监听端口（防火墙需要放行的端口）；缺省沿用当前端口。
    pub port: Option<u16>,
}

/// 直连状态（对外）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectStatus {
    pub enabled: bool,
    /// 是否正在监听公网地址。
    pub listening: bool,
    pub bind_host: String,
    pub public_hosts: Vec<String>,
    /// 客户端配置用的完整 MCP 地址（第一个公网地址）。
    pub mcp_url: Option<String>,
    /// 恢复监听失败等原因；已退回仅本机。
    pub error: Option<String>,
}

struct Running {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    /// stdio 中继连接的本地 socket；只要服务在运行就一起提供。
    local_socket: Option<LocalSocketServer>,
}

#[derive(Default)]
struct Control {
    enabled: bool,
    port: Option<u16>,
    running: Option<Running>,
    /// 启动时是否需要恢复网络模式（上次开着，且配置了命名隧道）。
    restore_network: bool,
}

/// 网络模式（公网隧道）状态。默认 Quick Tunnel：**公网地址每次启动都会变**；配置命名隧道后地址固定。
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
    /// `quick`（随机地址，重启即失效）或 `named`（自己的域名，地址固定）。
    pub mode: String,
    /// 已配置的命名隧道域名（令牌不外露）。
    pub named_hostname: Option<String>,
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
    pub direct: DirectStatus,
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
    #[cfg(test)]
    pub(crate) fn recent(&self, limit: usize, token_id: Option<&str>) -> Vec<AuditEntry> {
        self.page(limit, 0, token_id).0
    }

    /// 分页读取（最新的在前），同时返回符合过滤条件的总条数。
    pub(crate) fn page(
        &self,
        limit: usize,
        offset: usize,
        token_id: Option<&str>,
    ) -> (Vec<AuditEntry>, usize) {
        let entries = self.entries.lock().expect("mcp audit poisoned");
        let matching = entries
            .iter()
            .rev()
            .filter(|entry| token_id.is_none_or(|id| entry.token_id == id));
        let total = matching.clone().count();
        (matching.skip(offset).take(limit).cloned().collect(), total)
    }

    /// 清理记录：不带令牌则清空全部，否则只清理该令牌的。返回清理的条数。
    /// 文件按剩余内容整体原子重写，不会留下已清理的记录。
    pub(crate) fn clear(&self, token_id: Option<&str>) -> std::io::Result<usize> {
        let mut entries = self.entries.lock().expect("mcp audit poisoned");
        let before = entries.len();
        entries.retain(|entry| token_id.is_some_and(|id| entry.token_id != id));
        let removed = before - entries.len();
        if removed == 0 {
            return Ok(0);
        }
        *self
            .appended_since_compact
            .lock()
            .expect("mcp audit poisoned") = 0;
        let Some(path) = &self.path else {
            return Ok(removed);
        };
        if entries.is_empty() {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        } else {
            let mut body = String::new();
            for kept in entries.iter() {
                if let Ok(line) = serde_json::to_string(kept) {
                    body.push_str(&line);
                    body.push('\n');
                }
            }
            write_private_atomic(path, body.as_bytes())?;
        }
        Ok(removed)
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
    /// 网络模式是否开启。Quick Tunnel 不持久化（每次由用户显式开启）；命名隧道的开关会持久化并在重启后恢复。
    network_enabled: Arc<AtomicBool>,
    /// 命名隧道（域名 + 隧道令牌），与令牌原文同级别保管：`mcp-named-tunnel.json`（0600）。
    named: Mutex<Option<NamedTunnel>>,
    /// 直接对公网开放的配置与最近一次失败原因。
    direct: Mutex<DirectAccess>,
    direct_error: Mutex<Option<String>>,
    /// 与 HTTP 入口共享的主机名表。归运行时所有，重新监听时沿用同一份：
    /// 隧道主机名由网络模式的监视任务维护，公网主机名由直连配置决定。
    tunnel_hosts: Arc<RwLock<Vec<String>>>,
    public_hosts: Arc<RwLock<Vec<String>>>,
    network_generation: Arc<AtomicU64>,
    /// GPT Web 槽位端点（本地 socket，无令牌；身份与归属由槽位表决定）。
    slot_socket: AsyncMutex<Option<LocalSocketServer>>,
    path: Option<PathBuf>,
    tokens: Arc<TokenStore>,
    /// 令牌原文（token_id → 原文），与校验用的哈希分开存放在 `mcp-token-secrets.json`（0600），
    /// 只服务于「重新查看 / 复制」，不参与校验，也不进审计、日志和备份导出。
    secrets: Mutex<std::collections::HashMap<String, String>>,
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
            named: Mutex::new(None),
            direct: Mutex::new(DirectAccess::default()),
            direct_error: Mutex::new(None),
            tunnel_hosts: Arc::new(RwLock::new(Vec::new())),
            public_hosts: Arc::new(RwLock::new(Vec::new())),
            network_generation: Arc::new(AtomicU64::new(0)),
            slot_socket: AsyncMutex::new(None),
            path: None,
            tokens: Arc::new(TokenStore::new()),
            secrets: Mutex::new(Default::default()),
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
        // 原文只保留仍然有效的令牌；旧版本创建的令牌没有原文，保持「无法查看」。
        let now = now_ms();
        let active_ids = tokens
            .records()
            .into_iter()
            .filter(|record| record.is_active(now))
            .map(|record| record.token_id)
            .collect::<std::collections::HashSet<_>>();
        let secrets = std::fs::read(path.with_file_name(SECRETS_FILE))
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<std::collections::HashMap<String, String>>(&bytes).ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|(token_id, _)| active_ids.contains(token_id))
            .collect();
        let named = read_named_tunnel(&path.with_file_name(NAMED_TUNNEL_FILE));
        let restore_network = persisted.network && named.is_some();
        let direct = persisted.direct.clone();
        Self {
            audit,
            tunnel: QuickTunnelProvider::shared(),
            network_enabled: Arc::new(AtomicBool::new(false)),
            named: Mutex::new(named),
            direct: Mutex::new(direct),
            direct_error: Mutex::new(None),
            tunnel_hosts: Arc::new(RwLock::new(Vec::new())),
            public_hosts: Arc::new(RwLock::new(Vec::new())),
            network_generation: Arc::new(AtomicU64::new(0)),
            slot_socket: AsyncMutex::new(None),
            path: Some(path),
            tokens,
            secrets: Mutex::new(secrets),
            control: AsyncMutex::new(Control {
                enabled: persisted.enabled,
                port: persisted.port,
                running: None,
                restore_network,
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
            network: self.network_enabled.load(Ordering::SeqCst)
                && self
                    .named
                    .lock()
                    .expect("mcp named lock poisoned")
                    .is_some(),
            direct: self
                .direct
                .lock()
                .expect("mcp direct lock poisoned")
                .clone(),
        })
        .map_err(|error| McpRuntimeError::Persist(error.to_string()))?;
        write_private_atomic(path, &body)
            .map_err(|error| McpRuntimeError::Persist(error.to_string()))
    }

    fn persist_secrets(&self) -> Result<(), McpRuntimeError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let body = {
            let secrets = self.secrets.lock().expect("mcp secrets lock poisoned");
            serde_json::to_vec_pretty(&*secrets)
                .map_err(|error| McpRuntimeError::Persist(error.to_string()))?
        };
        write_private_atomic(&path.with_file_name(SECRETS_FILE), &body)
            .map_err(|error| McpRuntimeError::Persist(error.to_string()))
    }

    /// 忘掉这些令牌的原文（吊销后不可再查看）。
    fn forget_secrets(&self, token_ids: &[String]) {
        {
            let mut secrets = self.secrets.lock().expect("mcp secrets lock poisoned");
            for token_id in token_ids {
                secrets.remove(token_id);
            }
        }
        if let Err(error) = self.persist_secrets() {
            tracing::warn!(%error, "清理已吊销令牌的原文失败");
        }
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
        let named_hostname = self
            .named
            .lock()
            .expect("mcp named lock poisoned")
            .as_ref()
            .map(|named| named.hostname.clone());
        let mcp_url = snapshot
            .public_url
            .as_ref()
            .map(|url| format!("{}/mcp", url.trim_end_matches('/')));
        McpServiceStatus {
            enabled: control.enabled,
            running: control.running.is_some(),
            port: addr.map(|addr| addr.port()).or(control.port),
            // 本机地址始终是回环：监听 0.0.0.0 时也不把它当作客户端地址展示。
            url: addr.map(|addr| format!("http://{LOOPBACK}:{}/mcp", addr.port())),
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
            direct: self.direct_status_locked(control),
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
                mode: if named_hostname.is_some() {
                    "named"
                } else {
                    "quick"
                }
                .to_string(),
                named_hostname,
            },
        }
    }

    fn direct_status_locked(&self, control: &Control) -> DirectStatus {
        let direct = self
            .direct
            .lock()
            .expect("mcp direct lock poisoned")
            .clone();
        let port = control
            .running
            .as_ref()
            .map(|running| running.addr.port())
            .or(control.port);
        let listening = direct.enabled && control.running.is_some();
        DirectStatus {
            enabled: direct.enabled,
            listening,
            mcp_url: listening
                .then(|| {
                    direct
                        .public_hosts
                        .first()
                        .zip(port)
                        .map(|(host, port)| format!("http://{host}:{port}/mcp"))
                })
                .flatten(),
            bind_host: direct.bind_host,
            public_hosts: direct.public_hosts,
            error: self
                .direct_error
                .lock()
                .expect("mcp direct lock poisoned")
                .clone(),
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
    ///
    /// 直接对公网开放按上次的配置恢复监听；没有有效的网络令牌，或公网地址监听失败时
    /// 退回仅本机（失败原因留在状态里），不会因此让本机服务起不来。
    pub(crate) async fn start_if_enabled(&self, state: &ApiState) {
        let mut control = self.control.lock().await;
        {
            let mut direct = self.direct.lock().expect("mcp direct lock poisoned");
            if direct.enabled && !self.has_active_network_token() {
                direct.enabled = false;
            }
        }
        let direct_enabled = self
            .direct
            .lock()
            .expect("mcp direct lock poisoned")
            .enabled;
        if (control.enabled || direct_enabled) && control.running.is_none() {
            if let Err(error) = self.start_locked(state, &mut control).await {
                tracing::warn!(%error, "MCP 服务启动失败，保持关闭");
                if direct_enabled {
                    self.direct
                        .lock()
                        .expect("mcp direct lock poisoned")
                        .enabled = false;
                    *self.direct_error.lock().expect("mcp direct lock poisoned") =
                        Some(error.to_string());
                    if control.enabled
                        && let Err(error) = self.start_locked(state, &mut control).await
                    {
                        tracing::warn!(%error, "MCP 服务启动失败，保持关闭");
                    }
                }
            }
        }
        // 命名隧道的域名是固定的：上次开着网络模式就恢复，客户端配置无需改动。
        if control.restore_network {
            control.restore_network = false;
            if self.has_active_network_token()
                && let Err(error) = self.enable_network_locked(state, &mut control).await
            {
                tracing::warn!(%error, "MCP 网络模式恢复失败，保持关闭");
            }
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
        let direct = self
            .direct
            .lock()
            .expect("mcp direct lock poisoned")
            .clone();
        // 默认只监听回环；直接对公网开放时监听用户指定的地址（如 0.0.0.0）。
        let bind_host = if direct.enabled {
            direct.bind_host.clone()
        } else {
            LOOPBACK.to_string()
        };
        let bind_ip = parse_bind_host(&bind_host).map_err(McpRuntimeError::Token)?;
        let bind_error = |error: std::io::Error| McpRuntimeError::Bind {
            host: bind_host.clone(),
            port,
            reason: error.to_string(),
        };
        let listener = TcpListener::bind((bind_ip, port))
            .await
            .map_err(&bind_error)?;
        let addr = listener.local_addr().map_err(&bind_error)?;
        control.port = Some(addr.port());

        let server = build_mcp_server(state.clone(), self.tokens.clone());
        let mut http_state =
            HttpState::new(server.clone(), self.tokens.clone(), HttpConfig::default());
        http_state.tunnel_hosts = self.tunnel_hosts.clone();
        http_state.public_hosts = self.public_hosts.clone();
        *self.public_hosts.write().expect("public hosts poisoned") = if direct.enabled {
            direct.public_hosts.clone()
        } else {
            Vec::new()
        };
        let app = router(http_state);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
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
        });
        Ok(())
    }

    async fn stop_locked(&self, control: &mut Control) {
        // 服务停止意味着隧道没有东西可转发：先关网络模式。
        self.disable_network_locked().await;
        self.stop_listeners_locked(control).await;
    }

    /// 只关闭监听（HTTP 与 stdio 本地 socket），不动网络模式；重新监听前使用。
    async fn stop_listeners_locked(&self, control: &mut Control) {
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
                host: "stdio".to_string(),
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
            self.enable_network_locked(state, &mut control).await?;
        } else {
            self.disable_network_locked().await;
            self.persist(control.enabled, control.port)?;
        }
        Ok(self.status_locked(&control).await)
    }

    /// 启动隧道并记录开关。网络模式依赖本机服务在运行，但不改变用户的“启用”开关。
    async fn enable_network_locked(
        &self,
        state: &ApiState,
        control: &mut Control,
    ) -> Result<(), McpRuntimeError> {
        self.start_locked(state, control).await?;
        let port = control.port.expect("服务运行时端口已确定");
        let hosts = self.tunnel_hosts.clone();
        let named = self.named.lock().expect("mcp named lock poisoned").clone();
        self.tunnel.start(port, named).await;
        self.network_enabled.store(true, Ordering::SeqCst);
        let generation = self.network_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.spawn_network_watcher(hosts, generation);
        self.persist(control.enabled, control.port)
    }

    /// 设置直接对公网开放。开启需要至少一个允许网络使用的令牌；监听失败（端口被占用、
    /// 地址不属于本机、没有权限）时回到原来的配置并报错。
    pub(crate) async fn set_direct_access(
        &self,
        state: &ApiState,
        request: DirectAccessRequest,
    ) -> Result<McpServiceStatus, McpRuntimeError> {
        let next = if request.enabled {
            if !self.has_active_network_token() {
                return Err(McpRuntimeError::Token(
                    "需要至少一个有效且允许网络访问的令牌才能直接对公网开放".to_string(),
                ));
            }
            parse_bind_host(&request.bind_host).map_err(McpRuntimeError::Token)?;
            DirectAccess {
                enabled: true,
                bind_host: request
                    .bind_host
                    .trim()
                    .trim_matches(['[', ']'])
                    .to_string(),
                public_hosts: normalize_public_hosts(&request.public_hosts)
                    .map_err(McpRuntimeError::Token)?,
            }
        } else {
            DirectAccess {
                enabled: false,
                ..self
                    .direct
                    .lock()
                    .expect("mcp direct lock poisoned")
                    .clone()
            }
        };
        let mut control = self.control.lock().await;
        if request.enabled
            && request.port.is_some()
            && request.port != control.port
            && self.network_enabled.load(Ordering::SeqCst)
        {
            return Err(McpRuntimeError::Token(
                "网络模式（隧道）开启时不能修改端口，请先关闭网络模式".to_string(),
            ));
        }
        let previous = self
            .direct
            .lock()
            .expect("mcp direct lock poisoned")
            .clone();
        let previous_port = control.port;
        let was_running = control.running.is_some();
        if request.enabled
            && let Some(port) = request.port
        {
            control.port = Some(port);
        }
        *self.direct.lock().expect("mcp direct lock poisoned") = next.clone();
        if was_running || next.enabled {
            self.stop_listeners_locked(&mut control).await;
            if let Err(error) = self.start_locked(state, &mut control).await {
                // 回到原来的配置；原来在运行的就恢复运行。
                *self.direct.lock().expect("mcp direct lock poisoned") = previous;
                control.port = previous_port;
                if was_running {
                    let _ = self.start_locked(state, &mut control).await;
                }
                return Err(error);
            }
        }
        *self.direct_error.lock().expect("mcp direct lock poisoned") = None;
        // 关闭直连后，如果服务只是为它而运行（用户没启用、也没开网络模式），一并停掉。
        if !next.enabled && !control.enabled && !self.network_enabled.load(Ordering::SeqCst) {
            self.stop_listeners_locked(&mut control).await;
        }
        self.persist(control.enabled, control.port)?;
        Ok(self.status_locked(&control).await)
    }

    /// 最后一个允许网络使用的令牌没了：直连不再有人能用，立即收回公网监听。
    async fn close_direct_if_unused(&self, state: &ApiState, control: &mut Control) {
        let enabled = self
            .direct
            .lock()
            .expect("mcp direct lock poisoned")
            .enabled;
        if !enabled || self.has_active_network_token() {
            return;
        }
        self.direct
            .lock()
            .expect("mcp direct lock poisoned")
            .enabled = false;
        self.stop_listeners_locked(control).await;
        if control.enabled || self.network_enabled.load(Ordering::SeqCst) {
            let _ = self.start_locked(state, control).await;
        }
        let _ = self.persist(control.enabled, control.port);
    }

    /// 保存命名隧道配置（域名 + 隧道令牌）。网络模式正在运行时立即切换到这条隧道。
    pub(crate) async fn set_named_tunnel(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<McpServiceStatus, McpRuntimeError> {
        let named = parse_named_tunnel(hostname, token).map_err(McpRuntimeError::Token)?;
        let control = self.control.lock().await;
        if let Some(root) = self.state_root() {
            let body = serde_json::to_vec_pretty(&PersistedNamedTunnel {
                hostname: named.hostname.clone(),
                token: named.token.clone(),
            })
            .map_err(|error| McpRuntimeError::Persist(error.to_string()))?;
            write_private_atomic(&root.join(NAMED_TUNNEL_FILE), &body)
                .map_err(|error| McpRuntimeError::Persist(error.to_string()))?;
        }
        *self.named.lock().expect("mcp named lock poisoned") = Some(named.clone());
        if self.network_enabled.load(Ordering::SeqCst) {
            self.tunnel.stop().await;
            if let Some(port) = control.port {
                self.tunnel.start(port, Some(named)).await;
            }
        }
        self.persist(control.enabled, control.port)?;
        Ok(self.status_locked(&control).await)
    }

    /// 删除命名隧道配置。正在用它的网络模式随之关闭（不会悄悄退回随机地址）。
    pub(crate) async fn clear_named_tunnel(&self) -> Result<McpServiceStatus, McpRuntimeError> {
        let control = self.control.lock().await;
        if let Some(root) = self.state_root() {
            match std::fs::remove_file(root.join(NAMED_TUNNEL_FILE)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(McpRuntimeError::Persist(error.to_string())),
            }
        }
        let was_named = self
            .named
            .lock()
            .expect("mcp named lock poisoned")
            .take()
            .is_some();
        if was_named && self.network_enabled.load(Ordering::SeqCst) {
            self.disable_network_locked().await;
        }
        self.persist(control.enabled, control.port)?;
        Ok(self.status_locked(&control).await)
    }

    /// 命名隧道的公网 MCP 地址（用于连通性检查）。
    pub(crate) fn named_tunnel_hostname(&self) -> Option<String> {
        self.named
            .lock()
            .expect("mcp named lock poisoned")
            .as_ref()
            .map(|named| named.hostname.clone())
    }

    async fn disable_network_locked(&self) {
        self.network_generation.fetch_add(1, Ordering::SeqCst);
        self.network_enabled.store(false, Ordering::SeqCst);
        self.tunnel.stop().await;
        self.tunnel_hosts
            .write()
            .expect("tunnel hosts poisoned")
            .clear();
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
        self.secrets
            .lock()
            .expect("mcp secrets lock poisoned")
            .insert(issued.record.token_id.clone(), issued.secret.clone());
        if let Err(error) = self.persist_secrets() {
            // 原文存不下来用户就没法再查看它：不让这样的令牌生效。
            self.tokens.revoke(&issued.record.token_id, now_ms());
            self.forget_secrets(std::slice::from_ref(&issued.record.token_id));
            let _ = self.persist(control.enabled, control.port);
            return Err(error);
        }
        Ok(issued)
    }

    /// 令牌原文（重新查看 / 复制）。已吊销、已过期、或创建于旧版本而没有原文的令牌返回 `None`。
    pub(crate) fn token_secret(&self, token_id: &str) -> Option<String> {
        let now = now_ms();
        let active = self
            .tokens
            .records()
            .iter()
            .any(|record| record.token_id == token_id && record.is_active(now));
        if !active {
            return None;
        }
        self.secrets
            .lock()
            .expect("mcp secrets lock poisoned")
            .get(token_id)
            .cloned()
    }

    /// 令牌当前是否有可查看的原文（列表用，不返回原文）。
    pub(crate) fn token_has_secret(&self, token_id: &str) -> bool {
        self.secrets
            .lock()
            .expect("mcp secrets lock poisoned")
            .contains_key(token_id)
    }

    /// 编辑令牌：不更换原文，下一次调用起生效。收紧权限时，该令牌挂起的审批立即作废。
    pub(crate) async fn update_token(
        &self,
        state: &ApiState,
        token_id: &str,
        patch: TokenPatch,
    ) -> Result<TokenRecord, McpRuntimeError> {
        let mut control = self.control.lock().await;
        let (before, after) = self
            .tokens
            .update(token_id, patch, now_ms())
            .map_err(|error| McpRuntimeError::Token(error.to_string()))?;
        if let Err(error) = self.persist(control.enabled, control.port) {
            // 没落盘就不能让修改只活在内存里：改回去。
            let _ = self.tokens.update(
                token_id,
                TokenPatch {
                    client_name: Some(before.client_name.clone()),
                    profile: Some(before.profile),
                    expires_at_ms: Some(before.expires_at_ms),
                    network: Some(before.network),
                },
                now_ms(),
            );
            return Err(error);
        }
        if narrows_access(&before, &after) {
            cancel_pending(state, token_id);
        }
        if before.network && !after.network {
            self.close_direct_if_unused(state, &mut control).await;
        }
        Ok(after)
    }

    /// 重新生成原文：旧原文立即失效，其余配置不变，挂起的审批作废。返回新原文。
    pub(crate) async fn rotate_token(
        &self,
        state: &ApiState,
        token_id: &str,
    ) -> Result<(TokenRecord, String), McpRuntimeError> {
        let control = self.control.lock().await;
        let (record, secret) = self
            .tokens
            .rotate(token_id)
            .map_err(|error| McpRuntimeError::Token(error.to_string()))?;
        self.persist(control.enabled, control.port)?;
        self.secrets
            .lock()
            .expect("mcp secrets lock poisoned")
            .insert(token_id.to_string(), secret.clone());
        self.persist_secrets()?;
        cancel_pending(state, token_id);
        Ok((record, secret))
    }

    /// 吊销一个令牌，并取消它所有待审批。返回是否存在该令牌。
    pub(crate) async fn revoke(
        &self,
        state: &ApiState,
        token_id: &str,
    ) -> Result<bool, McpRuntimeError> {
        let mut control = self.control.lock().await;
        let revoked = self.tokens.revoke(token_id, now_ms());
        cancel_pending(state, token_id);
        self.persist(control.enabled, control.port)?;
        self.forget_secrets(&[token_id.to_string()]);
        self.close_direct_if_unused(state, &mut control).await;
        Ok(revoked)
    }

    pub(crate) async fn revoke_all(&self, state: &ApiState) -> Result<usize, McpRuntimeError> {
        let mut control = self.control.lock().await;
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
        self.forget_secrets(&ids);
        self.close_direct_if_unused(state, &mut control).await;
        Ok(count)
    }

    /// 工作区被移除时，其下令牌一并吊销。
    pub(crate) async fn revoke_workspace(
        &self,
        state: &ApiState,
        workspace_id: &str,
    ) -> Result<usize, McpRuntimeError> {
        let mut control = self.control.lock().await;
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
        self.forget_secrets(&ids);
        self.close_direct_if_unused(state, &mut control).await;
        Ok(count)
    }
}

/// 这次编辑有没有收紧该令牌的权限（降权、关闭公网、缩短有效期）。收紧时挂起的审批按旧权限发起，
/// 不能继续等；单纯放宽（例如从只读升到编辑）不需要作废。
fn narrows_access(before: &TokenRecord, after: &TokenRecord) -> bool {
    let profile_narrowed = before.profile != after.profile
        && !matches!(
            (before.profile, after.profile),
            (Profile::ReadOnly, _) | (Profile::Edit, Profile::EditTrusted | Profile::Exec)
        );
    let network_narrowed = before.network && !after.network;
    let expiry_narrowed = match (before.expires_at_ms, after.expires_at_ms) {
        (None, Some(_)) => true,
        (Some(old), Some(new)) => new < old,
        _ => false,
    };
    profile_narrowed || network_narrowed || expiry_narrowed
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

#[derive(Serialize, Deserialize)]
struct PersistedNamedTunnel {
    hostname: String,
    token: String,
}

/// 读取并重新校验命名隧道配置；文件缺失或内容不合法时按“未配置”处理。
fn read_named_tunnel(path: &std::path::Path) -> Option<NamedTunnel> {
    let bytes = std::fs::read(path).ok()?;
    let stored = serde_json::from_slice::<PersistedNamedTunnel>(&bytes).ok()?;
    parse_named_tunnel(&stored.hostname, &stored.token).ok()
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
    #[tokio::test]
    async fn secrets_live_in_their_own_private_file_and_vanish_with_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let state = state();
        let runtime = McpServiceRuntime::load(path.clone());
        let issued = runtime.issue_token(request()).await.unwrap();
        let token_id = issued.record.token_id.clone();

        let secrets_path = dir.path().join(SECRETS_FILE);
        assert!(
            std::fs::read_to_string(&secrets_path)
                .unwrap()
                .contains(&issued.secret)
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&issued.secret)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&secrets_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "原文文件必须是用户私有权限");
        }

        // 重启后仍可重新查看。
        let restarted = McpServiceRuntime::load(path.clone());
        assert_eq!(
            restarted.token_secret(&token_id).as_deref(),
            Some(issued.secret.as_str())
        );

        // 吊销后原文立即清除，文件里也不再有。
        assert!(restarted.revoke(&state, &token_id).await.unwrap());
        assert!(restarted.token_secret(&token_id).is_none());
        assert!(
            !std::fs::read_to_string(&secrets_path)
                .unwrap()
                .contains(&issued.secret)
        );
    }

    #[tokio::test]
    async fn a_token_created_before_secrets_were_kept_cannot_be_viewed_but_can_be_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let state = state();
        let runtime = McpServiceRuntime::load(path.clone());
        let issued = runtime.issue_token(request()).await.unwrap();
        // 模拟旧版本：没有原文文件。
        std::fs::remove_file(dir.path().join(SECRETS_FILE)).unwrap();

        let legacy = McpServiceRuntime::load(path);
        let token_id = issued.record.token_id.clone();
        assert!(!legacy.token_has_secret(&token_id));
        assert!(legacy.token_secret(&token_id).is_none());
        let (_, fresh) = legacy.rotate_token(&state, &token_id).await.unwrap();
        assert_ne!(fresh, issued.secret);
        assert_eq!(legacy.token_secret(&token_id), Some(fresh));
    }

    #[test]
    fn only_narrowing_edits_count_as_narrowing_access() {
        let base = || {
            let store = TokenStore::default();
            store
                .issue(
                    IssueTokenRequest {
                        profile: Profile::Edit,
                        network: true,
                        ttl_ms: Some(10_000),
                        ..request()
                    },
                    0,
                )
                .unwrap()
                .record
        };
        let before = base();
        let mut widened = before.clone();
        widened.profile = Profile::Exec;
        assert!(!narrows_access(&before, &widened), "升权限不取消挂起审批");
        let mut narrowed = before.clone();
        narrowed.profile = Profile::ReadOnly;
        assert!(narrows_access(&before, &narrowed));
        let mut offline = before.clone();
        offline.network = false;
        assert!(narrows_access(&before, &offline));
        let mut shorter = before.clone();
        shorter.expires_at_ms = Some(5_000);
        assert!(narrows_access(&before, &shorter));
        let mut renamed = before.clone();
        renamed.client_name = "other".to_string();
        assert!(!narrows_access(&before, &renamed));
    }
    fn named_token() -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(r#"{"a":"acct","t":"11111111-2222-3333-4444-555555555555","s":"c2VjcmV0"}"#)
    }

    #[tokio::test]
    async fn a_named_tunnel_gives_a_fixed_address_and_is_restored_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let state = state();
        let fake = crate::mcp_tunnel::fake::FakeTunnel::new(&format!("https://{TUNNEL_HOST}"));
        let runtime = McpServiceRuntime::load(path.clone()).with_tunnel_provider(fake.clone());
        runtime
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();

        let status = runtime
            .set_named_tunnel("https://MCP.example.com/", &named_token())
            .await
            .unwrap();
        assert_eq!(status.network.mode, "named");
        assert_eq!(
            status.network.named_hostname.as_deref(),
            Some("mcp.example.com")
        );
        let file = dir.path().join(NAMED_TUNNEL_FILE);
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains(&named_token())
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&named_token()),
            "令牌不进主配置"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(
            !serde_json::to_string(&status)
                .unwrap()
                .contains(&named_token()),
            "状态不外露令牌"
        );

        let status = runtime.set_network(&state, true).await.unwrap();
        assert_eq!(
            status.network.mcp_url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(
            fake.started_named
                .lock()
                .unwrap()
                .last()
                .cloned()
                .flatten()
                .as_deref(),
            Some("mcp.example.com")
        );

        // 模拟 daemon 退出（不经用户操作，开关保持原样），再重启：网络模式随命名隧道自动恢复，地址不变。
        runtime
            .stop_locked(&mut *runtime.control.lock().await)
            .await;
        let fake2 = crate::mcp_tunnel::fake::FakeTunnel::new(&format!("https://{TUNNEL_HOST}"));
        let restarted = McpServiceRuntime::load(path.clone()).with_tunnel_provider(fake2.clone());
        restarted.start_if_enabled(&state).await;
        let status = restarted.status().await;
        assert!(status.network.enabled, "命名隧道开着时重启后应恢复");
        assert_eq!(
            status.network.mcp_url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        restarted
            .stop_locked(&mut *restarted.control.lock().await)
            .await;

        // 用户关闭网络模式后，重启不再恢复。
        runtime.set_network(&state, false).await.unwrap();
        let again = McpServiceRuntime::load(path);
        again.start_if_enabled(&state).await;
        assert!(!again.status().await.network.enabled);
    }

    #[tokio::test]
    async fn quick_tunnels_are_never_restored_and_clearing_a_named_tunnel_closes_network_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let state = state();
        let fake = crate::mcp_tunnel::fake::FakeTunnel::new(&format!("https://{TUNNEL_HOST}"));
        let runtime = McpServiceRuntime::load(path.clone()).with_tunnel_provider(fake.clone());
        runtime
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();

        // Quick：开了也不持久化。
        let status = runtime.set_network(&state, true).await.unwrap();
        assert_eq!(status.network.mode, "quick");
        let restarted = McpServiceRuntime::load(path.clone());
        restarted.start_if_enabled(&state).await;
        assert!(!restarted.status().await.network.enabled);
        runtime.set_network(&state, false).await.unwrap();

        // 运行中保存命名隧道：立即切换到固定域名；删除后网络模式关闭，不悄悄退回随机地址。
        runtime.set_network(&state, true).await.unwrap();
        runtime
            .set_named_tunnel("mcp.example.com", &named_token())
            .await
            .unwrap();
        assert_eq!(
            runtime.status().await.network.mcp_url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        let status = runtime.clear_named_tunnel().await.unwrap();
        assert!(!status.network.enabled);
        assert_eq!(status.network.mode, "quick");
        assert!(!dir.path().join(NAMED_TUNNEL_FILE).exists());
        runtime.set_enabled(&state, false).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_named_tunnel_input_is_rejected_and_nothing_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = McpServiceRuntime::load(dir.path().join("mcp-server.json"));
        assert!(
            runtime
                .set_named_tunnel("mcp.example.com", "oops")
                .await
                .is_err()
        );
        assert!(
            runtime
                .set_named_tunnel("localhost", &named_token())
                .await
                .is_err()
        );
        assert!(!dir.path().join(NAMED_TUNNEL_FILE).exists());
        assert_eq!(runtime.status().await.network.mode, "quick");
    }
    #[test]
    fn audit_pages_newest_first_and_clearing_rewrites_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-audit.jsonl");
        let log = McpAuditLog::load(path.clone());
        let entry = |token: &str, at_ms: u64| AuditEntry {
            token_id: token.to_string(),
            client_name: token.to_string(),
            workspace_id: "ws".to_string(),
            tool: "magi.fs.read".to_string(),
            requires_approval: false,
            paths: vec![],
            outcome: "succeeded".to_string(),
            detail: None,
            at_ms,
        };
        for at in 1..=7 {
            log.record(entry(if at % 2 == 0 { "b" } else { "a" }, at));
        }
        let (first, total) = log.page(3, 0, None);
        assert_eq!(total, 7);
        assert_eq!(first.iter().map(|e| e.at_ms).collect::<Vec<_>>(), [7, 6, 5]);
        let (second, _) = log.page(3, 3, None);
        assert_eq!(
            second.iter().map(|e| e.at_ms).collect::<Vec<_>>(),
            [4, 3, 2]
        );
        let (only_b, total_b) = log.page(10, 0, Some("b"));
        assert_eq!((only_b.len(), total_b), (3, 3));
        assert!(log.page(3, 100, None).0.is_empty());

        // 只清理一个令牌：另一个保留，文件里也不再有被清理的记录。
        assert_eq!(log.clear(Some("b")).unwrap(), 3);
        assert_eq!(log.page(10, 0, None).1, 4);
        let reloaded = McpAuditLog::load(path.clone());
        assert_eq!(reloaded.page(10, 0, None).1, 4);
        assert!(reloaded.page(10, 0, Some("b")).0.is_empty());

        assert_eq!(log.clear(None).unwrap(), 4);
        assert!(!path.exists(), "清空后不留文件");
        assert_eq!(log.clear(None).unwrap(), 0);
        assert_eq!(McpAuditLog::load(path).page(10, 0, None).1, 0);
    }
    async fn call_with_host(url: &str, host: &str, secret: &str) -> u16 {
        reqwest::Client::new()
            .post(url)
            .header("host", host)
            .bearer_auth(secret)
            .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    fn direct_request(hosts: &[&str]) -> DirectAccessRequest {
        DirectAccessRequest {
            enabled: true,
            // 测试里监听回环：对端是回环地址时，公网主机名按“同机反向代理”处理，
            // 与真实公网对端走同一套网络请求规则（对端判定本身在 HTTP 层测试里覆盖）。
            bind_host: "127.0.0.1".to_string(),
            public_hosts: hosts.iter().map(|host| host.to_string()).collect(),
            port: None,
        }
    }

    #[tokio::test]
    async fn direct_access_needs_a_network_token_and_serves_only_network_tokens_by_public_host() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = McpServiceRuntime::load(dir.path().join("mcp-server.json"));
        let state = state();
        let local = runtime.issue_token(request()).await.unwrap();

        // 没有允许网络使用的令牌：拒绝开启，也不会开始监听。
        assert!(matches!(
            runtime
                .set_direct_access(&state, direct_request(&["mcp.example.com"]))
                .await,
            Err(McpRuntimeError::Token(_))
        ));
        assert!(!runtime.status().await.running);

        let remote = runtime
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();
        assert!(
            runtime
                .set_direct_access(&state, direct_request(&[]))
                .await
                .is_err(),
            "必须填写公网地址"
        );
        let status = runtime
            .set_direct_access(&state, direct_request(&["http://MCP.example.com:9/x"]))
            .await
            .unwrap();
        assert!(status.running && status.direct.listening);
        assert_eq!(status.direct.public_hosts, ["mcp.example.com"]);
        let port = status.port.unwrap();
        assert_eq!(
            status.direct.mcp_url.as_deref(),
            Some(format!("http://mcp.example.com:{port}/mcp").as_str())
        );
        // 客户端展示的本机地址仍是回环。
        assert_eq!(
            status.url.as_deref(),
            Some(format!("http://127.0.0.1:{port}/mcp").as_str())
        );

        let url = status.url.unwrap();
        assert_eq!(
            call_with_host(&url, "mcp.example.com", &remote.secret).await,
            200
        );
        assert_eq!(
            call_with_host(&url, "mcp.example.com", &local.secret).await,
            401,
            "只在本机使用的令牌不能经公网地址使用"
        );
        assert_eq!(
            call_with_host(&url, "other.example.com", &remote.secret).await,
            403
        );
        assert_eq!(call_with_host(&url, "127.0.0.1", &local.secret).await, 200);

        // 关闭后公网主机名立即失效，服务按用户的“启用”开关决定是否继续运行。
        let status = runtime
            .set_direct_access(
                &state,
                DirectAccessRequest {
                    enabled: false,
                    ..direct_request(&[])
                },
            )
            .await
            .unwrap();
        assert!(!status.direct.enabled && !status.running);
    }

    #[tokio::test]
    async fn direct_access_survives_restart_and_closes_with_the_last_network_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-server.json");
        let state = state();
        let first = McpServiceRuntime::load(path.clone());
        let remote = first
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();
        first
            .set_direct_access(&state, direct_request(&["203.0.113.5"]))
            .await
            .unwrap();
        first.stop_locked(&mut *first.control.lock().await).await;

        let second = McpServiceRuntime::load(path.clone());
        for _ in 0..20 {
            second.start_if_enabled(&state).await;
            if second.status().await.direct.listening {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let status = second.status().await;
        assert!(status.direct.listening, "重启后应按上次配置恢复监听");
        assert_eq!(status.direct.public_hosts, ["203.0.113.5"]);
        assert_eq!(
            call_with_host(&status.url.clone().unwrap(), "203.0.113.5", &remote.secret).await,
            200
        );

        // 吊销最后一个网络令牌：直连立即收回，也不会再恢复。
        second
            .revoke(&state, &remote.record.token_id)
            .await
            .unwrap();
        let status = second.status().await;
        assert!(!status.direct.enabled && !status.running);
        let third = McpServiceRuntime::load(path);
        third.start_if_enabled(&state).await;
        assert!(!third.status().await.running);
    }

    #[tokio::test]
    async fn a_failed_listen_rolls_back_to_the_previous_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = McpServiceRuntime::load(dir.path().join("mcp-server.json"));
        let state = state();
        runtime
            .issue_token(network_request(Profile::Edit))
            .await
            .unwrap();
        runtime.set_enabled(&state, true).await.unwrap();

        let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let busy_port = occupied.local_addr().unwrap().port();
        let before = runtime.status().await;
        let error = runtime
            .set_direct_access(
                &state,
                DirectAccessRequest {
                    port: Some(busy_port),
                    ..direct_request(&["203.0.113.5"])
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, McpRuntimeError::Bind { .. }), "{error}");
        let after = runtime.status().await;
        assert!(!after.direct.enabled, "失败后不应处于开启状态");
        assert!(after.running, "原来在运行的本机服务要恢复");
        assert_eq!(after.port, before.port, "端口也要回到原来的");

        // 不是本机的 IP 也一样：绑定失败，原样回滚。
        let wrong_ip = runtime
            .set_direct_access(
                &state,
                DirectAccessRequest {
                    bind_host: "203.0.113.77".to_string(),
                    ..direct_request(&["203.0.113.77"])
                },
            )
            .await;
        assert!(matches!(wrong_ip, Err(McpRuntimeError::Bind { .. })));
        assert!(runtime.status().await.running);
    }
}
