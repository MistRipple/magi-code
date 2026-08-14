//! 模型上下文窗口解析。
//!
//! 预算、告警和压缩阈值统一由 [`crate::ContextBudgetPolicy`] 负责。本模块只保留
//! 模型名到保守窗口上限的解析，避免再次出现第二套 token 预算实现。

/// 无法识别模型时使用的保守上下文窗口。
pub const DEFAULT_CONTEXT_WINDOW: i64 = 256_000;

/// 解析模型名对应的上下文窗口大小(token)。
pub fn resolve_context_window(resolved_model: &str) -> i64 {
    let model = resolved_model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return DEFAULT_CONTEXT_WINDOW;
    }
    if model.contains("claude") {
        return 200_000;
    }
    if model.starts_with("gpt-5")
        || model.starts_with("codex")
        || model.starts_with("o3")
        || model.starts_with("o4")
    {
        return 272_000;
    }
    if model.starts_with("gpt-4.1") {
        return 1_000_000;
    }
    if model.starts_with("gpt-4") {
        return 128_000;
    }
    if model.starts_with("gemini") {
        return 1_000_000;
    }
    DEFAULT_CONTEXT_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_model_families() {
        assert_eq!(resolve_context_window("gpt-5-codex"), 272_000);
        assert_eq!(resolve_context_window("Claude-3-5-Sonnet"), 200_000);
        assert_eq!(resolve_context_window("gpt-4.1"), 1_000_000);
        assert_eq!(resolve_context_window("gemini-2.0-pro"), 1_000_000);
        assert_eq!(resolve_context_window("glm-5.2"), DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn falls_back_for_unknown_models() {
        assert_eq!(resolve_context_window(""), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(
            resolve_context_window("future-model"),
            DEFAULT_CONTEXT_WINDOW
        );
    }
}
