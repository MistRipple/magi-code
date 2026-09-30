//! stdio 传输：`magi-web-harness --stdio` 是**轻量中继**（设计基线 §5.7.3）。
//!
//! 中继进程由 `openai/tunnel-client` 以子进程方式拉起，它**不自己持有令牌与挂起
//! 调用**：只把 stdin/stdout 与 daemon 内的本地 socket 互转。令牌校验、挂起调用
//! 与去重账本都在 daemon 侧（`HarnessRegistry`）完成。
//!
//! 这条路径是 stdio 形态**唯一**的本机接口：不监听 TCP、不写端口文件、不经过任何
//! HTTP 路由。本地 socket 仅当前 OS 用户可访问（Unix domain socket `0600`；
//! Windows 命名管道）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use super::HarnessRegistry;
use super::mcp::{self, JsonRpcRequest};
use super::token_ref;

/// 单条 JSON-RPC 消息的字节上限（防止中继被无界输入打爆）。
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// daemon 与 stdio 中继约定的本地端点路径。
///
/// 中继进程与 daemon 必须用同一个推导规则，否则本地 socket 对不上。
pub fn local_endpoint_for(state_root: &Path) -> PathBuf {
    // Unix domain sockets have a small platform-specific path limit (macOS is
    // usually 104 bytes).  A Magi state root can legitimately live under a
    // long temporary/project path, so putting the socket below `state_root`
    // makes the stdio relay fail before the tunnel can even start.  Keep the
    // endpoint deterministic for the daemon and the relay, but place it in the
    // OS temporary directory and bind the opaque name to the state root.
    //
    // The state root is not a secret; hashing it avoids leaking a workspace or
    // user path in the socket filename.  The daemon still enforces 0600 (or a
    // same-user Windows named pipe) when it creates the endpoint.
    let digest = crate::protocol::sha256_hex(&state_root.to_string_lossy());
    let name = format!("magi-web-{}.sock", &digest[..24]);
    std::env::temp_dir().join(name)
}

/// `magi-web-harness --stdio` 的入口。
pub struct StdioRelay;

impl StdioRelay {
    /// 把本进程的 stdin/stdout 与 daemon 的本地 socket 对接。
    ///
    /// 双向字节搬运：请求与响应都是**换行分隔的 JSON-RPC**，响应按 `id` 对上，
    /// 因此并发请求与挂起调用不需要在中继里再实现一遍多路复用。
    pub async fn run(local_endpoint: &Path) -> std::io::Result<()> {
        let stream = imp::connect(local_endpoint).await?;
        let (mut socket_read, mut socket_write) = tokio::io::split(stream);
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut to_socket = vec![0u8; 16 * 1024];
        let mut to_stdout = vec![0u8; 16 * 1024];

        // Keep both directions in one select loop.  If the daemon closes the
        // local socket, the relay must terminate even when tunnel-client keeps
        // stdin open.  If tunnel-client closes stdin, half-close the socket and
        // drain the daemon's final responses before exiting.
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

/// daemon 侧的本地 socket 监听。
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

/// 启动 daemon 侧的本地 socket 监听（仅当前用户可访问）。
pub async fn serve_local_socket(
    registry: Arc<HarnessRegistry>,
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
                    let registry = Arc::clone(&registry);
                    tokio::spawn(async move {
                        let _ = serve_stream(stream, registry).await;
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

/// 处理一条中继连接：逐行读 JSON-RPC，逐条回写响应。
async fn serve_stream<S>(stream: S, registry: Arc<HarnessRegistry>) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut reader = BufReader::new(reader);
    while let Some(raw_line) = read_bounded_line(&mut reader).await? {
        let line = String::from_utf8_lossy(&raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        let registry = Arc::clone(&registry);
        let writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let request: JsonRpcRequest = match serde_json::from_str(&line) {
                Ok(request) => request,
                Err(error) => {
                    let value = mcp::error(
                        serde_json::Value::Null,
                        mcp::JSONRPC_PARSE_ERROR,
                        format!("请求不是合法 JSON-RPC：{error}"),
                    );
                    let _ = write_line(&writer, &value).await;
                    return;
                }
            };
            let Some(response) = dispatch(&registry, request).await else {
                return;
            };
            let _ = write_line(&writer, &response).await;
        });
    }
    Ok(())
}

/// `AsyncBufReadExt::lines` 会在返回前无界累积一整行；stdio 是外部 Tunnel 的
/// 边界，必须在 daemon 侧解析前先限制消息大小。超限直接关闭该中继连接。
async fn read_bounded_line<R>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let take = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        if line.len().saturating_add(take) > MAX_MESSAGE_BYTES {
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

/// 与 HTTP 入口共用同一套派发规则：只有 `tools/call` 需要会话。
async fn dispatch(
    registry: &Arc<HarnessRegistry>,
    request: JsonRpcRequest,
) -> Option<serde_json::Value> {
    if !request.is_well_formed() {
        return Some(mcp::error(
            request.response_id(),
            mcp::JSONRPC_INVALID_REQUEST,
            "请求不是合法的 JSON-RPC 2.0",
        ));
    }
    if request.method != "tools/call" {
        return Some(match request.method.as_str() {
            "initialize" => mcp::success(request.response_id(), mcp::initialize_result()),
            "ping" => mcp::success(request.response_id(), mcp::ping_result()),
            "tools/list" => mcp::success(request.response_id(), mcp::tools_list_result()),
            "notifications/initialized" | "notifications/cancelled" => return None,
            other => mcp::error(
                request.response_id(),
                mcp::JSONRPC_METHOD_NOT_FOUND,
                format!("不支持的方法：{other}"),
            ),
        });
    }
    let Some(token) = super::http::turn_token_of(&request) else {
        if request.is_notification() {
            return None;
        }
        return Some(mcp::success(
            request.response_id(),
            mcp::tool_failure("invalid_token：请求缺少 turn_token"),
        ));
    };
    let Some(session) = registry.lookup(&token_ref(&token)) else {
        if request.is_notification() {
            return None;
        }
        return Some(mcp::success(
            request.response_id(),
            mcp::tool_failure("invalid_token：turn 令牌不属于任何在飞回复"),
        ));
    };
    session.handle(request).await
}

async fn write_line<W>(
    writer: &Arc<tokio::sync::Mutex<W>>,
    value: &serde_json::Value,
) -> std::io::Result<()>
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
        // An active daemon must not be displaced by a second daemon using the
        // same state root.  A failed connect means the socket is stale and can
        // safely be removed after that check.
        if endpoint.exists() {
            match UnixStream::connect(endpoint).await {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "web model harness socket is already serving",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    // A socket owned by another OS user must never be
                    // unlinked just because this daemon cannot connect to it.
                    // The local endpoint is the stdio trust boundary.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "web model harness socket is not accessible",
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
        // 仅当前 OS 用户可访问。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) =
                std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
            {
                let _ = std::fs::remove_file(endpoint);
                return Err(error);
            }
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

    /// 把一个本地路径映射为命名管道名（Windows 下没有文件系统套接字）。
    pub fn pipe_name(endpoint: &Path) -> String {
        let raw = endpoint.to_string_lossy();
        if raw.starts_with(r"\\.\pipe\") {
            raw.to_string()
        } else {
            let leaf = endpoint
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "magi-web-harness".to_string());
            format!(r"\\.\pipe\{leaf}")
        }
    }

    /// Windows 命名管道没有 `Listener` 类型：这里用一个最小句柄包住
    /// 「创建 → 等待连接」的循环。
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
        // The first instance makes a second daemon using the same endpoint
        // fail closed instead of silently sharing the stdio trust boundary.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{HarnessToolSchema, HarnessTurnSpec};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn spec() -> HarnessTurnSpec {
        HarnessTurnSpec {
            session_id: "s1".into(),
            thread_id: "orchestrator".into(),
            engine_id: "chatgpt-web/gpt-5".into(),
            epoch: 1,
            turn_id: "abcabcabcabcabcabcabcabcabcabcab".into(),
            token: "feedfacefeedfacefeedfacefeedface".into(),
            tools: vec![HarnessToolSchema {
                name: "read_file".into(),
                description: "读取文件".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            suspension_timeout: Duration::from_millis(200),
        }
    }

    #[tokio::test]
    async fn the_local_socket_serves_tools_list_and_rejects_unknown_tokens() {
        // Unix domain socket 的路径受 `SUN_LEN` 限制，临时目录名必须足够短。
        let dir = std::env::temp_dir().join(format!(
            "mwh-{}",
            &crate::protocol::sha256_hex(&format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))[..10]
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let endpoint = dir.join("harness.sock");
        let registry = Arc::new(HarnessRegistry::new());
        let server = serve_local_socket(Arc::clone(&registry), endpoint.clone())
            .await
            .expect("serve");
        let stream = imp::connect(&endpoint).await.expect("connect");
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();

        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .expect("write");
        write.flush().await.expect("flush");
        let line = lines.next_line().await.expect("read").expect("line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["result"]["tools"].as_array().expect("tools").len(), 2);

        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"magi_tool_call\",\"arguments\":{\"turn_token\":\"deadbeefdeadbeefdeadbeefdeadbeef\",\"name\":\"read_file\"}}}\n")
            .await
            .expect("write");
        write.flush().await.expect("flush");
        let line = lines.next_line().await.expect("read").expect("line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["result"]["isError"], true);
        let _ = registry.open(spec());
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stdio_reader_closes_an_oversized_message_before_dispatch() {
        let (mut writer, reader) = tokio::io::duplex(MAX_MESSAGE_BYTES + 2);
        let oversized = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        writer.write_all(&oversized).await.expect("write oversized");
        writer.write_all(b"\n").await.expect("write newline");
        let mut reader = BufReader::new(reader);
        let error = read_bounded_line(&mut reader)
            .await
            .expect_err("oversized message must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
