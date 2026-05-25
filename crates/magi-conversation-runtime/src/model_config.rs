//! Task System v2 — model config helpers。
//!
//! 错误返回值使用 `Result<_, String>`，由上层调用方桥接到自己的错误类型。
//!
//! 协议判定的**唯一事实源**是 `baseUrl` 的路径后缀：
//! - `urlMode = standard|proxy`：`baseUrl` 严格以 `/v1` 结尾 → OpenAI Chat Completions；
//!   其他 → Anthropic Messages。
//! - `urlMode = full`：`baseUrl` 严格以 `/v1/messages` 结尾 → Anthropic Messages；
//!   其他 → OpenAI Chat Completions。
//!
//! `provider` 字段不再参与路由决策，仅作为统计/展示标签，由上述推断同步派生。
//! 历史配置中残留的 `provider` 或 `openaiProtocol` JSON 字段会被静默忽略。

use magi_bridge_client::{HttpModelBridgeClient, HttpModelBridgeProtocol};
use magi_usage_authority::{LlmConfig, ReasoningEffort, UrlMode};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelUrlMode {
    Standard,
    Full,
    Proxy,
}

impl ModelUrlMode {
    fn from_label(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Self::Full,
            "proxy" => Self::Proxy,
            _ => Self::Standard,
        }
    }

    fn to_usage_url_mode(self) -> UrlMode {
        match self {
            Self::Full => UrlMode::Full,
            Self::Proxy => UrlMode::Proxy,
            Self::Standard => UrlMode::Default,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
}

impl ModelReasoningEffort {
    fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            _ => None,
        }
    }

    fn to_usage_reasoning_effort(self) -> ReasoningEffort {
        match self {
            Self::Low => ReasoningEffort::Low,
            Self::Medium => ReasoningEffort::Medium,
            Self::High => ReasoningEffort::High,
            Self::Xhigh => ReasoningEffort::Xhigh,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedModelConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    url_mode: ModelUrlMode,
    reasoning_effort: Option<ModelReasoningEffort>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleEngineModelConfig {
    pub template_id: String,
    pub engine_id: String,
    pub binding_revision: u32,
    pub config: NormalizedModelConfig,
}

impl NormalizedModelConfig {
    /// 从 settings JSON 构造归一化模型配置。
    ///
    /// `_default_provider` 参数仅为保持上层调用方签名兼容，实际不再参与逻辑——
    /// provider 完全由 `baseUrl + urlMode` 推断。历史配置里的 `provider` / `openaiProtocol`
    /// / `protocolEndpoint` 字段会被静默忽略。
    pub fn from_settings_value(value: &Value, _default_provider: &str) -> Self {
        let url_mode_label =
            string_field(value, "urlMode").unwrap_or_else(|| "standard".to_string());
        Self {
            base_url: string_field(value, "baseUrl"),
            api_key: string_field(value, "apiKey"),
            model: string_field(value, "model"),
            url_mode: ModelUrlMode::from_label(&url_mode_label),
            reasoning_effort: value
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .and_then(ModelReasoningEffort::from_label),
        }
    }

    /// 推断出的 provider 标签，用于 usage authority 分组与展示。
    /// 永远与 [`inferred_protocol`](Self::inferred_protocol) 同步。
    pub fn provider(&self) -> &'static str {
        match self.inferred_protocol() {
            HttpModelBridgeProtocol::ChatCompletions => "openai",
            HttpModelBridgeProtocol::AnthropicMessages => "anthropic",
        }
    }

    pub fn provider_key(&self) -> &'static str {
        self.provider()
    }

    pub fn require_base_url(&self) -> Result<&str, String> {
        self.base_url
            .as_deref()
            .ok_or_else(|| "模型配置缺少 baseUrl".to_string())
    }

    pub fn require_api_key(&self) -> Result<&str, String> {
        self.api_key
            .as_deref()
            .ok_or_else(|| "模型配置缺少 apiKey".to_string())
    }

    pub fn require_model(&self) -> Result<&str, String> {
        self.model
            .as_deref()
            .ok_or_else(|| "模型配置缺少 model".to_string())
    }

    /// 推断 HTTP 协议族，唯一事实源是 `baseUrl` 的路径后缀。
    pub fn inferred_protocol(&self) -> HttpModelBridgeProtocol {
        let normalized = self
            .base_url
            .as_deref()
            .map(|value| value.trim().trim_end_matches('/').to_ascii_lowercase())
            .unwrap_or_default();

        match self.url_mode {
            ModelUrlMode::Full => {
                if normalized.ends_with("/v1/messages") {
                    HttpModelBridgeProtocol::AnthropicMessages
                } else {
                    HttpModelBridgeProtocol::ChatCompletions
                }
            }
            ModelUrlMode::Standard | ModelUrlMode::Proxy => {
                if normalized.ends_with("/v1") {
                    HttpModelBridgeProtocol::ChatCompletions
                } else {
                    HttpModelBridgeProtocol::AnthropicMessages
                }
            }
        }
    }

    pub fn to_http_model_client(&self, default_model: &str) -> Option<HttpModelBridgeClient> {
        let base_url = self.http_client_base_url()?;
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| default_model.to_string());
        Some(HttpModelBridgeClient::new_with_protocol(
            base_url,
            self.api_key.clone(),
            model,
            self.inferred_protocol(),
            self.reasoning_effort
                .map(ModelReasoningEffort::to_usage_reasoning_effort),
        ))
    }

    pub fn to_usage_llm_config(&self) -> Option<LlmConfig> {
        Some(LlmConfig {
            provider: self.provider().to_string(),
            model: self.model.clone()?,
            base_url: self.base_url.clone()?,
            api_key: self.api_key.clone(),
            url_mode: self.url_mode.to_usage_url_mode(),
            reasoning_effort: self
                .reasoning_effort
                .map(ModelReasoningEffort::to_usage_reasoning_effort),
        })
    }

    pub fn models_list_url(&self) -> Result<String, String> {
        self.require_models_listable()?;
        let normalized = self.normalized_http_base_url()?;
        if normalized.ends_with("/v1") {
            return Ok(format!("{normalized}/models"));
        }
        Ok(format!("{normalized}/v1/models"))
    }

    pub fn require_models_listable(&self) -> Result<(), String> {
        if matches!(self.url_mode, ModelUrlMode::Full) {
            return Err("完整路径模式下不支持自动获取模型列表，请手动填写模型名".to_string());
        }
        Ok(())
    }

    fn normalized_http_base_url(&self) -> Result<String, String> {
        let normalized = self.require_base_url()?.trim().trim_end_matches('/');
        if normalized.is_empty() {
            return Err("模型配置缺少有效的 baseUrl".to_string());
        }
        if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
            return Err("baseUrl 必须以 http:// 或 https:// 开头".to_string());
        }
        Ok(normalized.to_string())
    }

    fn http_client_base_url(&self) -> Option<String> {
        let base_url = self.base_url.as_deref()?.trim().trim_end_matches('/');
        Some(base_url.to_string())
    }
}

pub fn configured_role_engine_model_config(
    settings_store: &crate::settings_store::SettingsStore,
    role_id: &str,
) -> Result<Option<RoleEngineModelConfig>, String> {
    let role_id = role_id.trim();
    if role_id.is_empty() {
        return Ok(None);
    }
    let Some(binding) = role_engine_binding(settings_store, role_id) else {
        return Ok(None);
    };
    if !binding.enabled {
        return Err(format!("角色 {role_id} 已禁用，不能执行子代理任务"));
    }
    let engine_llm = engine_llm_config(settings_store, &binding.engine_id).ok_or_else(|| {
        format!(
            "角色 {role_id} 绑定的模型引擎 {} 不存在或缺少 llm 配置",
            binding.engine_id
        )
    })?;
    let config = NormalizedModelConfig::from_settings_value(&engine_llm, "openai");
    config.require_base_url().map_err(|error| {
        format!(
            "角色 {role_id} 的模型引擎 {} 配置无效：{error}",
            binding.engine_id
        )
    })?;
    config.require_model().map_err(|error| {
        format!(
            "角色 {role_id} 的模型引擎 {} 配置无效：{error}",
            binding.engine_id
        )
    })?;
    Ok(Some(RoleEngineModelConfig {
        template_id: role_id.to_string(),
        engine_id: binding.engine_id,
        binding_revision: binding.binding_revision,
        config,
    }))
}

struct RoleEngineBinding {
    engine_id: String,
    binding_revision: u32,
    enabled: bool,
}

fn role_engine_binding(
    settings_store: &crate::settings_store::SettingsStore,
    role_id: &str,
) -> Option<RoleEngineBinding> {
    let agents = settings_store.get_section("agents");
    let entries = agents.as_array()?;
    for entry in entries {
        let raw = entry.get("agent").unwrap_or(entry);
        let Some(template_id) = string_field(raw, "templateId") else {
            continue;
        };
        if template_id != role_id {
            continue;
        }
        let engine_id = string_field(raw, "engineId").unwrap_or_default();
        let model_source = string_field(raw, "modelSource").unwrap_or_else(|| {
            if engine_id.is_empty() {
                "orchestrator".to_string()
            } else {
                "engine".to_string()
            }
        });
        let enabled = raw.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        if model_source != "engine" {
            return None;
        }
        if engine_id.is_empty() {
            return Some(RoleEngineBinding {
                engine_id,
                binding_revision: binding_revision(raw),
                enabled,
            });
        }
        return Some(RoleEngineBinding {
            engine_id,
            binding_revision: binding_revision(raw),
            enabled,
        });
    }
    None
}

fn engine_llm_config(
    settings_store: &crate::settings_store::SettingsStore,
    engine_id: &str,
) -> Option<Value> {
    let engine_id = engine_id.trim();
    if engine_id.is_empty() {
        return None;
    }
    let engines = settings_store.get_section("engines");
    let entries = engines.as_array()?;
    for entry in entries {
        let raw = entry.get("engine").unwrap_or(entry);
        let Some(id) = string_field(raw, "id").or_else(|| string_field(raw, "engineId")) else {
            continue;
        };
        if id != engine_id {
            continue;
        }
        let llm = raw.get("llm")?.clone();
        if llm.as_object().is_none_or(|object| object.is_empty()) {
            return None;
        }
        return Some(llm);
    }
    None
}

fn binding_revision(value: &Value) -> u32 {
    value
        .get("bindingRevision")
        .and_then(Value::as_u64)
        .and_then(|revision| u32::try_from(revision).ok())
        .unwrap_or(0)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standard_mode_v1_suffix_infers_openai_chat() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKey": "sk-test",
                "model": "deepseek-chat",
                "urlMode": "standard"
            }),
            "openai",
        );
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn standard_mode_without_v1_suffix_infers_anthropic() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://api.anthropic.com",
                "apiKey": "sk-test",
                "model": "claude-sonnet",
                "urlMode": "standard"
            }),
            "openai",
        );
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(config.provider(), "anthropic");
    }

    #[test]
    fn full_mode_messages_suffix_infers_anthropic() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://proxy.example.com/v1/messages",
                "apiKey": "sk-test",
                "model": "claude-sonnet",
                "urlMode": "full"
            }),
            "openai",
        );
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(config.provider(), "anthropic");
    }

    #[test]
    fn full_mode_chat_completions_suffix_infers_openai() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://proxy.example.com/v1/chat/completions",
                "apiKey": "sk-test",
                "model": "gpt-4",
                "urlMode": "full"
            }),
            "openai",
        );
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn trailing_slash_does_not_affect_v1_inference() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://api.deepseek.com/v1/",
                "apiKey": "sk-test",
                "model": "deepseek-chat",
                "urlMode": "standard"
            }),
            "openai",
        );
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn legacy_provider_and_protocol_fields_are_ignored() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "provider": "anthropic",
                "openaiProtocol": "responses",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKey": "sk-test",
                "model": "deepseek-chat",
                "urlMode": "standard"
            }),
            "anthropic",
        );
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn normalized_model_config_preserves_openai_fetch_models_contract() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "http://127.0.0.1:8320/v1",
                "apiKey": "test-key",
                "urlMode": "standard"
            }),
            "openai",
        );

        assert_eq!(config.provider(), "openai");
        assert_eq!(
            config.require_base_url().expect("baseUrl"),
            "http://127.0.0.1:8320/v1"
        );
        assert_eq!(config.require_api_key().expect("apiKey"), "test-key");
        config
            .require_models_listable()
            .expect("standard url mode can list models");
        assert_eq!(
            config.models_list_url().expect("models url"),
            "http://127.0.0.1:8320/v1/models"
        );
    }

    #[test]
    fn anthropic_base_url_allows_models_listing() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://api.anthropic.com",
                "apiKey": "test-key",
                "urlMode": "standard"
            }),
            "openai",
        );

        // /v1/models 在两个协议下都是合法路径，缺少 /v1 时由后端自动补齐
        config
            .require_models_listable()
            .expect("anthropic-style base url should also be listable");
        assert_eq!(
            config.models_list_url().expect("models url"),
            "https://api.anthropic.com/v1/models"
        );
    }

    #[test]
    fn full_mode_rejects_models_listing() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "http://127.0.0.1:8320/v1/chat/completions",
                "apiKey": "test-key",
                "urlMode": "full"
            }),
            "openai",
        );

        let error = config
            .models_list_url()
            .expect_err("full path has no canonical models endpoint");
        assert!(error.contains("完整路径模式下不支持自动获取模型列表"));
    }

    #[test]
    fn usage_llm_config_drops_legacy_protocol_field() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://example.test/v1",
                "model": "gpt-test",
                "urlMode": "standard"
            }),
            "openai",
        );

        let usage = config.to_usage_llm_config().expect("usage config");
        assert_eq!(usage.provider, "openai");
        assert_eq!(usage.model, "gpt-test");
        assert_eq!(usage.url_mode, UrlMode::Default);
    }

    #[test]
    fn http_client_uses_inferred_protocol() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKey": "test-key",
                "model": "deepseek-chat",
                "urlMode": "standard"
            }),
            "openai",
        );

        assert!(config.to_http_model_client("gpt-4").is_some());
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn http_client_uses_anthropic_when_inferred() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "https://api.anthropic.com",
                "apiKey": "test-key",
                "model": "claude-sonnet",
                "urlMode": "standard"
            }),
            "openai",
        );

        assert!(config.to_http_model_client("gpt-4").is_some());
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
    }

    #[test]
    fn role_engine_model_config_resolves_agent_binding() {
        let store = crate::settings_store::SettingsStore::new();
        store.set_section(
            "agents",
            json!([{
                "templateId": "reviewer",
                "modelSource": "engine",
                "engineId": "sonnet-4-5",
                "bindingRevision": 7,
                "enabled": true
            }]),
        );
        store.set_section(
            "engines",
            json!([{
                "id": "sonnet-4-5",
                "llm": {
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-role",
                    "model": "role-sonnet",
                    "urlMode": "standard",
                    "reasoningEffort": "high"
                }
            }]),
        );

        let resolved = configured_role_engine_model_config(&store, "reviewer")
            .expect("role binding should parse")
            .expect("role should bind engine");

        assert_eq!(resolved.template_id, "reviewer");
        assert_eq!(resolved.engine_id, "sonnet-4-5");
        assert_eq!(resolved.binding_revision, 7);
        assert_eq!(resolved.config.require_model().unwrap(), "role-sonnet");
        assert_eq!(
            resolved.config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn role_engine_model_config_returns_none_for_orchestrator_inheritance() {
        let store = crate::settings_store::SettingsStore::new();
        store.set_section(
            "agents",
            json!([{
                "templateId": "executor",
                "modelSource": "orchestrator",
                "engineId": "",
                "enabled": true
            }]),
        );

        assert!(
            configured_role_engine_model_config(&store, "executor")
                .expect("orchestrator inheritance is valid")
                .is_none()
        );
    }
}
