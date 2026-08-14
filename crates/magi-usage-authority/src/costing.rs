use crate::types::{UsageEventUsageDelta, UsageTokenInput};

pub struct NormalizedUsageTotals {
    pub raw_input_tokens: u64,
    pub raw_output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub net_input_tokens: u64,
    pub net_output_tokens: u64,
    pub total_tokens: u64,
}

pub fn normalize_usage_delta(delta: &UsageEventUsageDelta) -> NormalizedUsageTotals {
    let raw_input_tokens = delta.raw_input_tokens;
    let raw_output_tokens = delta.raw_output_tokens;
    let cache_read_tokens = delta.cache_read_tokens.unwrap_or(0);
    let cache_write_tokens = delta.cache_write_tokens.unwrap_or(0);
    let net_input_tokens = if delta.cache_read_included_in_input {
        raw_input_tokens.saturating_sub(cache_read_tokens)
    } else {
        raw_input_tokens
    };
    let net_output_tokens = raw_output_tokens;
    NormalizedUsageTotals {
        raw_input_tokens,
        raw_output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        net_input_tokens,
        net_output_tokens,
        total_tokens: net_input_tokens.saturating_add(net_output_tokens),
    }
}

pub fn context_window_tokens_from_usage(usage: &UsageTokenInput) -> u64 {
    if let Some(total_tokens) = usage.total_tokens.filter(|tokens| *tokens > 0) {
        return total_tokens;
    }

    let cache_read_tokens = if usage.cache_read_included_in_input {
        0
    } else {
        usage.cache_read_tokens.unwrap_or(0)
    };
    usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(cache_read_tokens)
        .saturating_add(usage.cache_write_tokens.unwrap_or(0))
}

/// 计算 provider 返回的当前请求上下文锚点。
///
/// 该值只用于判断下一次请求距离窗口上限还有多远，不能把 completion token
/// 或累计账单 total 直接当成活动上下文。只有 provider 没有提供 input 维度时，
/// 才退回 total 作为保守估算。
pub fn provider_context_tokens_from_usage(usage: &UsageTokenInput) -> u64 {
    let input_tokens = usage.input_tokens;
    let cache_read_tokens = if usage.cache_read_included_in_input {
        0
    } else {
        usage.cache_read_tokens.unwrap_or(0)
    };
    let context_tokens = input_tokens
        .saturating_add(cache_read_tokens)
        .saturating_add(usage.cache_write_tokens.unwrap_or(0));
    if context_tokens > 0 {
        context_tokens
    } else {
        usage.total_tokens.unwrap_or(0)
    }
}
