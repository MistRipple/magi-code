#[cfg(test)]
use crate::llm_types::{LlmMessage, LlmMessageContent};
use crate::llm_types::{ToolChoice, canonicalize_json};
use crate::prompt_ir::PromptIr;
use crate::prompt_ir::compile_prompt_ir;
#[cfg(test)]
use crate::prompt_ir::normalize_chat_tool_call_history;
#[cfg(test)]
use crate::protocol::AdaptedResponse;
use crate::protocol::streaming::{
    SseLineParser, StreamAccumulator, parse_stream_event, parse_stream_provider_context,
};
use crate::protocol::{
    AdaptedRequest, AnthropicMessagesAdapter, OpenAiChatCompletionsAdapter, OpenAiResponsesAdapter,
    ProviderAdapter, ProviderFamily, ProviderToolNameCodec,
};
#[cfg(test)]
use crate::types::ChatMessageOrigin;
use crate::types::{
    BridgeClientError, BridgeErrorLayer, ModelBridgeClient, ModelInvocationRequest, ModelResponse,
    ModelRetryRuntimeEvent, ModelRetryRuntimePhase, ModelStreamingDelta,
    model_invocation_cancelled_error, model_invocation_error_is_cancelled,
};
use crate::{ModelInvocationOptions, ModelTransportEvent, ModelTransportObserver};
use magi_app_server_protocol::{ModelTransportOutcome, ModelTransportUsage};
use magi_usage_authority::ReasoningEffort;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

const OPENAI_BASE_URL_ENV: &str = "MAGI_OPENAI_COMPAT_BASE_URL";
const OPENAI_API_KEY_ENV: &str = "MAGI_OPENAI_COMPAT_API_KEY";
const OPENAI_MODEL_ENV: &str = "MAGI_OPENAI_COMPAT_MODEL";
const OPENAI_STREAM_IDLE_TIMEOUT_MS_ENV: &str = "MAGI_OPENAI_COMPAT_STREAM_IDLE_TIMEOUT_MS";
const MODEL_PROVIDER_MAX_IN_FLIGHT: usize = 16;
/// 暂态故障（5xx、限流、断连、网关抖动）的重试是整个模型调用链里**唯一**的一层：
/// 退避窗口约 1.5 分钟，足以覆盖网关重启、上游短暂过载；更上层不再重复重跑同一请求。
const MODEL_PROVIDER_MAX_RETRIES: usize = 8;
const MODEL_PROVIDER_EMPTY_STREAM_RETRIES: usize = 2;
/// 等待响应超时（整段 300s 无响应）本身已经很久，再重试多次只会把一轮拖成几十分钟。
const MODEL_PROVIDER_TIMEOUT_RETRIES: usize = 2;
const MODEL_PROVIDER_RETRY_DELAYS_MILLIS: [u64; MODEL_PROVIDER_MAX_RETRIES] =
    [500, 1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 30_000];
const MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS: [u64; MODEL_PROVIDER_EMPTY_STREAM_RETRIES] =
    [1_000, 3_000];
const MODEL_PROVIDER_MAX_RETRY_AFTER_DELAY: Duration = Duration::from_secs(60);
const MODEL_PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const MODEL_PROVIDER_MIN_STREAM_IDLE_TIMEOUT: Duration = Duration::from_millis(100);
const MODEL_PROVIDER_MAX_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(3_600);
// 上下文压缩使用非流式辅助模型请求；30 秒不足以覆盖真实 Provider 的长历史摘要，
// 但取消仍由 execute_cancellable_http_post 的 cancellation channel 及时打断。
const MODEL_PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MODEL_CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MODEL_PROVIDER_TERMINAL_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

/// 同一上游连接对强制工具选择的真实协议能力。
///
/// 这不是模型型号白名单：能力由实际响应学习，并以协议、规范化端点和模型组成的
/// 连接指纹隔离。API Key 不参与指纹，也不会被记录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForcedToolChoiceCapability {
    Unknown,
    /// 提供方接受指定函数的强制选择。
    Exact,
    /// 提供方拒绝指定函数，但接受“本轮必须调用某个工具”。
    /// 当前 required-tool round 只暴露一个工具，因此该语义仍严格等价于指定函数。
    Required,
    /// 提供方没有可用的强制工具选择语义，只能由模型自主选择。
    AutoOnly,
}

static FORCED_TOOL_CHOICE_CAPABILITIES: OnceLock<
    Mutex<HashMap<String, ForcedToolChoiceCapability>>,
> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpModelBridgeProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderCacheRouteKey {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderCacheControl {
    None,
    AnthropicEphemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderContinuation {
    None,
    PreviousResponseId,
    Websocket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderCacheContract {
    pub route_key: ProviderCacheRouteKey,
    pub cache_control: ProviderCacheControl,
    pub continuation: ProviderContinuation,
}

/// 通过 HTTP 调用已配置的模型提供方。
///
/// HTTP I/O 放在独立线程执行，避免与调用方已经存在的 tokio runtime 冲突。
pub struct HttpModelBridgeClient {
    base_url: String,
    api_key: Option<String>,
    model: String,
    protocol: HttpModelBridgeProtocol,
    /// 推理强度配置：构造期注入；调用时透传到协议层 request body。
    /// `None` 表示未配置（保留协议默认行为）。
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_capability: Option<crate::reasoning::ReasoningCapability>,
    temperature: Option<f64>,
    stop: Vec<String>,
}

#[derive(Clone)]
struct HttpModelRequest {
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    tool_name_codec: ProviderToolNameCodec,
}

fn transport_started(
    body: &Value,
    observer: &ModelTransportObserver<'_>,
) -> Result<(), BridgeClientError> {
    observer(&ModelTransportEvent::Started {
        request_fingerprint: magi_core::hex_lower(Sha256::digest(body.to_string().as_bytes())),
    })
}

fn transport_finished<T>(
    result: &Result<T, BridgeClientError>,
    observer: &ModelTransportObserver<'_>,
) -> Result<(), BridgeClientError> {
    observer(&ModelTransportEvent::Finished {
        outcome: match result {
            Ok(_) => ModelTransportOutcome::Succeeded,
            Err(error) if model_invocation_error_is_cancelled(error) => {
                ModelTransportOutcome::Cancelled
            }
            Err(_) => ModelTransportOutcome::Failed,
        },
    })
}

fn transport_usage(usage: &crate::llm_types::LlmUsage) -> ModelTransportUsage {
    ModelTransportUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        cache_read_included_in_input: usage.cache_read_included_in_input,
    }
}

fn model_response_transport_usage(
    response: &crate::types::ModelResponse,
) -> Option<ModelTransportUsage> {
    let usage = response.usage.as_ref()?.clone();
    let usage = serde_json::from_value::<crate::llm_types::LlmUsage>(usage).ok()?;
    Some(transport_usage(&usage))
}

fn has_reported_token_counts(usage: &Value) -> bool {
    [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]
    .iter()
    .any(|field| usage.get(*field).and_then(Value::as_u64).is_some())
}

fn observed_response_usage(family: ProviderFamily, body: &str) -> Option<ModelTransportUsage> {
    // 缺少 usage 不是零用量；只有提供方明确返回时才规范化并记录。
    let raw: Value = serde_json::from_str(body).ok()?;
    raw.get("usage")
        .filter(|usage| has_reported_token_counts(usage))?;
    let response = match family {
        ProviderFamily::OpenAiChat => OpenAiChatCompletionsAdapter.parse_response(200, body),
        ProviderFamily::OpenAiResponses => OpenAiResponsesAdapter.parse_response(200, body),
        ProviderFamily::Anthropic => AnthropicMessagesAdapter.parse_response(200, body),
    }
    .ok()?;
    Some(transport_usage(&response.usage))
}

#[cfg(test)]
pub type ModelProbeRequest = (String, serde_json::Value, Vec<(String, String)>);

impl HttpModelBridgeClient {
    /// Create from environment variables.
    ///
    /// Returns `None` if `MAGI_OPENAI_COMPAT_BASE_URL` is not set.
    pub fn from_env() -> Option<Self> {
        let base_url = read_non_empty_env(OPENAI_BASE_URL_ENV)?;
        let api_key = read_non_empty_env(OPENAI_API_KEY_ENV);
        let model = read_non_empty_env(OPENAI_MODEL_ENV).unwrap_or_else(|| "gpt-4".to_string());
        Some(Self::new(base_url, api_key, model))
    }

    /// Create with explicit configuration (Chat Completions protocol, no reasoning effort).
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Self {
        Self::new_with_protocol(
            base_url,
            api_key,
            model,
            HttpModelBridgeProtocol::ChatCompletions,
            None,
        )
    }

    /// Create with explicit protocol and reasoning effort.
    ///
    /// `reasoning_effort = Some(_)` 时交给协议适配器按模型能力表装配：
    /// - 支持 OpenAI reasoning 的模型写入顶层 `reasoning_effort` 字段；
    /// - 支持 Anthropic thinking 的模型映射为对应 thinking 形态；
    /// - 未知或不支持的模型不注入可选推理字段。
    pub fn new_with_protocol(
        base_url: String,
        api_key: Option<String>,
        model: String,
        protocol: HttpModelBridgeProtocol,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Self {
        Self {
            base_url,
            api_key,
            model,
            protocol,
            reasoning_effort,
            reasoning_capability: None,
            temperature: None,
            stop: Vec::new(),
        }
    }

    pub fn with_reasoning_capability(
        mut self,
        capability: crate::reasoning::ReasoningCapability,
    ) -> Self {
        self.reasoning_effort = capability.reconcile(self.reasoning_effort);
        self.reasoning_capability = Some(capability);
        self
    }

    /// 显式调用参数覆盖连接默认值；无法表达的协议控制必须拒绝，不能静默忽略。
    pub fn with_generation_controls(
        mut self,
        temperature: Option<f64>,
        stop: Vec<String>,
        effort: Option<ReasoningEffort>,
    ) -> Result<Self, String> {
        if temperature.is_some_and(|v| !v.is_finite() || !(0.0..=2.0).contains(&v))
            || stop.len() > 16
            || stop.iter().any(|v| v.is_empty() || v.len() > 4096)
        {
            return Err("InvalidParams: 生成参数超出有效范围".into());
        }
        if let Some(effort) = effort {
            let capability = self
                .reasoning_capability
                .as_ref()
                .ok_or("模型未声明推理强度能力")?;
            if !capability.supported_efforts.contains(&effort) {
                return Err("模型不支持所选推理强度".into());
            }
            self.reasoning_effort = Some(effort);
        }
        self.temperature = temperature;
        self.stop = stop;
        Ok(self)
    }

    #[cfg(test)]
    fn chat_completions_url(&self) -> Result<String, BridgeClientError> {
        build_protocol_endpoint_url(&self.base_url, "/v1/chat/completions").map_err(|reason| {
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("invalid base_url: {reason}"),
            }
        })
    }

    fn provider_family(&self) -> ProviderFamily {
        match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => ProviderFamily::OpenAiChat,
            HttpModelBridgeProtocol::Responses => ProviderFamily::OpenAiResponses,
            HttpModelBridgeProtocol::AnthropicMessages => ProviderFamily::Anthropic,
        }
    }

    /// Provider 缓存能力的唯一协议合同；未知能力保持保守，不猜测兼容代理行为。
    pub fn cache_contract(&self) -> ProviderCacheContract {
        match self.protocol {
            HttpModelBridgeProtocol::Responses => ProviderCacheContract {
                route_key: ProviderCacheRouteKey::Supported,
                cache_control: ProviderCacheControl::None,
                continuation: ProviderContinuation::None,
            },
            HttpModelBridgeProtocol::ChatCompletions => {
                let direct_openai = reqwest::Url::parse(&self.base_url)
                    .ok()
                    .is_some_and(|url| url.host_str() == Some("api.openai.com"));
                ProviderCacheContract {
                    route_key: if direct_openai {
                        ProviderCacheRouteKey::Supported
                    } else {
                        ProviderCacheRouteKey::Unknown
                    },
                    cache_control: ProviderCacheControl::None,
                    continuation: ProviderContinuation::None,
                }
            }
            HttpModelBridgeProtocol::AnthropicMessages => ProviderCacheContract {
                route_key: ProviderCacheRouteKey::Unsupported,
                cache_control: ProviderCacheControl::AnthropicEphemeral,
                continuation: ProviderContinuation::None,
            },
        }
    }

    fn request_headers(&self) -> Vec<(String, String)> {
        match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions | HttpModelBridgeProtocol::Responses => self
                .api_key
                .as_ref()
                .map(|key| vec![("Authorization".to_string(), format!("Bearer {key}"))])
                .unwrap_or_default(),
            HttpModelBridgeProtocol::AnthropicMessages => self
                .api_key
                .as_ref()
                .map(|key| vec![("x-api-key".to_string(), key.clone())])
                .unwrap_or_default(),
        }
    }

    #[cfg(test)]
    fn build_http_request(
        &self,
        request: &ModelInvocationRequest,
        stream: bool,
    ) -> Result<HttpModelRequest, BridgeClientError> {
        self.build_http_request_with_output_limit(request, stream, None)
    }

    fn build_http_request_with_output_limit(
        &self,
        request: &ModelInvocationRequest,
        stream: bool,
        max_output_tokens: Option<u32>,
    ) -> Result<HttpModelRequest, BridgeClientError> {
        let mut headers = self.request_headers();
        // 多账号网关按会话把请求固定到同一个上游账号（各账号的提示词缓存互不共享）。
        // 头部用稳定 branch 摘要；Provider 缓存键随 epoch 断代不应重新分配上游账号。
        if let Some(key) = request
            .cache_affinity_key
            .as_deref()
            .filter(|key| !key.is_empty())
        {
            headers.push(("X-Session-Id".to_string(), key.to_string()));
        }
        let (params, tool_name_codec) = self.prepare_prompt_ir(request, stream, max_output_tokens);
        let mut adapted = self.adapt_request(&params)?;
        let url =
            build_protocol_endpoint_url(&self.base_url, &adapted.url_path).map_err(|reason| {
                BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("invalid base_url: {reason}"),
                }
            })?;
        crate::request_fingerprint::record_request_fingerprint(
            &self.model,
            &adapted.url_path,
            &adapted.body,
            params.volatile_tail_messages(),
        );
        headers.append(&mut adapted.extra_headers);
        Ok(HttpModelRequest {
            url,
            body: adapted.body,
            headers,
            tool_name_codec,
        })
    }

    /// 真实发送与缓存摘要共用的 Prompt IR 预处理。
    ///
    /// 这里是唯一同时处理 provider 能力降级和工具 wire name 编码的入口。
    /// `cache_prompt_ir` 调用同一函数，避免缓存身份仍基于 canonical 工具名而
    /// 实际请求使用编码后的名称。
    fn prepare_prompt_ir(
        &self,
        request: &ModelInvocationRequest,
        stream: bool,
        max_output_tokens: Option<u32>,
    ) -> (PromptIr, ProviderToolNameCodec) {
        let mut params =
            compile_prompt_ir(request, stream, self.reasoning_effort, max_output_tokens);
        params.reasoning_capability = self.reasoning_capability.clone();
        params.temperature = self.temperature;
        params.stop = self.stop.clone();
        // Chat Completions 兼容服务不保证接受 OpenAI 缓存扩展；直接 OpenAI
        // 连接才自动发送。Responses 沿其标准字段，Anthropic 由断点负责。
        if matches!(
            self.cache_contract().route_key,
            ProviderCacheRouteKey::Unknown | ProviderCacheRouteKey::Unsupported
        ) && matches!(self.protocol, HttpModelBridgeProtocol::ChatCompletions)
        {
            params.prompt_cache_key = None;
        }
        if matches!(params.tool_choice, Some(ToolChoice::Typed { .. })) {
            match self.forced_tool_choice_capability() {
                ForcedToolChoiceCapability::Required => {
                    params.tool_choice = Some(ToolChoice::Simple("required".to_string()));
                }
                ForcedToolChoiceCapability::AutoOnly => {
                    params.tool_choice = Some(ToolChoice::Typed {
                        kind: "auto".to_string(),
                        name: None,
                    });
                }
                ForcedToolChoiceCapability::Unknown | ForcedToolChoiceCapability::Exact => {}
            }
        }
        let tool_name_codec = ProviderToolNameCodec::for_params(&params);
        tool_name_codec.encode_request_params(&mut params);
        (params, tool_name_codec)
    }

    fn adapt_request(&self, params: &PromptIr) -> Result<AdaptedRequest, BridgeClientError> {
        match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => OpenAiChatCompletionsAdapter
                .build_request(params, &self.model)
                .map_err(|reason| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("build openai chat request failed: {reason}"),
                }),
            HttpModelBridgeProtocol::Responses => OpenAiResponsesAdapter
                .build_request(params, &self.model)
                .map_err(|reason| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("build openai responses request failed: {reason}"),
                }),
            HttpModelBridgeProtocol::AnthropicMessages => AnthropicMessagesAdapter
                .build_request(params, &self.model)
                .map_err(|reason| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("build anthropic request failed: {reason}"),
                }),
        }
    }

    #[cfg(test)]
    fn build_request_body(&self, request: &ModelInvocationRequest) -> serde_json::Value {
        self.build_http_request(request, false)
            .expect("model request should be buildable in tests")
            .body
    }

    /// 构造一次「最小可用」非流式请求（仅用于请求体装配单元测试）。
    ///
    /// 生产连接测试必须走 [`ModelBridgeClient::invoke_streaming`]，保证与真实会话
    /// 的流式协议一致；这里不能再作为设置页可用性探针入口。
    #[cfg(test)]
    pub fn build_probe_request(&self) -> Result<ModelProbeRequest, BridgeClientError> {
        let request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "probe".to_string(),
            prompt: "ping".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        };
        let http_request = self.build_http_request(&request, false)?;
        Ok((http_request.url, http_request.body, http_request.headers))
    }

    fn provider_request_key(&self) -> String {
        format!(
            "{protocol}|{base_url}|{model}",
            protocol = self.protocol.label(),
            base_url = normalize_provider_base_url(&self.base_url),
            model = self.model.trim(),
        )
    }

    fn forced_tool_choice_capability(&self) -> ForcedToolChoiceCapability {
        forced_tool_choice_capability(&self.provider_request_key())
    }

    fn mark_forced_tool_choice_capability(&self, capability: ForcedToolChoiceCapability) {
        record_forced_tool_choice_capability(self.provider_request_key(), capability);
    }

    fn invoke_streaming_observed(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
        max_output_tokens: Option<u32>,
        observer: &ModelTransportObserver<'_>,
    ) -> Result<ModelResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }

        let http_request =
            self.build_http_request_with_output_limit(&request, true, max_output_tokens)?;
        let provider_family = self.provider_family();
        let mut current_request = http_request;
        let mut fallback_capability = None;
        loop {
            let enforced = is_forced_tool_choice_request(&current_request.body, provider_family);
            let buffered_deltas = std::cell::RefCell::new(Vec::new());
            let result = execute_streaming_http_post_with_retries(
                self.provider_request_key(),
                current_request.clone(),
                provider_family,
                current_request.tool_name_codec.clone(),
                &|delta| {
                    if enforced {
                        buffered_deltas.borrow_mut().push(delta.clone());
                    } else {
                        on_delta(delta);
                    }
                },
                TransportRetryControls {
                    on_retry,
                    is_cancelled,
                    observer,
                },
            );

            match result {
                Err(error)
                    if is_forced_tool_choice_rejection_error(
                        &error,
                        &current_request.body,
                        provider_family,
                    ) =>
                {
                    let Some((capability, next_request)) =
                        request_with_next_tool_choice_enforcement(
                            &current_request,
                            provider_family,
                        )
                    else {
                        return Err(error);
                    };
                    on_retry(&ModelRetryRuntimeEvent {
                        phase: ModelRetryRuntimePhase::AttemptStarted,
                        attempt: 1,
                        max_attempts: 2,
                        delay_ms: None,
                    });
                    fallback_capability = Some(capability);
                    current_request = next_request;
                }
                Ok(response) => {
                    for delta in buffered_deltas.into_inner() {
                        on_delta(&delta);
                    }
                    if let Some(capability) = fallback_capability.or_else(|| {
                        capability_from_accepted_tool_choice_request(
                            &current_request,
                            provider_family,
                        )
                    }) {
                        self.mark_forced_tool_choice_capability(capability);
                    }
                    return Ok(response);
                }
                Err(error) => {
                    for delta in buffered_deltas.into_inner() {
                        on_delta(&delta);
                    }
                    return Err(error);
                }
            }
        }
    }
}

impl HttpModelBridgeProtocol {
    fn label(self) -> &'static str {
        match self {
            HttpModelBridgeProtocol::ChatCompletions => "chat-completions",
            HttpModelBridgeProtocol::Responses => "responses",
            HttpModelBridgeProtocol::AnthropicMessages => "anthropic-messages",
        }
    }
}

struct ModelProviderGate {
    slots: Mutex<HashMap<String, Arc<ModelProviderSlot>>>,
}

struct ModelProviderSlot {
    in_flight: Mutex<usize>,
    available: Condvar,
}

struct ModelProviderPermit {
    slot: Arc<ModelProviderSlot>,
}

impl ModelProviderGate {
    #[cfg(test)]
    fn acquire(&self, key: &str) -> ModelProviderPermit {
        let cancellation = AtomicBool::new(false);
        self.acquire_cancellable(key, &cancellation)
            .expect("non-cancellable provider permit acquisition cannot be cancelled")
    }

    fn acquire_cancellable(
        &self,
        key: &str,
        cancellation: &AtomicBool,
    ) -> Option<ModelProviderPermit> {
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slots
                .entry(key.to_string())
                .or_insert_with(|| {
                    Arc::new(ModelProviderSlot {
                        in_flight: Mutex::new(0),
                        available: Condvar::new(),
                    })
                })
                .clone()
        };

        let mut in_flight = slot
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *in_flight >= MODEL_PROVIDER_MAX_IN_FLIGHT {
            if cancellation.load(Ordering::SeqCst) {
                return None;
            }
            let (guard, _) = slot
                .available
                .wait_timeout(in_flight, MODEL_CANCELLATION_POLL_INTERVAL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            in_flight = guard;
        }
        if cancellation.load(Ordering::SeqCst) {
            return None;
        }
        *in_flight += 1;
        drop(in_flight);

        Some(ModelProviderPermit { slot })
    }
}

impl Drop for ModelProviderPermit {
    fn drop(&mut self) {
        let mut in_flight = self
            .slot
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *in_flight = in_flight.saturating_sub(1);
        self.slot.available.notify_one();
    }
}

fn model_provider_gate() -> &'static ModelProviderGate {
    static GATE: OnceLock<ModelProviderGate> = OnceLock::new();
    GATE.get_or_init(|| ModelProviderGate {
        slots: Mutex::new(HashMap::new()),
    })
}

fn shared_streaming_http_client() -> Result<reqwest::Client, BridgeClientError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // 对端静默消失（网关崩溃、网络切换）时，等响应头最长要 300s；TCP 保活让操作系统
            // 更早发现连接已死，错误交给重试层处理，而不是让一轮白等 5 分钟。
            .tcp_keepalive(Duration::from_secs(30))
            // 流式请求不设置总 timeout；响应体的 idle timeout 由 streaming_http_io
            // 按 chunk 单独控制，避免长回复被误判为超时。
            .build()
            .map_err(|error| error.to_string())
    }) {
        Ok(client) => Ok(client.clone()),
        Err(message) => Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("HTTP streaming client build failed: {message}"),
        }),
    }
}

fn shared_http_runtime() -> Result<&'static tokio::runtime::Runtime, BridgeClientError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .map_err(|error| error.to_string())
    }) {
        Ok(runtime) => Ok(runtime),
        Err(message) => Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("HTTP runtime build failed: {message}"),
        }),
    }
}

fn shared_request_http_client() -> Result<reqwest::Client, BridgeClientError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(MODEL_PROVIDER_REQUEST_TIMEOUT)
            .build()
            .map_err(|error| error.to_string())
    }) {
        Ok(client) => Ok(client.clone()),
        Err(message) => Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("HTTP client build failed: {message}"),
        }),
    }
}

fn normalize_provider_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

fn forced_tool_choice_capability(provider_key: &str) -> ForcedToolChoiceCapability {
    FORCED_TOOL_CHOICE_CAPABILITIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("forced tool choice capability registry lock poisoned")
        .get(provider_key)
        .copied()
        .unwrap_or(ForcedToolChoiceCapability::Unknown)
}

fn record_forced_tool_choice_capability(
    provider_key: String,
    capability: ForcedToolChoiceCapability,
) {
    FORCED_TOOL_CHOICE_CAPABILITIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("forced tool choice capability registry lock poisoned")
        .insert(provider_key, capability);
}

/// 退避表中第 `attempt` 次重试的基础等待时间。
fn base_retry_delay(attempt: usize) -> Duration {
    let index = attempt
        .saturating_sub(1)
        .min(MODEL_PROVIDER_RETRY_DELAYS_MILLIS.len().saturating_sub(1));
    Duration::from_millis(MODEL_PROVIDER_RETRY_DELAYS_MILLIS[index])
}

/// 服务商重试等待的唯一计算（流式与非流式共用）：优先遵守服务商给出的 Retry-After；
/// 否则按退避表并加入 ±25% 随机抖动，避免多个子代理在同一时刻集中重试。
fn provider_retry_delay(attempt: usize, retry_after: Option<Duration>) -> Duration {
    if let Some(retry_after) = retry_after {
        return retry_after;
    }
    let base = base_retry_delay(attempt).as_millis() as u64;
    let spread = base / 2;
    let jitter = if spread == 0 {
        0
    } else {
        retry_jitter_seed() % (spread + 1)
    };
    Duration::from_millis(base - base / 4 + jitter)
}

fn retry_jitter_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(SEQUENCE.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

fn empty_stream_retry_delay(attempt: usize) -> Duration {
    let index = attempt.saturating_sub(1).min(
        MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS
            .len()
            .saturating_sub(1),
    );
    Duration::from_millis(MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS[index])
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let delay = value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(now)
                .ok()
        })?;
    Some(delay.min(MODEL_PROVIDER_MAX_RETRY_AFTER_DELAY))
}

fn retryable_http_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 529) || status >= 500
}

/// 额度耗尽、欠费这类 429 不会因为等待而恢复，重试只会白白消耗时间。
fn is_permanent_quota_exhaustion(status: u16, detail: &str) -> bool {
    if status != 429 {
        return false;
    }
    let detail = detail.to_ascii_lowercase();
    detail.contains("insufficient_quota")
        || detail.contains("exceeded your current quota")
        || detail.contains("billing")
        || detail.contains("credit balance")
        || detail.contains("out of credits")
}

/// 带响应内容的状态码重试判定：在状态码规则之上排除永久性的额度耗尽。
fn retryable_http_response(status: u16, body: &str) -> bool {
    retryable_http_status(status) && !is_permanent_quota_exhaustion(status, body)
}

/// 暂态故障的文本特征：服务商用 200 + 错误体、或仅在消息里说明「过载 / 暂不可用」，
/// 没有可依据的状态码时靠它识别。
fn is_transient_provider_error_text(normalized: &str) -> bool {
    normalized.contains("overloaded")
        || normalized.contains("server is busy")
        || normalized.contains("servers are busy")
        || normalized.contains("service unavailable")
        || normalized.contains("temporarily unavailable")
        || normalized.contains("connection reset")
        || normalized.contains("connection aborted")
        || normalized.contains("connection closed")
        || normalized.contains("failed to connect")
        || normalized.contains("dns error")
}

fn retryable_bridge_error(error: &BridgeClientError) -> bool {
    if model_invocation_error_is_cancelled(error) || error.code() == Some(-32096) {
        return false;
    }
    match error {
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            ..
        } => true,
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            message,
            ..
        } => is_transient_provider_error_text(&message.to_ascii_lowercase()),
        BridgeClientError::HttpStatusFailed {
            http_status,
            message,
            ..
        } => {
            retryable_http_status(*http_status)
                && !is_permanent_quota_exhaustion(*http_status, message)
        }
        _ => false,
    }
}

fn is_timeout_error(error: &BridgeClientError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

/// 连接成功但上游没有返回任何内容（空流）：只给少量重试机会。
/// 返回 HTML 等非事件流内容通常是地址配错或网关登录页，重试没有意义，不在此列。
fn retryable_empty_stream_error(error: &BridgeClientError) -> bool {
    matches!(
        error,
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message,
        } if message.contains("empty stream response")
    )
}

fn provider_stream_idle_timeout_from_env(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .map(|value| {
            value.clamp(
                MODEL_PROVIDER_MIN_STREAM_IDLE_TIMEOUT,
                MODEL_PROVIDER_MAX_STREAM_IDLE_TIMEOUT,
            )
        })
        .unwrap_or(MODEL_PROVIDER_STREAM_IDLE_TIMEOUT)
}

fn provider_stream_idle_timeout() -> Duration {
    provider_stream_idle_timeout_from_env(
        env::var(OPENAI_STREAM_IDLE_TIMEOUT_MS_ENV).ok().as_deref(),
    )
}

fn sleep_retry_delay_cancellable(delay: Duration, is_cancelled: &dyn Fn() -> bool) -> bool {
    #[cfg(test)]
    {
        let _ = delay;
        !is_cancelled()
    }

    #[cfg(not(test))]
    {
        let deadline = std::time::Instant::now() + delay;
        loop {
            if is_cancelled() {
                return false;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return true;
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(MODEL_CANCELLATION_POLL_INTERVAL),
            );
        }
    }
}

type HttpPostResult = Result<(u16, String, Option<Duration>), BridgeClientError>;
enum HttpPostMessage {
    Headers(u16),
    Done(HttpPostResult),
}
type StreamingHttpResult = Result<ModelResponse, BridgeClientError>;
type ToolChoiceFallbackHttpResult = Result<
    (
        HttpModelRequest,
        u16,
        String,
        Option<Duration>,
        Option<ForcedToolChoiceCapability>,
    ),
    BridgeClientError,
>;

/// 一次 transport retry loop 共用的控制回调。把取消、重试生命周期和持久化
/// 观测作为同一组依赖传递，避免不同协议路径继续扩张参数列表或遗漏其中一项。
struct TransportRetryControls<'a> {
    on_retry: &'a dyn Fn(&ModelRetryRuntimeEvent),
    is_cancelled: &'a dyn Fn() -> bool,
    observer: &'a ModelTransportObserver<'a>,
}

fn receive_cancellable_http_result(
    rx: mpsc::Receiver<HttpPostMessage>,
    cancellation_tx: tokio::sync::oneshot::Sender<()>,
    cancellation: &AtomicBool,
    is_cancelled: &dyn Fn() -> bool,
    disconnected_message: &'static str,
    observer: &ModelTransportObserver<'_>,
) -> HttpPostResult {
    let mut cancellation_tx = Some(cancellation_tx);
    let mut cancellation_requested = false;
    loop {
        if !cancellation_requested && is_cancelled() {
            cancellation.store(true, Ordering::SeqCst);
            if let Some(tx) = cancellation_tx.take() {
                let _ = tx.send(());
            }
            cancellation_requested = true;
        }
        match rx.recv_timeout(MODEL_CANCELLATION_POLL_INTERVAL) {
            Ok(HttpPostMessage::Headers(status)) => observer(&ModelTransportEvent::Updated {
                http_status: Some(status),
                usage: None,
            })?,
            Ok(HttpPostMessage::Done(result)) => {
                return if cancellation_requested {
                    Err(model_invocation_cancelled_error())
                } else {
                    result
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32005),
                    message: disconnected_message.to_string(),
                });
            }
        }
    }
}

fn execute_cancellable_http_post(
    provider_key: String,
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    is_cancelled: &dyn Fn() -> bool,
    observer: &ModelTransportObserver<'_>,
) -> HttpPostResult {
    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancellation = cancellation.clone();
    let (cancellation_tx, cancellation_rx) = tokio::sync::oneshot::channel();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = match model_provider_gate()
            .acquire_cancellable(&provider_key, worker_cancellation.as_ref())
        {
            Some(_permit) => shared_http_runtime().and_then(|runtime| {
                runtime.block_on(async_http_post_io(url, body, headers, cancellation_rx, &tx))
            }),
            None => Err(model_invocation_cancelled_error()),
        };
        let _ = tx.send(HttpPostMessage::Done(result));
    });

    receive_cancellable_http_result(
        rx,
        cancellation_tx,
        cancellation.as_ref(),
        is_cancelled,
        "HTTP request thread terminated unexpectedly",
        observer,
    )
}

async fn async_http_post_io(
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    mut cancellation_rx: tokio::sync::oneshot::Receiver<()>,
    progress: &mpsc::Sender<HttpPostMessage>,
) -> HttpPostResult {
    let client = shared_request_http_client()?;
    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = tokio::select! {
        _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
        result = request.send() => result,
    }
    .map_err(|error| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: format!("provider transport failed: {error}"),
    })?;
    let status = response.status().as_u16();
    let _ = progress.send(HttpPostMessage::Headers(status));
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, SystemTime::now()));
    let response_body = tokio::select! {
        _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
        result = response.text() => result,
    }
    .map_err(|error| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: format!("reading response body failed: {error}"),
    })?;
    Ok((status, response_body, retry_after))
}

fn execute_cancellable_http_post_with_retries(
    provider_key: String,
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    provider_family: ProviderFamily,
    controls: TransportRetryControls<'_>,
) -> HttpPostResult {
    let mut retries = 0usize;
    loop {
        if (controls.is_cancelled)() {
            return Err(model_invocation_cancelled_error());
        }
        transport_started(&body, controls.observer)?;
        if (controls.is_cancelled)() {
            let result = Err(model_invocation_cancelled_error());
            transport_finished::<()>(&result, controls.observer)?;
            return Err(model_invocation_cancelled_error());
        }
        let result = execute_cancellable_http_post(
            provider_key.clone(),
            url.clone(),
            body.clone(),
            headers.clone(),
            controls.is_cancelled,
            controls.observer,
        );
        if let Ok((status, response_body, _)) = &result {
            let usage = observed_response_usage(provider_family, response_body);
            (controls.observer)(&ModelTransportEvent::Updated {
                http_status: Some(*status),
                usage,
            })?;
        }
        let outcome = match &result {
            Ok((status, body, _))
                if (200..300).contains(status) && provider_error_message(body).is_none() =>
            {
                ModelTransportOutcome::Succeeded
            }
            Err(error) if model_invocation_error_is_cancelled(error) => {
                ModelTransportOutcome::Cancelled
            }
            _ => ModelTransportOutcome::Failed,
        };
        (controls.observer)(&ModelTransportEvent::Finished { outcome })?;
        let delay = match &result {
            Ok((status, response_body, retry_after))
                if retryable_http_response(*status, response_body)
                    && retries < MODEL_PROVIDER_MAX_RETRIES =>
            {
                Some(provider_retry_delay(retries + 1, *retry_after))
            }
            Err(error) if retryable_bridge_error(error) && retries < MODEL_PROVIDER_MAX_RETRIES => {
                Some(provider_retry_delay(retries + 1, None))
            }
            _ => None,
        };
        let Some(delay) = delay else {
            if retries > 0 {
                (controls.on_retry)(&ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::Settled,
                    attempt: retries,
                    max_attempts: MODEL_PROVIDER_MAX_RETRIES,
                    delay_ms: None,
                });
            }
            return result;
        };
        retries += 1;
        (controls.on_retry)(&ModelRetryRuntimeEvent {
            phase: ModelRetryRuntimePhase::Scheduled,
            attempt: retries,
            max_attempts: MODEL_PROVIDER_MAX_RETRIES,
            delay_ms: Some(delay.as_millis() as u64),
        });
        if !sleep_retry_delay_cancellable(delay, controls.is_cancelled) {
            return Err(model_invocation_cancelled_error());
        }
        (controls.on_retry)(&ModelRetryRuntimeEvent {
            phase: ModelRetryRuntimePhase::AttemptStarted,
            attempt: retries,
            max_attempts: MODEL_PROVIDER_MAX_RETRIES,
            delay_ms: None,
        });
    }
}

/// 流式 HTTP 线程发回的消息类型。
enum StreamMessage {
    Observation(ModelTransportEvent),
    /// LLM 增量快照——携带已累积的正文与上游 thinking。
    Chunk(ModelStreamingDelta),
    /// HTTP I/O 结束——携带最终结果。
    Done(StreamingHttpResult),
}

fn provider_stream_event_error(
    provider_family: ProviderFamily,
    event: &crate::protocol::streaming::SseEvent,
) -> Option<BridgeClientError> {
    let payload = serde_json::from_str::<Value>(&event.data).ok()?;
    // OpenAI Responses 兼容网关可能省略 SSE `event:` 行，仅在 data JSON
    // 的 `type` 字段中携带事件名。错误事件也必须使用同一事实源，否则
    // response.failed 会被误当成普通数据继续解析。
    let response_event_type = event
        .event_type
        .as_deref()
        .or_else(|| payload["type"].as_str());
    let is_error_event = match provider_family {
        ProviderFamily::OpenAiChat => true,
        ProviderFamily::OpenAiResponses => {
            matches!(response_event_type, Some("error" | "response.failed"))
        }
        ProviderFamily::Anthropic => event.event_type.as_deref() == Some("error"),
    };
    if !is_error_event {
        return None;
    }
    let error = payload
        .get("error")
        .or_else(|| {
            payload
                .get("response")
                .and_then(|response| response.get("error"))
        })
        .or_else(|| (provider_family == ProviderFamily::OpenAiResponses).then_some(&payload))?;
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("provider stream returned an error");
    let normalized_message = message.to_ascii_lowercase();
    let transient = matches!(
        error_type,
        "overloaded_error"
            | "rate_limit_error"
            | "server_error"
            | "timeout_error"
            | "service_unavailable"
    ) || normalized_message.contains("overloaded")
        || normalized_message.contains("server is busy")
        || normalized_message.contains("servers are busy")
        || normalized_message.contains("service unavailable")
        || normalized_message.contains("temporarily unavailable");
    Some(BridgeClientError::CallFailed {
        layer: if transient {
            BridgeErrorLayer::Transport
        } else {
            BridgeErrorLayer::RemoteBusiness
        },
        code: Some(if transient { -32005 } else { -32006 }),
        message: format!("provider stream rejected request: {message}"),
    })
}

/// Receive timestamps belong to HTTP reads, never to individual SSE parsing steps.
#[derive(Default)]
struct BodyStreamTiming {
    first: Option<Duration>,
    last: Option<Duration>,
    response_received_at: Duration,
    usage_output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    non_text_output: bool,
    usage_observed: bool,
}

#[derive(Default)]
struct StreamDeltaProgress {
    content_len: usize,
    thinking_len: usize,
    tool_call_count: usize,
}

impl BodyStreamTiming {
    fn observe(
        &mut self,
        event: &crate::protocol::streaming::SseEvent,
        has_content: bool,
        received_at: Duration,
    ) -> bool {
        self.response_received_at = received_at;
        if has_content {
            self.first.get_or_insert(received_at);
            self.last = Some(received_at);
        }
        if let Ok(raw) = serde_json::from_str::<Value>(&event.data) {
            // A redacted thinking block has no visible thinking text, but still consumes output tokens.
            self.non_text_output |= raw
                .get("content_block")
                .and_then(|block| block.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "text");
            self.non_text_output |= raw
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|kind| !matches!(kind, "message" | "reasoning"));
            self.non_text_output |=
                raw.get("choices")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| {
                        choices.iter().any(|choice| {
                            choice["index"].as_u64().is_some_and(|index| index > 0)
                                || choice["delta"]
                                    .get("audio")
                                    .is_some_and(|audio| !audio.is_null())
                        })
                    });
            self.non_text_output |= raw
                .get("part")
                .and_then(|part| part.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|kind| !matches!(kind, "output_text" | "refusal"));
            self.non_text_output |= raw.get("type").and_then(Value::as_str).is_some_and(|kind| {
                kind.starts_with("response.audio.") || kind.starts_with("response.output_audio.")
            });
            let usage = raw
                .get("usage")
                .or_else(|| raw.pointer("/response/usage"))
                .or_else(|| raw.pointer("/message/usage"));
            if let Some(usage) = usage.filter(|usage| has_reported_token_counts(usage)) {
                self.usage_observed = true;
                // Replace the pair together. Intermediate reasoning metadata cannot be paired with a later total.
                self.usage_output_tokens = usage
                    .get("completion_tokens")
                    .or_else(|| usage.get("output_tokens"))
                    .and_then(Value::as_u64);
                self.reasoning_tokens = usage
                    .pointer("/completion_tokens_details/reasoning_tokens")
                    .or_else(|| usage.pointer("/output_tokens_details/reasoning_tokens"))
                    .and_then(Value::as_u64);
                self.non_text_output |= usage
                    .pointer("/completion_tokens_details/audio_tokens")
                    .or_else(|| usage.pointer("/output_tokens_details/audio_tokens"))
                    .and_then(Value::as_u64)
                    .is_some_and(|tokens| tokens > 0);
                return true;
            }
        }
        false
    }

    fn finish(
        &self,
        family: ProviderFamily,
        output_tokens: u64,
        has_thinking: bool,
        has_tools: bool,
    ) -> Option<magi_app_server_protocol::ModelResponseTiming> {
        let first = self.first?;
        let text_output_tokens =
            if self.usage_output_tokens != Some(output_tokens) || has_tools || self.non_text_output
            {
                None
            } else if let Some(reasoning) = self.reasoning_tokens {
                output_tokens.checked_sub(reasoning)
            } else if family == ProviderFamily::Anthropic && !has_thinking {
                Some(output_tokens)
            } else {
                None
            };
        Some(magi_app_server_protocol::ModelResponseTiming {
            response_duration_ms: (self.usage_output_tokens == Some(output_tokens))
                .then_some(self.response_received_at.as_millis() as u64),
            first_content_ms: first.as_millis() as u64,
            content_duration_ms: self.last.unwrap_or(first).saturating_sub(first).as_millis()
                as u64,
            text_output_tokens,
        })
    }
}

fn apply_provider_stream_event(
    provider_family: ProviderFamily,
    event: &crate::protocol::streaming::SseEvent,
    accumulator: &mut StreamAccumulator,
    delta_progress: &mut StreamDeltaProgress,
    tx: &std::sync::mpsc::Sender<StreamMessage>,
    timing: &mut BodyStreamTiming,
    received_at: Duration,
) -> Result<bool, BridgeClientError> {
    if let Some(error) = provider_stream_event_error(provider_family, event) {
        return Err(error);
    }
    if event.data.trim() == "[DONE]"
        || (provider_family == ProviderFamily::Anthropic
            && event.event_type.as_deref() == Some("message_stop"))
    {
        return Ok(true);
    }

    let mut llm_chunks = parse_stream_event(provider_family, event);
    let has_usage = timing.observe(
        event,
        llm_chunks
            .iter()
            .any(|chunk| chunk.content.as_ref().is_some_and(|text| !text.is_empty())),
        received_at,
    );
    if !has_usage {
        llm_chunks.retain(|chunk| chunk.usage.is_none());
    }
    accumulator.apply_all(&llm_chunks);
    // Responses 兼容网关可能先发 usage snapshot，再在 response.completed
    // 给出最终结算。最终事件是该 attempt 的权威 usage，不能继续用 max 合并，
    // 否则隐藏推理/中间统计会把 ledger 与 completed response 的 output tokens
    // 分叉。
    if (provider_family == ProviderFamily::OpenAiChat
        || (provider_family == ProviderFamily::OpenAiResponses
            && is_responses_terminal_event(event)))
        && let Some(usage) = llm_chunks
            .iter()
            .rev()
            .find_map(|chunk| chunk.usage.clone())
    {
        accumulator.replace_usage(usage);
    }
    if llm_chunks.iter().any(|chunk| chunk.usage.is_some()) {
        let _ = tx.send(StreamMessage::Observation(ModelTransportEvent::Updated {
            http_status: None,
            usage: Some(transport_usage(&accumulator.usage_snapshot())),
        }));
    }
    if let Some(context) = parse_stream_provider_context(provider_family, event) {
        accumulator.apply_provider_context(context);
    }
    let accumulated_content = accumulator.accumulated_content();
    let accumulated_thinking = accumulator.accumulated_thinking();
    let accumulated_tool_calls = accumulator.accumulated_tool_calls();
    let tool_call_count = accumulated_tool_calls.len();
    if accumulated_content.len() > delta_progress.content_len
        || accumulated_thinking.len() > delta_progress.thinking_len
        || tool_call_count > delta_progress.tool_call_count
    {
        // 只发这一帧新增的部分：流式就是增量，不能每帧把到目前为止的全文再发一遍。
        let (mut content, content_rewritten) =
            new_suffix(&accumulated_content, delta_progress.content_len);
        let (mut thinking, thinking_rewritten) =
            new_suffix(&accumulated_thinking, delta_progress.thinking_len);
        let replace = content_rewritten || thinking_rewritten;
        if replace {
            // 改写帧两个字段都携带完整文字。
            content = accumulated_content.clone();
            thinking = accumulated_thinking.clone();
        }
        let new_tool_calls = accumulated_tool_calls
            .get(delta_progress.tool_call_count..)
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        delta_progress.content_len = accumulated_content.len();
        delta_progress.thinking_len = accumulated_thinking.len();
        delta_progress.tool_call_count = tool_call_count;
        if tx
            .send(StreamMessage::Chunk(ModelStreamingDelta {
                content,
                thinking,
                replace,
                tool_calls: new_tool_calls,
            }))
            .is_err()
        {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: "stream consumer disconnected".to_string(),
            });
        }
    }
    Ok(false)
}

fn is_responses_terminal_event(event: &crate::protocol::streaming::SseEvent) -> bool {
    let parsed_type = serde_json::from_str::<serde_json::Value>(&event.data)
        .ok()
        .and_then(|value| value["type"].as_str().map(str::to_string));
    let event_type = event.event_type.as_deref().or(parsed_type.as_deref());
    matches!(
        event_type,
        Some("response.completed" | "response.incomplete" | "response.failed")
    )
}

/// `full` 相对已发送长度 `sent` 新增的后缀；若 `sent` 不落在字符边界上（上游把已发内容改写了），
/// 退回整段并标记为改写。
fn new_suffix(full: &str, sent: usize) -> (String, bool) {
    match full.get(sent..) {
        Some(suffix) => (suffix.to_string(), false),
        None => (full.to_string(), true),
    }
}

fn provider_stream_idle_timeout_error(stage: &str, timeout: Duration) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: format!(
            "provider stream idle timeout after {} ms while waiting for {stage}",
            timeout.as_millis()
        ),
    }
}

/// 执行流式 HTTP POST，通过 SSE 逐块读取 LLM 响应。
///
/// HTTP I/O 在独立线程内运行异步 reqwest，请求发送、响应体读取和 provider
/// 并发槽等待都监听同一取消信号。会话或任务被中断后，连接会被主动释放，
/// 而不是只停止 UI 写回后继续占用上游连接。
fn execute_streaming_http_post(
    provider_key: String,
    request: HttpModelRequest,
    provider_family: ProviderFamily,
    tool_name_codec: ProviderToolNameCodec,
    on_chunk: &dyn Fn(&ModelStreamingDelta),
    is_cancelled: &dyn Fn() -> bool,
    observer: &ModelTransportObserver<'_>,
) -> StreamingHttpResult {
    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancellation = cancellation.clone();
    let (cancellation_tx, cancellation_rx) = tokio::sync::oneshot::channel();
    let (tx, rx) = mpsc::channel::<StreamMessage>();

    std::thread::spawn(move || {
        let HttpModelRequest {
            url, body, headers, ..
        } = request;
        let result = match model_provider_gate()
            .acquire_cancellable(&provider_key, worker_cancellation.as_ref())
        {
            Some(_permit) => shared_http_runtime().and_then(|runtime| {
                runtime.block_on(streaming_http_io(StreamingHttpIoRequest {
                    provider_key: provider_key.clone(),
                    url,
                    body,
                    headers,
                    provider_family,
                    tool_name_codec,
                    tx: tx.clone(),
                    cancellation_rx,
                }))
            }),
            None => Err(model_invocation_cancelled_error()),
        };
        let _ = tx.send(StreamMessage::Done(result));
    });

    let mut cancellation_tx = Some(cancellation_tx);
    let mut cancellation_requested = false;
    let mut emitted_delta = false;
    loop {
        if !cancellation_requested && is_cancelled() {
            cancellation.store(true, Ordering::SeqCst);
            if let Some(tx) = cancellation_tx.take() {
                let _ = tx.send(());
            }
            cancellation_requested = true;
        }
        match rx.recv_timeout(MODEL_CANCELLATION_POLL_INTERVAL) {
            Ok(StreamMessage::Chunk(mut delta)) => {
                let mut completed_result = None;
                while let Ok(message) = rx.try_recv() {
                    match message {
                        StreamMessage::Observation(event) => observer(&event)?,
                        StreamMessage::Chunk(next_delta) => delta.merge(next_delta),
                        StreamMessage::Done(result) => {
                            completed_result = Some(result);
                            break;
                        }
                    }
                }
                if !cancellation_requested {
                    emitted_delta = true;
                    on_chunk(&delta);
                }
                if let Some(result) = completed_result {
                    return if cancellation_requested {
                        Err(model_invocation_cancelled_error())
                    } else {
                        result
                    };
                }
            }
            Ok(StreamMessage::Done(result)) => {
                return if cancellation_requested {
                    Err(model_invocation_cancelled_error())
                } else {
                    result
                };
            }
            Ok(StreamMessage::Observation(event)) => observer(&event)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32005),
                    message: if emitted_delta {
                        "provider stream interrupted: streaming HTTP request thread terminated unexpectedly"
                            .to_string()
                    } else {
                        "streaming HTTP request thread terminated unexpectedly".to_string()
                    },
                });
            }
        }
    }
}

fn execute_streaming_http_post_with_retries(
    provider_key: String,
    request: HttpModelRequest,
    provider_family: ProviderFamily,
    tool_name_codec: ProviderToolNameCodec,
    on_chunk: &dyn Fn(&ModelStreamingDelta),
    controls: TransportRetryControls<'_>,
) -> StreamingHttpResult {
    let mut retries = 0usize;
    let mut timeout_retries = 0usize;
    loop {
        if (controls.is_cancelled)() {
            return Err(model_invocation_cancelled_error());
        }
        transport_started(&request.body, controls.observer)?;
        if (controls.is_cancelled)() {
            transport_finished::<()>(&Err(model_invocation_cancelled_error()), controls.observer)?;
            return Err(model_invocation_cancelled_error());
        }
        let emitted_delta = AtomicBool::new(false);
        let guarded_chunk = |delta: &ModelStreamingDelta| {
            if !delta.content.is_empty() || !delta.thinking.is_empty() {
                emitted_delta.store(true, Ordering::SeqCst);
            }
            on_chunk(delta);
        };
        let result = execute_streaming_http_post(
            provider_key.clone(),
            request.clone(),
            provider_family,
            tool_name_codec.clone(),
            &guarded_chunk,
            controls.is_cancelled,
            controls.observer,
        );
        // SSE usage 可能先发中间快照，最终 ModelResponse 才是该 attempt 的
        // Provider 结算事实。以最终响应覆盖快照，避免 ledger 与 wire response
        // 的 input/output/cache usage 分叉。
        if let Ok(response) = &result
            && let Some(usage) = model_response_transport_usage(response)
        {
            (controls.observer)(&ModelTransportEvent::Updated {
                http_status: None,
                usage: Some(usage),
            })?;
        }
        transport_finished(&result, controls.observer)?;
        let can_retry =
            !emitted_delta.load(Ordering::SeqCst) && retries < MODEL_PROVIDER_MAX_RETRIES;
        let can_retry_empty_stream = retries < MODEL_PROVIDER_EMPTY_STREAM_RETRIES;
        let timeout_budget_left = result.as_ref().err().is_none_or(|error| {
            !is_timeout_error(error) || timeout_retries < MODEL_PROVIDER_TIMEOUT_RETRIES
        });
        match result {
            Err(error)
                if (can_retry && timeout_budget_left && retryable_bridge_error(&error))
                    || (can_retry_empty_stream && retryable_empty_stream_error(&error)) =>
            {
                retries += 1;
                if is_timeout_error(&error) {
                    timeout_retries += 1;
                }
                let is_empty_stream = retryable_empty_stream_error(&error);
                let delay = if is_empty_stream {
                    empty_stream_retry_delay(retries)
                } else {
                    provider_retry_delay(retries, provider_error_retry_after(&error))
                };
                let max_attempts = if is_empty_stream {
                    MODEL_PROVIDER_EMPTY_STREAM_RETRIES
                } else {
                    MODEL_PROVIDER_MAX_RETRIES
                };
                (controls.on_retry)(&ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::Scheduled,
                    attempt: retries,
                    max_attempts,
                    delay_ms: Some(delay.as_millis() as u64),
                });
                if !sleep_retry_delay_cancellable(delay, controls.is_cancelled) {
                    return Err(model_invocation_cancelled_error());
                }
                (controls.on_retry)(&ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::AttemptStarted,
                    attempt: retries,
                    max_attempts,
                    delay_ms: None,
                });
                continue;
            }
            other => {
                if retries > 0 {
                    let max_attempts = other
                        .as_ref()
                        .err()
                        .filter(|error| retryable_empty_stream_error(error))
                        .map(|_| MODEL_PROVIDER_EMPTY_STREAM_RETRIES)
                        .unwrap_or(MODEL_PROVIDER_MAX_RETRIES);
                    (controls.on_retry)(&ModelRetryRuntimeEvent {
                        phase: ModelRetryRuntimePhase::Settled,
                        attempt: retries,
                        max_attempts,
                        delay_ms: None,
                    });
                }
                return other;
            }
        }
    }
}

/// 独立线程内执行的流式 HTTP I/O 逻辑。
struct StreamingHttpIoRequest {
    provider_key: String,
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    provider_family: ProviderFamily,
    tool_name_codec: ProviderToolNameCodec,
    tx: mpsc::Sender<StreamMessage>,
    cancellation_rx: tokio::sync::oneshot::Receiver<()>,
}

async fn streaming_http_io(request: StreamingHttpIoRequest) -> StreamingHttpResult {
    let StreamingHttpIoRequest {
        provider_key,
        url,
        body,
        headers,
        provider_family,
        tool_name_codec,
        tx,
        mut cancellation_rx,
    } = request;
    let client = shared_streaming_http_client()?;
    let stream_idle_timeout = provider_stream_idle_timeout();

    let mut req_builder = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(&body);

    for (name, value) in headers {
        req_builder = req_builder.header(name.as_str(), value.as_str());
    }

    let started_at = Instant::now();
    let mut response = tokio::select! {
        _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
        result = tokio::time::timeout(stream_idle_timeout, req_builder.send()) => {
            match result {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    return Err(BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Transport,
                        code: Some(-32005),
                        message: format!("provider transport failed: {error}"),
                    });
                }
                Err(_) => {
                    return Err(provider_stream_idle_timeout_error(
                        "response headers",
                        stream_idle_timeout,
                    ));
                }
            }
        },
    };

    let status = response.status().as_u16();
    let _ = tx.send(StreamMessage::Observation(ModelTransportEvent::Updated {
        http_status: Some(status),
        usage: None,
    }));
    tracing::info!(
        target: "magi.performance",
        provider_key = %provider_key,
        stage = "provider_headers_received",
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "provider response timing"
    );
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, SystemTime::now()));

    // 非 2xx 状态码时直接读取完整响应体
    if !(200..300).contains(&status) {
        let response_body = tokio::select! {
            _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
            result = tokio::time::timeout(stream_idle_timeout, response.text()) => {
                match result {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(provider_stream_idle_timeout_error(
                            "error response body",
                            stream_idle_timeout,
                        ));
                    }
                }
            },
        }
        .map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("reading error response body failed: {error}"),
        })?;
        return Err(provider_http_status_error(
            status,
            &response_body,
            retry_after,
        ));
    }

    // 流式读取 SSE 事件
    let mut sse_parser = SseLineParser::new();
    let mut accumulator = StreamAccumulator::new();
    // 用于累积跨 read 调用的不完整 UTF-8 字节，防止多字节字符（如中文 3 字节）
    // 被 4096 buffer 边界切割后 lossy 替换为 U+FFFD 导致数据损坏。
    let mut utf8_remainder: Vec<u8> = Vec::new();
    let mut delta_progress = StreamDeltaProgress::default();
    let mut saw_sse_event = false;
    let mut saw_protocol_terminal = false;
    let mut terminal_drain_deadline = None;
    let mut raw_response = String::new();
    let mut first_delta_reported = false;
    let mut body_timing = BodyStreamTiming::default();
    let mut received_at = Duration::ZERO;

    'stream_read: loop {
        let chunk_result = if let Some(deadline) = terminal_drain_deadline {
            tokio::select! {
                _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
                _ = tokio::time::sleep_until(deadline) => break,
                result = response.chunk() => result,
            }
        } else {
            tokio::select! {
                _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
                result = tokio::time::timeout(
                    stream_idle_timeout,
                    response.chunk(),
                ) => match result {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(provider_stream_idle_timeout_error(
                            "SSE event",
                            stream_idle_timeout,
                        ));
                    }
                },
            }
        };
        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(error) => {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32005),
                    // 已收到 SSE 后发生读取错误，和缺少终止事件属于同一种
                    // “响应已部分交付但无法确认完整性”的语义。统一归一化后，
                    // 上层可以保留片段并续写；未收到事件时仍走传输层原请求重试。
                    message: if saw_sse_event {
                        format!("provider stream interrupted: reading stream chunk failed: {error}")
                    } else {
                        format!("reading stream chunk failed: {error}")
                    },
                });
            }
        };
        let Some(chunk) = chunk else {
            break;
        };

        received_at = started_at.elapsed();
        // All SSE events in this HTTP read share its receipt time.
        // 将 remainder 与新读取的字节合并后做安全的 UTF-8 解码
        utf8_remainder.extend_from_slice(&chunk);
        let (valid_str, consumed) = decode_utf8_safe(&utf8_remainder);
        if consumed > 0 {
            utf8_remainder.drain(..consumed);
        }
        if valid_str.is_empty() {
            continue;
        }
        raw_response.push_str(&valid_str);

        for sse_event in sse_parser.feed(&valid_str) {
            saw_sse_event = true;
            if apply_provider_stream_event(
                provider_family,
                &sse_event,
                &mut accumulator,
                &mut delta_progress,
                &tx,
                &mut body_timing,
                received_at,
            )? {
                saw_protocol_terminal = true;
                break 'stream_read;
            }
            if !first_delta_reported
                && (delta_progress.content_len > 0
                    || delta_progress.thinking_len > 0
                    || delta_progress.tool_call_count > 0)
            {
                first_delta_reported = true;
                tracing::info!(
                    target: "magi.performance",
                    provider_key = %provider_key,
                    stage = "provider_first_delta",
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "provider response timing"
                );
            }
        }
        if accumulator.saw_terminal() && terminal_drain_deadline.is_none() {
            terminal_drain_deadline =
                Some(tokio::time::Instant::now() + MODEL_PROVIDER_TERMINAL_DRAIN_TIMEOUT);
        }
    }

    if !saw_protocol_terminal {
        for sse_event in sse_parser.finish() {
            saw_sse_event = true;
            if apply_provider_stream_event(
                provider_family,
                &sse_event,
                &mut accumulator,
                &mut delta_progress,
                &tx,
                &mut body_timing,
                received_at,
            )? {
                saw_protocol_terminal = true;
                break;
            }
        }
    }

    if !saw_sse_event {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message: provider_non_sse_stream_message(&raw_response),
        });
    }

    if !saw_protocol_terminal && !accumulator.saw_terminal() {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: "provider stream interrupted: missing terminal SSE event".to_string(),
        });
    }

    // 流聚合结果直接转换为统一模型响应，不再构造中间协议 JSON。
    let adapted = tool_name_codec.decode_adapted_response(accumulator.finalize());
    if adapted.content.trim().is_empty()
        && adapted
            .thinking
            .as_ref()
            .is_none_or(|thinking| thinking.trim().is_empty())
        && adapted.tool_calls.is_empty()
        && adapted.provider_context.is_empty()
    {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message: "provider response invalid: empty stream response".to_string(),
        });
    }
    body_timing.response_received_at = received_at;
    if let Some(timing) = body_timing.finish(
        provider_family,
        adapted.usage.output_tokens,
        adapted
            .thinking
            .as_ref()
            .is_some_and(|text| !text.is_empty()),
        !adapted.tool_calls.is_empty(),
    ) {
        let _ = tx.send(StreamMessage::Observation(
            ModelTransportEvent::ResponseTiming(timing),
        ));
    }
    let mut response: ModelResponse = adapted.into();
    if !body_timing.usage_observed {
        response.usage = None;
    } else if body_timing.usage_output_tokens
        == response
            .usage
            .as_ref()
            .and_then(|usage| usage.get("outputTokens"))
            .and_then(Value::as_u64)
        && let Some(reasoning) = body_timing.reasoning_tokens
        && let Some(usage) = response.usage.as_mut()
    {
        usage["reasoningTokens"] = json!(reasoning);
    }
    Ok(response)
}

impl ModelBridgeClient for HttpModelBridgeClient {
    fn cache_prompt_ir(&self, request: &ModelInvocationRequest) -> PromptIr {
        self.prepare_prompt_ir(request, false, None).0
    }

    fn invoke_observed(
        &self,
        request: ModelInvocationRequest,
        options: ModelInvocationOptions<'_>,
        observer: &ModelTransportObserver<'_>,
    ) -> Result<ModelResponse, BridgeClientError> {
        match options.on_delta {
            Some(delta) => self.invoke_streaming_observed(
                request,
                delta,
                options.on_retry,
                options.is_cancelled,
                options.max_output_tokens,
                observer,
            ),
            None => self.invoke_cancellable_with_output_limit(
                request,
                options.max_output_tokens,
                options.on_retry,
                options.is_cancelled,
                observer,
            ),
        }
    }
    fn executor_capabilities(&self) -> crate::ExecutorCapabilities {
        crate::native_executor_capabilities()
    }

    fn cache_identity(&self) -> Option<crate::ModelCacheIdentity> {
        let protocol = match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => "openai_chat",
            HttpModelBridgeProtocol::Responses => "openai_responses",
            HttpModelBridgeProtocol::AnthropicMessages => "anthropic_messages",
        };
        // 与真实请求共用 renderer。未实际发送的 effort/capability 元数据不应轮换缓存。
        let request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: String::new(),
            prompt: "cache identity".into(),
            messages: None,
            tools: None,
            tool_choice: None,
        };
        let mut params = compile_prompt_ir(&request, false, self.reasoning_effort, None);
        params.reasoning_capability = self.reasoning_capability.clone();
        let adapted = self.adapt_request(&params).ok()?;
        let reasoning = ["reasoning_effort", "reasoning", "thinking", "output_config"]
            .into_iter()
            .filter_map(|field| {
                adapted
                    .body
                    .get(field)
                    .map(|value| (field.into(), value.clone()))
            })
            .collect::<serde_json::Map<String, Value>>();
        let endpoint = build_protocol_endpoint_url(&self.base_url, &adapted.url_path).ok()?;
        let mut endpoint = reqwest::Url::parse(&endpoint).ok()?;
        endpoint.set_username("").ok()?;
        endpoint.set_password(None).ok()?;
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        Some(crate::ModelCacheIdentity {
            protocol: protocol.into(),
            endpoint: endpoint.to_string(),
            model: self.model.clone(),
            reasoning_profile: canonicalize_json(&Value::Object(reasoning)).to_string(),
        })
    }

    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_with_cancellation(request, &|| false)
    }

    fn invoke_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_cancellable_with_output_limit(request, None, &|_| {}, is_cancelled, &|_| Ok(()))
    }

    fn invoke_with_retry_events_and_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_cancellable_with_output_limit(
            request,
            None,
            on_retry,
            is_cancelled,
            &|_| Ok(()),
        )
    }

    fn invoke_with_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        max_output_tokens: u32,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_cancellable_with_output_limit(
            request,
            Some(max_output_tokens),
            &|_| {},
            is_cancelled,
            &|_| Ok(()),
        )
    }

    fn invoke_with_retry_events_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        max_output_tokens: u32,
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_cancellable_with_output_limit(
            request,
            Some(max_output_tokens),
            on_retry,
            is_cancelled,
            &|_| Ok(()),
        )
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_observed(request, on_delta, &|_| {}, &|| false, None, &|_| Ok(()))
    }

    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_observed(request, on_delta, on_retry, &|| false, None, &|_| Ok(()))
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_observed(request, on_delta, on_retry, is_cancelled, None, &|_| Ok(()))
    }

    fn invoke_streaming_with_output_limit(
        &self,
        request: ModelInvocationRequest,
        max_output_tokens: u32,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming_observed(
            request,
            on_delta,
            on_retry,
            is_cancelled,
            Some(max_output_tokens),
            &|_| Ok(()),
        )
    }
}

impl HttpModelBridgeClient {
    fn invoke_cancellable_with_output_limit(
        &self,
        request: ModelInvocationRequest,
        max_output_tokens: Option<u32>,
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
        observer: &ModelTransportObserver<'_>,
    ) -> Result<ModelResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }
        let http_request =
            self.build_http_request_with_output_limit(&request, false, max_output_tokens)?;
        let fallback_attempts = std::cell::Cell::new(0usize);
        let (accepted_request, status, response_body, retry_after, fallback_capability) =
            execute_with_tool_choice_fallback(http_request, self.provider_family(), |request| {
                let fallback = fallback_attempts.get();
                if fallback > 0 {
                    on_retry(&ModelRetryRuntimeEvent {
                        phase: ModelRetryRuntimePhase::AttemptStarted,
                        attempt: fallback,
                        max_attempts: 2,
                        delay_ms: None,
                    });
                }
                fallback_attempts.set(fallback + 1);
                execute_cancellable_http_post_with_retries(
                    self.provider_request_key(),
                    request.url,
                    request.body,
                    request.headers,
                    self.provider_family(),
                    TransportRetryControls {
                        on_retry,
                        is_cancelled,
                        observer,
                    },
                )
            })?;

        if !(200..300).contains(&status) {
            return Err(provider_http_status_error(
                status,
                &response_body,
                retry_after,
            ));
        }
        if let Some(capability) = fallback_capability.or_else(|| {
            capability_from_accepted_tool_choice_request(&accepted_request, self.provider_family())
        }) {
            self.mark_forced_tool_choice_capability(capability);
        }
        let response =
            self.parse_success_payload(&response_body, &accepted_request.tool_name_codec)?;
        Ok(response)
    }
}

impl HttpModelBridgeClient {
    fn parse_success_payload(
        &self,
        response_body: &str,
        tool_name_codec: &ProviderToolNameCodec,
    ) -> Result<ModelResponse, BridgeClientError> {
        if let Some(message) = provider_error_message(response_body) {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32006),
                message: format!("provider rejected request: {message}"),
            });
        }
        let adapted = match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => {
                OpenAiChatCompletionsAdapter.parse_response(200, response_body)
            }
            HttpModelBridgeProtocol::Responses => {
                OpenAiResponsesAdapter.parse_response(200, response_body)
            }
            HttpModelBridgeProtocol::AnthropicMessages => {
                AnthropicMessagesAdapter.parse_response(200, response_body)
            }
        }
        .map_err(|reason| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message: format!("provider response invalid: {reason}"),
        })?;
        let adapted = tool_name_codec.decode_adapted_response(adapted);
        if adapted.content.trim().is_empty()
            && adapted
                .thinking
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
            && adapted.tool_calls.is_empty()
            && adapted.provider_context.is_empty()
        {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32007),
                message: "provider response invalid: empty model response".to_string(),
            });
        }
        Ok(adapted.into())
    }
}
fn read_non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
fn build_anthropic_messages_url_for_test(base_url: &str) -> Result<String, String> {
    let request = AnthropicMessagesAdapter
        .build_request(
            &PromptIr {
                prompt_cache_key: None,

                reasoning_capability: None,
                messages: vec![LlmMessage {
                    origin: Default::default(),
                    role: "user".to_string(),
                    content: LlmMessageContent::Text("ping".to_string()),
                }],
                max_tokens: None,
                temperature: None,
                stop: vec![],
                tools: None,
                stream: None,

                tool_choice: None,
                reasoning_effort: None,
            },
            "claude-test",
        )
        .expect("anthropic adapter should build test request");
    build_protocol_endpoint_url(base_url, &request.url_path)
}

fn build_protocol_endpoint_url(base_url: &str, endpoint_path: &str) -> Result<String, String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err("base_url must not be empty".to_string());
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("base_url must start with http:// or https://".to_string());
    }

    let normalized = trimmed.trim_end_matches('/');
    let endpoint_path = endpoint_path.trim();
    let endpoint_suffix = endpoint_path
        .trim_matches('/')
        .strip_prefix("v1/")
        .unwrap_or_else(|| endpoint_path.trim_matches('/'))
        .trim_matches('/');
    let endpoint_suffix = (!endpoint_suffix.is_empty())
        .then_some(endpoint_suffix)
        .ok_or_else(|| "endpoint_path must include an endpoint leaf".to_string())?;
    if normalized.ends_with(endpoint_path) || normalized.ends_with(&format!("/{endpoint_suffix}")) {
        return Ok(normalized.to_string());
    }
    if normalized.ends_with("/v1") {
        return Ok(format!("{normalized}/{endpoint_suffix}"));
    }

    Ok(format!("{normalized}{endpoint_path}"))
}

/// 安全地将字节切片解码为 UTF-8 字符串，不丢弃尾部不完整的多字节序列。
/// 返回 (已解码的合法字符串, 已消费的字节数)。
/// 尾部不完整的字节（1-3 字节）由调用方保留，等待下次读取后拼接。
fn decode_utf8_safe(bytes: &[u8]) -> (String, usize) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), bytes.len()),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            if valid_up_to == 0 {
                // 全部都是不完整的字节，等待下一次读取
                return (String::new(), 0);
            }
            // SAFETY: from_utf8 已验证 bytes[..valid_up_to] 是合法 UTF-8
            let valid_str = unsafe { std::str::from_utf8_unchecked(&bytes[..valid_up_to]) };
            (valid_str.to_string(), valid_up_to)
        }
    }
}

fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    const MAX_CHARS: usize = 512;
    let mut collected: String = trimmed.chars().take(MAX_CHARS).collect();
    if trimmed.chars().count() > MAX_CHARS {
        collected.push_str("...");
    }
    collected
}

fn provider_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    match value.get("error")? {
        Value::String(message) => {
            Some(message.trim().to_string()).filter(|value| !value.is_empty())
        }
        Value::Object(error) => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn provider_non_sse_stream_message(body: &str) -> String {
    if let Some(message) = provider_error_message(body) {
        return format!("provider rejected request: {message}");
    }
    format!(
        "provider response invalid: expected event stream, body={}",
        truncate_body(body)
    )
}

fn provider_error_retry_after(error: &BridgeClientError) -> Option<Duration> {
    match error {
        BridgeClientError::HttpStatusFailed { retry_after, .. } => *retry_after,
        _ => None,
    }
}

fn provider_http_status_error(
    status: u16,
    response_body: &str,
    retry_after: Option<Duration>,
) -> BridgeClientError {
    let message = if let Some(message) = provider_error_message(response_body) {
        format!("provider rejected request: {message}")
    } else {
        format!(
            "provider rejected request: body={}",
            truncate_body(response_body)
        )
    };
    BridgeClientError::HttpStatusFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32006),
        http_status: status,
        message,
        retry_after,
    }
}

fn is_forced_tool_choice_rejection(
    status: u16,
    response_body: &str,
    request_body: &Value,
    provider_family: ProviderFamily,
) -> bool {
    status == 400
        && is_forced_tool_choice_request(request_body, provider_family)
        && is_forced_tool_choice_rejection_message(response_body, true)
}

fn is_forced_tool_choice_rejection_error(
    error: &BridgeClientError,
    request_body: &Value,
    provider_family: ProviderFamily,
) -> bool {
    is_forced_tool_choice_request(request_body, provider_family)
        && is_forced_tool_choice_rejection_message(
            &error.to_string(),
            error.http_status() == Some(400),
        )
}

fn is_forced_tool_choice_request(request_body: &Value, provider_family: ProviderFamily) -> bool {
    !matches!(
        tool_choice_enforcement(request_body, provider_family),
        ToolChoiceEnforcement::Automatic
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolChoiceEnforcement {
    Exact,
    Required,
    Automatic,
}

fn tool_choice_enforcement(
    request_body: &Value,
    provider_family: ProviderFamily,
) -> ToolChoiceEnforcement {
    match provider_family {
        ProviderFamily::OpenAiChat if request_body["tool_choice"].is_object() => {
            ToolChoiceEnforcement::Exact
        }
        ProviderFamily::OpenAiChat
            if request_body["tool_choice"]
                .as_str()
                .is_some_and(|choice| choice == "required") =>
        {
            ToolChoiceEnforcement::Required
        }
        ProviderFamily::OpenAiResponses if request_body["tool_choice"].is_object() => {
            ToolChoiceEnforcement::Exact
        }
        ProviderFamily::OpenAiResponses
            if request_body["tool_choice"]
                .as_str()
                .is_some_and(|choice| choice == "required") =>
        {
            ToolChoiceEnforcement::Required
        }
        ProviderFamily::Anthropic
            if request_body["tool_choice"]["type"]
                .as_str()
                .is_some_and(|kind| kind == "tool") =>
        {
            ToolChoiceEnforcement::Exact
        }
        ProviderFamily::Anthropic
            if request_body["tool_choice"]["type"]
                .as_str()
                .is_some_and(|kind| kind == "any") =>
        {
            ToolChoiceEnforcement::Required
        }
        _ => ToolChoiceEnforcement::Automatic,
    }
}

fn is_forced_tool_choice_rejection_message(
    message: &str,
    allow_opaque_gateway_error: bool,
) -> bool {
    let normalized = message.to_ascii_lowercase().replace('_', " ");
    // 部分 OpenAI 兼容网关会吞掉上游的 tool_choice 细节，只保留这一泛化错误。
    // 仅在当前请求确实使用了强制工具选择时，调用方才会把它用于能力协商。
    let opaque_upstream_rejection = normalized.contains("upstream request failed");
    let mentions_forced_choice = normalized.contains("tool choice")
        || normalized.contains("tool selection")
        || normalized.contains("function calling");
    let rejection = normalized.contains("not support")
        || normalized.contains("unsupported")
        || normalized.contains("does not allow")
        || normalized.contains("not available")
        || normalized.contains("invalid")
        || normalized.contains("rejected")
        || normalized.contains("only supports")
        || normalized.contains("supports only")
        || normalized.contains("cannot")
        || normalized.contains("can't")
        || normalized.contains("must be")
        || normalized.contains("must use");
    (allow_opaque_gateway_error && opaque_upstream_rejection)
        || (mentions_forced_choice && rejection)
}

fn request_with_next_tool_choice_enforcement(
    request: &HttpModelRequest,
    provider_family: ProviderFamily,
) -> Option<(ForcedToolChoiceCapability, HttpModelRequest)> {
    let mut fallback = request.clone();
    match tool_choice_enforcement(&request.body, provider_family) {
        ToolChoiceEnforcement::Exact => {
            fallback.body["tool_choice"] = match provider_family {
                ProviderFamily::OpenAiChat | ProviderFamily::OpenAiResponses => json!("required"),
                ProviderFamily::Anthropic => json!({"type": "any"}),
            };
            Some((ForcedToolChoiceCapability::Required, fallback))
        }
        ToolChoiceEnforcement::Required => {
            fallback.body["tool_choice"] = match provider_family {
                ProviderFamily::OpenAiChat | ProviderFamily::OpenAiResponses => json!("auto"),
                ProviderFamily::Anthropic => json!({"type": "auto"}),
            };
            Some((ForcedToolChoiceCapability::AutoOnly, fallback))
        }
        ToolChoiceEnforcement::Automatic => None,
    }
}

fn execute_with_tool_choice_fallback<F>(
    initial_request: HttpModelRequest,
    provider_family: ProviderFamily,
    mut execute: F,
) -> ToolChoiceFallbackHttpResult
where
    F: FnMut(HttpModelRequest) -> HttpPostResult,
{
    let mut request = initial_request;
    let mut fallback_capability = None;
    loop {
        match execute(request.clone()) {
            Ok((status, body, retry_after))
                if is_forced_tool_choice_rejection(
                    status,
                    &body,
                    &request.body,
                    provider_family,
                ) =>
            {
                let Some((capability, next_request)) =
                    request_with_next_tool_choice_enforcement(&request, provider_family)
                else {
                    return Ok((request, status, body, retry_after, fallback_capability));
                };
                fallback_capability = Some(capability);
                request = next_request;
            }
            Ok(result) => return Ok((request, result.0, result.1, result.2, fallback_capability)),
            Err(error)
                if is_forced_tool_choice_rejection_error(
                    &error,
                    &request.body,
                    provider_family,
                ) =>
            {
                let Some((capability, next_request)) =
                    request_with_next_tool_choice_enforcement(&request, provider_family)
                else {
                    return Err(error);
                };
                fallback_capability = Some(capability);
                request = next_request;
            }
            Err(error) => return Err(error),
        }
    }
}

fn capability_from_accepted_tool_choice_request(
    request: &HttpModelRequest,
    provider_family: ProviderFamily,
) -> Option<ForcedToolChoiceCapability> {
    match tool_choice_enforcement(&request.body, provider_family) {
        ToolChoiceEnforcement::Exact => Some(ForcedToolChoiceCapability::Exact),
        ToolChoiceEnforcement::Required => Some(ForcedToolChoiceCapability::Required),
        // 普通自动选择请求不能证明提供方不支持强制语义，不能污染后续需要工具的
        // 同连接调用；只有从 required 明确降级后的请求才会写入 AutoOnly。
        ToolChoiceEnforcement::Automatic => None,
    }
}

#[cfg(test)]
mod tests;
