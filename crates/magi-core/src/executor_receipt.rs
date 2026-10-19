//! 外部执行器归一化的执行证据；记录事实，不授予 Magi 工具调用或重放资格。
use serde::{Deserialize, Serialize};

/// 失败/取消后的实际观测；只作审计与结果核对，不能授权恢复或工具重放。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutorFailureEvidence {
    pub receipts: Vec<ExecutorReceipt>,
    pub usage: Option<serde_json::Value>,
    pub items: Vec<serde_json::Value>,
    pub resources_settled: bool,
}

impl std::fmt::Debug for ExecutorFailureEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutorFailureEvidence")
            .field("receipt_count", &self.receipts.len())
            .field("usage_observed", &self.usage.is_some())
            .field("item_count", &self.items.len())
            .field("resources_settled", &self.resources_settled)
            .finish()
    }
}

impl ExecutorFailureEvidence {
    pub fn validate(&self) -> Result<(), String> {
        for receipt in &self.receipts {
            receipt.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorCommandStatus {
    Succeeded,
    Failed,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutorCommandEvidence {
    pub command: String,
    pub exit_code: Option<i64>,
    pub status: ExecutorCommandStatus,
    pub output: String,
}

/// 原执行器回执的标准证据投影；原始事件仍由 provider context 保留。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutorReceipt {
    pub executor: String,
    pub execution_id: String,
    pub commands: Vec<ExecutorCommandEvidence>,
}

impl ExecutorReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.executor.trim().is_empty() || self.execution_id.trim().is_empty() {
            return Err("执行器证据缺少 executor 或 execution_id".into());
        }
        for command in &self.commands {
            let expected = match command.exit_code {
                Some(0) => ExecutorCommandStatus::Succeeded,
                Some(_) => ExecutorCommandStatus::Failed,
                None => ExecutorCommandStatus::OutcomeUnknown,
            };
            if command.command.trim().is_empty() || command.status != expected {
                return Err("执行器命令缺少正文或状态与真实退出码不符".into());
            }
        }
        Ok(())
    }
}
