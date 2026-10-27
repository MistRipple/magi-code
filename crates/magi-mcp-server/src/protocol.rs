//! MCP（Model Context Protocol）的 JSON-RPC 消息形状。
//!
//! 只做「消息形状」这一件事：不含会话状态、认证、策略与传输。所有结果构造都集中在
//! 这里，避免各传输各自拼 JSON。

use serde::Deserialize;
use serde_json::{Value, json};

/// 本服务声明的 MCP 协议版本。
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub const JSONRPC_PARSE_ERROR: i64 = -32_700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32_600;
pub const JSONRPC_METHOD_NOT_FOUND: i64 = -32_601;
pub const JSONRPC_INVALID_PARAMS: i64 = -32_602;
pub const JSONRPC_INTERNAL_ERROR: i64 = -32_603;

/// 一条 JSON-RPC 请求（MCP 的请求与通知共用）。
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
        // 参数里可能带文件正文或命令，日志里不得出现。
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
    /// 通知没有 `id`，不接受任何响应。
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    pub fn response_id(&self) -> Value {
        self.id.clone().unwrap_or(Value::Null)
    }

    /// 必须是 2.0、有方法名，且 `id` 只能是字符串或数字。
    ///
    /// 把 null / 布尔 / 对象当作匿名 id，会让传输层重试与第二次真实调用无法区分。
    pub fn is_well_formed(&self) -> bool {
        self.jsonrpc == "2.0"
            && !self.method.trim().is_empty()
            && self
                .id
                .as_ref()
                .is_none_or(|id| id.is_string() || id.is_number())
    }
}

pub fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

/// MCP 工具级失败：**不是** JSON-RPC 错误，而是 `isError: true` 的工具结果，
/// 让模型与用户看到可读的失败原因。
pub fn tool_failure(text: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": true
    })
}

pub fn tool_success(text: impl Into<String>) -> Value {
    let text = text.into();
    let structured = serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object);
    let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    // ChatGPT 插件使用结构化结果继续推理；直接复用执行结果，文本供其他 MCP 客户端读取。
    if let Some(structured) = structured {
        result["structuredContent"] = structured;
    }
    result
}

/// `initialize` 的返回：声明协议版本、`tools` 能力与服务信息。
pub fn initialize_result(
    server_name: &str,
    server_version: &str,
    instructions: Option<&str>,
) -> Value {
    let mut result = json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        // 工具目录在令牌生命周期内固定，因此不声明 listChanged。
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": server_name, "version": server_version }
    });
    if let Some(instructions) = instructions {
        result["instructions"] = json!(instructions);
    }
    result
}

pub fn ping_result() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_json_preserves_the_same_content_for_structured_and_text_clients() {
        let payload = json!({"status":"succeeded", "content":"probe line\n", "truncated":false});
        let text = payload.to_string();
        let result = tool_success(&text);
        assert_eq!(result["structuredContent"], payload);
        assert_eq!(result["content"][0]["text"], text);
        assert_eq!(result["isError"], false);
        for text in ["plain text", "{truncated", "[1,2]", "null"] {
            let result = tool_success(text);
            assert!(result.get("structuredContent").is_none());
            assert_eq!(result["content"][0]["text"], text);
        }
        assert_eq!(tool_failure("failed")["isError"], true);
    }
}
