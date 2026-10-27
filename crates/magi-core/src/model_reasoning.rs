//! 跨派发、恢复和用量共享的模型选择事实；不保存连接凭据。
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

pub const EFFORTS: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
    ReasoningEffort::Max,
    ReasoningEffort::Ultra,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningTransport {
    Openai,
    AnthropicEffort,
    AnthropicBudget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasoningCapability {
    pub supported_efforts: Vec<ReasoningEffort>,
    pub default_effort: Option<ReasoningEffort>,
    pub transport: ReasoningTransport,
    #[serde(default)]
    pub source: String,
}

impl ReasoningCapability {
    pub fn validate(&mut self) -> Result<(), String> {
        if self.supported_efforts.len() > EFFORTS.len() {
            return Err("推理档位过多".into());
        }
        self.supported_efforts
            .sort_by_key(|value| EFFORTS.iter().position(|item| item == value));
        self.supported_efforts.dedup();
        if self
            .default_effort
            .is_some_and(|value| !self.supported_efforts.contains(&value))
        {
            return Err("默认推理强度必须属于可选档位".into());
        }
        Ok(())
    }

    pub fn reconcile(&self, requested: Option<ReasoningEffort>) -> Option<ReasoningEffort> {
        requested
            .filter(|value| self.supported_efforts.contains(value))
            .or(self.default_effort)
    }
}

/// 每次派发固定模型、能力与强度。连接身份摘要阻止恢复时静默换供应商/账号。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentModelSelection {
    pub engine_id: Option<String>,
    pub binding_revision: u32,
    pub connection_key: String,
    pub model: String,
    pub context_window_tokens: Option<u64>,
    pub capability: ReasoningCapability,
    pub requested_effort: Option<ReasoningEffort>,
    pub effective_effort: Option<ReasoningEffort>,
}
