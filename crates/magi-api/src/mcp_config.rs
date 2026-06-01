use crate::{errors::ApiError, scope_binding::strip_scope_binding_fields_from_map};
use magi_bridge_client::McpServerConfig;
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

pub(crate) const REDACTED_MCP_ENV_VALUE: &str = "********";

fn unwrap_mcp_server_payload<'a>(request: &'a Value) -> &'a Value {
    request
        .get("server")
        .or_else(|| request.get("updates"))
        .unwrap_or(request)
}

pub(crate) fn mcp_server_entry_id(entry: &Value) -> Option<&str> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| entry.get("serverId").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn normalize_mcp_server_snapshot_entry(entry: &Value) -> Option<Value> {
    let raw = unwrap_mcp_server_payload(entry);
    let mut object = raw.as_object().cloned()?;
    let server_id = object
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| object.get("serverId").and_then(Value::as_str))
        .or_else(|| entry.get("serverId").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();

    object.insert("id".to_string(), serde_json::json!(server_id));
    object.insert("serverId".to_string(), serde_json::json!(server_id));
    if object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        object.insert("name".to_string(), serde_json::json!(server_id));
    }

    if let Some(command) = object
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    {
        object.insert("command".to_string(), serde_json::json!(command));
    } else {
        object.remove("command");
    }

    object.insert("type".to_string(), serde_json::json!("stdio"));
    object.remove("url");
    object.remove("headers");
    strip_scope_binding_fields_from_map(&mut object);
    Some(Value::Object(object))
}

pub(crate) fn normalize_mcp_server_request_entry(request: &Value) -> Result<Value, ApiError> {
    let normalized = normalize_mcp_server_snapshot_entry(request)
        .ok_or_else(|| ApiError::InvalidInput("serverId 不能为空".to_string()))?;
    let command = normalized
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("MCP server 配置中缺少 command".to_string()))?;

    let mut object = normalized.as_object().cloned().unwrap_or_default();
    object.insert("command".to_string(), serde_json::json!(command));
    Ok(Value::Object(object))
}

pub(crate) fn redact_mcp_server_public_entry(entry: Value) -> Value {
    let mut entry = entry;
    if let Some(env) = entry.get_mut("env").and_then(Value::as_object_mut) {
        for value in env.values_mut() {
            if value.as_str().is_some() {
                *value = serde_json::json!(REDACTED_MCP_ENV_VALUE);
            }
        }
    }
    entry
}

pub(crate) fn preserve_redacted_mcp_env_values(entry: &mut Value, existing: Option<&Value>) {
    let Some(existing_env) = existing
        .and_then(|value| value.get("env"))
        .and_then(Value::as_object)
    else {
        return;
    };
    let Some(next_env) = entry.get_mut("env").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, value) in next_env.iter_mut() {
        if value.as_str() == Some(REDACTED_MCP_ENV_VALUE)
            && let Some(existing_value) = existing_env.get(key).and_then(Value::as_str)
        {
            *value = serde_json::json!(existing_value);
        }
    }
}

pub(crate) fn build_mcp_config_from_entry(entry: &Value) -> Option<McpServerConfig> {
    let normalized = normalize_mcp_server_snapshot_entry(entry)?;
    let object = normalized.as_object()?;
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

    Some(McpServerConfig {
        command,
        args,
        working_directory,
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_normalization_rejects_url_only_server() {
        let error = normalize_mcp_server_request_entry(&serde_json::json!({
            "id": "remote-server",
            "url": "https://example.test/mcp"
        }))
        .expect_err("当前运行时没有 HTTP MCP client，不应保存 URL-only 配置");

        match error {
            ApiError::InvalidInput(message) => assert!(message.contains("缺少 command")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn request_normalization_canonicalizes_stdio_server() {
        let entry = normalize_mcp_server_request_entry(&serde_json::json!({
            "id": "stdio-server",
            "command": " npx ",
            "url": "https://example.test/mcp",
            "headers": { "Authorization": "Bearer test" },
            "type": "streamable-http",
            "workspaceId": "workspace-old",
            "workspacePath": "/tmp/old",
            "sessionId": "session-old"
        }))
        .expect("stdio MCP server should normalize");

        assert_eq!(entry["id"], serde_json::json!("stdio-server"));
        assert_eq!(entry["serverId"], serde_json::json!("stdio-server"));
        assert_eq!(entry["command"], serde_json::json!("npx"));
        assert_eq!(entry["type"], serde_json::json!("stdio"));
        assert!(entry.get("url").is_none());
        assert!(entry.get("headers").is_none());
        assert!(entry.get("workspaceId").is_none());
        assert!(entry.get("workspacePath").is_none());
        assert!(entry.get("sessionId").is_none());
    }

    #[test]
    fn snapshot_normalization_keeps_invalid_server_visible_with_canonical_id() {
        let entry = normalize_mcp_server_snapshot_entry(&serde_json::json!({
            "server": {
                "serverId": " legacy ",
                "url": "https://example.test/mcp",
                "workspace_id": "workspace-old",
                "workspace_path": "/tmp/old",
                "session_id": "session-old"
            }
        }))
        .expect("entry with id should remain visible");

        assert_eq!(entry["id"], serde_json::json!("legacy"));
        assert_eq!(entry["serverId"], serde_json::json!("legacy"));
        assert_eq!(entry["name"], serde_json::json!("legacy"));
        assert_eq!(entry["type"], serde_json::json!("stdio"));
        assert!(entry.get("command").is_none());
        assert!(entry.get("url").is_none());
        assert!(entry.get("workspace_id").is_none());
        assert!(entry.get("workspace_path").is_none());
        assert!(entry.get("session_id").is_none());
    }

    #[test]
    fn config_builder_uses_same_normalization() {
        let config = build_mcp_config_from_entry(&serde_json::json!({
            "server": {
                "serverId": "stdio-server",
                "command": " npx ",
                "args": ["-y", "@modelcontextprotocol/server-filesystem"],
                "workingDirectory": " /tmp ",
                "env": {
                    "A": "1",
                    "IGNORED": 2
                }
            }
        }))
        .expect("config should build");

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
    }

    #[test]
    fn public_entry_redacts_env_values_but_keeps_keys() {
        let public = redact_mcp_server_public_entry(serde_json::json!({
            "id": "server",
            "env": {
                "TOKEN": "secret",
                "COUNT": 1
            }
        }));

        assert_eq!(
            public["env"]["TOKEN"],
            serde_json::json!(REDACTED_MCP_ENV_VALUE)
        );
        assert_eq!(public["env"]["COUNT"], serde_json::json!(1));
    }

    #[test]
    fn redacted_env_values_preserve_existing_secret_on_update() {
        let existing = serde_json::json!({
            "id": "server",
            "env": {
                "TOKEN": "secret",
                "OTHER": "old"
            }
        });
        let mut next = serde_json::json!({
            "id": "server",
            "env": {
                "TOKEN": REDACTED_MCP_ENV_VALUE,
                "OTHER": "new"
            }
        });

        preserve_redacted_mcp_env_values(&mut next, Some(&existing));

        assert_eq!(next["env"]["TOKEN"], serde_json::json!("secret"));
        assert_eq!(next["env"]["OTHER"], serde_json::json!("new"));
    }
}
