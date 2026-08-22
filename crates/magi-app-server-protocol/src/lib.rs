//! Magi App Server 控制协议。
//!
//! 该 crate 只描述客户端与 Magi runtime 之间的消息契约，不持有任何业务状态，
//! 也不依赖 HTTP、WebSocket 或桌面端实现。这样 Desktop、Web 和测试客户端可以
//! 共享同一套序列化边界，避免为每种传输重复定义 DTO。

use serde::Serialize;
use serde_json::Value;

pub mod generated;
pub use generated::{
    AccessProfile, AppServerMethodKind, AppServerMethodSignature, AppServerNotificationMethod,
    AppServerNotificationParams, AppServerRequestMethod, AppServerRequestParams, AppServerResult,
    ApprovalDecision, ApprovalRequestParams, BrowserAccessProfile, BrowserCapabilitySnapshot,
    BrowserHostStatus, BrowserToolAccess, BrowserToolDescriptor, BrowserToolParams,
    BrowserToolResult, BrowserToolResultStatus, BrowserToolsListParams, BrowserToolsListResult,
    CancelRequestParams, CanonicalToolCall, CanonicalTurn, CanonicalTurnEvent,
    CanonicalTurnEventKind, CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnItemStatus,
    CanonicalTurnStatus, CanonicalTurnVisibility, CanonicalWorkerRef, ClientCapabilities,
    ClientInfo, ClientNotification, ClientRequest, ClientResponse, EmptyParams, ErrorObject,
    EventCategory, EventEnvelope, EventNotificationParams, EventResyncReason,
    EventResyncRequiredParams, EventSnapshotParams, EventStreamSnapshot, EventSubscribeParams,
    EventSubscribeResult, InitializeParams, InitializeResult, JsonRpcError, JsonRpcMessage,
    JsonRpcNotification, JsonRpcRequest, JsonRpcRequestId, JsonRpcResponse, PingResult,
    ProtocolVersion, RequestId, ServerCapabilities, ServerNotification, ServerRequest,
    ServerResponse, SessionContextReference, SessionContextReferenceKind, SessionListParams,
    SessionListResult, SessionReadParams, SessionReadResult, SessionScope, SessionSummary,
    SessionTurnImage, TurnQueueInfo, TurnStartKind, TurnStartParams, TurnStartResult,
    TurnStartRoute,
};

pub use generated::{
    APP_SERVER_METHODS, JSONRPC_VERSION, PROTOCOL_MAJOR, PROTOCOL_MINOR, PROTOCOL_NAME,
};

pub const ERROR_INVALID_REQUEST: i32 = -32600;
pub const ERROR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERROR_INVALID_PARAMS: i32 = -32602;
pub const ERROR_INTERNAL: i32 = -32603;
pub const ERROR_NOT_INITIALIZED: i32 = -32000;
pub const ERROR_SERVER_OVERLOADED: i32 = -32001;
pub const ERROR_ALREADY_INITIALIZED: i32 = -32002;
pub const ERROR_REQUEST_CONFLICT: i32 = -32010;
pub const ERROR_SESSION_NOT_FOUND: i32 = -32004;
pub const ERROR_REQUEST_CANCELLED: i32 = -32800;
pub const ERROR_REQUEST_TIMEOUT: i32 = -32801;

impl ProtocolVersion {
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };

    pub fn is_compatible_with(&self, other: &Self) -> bool {
        self.major == other.major && self.minor >= other.minor
    }
}

impl AppServerRequestMethod {
    pub fn parse(method: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == method)
    }
}

impl AppServerNotificationMethod {
    pub fn parse(method: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == method)
    }
}

impl ClientCapabilities {
    pub fn opted_out(&self, method: &str) -> bool {
        self.opt_out_notification_methods
            .iter()
            .any(|candidate| candidate == method)
    }
}

impl ClientRequest {
    pub fn is_initialize(&self) -> bool {
        self.method == "initialize"
    }
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

/// 将已通过协议 Schema 定义的结果类型编码为 JSON-RPC response。
///
/// 业务层不应再手写 `json!` 组装方法结果；动态 JSON 只允许存在于 Schema 明确声明的
/// 开放字段中。序列化失败时返回协议内部错误，而不是把错误吞成空对象。
pub fn typed_response<T: Serialize>(
    id: RequestId,
    result: T,
) -> Result<ServerResponse, ErrorObject> {
    serde_json::to_value(result)
        .map(|value| response(id, value))
        .map_err(|error| ErrorObject::new(ERROR_INTERNAL, format!("响应序列化失败: {error}")))
}

pub fn typed_notification<T: Serialize>(
    method: impl Into<String>,
    params: T,
) -> Result<ServerNotification, ErrorObject> {
    serde_json::to_value(params)
        .map(|value| notification(method, value))
        .map_err(|error| ErrorObject::new(ERROR_INTERNAL, format!("通知序列化失败: {error}")))
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
        request_timeout_ms: None,
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
        if object.get("result").is_some() == object.get("error").is_some() {
            return Err(ProtocolError::InvalidRequest(
                "响应必须且只能包含 result 或 error 之一".to_string(),
            ));
        }
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
    fn response_cannot_contain_result_and_error_at_the_same_time() {
        let error = classify_client_message(&serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": "ambiguous-response",
            "result": {},
            "error": {"code": ERROR_INVALID_PARAMS, "message": "invalid"}
        }))
        .expect_err("JSON-RPC response 不能同时包含 result 和 error");
        assert!(error.to_string().contains("result 或 error"));
    }

    #[test]
    fn unknown_json_rpc_message_fields_are_rejected() {
        let error = classify_client_message(&serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": "unknown-field",
            "method": "ping",
            "params": {},
            "unexpected": true
        }))
        .expect_err("协议消息不能静默接受未知字段");
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

    #[test]
    fn canonical_event_wire_shape_is_generated_and_matches_sse_contract() {
        let event = EventEnvelope {
            event_id: "event-1".to_string(),
            event_type: "session.turn.item.upserted".to_string(),
            category: EventCategory::Domain,
            occurred_at: 42,
            sequence: 7,
            workspace_id: Some("workspace-1".to_string()),
            session_id: Some("session-1".to_string()),
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload: serde_json::json!({
                "canonicalSchemaVersion": "canonical-turn.v1",
                "canonicalEventKind": "turn_item_upsert"
            }),
        };
        let value = serde_json::to_value(event).expect("canonical event must serialize");
        assert_eq!(value["event_id"], "event-1");
        assert_eq!(value["event_type"], "session.turn.item.upserted");
        assert_eq!(value["workspace_id"], "workspace-1");
        assert_eq!(value["occurred_at"], 42);
        assert!(value.get("mission_id").is_none());
    }
}
