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
use std::sync::{Arc, Mutex};

use magi_conversation_runtime::external_approval::cancel_external_approvals;
use magi_core::UtcMillis;
use magi_mcp_server::http::{HttpConfig, HttpState, router};
use magi_mcp_server::{IssueTokenRequest, IssuedToken, TokenRecord, TokenStore};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::mcp_service::{build_mcp_server, external_session_id};
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
}

#[derive(Default)]
struct Control {
    enabled: bool,
    port: Option<u16>,
    running: Option<Running>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpServiceStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub url: Option<String>,
    pub active_tokens: usize,
}

pub(crate) struct McpServiceRuntime {
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
        Self {
            path: Some(path),
            tokens: Arc::new(TokenStore::from_records(persisted.tokens)),
            control: AsyncMutex::new(Control {
                enabled: persisted.enabled,
                port: persisted.port,
                running: None,
            }),
            persist_lock: Mutex::new(()),
        }
    }

    pub(crate) fn tokens(&self) -> Arc<TokenStore> {
        self.tokens.clone()
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
        self.status_locked(&control)
    }

    fn status_locked(&self, control: &Control) -> McpServiceStatus {
        let now = now_ms();
        let addr = control.running.as_ref().map(|running| running.addr);
        McpServiceStatus {
            enabled: control.enabled,
            running: control.running.is_some(),
            port: addr.map(|addr| addr.port()).or(control.port),
            url: addr.map(|addr| format!("http://{addr}/mcp")),
            active_tokens: self
                .tokens
                .records()
                .iter()
                .filter(|record| record.is_active(now))
                .count(),
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
            Self::stop_locked(&mut control).await;
        }
        control.enabled = enabled;
        self.persist(control.enabled, control.port)?;
        Ok(self.status_locked(&control))
    }

    /// daemon 启动时调用：上次是开启的就恢复。失败只记日志，不影响 daemon。
    pub(crate) async fn start_if_enabled(&self, state: &ApiState) {
        let mut control = self.control.lock().await;
        if control.enabled && control.running.is_none() {
            if let Err(error) = self.start_locked(state, &mut control).await {
                tracing::warn!(%error, "MCP 服务启动失败，保持关闭");
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
        let app = router(HttpState::new(
            server,
            self.tokens.clone(),
            HttpConfig::default(),
        ));
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        control.running = Some(Running {
            addr,
            stop: Some(stop),
            task,
        });
        Ok(())
    }

    async fn stop_locked(control: &mut Control) {
        if let Some(mut running) = control.running.take() {
            if let Some(stop) = running.stop.take() {
                let _ = stop.send(());
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
        McpServiceRuntime::stop_locked(&mut *first.control.lock().await).await;

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains(&issued.secret), "令牌原文不得落盘");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "配置文件必须是用户私有权限");
        }

        let second = McpServiceRuntime::load(path);
        second.start_if_enabled(&state).await;
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
}
