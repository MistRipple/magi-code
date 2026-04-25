use crate::errors::ApiError;
use magi_bridge_client::{HttpModelBridgeClient, HttpModelBridgeProtocol};
use magi_usage_authority::{LlmConfig, OpenAiProtocol, ReasoningEffort, UrlMode};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelUrlMode {
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
pub(crate) enum ModelOpenAiProtocol {
    Responses,
    Chat,
}

impl ModelOpenAiProtocol {
    fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "responses" => Some(Self::Responses),
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }

    fn to_usage_protocol(self) -> OpenAiProtocol {
        match self {
            Self::Responses => OpenAiProtocol::Responses,
            Self::Chat => OpenAiProtocol::Chat,
        }
    }

    fn to_http_protocol(self) -> HttpModelBridgeProtocol {
        match self {
            Self::Responses => HttpModelBridgeProtocol::Responses,
            Self::Chat => HttpModelBridgeProtocol::ChatCompletions,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelReasoningEffort {
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
pub(crate) struct NormalizedModelConfig {
    provider: String,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    url_mode: ModelUrlMode,
    openai_protocol: Option<ModelOpenAiProtocol>,
    protocol_endpoint: Option<String>,
    reasoning_effort: Option<ModelReasoningEffort>,
    enable_thinking: Option<bool>,
}

impl NormalizedModelConfig {
    pub(crate) fn from_settings_value(value: &Value, default_provider: &str) -> Self {
        let provider =
            string_field(value, "provider").unwrap_or_else(|| default_provider.trim().to_string());
        let url_mode_label =
            string_field(value, "urlMode").unwrap_or_else(|| "standard".to_string());
        Self {
            provider,
            base_url: string_field(value, "baseUrl"),
            api_key: string_field(value, "apiKey"),
            model: string_field(value, "model"),
            url_mode: ModelUrlMode::from_label(&url_mode_label),
            openai_protocol: value
                .get("openaiProtocol")
                .and_then(Value::as_str)
                .and_then(ModelOpenAiProtocol::from_label),
            protocol_endpoint: string_field(value, "protocolEndpoint"),
            reasoning_effort: value
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .and_then(ModelReasoningEffort::from_label),
            enable_thinking: value
                .get("enableThinking")
                .and_then(Value::as_bool)
                .or_else(|| value.get("thinking").and_then(Value::as_bool)),
        }
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn provider_key(&self) -> String {
        self.provider.trim().to_ascii_lowercase()
    }

    pub(crate) fn require_base_url(&self) -> Result<&str, ApiError> {
        self.base_url
            .as_deref()
            .ok_or_else(|| ApiError::InvalidInput("模型配置缺少 baseUrl".to_string()))
    }

    pub(crate) fn require_api_key(&self) -> Result<&str, ApiError> {
        self.api_key
            .as_deref()
            .ok_or_else(|| ApiError::InvalidInput("模型配置缺少 apiKey".to_string()))
    }

    pub(crate) fn require_model(&self) -> Result<&str, ApiError> {
        self.model
            .as_deref()
            .ok_or_else(|| ApiError::InvalidInput("模型配置缺少 model".to_string()))
    }

    pub(crate) fn to_http_model_client(
        &self,
        default_model: &str,
    ) -> Option<HttpModelBridgeClient> {
        let base_url = self.http_client_base_url()?;
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| default_model.to_string());
        Some(HttpModelBridgeClient::new_with_protocol(
            base_url,
            self.api_key.clone(),
            model,
            self.execution_openai_protocol().to_http_protocol(),
        ))
    }

    pub(crate) fn to_usage_llm_config(&self) -> Option<LlmConfig> {
        Some(LlmConfig {
            provider: self.provider.clone(),
            model: self.model.clone()?,
            base_url: self.base_url.clone()?,
            api_key: self.api_key.clone(),
            url_mode: self.url_mode.to_usage_url_mode(),
            openai_protocol: self
                .openai_protocol
                .map(ModelOpenAiProtocol::to_usage_protocol),
            reasoning_effort: self
                .reasoning_effort
                .map(ModelReasoningEffort::to_usage_reasoning_effort),
            enable_thinking: self.enable_thinking,
        })
    }

    pub(crate) fn openai_models_url(&self) -> Result<String, ApiError> {
        self.require_openai_models_listable()?;
        let normalized = self.normalized_http_base_url()?;
        if normalized.ends_with("/models") {
            return Ok(normalized);
        }
        if normalized.ends_with("/v1") || normalized.ends_with("/v3") {
            return Ok(format!("{normalized}/models"));
        }
        Ok(format!("{normalized}/v1/models"))
    }

    pub(crate) fn require_openai_models_listable(&self) -> Result<(), ApiError> {
        if matches!(self.url_mode, ModelUrlMode::Full) {
            return Err(ApiError::InvalidInput(
                "完整路径模式下不支持自动获取模型列表，请手动填写模型名".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) fn openai_probe_url(&self) -> Result<String, ApiError> {
        let normalized = self.normalized_http_base_url()?;
        if matches!(self.url_mode, ModelUrlMode::Full) {
            if let Some(endpoint) = self.protocol_endpoint.as_deref() {
                return Ok(format!("{normalized}{endpoint}"));
            }
            return Ok(normalized);
        }
        let suffix = if self.effective_openai_protocol() == ModelOpenAiProtocol::Chat {
            "/v1/chat/completions"
        } else {
            "/v1/responses"
        };
        Ok(format!("{normalized}{suffix}"))
    }

    pub(crate) fn anthropic_probe_url(&self) -> Result<String, ApiError> {
        let normalized = self.normalized_http_base_url()?;
        if matches!(self.url_mode, ModelUrlMode::Full) {
            return Ok(normalized);
        }
        Ok(format!("{normalized}/v1/messages"))
    }

    pub(crate) fn openai_probe_body(&self) -> Result<Value, ApiError> {
        let model = self.require_model()?;
        if self.effective_openai_protocol() == ModelOpenAiProtocol::Chat {
            Ok(json!({
                "model": model,
                "messages": [{
                    "role": "user",
                    "content": "ping"
                }],
                "max_tokens": 1,
                "stream": false,
            }))
        } else {
            Ok(json!({
                "model": model,
                "input": "ping",
                "max_output_tokens": 1,
            }))
        }
    }

    pub(crate) fn anthropic_probe_body(&self) -> Result<Value, ApiError> {
        let model = self.require_model()?;
        Ok(json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{
                "role": "user",
                "content": "ping"
            }]
        }))
    }

    fn normalized_http_base_url(&self) -> Result<String, ApiError> {
        let normalized = self.require_base_url()?.trim().trim_end_matches('/');
        if normalized.is_empty() {
            return Err(ApiError::InvalidInput(
                "模型配置缺少有效的 baseUrl".to_string(),
            ));
        }
        if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
            return Err(ApiError::InvalidInput(
                "baseUrl 必须以 http:// 或 https:// 开头".to_string(),
            ));
        }
        Ok(normalized.to_string())
    }

    fn effective_openai_protocol(&self) -> ModelOpenAiProtocol {
        self.openai_protocol
            .unwrap_or(ModelOpenAiProtocol::Responses)
    }

    fn execution_openai_protocol(&self) -> ModelOpenAiProtocol {
        if self.provider_key() == "openai" {
            self.effective_openai_protocol()
        } else {
            self.openai_protocol.unwrap_or(ModelOpenAiProtocol::Chat)
        }
    }

    fn http_client_base_url(&self) -> Option<String> {
        let base_url = self.base_url.as_deref()?.trim().trim_end_matches('/');
        if matches!(self.url_mode, ModelUrlMode::Full)
            && let Some(endpoint) = self.protocol_endpoint.as_deref()
        {
            return Some(format!("{base_url}{endpoint}"));
        }
        Some(base_url.to_string())
    }
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

    #[test]
    fn normalized_model_config_preserves_fetch_models_contract() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "provider": "anthropic",
                "baseUrl": "http://127.0.0.1:8320/",
                "apiKey": "test-key",
                "urlMode": "standard"
            }),
            "openai",
        );

        assert_eq!(config.provider(), "anthropic");
        assert_eq!(
            config.require_base_url().expect("baseUrl"),
            "http://127.0.0.1:8320/"
        );
        assert_eq!(config.require_api_key().expect("apiKey"), "test-key");
        config
            .require_openai_models_listable()
            .expect("standard url mode can list models");
        assert_eq!(
            config.openai_models_url().expect("models url"),
            "http://127.0.0.1:8320/v1/models"
        );
    }

    #[test]
    fn normalized_model_config_rejects_full_mode_models_url() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "baseUrl": "http://127.0.0.1:8320/v1/chat/completions",
                "apiKey": "test-key",
                "urlMode": "full"
            }),
            "openai",
        );

        let error = config
            .openai_models_url()
            .expect_err("full path has no canonical models endpoint");
        assert!(
            matches!(error, ApiError::InvalidInput(message) if message.contains("完整路径模式下不支持自动获取模型列表"))
        );
    }

    #[test]
    fn normalized_model_config_builds_usage_config_without_defaulting_protocol() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "provider": "openai-compatible",
                "baseUrl": "https://example.test",
                "model": "gpt-test",
                "urlMode": "standard"
            }),
            "openai",
        );

        let usage = config.to_usage_llm_config().expect("usage config");
        assert_eq!(usage.provider, "openai-compatible");
        assert_eq!(usage.model, "gpt-test");
        assert_eq!(usage.url_mode, UrlMode::Default);
        assert_eq!(usage.openai_protocol, None);
    }

    #[test]
    fn normalized_model_config_preserves_explicit_chat_protocol_for_usage() {
        let config = NormalizedModelConfig::from_settings_value(
            &json!({
                "provider": "openai",
                "baseUrl": "https://example.test",
                "model": "gpt-test",
                "urlMode": "standard",
                "openaiProtocol": "chat"
            }),
            "openai",
        );

        let usage = config.to_usage_llm_config().expect("usage config");
        assert_eq!(usage.openai_protocol, Some(OpenAiProtocol::Chat));
    }
}
