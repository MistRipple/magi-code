//! 仅供旧 harness 内部表示工具结果的私有兼容类型。
//! Web 页面不接收该文本协议；外部入口只使用标准 MCP tools/list/tools/call。

use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolResultStatus { Ok, Error }

pub fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn harness_tool_call_id(turn_id: &str, index: usize) -> String {
    format!("magi-{turn_id}-{index}")
}
