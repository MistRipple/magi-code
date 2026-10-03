//! stdio 传输：`magi-mcp --stdio` 是**轻量中继**，令牌校验与调用管线都在 daemon 侧。
//!
//! 中继进程只做两件事：连上 daemon 的本地 socket，先发一行握手 `{"magiMcpToken":"…"}`，
//! 之后把 stdin/stdout 与 socket 双向互转（换行分隔的 JSON-RPC）。令牌由 `--token-file` 或环境变量
//! 提供，**不接受命令行明文参数**（避免出现在进程列表里）。
//!
//! 这条路径不监听 TCP、不写端口文件、不经过任何 HTTP 路由。本地 socket 仅当前 OS 用户可访问
//! （Unix domain socket `0600`；Windows 命名管道拒绝远程客户端）。daemon 侧**每条请求**都重新校验令牌，
//! 因此吊销、过期与权限档变更立即对已建立的连接生效。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::protocol::{JSONRPC_PARSE_ERROR, JsonRpcRequest, error};
use crate::server::{ClientPrincipal, MAX_ARGUMENT_BYTES, McpServer};
use crate::token::TokenStore;

/// 单条 JSON-RPC 消息的字节上限：参数上限加外层余量。
const MAX_MESSAGE_BYTES: usize = MAX_ARGUMENT_BYTES + 64 * 1024;
/// 握手行上限（令牌本身只有几十字节）。
const MAX_HANDSHAKE_BYTES: usize = 4 * 1024;
/// 令牌在连接中途失效时使用的 JSON-RPC 错误码。
const JSONRPC_UNAUTHORIZED: i64 = -32001;

/// daemon 与中继约定的本地端点路径。两端必须用同一个推导规则。
///
/// Unix domain socket 路径长度有平台上限（macOS 约 104 字节），而 state root 可能很长，
/// 因此端点放在系统临时目录，名字用 state root 的哈希（不泄露用户路径）。
pub fn local_endpoint_for(state_root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!(
        "magi-mcp-endpoint-v1:{}",
        state_root.to_string_lossy()
    ));
    let hex: String = digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::env::temp_dir().join(format!("magi-mcp-{hex}.sock"))
}

/// 把本进程的 stdin/stdout 与 daemon 的本地 socket 对接。
pub struct StdioRelay;

impl StdioRelay {
    /// `token = None` 用于“槽位端点”：该端点不做令牌握手，调用方身份由 daemon 按槽位事实决定。
    pub async fn run(endpoint: &Path, token: Option<&str>) -> std::io::Result<()> {
        let stream = imp::connect(endpoint).await?;
        let (mut socket_read, mut socket_write) = tokio::io::split(stream);
        if let Some(token) = token {
            let mut handshake = serde_json::to_vec(&json!({ "magiMcpToken": token }))
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            handshake.push(b'\n');
            socket_write.write_all(&handshake).await?;
            socket_write.flush().await?;
        }

        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut to_socket = vec![0u8; 16 * 1024];
        let mut to_stdout = vec![0u8; 16 * 1024];
        // 同一个 select 循环处理两个方向：daemon 关闭 socket 时即使 stdin 仍开着也要退出；
        // stdin 关闭时半关闭 socket，并把 daemon 的最后响应排空。
        loop {
            tokio::select! {
                read = tokio::io::AsyncReadExt::read(&mut stdin, &mut to_socket) => {
                    let read = read?;
                    if read == 0 {
                        socket_write.shutdown().await?;
                        loop {
                            let read = tokio::io::AsyncReadExt::read(&mut socket_read, &mut to_stdout).await?;
                            if read == 0 {
                                return Ok(());
                            }
                            stdout.write_all(&to_stdout[..read]).await?;
                            stdout.flush().await?;
                        }
                    }
                    socket_write.write_all(&to_socket[..read]).await?;
                    socket_write.flush().await?;
                }
                read = tokio::io::AsyncReadExt::read(&mut socket_read, &mut to_stdout) => {
                    let read = read?;
                    if read == 0 {
                        return Ok(());
                    }
                    stdout.write_all(&to_stdout[..read]).await?;
                    stdout.flush().await?;
                }
            }
        }
    }
}

/// daemon 侧的本地 socket 监听句柄。
pub struct LocalSocketServer {
    pub endpoint: PathBuf,
    shutdown: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl LocalSocketServer {
    pub fn stop(self) {
        let _ = self.shutdown.send(());
        self.handle.abort();
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.endpoint);
        }
    }
}

/// 调用方身份由宿主按“当前事实”动态给出（例如 GPT Web 槽位的拥有者）。
/// 返回 `None` 表示此刻没有可用的身份，该请求被拒绝。
pub type PrincipalProvider = Arc<dyn Fn() -> Option<ClientPrincipal> + Send + Sync>;

/// 本地 socket 的认证方式。
#[derive(Clone)]
pub enum SocketAuth {
    /// 连接后先发一行令牌握手，之后**每条请求**重新认证（令牌端点）。
    Token(Arc<TokenStore>),
    /// 不做握手，身份由宿主按当前事实动态给出（槽位端点）。
    /// 端点仍然只有当前 OS 用户可访问；它不接受也不签发令牌。
    Principal(PrincipalProvider),
}

/// 启动令牌端点（连接后先握手令牌）。
pub async fn serve_local_socket(
    server: Arc<McpServer>,
    tokens: Arc<TokenStore>,
    endpoint: PathBuf,
) -> std::io::Result<LocalSocketServer> {
    serve_local_socket_with_auth(server, SocketAuth::Token(tokens), endpoint).await
}

/// 启动 daemon 侧的本地 socket 监听（仅当前用户可访问）。
pub async fn serve_local_socket_with_auth(
    server: Arc<McpServer>,
    auth: SocketAuth,
    endpoint: PathBuf,
) -> std::io::Result<LocalSocketServer> {
    let mut listener = imp::bind(&endpoint).await?;
    let (shutdown, mut receiver) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut receiver => break,
                accepted = imp::accept(&mut listener) => {
                    let Ok(stream) = accepted else { break };
                    let server = Arc::clone(&server);
                    let auth = auth.clone();
                    tokio::spawn(async move {
                        let _ = serve_stream(stream, server, auth).await;
                    });
                }
            }
        }
    });
    Ok(LocalSocketServer {
        endpoint,
        shutdown,
        handle,
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

async fn serve_stream<S>(stream: S, server: Arc<McpServer>, auth: SocketAuth) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);

    // 令牌端点：第一行必须是令牌握手。失败统一回 unauthorized 并关闭，不区分原因。
    let secret = match &auth {
        SocketAuth::Token(tokens) => {
            let secret = match read_bounded_line(&mut reader, MAX_HANDSHAKE_BYTES).await? {
                Some(line) => serde_json::from_slice::<Value>(&line)
                    .ok()
                    .and_then(|value| value.get("magiMcpToken")?.as_str().map(str::to_string)),
                None => None,
            };
            let Some(secret) =
                secret.filter(|secret| tokens.authenticate(secret, now_ms()).is_ok())
            else {
                write_line(&writer, &json!({ "error": "unauthorized" })).await?;
                return Ok(());
            };
            Some(secret)
        }
        SocketAuth::Principal(_) => None,
    };

    while let Some(raw_line) = read_bounded_line(&mut reader, MAX_MESSAGE_BYTES).await? {
        let line = String::from_utf8_lossy(&raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        // 每条请求都重新认证 / 重新取身份：吊销、过期、权限档与槽位变更立即生效。
        let now = now_ms();
        let principal = match (&auth, &secret) {
            (SocketAuth::Token(tokens), Some(secret)) => match tokens.authenticate(secret, now) {
                Ok(record) => ClientPrincipal::from(&record),
                Err(_) => {
                    write_line(
                        &writer,
                        &error(Value::Null, JSONRPC_UNAUTHORIZED, "令牌已失效"),
                    )
                    .await?;
                    return Ok(());
                }
            },
            (SocketAuth::Principal(provider), _) => match provider() {
                Some(principal) => principal,
                None => {
                    // 没有可用身份（例如槽位已释放）：该请求失败，连接保持，下一条请求重新判定。
                    let id = serde_json::from_str::<JsonRpcRequest>(&line)
                        .map(|request| request.response_id())
                        .unwrap_or(Value::Null);
                    write_line(
                        &writer,
                        &error(id, JSONRPC_UNAUTHORIZED, "当前没有可用的调用身份"),
                    )
                    .await?;
                    continue;
                }
            },
            (SocketAuth::Token(_), None) => return Ok(()),
        };
        let server = Arc::clone(&server);
        let writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
                Ok(request) => server.handle(&principal, request, now).await,
                Err(parse_error) => Some(error(
                    Value::Null,
                    JSONRPC_PARSE_ERROR,
                    format!("请求不是合法 JSON-RPC：{parse_error}"),
                )),
            };
            if let Some(response) = response {
                let _ = write_line(&writer, &response).await;
            }
        });
    }
    Ok(())
}

/// 逐行读取并限制单行大小；超限直接报错关闭连接，避免无界累积。
async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        let take = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        if line.len().saturating_add(take) > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "消息超过上限",
            ));
        }
        line.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            return Ok(Some(line));
        }
    }
}

async fn write_line<W>(writer: &Arc<tokio::sync::Mutex<W>>, value: &Value) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut payload = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    payload.push(b'\n');
    let mut writer = writer.lock().await;
    writer.write_all(&payload).await?;
    writer.flush().await
}

#[cfg(unix)]
mod imp {
    use super::*;
    use tokio::net::{UnixListener, UnixStream};

    pub async fn bind(endpoint: &Path) -> std::io::Result<UnixListener> {
        // 正在服务的 daemon 不能被同一 state root 的第二个 daemon 顶掉；连不上才视为陈旧 socket。
        if endpoint.exists() {
            match UnixStream::connect(endpoint).await {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "MCP 本地 socket 已被占用",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    // 属于其他 OS 用户的 socket 不能因为“连不上”就被删除。
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "MCP 本地 socket 不可访问",
                    ));
                }
                Err(_) => {}
            }
            std::fs::remove_file(endpoint)?;
        }
        if let Some(parent) = endpoint.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(endpoint)?;
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(endpoint);
            return Err(error);
        }
        Ok(listener)
    }

    pub async fn accept(listener: &mut UnixListener) -> std::io::Result<UnixStream> {
        listener.accept().await.map(|(stream, _)| stream)
    }

    pub async fn connect(endpoint: &Path) -> std::io::Result<UnixStream> {
        UnixStream::connect(endpoint).await
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    /// Windows 没有文件系统套接字：把端点路径映射为命名管道名。
    pub fn pipe_name(endpoint: &Path) -> String {
        let raw = endpoint.to_string_lossy();
        if raw.starts_with(r"\\.\pipe\") {
            raw.to_string()
        } else {
            let leaf = endpoint
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "magi-mcp".to_string());
            format!(r"\\.\pipe\{leaf}")
        }
    }

    pub struct Listener {
        name: String,
        first_instance: bool,
    }

    pub async fn bind(endpoint: &Path) -> std::io::Result<Listener> {
        Ok(Listener {
            name: pipe_name(endpoint),
            first_instance: true,
        })
    }

    pub async fn accept(pipe: &mut Listener) -> std::io::Result<NamedPipeServer> {
        let mut options = ServerOptions::new();
        // 第一个实例让同一端点的第二个 daemon 明确失败，而不是静默共享信任边界。
        options
            .reject_remote_clients(true)
            .first_pipe_instance(pipe.first_instance);
        let server = options.create(&pipe.name)?;
        pipe.first_instance = false;
        server.connect().await?;
        Ok(server)
    }

    pub async fn connect(endpoint: &Path) -> std::io::Result<NamedPipeClient> {
        ClientOptions::new().open(pipe_name(endpoint))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::catalog::{ToolSchema, ToolSchemaProvider};
    use crate::path_guard::PathRequest;
    use crate::profile::Profile;
    use crate::server::{
        BoxFuture, InvocationOutcome, ToolBackend, ToolInvocation, WorkspaceResolver,
    };
    use crate::token::{AttributionMode, IssueTokenRequest};

    struct Schemas;
    impl ToolSchemaProvider for Schemas {
        fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
            Some(ToolSchema {
                description: internal_name.to_string(),
                input_schema: json!({ "type": "object" }),
            })
        }
    }

    struct Backend;
    impl ToolBackend for Backend {
        fn schemas(&self) -> &dyn ToolSchemaProvider {
            static SCHEMAS: Schemas = Schemas;
            &SCHEMAS
        }
        fn path_requests(&self, _: &str, _: &Value, _: &Path) -> Vec<PathRequest> {
            Vec::new()
        }
        fn invoke<'a>(&'a self, _: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
            Box::pin(async {
                InvocationOutcome {
                    text: "done".to_string(),
                    is_error: false,
                }
            })
        }
    }

    struct Root;
    impl WorkspaceResolver for Root {
        fn root_of(&self, _: &str) -> Option<PathBuf> {
            Some(std::env::temp_dir())
        }
    }

    struct Fixture {
        endpoint: PathBuf,
        secret: String,
        token_id: String,
        tokens: Arc<TokenStore>,
        server: Option<LocalSocketServer>,
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(server) = self.server.take() {
                server.stop();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn fixture() -> Fixture {
        // Unix socket 路径受 SUN_LEN 限制，目录名要短。
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mms-{}-{}",
            std::process::id() % 100_000,
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = dir.join("m.sock");
        let tokens = Arc::new(TokenStore::new());
        let issued = tokens
            .issue(
                IssueTokenRequest {
                    client_name: "test".to_string(),
                    workspace_id: "ws".to_string(),
                    profile: Profile::Edit,
                    attribution: AttributionMode::External,
                    ttl_ms: None,
                    network: false,
                },
                now_ms(),
            )
            .unwrap();
        let server = Arc::new(McpServer::new(
            "magi",
            "test",
            Arc::new(Backend),
            Arc::new(Root),
        ));
        let handle = serve_local_socket(server, tokens.clone(), endpoint.clone())
            .await
            .unwrap();
        Fixture {
            endpoint,
            secret: issued.secret,
            token_id: issued.record.token_id,
            tokens,
            server: Some(handle),
            dir,
        }
    }

    async fn connect_lines(
        fixture: &Fixture,
        handshake: &str,
    ) -> (
        tokio::io::WriteHalf<tokio::net::UnixStream>,
        tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>>,
    ) {
        let stream = imp::connect(&fixture.endpoint).await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        write
            .write_all(format!("{handshake}\n").as_bytes())
            .await
            .unwrap();
        (write, BufReader::new(read).lines())
    }

    fn handshake(secret: &str) -> String {
        json!({ "magiMcpToken": secret }).to_string()
    }

    #[tokio::test]
    async fn valid_token_can_list_tools_and_the_socket_is_private() {
        let f = fixture().await;
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f.endpoint).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "本地 socket 必须只有当前用户可访问");
        }
        let (mut write, mut lines) = connect_lines(&f, &handshake(&f.secret)).await;
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert!(
            value["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "magi.fs.write")
        );
    }

    #[tokio::test]
    async fn wrong_missing_or_malformed_handshakes_are_rejected_uniformly() {
        let f = fixture().await;
        for first_line in [
            handshake("magi_mcp_wrong"),
            "{}".to_string(),
            "not json".to_string(),
        ] {
            let (_write, mut lines) = connect_lines(&f, &first_line).await;
            let line = lines.next_line().await.unwrap().unwrap();
            assert_eq!(line, r#"{"error":"unauthorized"}"#, "{first_line}");
            assert!(
                lines.next_line().await.unwrap().is_none(),
                "认证失败后必须关闭连接"
            );
        }
    }

    #[tokio::test]
    async fn revoking_a_token_takes_effect_on_an_open_connection() {
        let f = fixture().await;
        let (mut write, mut lines) = connect_lines(&f, &handshake(&f.secret)).await;
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("\"result\""), "{line}");

        assert!(f.tokens.revoke(&f.token_id, now_ms()));
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("-32001"), "{line}");
        assert!(lines.next_line().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_messages_close_the_connection() {
        let f = fixture().await;
        let (mut write, mut lines) = connect_lines(&f, &handshake(&f.secret)).await;
        let oversized = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        let _ = write.write_all(&oversized).await;
        let _ = write.write_all(b"\n").await;
        assert!(lines.next_line().await.map_or(true, |line| line.is_none()));
    }

    #[test]
    fn endpoint_is_deterministic_short_and_does_not_leak_the_state_root() {
        let root = Path::new("/Users/alice/very/long/private/state/root");
        let endpoint = local_endpoint_for(root);
        assert_eq!(endpoint, local_endpoint_for(root));
        assert_ne!(endpoint, local_endpoint_for(Path::new("/other")));
        assert!(!endpoint.to_string_lossy().contains("alice"));
        assert!(endpoint.to_string_lossy().len() < 100);
    }

    #[tokio::test]
    async fn principal_endpoint_needs_no_handshake_and_follows_the_provider_per_request() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static SEQ2: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mms-slot-{}-{}",
            std::process::id() % 100_000,
            SEQ2.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = dir.join("slot.sock");
        let available = Arc::new(AtomicBool::new(false));
        let provider: PrincipalProvider = {
            let available = available.clone();
            Arc::new(move || {
                available.load(Ordering::SeqCst).then(|| ClientPrincipal {
                    token_id: "web-slot".to_string(),
                    token_prefix: "web-slot".to_string(),
                    client_name: "GPT Web".to_string(),
                    workspace_id: "ws".to_string(),
                    profile: Profile::Edit,
                    attribution: AttributionMode::External,
                })
            })
        };
        let server = Arc::new(McpServer::new(
            "magi",
            "test",
            Arc::new(Backend),
            Arc::new(Root),
        ));
        let handle =
            serve_local_socket_with_auth(server, SocketAuth::Principal(provider), endpoint.clone())
                .await
                .unwrap();
        let stream = imp::connect(&endpoint).await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();

        // 没有身份：这条请求失败，但连接保持。
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        let first: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["error"]["code"], -32001, "{first}");
        assert_eq!(first["id"], 1);

        // 身份出现后，同一连接上的下一条请求立即可用。
        available.store(true, Ordering::SeqCst);
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        let second: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(
            second["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "{second}"
        );

        // 身份消失（槽位释放）：立即失效。
        available.store(false, Ordering::SeqCst);
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        let third: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(third["error"]["code"], -32001);
        handle.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
