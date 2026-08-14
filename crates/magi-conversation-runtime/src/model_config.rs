//! 任务系统 — model config helpers。
//!
//! 错误返回值使用 `Result<_, String>`，由上层调用方桥接到自己的错误类型。
//!
//! 请求协议的**唯一事实源**是模型配置中的 `apiProtocol`。模型名称和 URL 只描述
//! 模型身份与地址，不参与协议路由，避免同一聚合网关切换模型时改变请求结构。
//!
//! `provider` 字段不再参与路由决策，仅作为统计/展示标签，由 `apiProtocol` 派生。
//! 配置输入不再接受 `provider` / `openaiProtocol` / `protocolEndpoint`，避免多个字段
//! 同时表达协议。

use magi_bridge_client::{
    EndpointUrlMode, HttpImageGenerationClient, HttpModelBridgeClient, HttpModelBridgeProtocol,
};
use magi_core::SessionId;
use magi_settings_store::DEPRECATED_MODEL_CONFIG_FIELDS;
use magi_usage_authority::{LlmConfig, ReasoningEffort, UrlMode};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

pub const DEFAULT_ORCHESTRATOR_REASONING_EFFORT: &str = "medium";
pub const VISION_MODEL_SECTION: &str = "vision";
pub const DEFAULT_VISION_CONTEXT_WINDOW: u64 = 128_000;

const BUILTIN_TEXT_MODEL_RULES: &[(&str, &str)] = &[
    ("openai-gpt-3.5", r"(?i)^gpt-3\.5(?:-turbo)?(?:[-.:].*)?$"),
    ("openai-text-family", r"(?i)^text[-.:].+$"),
    ("openai-o-mini", r"(?i)^(?:o1-mini|o3-mini)(?:[-.:].*)?$"),
    (
        "deepseek-text-family",
        r"(?i)^deepseek-(?:chat|reasoner|coder)(?:[-.:].*)?$",
    ),
    (
        "qwen-coder-family",
        r"(?i)^(?:qwen|qwq)[^/]*coder(?:[-.:].*)?$",
    ),
    ("codestral-family", r"(?i)^codestral(?:[-.:].*)?$"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextModelRuleMatchMode {
    Exact,
    Regex,
}

impl TextModelRuleMatchMode {
    fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "exact" => Some(Self::Exact),
            "regex" => Some(Self::Regex),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextModelRule {
    pub match_mode: TextModelRuleMatchMode,
    pub pattern: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelUrlMode {
    Standard,
    Full,
    Proxy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelApiProtocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

impl ModelApiProtocol {
    fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai_chat" => Some(Self::OpenAiChat),
            "openai_responses" => Some(Self::OpenAiResponses),
            "anthropic_messages" => Some(Self::AnthropicMessages),
            _ => None,
        }
    }

    fn to_http_protocol(self) -> HttpModelBridgeProtocol {
        match self {
            Self::OpenAiChat => HttpModelBridgeProtocol::ChatCompletions,
            Self::OpenAiResponses => HttpModelBridgeProtocol::Responses,
            Self::AnthropicMessages => HttpModelBridgeProtocol::AnthropicMessages,
        }
    }

    fn provider(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai",
            Self::OpenAiResponses => "openai",
            Self::AnthropicMessages => "anthropic",
        }
    }
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
    api_protocol: ModelApiProtocol,
    reasoning_effort: Option<ModelReasoningEffort>,
    context_window_tokens: Option<u64>,
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
    /// `apiProtocol` 是请求协议唯一事实源。已配置的模型连接缺少该字段时直接拒绝，
    /// 防止运行时重新根据模型名或地址猜测协议。完全空的 section 仅用于表达“未配置”，
    /// 不会构造客户端。
    pub fn from_settings_value(value: &Value) -> Result<Self, String> {
        reject_deprecated_model_config_fields(value)?;
        let url_mode_label =
            string_field(value, "urlMode").unwrap_or_else(|| "standard".to_string());
        let has_connection_fields = ["baseUrl", "apiKey", "model", "urlMode", "apiProtocol"]
            .iter()
            .any(|field| value.get(*field).is_some());
        let api_protocol = match string_field(value, "apiProtocol") {
            Some(label) => ModelApiProtocol::from_label(&label).ok_or_else(|| {
                "apiProtocol 无效，必须是 openai_chat、openai_responses 或 anthropic_messages"
                    .to_string()
            })?,
            None if has_connection_fields => {
                return Err("模型配置缺少 apiProtocol".to_string());
            }
            None => ModelApiProtocol::OpenAiChat,
        };
        Ok(Self {
            base_url: string_field(value, "baseUrl"),
            api_key: string_field(value, "apiKey"),
            model: string_field(value, "model"),
            url_mode: ModelUrlMode::from_label(&url_mode_label),
            api_protocol,
            reasoning_effort: value
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .and_then(ModelReasoningEffort::from_label),
            context_window_tokens: value.get("contextWindowTokens").and_then(Value::as_u64),
        })
    }

    /// 从显式协议派生的 provider 标签，用于 usage authority 分组与展示。
    pub fn provider(&self) -> &'static str {
        self.api_protocol.provider()
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

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let model = model.into();
        self.model = (!model.trim().is_empty()).then(|| model.trim().to_string());
        self
    }

    pub fn api_protocol(&self) -> HttpModelBridgeProtocol {
        self.api_protocol.to_http_protocol()
    }

    pub fn context_window_tokens(&self) -> Option<u64> {
        self.context_window_tokens
    }

    pub fn to_http_model_client(&self) -> Option<HttpModelBridgeClient> {
        let base_url = self.normalized_http_base_url().ok()?;
        let model = self.model.as_deref()?.trim();
        if model.is_empty() {
            return None;
        }
        let url_mode = match self.url_mode {
            ModelUrlMode::Full => EndpointUrlMode::Full,
            ModelUrlMode::Standard | ModelUrlMode::Proxy => EndpointUrlMode::Standard,
        };
        Some(HttpModelBridgeClient::new_with_protocol_and_url_mode(
            base_url,
            self.api_key.clone(),
            model.to_string(),
            self.api_protocol(),
            url_mode,
            self.reasoning_effort
                .map(ModelReasoningEffort::to_usage_reasoning_effort),
        ))
    }

    pub fn to_http_image_generation_client(&self) -> Result<HttpImageGenerationClient, String> {
        let base_url = self.normalized_http_base_url()?;
        let model = self.require_model()?.to_string();
        let url_mode = match self.url_mode {
            ModelUrlMode::Full => EndpointUrlMode::Full,
            ModelUrlMode::Standard | ModelUrlMode::Proxy => EndpointUrlMode::Standard,
        };
        Ok(HttpImageGenerationClient::new(
            base_url,
            self.api_key.clone(),
            model,
            url_mode,
        ))
    }

    /// 图片理解使用普通对话协议，独立于图片生成协议。
    pub fn to_http_vision_client(&self) -> Option<HttpModelBridgeClient> {
        self.to_http_model_client()
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
        let base_url = self.require_base_url()?.trim();
        if base_url.is_empty() {
            return Err("模型配置缺少有效的 baseUrl".to_string());
        }
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err("baseUrl 必须以 http:// 或 https:// 开头".to_string());
        }
        match self.url_mode {
            ModelUrlMode::Full => Ok(base_url.to_string()),
            ModelUrlMode::Standard | ModelUrlMode::Proxy => {
                Ok(base_url.trim_end_matches('/').to_string())
            }
        }
    }
}

fn builtin_text_model_regexes() -> &'static [Regex] {
    static RULES: OnceLock<Vec<Regex>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            BUILTIN_TEXT_MODEL_RULES
                .iter()
                .map(|(_, pattern)| Regex::new(pattern).expect("内置文本模型正则必须有效"))
                .collect()
        })
        .as_slice()
}

pub fn parse_user_text_model_rules(value: &Value) -> Result<Vec<TextModelRule>, String> {
    let Some(entries) = value.get("textModelRules") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| "textModelRules 必须是数组".to_string())?;
    if entries.len() > 128 {
        return Err("textModelRules 最多允许 128 条规则".to_string());
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let match_mode = entry
                .get("matchMode")
                .and_then(Value::as_str)
                .and_then(TextModelRuleMatchMode::from_label)
                .ok_or_else(|| {
                    format!("textModelRules[{index}].matchMode 必须是 exact 或 regex")
                })?;
            let pattern = entry
                .get("pattern")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .ok_or_else(|| format!("textModelRules[{index}].pattern 不能为空"))?;
            if pattern.len() > 512 {
                return Err(format!(
                    "textModelRules[{index}].pattern 不能超过 512 个字符"
                ));
            }
            if match_mode == TextModelRuleMatchMode::Regex {
                Regex::new(pattern)
                    .map_err(|error| format!("textModelRules[{index}] 正则无效：{error}"))?;
            }
            Ok(TextModelRule {
                match_mode,
                pattern: pattern.to_string(),
            })
        })
        .collect()
}

pub fn validate_vision_model_settings(value: &Value) -> Result<(), String> {
    let config = NormalizedModelConfig::from_settings_value(value)?;
    config.require_base_url()?;
    config.require_api_key()?;
    config.require_model()?;
    if value.get("contextWindowTokens").is_some()
        && value
            .get("contextWindowTokens")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err("识图模型上下文窗口必须是正整数".to_string());
    }
    let context_window = config
        .context_window_tokens()
        .unwrap_or(DEFAULT_VISION_CONTEXT_WINDOW);
    if !(crate::model_context_window::MIN_MODEL_CONTEXT_WINDOW
        ..=crate::model_context_window::MAX_MODEL_CONTEXT_WINDOW)
        .contains(&context_window)
    {
        return Err(format!(
            "识图模型上下文窗口必须在 {} 到 {} token 之间",
            crate::model_context_window::MIN_MODEL_CONTEXT_WINDOW,
            crate::model_context_window::MAX_MODEL_CONTEXT_WINDOW,
        ));
    }
    parse_user_text_model_rules(value)?;
    if config.to_http_vision_client().is_none() {
        return Err("识图模型接口地址或模型配置无效".to_string());
    }
    Ok(())
}

pub fn model_matches_text_model_rule(
    settings_store: Option<&magi_settings_store::SettingsStore>,
    model: &str,
) -> bool {
    let model = model.trim();
    if model.is_empty() {
        return false;
    }
    if builtin_text_model_regexes()
        .iter()
        .any(|rule| rule.is_match(model))
    {
        return true;
    }
    let Some(store) = settings_store else {
        return false;
    };
    parse_user_text_model_rules(&store.get_section(VISION_MODEL_SECTION))
        .unwrap_or_default()
        .iter()
        .any(|rule| match rule.match_mode {
            TextModelRuleMatchMode::Exact => rule.pattern.eq_ignore_ascii_case(model),
            TextModelRuleMatchMode::Regex => {
                Regex::new(&rule.pattern).is_ok_and(|pattern| pattern.is_match(model))
            }
        })
}

pub fn resolve_vision_execution_config(
    settings_store: Option<&magi_settings_store::SettingsStore>,
    selected_model: &str,
    request_contains_images: bool,
) -> Result<Option<NormalizedModelConfig>, String> {
    if !request_contains_images || !model_matches_text_model_rule(settings_store, selected_model) {
        return Ok(None);
    }
    let store = settings_store.ok_or_else(|| {
        format!("模型 {selected_model} 只能处理文本，但当前运行环境没有识图模型配置")
    })?;
    let raw = store.get_section(VISION_MODEL_SECTION);
    validate_vision_model_settings(&raw).map_err(|error| {
        format!("模型 {selected_model} 只能处理文本，识图模型配置不可用：{error}")
    })?;
    NormalizedModelConfig::from_settings_value(&raw).map(Some)
}

pub fn reject_deprecated_model_config_fields(value: &Value) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    for field in DEPRECATED_MODEL_CONFIG_FIELDS {
        if object.contains_key(*field) {
            return Err(format!(
                "模型配置字段 {field} 已废弃，请使用 baseUrl/apiKey/model/urlMode/apiProtocol/reasoningEffort"
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
    let defaults =
        settings_store.get_section(magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION);
    merge_orchestrator_session_override(&mut config, &defaults);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model_config(value: Value) -> NormalizedModelConfig {
        NormalizedModelConfig::from_settings_value(&value).expect("模型配置应符合当前协议")
    }

    #[test]
    fn explicit_openai_protocol_is_independent_of_model_name_and_url() {
        let config = model_config(json!({
            "baseUrl": "https://gateway.example.com/anthropic",
            "apiKey": "sk-test",
            "model": "claude-sonnet",
            "urlMode": "standard",
            "apiProtocol": "openai_chat"
        }));
        assert_eq!(
            config.api_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
        assert_eq!(config.provider(), "openai");
    }

    #[test]
    fn explicit_anthropic_protocol_is_independent_of_model_name_and_url() {
        let config = model_config(json!({
            "baseUrl": "https://gateway.example.com",
            "apiKey": "sk-test",
            "model": "deepseek-chat",
            "urlMode": "standard",
            "apiProtocol": "anthropic_messages"
        }));
        assert_eq!(
            config.api_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
        assert_eq!(config.provider(), "anthropic");
    }

    #[test]
    fn explicit_openai_responses_protocol_is_supported() {
        let config = model_config(json!({
            "baseUrl": "https://api.openai.com",
            "apiKey": "sk-test",
            "model": "gpt-5",
            "urlMode": "standard",
            "apiProtocol": "openai_responses"
        }));

        assert_eq!(config.api_protocol(), HttpModelBridgeProtocol::Responses);
        assert_eq!(config.provider(), "openai");
        assert!(config.to_http_model_client().is_some());
    }

    #[test]
    fn configured_model_requires_explicit_api_protocol() {
        let error = NormalizedModelConfig::from_settings_value(&json!({
            "baseUrl": "https://gateway.example.com",
            "apiKey": "sk-test",
            "model": "kiro-claude-sonnet-4-6",
            "urlMode": "standard"
        }))
        .expect_err("已配置连接不得缺少 apiProtocol");

        assert!(error.contains("缺少 apiProtocol"));
    }

    #[test]
    fn invalid_api_protocol_is_rejected() {
        let error = NormalizedModelConfig::from_settings_value(&json!({
            "baseUrl": "https://gateway.example.com",
            "apiProtocol": "auto"
        }))
        .expect_err("未知协议不得进入运行时");

        assert!(error.contains("apiProtocol 无效"));
    }

    #[test]
    fn deprecated_model_config_fields_are_rejected() {
        for field in DEPRECATED_MODEL_CONFIG_FIELDS {
            let mut config = json!({
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKey": "sk-test",
                "model": "deepseek-chat",
                "urlMode": "standard",
                "apiProtocol": "openai_chat"
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
            "urlMode": "standard",
            "apiProtocol": "openai_chat"
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
            "urlMode": "standard",
            "apiProtocol": "anthropic_messages"
        }));

        assert_eq!(
            config.api_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
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
            "urlMode": "full",
            "apiProtocol": "openai_chat"
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
            "urlMode": "standard",
            "apiProtocol": "openai_chat"
        }));

        let usage = config.to_usage_llm_config().expect("usage config");
        assert_eq!(usage.provider, "openai");
        assert_eq!(usage.model, "gpt-test");
        assert_eq!(usage.url_mode, UrlMode::Default);
    }

    #[test]
    fn http_client_uses_explicit_openai_protocol() {
        let config = model_config(json!({
            "baseUrl": "https://api.deepseek.com/v1",
            "apiKey": "test-key",
            "model": "deepseek-chat",
            "urlMode": "standard",
            "apiProtocol": "openai_chat"
        }));

        assert!(config.to_http_model_client().is_some());
        assert_eq!(
            config.api_protocol(),
            HttpModelBridgeProtocol::ChatCompletions
        );
    }

    #[test]
    fn http_client_uses_explicit_anthropic_protocol() {
        let config = model_config(json!({
            "baseUrl": "https://api.anthropic.com",
            "apiKey": "test-key",
            "model": "claude-sonnet",
            "urlMode": "standard",
            "apiProtocol": "anthropic_messages"
        }));

        assert!(config.to_http_model_client().is_some());
        assert_eq!(
            config.api_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
    }

    #[test]
    fn full_mode_does_not_override_explicit_protocol() {
        let config = model_config(json!({
            "baseUrl": "https://openai-compatible.example.com/v1/chat/completions",
            "apiKey": "test-key",
            "model": "claude-sonnet",
            "urlMode": "full",
            "apiProtocol": "anthropic_messages"
        }));

        assert!(config.to_http_model_client().is_some());
        assert_eq!(
            config.api_protocol(),
            HttpModelBridgeProtocol::AnthropicMessages
        );
    }

    #[test]
    fn text_model_rules_combine_builtin_and_user_entries() {
        let store = magi_settings_store::SettingsStore::new();
        assert!(model_matches_text_model_rule(
            Some(&store),
            "deepseek-reasoner"
        ));
        assert!(!model_matches_text_model_rule(Some(&store), "gpt-4.1"));
        store
            .set_section(
                VISION_MODEL_SECTION,
                json!({
                    "textModelRules": [
                        {"matchMode": "exact", "pattern": "company-text-model"},
                        {"matchMode": "regex", "pattern": "^legacy-[0-9]+$"}
                    ]
                }),
            )
            .unwrap();
        assert!(model_matches_text_model_rule(
            Some(&store),
            "COMPANY-TEXT-MODEL"
        ));
        assert!(model_matches_text_model_rule(Some(&store), "legacy-42"));
    }

    #[test]
    fn vision_execution_requires_both_image_input_and_text_model_match() {
        let store = magi_settings_store::SettingsStore::new();
        store
            .set_section(
                VISION_MODEL_SECTION,
                json!({
                    "baseUrl": "https://vision.example.com/v1",
                    "apiKey": "test-key",
                    "model": "vision-model",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                    "contextWindowTokens": 256000,
                    "textModelRules": []
                }),
            )
            .unwrap();

        assert!(
            resolve_vision_execution_config(Some(&store), "deepseek-chat", false)
                .unwrap()
                .is_none(),
            "纯文本请求不得切换识图模型"
        );
        assert!(
            resolve_vision_execution_config(Some(&store), "gpt-4.1", true)
                .unwrap()
                .is_none(),
            "未命中文本模型规则时不得切换识图模型"
        );
        let resolved = resolve_vision_execution_config(Some(&store), "deepseek-chat", true)
            .unwrap()
            .expect("图片请求命中文本模型规则时必须切换识图模型");
        assert_eq!(resolved.require_model().unwrap(), "vision-model");
        assert_eq!(resolved.context_window_tokens(), Some(256000));
    }

    #[test]
    fn vision_settings_reject_invalid_regex_and_context_window() {
        let invalid_regex = json!({
            "baseUrl": "https://vision.example.com/v1",
            "apiKey": "test-key",
            "model": "vision-model",
            "urlMode": "standard",
            "apiProtocol": "openai_chat",
            "contextWindowTokens": 128000,
            "textModelRules": [{"matchMode": "regex", "pattern": "("}]
        });
        assert!(validate_vision_model_settings(&invalid_regex).is_err());

        let invalid_window = json!({
            "baseUrl": "https://vision.example.com/v1",
            "apiKey": "test-key",
            "model": "vision-model",
            "urlMode": "standard",
            "apiProtocol": "openai_chat",
            "contextWindowTokens": 1000
        });
        assert!(validate_vision_model_settings(&invalid_window).is_err());
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
                        "apiProtocol": "openai_chat",
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
            resolved.config.api_protocol(),
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

    #[test]
    fn orchestrator_model_config_uses_defaults_for_legacy_session() {
        let store = magi_settings_store::SettingsStore::new();
        store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat",
                }),
            )
            .unwrap();
        store
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                json!({
                    "model": "model-last-used",
                    "reasoningEffort": "high"
                }),
            )
            .unwrap();
        let legacy_session = SessionId::new("session-without-model-override");

        let config = resolve_orchestrator_model_config(&store, Some(&legacy_session))
            .expect("旧会话应继承权威的用户默认模型");
        assert_eq!(
            config.require_model().expect("默认模型必须可执行"),
            "model-last-used"
        );
        assert_eq!(
            config
                .to_usage_llm_config()
                .and_then(|config| config.reasoning_effort),
            Some(magi_usage_authority::ReasoningEffort::High),
        );
    }
}
