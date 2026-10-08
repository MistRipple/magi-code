//! 工具失败载荷的唯一构造入口。
//!
//! 约定见 `docs/builtin-tool-failure-contract.md`：每个失败都有稳定的 `error_code`、
//! 面向模型的 `error`（只描述类别，不带底层错误文本）和 `instruction`（下一步怎么做）；
//! 失败依赖当前状态时附带该状态，模型据此重新提交，而不是凭过期记忆反复试。
//! 内置工具、update_plan、目标工具等所有产生失败结果的地方都从这里构造，避免各写一份 JSON。

use serde_json::{Map, Value};

/// 一次工具失败。`status` 为 `failed`（执行出错）或 `rejected`（被规则拒绝）。
pub struct ToolFailure {
    tool: String,
    rejected: bool,
    error_code: String,
    error: String,
    instruction: Option<String>,
    extra: Map<String, Value>,
}

impl ToolFailure {
    /// `kind` 是失败类别，`error_code` 为 `{tool}_{kind}`。
    pub fn new(tool: &str, kind: &str, error: impl Into<String>) -> Self {
        Self::coded(tool, &format!("{tool}_{kind}"), error)
    }

    /// 错误码不是 `{tool}_{类别}` 形式时使用：同一领域的多个工具共享错误码前缀（如 `git_*`）。
    pub fn coded(tool: &str, error_code: &str, error: impl Into<String>) -> Self {
        Self {
            tool: tool.to_string(),
            rejected: false,
            error_code: error_code.to_string(),
            error: error.into(),
            instruction: None,
            extra: Map::new(),
        }
    }

    pub fn rejected(mut self) -> Self {
        self.rejected = true;
        self
    }

    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.extra.insert(key.to_string(), value.into());
        self
    }

    pub fn into_payload(self) -> String {
        let mut payload = Map::new();
        payload.insert("tool".to_string(), Value::String(self.tool));
        payload.insert(
            "status".to_string(),
            Value::String(if self.rejected { "rejected" } else { "failed" }.to_string()),
        );
        payload.insert("error_code".to_string(), Value::String(self.error_code));
        payload.insert("error".to_string(), Value::String(self.error));
        if let Some(instruction) = self.instruction {
            payload.insert("instruction".to_string(), Value::String(instruction));
        }
        for (key, value) in self.extra {
            payload.insert(key, value);
        }
        Value::Object(payload).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_carries_code_status_error_instruction_and_extra_fields() {
        let value: Value = serde_json::from_str(
            &ToolFailure::new("file_read", "not_found", "路径不存在")
                .instruction("先确认路径")
                .with("extra", "x")
                .into_payload(),
        )
        .expect("json");

        assert_eq!(value["tool"], "file_read");
        assert_eq!(value["status"], "failed");
        assert_eq!(value["error_code"], "file_read_not_found");
        assert_eq!(value["instruction"], "先确认路径");
        assert_eq!(value["extra"], "x");
    }

    #[test]
    fn rejected_failures_use_rejected_status_and_may_omit_the_instruction() {
        let value: Value = serde_json::from_str(
            &ToolFailure::coded("git_push", "git_push_rejected", "被拒绝")
                .rejected()
                .into_payload(),
        )
        .expect("json");

        assert_eq!(value["status"], "rejected");
        assert_eq!(value["error_code"], "git_push_rejected");
        assert!(value.get("instruction").is_none());
    }
}
