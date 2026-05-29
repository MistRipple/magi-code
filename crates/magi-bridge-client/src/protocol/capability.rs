//! 模型能力数据表 —— 协议适配器装配 wire 字段的单一数据源。
//!
//! 设计原则：
//! - **数据驱动**：所有 `model_id` → 协议字段形态的映射写在静态
//!   [`CAPABILITY_ENTRIES`] 中。协议适配器（`anthropic.rs` / `openai_chat.rs`）
//!   不再 hardcode 字符串前缀分支，统一调 [`resolve_capability_profile`]
//!   查表。
//! - **未知模型保守默认**：命中不到任何条目的 model id 走
//!   [`UNKNOWN_MODEL_DEFAULT`]：不发 thinking / 不发 reasoning_effort，
//!   不附加任何 beta header，避免向未知端点注入可能被拒绝的字段。
//! - **单实现**：本模块只暴露 [`resolve_capability_profile`] 一个入口，
//!   不并存 HashMap registry + 函数式查表两种路径
//!   （cn-engineering-standard：不让同一功能长期并存多种实现方式）。
//!
//! 后续 Task #121 将由 `anthropic.rs::build_request` / `openai_chat.rs::build_request`
//! 按 profile 字段装配 `thinking.budget_tokens` vs `thinking.effort` vs
//! `thinking.display` 与 `reasoning_effort` 顶层字段，并注入必要 beta header。

/// Anthropic Messages 协议下 `thinking` 字段的载荷形态。
///
/// Anthropic 推出 extended thinking 后协议形态随版本演进：
/// - **3.7 Sonnet / 4.0 / 4.5 / 4.6**：`thinking: { type: "enabled", budget_tokens: N }`，
///   显式声明预算。
/// - **4.7+ 系列**：切换为 Adaptive Thinking only mode，使用
///   `thinking: { type: "enabled", effort: "low|medium|high" }` 替代 budget。
/// - **`Adaptive`**（早期 beta）：`thinking: { type: "adaptive" }`，
///   不带 budget / effort，模型自行决定。
/// - **None**：模型不支持 extended thinking（GPT 系 / Gemini / 未知模型）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingKind {
    None,
    BudgetTokens,
    Effort,
    Adaptive,
}

/// 一条模型能力声明，决定协议适配器如何装配 wire 字段。
///
/// 字段按"装配什么"组织，不混入"如何装配"——后者属于协议适配器职责。
#[derive(Clone, Copy, Debug)]
pub struct ModelCapabilityProfile {
    /// thinking 字段载荷形态（Anthropic 协议适配器读取）。
    pub thinking_kind: ThinkingKind,
    /// 是否支持 `thinking.display: "summarized"`（Anthropic Opus 4.7+ 引入）。
    ///
    /// 启用后服务端返回精简版思考摘要而不是完整思考块，可观测同时降低返回体积。
    pub supports_thinking_display: bool,
    /// 是否支持顶层 `reasoning_effort` 字段（OpenAI Chat Completions 协议）。
    ///
    /// 命中此项的模型（GPT-5 / o3 / o4 系列）才会把
    /// `LlmMessageParams::reasoning_effort` 注入到 OpenAI 兼容请求体。
    pub supports_openai_reasoning_effort: bool,
    /// 默认 thinking budget tokens（仅在 [`ThinkingKind::BudgetTokens`]
    /// 形态下被读取）。`0` 表示无显式默认，由适配器使用其内置 fallback。
    pub default_budget_tokens: u32,
    /// 调用时必须附带的 HTTP beta header（如 Opus 4.7 的
    /// `anthropic-beta: task-budgets-2026-03-13`）。
    ///
    /// 适配器拿到的是 `(name, value)` 二元组列表的扁平形态——header value
    /// 留给适配器拼，因为同一 beta key 可能跨模型共用 value。
    pub beta_headers: &'static [(&'static str, &'static str)],
}

impl ModelCapabilityProfile {
    /// 是否启用 thinking（任意载荷形态均算启用）。
    pub fn supports_thinking(&self) -> bool {
        !matches!(self.thinking_kind, ThinkingKind::None)
    }
}

struct CapabilityEntry {
    /// 模型 id 的匹配前缀。命中规则：`model.starts_with(prefix)`。
    /// 多条命中时取**最长前缀**——确保 `claude-opus-4-7-...` 匹配 4.7 条目
    /// 而非更宽的 `claude-opus-4` 条目。
    prefix: &'static str,
    profile: ModelCapabilityProfile,
}

/// 未命中任何 entry 时的保守默认 profile。
///
/// 任何未知 model id 都被视作"协议层不附加任何可选字段"——这是
/// cn-engineering-standard 的"先正确"原则：未知模型可能是兼容协议但不
/// 支持 thinking 的小模型 / 自部署模型 / 旧版本，盲发 thinking 字段会
/// 直接 400。
pub const UNKNOWN_MODEL_DEFAULT: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::None,
    supports_thinking_display: false,
    supports_openai_reasoning_effort: false,
    default_budget_tokens: 0,
    beta_headers: &[],
};

/// Anthropic Claude 4.7+ 系列（Adaptive Thinking only mode）。
///
/// 协议变化点：
/// - `thinking.budget_tokens` 废弃 → 改用 `thinking: { type: "adaptive" }`，
///   推理强度由顶层 `output_config.effort: "low|medium|high|xhigh"` 控制；
/// - 引入 `thinking.display: "summarized"` 控制思考块返回形态；
/// - 必须附带 beta header `anthropic-beta: task-budgets-2026-03-13`。
const PROFILE_CLAUDE_4_7: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::Effort,
    supports_thinking_display: true,
    supports_openai_reasoning_effort: false,
    default_budget_tokens: 0,
    beta_headers: &[("anthropic-beta", "task-budgets-2026-03-13")],
};

/// Anthropic Claude 3.7 / 4.0 / 4.5 / 4.6 系列（legacy budget_tokens）。
const PROFILE_CLAUDE_BUDGET_TOKENS: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::BudgetTokens,
    supports_thinking_display: false,
    supports_openai_reasoning_effort: false,
    default_budget_tokens: 4096,
    beta_headers: &[],
};

/// OpenAI GPT-5 / o3 / o4 reasoning 系列：顶层 `reasoning_effort` 字段。
///
/// 不属于 Anthropic 协议范畴 → `thinking_kind = None`。
const PROFILE_OPENAI_REASONING: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::None,
    supports_thinking_display: false,
    supports_openai_reasoning_effort: true,
    default_budget_tokens: 0,
    beta_headers: &[],
};

const CAPABILITY_ENTRIES: &[CapabilityEntry] = &[
    // ===== Anthropic Claude 4.7+（Adaptive Thinking only mode） =====
    // 前缀比下面 4.x legacy 条目更长，最长前缀匹配确保优先命中本条。
    CapabilityEntry {
        prefix: "claude-opus-4-7",
        profile: PROFILE_CLAUDE_4_7,
    },
    CapabilityEntry {
        prefix: "claude-sonnet-4-7",
        profile: PROFILE_CLAUDE_4_7,
    },
    CapabilityEntry {
        prefix: "claude-haiku-4-7",
        profile: PROFILE_CLAUDE_4_7,
    },
    CapabilityEntry {
        prefix: "claude-opus-4-8",
        profile: PROFILE_CLAUDE_4_7,
    },
    CapabilityEntry {
        prefix: "claude-sonnet-4-8",
        profile: PROFILE_CLAUDE_4_7,
    },
    CapabilityEntry {
        prefix: "claude-haiku-4-8",
        profile: PROFILE_CLAUDE_4_7,
    },
    // ===== Anthropic Claude 3.7 / 4.0 / 4.5 / 4.6（legacy budget_tokens） =====
    CapabilityEntry {
        prefix: "claude-3-7",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-opus-4",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-sonnet-4",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-haiku-4",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-4",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    // ===== OpenAI reasoning 系列 =====
    CapabilityEntry {
        prefix: "gpt-5",
        profile: PROFILE_OPENAI_REASONING,
    },
    CapabilityEntry {
        prefix: "o4-",
        profile: PROFILE_OPENAI_REASONING,
    },
    CapabilityEntry {
        prefix: "o3-",
        profile: PROFILE_OPENAI_REASONING,
    },
    CapabilityEntry {
        prefix: "o3",
        profile: PROFILE_OPENAI_REASONING,
    },
];

/// 根据 model id 查表得到能力 profile。
///
/// 匹配规则：遍历 [`CAPABILITY_ENTRIES`]，取**前缀最长**的命中项。最长
/// 前缀语义保证 `claude-opus-4-7-xxx` 优先匹配 4.7 条目而不是更宽的
/// `claude-opus-4`。任何未命中返回 [`UNKNOWN_MODEL_DEFAULT`]。
pub fn resolve_capability_profile(model: &str) -> &'static ModelCapabilityProfile {
    let mut best: Option<&CapabilityEntry> = None;
    for entry in CAPABILITY_ENTRIES {
        if !model.starts_with(entry.prefix) {
            continue;
        }
        match best {
            Some(prev) if prev.prefix.len() >= entry.prefix.len() => {}
            _ => best = Some(entry),
        }
    }
    best.map(|e| &e.profile).unwrap_or(&UNKNOWN_MODEL_DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_4_7_maps_to_effort_profile_with_beta_header() {
        let profile = resolve_capability_profile("claude-opus-4-7-20260520");
        assert_eq!(profile.thinking_kind, ThinkingKind::Effort);
        assert!(profile.supports_thinking_display);
        assert!(!profile.supports_openai_reasoning_effort);
        assert_eq!(profile.default_budget_tokens, 0);
        assert_eq!(
            profile.beta_headers,
            &[("anthropic-beta", "task-budgets-2026-03-13")]
        );
    }

    #[test]
    fn opus_4_5_falls_back_to_budget_tokens_profile() {
        // 最长前缀语义：claude-opus-4-5 不应匹配到 claude-opus-4-7。
        let profile = resolve_capability_profile("claude-opus-4-5-20250930");
        assert_eq!(profile.thinking_kind, ThinkingKind::BudgetTokens);
        assert!(!profile.supports_thinking_display);
        assert_eq!(profile.default_budget_tokens, 4096);
        assert!(profile.beta_headers.is_empty());
    }

    #[test]
    fn sonnet_3_7_uses_budget_tokens_profile() {
        let profile = resolve_capability_profile("claude-3-7-sonnet-20250219");
        assert_eq!(profile.thinking_kind, ThinkingKind::BudgetTokens);
        assert_eq!(profile.default_budget_tokens, 4096);
    }

    #[test]
    fn gpt_5_supports_openai_reasoning_effort_but_no_anthropic_thinking() {
        let profile = resolve_capability_profile("gpt-5-turbo");
        assert_eq!(profile.thinking_kind, ThinkingKind::None);
        assert!(profile.supports_openai_reasoning_effort);
    }

    #[test]
    fn o3_and_o4_match_openai_reasoning_profile() {
        for model in ["o3", "o3-mini", "o4-mini-2025-04-16"] {
            let profile = resolve_capability_profile(model);
            assert!(
                profile.supports_openai_reasoning_effort,
                "model {model} 应当走 OpenAI reasoning profile"
            );
        }
    }

    #[test]
    fn unknown_model_falls_back_to_conservative_default() {
        for model in ["gpt-4o", "gemini-2.0-flash", "qwen-max", "my-custom-model"] {
            let profile = resolve_capability_profile(model);
            assert_eq!(
                profile.thinking_kind,
                ThinkingKind::None,
                "未知模型 {model} 不得启用 thinking"
            );
            assert!(
                !profile.supports_openai_reasoning_effort,
                "未知模型 {model} 不得自动注入 reasoning_effort"
            );
            assert!(profile.beta_headers.is_empty());
        }
    }

    #[test]
    fn supports_thinking_helper_aligns_with_thinking_kind() {
        assert!(resolve_capability_profile("claude-opus-4-7-x").supports_thinking());
        assert!(resolve_capability_profile("claude-3-7-sonnet").supports_thinking());
        assert!(!resolve_capability_profile("gpt-5").supports_thinking());
        assert!(!resolve_capability_profile("unknown-model").supports_thinking());
    }
}
