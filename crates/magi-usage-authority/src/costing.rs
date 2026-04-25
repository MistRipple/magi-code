use crate::types::UsageEventUsageDelta;

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
