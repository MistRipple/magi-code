use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBudgetWarningLevel {
    Normal,
    Notice,
    Warning,
    Danger,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBudgetPressure {
    pub token_limit: u64,
    pub token_used: u64,
    pub remaining_tokens: u64,
    pub usage_ratio: f64,
    pub warning_level: RuntimeBudgetWarningLevel,
}

pub fn resolve_runtime_budget_pressure(
    token_used: u64,
    token_limit: u64,
) -> RuntimeBudgetPressure {
    let safe_limit = token_limit.max(1);
    let safe_used = token_used.min(safe_limit);
    let usage_ratio = (safe_used as f64 / safe_limit as f64).clamp(0.0, 1.0);
    let remaining_tokens = safe_limit.saturating_sub(safe_used);

    let warning_level = if usage_ratio >= 0.95 {
        RuntimeBudgetWarningLevel::Danger
    } else if usage_ratio >= 0.85 {
        RuntimeBudgetWarningLevel::Warning
    } else if usage_ratio >= 0.7 {
        RuntimeBudgetWarningLevel::Notice
    } else {
        RuntimeBudgetWarningLevel::Normal
    };

    RuntimeBudgetPressure {
        token_limit: safe_limit,
        token_used: safe_used,
        remaining_tokens,
        usage_ratio,
        warning_level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_usage() {
        let p = resolve_runtime_budget_pressure(5000, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Normal);
        assert_eq!(p.remaining_tokens, 5000);
        assert!((p.usage_ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn notice_usage() {
        let p = resolve_runtime_budget_pressure(7500, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Notice);
    }

    #[test]
    fn warning_usage() {
        let p = resolve_runtime_budget_pressure(8500, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Warning);
    }

    #[test]
    fn danger_usage() {
        let p = resolve_runtime_budget_pressure(9600, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Danger);
    }

    #[test]
    fn zero_limit_clamped() {
        let p = resolve_runtime_budget_pressure(100, 0);
        assert_eq!(p.token_limit, 1);
        assert_eq!(p.token_used, 1);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Danger);
    }

    #[test]
    fn zero_usage() {
        let p = resolve_runtime_budget_pressure(0, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Normal);
        assert_eq!(p.remaining_tokens, 10000);
        assert!((p.usage_ratio).abs() < 1e-9);
    }

    #[test]
    fn boundary_at_seventy_percent() {
        let p = resolve_runtime_budget_pressure(7000, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Notice);
    }

    #[test]
    fn boundary_at_eighty_five_percent() {
        let p = resolve_runtime_budget_pressure(8500, 10000);
        assert_eq!(p.warning_level, RuntimeBudgetWarningLevel::Warning);
    }
}
