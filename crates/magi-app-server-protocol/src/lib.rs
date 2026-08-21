//! Magi App Server 控制协议。
//!
//! 该 crate 只描述客户端与 Magi runtime 之间的消息契约，不持有任何业务状态，
//! 也不依赖 HTTP、WebSocket 或桌面端实现。这样 Desktop、Web 和测试客户端可以
//! 共享同一套序列化边界，避免为每种传输重复定义 DTO。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_NAME: &str = "magi.app-server";
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;
pub const JSONRPC_VERSION: &str = "2.0";

pub const ERROR_INVALID_REQUEST: i32 = -32600;
pub const ERROR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERROR_INVALID_PARAMS: i32 = -32602;
pub const ERROR_INTERNAL: i32 = -32603;
pub const ERROR_NOT_INITIALIZED: i32 = -32000;
pub const ERROR_SERVER_OVERLOADED: i32 = -32001;
pub const ERROR_ALREADY_INITIALIZED: i32 = -32002;
pub const ERROR_REQUEST_CONFLICT: i32 = -32010;
pub const ERROR_SESSION_NOT_FOUND: i32 = -32004;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RequestId {
    String(String),
    Number(String),
}

impl RequestId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ProtocolError::InvalidRequest(
                "request id 不能为空".to_string(),
            ));
        }
        Ok(Self::String(value))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::String(value) | Self::Number(value) => value,
        }
    }
}

impl Serialize for RequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::String(value) => serializer.serialize_str(value),
            Self::Number(value) => {
                let number = value
                    .parse::<serde_json::Number>()
                    .map_err(|_| serde::ser::Error::custom("request id 数字无法序列化"))?;
                number.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(value) => RequestId::new(value).map_err(serde::de::Error::custom),
            Value::Number(value) => Ok(Self::Number(value.to_string())),
            _ => Err(serde::de::Error::custom("request id 必须是字符串或数字")),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };

    pub fn is_compatible_with(&self, other: &Self) -> bool {
        self.major == other.major && self.minor >= other.minor
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub desktop_browser_surface: bool,
    #[serde(default)]
    pub browser_tools: bool,
    #[serde(default)]
    pub approvals: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub opt_out_notification_methods: Vec<String>,
}

impl ClientCapabilities {
    pub fn opted_out(&self, method: &str) -> bool {
        self.opt_out_notification_methods
            .iter()
            .any(|candidate| candidate == method)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerCapabilities {
    pub sessions: bool,
    pub turns: bool,
    pub events: bool,
    pub approvals: bool,
    pub browser_tools: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(default)]
    pub protocol: Option<ProtocolVersion>,
    #[serde(default)]
    pub capabilities: ClientCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeResult {
    pub server_info: ClientInfo,
    pub protocol: ProtocolVersion,
    pub runtime_epoch: String,
    pub capabilities: ServerCapabilities,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub id: RequestId,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl ClientRequest {
    pub fn is_initialize(&self) -> bool {
        self.method == "initialize"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientNotification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub id: RequestId,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerNotification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl ErrorObject {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
            retryable: None,
        }
    }

    pub fn retryable(mut self, value: bool) -> Self {
        self.retryable = Some(value);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidRequest(String),
    InvalidParams(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) | Self::InvalidParams(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProtocolError {}

pub fn response(id: RequestId, result: Value) -> ServerResponse {
    ServerResponse {
        jsonrpc: Some(JSONRPC_VERSION.to_string()),
        id,
        result: Some(result),
        error: None,
    }
}

pub fn error_response(id: RequestId, error: ErrorObject) -> ServerResponse {
    ServerResponse {
        jsonrpc: Some(JSONRPC_VERSION.to_string()),
        id,
        result: None,
        error: Some(error),
    }
}

pub fn notification(method: impl Into<String>, params: Value) -> ServerNotification {
    ServerNotification {
        jsonrpc: Some(JSONRPC_VERSION.to_string()),
        method: method.into(),
        params,
    }
}

pub fn classify_client_message(value: &Value) -> Result<ClientMessage, ProtocolError> {
    let object = value
        .as_object()
        .ok_or_else(|| ProtocolError::InvalidRequest("消息必须是 JSON 对象".to_string()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(ProtocolError::InvalidRequest(
            "jsonrpc 必须是 \"2.0\"".to_string(),
        ));
    }
    if object.get("method").is_some() {
        if object.get("id").is_some() {
            serde_json::from_value(value.clone())
                .map(ClientMessage::Request)
                .map_err(|error| ProtocolError::InvalidRequest(error.to_string()))
        } else {
            serde_json::from_value(value.clone())
                .map(ClientMessage::Notification)
                .map_err(|error| ProtocolError::InvalidRequest(error.to_string()))
        }
    } else if object.get("id").is_some()
        && (object.get("result").is_some() || object.get("error").is_some())
    {
        serde_json::from_value(value.clone())
            .map(ClientMessage::Response)
            .map_err(|error| ProtocolError::InvalidRequest(error.to_string()))
    } else {
        Err(ProtocolError::InvalidRequest(
            "消息必须包含 method 或 result/error".to_string(),
        ))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    Request(ClientRequest),
    Notification(ClientNotification),
    Response(ClientResponse),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_request_id_keeps_numeric_json_type_in_response() {
        let message = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": 7,
            "method": "initialize",
            "params": {}
        });
        let ClientMessage::Request(request) = classify_client_message(&message).unwrap() else {
            panic!("应识别为请求")
        };
        assert_eq!(request.id.as_str(), "7");
        assert!(request.is_initialize());
        assert_eq!(
            serde_json::to_value(response(request.id.clone(), serde_json::json!({}))).unwrap()["id"],
            7
        );
        assert_eq!(
            serde_json::to_value(response(request.id, serde_json::json!({}))).unwrap()["jsonrpc"],
            JSONRPC_VERSION
        );
    }

    #[test]
    fn missing_jsonrpc_version_is_rejected() {
        let error = classify_client_message(&serde_json::json!({
            "id": "missing-jsonrpc",
            "method": "ping",
            "params": {}
        }))
        .expect_err("缺少 JSON-RPC 版本必须失败");
        assert!(error.to_string().contains("jsonrpc"));
    }

    #[test]
    fn notification_opt_out_is_exact_match_only() {
        let capabilities = ClientCapabilities {
            opt_out_notification_methods: vec!["item/agentMessage/delta".to_string()],
            ..ClientCapabilities::default()
        };
        assert!(capabilities.opted_out("item/agentMessage/delta"));
        assert!(!capabilities.opted_out("item/agentMessage/delta/extra"));
    }

    #[test]
    fn server_request_response_keeps_request_id_and_error_shape() {
        let id = RequestId::new("approval-1").unwrap();
        let response = error_response(
            id,
            ErrorObject::new(ERROR_INVALID_PARAMS, "参数无效").retryable(false),
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["id"], "approval-1");
        assert_eq!(value["error"]["code"], ERROR_INVALID_PARAMS);
        assert_eq!(value["error"]["retryable"], false);
    }

    #[test]
    fn invalid_message_is_rejected_before_dispatch() {
        let error = classify_client_message(&serde_json::json!({"id": "missing-method"}))
            .expect_err("缺少 method 与 result/error 必须失败");
        assert!(matches!(error, ProtocolError::InvalidRequest(_)));
    }

    #[test]
    fn newer_minor_protocol_is_rejected_but_older_minor_is_compatible() {
        assert!(
            ProtocolVersion::CURRENT.is_compatible_with(&ProtocolVersion {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
            })
        );
        assert!(
            !ProtocolVersion::CURRENT.is_compatible_with(&ProtocolVersion {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR + 1,
            })
        );
        assert!(
            !ProtocolVersion::CURRENT.is_compatible_with(&ProtocolVersion {
                major: PROTOCOL_MAJOR + 1,
                minor: 0,
            })
        );
    }
}
