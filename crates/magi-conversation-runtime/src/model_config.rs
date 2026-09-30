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
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;

pub const DEFAULT_ORCHESTRATOR_REASONING_EFFORT: &str = "medium";
pub const VISION_MODEL_SECTION: &str = "vision";
pub const DEFAULT_VISION_CONTEXT_WINDOW: u64 = 128_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinTextModelRuleDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    pub examples: &'static [&'static str],
    #[serde(skip)]
    pattern: &'static str,
}

const BUILTIN_TEXT_MODEL_RULES: &[BuiltinTextModelRuleDefinition] = &[
    BuiltinTextModelRuleDefinition {
        id: "glm-4.5-air-family",
        display_name: "GLM 4.5 Air",
        examples: &["glm-4.5-air", "glm 4.5 air"],
        pattern: r"(?i)^glm[-_.: ]*4[-_.: ]*5[-_.: ]*air(?:[-_.: ].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "glm-5.2-family",
        display_name: "GLM 5.2",
        examples: &["glm-5.2", "glm 5.2-air"],
        pattern: r"(?i)^glm[-_.: ]*5[-_.: ]*2(?:[-_.: ].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "deepseek-v4-family",
        display_name: "DeepSeek V4",
        examples: &["deepseek-v4", "deepseek-v4-flash"],
        pattern: r"(?i)^deepseek[-_.: ]*v?4(?:[-_.: ].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "deepseek-text-family",
        display_name: "DeepSeek Chat / Reasoner / Coder",
        examples: &["deepseek-chat", "deepseek-reasoner"],
        pattern: r"(?i)^deepseek-(?:chat|reasoner|coder)(?:[-.:].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "openai-gpt-3.5",
        display_name: "GPT-3.5",
        examples: &["gpt-3.5-turbo"],
        pattern: r"(?i)^gpt-3\.5(?:-turbo)?(?:[-.:].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "openai-o-mini",
        display_name: "O1 Mini / O3 Mini",
        examples: &["o1-mini", "o3-mini"],
        pattern: r"(?i)^(?:o1-mini|o3-mini)(?:[-.:].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "qwen-coder-family",
        display_name: "Qwen / QwQ Coder",
        examples: &["qwen-coder", "qwq-coder"],
        pattern: r"(?i)^(?:qwen|qwq)[^/]*coder(?:[-.:].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "codestral-family",
        display_name: "Codestral",
        examples: &["codestral-latest"],
        pattern: r"(?i)^codestral(?:[-.:].*)?$",
    },
    BuiltinTextModelRuleDefinition {
        id: "openai-text-family",
        display_name: "OpenAI Text",
        examples: &["text-davinci-003"],
        pattern: r"(?i)^text[-.:].+$",
    },
];

pub fn builtin_text_model_rule_catalog() -> &'static [BuiltinTextModelRuleDefinition] {
    BUILTIN_TEXT_MODEL_RULES
}

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
    /// GPT Web 模型：推理由内置浏览器里的 ChatGPT 网页完成，**没有 HTTP 传输**。
    ///
    /// 它不写 `llm`、不带 `baseUrl / apiKey / model`，因此任何把协议当 HTTP
    /// 处理的分支都必须显式拒绝它，不得回退到 `openai_chat`（设计基线 §5.6、A7）。
    ChatGptWeb,
}

impl ModelApiProtocol {
    fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai_chat" => Some(Self::OpenAiChat),
            "openai_responses" => Some(Self::OpenAiResponses),
            "anthropic_messages" => Some(Self::AnthropicMessages),
            "chatgpt_web" => Some(Self::ChatGptWeb),
            _ => None,
        }
    }

    /// 归一为 HTTP 传输协议；`None` 表示该协议**没有 HTTP 传输**。
    ///
    /// 返回 `Option` 让调用方在编译期就无法把 Web 引擎当成 HTTP 协议处理。
    fn to_http_protocol(self) -> Option<HttpModelBridgeProtocol> {
        match self {
            Self::OpenAiChat => Some(HttpModelBridgeProtocol::ChatCompletions),
            Self::OpenAiResponses => Some(HttpModelBridgeProtocol::Responses),
            Self::AnthropicMessages => Some(HttpModelBridgeProtocol::AnthropicMessages),
            Self::ChatGptWeb => None,
        }
    }

    fn provider(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai",
            Self::OpenAiResponses => "openai",
            Self::AnthropicMessages => "anthropic",
            Self::ChatGptWeb => "chatgpt_web",
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

    /// 归一化标签，与 settings 中 `reasoningEffort` 的取值一致。
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
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
                "apiProtocol 无效，必须是 openai_chat、openai_responses、anthropic_messages 或 chatgpt_web"
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

    /// 该配置的 HTTP 传输协议；`None` 表示非 HTTP 引擎（GPT Web）。
    pub fn api_protocol(&self) -> Option<HttpModelBridgeProtocol> {
        self.api_protocol.to_http_protocol()
    }

    /// 该配置是否就是非 HTTP 的 GPT Web 引擎。
    pub fn is_chatgpt_web(&self) -> bool {
        self.api_protocol == ModelApiProtocol::ChatGptWeb
    }

    pub fn context_window_tokens(&self) -> Option<u64> {
        self.context_window_tokens
    }

    /// 归一化后的思考强度标签；未配置时为 `None`。
    pub fn reasoning_effort_label(&self) -> Option<&'static str> {
        self.reasoning_effort.map(ModelReasoningEffort::label)
    }

    pub fn to_http_model_client(&self) -> Option<HttpModelBridgeClient> {
        // GPT Web 引擎没有 HTTP 传输：显式拒绝，绝不回退到任何 HTTP 协议
        // （设计基线 §5.6：宁可报错，不得静默换 HTTP 模型）。
        let protocol = self.api_protocol.to_http_protocol()?;
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
            protocol,
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
                .map(|rule| Regex::new(rule.pattern).expect("内置文本模型正则必须有效"))
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
        .any(|regex| regex.is_match(model))
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
    current_turn_contains_images: bool,
) -> Result<Option<NormalizedModelConfig>, String> {
    if !current_turn_contains_images
        || !model_matches_text_model_rule(settings_store, selected_model)
    {
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
    let defaults =
        settings_store.get_section(magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION);
    let session_override = session_id
        .map(|session_id| settings_store.get_session_section(session_id, "orchestrator"))
        .unwrap_or(serde_json::Value::Null);
    // 引擎绑定（A22）：会话级 `engineId` 决定该会话由哪个引擎承载。绑定了引擎就
    // 只读该引擎，绝不叠加全局 base——否则用户选中的 Web 引擎会被 HTTP 模型顶替，
    // 或反过来让 Web 引擎拿到不相干的连接配置。
    if let Some(engine_id) = orchestrator_engine_id(settings_store, session_id) {
        let entry = engine_entry(settings_store, &engine_id)
            .ok_or_else(|| format!("会话绑定的模型引擎不存在：{engine_id}"))?;
        let mut config = if is_chatgpt_web_engine_entry(&entry) {
            chatgpt_web_engine_config(&entry, &engine_id)?
        } else {
            let llm = entry
                .get("llm")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if llm.as_object().is_none_or(|object| object.is_empty()) {
                return Err(format!("模型引擎 {engine_id} 缺少 llm 配置"));
            }
            llm
        };
        merge_orchestrator_session_override(&mut config, &defaults);
        merge_orchestrator_session_override(&mut config, &session_override);
        ensure_orchestrator_reasoning_effort(&mut config);
        return NormalizedModelConfig::from_settings_value(&config)
            .map_err(|error| format!("引擎 {engine_id} 的模型配置无效：{error}"));
    }
    let mut config = settings_store.get_section("orchestrator");
    strip_orchestrator_session_owned_fields(&mut config);
    merge_orchestrator_session_override(&mut config, &defaults);
    merge_orchestrator_session_override(&mut config, &session_override);
    ensure_orchestrator_reasoning_effort(&mut config);
    NormalizedModelConfig::from_settings_value(&config)
        .map_err(|error| format!("orchestrator 模型配置无效：{error}"))
}

/// 会话级主模型覆盖的引擎绑定（A22），只在会话级 section 生效。
///
/// 引擎绑定不是跨会话默认值：`ORCHESTRATOR_SESSION_DEFAULTS_SECTION` 不允许携带
/// `engineId`（settings-store 的归一化会剥离它），这里也只读会话级 override。
pub fn orchestrator_engine_id(
    settings_store: &magi_settings_store::SettingsStore,
    session_id: Option<&SessionId>,
) -> Option<String> {
    let session_id = session_id?;
    let section = settings_store.get_session_section(session_id, "orchestrator");
    string_field(&section, "engineId")
}

/// 角色绑定的引擎 id（角色不存在或未绑定引擎时返回 `None`）。
pub fn role_bound_engine_id(
    settings_store: &magi_settings_store::SettingsStore,
    role_id: &str,
) -> Option<String> {
    role_engine_binding(settings_store, role_id).map(|binding| binding.engine_id)
}

/// 角色绑定的引擎是否就是 GPT Web 引擎。
///
/// 角色继承编排模型时（`engineId` 为空）返回 `false`：继承的判定在
/// conversation dispatcher，不在这里重复。
pub fn role_engine_is_chatgpt_web(
    settings_store: &magi_settings_store::SettingsStore,
    role_id: &str,
) -> bool {
    let Some(binding) = role_engine_binding(settings_store, role_id) else {
        return false;
    };
    engine_entry(settings_store, &binding.engine_id)
        .is_some_and(|entry| is_chatgpt_web_engine_entry(&entry))
}

/// Web 引擎的会话级运行期开关（§3.10）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrchestratorWebEngineSettings {
    pub engine_id: String,
    pub effort: String,
    /// 工具能力开关，默认开启（A9）。
    pub tool_enabled: bool,
    /// 「每轮新建对话」，默认关闭（§5.6）。
    pub new_chat_per_turn: bool,
    pub tool_round_limit: u32,
    /// 连接器是否已由站点适配层配置并回读确认（`origin.connector`，§5.7.3）。
    ///
    /// 没配置连接器时 T3 不可能成立，因此**请求档位**直接落 T2；配置了连接器才
    /// 请求 T3，由宿主按通道就绪状态决定是否真的走 T3（A15）。
    pub connector_configured: bool,
}

/// 读取会话级 Web 引擎的引擎级开关；未绑定引擎或绑定的是 HTTP 引擎时返回 `None`。
pub fn orchestrator_web_engine_settings(
    settings_store: &magi_settings_store::SettingsStore,
    session_id: Option<&SessionId>,
) -> Result<Option<OrchestratorWebEngineSettings>, String> {
    let Some(engine_id) = orchestrator_engine_id(settings_store, session_id) else {
        return Ok(None);
    };
    let config = resolve_orchestrator_model_config(settings_store, session_id)?;
    let effort = config.reasoning_effort_label().map(str::to_string);
    web_engine_settings(settings_store, &engine_id, effort)
}

/// 按显式引擎 id 读取 Web 引擎开关。
///
/// 两个调用方各自提供 effort：编排者来自会话级覆盖，角色来自角色绑定配置。
pub fn web_engine_settings(
    settings_store: &magi_settings_store::SettingsStore,
    engine_id: &str,
    effort: Option<String>,
) -> Result<Option<OrchestratorWebEngineSettings>, String> {
    let entry = engine_entry(settings_store, engine_id)
        .ok_or_else(|| format!("模型引擎不存在：{engine_id}"))?;
    if !is_chatgpt_web_engine_entry(&entry) {
        return Ok(None);
    }
    Ok(Some(OrchestratorWebEngineSettings {
        engine_id: engine_id.trim().to_string(),
        effort: effort
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_ORCHESTRATOR_REASONING_EFFORT.to_string()),
        tool_enabled: entry
            // 引擎级工具开关的字段名只有 `toolsEnabled`：settings 写入路径、
            // daemon 可用性投影与前端引擎条目都用它。读另一个名字等于让开关
            // 静默失效（A9、§5.7.0）。
            .get("toolsEnabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        new_chat_per_turn: entry
            .get("newChatPerTurn")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        tool_round_limit: entry
            .get("toolRoundLimit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_WEB_TOOL_ROUND_LIMIT),
        connector_configured: entry
            .get("origin")
            .and_then(|origin| origin.get("connector"))
            .is_some_and(|connector| !connector.is_null()),
    }))
}

/// Web 引擎的 T2 轮数上限默认值，与 `magi-web-model` 的 client 默认保持一致。
pub const DEFAULT_WEB_TOOL_ROUND_LIMIT: u32 = 20;

/// 引擎条目是否是 GPT Web 引擎（`apiProtocol = chatgpt_web`，不写 `llm`）。
pub fn is_chatgpt_web_engine_entry(entry: &serde_json::Value) -> bool {
    entry.get("apiProtocol").and_then(serde_json::Value::as_str) == Some("chatgpt_web")
}

/// Web 引擎条目的顶层字段就是它的运行时配置（A22：不写 `llm`）。
///
/// `contextWindowTokens` 由发现通道把站点上限写进引擎顶层，是上下文预算的输入。
fn chatgpt_web_engine_config(
    entry: &serde_json::Value,
    engine_id: &str,
) -> Result<serde_json::Value, String> {
    let family = engine_id
        .strip_prefix("chatgpt-web/")
        .unwrap_or(engine_id)
        .to_string();
    let mut config = serde_json::Map::new();
    config.insert(
        "apiProtocol".to_string(),
        serde_json::Value::String("chatgpt_web".to_string()),
    );
    config.insert(
        "model".to_string(),
        serde_json::Value::String(string_field(entry, "model").unwrap_or(family)),
    );
    if let Some(tokens) = entry
        .get("contextWindowTokens")
        .and_then(serde_json::Value::as_u64)
    {
        config.insert(
            "contextWindowTokens".to_string(),
            serde_json::Value::from(tokens),
        );
    }
    Ok(serde_json::Value::Object(config))
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
    // 引擎绑定（A22）：会话级 `engineId` 与其它字段同一条合并路径。
    // 空串是显式的「继承编排模型」，必须**删除**已有绑定，否则选择器无法把
    // 会话从 Web 引擎切回 provider 模型。
    if let Some(engine_id) = override_map.get("engineId") {
        if let Some(engine_id) = engine_id.as_str() {
            let engine_id = engine_id.trim();
            if engine_id.is_empty() {
                base_map.remove("engineId");
            } else {
                base_map.insert(
                    "engineId".to_string(),
                    serde_json::Value::String(engine_id.to_string()),
                );
            }
        }
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
        // 上层显式选择 Orchestrator）；非空 = 显式绑定到某个 engine。
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

/// 按 id 找引擎条目。
fn engine_entry(
    settings_store: &magi_settings_store::SettingsStore,
    engine_id: &str,
) -> Option<Value> {
    let engine_id = engine_id.trim();
    if engine_id.is_empty() {
        return None;
    }
    let engines = settings_store.get_section("engines");
    let entries = engines.as_array()?;
    entries
        .iter()
        .find(|entry| string_field(entry, "id").is_some_and(|id| id == engine_id))
        .cloned()
}

fn engine_llm_config(
    settings_store: &magi_settings_store::SettingsStore,
    engine_id: &str,
) -> Option<Value> {
    let llm = engine_entry(settings_store, engine_id)?.get("llm")?.clone();
    if llm.as_object().is_none_or(|object| object.is_empty()) {
        return None;
    }
    Some(llm)
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
    fn session_engine_binding_resolves_a_web_engine_without_llm() {
        let store = magi_settings_store::SettingsStore::new();
        let session_id = SessionId::new("session-web-binding");
        store
            .set_section(
                "engines",
                json!([
                    {
                        "id": "chatgpt-web/gpt-5",
                        "displayName": "GPT-5 (Web)",
                        "apiProtocol": "chatgpt_web",
                        "model": "gpt-5",
                        "contextWindowTokens": 120_000,
                        "efforts": ["low", "medium", "high"],
                        "toolsEnabled": true,
                        "newChatPerTurn": false,
                    }
                ]),
            )
            .expect("写入 engines");
        // 全局 base 必须被完全忽略：Web 引擎没有连接凭据，也不允许借用别的连接。
        store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "model": "global-model",
                    "apiProtocol": "openai_chat",
                }),
            )
            .expect("写入 orchestrator");
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({ "engineId": "chatgpt-web/gpt-5", "reasoningEffort": "high" }),
            )
            .expect("写入会话级覆盖");

        let config = resolve_orchestrator_model_config(&store, Some(&session_id))
            .expect("会话绑定的 Web 引擎应当可解析");
        assert!(
            config.is_chatgpt_web(),
            "绑定 Web 引擎后协议必须是 chatgpt_web"
        );
        assert!(
            config.to_http_model_client().is_none(),
            "Web 引擎不得派生出任何 HTTP 传输"
        );
        assert_eq!(config.provider(), "chatgpt_web");
        assert_eq!(config.reasoning_effort_label(), Some("high"));
        assert_eq!(config.context_window_tokens(), Some(120_000));

        let settings = orchestrator_web_engine_settings(&store, Some(&session_id))
            .expect("读取 Web 引擎开关")
            .expect("绑定的是 Web 引擎");
        assert_eq!(settings.engine_id, "chatgpt-web/gpt-5");
        assert_eq!(settings.effort, "high");
        assert!(settings.tool_enabled);
        assert!(!settings.new_chat_per_turn);
        assert_eq!(settings.tool_round_limit, DEFAULT_WEB_TOOL_ROUND_LIMIT);
    }

    #[test]
    fn session_engine_binding_to_a_missing_engine_fails_instead_of_falling_back() {
        let store = magi_settings_store::SettingsStore::new();
        let session_id = SessionId::new("session-missing-engine");
        store
            .set_section(
                "orchestrator",
                json!({
                    "baseUrl": "https://api.example.com/v1",
                    "apiKey": "sk-orch",
                    "model": "global-model",
                    "apiProtocol": "openai_chat",
                }),
            )
            .expect("写入 orchestrator");
        store
            .set_session_section(
                &session_id,
                "orchestrator",
                json!({ "engineId": "chatgpt-web/does-not-exist" }),
            )
            .expect("写入会话级覆盖");
        let error = resolve_orchestrator_model_config(&store, Some(&session_id))
            .expect_err("绑定不存在的引擎必须失败");
        assert!(error.contains("不存在"), "{error}");
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
            Some(HttpModelBridgeProtocol::ChatCompletions)
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
            Some(HttpModelBridgeProtocol::AnthropicMessages)
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

        assert_eq!(
            config.api_protocol(),
            Some(HttpModelBridgeProtocol::Responses)
        );
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
    fn chatgpt_web_engine_has_no_http_transport() {
        // Web 引擎承认 chatgpt_web 这个协议标签，但不得派生出任何 HTTP 传输；
        // 任何把它当 HTTP 处理的分支都会在编译期强制显式处理 `None`（A7、§5.6）。
        let config = NormalizedModelConfig::from_settings_value(&json!({
            "apiProtocol": "chatgpt_web"
        }))
        .expect("chatgpt_web 是已知协议");

        assert!(config.is_chatgpt_web());
        assert_eq!(config.api_protocol(), None);
        assert_eq!(config.provider(), "chatgpt_web");
        assert!(config.to_http_model_client().is_none());
        assert!(config.to_http_vision_client().is_none());
        assert!(
            config.to_http_image_generation_client().is_err(),
            "Web 引擎不得构造 HTTP 图片生成客户端"
        );
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
            Some(HttpModelBridgeProtocol::AnthropicMessages)
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
            Some(HttpModelBridgeProtocol::ChatCompletions)
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
            Some(HttpModelBridgeProtocol::AnthropicMessages)
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
            Some(HttpModelBridgeProtocol::AnthropicMessages)
        );
    }

    #[test]
    fn text_model_rules_combine_builtin_and_user_entries() {
        let store = magi_settings_store::SettingsStore::new();
        assert!(model_matches_text_model_rule(
            Some(&store),
            "deepseek-reasoner"
        ));
        assert!(model_matches_text_model_rule(
            Some(&store),
            "deepseek-v4-flash"
        ));
        assert!(model_matches_text_model_rule(Some(&store), "DeepSeek V4"));
        assert!(model_matches_text_model_rule(Some(&store), "GLM-5.2"));
        assert!(model_matches_text_model_rule(Some(&store), "glm 5.2-air"));
        assert!(model_matches_text_model_rule(Some(&store), "GLM-4.5-Air"));
        assert!(model_matches_text_model_rule(Some(&store), "glm 4.5 air"));
        assert!(!model_matches_text_model_rule(Some(&store), "GLM-4.5V"));
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
    fn builtin_text_model_rule_catalog_is_human_readable_and_complete() {
        let catalog = builtin_text_model_rule_catalog();
        assert_eq!(catalog.len(), BUILTIN_TEXT_MODEL_RULES.len());
        assert!(catalog.iter().all(|rule| {
            !rule.id.is_empty() && !rule.display_name.is_empty() && !rule.examples.is_empty()
        }));
        assert!(catalog.iter().any(|rule| rule.display_name == "GLM 5.2"));
        assert!(
            catalog
                .iter()
                .any(|rule| rule.display_name == "GLM 4.5 Air")
        );
        assert!(
            catalog
                .iter()
                .any(|rule| rule.display_name == "DeepSeek V4")
        );
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
        let resolved = resolve_vision_execution_config(Some(&store), "GLM-4.5-Air", true)
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
            Some(HttpModelBridgeProtocol::ChatCompletions)
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
