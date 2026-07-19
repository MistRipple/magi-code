//! 任务系统 — model config helpers。
//!
//! 错误返回值使用 `Result<_, String>`，由上层调用方桥接到自己的错误类型。
//!
//! 协议判定的**唯一事实源**是归一化模型配置：
//! - `urlMode = standard|proxy`：显式的 `/anthropic` 路径前缀优先识别为
//!   Anthropic Messages；其余地址再按模型名识别协议。
//! - `urlMode = full`：用户已经填写完整端点，按端点路径识别协议；`/v1/messages`
//!   走 Anthropic Messages，其他走 OpenAI Chat Completions。
//!
//! `provider` 字段不再参与路由决策，仅作为统计/展示标签，由上述推断同步派生。
//! 配置输入不再接受 `provider` / `openaiProtocol` / `protocolEndpoint`，避免持久化
//! 字段和推断结果形成双事实源。

use magi_bridge_client::{
    HttpImageGenerationClient, HttpModelBridgeClient, HttpModelBridgeProtocol,
    ImageGenerationUrlMode,
};
use magi_core::SessionId;
use magi_settings_store::DEPRECATED_MODEL_CONFIG_FIELDS;
use magi_usage_authority::{LlmConfig, ReasoningEffort, UrlMode};
use serde_json::Value;

pub const DEFAULT_ORCHESTRATOR_REASONING_EFFORT: &str = "medium";

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
    /// provider 完全由归一化后的 `urlMode + baseUrl + model` 推断；配置输入只允许
    /// 当前字段，废弃字段必须在保存前清理，否则这里直接拒绝。
    pub fn from_settings_value(value: &Value) -> Result<Self, String> {
        reject_deprecated_model_config_fields(value)?;
        let url_mode_label =
            string_field(value, "urlMode").unwrap_or_else(|| "standard".to_string());
        Ok(Self {
            base_url: string_field(value, "baseUrl"),
            api_key: string_field(value, "apiKey"),
            model: string_field(value, "model"),
            url_mode: ModelUrlMode::from_label(&url_mode_label),
            reasoning_effort: value
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .and_then(ModelReasoningEffort::from_label),
        })
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

    pub fn inferred_protocol(&self) -> HttpModelBridgeProtocol {
        self.inferred_protocol_for_model(self.model.as_deref())
    }

    /// 推断 HTTP 协议族。
    ///
    /// 显式 `/anthropic` 前缀或 `/messages` 端点优先决定 Anthropic 协议；
    /// 普通 standard/proxy 网关根地址再按模型家族识别。full 模式必须尊重
    /// 完整端点路径，避免把 OpenAI 兼容代理中的 Claude 模型误路由。
    fn inferred_protocol_for_model(&self, model: Option<&str>) -> HttpModelBridgeProtocol {
        let normalized = self
            .base_url
            .as_deref()
            .map(|value| value.trim().trim_end_matches('/').to_ascii_lowercase())
            .unwrap_or_default();

        match self.url_mode {
            ModelUrlMode::Full => {
                if is_anthropic_endpoint(&normalized) {
                    HttpModelBridgeProtocol::AnthropicMessages
                } else {
                    HttpModelBridgeProtocol::ChatCompletions
                }
            }
            ModelUrlMode::Standard | ModelUrlMode::Proxy => match model {
                _ if is_anthropic_endpoint(&normalized) => {
                    HttpModelBridgeProtocol::AnthropicMessages
                }
                Some(model) if is_anthropic_model_name(model) => {
                    HttpModelBridgeProtocol::AnthropicMessages
                }
                _ => HttpModelBridgeProtocol::ChatCompletions,
            },
        }
    }

    pub fn to_http_model_client(&self) -> Option<HttpModelBridgeClient> {
        let base_url = self.http_client_base_url()?;
        let model = self.model.as_deref()?.trim();
        if model.is_empty() {
            return None;
        }
        let protocol = self.inferred_protocol_for_model(Some(model));
        Some(HttpModelBridgeClient::new_with_protocol(
            base_url,
            self.api_key.clone(),
            model.to_string(),
            protocol,
            self.reasoning_effort
                .map(ModelReasoningEffort::to_usage_reasoning_effort),
        ))
    }

    pub fn to_http_image_generation_client(&self) -> Result<HttpImageGenerationClient, String> {
        let base_url = self.normalized_http_base_url()?;
        let model = self.require_model()?.to_string();
        let url_mode = match self.url_mode {
            ModelUrlMode::Full => ImageGenerationUrlMode::Full,
            ModelUrlMode::Standard | ModelUrlMode::Proxy => ImageGenerationUrlMode::Standard,
        };
        Ok(HttpImageGenerationClient::new(
            base_url,
            self.api_key.clone(),
            model,
            url_mode,
        ))
    }

    pub fn to_usage_llm_config(&self) -> Option<LlmConfig> {
        Some(LlmConfig {
            provider: self.provider().to_string(),
            model: self.model.clone()?,
            base_url: self.base_url.clone()?,
            api_key: self.api_key.clone(),
            account_fingerprint: None,
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

pub fn reject_deprecated_model_config_fields(value: &Value) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    for field in DEPRECATED_MODEL_CONFIG_FIELDS {
        if object.contains_key(*field) {
            return Err(format!(
                "模型配置字段 {field} 已废弃，请使用 baseUrl/apiKey/model/urlMode/reasoningEffort"
            ));
        }
    }
    Ok(())
}

pub fn configured_role_engine_model_config(
    settings_store: &magi_settings_store::SettingsStore,
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
        return Err(format!("角色 {role_id} 已禁用，不能执行代理任务"));
    }
    let engine_llm = engine_llm_config(settings_store, &binding.engine_id).ok_or_else(|| {
        format!(
            "角色 {role_id} 绑定的模型引擎 {} 不存在或缺少 llm 配置",
            binding.engine_id
        )
    })?;
    let config = NormalizedModelConfig::from_settings_value(&engine_llm)?;
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

pub fn resolve_orchestrator_model_config(
    settings_store: &magi_settings_store::SettingsStore,
    session_id: Option<&SessionId>,
) -> Result<NormalizedModelConfig, String> {
    let mut config = settings_store.get_section("orchestrator");
    strip_orchestrator_session_owned_fields(&mut config);
    if let Some(session_id) = session_id {
        let override_section = settings_store.get_session_section(session_id, "orchestrator");
        merge_orchestrator_session_override(&mut config, &override_section);
    }
    ensure_orchestrator_reasoning_effort(&mut config);
    NormalizedModelConfig::from_settings_value(&config)
        .map_err(|error| format!("orchestrator 模型配置无效：{error}"))
}

pub fn ensure_orchestrator_reasoning_effort(config: &mut serde_json::Value) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    let serde_json::Value::Object(config) = config else {
        return;
    };
    let is_valid = config
        .get("reasoningEffort")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| matches!(value.trim(), "low" | "medium" | "high" | "xhigh"));
    if !is_valid {
        config.insert(
            "reasoningEffort".to_string(),
            serde_json::Value::String(DEFAULT_ORCHESTRATOR_REASONING_EFFORT.to_string()),
        );
    }
}

pub fn strip_orchestrator_session_owned_fields(base: &mut serde_json::Value) {
    if let serde_json::Value::Object(base_map) = base {
        base_map.remove("model");
        base_map.remove("reasoningEffort");
    }
}

/// 把会话级覆盖（仅 `model` / `reasoningEffort`）叠加到全局 orchestrator base 上。
///
/// 设计约束：会话覆盖**只能**改主模型与思考强度，绝不携带 baseUrl / apiKey，
/// 避免会话级配置悄悄替换连接凭据。`reasoningEffort` 为 JSON `null` 时恢复为
/// 产品默认的中等推理强度，运行期不允许出现空强度。
pub fn merge_orchestrator_session_override(
    base: &mut serde_json::Value,
    override_section: &serde_json::Value,
) {
    let serde_json::Value::Object(override_map) = override_section else {
        return;
    };
    if override_map.is_empty() {
        return;
    }
    if !base.is_object() {
        *base = serde_json::Value::Object(serde_json::Map::new());
    }
    let serde_json::Value::Object(base_map) = base else {
        return;
    };
    if let Some(model) = override_map.get("model")
        && let Some(model) = model.as_str()
        && !model.trim().is_empty()
    {
        base_map.insert(
            "model".to_string(),
            serde_json::Value::String(model.trim().to_string()),
        );
    }
    if override_map.contains_key("reasoningEffort") {
        match override_map.get("reasoningEffort") {
            Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
                base_map.insert(
                    "reasoningEffort".to_string(),
                    serde_json::Value::String(value.trim().to_string()),
                );
            }
            Some(serde_json::Value::Null) => {
                base_map.insert(
                    "reasoningEffort".to_string(),
                    serde_json::Value::String(DEFAULT_ORCHESTRATOR_REASONING_EFFORT.to_string()),
                );
            }
            _ => {}
        }
    }
}

struct RoleEngineBinding {
    engine_id: String,
    binding_revision: u32,
    enabled: bool,
}

fn role_engine_binding(
    settings_store: &magi_settings_store::SettingsStore,
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
        // `engineId` 空串 = 继承编排模型（resolve_target_for_role 在 Agent 分支返回 None 后
        // 上层会显式回退到 Orchestrator）；非空 = 显式绑定到某个 engine。
        // 该字段是「继承 vs 显式」的唯一事实源，不再保留 modelSource 二次枚举。
        let engine_id = string_field(raw, "engineId").unwrap_or_default();
        if engine_id.is_empty() {
            return None;
        }
        let enabled = raw.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        return Some(RoleEngineBinding {
            engine_id,
            binding_revision: binding_revision(raw),
            enabled,
        });
    }
    None
}

fn engine_llm_config(
    settings_store: &magi_settings_store::SettingsStore,
    engine_id: &str,
) -> Option<Value> {
    let engine_id = engine_id.trim();
    if engine_id.is_empty() {
        return None;
    }
    let engines = settings_store.get_section("engines");
    let entries = engines.as_array()?;
    for entry in entries {
        let Some(id) = string_field(entry, "id") else {
            continue;
        };
        if id != engine_id {
            continue;
        }
        let llm = entry.get("llm")?.clone();
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

fn is_anthropic_model_name(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

fn is_anthropic_endpoint(normalized_base_url: &str) -> bool {
    normalized_base_url.ends_with("/anthropic") || normalized_base_url.ends_with("/messages")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model_config(value: Value) -> NormalizedModelConfig {
        NormalizedModelConfig::from_settings_value(&value).expect("模型配置应符合当前协议")
    }

    #[test]
    fn standard_mode_v1_suffix_infers_openai_chat() {
        let config = model_config(json!({
            "baseUrl": "https://api.deepseek.com/v1",
            "apiKey": "sk-test",
            "model": "deepseek-chat",
            "urlMode": "standard"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn standard_mode_without_v1_suffix_uses_openai_chat() {
        let config = model_config(json!({
            "baseUrl": "https://gateway.example.com",
            "apiKey": "sk-test",
            "model": "gateway-model",
            "urlMode": "standard"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn standard_mode_claude_model_infers_anthropic_messages() {
        let config = model_config(json!({
            "baseUrl": "https://gateway.example.com",
            "apiKey": "sk-test",
            "model": "kiro-claude-sonnet-4-6",
            "urlMode": "standard"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(config.provider(), "anthropic");
    }

    #[test]
    fn standard_mode_anthropic_prefix_overrides_non_claude_model_name() {
        let config = model_config(json!({
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiKey": "sk-test",
            "model": "deepseek-chat",
            "urlMode": "standard"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(config.provider(), "anthropic");
    }

    #[test]
    fn same_base_url_routes_by_model_family() {
        let base = "https://gateway.example.com";
        let claude_config = model_config(json!({
            "baseUrl": base,
            "apiKey": "sk-test",
            "model": "claude-opus-4-5",
            "urlMode": "standard"
        }));
        let gpt_config = model_config(json!({
            "baseUrl": base,
            "apiKey": "sk-test",
            "model": "gpt-5",
            "urlMode": "standard"
        }));

        assert_eq!(
            claude_config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(
            gpt_config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn full_mode_messages_suffix_infers_anthropic() {
        let config = model_config(json!({
            "baseUrl": "https://proxy.example.com/v1/messages",
            "apiKey": "sk-test",
            "model": "claude-sonnet",
            "urlMode": "full"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(config.provider(), "anthropic");
    }

    #[test]
    fn full_mode_chat_completions_suffix_infers_openai() {
        let config = model_config(json!({
            "baseUrl": "https://proxy.example.com/v1/chat/completions",
            "apiKey": "sk-test",
            "model": "gpt-4",
            "urlMode": "full"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn trailing_slash_does_not_affect_v1_inference() {
        let config = model_config(json!({
            "baseUrl": "https://api.deepseek.com/v1/",
            "apiKey": "sk-test",
            "model": "deepseek-chat",
            "urlMode": "standard"
        }));
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn deprecated_model_config_fields_are_rejected() {
        for field in DEPRECATED_MODEL_CONFIG_FIELDS {
            let mut config = json!({
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKey": "sk-test",
                "model": "deepseek-chat",
                "urlMode": "standard"
            });
            config[field] = json!("deprecated");

            let error = NormalizedModelConfig::from_settings_value(&config)
                .expect_err("废弃模型配置字段必须被拒绝");
            assert!(error.contains(field), "错误信息应指出被拒绝字段: {error}");
        }
    }

    #[test]
    fn normalized_model_config_preserves_openai_fetch_models_contract() {
        let config = model_config(json!({
            "baseUrl": "http://127.0.0.1:8320/v1",
            "apiKey": "test-key",
            "urlMode": "standard"
        }));

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
    fn standard_root_base_url_uses_openai_compatible_models_listing() {
        let config = model_config(json!({
            "baseUrl": "https://api.anthropic.com",
            "apiKey": "test-key",
            "urlMode": "standard"
        }));

        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        config
            .require_models_listable()
            .expect("standard url mode should list OpenAI-compatible models");
        assert_eq!(
            config.models_list_url().expect("models url"),
            "https://api.anthropic.com/v1/models"
        );
    }

    #[test]
    fn full_mode_rejects_models_listing() {
        let config = model_config(json!({
            "baseUrl": "http://127.0.0.1:8320/v1/chat/completions",
            "apiKey": "test-key",
            "urlMode": "full"
        }));

        let error = config
            .models_list_url()
            .expect_err("full path has no canonical models endpoint");
        assert!(error.contains("完整路径模式下不支持自动获取模型列表"));
    }

    #[test]
    fn usage_llm_config_drops_legacy_protocol_field() {
        let config = model_config(json!({
            "baseUrl": "https://example.test/v1",
            "model": "gpt-test",
            "urlMode": "standard"
        }));

        let usage = config.to_usage_llm_config().expect("usage config");
        assert_eq!(usage.provider, "openai");
        assert_eq!(usage.model, "gpt-test");
        assert_eq!(usage.url_mode, UrlMode::Default);
    }

    #[test]
    fn http_client_uses_inferred_protocol() {
        let config = model_config(json!({
            "baseUrl": "https://api.deepseek.com/v1",
            "apiKey": "test-key",
            "model": "deepseek-chat",
            "urlMode": "standard"
        }));

        assert!(config.to_http_model_client().is_some());
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn http_client_uses_anthropic_for_standard_claude_model() {
        let config = model_config(json!({
            "baseUrl": "https://api.anthropic.com",
            "apiKey": "test-key",
            "model": "claude-sonnet",
            "urlMode": "standard"
        }));

        assert!(config.to_http_model_client().is_some());
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
    }

    #[test]
    fn full_mode_path_overrides_claude_model_name() {
        let config = model_config(json!({
            "baseUrl": "https://openai-compatible.example.com/v1/chat/completions",
            "apiKey": "test-key",
            "model": "claude-sonnet",
            "urlMode": "full"
        }));

        assert!(config.to_http_model_client().is_some());
        assert_eq!(
            config.inferred_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn role_engine_model_config_resolves_agent_binding() {
        let store = magi_settings_store::SettingsStore::new();
        store
            .set_section(
                "agents",
                json!([{
                    "templateId": "reviewer",
                    "engineId": "sonnet-4-5",
                    "bindingRevision": 7,
                    "enabled": true
                }]),
            )
            .unwrap();
        store
            .set_section(
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
            )
            .unwrap();

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
        let store = magi_settings_store::SettingsStore::new();
        store
            .set_section(
                "agents",
                json!([{
                    "templateId": "executor",
                    "engineId": "",
                    "enabled": true
                }]),
            )
            .unwrap();

        assert!(
            configured_role_engine_model_config(&store, "executor")
                .expect("orchestrator inheritance is valid")
                .is_none()
        );
    }
}
