use crate::types::{
    BridgeClientError, BridgeErrorLayer, BridgeResponse, McpBridgeClient, McpToolCallRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

const MCP_SERVER_COMMAND_ENV: &str = "MAGI_MCP_SERVER_COMMAND";
const MCP_SERVER_ARGS_ENV: &str = "MAGI_MCP_SERVER_ARGS";
const MCP_SERVER_WORKING_DIR_ENV: &str = "MAGI_MCP_SERVER_WORKING_DIR";

/// Configuration for connecting to a real MCP server via stdio transport.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
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
        })
    }

    /// Create with explicit command and arguments.
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            working_directory: None,
            env: BTreeMap::new(),
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
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
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

        let mut cmd = Command::new(&self.config.command);
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

        let mut child = cmd.spawn().map_err(|error| {
            mcp_transport_error(format!(
                "spawn MCP server {} failed: {error}",
                self.config.command
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            mcp_transport_error(format!(
                "MCP server {} stdin unavailable",
                self.config.command
            ))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            mcp_transport_error(format!(
                "MCP server {} stdout unavailable",
                self.config.command
            ))
        })?;
        if let Some(stderr) = child.stderr.take() {
            spawn_mcp_stderr_drain(stderr);
        }

        let mut conn = McpConnection {
            child,
            reader: BufReader::new(stdout),
            writer: stdin,
        };

        // Step 1: Send initialize request
        let init_id = self.next_request_id();
        let init_request = json!({
            "jsonrpc": "2.0",
            "id": init_id,
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

        send_json(&mut conn.writer, &init_request)?;
        let init_response = read_json_response(&mut conn.reader)?;
        validate_jsonrpc_response(&init_response, init_id)?;

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
        let response = read_json_response(&mut conn.reader)?;
        validate_jsonrpc_response(&response, request_id)?;

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
        if let Ok(mut guard) = self.connection.lock() {
            if let Some(mut conn) = guard.take() {
                let _ = conn.child.kill();
                let _ = conn.child.wait();
            }
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

fn read_json_response(reader: &mut impl BufRead) -> Result<Value, BridgeClientError> {
    // MCP servers may emit log messages or notifications before the response.
    // We need to skip those and find the actual response (which has an "id" field).
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).map_err(|error| {
            mcp_transport_error(format!("read from MCP server failed: {error}"))
        })?;
        if bytes_read == 0 {
            return Err(mcp_transport_error(
                "MCP server closed stdout before sending response".to_string(),
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value: Value =
            serde_json::from_str(trimmed).map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("parse MCP response failed: {error}; raw={trimmed}"),
            })?;

        // Skip notifications (messages without "id") -- these are server-initiated
        // log messages, progress updates, etc.
        if value.get("id").is_some() {
            return Ok(value);
        }
        // Otherwise skip this line and read the next one
    }
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
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"test.tool","description":"A test tool","inputSchema":{"type":"object"}}]}}\n' "$id"
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
}
