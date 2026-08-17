//! 上下文压力的唯一权威模型。
//!
//! 该模块只负责 token 语义、模型绑定和窗口预算计算，不读取 transcript、不执行
//! 模型调用，也不负责事件投影。运行时必须把当前活动上下文转换为本模块的输入，
//! 然后把返回的 [`ContextPressureSnapshot`] 作为后续唯一事实源。

use serde::{Deserialize, Serialize};

/// 默认主动压缩比例。它不是硬上限，最终阈值还要扣除输出和恢复预留。
pub const DEFAULT_PROACTIVE_THRESHOLD_PERCENT: u64 = 85;
/// 默认保留近期完整历史的比例。
pub const DEFAULT_RETAINED_HISTORY_PERCENT: u64 = 18;
/// 恢复压缩和当前请求的最小安全余量。
pub const DEFAULT_RECOVERY_BUFFER_TOKENS: u64 = 13_000;
/// 没有模型输出元数据时的保守输出预留。
pub const DEFAULT_RESPONSE_RESERVE_TOKENS: u64 = 20_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelIdentitySnapshot {
    pub provider: String,
    pub model: String,
    pub binding_revision: u32,
}

impl ModelIdentitySnapshot {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        binding_revision: u32,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            binding_revision,
        }
    }

    pub fn stable_key(&self) -> String {
        format!("{}:{}:{}", self.provider, self.model, self.binding_revision)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMeasurement {
    Provider,
    Estimated,
    Compacted,
}

impl ContextMeasurement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Estimated => "estimated",
            Self::Compacted => "compacted",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPressureLevel {
    Normal,
    Notice,
    Warning,
    CompactionDue,
    Overflow,
}

impl ContextPressureLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::CompactionDue => "compaction_due",
            Self::Overflow => "overflow",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextBudgetPolicy {
    pub context_window_tokens: u64,
    pub response_reserve_tokens: u64,
    pub recovery_buffer_tokens: u64,
    pub proactive_threshold_tokens: u64,
    pub hard_request_limit_tokens: u64,
    pub retained_history_target_tokens: u64,
}

impl ContextBudgetPolicy {
    pub fn for_window(
        context_window_tokens: u64,
        max_output_tokens: Option<u64>,
        tool_reserve_tokens: u64,
    ) -> Self {
        let context_window_tokens = context_window_tokens.max(1);
        let response_reserve_tokens = max_output_tokens
            .unwrap_or(DEFAULT_RESPONSE_RESERVE_TOKENS)
            .max(tool_reserve_tokens);
        let recovery_buffer_tokens = DEFAULT_RECOVERY_BUFFER_TOKENS
            .max(context_window_tokens / 50)
            .min(context_window_tokens.saturating_sub(1));
        let hard_request_limit_tokens = context_window_tokens
            .saturating_sub(response_reserve_tokens)
            .max(1);
        let percentage_limit =
            context_window_tokens.saturating_mul(DEFAULT_PROACTIVE_THRESHOLD_PERCENT) / 100;
        let proactive_threshold_tokens = percentage_limit
            .min(hard_request_limit_tokens.saturating_sub(recovery_buffer_tokens))
            .max(1);
        let retained_history_target_tokens =
            context_window_tokens.saturating_mul(DEFAULT_RETAINED_HISTORY_PERCENT) / 100;

        Self {
            context_window_tokens,
            response_reserve_tokens,
            recovery_buffer_tokens,
            proactive_threshold_tokens,
            hard_request_limit_tokens,
            retained_history_target_tokens,
        }
    }

    pub fn level_for(self, projected_request_tokens: u64) -> ContextPressureLevel {
        if projected_request_tokens > self.hard_request_limit_tokens {
            return ContextPressureLevel::Overflow;
        }
        if projected_request_tokens >= self.proactive_threshold_tokens {
            return ContextPressureLevel::CompactionDue;
        }
        let usage_percent =
            projected_request_tokens.saturating_mul(100) / self.context_window_tokens.max(1);
        if usage_percent >= 70 {
            ContextPressureLevel::Warning
        } else if usage_percent >= 55 {
            ContextPressureLevel::Notice
        } else {
            ContextPressureLevel::Normal
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPressureSnapshot {
    pub session_id: String,
    pub thread_id: Option<String>,
    pub model: ModelIdentitySnapshot,
    pub context_window_tokens: u64,
    pub provider_context_tokens: Option<u64>,
    pub projected_request_tokens: u64,
    pub response_reserve_tokens: u64,
    pub recovery_buffer_tokens: u64,
    pub proactive_threshold_tokens: u64,
    pub hard_request_limit_tokens: u64,
    pub retained_history_target_tokens: u64,
    pub measurement: ContextMeasurement,
    pub pressure_level: ContextPressureLevel,
    pub anchor_call_id: Option<String>,
    pub checkpoint_generation: u64,
    pub observed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPressureProjection {
    pub session_id: String,
    pub thread_id: Option<String>,
    pub model: ModelIdentitySnapshot,
    pub policy: ContextBudgetPolicy,
    pub provider_context_tokens: Option<u64>,
    pub projected_request_tokens: u64,
    pub measurement: ContextMeasurement,
    pub anchor_call_id: Option<String>,
    pub checkpoint_generation: u64,
    pub observed_at: u64,
}

impl ContextPressureSnapshot {
    pub fn from_projected(input: ContextPressureProjection) -> Self {
        Self {
            session_id: input.session_id,
            thread_id: input.thread_id,
            model: input.model,
            context_window_tokens: input.policy.context_window_tokens,
            provider_context_tokens: input.provider_context_tokens,
            projected_request_tokens: input.projected_request_tokens,
            response_reserve_tokens: input.policy.response_reserve_tokens,
            recovery_buffer_tokens: input.policy.recovery_buffer_tokens,
            proactive_threshold_tokens: input.policy.proactive_threshold_tokens,
            hard_request_limit_tokens: input.policy.hard_request_limit_tokens,
            retained_history_target_tokens: input.policy.retained_history_target_tokens,
            measurement: input.measurement,
            pressure_level: input.policy.level_for(input.projected_request_tokens),
            anchor_call_id: input.anchor_call_id,
            checkpoint_generation: input.checkpoint_generation,
            observed_at: input.observed_at,
        }
    }

    pub fn usage_ratio(&self) -> f64 {
        (self.projected_request_tokens as f64 / self.context_window_tokens.max(1) as f64)
            .clamp(0.0, 1.0)
    }

    pub fn remaining_tokens(&self) -> u64 {
        self.context_window_tokens
            .saturating_sub(self.projected_request_tokens)
    }

    pub fn anchor_matches(
        &self,
        model: &ModelIdentitySnapshot,
        checkpoint_generation: u64,
    ) -> bool {
        self.model == *model && self.checkpoint_generation == checkpoint_generation
    }
}

/// provider 锚点后新增的模型可见内容。所有调用方都必须先通过该函数合并，
/// 禁止再用累计账单计算当前请求大小。
pub fn project_request_tokens(
    provider_context_tokens: Option<u64>,
    fallback_context_tokens: u64,
    delta_after_anchor_tokens: u64,
) -> (u64, Option<u64>, ContextMeasurement) {
    match provider_context_tokens {
        Some(anchor) if anchor > 0 => (
            anchor.saturating_add(delta_after_anchor_tokens),
            Some(anchor),
            ContextMeasurement::Provider,
        ),
        _ => (
            fallback_context_tokens.saturating_add(delta_after_anchor_tokens),
            None,
            ContextMeasurement::Estimated,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(window: u64) -> ContextBudgetPolicy {
        ContextBudgetPolicy::for_window(window, Some(16_000), 4_000)
    }

    #[test]
    fn policy_uses_output_reserve_and_85_percent_ceiling() {
        let budget = policy(272_000);
        assert_eq!(budget.response_reserve_tokens, 16_000);
        assert_eq!(budget.hard_request_limit_tokens, 256_000);
        assert_eq!(budget.proactive_threshold_tokens, 231_200);
        assert_eq!(budget.retained_history_target_tokens, 48_960);
    }

    #[test]
    fn provider_anchor_is_preferred_and_measurement_is_explicit() {
        let (projected, anchor, measurement) = project_request_tokens(Some(10_000), 2_000, 500);
        assert_eq!(projected, 10_500);
        assert_eq!(anchor, Some(10_000));
        assert_eq!(measurement, ContextMeasurement::Provider);
    }

    #[test]
    fn estimate_is_used_without_a_valid_provider_anchor() {
        let (projected, anchor, measurement) = project_request_tokens(None, 2_000, 500);
        assert_eq!(projected, 2_500);
        assert_eq!(anchor, None);
        assert_eq!(measurement, ContextMeasurement::Estimated);
    }

    #[test]
    fn pressure_levels_are_derived_from_one_policy() {
        let budget = policy(100_000);
        assert_eq!(budget.level_for(1_000), ContextPressureLevel::Normal);
        assert_eq!(budget.level_for(60_000), ContextPressureLevel::Notice);
        assert_eq!(
            budget.level_for(80_000),
            ContextPressureLevel::CompactionDue
        );
        assert_eq!(budget.level_for(257_000), ContextPressureLevel::Overflow);
    }
}
