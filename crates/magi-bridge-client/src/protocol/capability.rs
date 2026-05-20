use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelCapability {
    ToolUse,
    Streaming,
    Vision,
    SystemPrompt,
    ExtendedThinking,
    JsonMode,
    CacheControl,
}

/// 唯一真理源：判断模型 ID 是否支持 Anthropic Extended Thinking。
///
/// 命中的模型在 `AnthropicMessagesAdapter::build_request` 时会自动注入
/// `"thinking": {"type": "enabled", ...}` 字段，无需用户配置。
///
/// 当前覆盖范围（按 Anthropic 公开文档）：
/// - Claude 3.7 Sonnet（首次引入 extended thinking）
/// - Claude 4 / 4.5 系列（Opus 4、Sonnet 4、Haiku 4.5）
pub fn supports_extended_thinking(model: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "claude-3-7",
        "claude-opus-4",
        "claude-sonnet-4",
        "claude-haiku-4",
        "claude-4",
    ];
    PREFIXES.iter().any(|p| model.starts_with(p))
}

#[derive(Clone, Debug, Default)]
pub struct CapabilityRegistry {
    capabilities: HashMap<String, Vec<ModelCapability>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry.register_defaults();
        registry
    }

    fn register_defaults(&mut self) {
        let base_caps = vec![
            ModelCapability::ToolUse,
            ModelCapability::Streaming,
            ModelCapability::Vision,
            ModelCapability::SystemPrompt,
            ModelCapability::JsonMode,
        ];
        for prefix in &["gpt-4", "gpt-3.5", "gemini"] {
            self.capabilities
                .insert(prefix.to_string(), base_caps.clone());
        }
        // Claude 系列：在基础能力之上，3.7+ 和 4.x 全部支持 ExtendedThinking。
        for prefix in &[
            "claude-3",
            "claude-3-7",
            "claude-opus-4",
            "claude-sonnet-4",
            "claude-haiku-4",
            "claude-4",
        ] {
            let mut caps = base_caps.clone();
            if supports_extended_thinking(prefix) {
                caps.push(ModelCapability::ExtendedThinking);
            }
            self.capabilities.insert(prefix.to_string(), caps);
        }
    }

    pub fn register(&mut self, model_prefix: &str, caps: Vec<ModelCapability>) {
        self.capabilities.insert(model_prefix.to_string(), caps);
    }

    pub fn has_capability(&self, model: &str, cap: &ModelCapability) -> bool {
        for (prefix, caps) in &self.capabilities {
            if model.starts_with(prefix) {
                return caps.contains(cap);
            }
        }
        true
    }

    pub fn get_capabilities(&self, model: &str) -> Vec<ModelCapability> {
        for (prefix, caps) in &self.capabilities {
            if model.starts_with(prefix) {
                return caps.clone();
            }
        }
        vec![
            ModelCapability::ToolUse,
            ModelCapability::Streaming,
            ModelCapability::SystemPrompt,
        ]
    }
}
