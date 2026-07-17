use crate::types::{
    BridgeClientError, BridgeErrorLayer, BridgeResponse, McpBridgeClient, McpToolCallRequest,
};
use magi_process::{ManagedChild, spawn_managed, std_command};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::Stdio,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::Duration,
};

const MCP_SERVER_COMMAND_ENV: &str = "MAGI_MCP_SERVER_COMMAND";
const MCP_SERVER_ARGS_ENV: &str = "MAGI_MCP_SERVER_ARGS";
const MCP_SERVER_WORKING_DIR_ENV: &str = "MAGI_MCP_SERVER_WORKING_DIR";
const DEFAULT_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const STREAMABLE_HTTP_MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Configuration for connecting to a real MCP server via stdio transport.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub request_timeout: Duration,
}

/// Streamable HTTP 传输使用的 MCP 服务配置。
#[derive(Clone, Debug)]
pub struct HttpMcpServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub request_timeout: Duration,
}

/// MCP 服务连接的规范化传输配置。
#[derive(Clone, Debug)]
pub enum McpServerConnectionConfig {
    Stdio(McpServerConfig),
    StreamableHttp(HttpMcpServerConfig),
}

impl McpServerConfig {
    /// Create from environment variables.
    ///
    /// Returns `None` if `MAGI_MCP_SERVER_COMMAND` is not set.
    pub fn from_env() -> Option<Self> {
        let command = read_non_empty_env(MCP_SERVER_COMMAND_ENV)?;
        let args = read_non_empty_env(MCP_SERVER_ARGS_ENV)
            .map(|raw| {
                raw.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let working_directory = read_non_empty_env(MCP_SERVER_WORKING_DIR_ENV).map(PathBuf::from);

        Some(Self {
            command,
            args,
            working_directory,
            env: BTreeMap::new(),
            request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
        })
    }

    /// Create with explicit command and arguments.
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            working_directory: None,
            env: BTreeMap::new(),
            request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
        }
    }

    pub fn with_working_directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(dir.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout.max(Duration::from_millis(100));
        self
    }
}

/// A `McpBridgeClient` implementation that spawns a real MCP server process
/// and communicates over stdio using JSON-RPC 2.0 (the MCP transport protocol).
///
/// The client handles the MCP lifecycle:
/// 1. Spawn the server subprocess
/// 2. Send `initialize` request
/// 3. Send `notifications/initialized` notification
/// 4. Send `tools/list` to discover available tools
/// 5. Send `tools/call` for each tool invocation
///
/// The subprocess is spawned once on the first call and kept alive for
/// subsequent requests. A mutex protects concurrent access to the stdio
/// streams.
pub struct StdioMcpBridgeClient {
    config: McpServerConfig,
    connection: Mutex<Option<McpConnection>>,
    initialized: AtomicBool,
    next_id: AtomicU64,
}

struct McpConnection {
    child: ManagedChild,
    responses: Receiver<Result<Value, String>>,
    writer: std::process::ChildStdin,
}

impl std::fmt::Debug for StdioMcpBridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioMcpBridgeClient")
            .field("config", &self.config)
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .finish()
    }
}

impl StdioMcpBridgeClient {
    /// Create a new client from the given configuration.
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            connection: Mutex::new(None),
            initialized: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }
    }

    /// Create from environment variables.
    ///
    /// Returns `None` if `MAGI_MCP_SERVER_COMMAND` is not set.
    pub fn from_env() -> Option<Self> {
        McpServerConfig::from_env().map(Self::new)
    }

    /// Returns the list of tools exposed by the connected MCP server.
    ///
    /// Ensures the connection is initialized before querying.
    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, BridgeClientError> {
        self.ensure_initialized()?;
        let response = self.send_request("tools/list", json!({}))?;
        let tools_value = response
            .get("tools")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let tools: Vec<McpToolInfo> =
            serde_json::from_value(tools_value).map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("decode tools/list response failed: {error}"),
            })?;
        Ok(tools)
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn ensure_initialized(&self) -> Result<(), BridgeClientError> {
        if self.initialized.load(Ordering::Relaxed) {
            return Ok(());
        }

        self.spawn_and_initialize()
    }

    fn spawn_and_initialize(&self) -> Result<(), BridgeClientError> {
        let mut connection_guard = self
            .connection
            .lock()
            .map_err(|_| mcp_transport_error("connection mutex poisoned".to_string()))?;

        // Double-check after acquiring lock
        if self.initialized.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mut cmd = std_command(&self.config.command);
        cmd.args(&self.config.args);
        if let Some(dir) = &self.config.working_directory {
            cmd.current_dir(dir);
        }
        for (key, value) in &self.config.env {
            cmd.env(key, value);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = spawn_managed(&mut cmd).map_err(|error| {
            mcp_transport_error(format!(
                "spawn MCP server {} failed: {error}",
                self.config.command
            ))
        })?;

        let stdin = child.take_stdin().ok_or_else(|| {
            mcp_transport_error(format!(
                "MCP server {} stdin unavailable",
                self.config.command
            ))
        })?;
        let stdout = child.take_stdout().ok_or_else(|| {
            mcp_transport_error(format!(
                "MCP server {} stdout unavailable",
                self.config.command
            ))
        })?;
        if let Some(stderr) = child.take_stderr() {
            spawn_mcp_stderr_drain(stderr);
        }

        let mut conn = McpConnection {
            child,
            responses: spawn_mcp_stdout_reader(stdout),
            writer: stdin,
        };

        // Step 1: Send initialize request
        let init_id = self.next_request_id();
        let init_request = json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": STREAMABLE_HTTP_MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "magi-bridge-client",
                    "version": "1.0.0"
                }
            }
        });

        send_json(&mut conn.writer, &init_request)?;
        let init_response =
            match receive_json_response(&conn.responses, self.config.request_timeout) {
                Ok(response) => response,
                Err(error) => {
                    terminate_mcp_connection(&mut conn);
                    return Err(error);
                }
            };
        if let Err(error) = validate_jsonrpc_response(&init_response, init_id) {
            terminate_mcp_connection(&mut conn);
            return Err(error);
        }

        // Step 2: Send initialized notification (no id = notification)
        let initialized_notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        send_json(&mut conn.writer, &initialized_notification)?;

        *connection_guard = Some(conn);
        self.initialized.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn send_request(&self, method: &str, params: Value) -> Result<Value, BridgeClientError> {
        self.ensure_initialized()?;

        let mut connection_guard = self
            .connection
            .lock()
            .map_err(|_| mcp_transport_error("connection mutex poisoned".to_string()))?;

        let conn = connection_guard
            .as_mut()
            .ok_or_else(|| mcp_transport_error("MCP server not connected".to_string()))?;

        let request_id = self.next_request_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        });

        send_json(&mut conn.writer, &request)?;
        let response = match receive_json_response(&conn.responses, self.config.request_timeout) {
            Ok(response) => response,
            Err(error) => {
                if let Some(mut failed) = connection_guard.take() {
                    terminate_mcp_connection(&mut failed);
                }
                self.initialized.store(false, Ordering::Relaxed);
                return Err(error);
            }
        };
        if let Err(error) = validate_jsonrpc_response(&response, request_id) {
            if let Some(mut failed) = connection_guard.take() {
                terminate_mcp_connection(&mut failed);
            }
            self.initialized.store(false, Ordering::Relaxed);
            return Err(error);
        }

        // Extract result
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: "MCP response missing 'result' field".to_string(),
            })
    }
}

impl McpBridgeClient for StdioMcpBridgeClient {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError> {
        let input = parse_mcp_tool_arguments(&request.input)?;

        let params = json!({
            "name": request.tool_name,
            "arguments": input
        });

        let result = self.send_request("tools/call", params)?;

        // MCP tools/call returns { content: [...], isError?: bool }
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let payload = if let Some(content) = result.get("content") {
            // Extract text content from the MCP content array
            extract_text_content(content)
        } else {
            result.to_string()
        };

        Ok(BridgeResponse {
            ok: !is_error,
            payload,
        })
    }
}

#[derive(Debug, Default)]
struct HttpMcpConnectionState {
    initialized: bool,
    session_id: Option<String>,
    protocol_version: Option<String>,
}

/// Streamable HTTP 传输的 MCP 客户端。
pub struct StreamableHttpMcpBridgeClient {
    config: HttpMcpServerConfig,
    state: Mutex<HttpMcpConnectionState>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for StreamableHttpMcpBridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let initialized = self
            .state
            .lock()
            .map(|state| state.initialized)
            .unwrap_or(false);
        f.debug_struct("StreamableHttpMcpBridgeClient")
            .field("initialized", &initialized)
            .finish()
    }
}

impl StreamableHttpMcpBridgeClient {
    pub fn new(config: HttpMcpServerConfig) -> Self {
        Self {
            config,
            state: Mutex::new(HttpMcpConnectionState::default()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, BridgeClientError> {
        let response = self.send_request("tools/list", json!({}))?;
        let tools_value = response
            .get("tools")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        serde_json::from_value(tools_value).map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: format!("decode tools/list response failed: {error}"),
        })
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn ensure_initialized(
        &self,
        state: &mut HttpMcpConnectionState,
    ) -> Result<(), BridgeClientError> {
        if state.initialized {
            return Ok(());
        }

        let request_id = self.next_request_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "magi-bridge-client",
                    "version": "1.0.0"
                }
            }
        });
        let response =
            execute_mcp_http_post(self.config.clone(), request, None, None, Some(request_id))?;
        let response_value = decode_mcp_http_response(&response, request_id)?;
        validate_jsonrpc_response(&response_value, request_id)?;
        let result = response_value
            .get("result")
            .ok_or_else(|| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: "MCP initialize response missing 'result' field".to_string(),
            })?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("2024-11-05")
            .to_string();
        let session_id = response.session_id;

        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        let notification_response = execute_mcp_http_post(
            self.config.clone(),
            notification,
            session_id.clone(),
            Some(protocol_version.clone()),
            None,
        )?;
        ensure_mcp_http_success(&notification_response)?;

        state.session_id = session_id;
        state.protocol_version = Some(protocol_version);
        state.initialized = true;
        Ok(())
    }

    fn send_request(&self, method: &str, params: Value) -> Result<Value, BridgeClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| mcp_transport_error("HTTP MCP state mutex poisoned".to_string()))?;
        self.ensure_initialized(&mut state)?;

        let request_id = self.next_request_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        });
        let response = execute_mcp_http_post(
            self.config.clone(),
            request,
            state.session_id.clone(),
            state.protocol_version.clone(),
            Some(request_id),
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                reset_http_mcp_state(&mut state);
                return Err(error);
            }
        };
        let response_value = match decode_mcp_http_response(&response, request_id) {
            Ok(value) => value,
            Err(error) => {
                reset_http_mcp_state(&mut state);
                return Err(error);
            }
        };
        if let Err(error) = validate_jsonrpc_response(&response_value, request_id) {
            if matches!(
                error,
                BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol | BridgeErrorLayer::Transport,
                    ..
                }
            ) {
                reset_http_mcp_state(&mut state);
            }
            return Err(error);
        }
        response_value
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: "MCP response missing 'result' field".to_string(),
            })
    }
}

impl McpBridgeClient for StreamableHttpMcpBridgeClient {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError> {
        let input = parse_mcp_tool_arguments(&request.input)?;
        let result = self.send_request(
            "tools/call",
            json!({
                "name": request.tool_name,
                "arguments": input
            }),
        )?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let payload = result
            .get("content")
            .map(extract_text_content)
            .unwrap_or_else(|| result.to_string());
        Ok(BridgeResponse {
            ok: !is_error,
            payload,
        })
    }
}

/// API 与 daemon 连接池共用的 MCP 客户端。
#[derive(Debug)]
pub enum McpServerClient {
    Stdio(StdioMcpBridgeClient),
    StreamableHttp(StreamableHttpMcpBridgeClient),
}

impl McpServerClient {
    pub fn new(config: McpServerConnectionConfig) -> Self {
        match config {
            McpServerConnectionConfig::Stdio(config) => {
                Self::Stdio(StdioMcpBridgeClient::new(config))
            }
            McpServerConnectionConfig::StreamableHttp(config) => {
                Self::StreamableHttp(StreamableHttpMcpBridgeClient::new(config))
            }
        }
    }

    pub fn from_stdio(config: McpServerConfig) -> Self {
        Self::Stdio(StdioMcpBridgeClient::new(config))
    }

    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, BridgeClientError> {
        match self {
            Self::Stdio(client) => client.list_tools(),
            Self::StreamableHttp(client) => client.list_tools(),
        }
    }
}

impl McpBridgeClient for McpServerClient {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError> {
        match self {
            Self::Stdio(client) => client.call_tool(request),
            Self::StreamableHttp(client) => client.call_tool(request),
        }
    }
}

#[derive(Debug)]
struct McpHttpResponse {
    status: u16,
    content_type: Option<String>,
    session_id: Option<String>,
    body: String,
}

fn execute_mcp_http_post(
    config: HttpMcpServerConfig,
    body: Value,
    session_id: Option<String>,
    protocol_version: Option<String>,
    expected_response_id: Option<u64>,
) -> Result<McpHttpResponse, BridgeClientError> {
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(config.request_timeout.min(Duration::from_secs(10)))
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| {
                mcp_transport_error(format!("build HTTP MCP client failed: {error}"))
            })?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in config.headers {
            let name =
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                    BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: format!("invalid HTTP MCP header name: {error}"),
                    }
                })?;
            let value = reqwest::header::HeaderValue::from_str(&value).map_err(|error| {
                BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("invalid HTTP MCP header value: {error}"),
                }
            })?;
            headers.insert(name, value);
        }
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
        );
        if let Some(session_id) = session_id {
            headers.insert(
                reqwest::header::HeaderName::from_static("mcp-session-id"),
                reqwest::header::HeaderValue::from_str(&session_id).map_err(|error| {
                    BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: format!("invalid MCP session id: {error}"),
                    }
                })?,
            );
        }
        if let Some(protocol_version) = protocol_version {
            headers.insert(
                reqwest::header::HeaderName::from_static("mcp-protocol-version"),
                reqwest::header::HeaderValue::from_str(&protocol_version).map_err(|error| {
                    BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: format!("invalid MCP protocol version: {error}"),
                    }
                })?,
            );
        }

        let response = client
            .post(&config.url)
            .headers(headers)
            .json(&body)
            .send()
            .map_err(|error| mcp_transport_error(format!("HTTP MCP request failed: {error}")))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let is_event_stream = content_type.as_deref().is_some_and(|content_type| {
            content_type
                .to_ascii_lowercase()
                .contains("text/event-stream")
        });
        let body = if !(200..300).contains(&status) {
            response.text().map_err(|error| {
                mcp_transport_error(format!("read HTTP MCP response failed: {error}"))
            })?
        } else {
            match expected_response_id {
                None => String::new(),
                Some(expected_response_id) if is_event_stream => {
                    read_matching_mcp_sse_response(response, expected_response_id)?
                }
                Some(_) => response.text().map_err(|error| {
                    mcp_transport_error(format!("read HTTP MCP response failed: {error}"))
                })?,
            }
        };
        Ok(McpHttpResponse {
            status,
            content_type,
            session_id,
            body,
        })
    })
    .join()
    .map_err(|_| mcp_transport_error("HTTP MCP request thread panicked".to_string()))?
}

fn read_matching_mcp_sse_response(
    response: reqwest::blocking::Response,
    expected_id: u64,
) -> Result<String, BridgeClientError> {
    let mut reader = BufReader::new(response);
    let mut data_lines = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| mcp_transport_error(format!("read HTTP MCP SSE failed: {error}")))?;
        if read == 0 {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("HTTP MCP SSE stream ended before request id {expected_id}"),
            });
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if data_lines.is_empty() {
                continue;
            }
            let data = data_lines.join("\n");
            data_lines.clear();
            let value = serde_json::from_str::<Value>(&data).map_err(|error| {
                BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("decode HTTP MCP SSE data failed: {error}"),
                }
            })?;
            if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
                return Ok(format!("data: {data}\n\n"));
            }
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }
}

fn ensure_mcp_http_success(response: &McpHttpResponse) -> Result<(), BridgeClientError> {
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    let detail = response.body.chars().take(500).collect::<String>();
    Err(mcp_transport_error(format!(
        "HTTP MCP server returned status {}{}",
        response.status,
        if detail.trim().is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )))
}

fn decode_mcp_http_response(
    response: &McpHttpResponse,
    expected_id: u64,
) -> Result<Value, BridgeClientError> {
    ensure_mcp_http_success(response)?;
    if response.body.trim().is_empty() {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: "HTTP MCP response body is empty".to_string(),
        });
    }
    let is_event_stream = response
        .content_type
        .as_deref()
        .is_some_and(|content_type| {
            content_type
                .to_ascii_lowercase()
                .contains("text/event-stream")
        })
        || response.body.trim_start().starts_with("data:")
        || response.body.trim_start().starts_with("event:");
    if is_event_stream {
        return decode_mcp_sse_response(&response.body, expected_id);
    }
    serde_json::from_str(&response.body).map_err(|error| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Protocol,
        code: None,
        message: format!("decode HTTP MCP JSON response failed: {error}"),
    })
}

fn decode_mcp_sse_response(body: &str, expected_id: u64) -> Result<Value, BridgeClientError> {
    let mut data_lines = Vec::new();
    let mut values = Vec::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data_lines.is_empty() {
                let data = data_lines.join("\n");
                let value = serde_json::from_str::<Value>(&data).map_err(|error| {
                    BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: format!("decode HTTP MCP SSE data failed: {error}"),
                    }
                })?;
                values.push(value);
                data_lines.clear();
            }
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }
    values
        .into_iter()
        .find(|value| value.get("id").and_then(Value::as_u64) == Some(expected_id))
        .ok_or_else(|| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: format!("HTTP MCP SSE response missing request id {expected_id}"),
        })
}

fn reset_http_mcp_state(state: &mut HttpMcpConnectionState) {
    state.initialized = false;
    state.session_id = None;
    state.protocol_version = None;
}

fn parse_mcp_tool_arguments(input: &str) -> Result<Value, BridgeClientError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: "MCP tool arguments must be a JSON object, got empty input".to_string(),
        });
    }

    let value =
        serde_json::from_str::<Value>(trimmed).map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: format!("MCP tool arguments must be valid JSON: {error}"),
        })?;

    if !value.is_object() {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: "MCP tool arguments must be a JSON object".to_string(),
        });
    }

    Ok(value)
}

impl Drop for StdioMcpBridgeClient {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.connection.lock()
            && let Some(mut conn) = guard.take()
        {
            terminate_mcp_connection(&mut conn);
        }
    }
}

/// Information about a tool exposed by an MCP server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub annotations: Option<Value>,
}
// --- Internal helpers

fn send_json(writer: &mut impl Write, value: &Value) -> Result<(), BridgeClientError> {
    let json = serde_json::to_string(value).map_err(|error| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Protocol,
        code: None,
        message: format!("serialize MCP request failed: {error}"),
    })?;
    writeln!(writer, "{json}")
        .map_err(|error| mcp_transport_error(format!("write to MCP server failed: {error}")))?;
    writer
        .flush()
        .map_err(|error| mcp_transport_error(format!("flush MCP server stdin failed: {error}")))?;
    Ok(())
}

fn spawn_mcp_stdout_reader(stdout: std::process::ChildStdout) -> Receiver<Result<Value, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            let read = match reader.read_line(&mut line) {
                Ok(read) => read,
                Err(error) => {
                    let _ = sender.send(Err(format!("read from MCP server failed: {error}")));
                    return;
                }
            };
            if read == 0 {
                let _ = sender.send(Err(
                    "MCP server closed stdout before sending response".to_string()
                ));
                return;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(error) => {
                    let _ = sender.send(Err(format!(
                        "parse MCP response failed: {error}; raw={trimmed}"
                    )));
                    return;
                }
            };
            if value.get("id").is_some() && sender.send(Ok(value)).is_err() {
                return;
            }
        }
    });
    receiver
}

fn receive_json_response(
    responses: &Receiver<Result<Value, String>>,
    timeout: Duration,
) -> Result<Value, BridgeClientError> {
    match responses.recv_timeout(timeout) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(mcp_transport_error(message)),
        Err(RecvTimeoutError::Timeout) => Err(mcp_transport_error(format!(
            "MCP request timeout after {} ms",
            timeout.as_millis()
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(mcp_transport_error(
            "MCP response channel disconnected".to_string(),
        )),
    }
}

fn terminate_mcp_connection(connection: &mut McpConnection) {
    let _ = connection.child.terminate();
}

fn validate_jsonrpc_response(response: &Value, expected_id: u64) -> Result<(), BridgeClientError> {
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: format!(
                "MCP response has unexpected jsonrpc version: {}",
                response
                    .get("jsonrpc")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<missing>".to_string())
            ),
        });
    }

    let response_id = response.get("id").and_then(Value::as_u64);
    if response_id != Some(expected_id) {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: None,
            message: format!(
                "MCP response id mismatch: expected={expected_id} actual={}",
                response
                    .get("id")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<missing>".to_string())
            ),
        });
    }

    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        let _data = error.get("data").cloned();

        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(code),
            message: format!("MCP server error [{code}]: {message}"),
        });
    }

    Ok(())
}

fn extract_text_content(content: &Value) -> String {
    match content.as_array() {
        Some(items) => {
            let text_parts: Vec<&str> = items
                .iter()
                .filter_map(|item| {
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("text");
                    if item_type == "text" {
                        item.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .collect();
            if text_parts.is_empty() {
                content.to_string()
            } else {
                text_parts.join("\n")
            }
        }
        None => content.to_string(),
    }
}

fn mcp_transport_error(message: String) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: None,
        message,
    }
}

fn spawn_mcp_stderr_drain(stderr: std::process::ChildStderr) {
    let _ = std::thread::Builder::new()
        .name("magi-mcp-stderr-drain".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
}

fn read_non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn mcp_server_config_from_env_returns_none_without_command() {
        let saved = env::var(MCP_SERVER_COMMAND_ENV).ok();
        // SAFETY: test code only
        unsafe { env::remove_var(MCP_SERVER_COMMAND_ENV) };

        let config = McpServerConfig::from_env();
        assert!(
            config.is_none(),
            "from_env() should return None without MAGI_MCP_SERVER_COMMAND"
        );

        if let Some(value) = saved {
            // SAFETY: test code only
            unsafe { env::set_var(MCP_SERVER_COMMAND_ENV, value) };
        }
    }

    #[test]
    fn extract_text_content_extracts_text_items() {
        let content = json!([
            {"type": "text", "text": "hello"},
            {"type": "text", "text": "world"},
        ]);
        assert_eq!(extract_text_content(&content), "hello\nworld");
    }

    #[test]
    fn extract_text_content_falls_back_for_non_text_items() {
        let content = json!([
            {"type": "image", "data": "base64data"},
        ]);
        // Should fall back to stringified JSON since no text items
        let result = extract_text_content(&content);
        assert!(result.contains("image"));
    }

    #[test]
    fn extract_text_content_handles_scalar_value() {
        let content = json!("plain text");
        assert_eq!(extract_text_content(&content), "\"plain text\"");
    }

    #[test]
    fn validate_jsonrpc_response_accepts_valid_response() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": []}
        });
        assert!(validate_jsonrpc_response(&response, 1).is_ok());
    }

    #[test]
    fn validate_jsonrpc_response_rejects_wrong_id() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": {}
        });
        let error =
            validate_jsonrpc_response(&response, 1).expect_err("wrong id should be rejected");
        match error {
            BridgeClientError::CallFailed { message, .. } => {
                assert!(message.contains("id mismatch"), "message was: {message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn validate_jsonrpc_response_surfaces_server_error() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32601,
                "message": "method not found"
            }
        });
        let error =
            validate_jsonrpc_response(&response, 1).expect_err("server error should be surfaced");
        match error {
            BridgeClientError::CallFailed {
                layer,
                code,
                message,
            } => {
                assert_eq!(layer, BridgeErrorLayer::RemoteBusiness);
                assert_eq!(code, Some(-32601));
                assert!(
                    message.contains("method not found"),
                    "message was: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn stdio_mcp_client_connects_to_mock_mcp_server() {
        // Create a shell script that acts as a minimal MCP server:
        // 1. Reads initialize request, responds with capabilities
        // 2. Reads initialized notification (no response needed)
        // 3. Reads tools/call request, responds with content
        let script = r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock-mcp","version":"0.1.0"}}}\n' "$id"
            ;;
        notifications/initialized)
            # notification, no response
            ;;
        tools/call)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"mock tool result"}]}}\n' "$id"
            ;;
        tools/list)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"mock.echo","description":"A mock echo tool"}]}}\n' "$id"
            ;;
        *)
            printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
            ;;
    esac
done
"#;

        let config = McpServerConfig::new("sh", vec!["-c".to_string(), script.to_string()]);
        let client = StdioMcpBridgeClient::new(config);

        // Test tool call
        let response = client
            .call_tool(McpToolCallRequest {
                server_name: "mock-mcp".to_string(),
                tool_name: "mock.echo".to_string(),
                input: "{}".to_string(),
            })
            .expect("tool call should succeed against mock MCP server");

        assert!(response.ok);
        assert_eq!(response.payload, "mock tool result");
    }

    #[test]
    fn stdio_mcp_client_list_tools_returns_tool_info() {
        let script = r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock-mcp","version":"0.1.0"}}}\n' "$id"
            ;;
        notifications/initialized)
            ;;
        tools/list)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"test.tool","description":"A test tool","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]}}\n' "$id"
            ;;
        *)
            printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
            ;;
    esac
done
"#;

        let config = McpServerConfig::new("sh", vec!["-c".to_string(), script.to_string()]);
        let client = StdioMcpBridgeClient::new(config);

        let tools = client
            .list_tools()
            .expect("list_tools should succeed against mock MCP server");

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "test.tool");
        assert_eq!(tools[0].description.as_deref(), Some("A test tool"));
        assert!(tools[0].input_schema.is_some());
        assert_eq!(
            tools[0]
                .annotations
                .as_ref()
                .and_then(|value| value.get("readOnlyHint"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn stdio_mcp_client_reports_spawn_failure() {
        let config =
            McpServerConfig::new("/nonexistent/mcp-server-binary-that-does-not-exist", vec![]);
        let client = StdioMcpBridgeClient::new(config);

        let error = client
            .call_tool(McpToolCallRequest {
                server_name: "missing".to_string(),
                tool_name: "test".to_string(),
                input: "{}".to_string(),
            })
            .expect_err("nonexistent binary should fail to spawn");

        match error {
            BridgeClientError::CallFailed { layer, .. } => {
                assert_eq!(layer, BridgeErrorLayer::Transport);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn stdio_mcp_client_times_out_and_invalidates_hung_connection() {
        let script = r#"
while IFS= read -r line; do
    sleep 2
done
"#;
        let config = McpServerConfig::new("sh", vec!["-c".to_string(), script.to_string()])
            .with_request_timeout(std::time::Duration::from_millis(100));
        let client = StdioMcpBridgeClient::new(config);

        let started = std::time::Instant::now();
        let error = client
            .list_tools()
            .expect_err("hung MCP server should reach the request deadline");

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        match error {
            BridgeClientError::CallFailed { layer, message, .. } => {
                assert_eq!(layer, BridgeErrorLayer::Transport);
                assert!(message.contains("timeout"), "message was: {message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!client.initialized.load(Ordering::Relaxed));
    }

    #[test]
    fn stdio_mcp_client_rejects_invalid_tool_arguments_before_dispatch() {
        let config = McpServerConfig::new("sh", vec!["-c".to_string(), "exit 99".to_string()]);
        let client = StdioMcpBridgeClient::new(config);

        for input in ["", "not json", "[]"] {
            let error = client
                .call_tool(McpToolCallRequest {
                    server_name: "mock-mcp".to_string(),
                    tool_name: "mock.echo".to_string(),
                    input: input.to_string(),
                })
                .expect_err("invalid MCP arguments should be rejected locally");

            match error {
                BridgeClientError::CallFailed { layer, message, .. } => {
                    assert_eq!(layer, BridgeErrorLayer::Protocol);
                    assert!(
                        message.contains("MCP tool arguments"),
                        "message was: {message}"
                    );
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn stdio_mcp_client_handles_error_response_from_tool_call() {
        let script = r#"
while IFS= read -r line; do
    method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)
    id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$method" in
        initialize)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"mock-mcp","version":"0.1.0"}}}\n' "$id"
            ;;
        notifications/initialized)
            ;;
        tools/call)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"tool execution failed: resource not found"}],"isError":true}}\n' "$id"
            ;;
        *)
            printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
            ;;
    esac
done
"#;

        let config = McpServerConfig::new("sh", vec!["-c".to_string(), script.to_string()]);
        let client = StdioMcpBridgeClient::new(config);

        let response = client
            .call_tool(McpToolCallRequest {
                server_name: "mock-mcp".to_string(),
                tool_name: "failing.tool".to_string(),
                input: "{}".to_string(),
            })
            .expect("error tool response should still return a BridgeResponse");

        assert!(!response.ok);
        assert!(response.payload.contains("resource not found"));
    }

    #[test]
    fn streamable_http_mcp_client_initializes_lists_and_calls_tools() {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("mock HTTP MCP listener should bind");
        let address = listener
            .local_addr()
            .expect("mock listener should have address");
        let requests = Arc::new(Mutex::new(Vec::<(BTreeMap<String, String>, Value)>::new()));
        let captured = requests.clone();

        let server = thread::spawn(move || {
            for index in 0..4 {
                let (mut stream, _) = listener
                    .accept()
                    .expect("mock server should accept request");
                let (headers, body) = read_http_json_request(&mut stream);
                captured
                    .lock()
                    .expect("captured request lock should succeed")
                    .push((headers, body.clone()));

                match index {
                    0 => write_http_response(
                        &mut stream,
                        "200 OK",
                        &[
                            ("Content-Type", "application/json"),
                            ("Mcp-Session-Id", "session-test"),
                        ],
                        &json!({
                            "jsonrpc": "2.0",
                            "id": body["id"],
                            "result": {
                                "protocolVersion": "2024-11-05",
                                "capabilities": { "tools": {} },
                                "serverInfo": { "name": "mock-http-mcp", "version": "1.0.0" }
                            }
                        })
                        .to_string(),
                    ),
                    1 => write_http_response(&mut stream, "202 Accepted", &[], ""),
                    2 => write_http_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "text/event-stream")],
                        &format!(
                            "event: message\ndata: {}\n\n",
                            json!({
                                "jsonrpc": "2.0",
                                "id": body["id"],
                                "result": {
                                    "tools": [{
                                        "name": "search.web",
                                        "description": "Search the web",
                                        "inputSchema": { "type": "object" }
                                    }]
                                }
                            })
                        ),
                    ),
                    3 => write_http_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        &json!({
                            "jsonrpc": "2.0",
                            "id": body["id"],
                            "result": {
                                "content": [{ "type": "text", "text": "search result" }]
                            }
                        })
                        .to_string(),
                    ),
                    _ => unreachable!(),
                }
            }
        });

        let client = StreamableHttpMcpBridgeClient::new(HttpMcpServerConfig {
            url: format!("http://{address}/mcp"),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer test".to_string())]),
            request_timeout: Duration::from_secs(2),
        });

        let tools = client
            .list_tools()
            .expect("HTTP MCP tools/list should succeed");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search.web");

        let response = client
            .call_tool(McpToolCallRequest {
                server_name: "web-search-prime".to_string(),
                tool_name: "search.web".to_string(),
                input: r#"{"query":"Magi"}"#.to_string(),
            })
            .expect("HTTP MCP tools/call should succeed");
        assert!(response.ok);
        assert_eq!(response.payload, "search result");

        server
            .join()
            .expect("mock HTTP MCP server should stop cleanly");
        let requests = requests.lock().expect("captured requests should lock");
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].1["method"], json!("initialize"));
        assert_eq!(requests[1].1["method"], json!("notifications/initialized"));
        assert_eq!(requests[2].1["method"], json!("tools/list"));
        assert_eq!(requests[3].1["method"], json!("tools/call"));
        for (index, (headers, _)) in requests.iter().enumerate() {
            assert_eq!(
                headers.get("authorization").map(String::as_str),
                Some("Bearer test")
            );
            if index > 0 {
                assert_eq!(
                    headers.get("mcp-session-id").map(String::as_str),
                    Some("session-test")
                );
                assert_eq!(
                    headers.get("mcp-protocol-version").map(String::as_str),
                    Some("2024-11-05")
                );
            }
        }
    }

    #[test]
    fn streamable_http_mcp_client_returns_before_sse_stream_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener should bind");
        let address = listener
            .local_addr()
            .expect("mock listener should have address");
        let server = thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().expect("mock server should accept");
                let (_, body) = read_http_json_request(&mut stream);
                match index {
                    0 => write_http_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        &json!({
                            "jsonrpc": "2.0",
                            "id": body["id"],
                            "result": {
                                "protocolVersion": "2025-06-18",
                                "capabilities": { "tools": {} },
                                "serverInfo": { "name": "mock", "version": "1.0.0" }
                            }
                        })
                        .to_string(),
                    ),
                    1 => write_http_response(&mut stream, "202 Accepted", &[], ""),
                    2 => {
                        let payload = json!({
                            "jsonrpc": "2.0",
                            "id": body["id"],
                            "result": { "tools": [] }
                        });
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\ndata: {payload}\n\n"
                        );
                        stream
                            .write_all(response.as_bytes())
                            .expect("SSE response should write");
                        stream.flush().expect("SSE response should flush");
                        thread::sleep(Duration::from_secs(1));
                    }
                    _ => unreachable!(),
                }
            }
        });

        let client = StreamableHttpMcpBridgeClient::new(HttpMcpServerConfig {
            url: format!("http://{address}/mcp"),
            headers: BTreeMap::new(),
            request_timeout: Duration::from_secs(2),
        });
        let started = std::time::Instant::now();
        let tools = client.list_tools().expect("SSE tools/list should succeed");

        assert!(tools.is_empty());
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "client waited for SSE stream closure: {:?}",
            started.elapsed()
        );
        server.join().expect("mock server should stop");
    }

    fn read_http_json_request(stream: &mut TcpStream) -> (BTreeMap<String, String>, Value) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("mock stream read timeout should configure");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream
                .read(&mut buffer)
                .expect("mock request should be readable");
            assert!(read > 0, "HTTP request closed before headers completed");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let header_text = String::from_utf8(bytes[..header_end].to_vec())
            .expect("HTTP request headers should be UTF-8");
        let headers = header_text
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect::<BTreeMap<_, _>>();
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .expect("HTTP request should include content-length");
        while bytes.len() - header_end < content_length {
            let read = stream
                .read(&mut buffer)
                .expect("mock request body should be readable");
            assert!(read > 0, "HTTP request closed before body completed");
            bytes.extend_from_slice(&buffer[..read]);
        }
        let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])
            .expect("HTTP MCP request body should be JSON");
        (headers, body)
    }

    fn write_http_response(
        stream: &mut TcpStream,
        status: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(body);
        stream
            .write_all(response.as_bytes())
            .expect("mock HTTP response should write");
    }
}
