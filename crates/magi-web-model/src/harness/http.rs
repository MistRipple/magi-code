//! `magi-web-harness` 的 Streamable HTTP 传输（设计基线 §5.7.3）。
//!
//! 只监听 `127.0.0.1` 的**随机高端口**，端口号由调用方写入 `state_root` 下的
//! 端口文件，供 Connect 的本地转发进程读取。它是**独立入口**：不挂主 app、
//! 不共享鉴权中间件、不暴露任何 `/api/*` 路由；经由它无法访问 daemon 的其他路径。
//!
//! 关于信任边界：设计基线明确规定「同 OS 用户下的其他进程在信任边界之内」，
//! 因此除 turn 令牌外不叠加本机身份校验——这是显式取舍，不是遗漏。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;

use super::mcp::{self, JsonRpcRequest};
use super::{HarnessRegistry, token_ref};

const MAX_HTTP_BODY_BYTES: usize = 4 * 1024 * 1024;

/// 已启动的 harness HTTP 入口。
pub struct HarnessHttpServer {
    pub port: u16,
    shutdown: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl HarnessHttpServer {
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// 停止监听（应用退出、连接器关闭时调用）。
    pub fn stop(self) {
        let _ = self.shutdown.send(());
        self.handle.abort();
    }
}

/// 在 `127.0.0.1` 的随机高端口上启动 harness。
pub async fn serve_http(registry: Arc<HarnessRegistry>) -> std::io::Result<HarnessHttpServer> {
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let port = listener.local_addr()?.port();
    let router = Router::new()
        .route("/mcp", post(handle_mcp))
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(registry);
    let (shutdown, mut receiver) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = (&mut receiver).await;
            })
            .await;
    });
    Ok(HarnessHttpServer {
        port,
        shutdown,
        handle,
    })
}

async fn handle_mcp(State(registry): State<Arc<HarnessRegistry>>, body: Bytes) -> Response {
    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return Json(mcp::error(
                Value::Null,
                mcp::JSONRPC_PARSE_ERROR,
                format!("请求体不是合法 JSON-RPC：{error}"),
            ))
            .into_response();
        }
    };
    if !request.is_well_formed() {
        return Json(mcp::error(
            request.response_id(),
            mcp::JSONRPC_INVALID_REQUEST,
            "请求不是合法的 JSON-RPC 2.0",
        ))
        .into_response();
    }
    // 只有 `tools/call` 需要会话；握手与 `tools/list` 与具体轮次无关。
    if request.method != "tools/call" {
        return match direct_response(&request) {
            Some(value) => Json(value).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        };
    }
    let Some(token) = turn_token_of(&request) else {
        if request.is_notification() {
            return StatusCode::ACCEPTED.into_response();
        }
        return Json(mcp::success(
            request.response_id(),
            mcp::tool_failure("invalid_token：请求缺少 turn_token"),
        ))
        .into_response();
    };
    let Some(session) = registry.lookup(&token_ref(&token)) else {
        if request.is_notification() {
            return StatusCode::ACCEPTED.into_response();
        }
        return Json(mcp::success(
            request.response_id(),
            mcp::tool_failure("invalid_token：turn 令牌不属于任何在飞回复"),
        ))
        .into_response();
    };
    match session.handle(request).await {
        Some(value) => Json(value).into_response(),
        // 通知不产生响应；HTTP 语义下回 202。
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn direct_response(request: &JsonRpcRequest) -> Option<Value> {
    match request.method.as_str() {
        "notifications/initialized" | "notifications/cancelled" => None,
        "initialize" => Some(mcp::success(
            request.response_id(),
            mcp::initialize_result(),
        )),
        "ping" => Some(mcp::success(request.response_id(), mcp::ping_result())),
        "tools/list" => Some(mcp::success(
            request.response_id(),
            mcp::tools_list_result(),
        )),
        other => Some(mcp::error(
            request.response_id(),
            mcp::JSONRPC_METHOD_NOT_FOUND,
            format!("不支持的方法：{other}"),
        )),
    }
}

/// 从 `tools/call` 的参数里取出 turn 令牌（两个桥接工具都要求它）。
pub fn turn_token_of(request: &JsonRpcRequest) -> Option<String> {
    let params = request.params.as_ref()?;
    let arguments = params.get("arguments")?;
    let arguments = match arguments {
        Value::String(raw) => serde_json::from_str::<Value>(raw).ok()?,
        value => value.clone(),
    };
    arguments
        .get("turn_token")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessTurnSpec;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn spec() -> HarnessTurnSpec {
        HarnessTurnSpec {
            session_id: "s1".into(),
            thread_id: "orchestrator".into(),
            engine_id: "chatgpt-web/gpt-5".into(),
            epoch: 1,
            turn_id: "abcabcabcabcabcabcabcabcabcabcab".into(),
            token: "feedfacefeedfacefeedfacefeedface".into(),
            tools: vec![super::super::HarnessToolSchema {
                name: "read_file".into(),
                description: "读取文件".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            suspension_timeout: Duration::from_millis(200),
        }
    }

    async fn post(port: u16, body: &str) -> Value {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let text = String::from_utf8(response).expect("utf8");
        let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
        serde_json::from_str(body).expect("json body")
    }

    async fn status(port: u16, method: &str, path: &str) -> u16 {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8(response)
            .expect("utf8")
            .split_whitespace()
            .nth(1)
            .expect("status")
            .parse()
            .expect("status number")
    }

    #[tokio::test]
    async fn initialize_and_tools_list_do_not_need_a_session() {
        let registry = Arc::new(HarnessRegistry::new());
        let server = serve_http(Arc::clone(&registry)).await.expect("serve");
        let value = post(
            server.port,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;
        assert_eq!(value["result"]["tools"].as_array().expect("tools").len(), 2);
        server.stop();
    }

    #[tokio::test]
    async fn a_tool_call_with_an_unknown_token_fails_closed() {
        let registry = Arc::new(HarnessRegistry::new());
        let server = serve_http(Arc::clone(&registry)).await.expect("serve");
        let value = post(
            server.port,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"magi_tool_call","arguments":{"turn_token":"deadbeefdeadbeefdeadbeefdeadbeef","name":"read_file"}}}"#,
        )
        .await;
        assert_eq!(value["result"]["isError"], true);
        assert!(
            value["result"]["content"][0]["text"]
                .as_str()
                .expect("text")
                .contains("invalid_token")
        );
        server.stop();
    }

    #[tokio::test]
    async fn the_http_harness_has_no_daemon_api_surface() {
        let registry = Arc::new(HarnessRegistry::new());
        let server = serve_http(Arc::clone(&registry)).await.expect("serve");
        assert_eq!(
            status(server.port, "GET", "/api/browser/sessions").await,
            404
        );
        assert_eq!(status(server.port, "GET", "/mcp").await, 405);
        server.stop();
    }

    #[tokio::test]
    async fn a_registered_session_suspends_and_resumes_over_http() {
        let registry = Arc::new(HarnessRegistry::new());
        let session = registry.open(spec());
        let server = serve_http(Arc::clone(&registry)).await.expect("serve");
        let caller = {
            let port = server.port;
            tokio::spawn(async move {
                post(
                    port,
                    r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"magi_tool_call","arguments":{"turn_token":"feedfacefeedfacefeedfacefeedface","name":"read_file","arguments":{"path":"a"}}}}"#,
                )
                .await
            })
        };
        let batch = session
            .next_batch(Duration::from_millis(20))
            .await
            .expect("batch");
        session
            .deliver_results(&[(
                batch.calls[0].tool_call_id.clone(),
                super::super::ToolCallOutcome {
                    status: crate::protocol::ToolResultStatus::Ok,
                    body: "文件内容".into(),
                },
            )])
            .expect("deliver");
        let value = caller.await.expect("join");
        assert_eq!(value["result"]["content"][0]["text"], "文件内容");
        server.stop();
    }
}
