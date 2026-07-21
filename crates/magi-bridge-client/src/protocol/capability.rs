//! 模型能力数据表 —— 协议适配器装配 wire 字段的单一数据源。
//!
//! 设计原则：
//! - **数据驱动**：所有 `model_id` → 协议字段形态的映射写在静态
//!   [`CAPABILITY_ENTRIES`] 中。协议适配器（`anthropic.rs` / `openai_chat.rs`）
//!   不再 hardcode 字符串前缀分支，统一调 [`resolve_capability_profile`]
//!   查表。
//! - **Claude thinking 兼容靠 legacy 白名单，而非枚举新版本**：实测服务端
//!   契约——`claude-*-3-7 / 4-0 / 4-5 / 4-6` 仍认旧的
//!   `thinking: { type:"enabled", budget_tokens }`；`4-7+`（含未来新版）已切到
//!   Adaptive Thinking only mode（`thinking: { type:"adaptive" }` +
//!   顶层 `output_config.effort`），旧形态会被 400 拒绝。Anthropic 方向是
//!   adaptive-only 单调演进，新版本不会倒退回 budget_tokens——故策略反转为：
//!   **枚举有限且封闭的 legacy 集走 budget_tokens，其余所有 `claude-*`
//!   （含未知新版）默认 adaptive**，新模型开箱即用，不必逐版加前缀。
//! - **非 Claude 未知模型保守默认**：命中不到任何条目、且非 `claude-` 前缀的
//!   model id 走 [`UNKNOWN_MODEL_DEFAULT`]：不发 thinking / 不发
//!   reasoning_effort，避免向未知端点注入可能被拒绝的字段。
//! - **单实现**：本模块只暴露 [`resolve_capability_profile`] 一个入口。

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
    /// 是否支持 OpenAI Chat Completions 的强制工具选择对象。
    ///
    /// 部分带思考模式的模型只接受 `tool_choice: "auto"`，即使声明了工具也会
    /// 拒绝 `{"type":"function", ...}`。这类模型必须由适配器直接降级到自动选择，
    /// 避免把一个已知会失败的请求发给服务端。
    pub supports_forced_tool_choice: bool,
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
    supports_forced_tool_choice: true,
    default_budget_tokens: 0,
    beta_headers: &[],
};

/// Anthropic Claude Adaptive Thinking only mode（4.7+ 及未来新版默认）。
///
/// 协议形态（实测服务端契约）：
/// - `thinking: { type: "adaptive" }`，推理强度由顶层
///   `output_config.effort: "low|medium|high|xhigh"` 控制；
/// - 支持 `thinking.display: "summarized"` 控制思考块返回形态；
/// - 必须附带 beta header `anthropic-beta: task-budgets-2026-03-13`。
const PROFILE_CLAUDE_ADAPTIVE: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::Effort,
    supports_thinking_display: true,
    supports_openai_reasoning_effort: false,
    supports_forced_tool_choice: true,
    default_budget_tokens: 0,
    beta_headers: &[("anthropic-beta", "task-budgets-2026-03-13")],
};

/// Anthropic Claude 3.7 / 4.0 / 4.5 / 4.6 系列（legacy budget_tokens）。
const PROFILE_CLAUDE_BUDGET_TOKENS: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::BudgetTokens,
    supports_thinking_display: false,
    supports_openai_reasoning_effort: false,
    supports_forced_tool_choice: true,
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
    supports_forced_tool_choice: true,
    default_budget_tokens: 0,
    beta_headers: &[],
};

/// DeepSeek V4 的思考模式只接受自动工具选择，不接受 OpenAI 的强制工具对象。
const PROFILE_DEEPSEEK_V4: ModelCapabilityProfile = ModelCapabilityProfile {
    thinking_kind: ThinkingKind::None,
    supports_thinking_display: false,
    supports_openai_reasoning_effort: false,
    supports_forced_tool_choice: false,
    default_budget_tokens: 0,
    beta_headers: &[],
};

const CAPABILITY_ENTRIES: &[CapabilityEntry] = &[
    // ===== Anthropic Claude legacy（budget_tokens 白名单，封闭有限集） =====
    // 只有这些明确的旧版本仍认 thinking:{type:"enabled", budget_tokens}。
    // 其余所有 claude-* —— 含 4.7 / 4.8 及未来新版 —— 由 resolve 默认走
    // adaptive（见 PROFILE_CLAUDE_ADAPTIVE + Claude 默认分支），不必逐版枚举。
    CapabilityEntry {
        prefix: "claude-3-7",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    // 4.0 系列（无小版本号或 4-0-*）。注意不要用裸 "claude-opus-4" 这类宽前缀，
    // 否则会错误吞掉 4-7 / 4-8。
    CapabilityEntry {
        prefix: "claude-opus-4-0",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-sonnet-4-0",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-haiku-4-0",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    // 4.5 系列
    CapabilityEntry {
        prefix: "claude-opus-4-5",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-sonnet-4-5",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-haiku-4-5",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    // 4.6 系列
    CapabilityEntry {
        prefix: "claude-opus-4-6",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-sonnet-4-6",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    CapabilityEntry {
        prefix: "claude-haiku-4-6",
        profile: PROFILE_CLAUDE_BUDGET_TOKENS,
    },
    // ===== OpenAI reasoning 系列 =====
    CapabilityEntry {
        prefix: "gpt-5",
        profile: PROFILE_OPENAI_REASONING,
    },
    // DeepSeek V4 thinking mode rejects tool_choice=function/object；工具本身仍可用，
    // 只需让模型在 auto 模式下决定是否调用。
    CapabilityEntry {
        prefix: "deepseek-v4",
        profile: PROFILE_DEEPSEEK_V4,
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
/// 匹配规则：
/// 1. 遍历 [`CAPABILITY_ENTRIES`]，取**前缀最长**的命中项（legacy 白名单 +
///    OpenAI reasoning 系列）。
/// 2. 未命中但以 `claude-` 开头 → 默认 [`PROFILE_CLAUDE_ADAPTIVE`]。
///    覆盖 4.7 / 4.8 及一切未来 Claude 新版——无需逐版加前缀。
/// 3. 其余未知 model id → [`UNKNOWN_MODEL_DEFAULT`]（保守不发 thinking）。
pub fn resolve_capability_profile(model: &str) -> &'static ModelCapabilityProfile {
    let normalized_model = model.trim().to_ascii_lowercase();
    let mut best: Option<&CapabilityEntry> = None;
    for entry in CAPABILITY_ENTRIES {
        if !normalized_model.starts_with(entry.prefix) {
            continue;
        }
        match best {
            Some(prev) if prev.prefix.len() >= entry.prefix.len() => {}
            _ => best = Some(entry),
        }
    }
    if let Some(entry) = best {
        return &entry.profile;
    }
    // 未命中白名单：Claude 系默认走 adaptive（adaptive-only 单调演进，
    // 新版本只会是 adaptive），其余未知模型保守不发 thinking。
    if normalized_model.starts_with("claude-") {
        return &PROFILE_CLAUDE_ADAPTIVE;
    }
    &UNKNOWN_MODEL_DEFAULT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_4_7_maps_to_adaptive_profile_with_beta_header() {
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
    fn unenumerated_new_claude_versions_default_to_adaptive() {
        // 兼容核心：未逐版枚举的新版本（4.8 / 4.9 / 5.0…）一律默认 adaptive，
        // 开箱即用，不必每出一个新模型加一条前缀。
        for model in [
            "claude-opus-4-8",
            "claude-opus-4-8-20260815",
            "claude-sonnet-4-8",
            "claude-haiku-4-9",
            "claude-opus-5-0-20270101",
            "claude-next-experimental",
        ] {
            let profile = resolve_capability_profile(model);
            assert_eq!(
                profile.thinking_kind,
                ThinkingKind::Effort,
                "新版 Claude {model} 应默认 adaptive(Effort) profile"
            );
            assert!(
                !profile.beta_headers.is_empty(),
                "adaptive profile 应带 beta header"
            );
        }
    }

    #[test]
    fn legacy_claude_4_5_4_6_use_budget_tokens() {
        for model in [
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-6",
            "claude-opus-4-0-20250514",
            "claude-3-7-sonnet-20250219",
        ] {
            let profile = resolve_capability_profile(model);
            assert_eq!(
                profile.thinking_kind,
                ThinkingKind::BudgetTokens,
                "legacy Claude {model} 应走 budget_tokens profile"
            );
            assert_eq!(profile.default_budget_tokens, 4096);
            assert!(
                profile.beta_headers.is_empty(),
                "legacy profile 不带 adaptive beta header"
            );
        }
    }

    #[test]
    fn opus_4_5_falls_back_to_budget_tokens_profile() {
        // 最长前缀语义：claude-opus-4-5 命中 legacy 白名单而非默认 adaptive。
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
        assert!(profile.supports_forced_tool_choice);
    }

    #[test]
    fn deepseek_v4_disables_forced_tool_choice_case_insensitively() {
        for model in [
            "DeepSeek-V4-Flash",
            "deepseek-v4-flash",
            "DEEPSEEK-V4-FLASH",
        ] {
            let profile = resolve_capability_profile(model);
            assert!(!profile.supports_forced_tool_choice, "model {model}");
        }
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
