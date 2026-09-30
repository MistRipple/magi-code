//! MCP（Model Context Protocol）消息形状与桥接工具定义（设计基线 §5.7.3）。
//!
//! `magi-web-harness` 暴露的**桥接工具集是固定的两个**：`magi_tool_inventory`
//! 与 `magi_tool_call`。实际可调用的工具完全由本次 `request.tools` 决定，而它
//! 来自 Magi 的 registry 与策略；桥接工具 schema 若必须变更，发布新的连接器名
//! 并由 Magi 自动替换配置，不复用旧连接器。
//!
//! 本模块只做「JSON-RPC 与 MCP 消息形状」这一件事，不含会话状态与传输。

use serde::Deserialize;
use serde_json::{Value, json};

/// 本 harness 声明的 MCP 协议版本。
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// 桥接工具：检索本次调用允许的工具。
pub const BRIDGE_TOOL_INVENTORY: &str = "magi_tool_inventory";

/// 桥接工具：把一次工具调用转成 `ChatToolCall` 并挂起等待结果。
pub const BRIDGE_TOOL_CALL: &str = "magi_tool_call";

pub const JSONRPC_PARSE_ERROR: i64 = -32_700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32_600;
pub const JSONRPC_METHOD_NOT_FOUND: i64 = -32_601;
pub const JSONRPC_INVALID_PARAMS: i64 = -32_602;
pub const JSONRPC_INTERNAL_ERROR: i64 = -32_603;

/// 输入：一条 JSON-RPC 请求（MCP 的请求 / 通知共用）。
#[derive(Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl std::fmt::Debug for JsonRpcRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsonRpcRequest")
            .field("jsonrpc", &self.jsonrpc)
            .field("id", &self.id)
            .field("method", &self.method)
            .field("params", &self.params.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl JsonRpcRequest {
    /// 是否是通知（没有 `id`）。通知不接受任何响应。
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// 响应用的 `id`：通知没有 id，这里统一退化为 `null`。
    pub fn response_id(&self) -> Value {
        self.id.clone().unwrap_or(Value::Null)
    }

    /// 该请求的结构是否合法（必须是 2.0 且有方法名）。
    pub fn is_well_formed(&self) -> bool {
        self.jsonrpc == "2.0"
            && !self.method.trim().is_empty()
            // JSON-RPC request ids are strings or numbers.  Treating null,
            // booleans, and objects as anonymous ids would make a transport
            // retry impossible to distinguish from a second real call.
            && self
                .id
                .as_ref()
                .is_none_or(|id| id.is_string() || id.is_number())
    }
}

/// 成功响应。
pub fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// 失败响应。
pub fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

/// MCP 工具级失败：**不是** JSON-RPC 错误，而是 `isError: true` 的工具结果。
///
/// 站点侧与隧道侧的超时、令牌失效、工具名不在清单里都走这里，让模型看到
/// 可读的失败原因（设计基线 §5.7.3）。
pub fn tool_failure(text: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": true
    })
}

/// MCP 工具级成功。
pub fn tool_success(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": false })
}

/// 把 Magi 的工具执行结果渲染成 MCP 工具结果。
pub fn tool_outcome(outcome: &crate::harness::ToolCallOutcome) -> Value {
    use crate::protocol::ToolResultStatus;
    json!({
        "content": [{ "type": "text", "text": outcome.body }],
        "isError": matches!(outcome.status, ToolResultStatus::Error)
    })
}

/// `initialize` 的返回：声明协议版本与 `tools` 能力。
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "magi-web-harness", "version": crate::harness::HARNESS_REVISION }
    })
}

/// `tools/list` 的返回：固定两个桥接工具。
pub fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": BRIDGE_TOOL_INVENTORY,
                "description": "检索本次回复允许调用的工具：返回名称、说明与参数 schema。只读。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "turn_token": { "type": "string", "description": "重锚块中下发的 turn 令牌，原样回带" },
                        "query": { "type": "string", "description": "按名称 / 说明过滤，可省略" },
                        "offset": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["turn_token"],
                    "additionalProperties": false
                }
            },
            {
                "name": BRIDGE_TOOL_CALL,
                "description": "调用一个本次回复允许的工具；调用会挂起直到 Magi 返回结果，随后你可以在同一条回复里继续。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "turn_token": { "type": "string", "description": "重锚块中下发的 turn 令牌，原样回带" },
                        "name": { "type": "string", "description": "工具名，必须来自 magi_tool_inventory" },
                        "arguments": { "type": "object", "description": "该工具的参数对象" }
                    },
                    "required": ["turn_token", "name"],
                    "additionalProperties": false
                }
            }
        ]
    })
}

/// `ping` 的返回。
pub fn ping_result() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_tool_set_is_frozen_to_two_tools() {
        let value = tools_list_result();
        let tools = value["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], BRIDGE_TOOL_INVENTORY);
        assert_eq!(tools[1]["name"], BRIDGE_TOOL_CALL);
        for tool in tools {
            let required = tool["inputSchema"]["required"]
                .as_array()
                .expect("required");
            assert!(required.iter().any(|value| value == "turn_token"));
        }
    }

    #[test]
    fn tool_failures_are_tool_results_not_protocol_errors() {
        let value = tool_failure("magi_tool_timeout");
        assert_eq!(value["isError"], true);
        assert!(value.get("error").is_none());
    }

    #[test]
    fn notifications_have_no_id_and_no_response() {
        let request: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .expect("parse");
        assert!(request.is_notification());
        assert_eq!(request.response_id(), Value::Null);
    }
}
