//! 上下文窗口预算计算。
//!
//! 该模块以最近一次模型请求返回的当前上下文窗口 token 作为窗口占用量,
//! 而非会话累计花费。面向产品展示时,占用率必须直接等于
//! `tokens_used / context_window`,避免“已用 9.6k / 窗口 272k 却显示 0%”这类
//! 与用户直觉相悖的结果。
//!
//! 模型窗口表使用保守的前缀匹配 + fallback,覆盖 magi 当前支持的两大协议族
//! (Claude 走 Anthropic 200k,其余走 OpenAI 兼容)。无法识别的模型回退到
//! `DEFAULT_CONTEXT_WINDOW`(与 bridge adapter 的默认窗口保持一致)。

/// 始终存在于上下文中的基线 token(系统提示 + 固定工具说明)。
///
/// 仅保留给需要对照 codex 剩余额语义的内部计算使用;产品 UI 的“上下文用量”
/// 不扣除该基线。
pub const BASELINE_TOKENS: i64 = 12_000;

/// codex 默认有效窗口百分比:实际可用窗口 = 解析窗口 * 95%。
pub const EFFECTIVE_CONTEXT_WINDOW_PERCENT: i64 = 95;

/// codex auto-compact 触发阈值:解析窗口的 90%。占用率达到该比例视为危险。
pub const AUTO_COMPACT_PERCENT: i64 = 90;

/// 无法识别模型时的保守 fallback 窗口,与 bridge adapter 默认配置保持一致。
pub const DEFAULT_CONTEXT_WINDOW: i64 = 256_000;

/// 预算告警级别,与前端 `runtimeDiagnostics` 的 tone 对齐。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetWarningLevel {
    /// 占用率低,正常。
    Normal,
    /// 占用率偏高(超过有效窗口),提示注意。
    Notice,
    /// 占用率较高(超过有效窗口与 auto-compact 之间),警告。
    Warning,
    /// 占用率达到 auto-compact 阈值,危险。
    Danger,
}

impl BudgetWarningLevel {
    /// 返回与前端约定一致的稳定字符串标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetWarningLevel::Normal => "normal",
            BudgetWarningLevel::Notice => "notice",
            BudgetWarningLevel::Warning => "warning",
            BudgetWarningLevel::Danger => "danger",
        }
    }
}

/// 单次预算评估结果。所有 token 字段以「解析窗口」为分母口径。
#[derive(Clone, Copy, Debug)]
pub struct ContextBudget {
    /// 模型解析出的总上下文窗口。
    pub context_window: i64,
    /// 最近一次请求返回的当前上下文窗口占用 token。
    pub tokens_used: i64,
    /// 相对总窗口的剩余 token(`context_window - tokens_used`,下限 0)。
    pub remaining_tokens: i64,
    /// 相对总窗口的剩余百分比(0..=100)。
    pub percent_remaining: i64,
    /// 占用率(0.0..=1.0),= tokens_used / context_window,供前端进度条使用。
    pub usage_ratio: f64,
    /// 告警级别。
    pub warning_level: BudgetWarningLevel,
}

/// 解析模型名对应的上下文窗口大小(token)。
///
/// 采用大小写无关的前缀匹配。识别失败回退到 [`DEFAULT_CONTEXT_WINDOW`]。
pub fn resolve_context_window(resolved_model: &str) -> i64 {
    let model = resolved_model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return DEFAULT_CONTEXT_WINDOW;
    }

    // Claude / Anthropic 家族:统一 200k(部分企业版更大,这里取保守通用值)。
    if model.contains("claude") {
        return 200_000;
    }

    // OpenAI gpt-5 / codex 家族:272k。
    if model.starts_with("gpt-5")
        || model.starts_with("codex")
        || model.starts_with("o3")
        || model.starts_with("o4")
    {
        return 272_000;
    }

    // gpt-4.1 系列窗口 1M,但保守取已知稳定值;gpt-4o / gpt-4-turbo 为 128k。
    if model.starts_with("gpt-4.1") {
        return 1_000_000;
    }
    if model.starts_with("gpt-4") {
        return 128_000;
    }

    // OpenAI 兼容代理经常只返回模型名,不返回精确窗口元数据。DeepSeek / Qwen /
    // GLM / Kimi 等第三方或本地兼容模型不要按名称臆测 128k,统一走 Magi 的
    // 默认上下文窗口,与 bridge adapter 保持同一产品口径。
    if model.starts_with("deepseek")
        || model.starts_with("qwen")
        || model.starts_with("glm")
        || model.starts_with("kimi")
    {
        return DEFAULT_CONTEXT_WINDOW;
    }

    // Gemini 家族:1.5/2.x 至少 1M,这里取保守的 1M。
    if model.starts_with("gemini") {
        return 1_000_000;
    }

    DEFAULT_CONTEXT_WINDOW
}

/// codex 口径的剩余百分比计算。
///
/// 等价于 codex `TokenUsage::percent_of_context_window_remaining`:分子分母都
/// 扣除 [`BASELINE_TOKENS`],结果钳制到 0..=100。
pub fn percent_of_context_window_remaining(tokens_used: i64, context_window: i64) -> i64 {
    if context_window <= BASELINE_TOKENS {
        return 0;
    }
    let effective_window = context_window - BASELINE_TOKENS;
    let used = (tokens_used - BASELINE_TOKENS).max(0);
    let remaining = (effective_window - used).max(0);
    ((remaining as f64 / effective_window as f64) * 100.0)
        .clamp(0.0, 100.0)
        .round() as i64
}

/// 综合评估一次上下文预算。
///
/// `tokens_used` 应为最近一次成功请求返回的当前上下文窗口占用 token。
pub fn evaluate_context_budget(tokens_used: i64, context_window: i64) -> ContextBudget {
    let tokens_used = tokens_used.max(0);
    let remaining_tokens = (context_window - tokens_used).max(0);
    let usage_ratio = if context_window > 0 {
        (tokens_used as f64 / context_window as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let percent_remaining = ((1.0 - usage_ratio) * 100.0).clamp(0.0, 100.0).round() as i64;

    // 告警级别按相对「解析窗口」的占用百分比划分:
    //   < effective(95%)        -> Normal
    //   effective..auto_compact  -> Notice / Warning 过渡
    //   >= auto_compact(90%)     -> Danger
    let used_percent_of_window = if context_window > 0 {
        ((tokens_used as f64 / context_window as f64) * 100.0).round() as i64
    } else {
        0
    };
    let warning_level = if used_percent_of_window >= AUTO_COMPACT_PERCENT {
        BudgetWarningLevel::Danger
    } else if used_percent_of_window >= EFFECTIVE_CONTEXT_WINDOW_PERCENT - 15 {
        // 80%..90%:警告区
        BudgetWarningLevel::Warning
    } else if used_percent_of_window >= EFFECTIVE_CONTEXT_WINDOW_PERCENT - 35 {
        // 60%..80%:提示区
        BudgetWarningLevel::Notice
    } else {
        BudgetWarningLevel::Normal
    };

    ContextBudget {
        context_window,
        tokens_used,
        remaining_tokens,
        percent_remaining,
        usage_ratio,
        warning_level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_context_window_matches_known_families() {
        assert_eq!(resolve_context_window("gpt-5-codex"), 272_000);
        assert_eq!(resolve_context_window("codex-mini"), 272_000);
        assert_eq!(resolve_context_window("Claude-3-5-Sonnet"), 200_000);
        assert_eq!(resolve_context_window("gpt-4o"), 128_000);
        assert_eq!(resolve_context_window("gpt-4.1"), 1_000_000);
        assert_eq!(resolve_context_window("gemini-2.0-pro"), 1_000_000);
        assert_eq!(
            resolve_context_window("deepseek-reasoner"),
            DEFAULT_CONTEXT_WINDOW
        );
        assert_eq!(resolve_context_window("glm-5.2"), DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn resolve_context_window_falls_back_on_unknown_or_empty() {
        assert_eq!(resolve_context_window(""), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(resolve_context_window("   "), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(
            resolve_context_window("some-future-model"),
            DEFAULT_CONTEXT_WINDOW
        );
    }

    #[test]
    fn percent_remaining_matches_codex_baseline_semantics() {
        // 占用刚好等于 baseline 时显示满额。
        assert_eq!(
            percent_of_context_window_remaining(BASELINE_TOKENS, 272_000),
            100
        );
        // 占用填满整个窗口时归零。
        assert_eq!(percent_of_context_window_remaining(272_000, 272_000), 0);
        // 窗口不大于 baseline 时直接归零，避免除零/负分母。
        assert_eq!(
            percent_of_context_window_remaining(5_000, BASELINE_TOKENS),
            0
        );
    }

    #[test]
    fn evaluate_context_budget_uses_direct_occupancy_for_product_display() {
        let budget = evaluate_context_budget(9_600, 272_000);
        assert_eq!(budget.tokens_used, 9_600);
        assert_eq!(budget.remaining_tokens, 262_400);
        assert!((budget.usage_ratio - (9_600.0 / 272_000.0)).abs() < 1e-9);
        assert_eq!(budget.percent_remaining, 96);
    }

    #[test]
    fn evaluate_context_budget_classifies_warning_levels() {
        let window = 100_000;
        assert_eq!(
            evaluate_context_budget(50_000, window).warning_level,
            BudgetWarningLevel::Normal
        );
        assert_eq!(
            evaluate_context_budget(65_000, window).warning_level,
            BudgetWarningLevel::Notice
        );
        assert_eq!(
            evaluate_context_budget(85_000, window).warning_level,
            BudgetWarningLevel::Warning
        );
        assert_eq!(
            evaluate_context_budget(95_000, window).warning_level,
            BudgetWarningLevel::Danger
        );
    }

    #[test]
    fn evaluate_context_budget_keeps_ratios_consistent() {
        let budget = evaluate_context_budget(136_000, 272_000);
        assert_eq!(budget.remaining_tokens, 136_000);
        assert_eq!(budget.context_window, 272_000);
        // usage_ratio = tokens_used / context_window，应落在 (0,1)。
        assert!(budget.usage_ratio > 0.0 && budget.usage_ratio < 1.0);
        assert!((budget.usage_ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn evaluate_context_budget_clamps_negative_usage() {
        let budget = evaluate_context_budget(-500, 272_000);
        assert_eq!(budget.tokens_used, 0);
        assert_eq!(budget.remaining_tokens, 272_000);
        assert_eq!(budget.percent_remaining, 100);
        assert_eq!(budget.warning_level, BudgetWarningLevel::Normal);
    }
}
