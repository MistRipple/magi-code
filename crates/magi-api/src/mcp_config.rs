use crate::errors::ApiError;
use magi_bridge_client::{HttpMcpServerConfig, McpServerConfig, McpServerConnectionConfig};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

const MCP_TRANSPORT_STDIO: &str = "stdio";
const MCP_TRANSPORT_STREAMABLE_HTTP: &str = "streamable-http";
/// MCP server 配置条目的当前 schema；写入请求出现其他字段一律拒绝。
const MCP_SERVER_ENTRY_FIELDS: &[&str] = &[
    "id",
    "name",
    "type",
    "enabled",
    "command",
    "args",
    "env",
    "workingDirectory",
    "url",
    "headers",
    "requestTimeoutMs",
];

pub fn mcp_server_entry_id(entry: &Value) -> Option<&str> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn normalize_mcp_server_snapshot_entry(entry: &Value) -> Option<Value> {
    let server_id = mcp_server_entry_id(entry)?.to_string();
    let mut object = entry.as_object().cloned()?;

    object.insert("id".to_string(), serde_json::json!(server_id));
    if object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        object.insert("name".to_string(), serde_json::json!(server_id));
    }

    let transport = canonical_mcp_transport(&object);
    object.insert("type".to_string(), serde_json::json!(transport));
    match transport {
        MCP_TRANSPORT_STREAMABLE_HTTP => {
            normalize_trimmed_string_field(&mut object, "url");
            normalize_string_map_field(&mut object, "headers");
            object.remove("command");
            object.remove("args");
            object.remove("workingDirectory");
            object.remove("env");
        }
        _ => {
            normalize_trimmed_string_field(&mut object, "command");
            object.remove("url");
            object.remove("headers");
        }
    }
    Some(Value::Object(object))
}

pub(crate) fn normalize_mcp_server_request_entry(request: &Value) -> Result<Value, ApiError> {
    validate_mcp_server_request_schema(request)?;
    let normalized = normalize_mcp_server_snapshot_entry(request)
        .ok_or_else(|| ApiError::InvalidInput("MCP server id 不能为空".to_string()))?;
    let transport = normalized
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(MCP_TRANSPORT_STDIO);
    match transport {
        MCP_TRANSPORT_STREAMABLE_HTTP => {
            let url = normalized
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::InvalidInput("HTTP MCP server 配置中缺少 url".to_string())
                })?;
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| ApiError::InvalidInput("HTTP MCP server 的 url 无效".to_string()))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(ApiError::InvalidInput(
                    "HTTP MCP server 的 url 仅支持 http 或 https".to_string(),
                ));
            }
        }
        _ => {
            normalized
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::InvalidInput("MCP server 配置中缺少 command".to_string())
                })?;
        }
    }
    Ok(normalized)
}

pub fn mcp_server_entry_enabled(entry: &Value) -> bool {
    entry
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

pub fn build_mcp_config_from_entry(entry: &Value) -> Option<McpServerConnectionConfig> {
    let normalized = normalize_mcp_server_snapshot_entry(entry)?;
    let object = normalized.as_object()?;
    let request_timeout = Duration::from_millis(
        object
            .get("requestTimeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(30_000)
            .clamp(1_000, 300_000),
    );
    if object.get("type").and_then(Value::as_str) == Some(MCP_TRANSPORT_STREAMABLE_HTTP) {
        let url = object.get("url")?.as_str()?.trim().to_string();
        if url.is_empty() {
            return None;
        }
        let headers = string_map_from_object(object.get("headers"));
        return Some(McpServerConnectionConfig::StreamableHttp(
            HttpMcpServerConfig {
                url,
                headers,
                request_timeout,
            },
        ));
    }
    let command = object.get("command")?.as_str()?.trim().to_string();
    if command.is_empty() {
        return None;
    }
    let args = object
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let working_directory = object
        .get("workingDirectory")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let env = object
        .get("env")
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(key, value)| value.as_str().map(|s| (key.clone(), s.to_string())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    Some(McpServerConnectionConfig::Stdio(McpServerConfig {
        command,
        args,
        working_directory,
        env,
        request_timeout,
    }))
}

fn canonical_mcp_transport(object: &serde_json::Map<String, Value>) -> &'static str {
    if object.get("type").and_then(Value::as_str) == Some(MCP_TRANSPORT_STREAMABLE_HTTP) {
        MCP_TRANSPORT_STREAMABLE_HTTP
    } else {
        MCP_TRANSPORT_STDIO
    }
}

fn validate_mcp_server_request_schema(request: &Value) -> Result<(), ApiError> {
    let Some(object) = request.as_object() else {
        return Err(ApiError::InvalidInput(
            "MCP server 配置必须是对象".to_string(),
        ));
    };
    if let Some(field) = object
        .keys()
        .find(|field| !MCP_SERVER_ENTRY_FIELDS.contains(&field.as_str()))
    {
        return Err(ApiError::InvalidInput(format!(
            "MCP server 配置不支持字段 {field}"
        )));
    }
    match object.get("type").and_then(Value::as_str) {
        Some(MCP_TRANSPORT_STDIO | MCP_TRANSPORT_STREAMABLE_HTTP) => {}
        Some(other) => {
            return Err(ApiError::InvalidInput(format!(
                "不支持的 MCP transport 类型: {other}"
            )));
        }
        None => {
            return Err(ApiError::InvalidInput(
                "MCP server 配置缺少 type（stdio 或 streamable-http）".to_string(),
            ));
        }
    }
    if let Some(headers) = object.get("headers") {
        let Some(headers) = headers.as_object() else {
            return Err(ApiError::InvalidInput(
                "HTTP MCP server 的 headers 必须是对象".to_string(),
            ));
        };
        if headers.values().any(|value| !value.is_string()) {
            return Err(ApiError::InvalidInput(
                "HTTP MCP server 的 headers 值必须是字符串".to_string(),
            ));
        }
    }
    Ok(())
}

fn normalize_trimmed_string_field(object: &mut serde_json::Map<String, Value>, field: &str) {
    if let Some(value) = object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    {
        object.insert(field.to_string(), serde_json::json!(value));
    } else {
        object.remove(field);
    }
}

fn normalize_string_map_field(object: &mut serde_json::Map<String, Value>, field: &str) {
    let values = string_map_from_object(object.get(field));
    if values.is_empty() {
        object.remove(field);
    } else {
        object.insert(field.to_string(), serde_json::json!(values));
    }
}

fn string_map_from_object(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_normalization_accepts_url_only_http_server() {
        let entry = normalize_mcp_server_request_entry(&serde_json::json!({
            "id": "remote-server",
            "type": "streamable-http",
            "url": " https://example.test/mcp ",
            "headers": {
                "Authorization": "Bearer test"
            },
            "command": "must-be-removed"
        }))
        .expect("HTTP MCP server should normalize");

        assert_eq!(entry["type"], serde_json::json!("streamable-http"));
        assert_eq!(entry["url"], serde_json::json!("https://example.test/mcp"));
        assert_eq!(
            entry["headers"]["Authorization"],
            serde_json::json!("Bearer test")
        );
        assert!(entry.get("command").is_none());
    }

    #[test]
    fn request_normalization_rejects_invalid_http_url() {
        let error = normalize_mcp_server_request_entry(&serde_json::json!({
            "id": "remote-server",
            "type": "streamable-http",
            "url": "file:///tmp/mcp"
        }))
        .expect_err("HTTP MCP should reject non-HTTP URL schemes");

        match error {
            ApiError::InvalidInput(message) => assert!(message.contains("http 或 https")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn request_normalization_keeps_stdio_command_required() {
        let error = normalize_mcp_server_request_entry(&serde_json::json!({
            "id": "stdio-server",
            "type": "stdio"
        }))
        .expect_err("stdio MCP should continue requiring command");

        match error {
            ApiError::InvalidInput(message) => assert!(message.contains("缺少 command")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn request_normalization_canonicalizes_stdio_server() {
        let entry = normalize_mcp_server_request_entry(&serde_json::json!({
            "id": "stdio-server",
            "type": "stdio",
            "command": " npx "
        }))
        .expect("stdio MCP server should normalize");

        assert_eq!(entry["id"], serde_json::json!("stdio-server"));
        assert!(entry.get("serverId").is_none());
        assert_eq!(entry["command"], serde_json::json!("npx"));
        assert_eq!(entry["type"], serde_json::json!("stdio"));
        assert!(entry.get("url").is_none());
        assert!(entry.get("headers").is_none());
    }

    #[test]
    fn request_normalization_rejects_fields_outside_schema() {
        for (request, field) in [
            (
                serde_json::json!({
                    "id": "stdio-server",
                    "type": "stdio",
                    "command": "npx",
                    "connected": true
                }),
                "connected",
            ),
            (
                serde_json::json!({ "server": { "id": "stdio-server", "command": "npx" } }),
                "server",
            ),
        ] {
            let error = normalize_mcp_server_request_entry(&request)
                .expect_err("MCP server 写入只接受当前 schema 字段");
            match error {
                ApiError::InvalidInput(message) => assert!(message.contains(field), "{message}"),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn request_normalization_requires_canonical_transport_type() {
        for request in [
            serde_json::json!({ "id": "remote", "type": "http", "url": "https://example.test/mcp" }),
            serde_json::json!({ "id": "stdio-server", "command": "npx" }),
        ] {
            normalize_mcp_server_request_entry(&request)
                .expect_err("MCP transport 只接受 stdio / streamable-http");
        }
    }

    #[test]
    fn snapshot_normalization_trims_http_entry() {
        let entry = normalize_mcp_server_snapshot_entry(&serde_json::json!({
            "id": " remote ",
            "type": "streamable-http",
            "url": " https://example.test/mcp "
        }))
        .expect("entry with id should remain visible");

        assert_eq!(entry["id"], serde_json::json!("remote"));
        assert_eq!(entry["name"], serde_json::json!("remote"));
        assert_eq!(entry["type"], serde_json::json!("streamable-http"));
        assert!(entry.get("command").is_none());
        assert_eq!(entry["url"], serde_json::json!("https://example.test/mcp"));
    }

    #[test]
    fn config_builder_uses_same_normalization() {
        let config = build_mcp_config_from_entry(&serde_json::json!({
            "id": "stdio-server",
            "command": " npx ",
            "args": ["-y", "@modelcontextprotocol/server-filesystem"],
            "workingDirectory": " /tmp ",
            "requestTimeoutMs": 45_000,
            "env": {
                "A": "1",
                "IGNORED": 2
            }
        }))
        .expect("config should build");

        let McpServerConnectionConfig::Stdio(config) = config else {
            panic!("expected stdio MCP config");
        };
        assert_eq!(config.command, "npx");
        assert_eq!(
            config.args,
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string()
            ]
        );
        assert_eq!(config.working_directory, Some(PathBuf::from("/tmp")));
        assert_eq!(config.env.get("A").map(String::as_str), Some("1"));
        assert!(!config.env.contains_key("IGNORED"));
        assert_eq!(config.request_timeout, Duration::from_millis(45_000));
    }

    #[test]
    fn config_builder_builds_streamable_http_config() {
        let config = build_mcp_config_from_entry(&serde_json::json!({
            "id": "remote-server",
            "type": "streamable-http",
            "url": "https://example.test/mcp",
            "headers": {
                "Authorization": "Bearer test",
                "IGNORED": 2
            },
            "requestTimeoutMs": 45_000
        }))
        .expect("HTTP config should build");

        let McpServerConnectionConfig::StreamableHttp(config) = config else {
            panic!("expected streamable HTTP MCP config");
        };
        assert_eq!(config.url, "https://example.test/mcp");
        assert_eq!(
            config.headers.get("Authorization").map(String::as_str),
            Some("Bearer test")
        );
        assert!(!config.headers.contains_key("IGNORED"));
        assert_eq!(config.request_timeout, Duration::from_millis(45_000));
    }
}
