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
        let full_caps = vec![
            ModelCapability::ToolUse,
            ModelCapability::Streaming,
            ModelCapability::Vision,
            ModelCapability::SystemPrompt,
            ModelCapability::JsonMode,
        ];
        for prefix in &["gpt-4", "gpt-3.5", "claude-3", "claude-4", "gemini"] {
            self.capabilities
                .insert(prefix.to_string(), full_caps.clone());
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
