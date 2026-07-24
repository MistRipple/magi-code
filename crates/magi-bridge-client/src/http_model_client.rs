use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams, ToolChoice, ToolDefinition,
    ToolInputSchema, parse_tool_result_model_content,
};
use crate::protocol::streaming::{SseLineParser, StreamAccumulator, parse_stream_event};
use crate::protocol::{
    AdaptedResponse, AnthropicMessagesAdapter, OpenAiChatCompletionsAdapter, ProviderAdapter,
    ProviderFamily, ProviderToolNameCodec,
};
use crate::types::{
    BridgeClientError, BridgeErrorLayer, BridgeResponse, ModelBridgeClient, ModelInvocationRequest,
    ModelRetryRuntimeEvent, ModelRetryRuntimePhase, ModelStreamingDelta,
    model_invocation_cancelled_error, model_invocation_error_is_cancelled,
};
use magi_usage_authority::ReasoningEffort;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, SystemTime};

const OPENAI_BASE_URL_ENV: &str = "MAGI_OPENAI_COMPAT_BASE_URL";
const OPENAI_API_KEY_ENV: &str = "MAGI_OPENAI_COMPAT_API_KEY";
const OPENAI_MODEL_ENV: &str = "MAGI_OPENAI_COMPAT_MODEL";
const MODEL_PROVIDER_MAX_IN_FLIGHT: usize = 16;
const MODEL_PROVIDER_MAX_RETRIES: usize = 5;
const MODEL_PROVIDER_EMPTY_STREAM_RETRIES: usize = 2;
const MODEL_PROVIDER_RETRY_DELAYS_SECONDS: [u64; MODEL_PROVIDER_MAX_RETRIES] = [10, 15, 30, 45, 60];
const MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS: [u64; MODEL_PROVIDER_EMPTY_STREAM_RETRIES] =
    [1_000, 3_000];
const MODEL_PROVIDER_MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
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
    AnthropicMessages,
}

/// 直接通过 HTTP 调用已配置的模型提供方，绕过 JSON-RPC 子进程 loopback。
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
}

#[derive(Clone)]
struct HttpModelRequest {
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    tool_name_codec: ProviderToolNameCodec,
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
        }
    }

    #[cfg(test)]
    fn chat_completions_url(&self) -> Result<String, BridgeClientError> {
        build_openai_chat_completions_url(&self.base_url).map_err(|reason| {
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
            HttpModelBridgeProtocol::AnthropicMessages => ProviderFamily::Anthropic,
        }
    }

    fn request_headers(&self) -> Vec<(String, String)> {
        match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => self
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

    fn build_http_request(
        &self,
        request: &ModelInvocationRequest,
        stream: bool,
    ) -> Result<HttpModelRequest, BridgeClientError> {
        let mut headers = self.request_headers();
        let mut params = llm_message_params_from_invocation(request, stream, self.reasoning_effort);
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
        let mut adapted = match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => OpenAiChatCompletionsAdapter
                .build_request(&params, &self.model)
                .map_err(|reason| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("build openai chat request failed: {reason}"),
                })?,
            HttpModelBridgeProtocol::AnthropicMessages => AnthropicMessagesAdapter
                .build_request(&params, &self.model)
                .map_err(|reason| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("build anthropic request failed: {reason}"),
                })?,
        };
        let url =
            build_protocol_endpoint_url(&self.base_url, &adapted.url_path).map_err(|reason| {
                BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Protocol,
                    code: None,
                    message: format!("invalid base_url: {reason}"),
                }
            })?;
        headers.append(&mut adapted.extra_headers);
        Ok(HttpModelRequest {
            url,
            body: adapted.body,
            headers,
            tool_name_codec,
        })
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
    ) -> Result<BridgeResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }

        let http_request = self.build_http_request(&request, true)?;
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
                on_retry,
                is_cancelled,
            );

            match result {
                Ok((status, response_body, _retry_after))
                    if is_forced_tool_choice_rejection(
                        status,
                        &response_body,
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
                        return Err(provider_http_status_error(status, &response_body));
                    };
                    fallback_capability = Some(capability);
                    current_request = next_request;
                }
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
                    fallback_capability = Some(capability);
                    current_request = next_request;
                }
                Ok((status, response_body, _retry_after)) => {
                    if !(200..300).contains(&status) {
                        for delta in buffered_deltas.into_inner() {
                            on_delta(&delta);
                        }
                        return Err(provider_http_status_error(status, &response_body));
                    }
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
                    return Ok(BridgeResponse {
                        ok: true,
                        payload: response_body,
                    });
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

fn retry_delay(attempt: usize, _provider_key: &str) -> Duration {
    let index = attempt
        .saturating_sub(1)
        .min(MODEL_PROVIDER_RETRY_DELAYS_SECONDS.len().saturating_sub(1));
    Duration::from_secs(MODEL_PROVIDER_RETRY_DELAYS_SECONDS[index])
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
    Some(delay.min(MODEL_PROVIDER_MAX_RETRY_DELAY))
}

fn retryable_http_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 529) || status >= 500
}

fn retryable_bridge_error(error: &BridgeClientError) -> bool {
    if model_invocation_error_is_cancelled(error) {
        return false;
    }
    matches!(
        error,
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            ..
        }
    )
}

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

fn sleep_before_retry(provider_key: &str, retry_attempt: usize) {
    sleep_retry_delay(retry_delay(retry_attempt, provider_key));
}

fn sleep_retry_delay(delay: Duration) {
    #[cfg(not(test))]
    std::thread::sleep(delay);
    #[cfg(test)]
    let _ = delay;
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

fn receive_cancellable_http_result(
    rx: mpsc::Receiver<HttpPostResult>,
    cancellation_tx: tokio::sync::oneshot::Sender<()>,
    cancellation: &AtomicBool,
    is_cancelled: &dyn Fn() -> bool,
    disconnected_message: &'static str,
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
            Ok(result) => {
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

/// Execute a blocking HTTP POST on a dedicated thread so we never conflict
/// with a tokio runtime that may be active in the caller's context.
///
/// This avoids the "Cannot drop a runtime in a context where blocking is not
/// allowed" panic that `reqwest::blocking::Client` triggers when constructed
/// or dropped inside a `#[tokio::test]` or other async context.
fn execute_http_post(
    provider_key: String,
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
) -> Result<(u16, String, Option<Duration>), BridgeClientError> {
    std::thread::spawn(move || {
        let _permit = model_provider_gate().acquire(&provider_key);
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: format!("HTTP client build failed: {error}"),
            })?;

        let mut req_builder = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body);

        for (name, value) in headers {
            req_builder = req_builder.header(name.as_str(), value.as_str());
        }

        let response = req_builder
            .send()
            .map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: format!("provider transport failed: {error}"),
            })?;

        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_retry_after(value, SystemTime::now()));
        let response_body = response
            .text()
            .map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: format!("reading response body failed: {error}"),
            })?;

        Ok((status, response_body, retry_after))
    })
    .join()
    .map_err(|_| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: "HTTP request thread panicked".to_string(),
    })?
}

fn execute_cancellable_http_post(
    provider_key: String,
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    is_cancelled: &dyn Fn() -> bool,
) -> HttpPostResult {
    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancellation = cancellation.clone();
    let (cancellation_tx, cancellation_rx) = tokio::sync::oneshot::channel();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = match model_provider_gate()
            .acquire_cancellable(&provider_key, worker_cancellation.as_ref())
        {
            Some(_permit) => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32005),
                    message: format!("HTTP runtime build failed: {error}"),
                })
                .and_then(|runtime| {
                    runtime.block_on(async_http_post_io(url, body, headers, cancellation_rx))
                }),
            None => Err(model_invocation_cancelled_error()),
        };
        let _ = tx.send(result);
    });

    receive_cancellable_http_result(
        rx,
        cancellation_tx,
        cancellation.as_ref(),
        is_cancelled,
        "HTTP request thread terminated unexpectedly",
    )
}

async fn async_http_post_io(
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    mut cancellation_rx: tokio::sync::oneshot::Receiver<()>,
) -> HttpPostResult {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("HTTP client build failed: {error}"),
        })?;
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
    is_cancelled: &dyn Fn() -> bool,
) -> HttpPostResult {
    let mut retries = 0usize;
    loop {
        let result = execute_cancellable_http_post(
            provider_key.clone(),
            url.clone(),
            body.clone(),
            headers.clone(),
            is_cancelled,
        );
        match result {
            Ok((status, _response_body, retry_after))
                if retryable_http_status(status) && retries < MODEL_PROVIDER_MAX_RETRIES =>
            {
                retries += 1;
                let delay = retry_after.unwrap_or_else(|| retry_delay(retries, &provider_key));
                if !sleep_retry_delay_cancellable(delay, is_cancelled) {
                    return Err(model_invocation_cancelled_error());
                }
            }
            Err(error)
                if retryable_bridge_error(&error) && retries < MODEL_PROVIDER_MAX_RETRIES =>
            {
                retries += 1;
                if !sleep_retry_delay_cancellable(retry_delay(retries, &provider_key), is_cancelled)
                {
                    return Err(model_invocation_cancelled_error());
                }
            }
            other => return other,
        }
    }
}

fn execute_http_post_with_retries(
    provider_key: String,
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
) -> Result<(u16, String, Option<Duration>), BridgeClientError> {
    let mut retries = 0usize;
    loop {
        let result = execute_http_post(
            provider_key.clone(),
            url.clone(),
            body.clone(),
            headers.clone(),
        );
        match result {
            Ok((status, _response_body, retry_after))
                if retryable_http_status(status) && retries < MODEL_PROVIDER_MAX_RETRIES =>
            {
                retries += 1;
                sleep_retry_delay(
                    retry_after.unwrap_or_else(|| retry_delay(retries, &provider_key)),
                );
                continue;
            }
            Err(error)
                if retryable_bridge_error(&error) && retries < MODEL_PROVIDER_MAX_RETRIES =>
            {
                retries += 1;
                sleep_before_retry(&provider_key, retries);
                continue;
            }
            other => return other,
        }
    }
}

/// 流式 HTTP 线程发回的消息类型。
enum StreamMessage {
    /// LLM 增量快照——携带已累积的正文与上游 thinking。
    Chunk(ModelStreamingDelta),
    /// HTTP I/O 结束——携带最终结果。
    Done(Result<(u16, String, Option<Duration>), BridgeClientError>),
}

fn provider_stream_event_error(
    provider_family: ProviderFamily,
    event: &crate::protocol::streaming::SseEvent,
) -> Option<BridgeClientError> {
    let payload = serde_json::from_str::<Value>(&event.data).ok()?;
    let error = payload.get("error")?;
    let is_error_event = match provider_family {
        ProviderFamily::OpenAiChat => true,
        ProviderFamily::Anthropic => event.event_type.as_deref() == Some("error"),
    };
    if !is_error_event {
        return None;
    }
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("provider stream returned an error");
    let transient = matches!(
        error_type,
        "overloaded_error"
            | "rate_limit_error"
            | "server_error"
            | "timeout_error"
            | "service_unavailable"
    );
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

fn apply_provider_stream_event(
    provider_family: ProviderFamily,
    tool_name_codec: ProviderToolNameCodec,
    event: &crate::protocol::streaming::SseEvent,
    accumulator: &mut StreamAccumulator,
    last_content_delta_len: &mut usize,
    last_thinking_delta_len: &mut usize,
    tx: &std::sync::mpsc::Sender<StreamMessage>,
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
    tool_name_codec.decode_stream_chunks(&mut llm_chunks);
    accumulator.apply_all(&llm_chunks);
    let accumulated_content = accumulator.accumulated_content();
    let accumulated_thinking = accumulator.accumulated_thinking();
    if accumulated_content.len() > *last_content_delta_len
        || accumulated_thinking.len() > *last_thinking_delta_len
    {
        *last_content_delta_len = accumulated_content.len();
        *last_thinking_delta_len = accumulated_thinking.len();
        if tx
            .send(StreamMessage::Chunk(ModelStreamingDelta {
                content: accumulated_content,
                thinking: accumulated_thinking,
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
) -> Result<(u16, String, Option<Duration>), BridgeClientError> {
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
            Some(_permit) => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32005),
                    message: format!("streaming HTTP runtime build failed: {error}"),
                })
                .and_then(|runtime| {
                    runtime.block_on(streaming_http_io(
                        url,
                        body,
                        headers,
                        provider_family,
                        tool_name_codec,
                        &tx,
                        cancellation_rx,
                    ))
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
                        StreamMessage::Chunk(next_delta) => delta = next_delta,
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
    on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(u16, String, Option<Duration>), BridgeClientError> {
    let mut retries = 0usize;
    loop {
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
            is_cancelled,
        );
        let can_retry =
            !emitted_delta.load(Ordering::SeqCst) && retries < MODEL_PROVIDER_MAX_RETRIES;
        let can_retry_empty_stream = retries < MODEL_PROVIDER_EMPTY_STREAM_RETRIES;
        match result {
            Ok((status, _response_body, retry_after))
                if can_retry && retryable_http_status(status) =>
            {
                retries += 1;
                let delay = retry_after.unwrap_or_else(|| retry_delay(retries, &provider_key));
                on_retry(&ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::Scheduled,
                    attempt: retries,
                    max_attempts: MODEL_PROVIDER_MAX_RETRIES,
                    delay_ms: Some(delay.as_millis() as u64),
                });
                if !sleep_retry_delay_cancellable(delay, is_cancelled) {
                    return Err(model_invocation_cancelled_error());
                }
                on_retry(&ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::AttemptStarted,
                    attempt: retries,
                    max_attempts: MODEL_PROVIDER_MAX_RETRIES,
                    delay_ms: None,
                });
                continue;
            }
            Err(error)
                if (can_retry && retryable_bridge_error(&error))
                    || (can_retry_empty_stream && retryable_empty_stream_error(&error)) =>
            {
                retries += 1;
                let is_empty_stream = retryable_empty_stream_error(&error);
                let delay = if is_empty_stream {
                    empty_stream_retry_delay(retries)
                } else {
                    retry_delay(retries, &provider_key)
                };
                let max_attempts = if is_empty_stream {
                    MODEL_PROVIDER_EMPTY_STREAM_RETRIES
                } else {
                    MODEL_PROVIDER_MAX_RETRIES
                };
                on_retry(&ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::Scheduled,
                    attempt: retries,
                    max_attempts,
                    delay_ms: Some(delay.as_millis() as u64),
                });
                if !sleep_retry_delay_cancellable(delay, is_cancelled) {
                    return Err(model_invocation_cancelled_error());
                }
                on_retry(&ModelRetryRuntimeEvent {
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
                    on_retry(&ModelRetryRuntimeEvent {
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
async fn streaming_http_io(
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    provider_family: ProviderFamily,
    tool_name_codec: ProviderToolNameCodec,
    tx: &mpsc::Sender<StreamMessage>,
    mut cancellation_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<(u16, String, Option<Duration>), BridgeClientError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // 流式场景下 timeout 用于检测 LLM 长时间无输出（卡死），
        // 设 5 分钟：正常思考时间足够，真正无响应时能及时报错。
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("HTTP client build failed: {error}"),
        })?;

    let mut req_builder = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(&body);

    for (name, value) in headers {
        req_builder = req_builder.header(name.as_str(), value.as_str());
    }

    let mut response = tokio::select! {
        _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
        result = req_builder.send() => result,
    }
    .map_err(|error| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: format!("provider transport failed: {error}"),
    })?;

    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, SystemTime::now()));

    // 非 2xx 状态码时直接读取完整响应体
    if !(200..300).contains(&status) {
        let response_body = tokio::select! {
            _ = &mut cancellation_rx => return Err(model_invocation_cancelled_error()),
            result = response.text() => result,
        }
        .map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("reading error response body failed: {error}"),
        })?;
        return Ok((status, response_body, retry_after));
    }

    // 流式读取 SSE 事件
    let mut sse_parser = SseLineParser::new();
    let mut accumulator = StreamAccumulator::new();
    // 用于累积跨 read 调用的不完整 UTF-8 字节，防止多字节字符（如中文 3 字节）
    // 被 4096 buffer 边界切割后 lossy 替换为 U+FFFD 导致数据损坏。
    let mut utf8_remainder: Vec<u8> = Vec::new();
    let mut last_content_delta_len = 0usize;
    let mut last_thinking_delta_len = 0usize;
    let mut saw_sse_event = false;
    let mut saw_protocol_terminal = false;
    let mut terminal_drain_deadline = None;
    let mut raw_response = String::new();

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
                result = response.chunk() => result,
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
                tool_name_codec.clone(),
                &sse_event,
                &mut accumulator,
                &mut last_content_delta_len,
                &mut last_thinking_delta_len,
                tx,
            )? {
                saw_protocol_terminal = true;
                break 'stream_read;
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
                tool_name_codec.clone(),
                &sse_event,
                &mut accumulator,
                &mut last_content_delta_len,
                &mut last_thinking_delta_len,
                tx,
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

    // 直接将 StreamAccumulator 转换为 BridgeResponse payload，
    // 跳过自构造 OpenAI JSON → 再反序列化的冗余链路。
    let adapted = tool_name_codec.decode_adapted_response(accumulator.finalize());
    if adapted.content.trim().is_empty()
        && adapted
            .thinking
            .as_ref()
            .is_none_or(|thinking| thinking.trim().is_empty())
        && adapted.tool_calls.is_empty()
    {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message: "provider response invalid: empty stream response".to_string(),
        });
    }
    let payload = adapted_response_to_bridge_payload(&adapted);

    Ok((status, payload, retry_after))
}

impl ModelBridgeClient for HttpModelBridgeClient {
    fn invoke(&self, request: ModelInvocationRequest) -> Result<BridgeResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }

        let http_request = self.build_http_request(&request, false)?;

        let (accepted_request, status, response_body, _retry_after, fallback_capability) =
            execute_with_tool_choice_fallback(http_request, self.provider_family(), |request| {
                execute_http_post_with_retries(
                    self.provider_request_key(),
                    request.url,
                    request.body,
                    request.headers,
                )
            })?;

        if !(200..300).contains(&status) {
            return Err(provider_http_status_error(status, &response_body));
        }

        if let Some(capability) = fallback_capability.or_else(|| {
            capability_from_accepted_tool_choice_request(&accepted_request, self.provider_family())
        }) {
            self.mark_forced_tool_choice_capability(capability);
        }

        let payload =
            self.parse_success_payload(&response_body, &accepted_request.tool_name_codec)?;

        Ok(BridgeResponse { ok: true, payload })
    }

    fn invoke_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<BridgeResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }
        let http_request = self.build_http_request(&request, false)?;
        let (accepted_request, status, response_body, _retry_after, fallback_capability) =
            execute_with_tool_choice_fallback(http_request, self.provider_family(), |request| {
                execute_cancellable_http_post_with_retries(
                    self.provider_request_key(),
                    request.url,
                    request.body,
                    request.headers,
                    is_cancelled,
                )
            })?;

        if !(200..300).contains(&status) {
            return Err(provider_http_status_error(status, &response_body));
        }
        if let Some(capability) = fallback_capability.or_else(|| {
            capability_from_accepted_tool_choice_request(&accepted_request, self.provider_family())
        }) {
            self.mark_forced_tool_choice_capability(capability);
        }
        let payload =
            self.parse_success_payload(&response_body, &accepted_request.tool_name_codec)?;
        Ok(BridgeResponse { ok: true, payload })
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<BridgeResponse, BridgeClientError> {
        self.invoke_streaming_observed(request, on_delta, &|_| {}, &|| false)
    }

    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<BridgeResponse, BridgeClientError> {
        self.invoke_streaming_observed(request, on_delta, on_retry, &|| false)
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<BridgeResponse, BridgeClientError> {
        self.invoke_streaming_observed(request, on_delta, on_retry, is_cancelled)
    }
}

impl HttpModelBridgeClient {
    fn parse_success_payload(
        &self,
        response_body: &str,
        tool_name_codec: &ProviderToolNameCodec,
    ) -> Result<String, BridgeClientError> {
        if let Some(message) = provider_error_message(response_body) {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32006),
                message: format!("provider rejected request: {message}"),
            });
        }
        match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => {
                let envelope: OpenAiCompatibleChatCompletionEnvelope =
                    serde_json::from_str(response_body).map_err(|error| {
                        BridgeClientError::CallFailed {
                            layer: BridgeErrorLayer::RemoteBusiness,
                            code: Some(-32007),
                            message: format!(
                                "provider response invalid: decode chat completion failed: {error}"
                            ),
                        }
                    })?;

                select_openai_bridge_payload(envelope.choices, envelope.usage, tool_name_codec)
                    .map_err(|reason| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::RemoteBusiness,
                        code: Some(-32007),
                        message: format!("provider response invalid: {reason}"),
                    })
            }
            HttpModelBridgeProtocol::AnthropicMessages => {
                let adapted = AnthropicMessagesAdapter
                    .parse_response(200, response_body)
                    .map_err(|reason| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::RemoteBusiness,
                        code: Some(-32007),
                        message: format!("provider response invalid: {reason}"),
                    })?;
                Ok(adapted_response_to_bridge_payload(
                    &tool_name_codec.decode_adapted_response(adapted),
                ))
            }
        }
    }
}
// --- OpenAI response types -- mirrors model_loopback.rs for consistency

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleChatCompletionEnvelope {
    #[serde(default)]
    usage: Option<OpenAiCompatibleUsage>,
    choices: Vec<OpenAiCompatibleChatChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleChatChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    message: Option<OpenAiCompatibleChatMessage>,
    #[serde(default)]
    text: Option<String>,
}

impl OpenAiCompatibleChatChoice {
    fn into_payload(self, usage: Option<OpenAiCompatibleUsage>) -> OpenAiCompatibleSuccessPayload {
        let (content, reasoning_content, tool_calls) = match self.message {
            Some(message) => {
                let reasoning_content = message
                    .reasoning_content
                    .filter(|content| !content.trim().is_empty());
                (
                    message
                        .content
                        .map(OpenAiCompatibleMessageContent::into_text)
                        .filter(|content| !content.trim().is_empty())
                        .or(message.refusal),
                    reasoning_content,
                    message.tool_calls,
                )
            }
            None => (None, None, Vec::new()),
        };

        OpenAiCompatibleSuccessPayload {
            content: content.or(self.text),
            reasoning_content,
            finish_reason: self.finish_reason,
            usage,
            tool_calls,
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleChatMessage {
    #[serde(default)]
    content: Option<OpenAiCompatibleMessageContent>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default, deserialize_with = "deserialize_openai_tool_calls")]
    tool_calls: Vec<OpenAiCompatibleToolCall>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiCompatibleMessageContent {
    Text(String),
    Parts(Vec<OpenAiCompatibleMessagePart>),
}

impl OpenAiCompatibleMessageContent {
    fn into_text(self) -> String {
        match self {
            Self::Text(text) => text,
            Self::Parts(parts) => parts.into_iter().filter_map(|part| part.text).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleMessagePart {
    #[serde(rename = "type")]
    _kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleSuccessPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<OpenAiCompatibleUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiCompatibleToolCall>,
}

impl OpenAiCompatibleSuccessPayload {
    fn into_bridge_payload(self) -> Result<String, String> {
        if self.content.is_none() && self.reasoning_content.is_none() && self.tool_calls.is_empty()
        {
            return Err(
                "missing message.content/text, message.reasoning_content or message.tool_calls"
                    .to_string(),
            );
        }

        if self.finish_reason.is_none()
            && self.usage.is_none()
            && self.reasoning_content.is_none()
            && self.tool_calls.is_empty()
        {
            return Ok(self.content.unwrap_or_default());
        }

        serde_json::to_string(&self)
            .map_err(|error| format!("serialize structured payload failed: {error}"))
    }
}

fn select_openai_bridge_payload(
    choices: Vec<OpenAiCompatibleChatChoice>,
    usage: Option<OpenAiCompatibleUsage>,
    tool_name_codec: &ProviderToolNameCodec,
) -> Result<String, String> {
    if choices.is_empty() {
        return Err("missing choices[0]".to_string());
    }

    let mut invalid_choices = Vec::new();
    for (index, choice) in choices.into_iter().enumerate() {
        let mut payload = choice.into_payload(usage.clone());
        for tool_call in &mut payload.tool_calls {
            tool_call.function.name = tool_name_codec.decode_name(&tool_call.function.name);
        }
        match payload.into_bridge_payload() {
            Ok(payload) => return Ok(payload),
            Err(reason) => invalid_choices.push(format!("choices[{index}]: {reason}")),
        }
    }

    Err(format!(
        "no bridgeable choices in response: {}",
        invalid_choices.join("; ")
    ))
}

#[derive(Debug, Clone, serde::Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<serde_json::Value>,
    #[serde(default)]
    completion_tokens_details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    function: OpenAiCompatibleToolFunction,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleToolFunction {
    name: String,
    #[serde(deserialize_with = "deserialize_openai_tool_arguments")]
    arguments: String,
}

fn deserialize_openai_tool_arguments<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawOpenAiToolArguments {
        String(String),
        Json(serde_json::Value),
    }

    match RawOpenAiToolArguments::deserialize(deserializer)? {
        RawOpenAiToolArguments::String(arguments) => Ok(arguments),
        RawOpenAiToolArguments::Json(arguments) => {
            serde_json::to_string(&arguments).map_err(serde::de::Error::custom)
        }
    }
}

fn deserialize_openai_tool_calls<'de, D>(
    deserializer: D,
) -> Result<Vec<OpenAiCompatibleToolCall>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Vec<OpenAiCompatibleToolCall>>::deserialize(deserializer)
        .map(|tool_calls| tool_calls.unwrap_or_default())
}

fn read_non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
fn build_openai_chat_completions_url(base_url: &str) -> Result<String, String> {
    build_protocol_endpoint_url(base_url, "/v1/chat/completions")
}

#[cfg(test)]
fn build_anthropic_messages_url_for_test(base_url: &str) -> Result<String, String> {
    let request = AnthropicMessagesAdapter
        .build_request(
            &LlmMessageParams {
                messages: vec![LlmMessage {
                    role: "user".to_string(),
                    content: LlmMessageContent::Text("ping".to_string()),
                }],
                max_tokens: None,
                temperature: None,
                tools: None,
                stream: None,
                system_prompt: None,
                tool_choice: None,
                reasoning_effort: None,
            },
            "claude-test",
        )
        .expect("anthropic adapter should build test request");
    build_protocol_endpoint_url(base_url, &request.url_path)
}

fn build_protocol_endpoint_url(base_url: &str, endpoint_path: &str) -> Result<String, String> {
    let normalized = base_url.trim().trim_end_matches('/');
    let endpoint_path = endpoint_path.trim();
    let endpoint_suffix = endpoint_path
        .trim_matches('/')
        .strip_prefix("v1/")
        .unwrap_or_else(|| endpoint_path.trim_matches('/'))
        .trim_matches('/');
    let endpoint_suffix = (!endpoint_suffix.is_empty())
        .then_some(endpoint_suffix)
        .ok_or_else(|| "endpoint_path must include an endpoint leaf".to_string())?;
    if normalized.is_empty() {
        return Err("base_url must not be empty".to_string());
    }
    if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
        return Err("base_url must start with http:// or https://".to_string());
    }
    if normalized.ends_with(endpoint_path) || normalized.ends_with(&format!("/{endpoint_suffix}")) {
        return Ok(normalized.to_string());
    }
    if normalized.ends_with("/v1") {
        return Ok(format!("{normalized}/{endpoint_suffix}"));
    }

    Ok(format!("{normalized}{endpoint_path}"))
}

fn llm_message_params_from_invocation(
    request: &ModelInvocationRequest,
    stream: bool,
    reasoning_effort: Option<ReasoningEffort>,
) -> LlmMessageParams {
    let messages = request
        .messages
        .as_ref()
        .map(|messages| messages.iter().map(llm_message_from_chat_message).collect())
        .unwrap_or_else(|| {
            vec![LlmMessage {
                role: "user".to_string(),
                content: LlmMessageContent::Text(request.prompt.clone()),
            }]
        });
    LlmMessageParams {
        messages,
        max_tokens: None,
        temperature: None,
        tools: request.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(tool_definition_from_chat_tool)
                .collect::<Vec<_>>()
        }),
        stream: Some(stream),
        system_prompt: None,
        tool_choice: request
            .tool_choice
            .as_ref()
            .map(|choice| ToolChoice::Typed {
                kind: "tool".to_string(),
                name: Some(choice.function.name.clone()),
            }),
        reasoning_effort,
    }
}

fn llm_message_from_chat_message(message: &crate::types::ChatMessage) -> LlmMessage {
    if let Some(tool_call_id) = message.tool_call_id.as_ref() {
        let tool_result_content = parse_tool_result_model_content(message.content.as_deref());
        return LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: tool_call_id.clone(),
                content: tool_result_content.text,
                is_error: false,
                images: tool_result_content.images,
            }]),
        };
    }

    let mut blocks = Vec::new();
    if let Some(content) = message
        .content
        .as_ref()
        .filter(|content| !content.is_empty())
    {
        blocks.push(LlmContentBlock::Text {
            text: content.clone(),
        });
    }
    for image in &message.images {
        blocks.push(LlmContentBlock::Image {
            source: image.clone(),
        });
    }
    for tool_call in &message.tool_calls {
        let input = serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
            .unwrap_or_else(|_| json!({}));
        blocks.push(LlmContentBlock::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.function.name.clone(),
            input,
        });
    }

    let content = if blocks.is_empty()
        || (blocks.len() == 1 && message.images.is_empty() && message.tool_calls.is_empty())
    {
        LlmMessageContent::Text(message.content.clone().unwrap_or_default())
    } else {
        LlmMessageContent::Blocks(blocks)
    };
    LlmMessage {
        role: message.role.clone(),
        content,
    }
}

fn tool_definition_from_chat_tool(tool: &crate::types::ChatToolDefinition) -> ToolDefinition {
    let parameters = &tool.function.parameters;
    ToolDefinition {
        name: tool.function.name.clone(),
        description: tool.function.description.clone(),
        input_schema: ToolInputSchema {
            kind: parameters
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("object")
                .to_string(),
            properties: parameters
                .get("properties")
                .cloned()
                .unwrap_or_else(|| json!({})),
            required: parameters
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                }),
        },
        origin: tool.origin,
    }
}

/// 将 `AdaptedResponse` 直接转换为 `BridgeResponse.payload` 格式，
/// 跳过自构造 OpenAI choices[] envelope → 再反序列化的冗余链路。
fn adapted_response_to_bridge_payload(adapted: &AdaptedResponse) -> String {
    let tool_calls: Vec<OpenAiCompatibleToolCall> = adapted
        .tool_calls
        .iter()
        .map(|tc| OpenAiCompatibleToolCall {
            id: Some(tc.id.clone()),
            kind: Some("function".to_string()),
            function: OpenAiCompatibleToolFunction {
                name: tc.name.clone(),
                arguments: tc.raw_arguments.as_deref().unwrap_or("{}").to_string(),
            },
        })
        .collect();
    let prompt_tokens_details = if adapted.usage.cache_read_included_in_input {
        adapted
            .usage
            .cache_read_tokens
            .map(|cached_tokens| json!({ "cached_tokens": cached_tokens }))
    } else {
        None
    };

    let payload = OpenAiCompatibleSuccessPayload {
        content: if adapted.content.is_empty() {
            None
        } else {
            Some(adapted.content.clone())
        },
        reasoning_content: adapted
            .thinking
            .as_ref()
            .filter(|thinking| !thinking.trim().is_empty())
            .cloned(),
        finish_reason: Some(adapted.stop_reason.clone()),
        usage: Some(OpenAiCompatibleUsage {
            prompt_tokens: Some(adapted.usage.input_tokens),
            completion_tokens: Some(adapted.usage.output_tokens),
            total_tokens: Some(adapted.usage.input_tokens + adapted.usage.output_tokens),
            prompt_tokens_details,
            completion_tokens_details: None,
        }),
        tool_calls,
    };

    payload
        .into_bridge_payload()
        .unwrap_or_else(|_| adapted.content.clone())
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

fn provider_http_status_error(status: u16, response_body: &str) -> BridgeClientError {
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
        && is_forced_tool_choice_rejection_message(response_body)
}

fn is_forced_tool_choice_rejection_error(
    error: &BridgeClientError,
    request_body: &Value,
    provider_family: ProviderFamily,
) -> bool {
    is_forced_tool_choice_request(request_body, provider_family)
        && is_forced_tool_choice_rejection_message(&error.to_string())
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

fn is_forced_tool_choice_rejection_message(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase().replace('_', " ");
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
    mentions_forced_choice && rejection
}

fn request_with_next_tool_choice_enforcement(
    request: &HttpModelRequest,
    provider_family: ProviderFamily,
) -> Option<(ForcedToolChoiceCapability, HttpModelRequest)> {
    let mut fallback = request.clone();
    match tool_choice_enforcement(&request.body, provider_family) {
        ToolChoiceEnforcement::Exact => {
            fallback.body["tool_choice"] = match provider_family {
                ProviderFamily::OpenAiChat => json!("required"),
                ProviderFamily::Anthropic => json!({"type": "any"}),
            };
            Some((ForcedToolChoiceCapability::Required, fallback))
        }
        ToolChoiceEnforcement::Required => {
            fallback.body["tool_choice"] = match provider_family {
                ProviderFamily::OpenAiChat => json!("auto"),
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
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn from_env_returns_none_when_base_url_not_set() {
        // Ensure the env var is not set in this process context.
        // from_env() reads MAGI_OPENAI_COMPAT_BASE_URL -- if it is not set,
        // it should return None.
        let saved = env::var(OPENAI_BASE_URL_ENV).ok();
        // SAFETY: test code runs serially via `cargo test`; no other thread
        // accesses this env var concurrently in this test binary.
        unsafe { env::remove_var(OPENAI_BASE_URL_ENV) };

        let result = HttpModelBridgeClient::from_env();
        assert!(
            result.is_none(),
            "from_env() should return None without BASE_URL"
        );

        // Restore if it was set.
        if let Some(value) = saved {
            // SAFETY: same as above.
            unsafe { env::set_var(OPENAI_BASE_URL_ENV, value) };
        }
    }

    #[test]
    fn build_request_body_creates_openai_compatible_payload() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "hello world".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        });
        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello world");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn build_request_body_promotes_multimodal_tool_result_model_content() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let tool_result = serde_json::json!({
            "tool": "view_image",
            "status": "succeeded",
            "summary": "已读取图片",
            "model_content": [
                { "type": "text", "text": "已读取图片" },
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "AAA"
                    }
                }
            ]
        });

        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "ignored when messages exist".to_string(),
            messages: Some(vec![crate::types::ChatMessage {
                role: "tool".to_string(),
                content: Some(tool_result.to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call_view_image".to_string()),
            }]),
            tools: None,
            tool_choice: None,
        });

        assert_eq!(body["messages"].as_array().expect("messages").len(), 2);
        assert_eq!(body["messages"][0]["role"], "tool");
        assert_eq!(body["messages"][0]["content"], "已读取图片");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(
            body["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
    }

    #[test]
    fn build_request_body_sends_chat_message_images_as_multimodal_blocks() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "ignored when messages exist".to_string(),
            messages: Some(vec![crate::types::ChatMessage {
                role: "user".to_string(),
                content: Some("识别这张图片".to_string()),
                images: vec![crate::llm_types::ImageSource {
                    kind: "base64".to_string(),
                    media_type: "image/png".to_string(),
                    data: "AAA".to_string(),
                }],
                tool_calls: Vec::new(),
                tool_call_id: None,
            }]),
            tools: None,
            tool_choice: None,
        });

        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "识别这张图片");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
    }

    #[test]
    fn build_request_body_preserves_forced_tool_choice() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "classify".to_string(),
            messages: None,
            tools: Some(vec![crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "classify_session_turn".to_string(),
                    description: "分类当前会话 turn".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "route": { "type": "string" }
                        },
                        "required": ["route"]
                    }),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function(
                "classify_session_turn",
            )),
        });

        assert_eq!(body["tool_choice"]["type"], "function");
        assert!(
            body["tool_choice"]["function"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("magi_builtin_classify_session_turn_"))
        );
    }

    #[test]
    fn request_uses_wire_safe_names_for_every_tool_source() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "any-openai-compatible-model".to_string(),
        );
        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "搜索最新消息".to_string(),
            messages: Some(vec![crate::types::ChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![crate::types::ChatToolCall {
                    id: "call-search-1".to_string(),
                    kind: "function".to_string(),
                    function: crate::types::ChatToolFunction {
                        name: "web_search".to_string(),
                        arguments: r#"{"query":"OpenAI"}"#.to_string(),
                    },
                }],
                tool_call_id: None,
            }]),
            tools: Some(vec![
                crate::types::ChatToolDefinition {
                    kind: "function".to_string(),
                    function: crate::types::ChatToolFunctionDefinition {
                        name: "web_search".to_string(),
                        description: "搜索网页".to_string(),
                        parameters: serde_json::json!({
                            "type": "object",
                            "properties": { "query": { "type": "string" } },
                            "required": ["query"]
                        }),
                    },
                    origin: crate::types::ChatToolOrigin::Builtin,
                },
                crate::types::ChatToolDefinition {
                    kind: "function".to_string(),
                    function: crate::types::ChatToolFunctionDefinition {
                        name: "shell_exec".to_string(),
                        description: "执行命令".to_string(),
                        parameters: serde_json::json!({
                            "type": "object",
                            "properties": { "command": { "type": "string" } },
                            "required": ["command"]
                        }),
                    },
                    origin: crate::types::ChatToolOrigin::Builtin,
                },
                crate::types::ChatToolDefinition {
                    kind: "function".to_string(),
                    function: crate::types::ChatToolFunctionDefinition {
                        name: "mcp__repo__inspect".to_string(),
                        description: "检查仓库".to_string(),
                        parameters: serde_json::json!({
                            "type": "object",
                            "properties": {}
                        }),
                    },
                    origin: crate::types::ChatToolOrigin::ExternalMcp,
                },
                crate::types::ChatToolDefinition {
                    kind: "function".to_string(),
                    function: crate::types::ChatToolFunctionDefinition {
                        name: "skill__review__inspect".to_string(),
                        description: "执行审查技能".to_string(),
                        parameters: serde_json::json!({
                            "type": "object",
                            "properties": {}
                        }),
                    },
                    origin: crate::types::ChatToolOrigin::Skill,
                },
            ]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("web_search")),
        });

        for tool in body["tools"].as_array().expect("tools must be an array") {
            assert!(
                tool["function"]["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("magi_"))
            );
        }
        assert!(
            body["tools"][0]["function"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("magi_builtin_web_search_"))
        );
        assert!(
            body["tools"][2]["function"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("magi_mcp_mcp_repo_inspect_"))
        );
        assert!(
            body["tools"][3]["function"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("magi_skill_skill_review_inspect_"))
        );
        assert!(body["tools"][0].get("origin").is_none());
        assert!(body["tools"][2].get("origin").is_none());
        assert!(body["tools"][3].get("origin").is_none());
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            body["tools"][0]["function"]["name"]
        );
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            body["tools"][0]["function"]["name"]
        );
    }

    #[test]
    fn response_restores_canonical_tool_name_from_current_surface() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "any-openai-compatible-model".to_string(),
        );
        let invocation = ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "search".to_string(),
            messages: None,
            tools: Some(vec![crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "web_search".to_string(),
                    description: "search".to_string(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("web_search")),
        };
        let http_request = client
            .build_http_request(&invocation, false)
            .expect("request should build");
        let wire_name = http_request.tool_name_codec.encode_name("web_search");
        let canonical = client
            .parse_success_payload(
                &serde_json::json!({
                    "choices": [{
                        "finish_reason": "tool_calls",
                        "message": {
                            "tool_calls": [{
                                "id": "call-search-1",
                                "type": "function",
                                "function": {
                                    "name": wire_name,
                                    "arguments": "{\"query\":\"OpenAI\"}"
                                }
                            }]
                        }
                    }]
                })
                .to_string(),
                &http_request.tool_name_codec,
            )
            .expect("provider response should be bridgeable");
        let value: serde_json::Value = serde_json::from_str(&canonical).expect("canonical payload");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "web_search");
    }

    #[test]
    fn streaming_tool_calls_restore_canonical_tool_name() {
        let tool_definition = crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "web_search".to_string(),
                description: "搜索网页".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        };
        let invocation = ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "搜索 Magi".to_string(),
            messages: None,
            tools: Some(vec![tool_definition.clone()]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("web_search")),
        };
        let codec_client = HttpModelBridgeClient::new(
            "http://127.0.0.1:1".to_string(),
            Some("test-key".to_string()),
            "any-openai-compatible-model".to_string(),
        );
        let wire_name = codec_client
            .build_http_request(&invocation, true)
            .expect("request should build")
            .tool_name_codec
            .encode_name("web_search");
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call-search-1\",\"type\":\"function\",\"function\":{{\"name\":\"{wire_name}\",\"arguments\":\"{{\\\"query\\\":\\\"Magi\\\"}}\"}}}}]}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
            ),
        );
        let client = HttpModelBridgeClient::new(
            server.address,
            Some("test-key".to_string()),
            "any-openai-compatible-model".to_string(),
        );

        let response = client
            .invoke_streaming(invocation, &|_| {})
            .expect("streaming response should be bridgeable");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["tool_calls"][0]["function"]["name"], "web_search");

        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive request");
        let body: serde_json::Value =
            serde_json::from_str(&recorded.body).expect("request body should be json");
        assert_eq!(body["tools"][0]["function"]["name"], wire_name);
        assert_eq!(body["tool_choice"]["function"]["name"], wire_name);
    }

    #[test]
    fn forced_tool_choice_rejection_retries_with_required_choice() {
        let request = HttpModelRequest {
            url: "http://localhost/v1/chat/completions".to_string(),
            body: json!({
                "tools": [{"type": "function"}],
                "tool_choice": {
                    "type": "function",
                    "function": {"name": "shell_exec"}
                }
            }),
            headers: Vec::new(),
            tool_name_codec: ProviderToolNameCodec::for_params(
                &llm_message_params_from_invocation(
                    &ModelInvocationRequest {
                        provider: "test".to_string(),
                        prompt: "test".to_string(),
                        messages: None,
                        tools: None,
                        tool_choice: None,
                    },
                    false,
                    None,
                ),
            ),
        };
        let error_body = r#"{"error":{"message":"tool_choice does not support required or object in thinking mode"}}"#;

        assert!(is_forced_tool_choice_rejection(
            400,
            error_body,
            &request.body,
            ProviderFamily::OpenAiChat
        ));
        let (capability, fallback) =
            request_with_next_tool_choice_enforcement(&request, ProviderFamily::OpenAiChat)
                .expect("exact choice must have a required fallback");
        assert_eq!(capability, ForcedToolChoiceCapability::Required);
        assert_eq!(fallback.body["tool_choice"], "required");
        assert_eq!(request.body["tool_choice"]["type"], "function");
    }

    #[test]
    fn required_choice_rejection_retries_with_automatic_choice() {
        let request = HttpModelRequest {
            url: "http://localhost/v1/chat/completions".to_string(),
            body: json!({
                "tools": [{"type": "function"}],
                "tool_choice": "required"
            }),
            headers: Vec::new(),
            tool_name_codec: ProviderToolNameCodec::for_params(
                &llm_message_params_from_invocation(
                    &ModelInvocationRequest {
                        provider: "test".to_string(),
                        prompt: "test".to_string(),
                        messages: None,
                        tools: None,
                        tool_choice: None,
                    },
                    false,
                    None,
                ),
            ),
        };

        let (capability, fallback) =
            request_with_next_tool_choice_enforcement(&request, ProviderFamily::OpenAiChat)
                .expect("required choice must have an automatic fallback");
        assert_eq!(capability, ForcedToolChoiceCapability::AutoOnly);
        assert_eq!(fallback.body["tool_choice"], "auto");
    }

    #[test]
    fn unrelated_bad_request_is_not_retried_with_automatic_choice() {
        let body = json!({
            "tools": [{"type": "function"}],
            "tool_choice": {
                "type": "function",
                "function": {"name": "shell_exec"}
            }
        });

        assert!(!is_forced_tool_choice_rejection(
            400,
            r#"{"error":{"message":"context length exceeded"}}"#,
            &body,
            ProviderFamily::OpenAiChat
        ));
    }

    #[test]
    fn build_anthropic_request_body_uses_messages_contract() {
        let client = HttpModelBridgeClient::new_with_protocol(
            "https://api.anthropic.com".to_string(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );

        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "anthropic".to_string(),
            prompt: "ignored when messages exist".to_string(),
            messages: Some(vec![
                crate::types::ChatMessage {
                    role: "system".to_string(),
                    content: Some("系统约束".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
                crate::types::ChatMessage {
                    role: "user".to_string(),
                    content: Some("你好".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            ]),
            tools: Some(vec![crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "shell_exec".to_string(),
                    description: "运行 shell 命令".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "cmd": { "type": "string" }
                        },
                        "required": ["cmd"]
                    }),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
        });

        assert_eq!(body["model"], "claude-sonnet-test");
        assert_eq!(body["system"], "系统约束");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "你好");
        assert!(
            body["tools"][0]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("magi_builtin_shell_exec_"))
        );
        assert_eq!(body["tools"][0]["input_schema"]["required"][0], "cmd");
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], body["tools"][0]["name"]);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn chat_completions_injects_reasoning_effort_when_configured() {
        let cases = [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Xhigh, "xhigh"),
        ];
        for (effort, expected) in cases {
            let client = HttpModelBridgeClient::new_with_protocol(
                "https://api.example.com/v1".to_string(),
                Some("test-key".to_string()),
                // 选用命中能力表 OpenAI reasoning profile 的模型名（gpt-5*）；
                // 未命中条目的模型即便配置了 effort 也不会注入 reasoning_effort。
                "gpt-5-test".to_string(),
                HttpModelBridgeProtocol::ChatCompletions,
                Some(effort),
            );
            let body = client.build_request_body(&ModelInvocationRequest {
                provider: "openai".to_string(),
                prompt: "ping".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            });
            assert_eq!(
                body["reasoning_effort"], expected,
                "effort {:?} should serialize to `{}`",
                effort, expected
            );
        }
    }

    #[test]
    fn chat_completions_omits_reasoning_effort_when_unset() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-test".to_string(),
        );
        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "ping".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        });
        assert!(
            body.get("reasoning_effort").is_none(),
            "未配置推理级别时不得写入 reasoning_effort 字段"
        );
    }

    #[test]
    fn anthropic_request_maps_reasoning_effort_to_budget_tokens() {
        let cases = [
            (ReasoningEffort::Low, 1024u32),
            (ReasoningEffort::Medium, 4096u32),
            (ReasoningEffort::High, 16384u32),
            (ReasoningEffort::Xhigh, 32768u32),
        ];
        for (effort, expected_budget) in cases {
            let client = HttpModelBridgeClient::new_with_protocol(
                "https://api.anthropic.com".to_string(),
                Some("sk-ant-test".to_string()),
                // 选用命中能力表 BudgetTokens profile 的模型名（claude-opus-4 前缀）；
                // 4.7 系列改走 effort 路径，legacy 4.x 才映射到 budget_tokens。
                "claude-opus-4-5-test".to_string(),
                HttpModelBridgeProtocol::AnthropicMessages,
                Some(effort),
            );
            let body = client.build_request_body(&ModelInvocationRequest {
                provider: "anthropic".to_string(),
                prompt: "ping".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            });
            assert_eq!(
                body["thinking"]["type"], "enabled",
                "effort {:?} 必须强制启用 thinking",
                effort
            );
            assert_eq!(
                body["thinking"]["budget_tokens"], expected_budget,
                "effort {:?} 应映射到 budget_tokens={}",
                effort, expected_budget
            );
            assert_eq!(
                body["temperature"], 1,
                "Anthropic 在 thinking 启用时 temperature 必须为 1"
            );
            let max_tokens = body["max_tokens"].as_u64().expect("max_tokens 必须为整数");
            assert!(
                max_tokens > expected_budget as u64,
                "max_tokens={} 必须严格大于 budget_tokens={}",
                max_tokens,
                expected_budget
            );
        }
    }

    #[test]
    fn probe_request_inherits_reasoning_effort_for_chat_completions() {
        let client = HttpModelBridgeClient::new_with_protocol(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-5-test".to_string(),
            HttpModelBridgeProtocol::ChatCompletions,
            Some(ReasoningEffort::Xhigh),
        );
        let (url, body, headers) = client
            .build_probe_request()
            .expect("probe request should build");

        assert_eq!(url, "https://api.example.com/v1/chat/completions");
        assert_eq!(body["model"], "gpt-5-test");
        assert_eq!(body["messages"][0]["content"], "ping");
        assert_eq!(
            body["reasoning_effort"], "xhigh",
            "OpenAI 探针必须原样透传 reasoning_effort（Xhigh → xhigh，不再降级到 high）"
        );
        assert!(headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Authorization") && value == "Bearer test-key"
        }));
    }

    #[test]
    fn probe_request_inherits_thinking_budget_for_anthropic() {
        let client = HttpModelBridgeClient::new_with_protocol(
            "https://api.anthropic.com".to_string(),
            Some("sk-ant-test".to_string()),
            // 命中 claude-opus-4 → BudgetTokens profile；4.7 系列改走 effort 路径。
            "claude-opus-4-5-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            Some(ReasoningEffort::Xhigh),
        );
        let (url, body, headers) = client
            .build_probe_request()
            .expect("probe request should build");

        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert_eq!(body["model"], "claude-opus-4-5-test");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 32768);
        assert_eq!(body["temperature"], 1);
        let max_tokens = body["max_tokens"]
            .as_u64()
            .expect("Anthropic 探针 max_tokens 必须为整数");
        assert!(
            max_tokens > 32768,
            "max_tokens={max_tokens} 必须严格大于 budget_tokens=32768"
        );
        assert!(headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-api-key") && value == "sk-ant-test"
        }));
        assert!(headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("anthropic-version") && value == "2023-06-01"
        }));
    }

    #[test]
    fn probe_request_omits_reasoning_effort_when_unset() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-test".to_string(),
        );
        let (_, body, _) = client
            .build_probe_request()
            .expect("probe request should build");
        assert!(
            body.get("reasoning_effort").is_none(),
            "未配置推理级别时探针不得写入 reasoning_effort"
        );
    }

    #[test]
    fn adapted_payload_preserves_cached_prompt_tokens() {
        let payload = adapted_response_to_bridge_payload(&AdaptedResponse {
            content: "hello".to_string(),
            thinking: None,
            tool_calls: Vec::new(),
            usage: crate::llm_types::LlmUsage {
                input_tokens: 10,
                output_tokens: 3,
                cache_read_tokens: Some(4),
                cache_write_tokens: None,
                cache_read_included_in_input: true,
            },
            stop_reason: "stop".to_string(),
            raw: None,
        });
        let value: serde_json::Value =
            serde_json::from_str(&payload).expect("payload should stay json");

        assert_eq!(value["usage"]["prompt_tokens"], 10);
        assert_eq!(value["usage"]["prompt_tokens_details"]["cached_tokens"], 4);
    }

    #[test]
    fn empty_prompt_returns_error() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-4".to_string(),
        );

        let result = client.invoke(ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "   ".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        });

        let error = result.expect_err("empty prompt should fail");
        match error {
            BridgeClientError::CallFailed { code, .. } => {
                assert_eq!(code, Some(-32002));
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn http_status_error_exposes_structured_status() {
        let error = provider_http_status_error(
            429,
            r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#,
        );

        assert_eq!(error.layer(), Some(BridgeErrorLayer::RemoteBusiness));
        assert_eq!(error.code(), Some(-32006));
        assert_eq!(error.http_status(), Some(429));
        assert!(error.to_string().contains("rate limited"));
        assert!(error.to_string().contains("http_status=429"));
    }

    #[test]
    fn chat_completions_url_builds_correctly() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            None,
            "gpt-4".to_string(),
        );
        assert_eq!(
            client.chat_completions_url().unwrap(),
            "https://api.example.com/v1/chat/completions"
        );

        let client2 = HttpModelBridgeClient::new(
            "https://api.example.com/v1/chat/completions".to_string(),
            None,
            "gpt-4".to_string(),
        );
        assert_eq!(
            client2.chat_completions_url().unwrap(),
            "https://api.example.com/v1/chat/completions"
        );

        let client3 =
            HttpModelBridgeClient::new("not-a-url".to_string(), None, "gpt-4".to_string());
        assert!(client3.chat_completions_url().is_err());

        let client4 = HttpModelBridgeClient::new(
            "http://example.com:8320/".to_string(),
            None,
            "gpt-4".to_string(),
        );
        assert_eq!(
            client4.chat_completions_url().unwrap(),
            "http://example.com:8320/v1/chat/completions"
        );
    }

    #[test]
    fn anthropic_messages_url_builds_correctly() {
        assert_eq!(
            build_anthropic_messages_url_for_test("https://api.anthropic.com").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            build_anthropic_messages_url_for_test("https://api.anthropic.com/v1").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            build_anthropic_messages_url_for_test("https://api.anthropic.com/v1/messages").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            build_anthropic_messages_url_for_test("https://api.deepseek.com/anthropic").unwrap(),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    // -----------------------------------------------------------------------
    // Integration test with a local mock HTTP server
    // -----------------------------------------------------------------------

    struct MockHttpServer {
        address: String,
        _handle: std::thread::JoinHandle<()>,
        request_receiver: mpsc::Receiver<RecordedRequest>,
    }

    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
        authorization: Option<String>,
        x_api_key: Option<String>,
        anthropic_version: Option<String>,
    }

    struct MockHttpResponse {
        status: u16,
        content_type: String,
        response_text: String,
    }

    fn spawn_mock_server(status: u16, response_body: serde_json::Value) -> MockHttpServer {
        spawn_mock_server_with_response_text(status, "application/json", response_body.to_string())
    }

    fn spawn_mock_server_with_response_text(
        status: u16,
        content_type: &'static str,
        response_text: String,
    ) -> MockHttpServer {
        spawn_mock_server_sequence(vec![MockHttpResponse {
            status,
            content_type: content_type.to_string(),
            response_text,
        }])
    }

    fn spawn_mock_server_sequence(responses: Vec<MockHttpResponse>) -> MockHttpServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let address = listener.local_addr().expect("address should exist");
        let (sender, receiver) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            for response_config in responses {
                let (mut stream, _) = listener.accept().expect("mock server should accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("timeout should set");

                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 4096];
                let header_end = loop {
                    let read = stream.read(&mut chunk).expect("should read");
                    assert!(read > 0, "should receive data");
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };

                let header_text = String::from_utf8(buffer[..header_end].to_vec())
                    .expect("headers should be utf-8");
                let mut lines = header_text.split("\r\n");
                let request_line = lines.next().expect("request line should exist").to_string();
                let mut authorization = None;
                let mut x_api_key = None;
                let mut anthropic_version = None;
                let mut content_length: usize = 0;
                for line in lines {
                    if line.is_empty() {
                        continue;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        let name_lower = name.trim().to_ascii_lowercase();
                        if name_lower == "content-length" {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                        if name_lower == "authorization" {
                            authorization = Some(value.trim().to_string());
                        }
                        if name_lower == "x-api-key" {
                            x_api_key = Some(value.trim().to_string());
                        }
                        if name_lower == "anthropic-version" {
                            anthropic_version = Some(value.trim().to_string());
                        }
                    }
                }

                while buffer.len() < header_end + content_length {
                    let read = stream.read(&mut chunk).expect("should read body");
                    assert!(read > 0, "should receive body data");
                    buffer.extend_from_slice(&chunk[..read]);
                }

                let body =
                    String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
                        .expect("body should be utf-8");

                // Parse method and path from request line
                let parts: Vec<&str> = request_line.split_whitespace().collect();
                let method = parts.first().unwrap_or(&"").to_string();
                let path = parts.get(1).unwrap_or(&"").to_string();

                let _ = sender.send(RecordedRequest {
                    method,
                    path,
                    body,
                    authorization,
                    x_api_key,
                    anthropic_version,
                });

                let response = format!(
                    "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_text}",
                    status_reason(response_config.status),
                    response_config.response_text.len(),
                    status = response_config.status,
                    content_type = response_config.content_type,
                    response_text = response_config.response_text,
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("should write response");
                stream.flush().expect("should flush");
            }
        });

        MockHttpServer {
            address: format!("http://{address}/v1"),
            _handle: handle,
            request_receiver: receiver,
        }
    }

    fn spawn_cancellation_server(
        streaming: bool,
    ) -> (String, mpsc::Receiver<()>, mpsc::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let address = listener.local_addr().expect("address should exist");
        let (request_ready_tx, request_ready_rx) = mpsc::channel();
        let (disconnected_tx, disconnected_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock server should accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("timeout should set");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = match stream.read(&mut chunk) {
                    Ok(0) => {
                        let _ = disconnected_tx.send(());
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(_) => {
                        let _ = disconnected_tx.send(());
                        return;
                    }
                    Ok(read) => read,
                };
                request.extend_from_slice(&chunk[..read]);
                if let Some(pos) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let read = match stream.read(&mut chunk) {
                    Ok(0) => {
                        let _ = disconnected_tx.send(());
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(_) => {
                        let _ = disconnected_tx.send(());
                        return;
                    }
                    Ok(read) => read,
                };
                request.extend_from_slice(&chunk[..read]);
            }
            if streaming {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"started\"}}]}\n\n",
                    )
                    .expect("stream headers should write");
                stream.flush().expect("stream should flush");
            }
            let _ = request_ready_tx.send(());
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => {
                        let _ = disconnected_tx.send(());
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(_) => {
                        let _ = disconnected_tx.send(());
                        break;
                    }
                    Ok(_) => {}
                }
            }
        });
        (
            format!("http://{address}/v1"),
            request_ready_rx,
            disconnected_rx,
        )
    }

    fn status_reason(status: u16) -> &'static str {
        match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "Test Response",
        }
    }

    #[test]
    fn invoke_against_mock_server_returns_success() {
        let server = spawn_mock_server(
            200,
            serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "hello from direct HTTP client"
                    }
                }]
            }),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let response = client
            .invoke(ModelInvocationRequest {
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect("invoke should succeed against mock server");

        assert!(response.ok);
        assert_eq!(response.payload, "hello from direct HTTP client");

        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive request");
        assert_eq!(recorded.method, "POST");
        assert_eq!(recorded.path, "/v1/chat/completions");
        assert_eq!(
            recorded.authorization.as_deref(),
            Some("Bearer sk-test-key")
        );

        let body: serde_json::Value =
            serde_json::from_str(&recorded.body).expect("body should be json");
        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn cancellable_non_streaming_request_releases_connection() {
        let (address, request_ready, disconnected) = spawn_cancellation_server(false);
        let client = HttpModelBridgeClient::new(
            address,
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation_signal = cancelled.clone();
        let (cancellation_started_tx, cancellation_started_rx) = mpsc::channel();
        std::thread::spawn(move || {
            request_ready
                .recv_timeout(Duration::from_secs(5))
                .expect("request should reach provider before cancellation");
            let cancellation_started = std::time::Instant::now();
            cancellation_signal.store(true, Ordering::SeqCst);
            let _ = cancellation_started_tx.send(cancellation_started);
        });
        let error = client
            .invoke_with_cancellation(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|| cancelled.load(Ordering::SeqCst),
            )
            .expect_err("cancelled request should fail as cancelled");
        assert!(model_invocation_error_is_cancelled(&error));
        let cancellation_started = cancellation_started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cancellation should start after provider receives request");
        assert!(cancellation_started.elapsed() < Duration::from_secs(2));
        disconnected
            .recv_timeout(Duration::from_secs(5))
            .expect("cancelled request should close provider connection");
    }

    #[test]
    fn cancellable_streaming_request_releases_connection() {
        let (address, request_ready, disconnected) = spawn_cancellation_server(true);
        let client = HttpModelBridgeClient::new(
            address,
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation_signal = cancelled.clone();
        let (cancellation_started_tx, cancellation_started_rx) = mpsc::channel();
        std::thread::spawn(move || {
            request_ready
                .recv_timeout(Duration::from_secs(5))
                .expect("stream should reach provider before cancellation");
            let cancellation_started = std::time::Instant::now();
            cancellation_signal.store(true, Ordering::SeqCst);
            let _ = cancellation_started_tx.send(cancellation_started);
        });
        let error = client
            .invoke_streaming_with_cancellation(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
                &|_| {},
                &|| cancelled.load(Ordering::SeqCst),
            )
            .expect_err("cancelled stream should fail as cancelled");
        assert!(model_invocation_error_is_cancelled(&error));
        let cancellation_started = cancellation_started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stream cancellation should start after provider receives request");
        assert!(cancellation_started.elapsed() < Duration::from_secs(2));
        disconnected
            .recv_timeout(Duration::from_secs(5))
            .expect("cancelled stream should close provider connection");
    }

    #[test]
    fn invoke_2xx_error_body_is_reported_as_provider_error() {
        let server = spawn_mock_server_with_response_text(
            200,
            "application/json",
            serde_json::json!({
                "error": {
                    "message": "claude executor: upstream returned empty stream response",
                    "type": "server_error",
                    "code": "internal_server_error",
                }
            })
            .to_string(),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let error = client
            .invoke(ModelInvocationRequest {
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect_err("2xx provider error envelope must be surfaced directly");

        assert!(
            error
                .to_string()
                .contains("claude executor: upstream returned empty stream response")
        );
    }

    #[test]
    fn streaming_preserves_latest_delta_when_one_http_read_contains_multiple_events() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let deltas = std::cell::RefCell::new(Vec::new());

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| deltas.borrow_mut().push(delta.content.clone()),
            )
            .expect("streaming invoke should succeed against mock server");

        assert!(response.ok);
        let deltas = deltas.into_inner();
        assert_eq!(deltas.last().map(String::as_str), Some("Hello"));
        assert!(
            (1..=2).contains(&deltas.len()),
            "累计快照允许合并中间态，但必须保留最终完整内容"
        );

        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive request");
        let body: serde_json::Value =
            serde_json::from_str(&recorded.body).expect("body should be json");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn streaming_coalesces_backlogged_cumulative_snapshots_before_completion() {
        let mut response_text = String::new();
        for _ in 0..200 {
            response_text.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"字\"}}]}\n\n");
        }
        response_text.push_str(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
        let server = spawn_mock_server_with_response_text(200, "text/event-stream", response_text);
        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let callback_count = std::cell::Cell::new(0usize);
        let latest_content = std::cell::RefCell::new(String::new());
        let started_at = Instant::now();

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| {
                    callback_count.set(callback_count.get() + 1);
                    *latest_content.borrow_mut() = delta.content.clone();
                    std::thread::sleep(Duration::from_millis(10));
                },
            )
            .expect("backlogged stream should complete");

        assert!(response.ok);
        assert_eq!(latest_content.borrow().chars().count(), 200);
        assert!(
            callback_count.get() < 20,
            "累计快照积压时不得逐条回放全部中间态"
        );
        assert!(
            started_at.elapsed() < Duration::from_millis(500),
            "业务写回速度不得把已完成的上游流拖成逐字播放"
        );
    }

    #[test]
    fn streaming_retries_forced_tool_choice_as_required_choice() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 400,
                content_type: "application/json".to_string(),
                response_text: r#"{"error":{"message":"tool_choice parameter does not support being set to required or object in thinking mode"}}"#.to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"已恢复\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"已学习\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gateway-model-v2026".to_string(),
        );
        let request = ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "执行工具".to_string(),
            messages: None,
            tools: Some(vec![crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "shell_exec".to_string(),
                    description: "执行 shell 命令".to_string(),
                    parameters: json!({"type": "object"}),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
        };

        let response = client
            .invoke_streaming(request.clone(), &|_| {})
            .expect("forced choice rejection should recover with required choice");
        assert!(response.ok);
        let learned_response = client
            .invoke_streaming(request, &|_| {})
            .expect("learned required-only capability should keep working");
        assert!(learned_response.ok);

        let first: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("first request should arrive")
                .body,
        )
        .expect("first request should be json");
        let second: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("fallback request should arrive")
                .body,
        )
        .expect("fallback request should be json");
        assert_eq!(first["tool_choice"]["type"], "function");
        assert_eq!(second["tool_choice"], "required");
        let third: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("learned request should arrive")
                .body,
        )
        .expect("learned request should be json");
        assert_eq!(third["tool_choice"], "required");
    }

    #[test]
    fn streaming_retries_forced_tool_choice_when_provider_rejects_after_delta() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"首轮半截\"}}]}\n\n",
                    "data: {\"error\":{\"message\":\"tool_choice is not supported in thinking mode\"}}\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"自动恢复\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-5-turbo".to_string(),
        );
        let deltas = std::cell::RefCell::new(Vec::new());
        let request = ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "执行工具".to_string(),
            messages: None,
            tools: Some(vec![crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "shell_exec".to_string(),
                    description: "执行 shell 命令".to_string(),
                    parameters: json!({"type": "object"}),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
        };

        let response = client
            .invoke_streaming(request, &|delta| {
                deltas.borrow_mut().push(delta.content.clone())
            })
            .expect("stream rejection after a partial delta should recover");
        assert!(response.ok);
        assert_eq!(deltas.into_inner(), vec!["自动恢复".to_string()]);

        let first: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("first request should arrive")
                .body,
        )
        .expect("first request should be json");
        let second: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("fallback request should arrive")
                .body,
        )
        .expect("fallback request should be json");
        assert_eq!(first["tool_choice"]["type"], "function");
        assert_eq!(second["tool_choice"], "required");
    }

    #[test]
    fn anthropic_streaming_retries_forced_tool_choice_after_error_event() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: content_block_delta\n",
                    "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"首轮半截\"}}\n\n",
                    "event: error\n",
                    "data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"tool_choice type tool is not supported\"}}\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: content_block_delta\n",
                    "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"自动恢复\"}}\n\n",
                    "event: message_delta\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                    "event: message_stop\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                )
                .to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new_with_protocol(
            server.address.clone(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );
        let deltas = std::cell::RefCell::new(Vec::new());
        let request = ModelInvocationRequest {
            provider: "anthropic".to_string(),
            prompt: "执行工具".to_string(),
            messages: None,
            tools: Some(vec![crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "shell_exec".to_string(),
                    description: "执行 shell 命令".to_string(),
                    parameters: json!({"type": "object"}),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
        };

        let response = client
            .invoke_streaming(request, &|delta| {
                deltas.borrow_mut().push(delta.content.clone())
            })
            .expect("Anthropic stream rejection should recover");
        assert!(response.ok);
        assert_eq!(deltas.into_inner(), vec!["自动恢复".to_string()]);

        let first: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("first request should arrive")
                .body,
        )
        .expect("first request should be json");
        let second: Value = serde_json::from_str(
            &server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("fallback request should arrive")
                .body,
        )
        .expect("fallback request should be json");
        assert_eq!(first["tool_choice"]["type"], "tool");
        assert_eq!(second["tool_choice"]["type"], "any");
    }

    #[test]
    fn streaming_openai_partial_eof_without_terminal_event_is_error() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"半截回复\"},\"finish_reason\":null}]}\n\n"
                .to_string(),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let deltas = std::cell::RefCell::new(Vec::new());

        let error = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| deltas.borrow_mut().push(delta.content.clone()),
            )
            .expect_err("缺少 [DONE]/finish_reason 的提前 EOF 不能被当成正常完成");

        assert!(
            error
                .to_string()
                .contains("provider stream interrupted: missing terminal SSE event")
        );
        assert_eq!(deltas.into_inner(), vec!["半截回复".to_string()]);
    }

    #[test]
    fn streaming_openai_finish_reason_without_done_marker_is_complete() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )
            .to_string(),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect("finish_reason 已经证明 OpenAI 流完整");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "完整回复");
        assert_eq!(payload["finish_reason"], "stop");
    }

    #[test]
    fn streaming_openai_accepts_done_marker_without_trailing_blank_line() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"}}]}\n\n",
                "data: [DONE]\n",
            )
            .to_string(),
        );
        let client = HttpModelBridgeClient::new(
            server.address,
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect("EOF 前没有额外空行时仍应提交最终 SSE 事件");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "完整回复");
    }

    #[test]
    fn streaming_anthropic_partial_eof_without_message_stop_is_error() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"半截 Claude 回复\"}}\n\n",
                "event: content_block_stop\ndata: {\"index\":0}\n\n",
            )
            .to_string(),
        );

        let client = HttpModelBridgeClient::new_with_protocol(
            server.address.clone(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );
        let deltas = std::cell::RefCell::new(Vec::new());

        let error = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "anthropic".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| deltas.borrow_mut().push(delta.content.clone()),
            )
            .expect_err("缺少 message_stop/stop_reason 的 Anthropic 提前 EOF 不能完成 turn");

        assert!(
            error
                .to_string()
                .contains("provider stream interrupted: missing terminal SSE event")
        );
        assert_eq!(deltas.into_inner(), vec!["半截 Claude 回复".to_string()]);
    }

    #[test]
    fn streaming_anthropic_message_stop_marks_stream_complete() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"完整 Claude 回复\"}}\n\n",
                "event: content_block_stop\ndata: {\"index\":0}\n\n",
                "event: message_stop\ndata: {}\n\n",
            )
            .to_string(),
        );

        let client = HttpModelBridgeClient::new_with_protocol(
            server.address.clone(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "anthropic".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect("message_stop 已经证明 Anthropic 流完整");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "完整 Claude 回复");
        assert_eq!(payload["finish_reason"], "end_turn");
    }

    #[test]
    fn streaming_returns_when_done_marker_arrives_before_socket_close() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock stream server should bind");
        let address = listener.local_addr().expect("address should exist");
        let (request_sender, request_receiver) = mpsc::channel();
        let server_handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock stream server should accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout should set");

            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).expect("should read request");
                assert!(read > 0, "should receive request bytes");
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let header_text =
                String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
            let content_length = header_text
                .split("\r\n")
                .filter_map(|line| line.split_once(':'))
                .find_map(|(name, value)| {
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while buffer.len() < header_end + content_length {
                let read = stream.read(&mut chunk).expect("should read request body");
                assert!(read > 0, "should receive request body");
                buffer.extend_from_slice(&chunk[..read]);
            }
            let _ = request_sender.send(());

            let response_head = concat!(
                "HTTP/1.1 200 OK\r\n",
                "content-type: text/event-stream\r\n",
                "connection: keep-alive\r\n",
                "\r\n",
            );
            stream
                .write_all(response_head.as_bytes())
                .expect("should write response headers");
            stream
                .write_all(
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
                        "data: [DONE]\n\n",
                    )
                    .as_bytes(),
                )
                .expect("should write stream body");
            stream.flush().expect("should flush stream body");
            std::thread::sleep(Duration::from_secs(2));
        });

        let client = HttpModelBridgeClient::new(
            format!("http://{address}/v1"),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let (result_sender, result_receiver) = mpsc::channel();
        let client_handle = std::thread::spawn(move || {
            let result = client.invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            );
            let _ = result_sender.send(result);
        });

        request_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("mock server should receive request");
        let response = result_receiver
            .recv_timeout(Duration::from_millis(500))
            .expect("[DONE] 后必须立即完成，不能等 socket close");
        assert!(response.expect("streaming invoke should succeed").ok);

        client_handle
            .join()
            .expect("client streaming thread should not panic");
        server_handle
            .join()
            .expect("mock stream server should not panic");
    }

    #[test]
    fn streaming_returns_after_finish_reason_drain_without_waiting_for_socket_close() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock stream server should bind");
        let address = listener.local_addr().expect("address should exist");
        let (request_sender, request_receiver) = mpsc::channel();
        let server_handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock stream server should accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout should set");

            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).expect("should read request");
                assert!(read > 0, "should receive request bytes");
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let header_text =
                String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
            let content_length = header_text
                .split("\r\n")
                .filter_map(|line| line.split_once(':'))
                .find_map(|(name, value)| {
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while buffer.len() < header_end + content_length {
                let read = stream.read(&mut chunk).expect("should read request body");
                assert!(read > 0, "should receive request body");
                buffer.extend_from_slice(&chunk[..read]);
            }
            let _ = request_sender.send(());

            stream
                .write_all(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "content-type: text/event-stream\r\n",
                        "connection: keep-alive\r\n",
                        "\r\n",
                        "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    )
                    .as_bytes(),
                )
                .expect("should write terminal stream events");
            stream.flush().expect("should flush terminal stream events");
            std::thread::sleep(Duration::from_millis(50));
            stream
                .write_all(
                    b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n",
                )
                .expect("should write delayed usage event");
            stream.flush().expect("should flush delayed usage event");
            std::thread::sleep(Duration::from_secs(2));
        });

        let client = HttpModelBridgeClient::new(
            format!("http://{address}/v1"),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let (result_sender, result_receiver) = mpsc::channel();
        let client_handle = std::thread::spawn(move || {
            let result = client.invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            );
            let _ = result_sender.send(result);
        });

        request_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("mock server should receive request");
        let response = result_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("finish_reason 后必须在排空窗口内完成，不能等待 socket close")
            .expect("streaming invoke should succeed");
        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "完整回复");
        assert_eq!(payload["finish_reason"], "stop");
        assert_eq!(payload["usage"]["prompt_tokens"], 5);
        assert_eq!(payload["usage"]["completion_tokens"], 3);

        client_handle
            .join()
            .expect("client streaming thread should not panic");
        server_handle
            .join()
            .expect("mock stream server should not panic");
    }

    #[test]
    fn streaming_json_error_is_reported_as_provider_error() {
        let server = spawn_mock_server_with_response_text(
            200,
            "application/json",
            serde_json::json!({
                "error": {
                    "message": "empty_stream: upstream stream closed before first payload",
                    "type": "server_error",
                    "code": "internal_server_error",
                }
            })
            .to_string(),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let error = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect_err("JSON error body must not be collapsed into empty assistant response");

        assert!(matches!(
            error,
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                ..
            }
        ));
        assert!(
            error
                .to_string()
                .contains("empty_stream: upstream stream closed before first payload")
        );
    }

    #[test]
    fn streaming_retries_terminal_empty_response_before_returning_failure() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let deltas = std::cell::RefCell::new(Vec::new());

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| deltas.borrow_mut().push(delta.content.clone()),
            )
            .expect("terminal empty response should reconnect before failing");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "recovered");
        assert_eq!(deltas.into_inner(), vec!["recovered".to_string()]);
        for _ in 0..2 {
            server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("mock server should receive the initial and retry requests");
        }
    }

    #[test]
    fn streaming_non_sse_html_is_protocol_failure() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/html",
            "<!doctype html><html><body>gateway ui</body></html>".to_string(),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let error = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect_err("HTML response must not be treated as an empty model answer");

        assert!(
            error
                .to_string()
                .contains("provider response invalid: expected event stream")
        );
    }

    #[test]
    fn invoke_retries_retryable_status_before_success() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 429,
                content_type: "application/json".to_string(),
                response_text: serde_json::json!({
                    "error": {
                        "message": "rate limited",
                        "type": "rate_limit_error",
                        "code": "too_many_requests",
                    }
                })
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "application/json".to_string(),
                response_text: serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": "retry recovered"
                        }
                    }]
                })
                .to_string(),
            },
        ]);

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let response = client
            .invoke(ModelInvocationRequest {
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect("retryable rejection should recover on the next attempt");

        assert_eq!(response.payload, "retry recovered");
        for _ in 0..2 {
            let recorded = server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("mock server should receive every retry attempt");
            assert_eq!(recorded.path, "/v1/chat/completions");
        }
    }

    #[test]
    fn model_provider_gate_caps_same_provider_at_internal_limit() {
        let gate = Arc::new(ModelProviderGate {
            slots: Mutex::new(HashMap::new()),
        });
        let permits = (0..MODEL_PROVIDER_MAX_IN_FLIGHT)
            .map(|_| gate.acquire("anthropic-messages|http://localhost:8317/v1|kiro"))
            .collect::<Vec<_>>();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let second_gate = Arc::clone(&gate);

        let handle = std::thread::spawn(move || {
            let _second_permit =
                second_gate.acquire("anthropic-messages|http://localhost:8317/v1|kiro");
            acquired_tx
                .send(())
                .expect("test receiver should still be alive");
        });

        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(120))
                .is_err(),
            "同一 provider/model 达到并发上限后必须排队"
        );

        drop(permits);
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("释放请求槽位后，下一个同端点请求应继续执行");
        handle.join().expect("waiting thread should not panic");
    }

    #[test]
    fn model_provider_gate_allows_mainline_and_five_agents() {
        let gate = Arc::new(ModelProviderGate {
            slots: Mutex::new(HashMap::new()),
        });
        let existing_permits = (0..4)
            .map(|_| gate.acquire("openai-chat|http://localhost:8317/v1|gpt"))
            .collect::<Vec<_>>();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let handles = (0..2)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let acquired_tx = acquired_tx.clone();
                std::thread::spawn(move || {
                    let _permit = gate.acquire("openai-chat|http://localhost:8317/v1|gpt");
                    acquired_tx
                        .send(())
                        .expect("test receiver should still be alive");
                })
            })
            .collect::<Vec<_>>();
        drop(acquired_tx);

        let acquired_concurrently =
            (0..2).all(|_| acquired_rx.recv_timeout(Duration::from_millis(250)).is_ok());
        drop(existing_permits);
        for handle in handles {
            handle.join().expect("waiting thread should not panic");
        }

        assert!(
            acquired_concurrently,
            "同一模型必须至少允许主线与五个子代理并发请求"
        );
    }

    #[test]
    fn streaming_retries_before_first_delta_only() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 503,
                content_type: "application/json".to_string(),
                response_text: serde_json::json!({
                    "error": {
                        "message": "provider warming up",
                        "type": "server_error",
                        "code": "unavailable",
                    }
                })
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .to_string(),
            },
        ]);

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let deltas = std::cell::RefCell::new(Vec::new());

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| deltas.borrow_mut().push(delta.content.clone()),
            )
            .expect("streaming retry should recover before any delta is emitted");

        assert!(response.ok);
        assert_eq!(deltas.into_inner(), vec!["Hi".to_string()]);
        for _ in 0..2 {
            let recorded = server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("mock server should receive streaming retry attempts");
            assert_eq!(recorded.path, "/v1/chat/completions");
        }
    }

    #[test]
    fn streaming_retries_incomplete_connection_before_first_delta() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {}\n\n".to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"reconnected\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .to_string(),
            },
        ]);

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect("首个可见增量前连接中断时应重连并继续");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "reconnected");
        for _ in 0..2 {
            server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("mock server should receive reconnect attempts");
        }
    }

    #[test]
    fn streaming_does_not_retry_after_visible_delta() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n"
                    .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"duplicate\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new(
            server.address,
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let deltas = std::cell::RefCell::new(Vec::new());

        client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|delta| deltas.borrow_mut().push(delta.content.clone()),
            )
            .expect_err("已经展示正文后不能从头重试并制造重复文本");

        assert_eq!(deltas.into_inner(), vec!["partial".to_string()]);
        server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("first request should be recorded");
        assert!(
            server
                .request_receiver
                .recv_timeout(Duration::from_millis(250))
                .is_err(),
            "可见增量后的失败不能发起第二次模型请求"
        );
    }

    #[test]
    fn streaming_retries_transient_provider_error_event() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: error\n",
                    "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"temporarily overloaded\"}}\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: content_block_delta\n",
                    "data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"recovered\"}}\n\n",
                    "event: message_stop\n",
                    "data: {}\n\n",
                )
                .to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new_with_protocol(
            server.address.clone(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );

        let response = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "anthropic".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect("overloaded SSE error 应在首个增量前重连");

        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "recovered");
        for _ in 0..2 {
            server
                .request_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("mock server should receive reconnect attempts");
        }
    }

    #[test]
    fn model_retry_policy_uses_fixed_five_step_backoff() {
        assert_eq!(MODEL_PROVIDER_MAX_RETRIES, 5);
        assert_eq!(retry_delay(1, "provider"), Duration::from_secs(10));
        assert_eq!(retry_delay(2, "provider"), Duration::from_secs(15));
        assert_eq!(retry_delay(3, "provider"), Duration::from_secs(30));
        assert_eq!(retry_delay(4, "provider"), Duration::from_secs(45));
        assert_eq!(retry_delay(5, "provider"), Duration::from_secs(60));
    }

    #[test]
    fn streaming_retry_reports_runtime_lifecycle() {
        let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 503,
                content_type: "application/json".to_string(),
                response_text: serde_json::json!({
                    "error": {
                        "message": "provider warming up",
                        "type": "server_error"
                    }
                })
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .to_string(),
            },
        ]);
        let client = HttpModelBridgeClient::new(
            server.address,
            Some("sk-test-key".to_string()),
            "gpt-4.1-mini".to_string(),
        );
        let events = std::cell::RefCell::new(Vec::new());

        client
            .invoke_streaming_with_retry_events(
                ModelInvocationRequest {
                    provider: "openai-compatible".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
                &|event| events.borrow_mut().push(event.clone()),
            )
            .expect("retry should recover");

        assert_eq!(
            events.into_inner(),
            vec![
                ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::Scheduled,
                    attempt: 1,
                    max_attempts: 5,
                    delay_ms: Some(10_000),
                },
                ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::AttemptStarted,
                    attempt: 1,
                    max_attempts: 5,
                    delay_ms: None,
                },
                ModelRetryRuntimeEvent {
                    phase: ModelRetryRuntimePhase::Settled,
                    attempt: 1,
                    max_attempts: 5,
                    delay_ms: None,
                },
            ]
        );
    }

    #[test]
    fn retry_policy_rejects_permanent_provider_failures() {
        assert!(!retryable_http_status(400));
        assert!(!retryable_http_status(401));
        assert!(!retryable_http_status(403));
        assert!(!retryable_http_status(404));
        assert!(retryable_http_status(408));
        assert!(retryable_http_status(429));
        assert!(retryable_http_status(500));
        assert!(retryable_http_status(529));
    }

    #[test]
    fn retry_after_supports_seconds_http_dates_and_sixty_second_cap() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(parse_retry_after("12", now), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("120", now), Some(Duration::from_secs(60)));
        let http_date = httpdate::fmt_http_date(now + Duration::from_secs(30));
        assert_eq!(
            parse_retry_after(&http_date, now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after("invalid", now), None);
    }

    #[test]
    fn streaming_surfaces_anthropic_error_event() {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: error\n",
                "data: {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}\n\n",
            )
            .to_string(),
        );
        let client = HttpModelBridgeClient::new_with_protocol(
            server.address,
            Some("bad-key".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );

        let error = client
            .invoke_streaming(
                ModelInvocationRequest {
                    provider: "anthropic".to_string(),
                    prompt: "hello".to_string(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                &|_| {},
            )
            .expect_err("SSE error event must fail the invocation");

        assert!(error.to_string().contains("invalid x-api-key"));
        assert_eq!(error.code(), Some(-32006));
    }

    #[test]
    fn invoke_against_mock_server_returns_structured_payload_with_usage() {
        let server = spawn_mock_server(
            200,
            serde_json::json!({
                "usage": {
                    "prompt_tokens": 5,
                    "completion_tokens": 3,
                    "total_tokens": 8,
                },
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "content": "structured reply"
                    }
                }]
            }),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test".to_string()),
            "gpt-4".to_string(),
        );

        let response = client
            .invoke(ModelInvocationRequest {
                provider: "openai".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect("invoke should succeed");

        assert!(response.ok);
        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "structured reply");
        assert_eq!(payload["finish_reason"], "stop");
        assert_eq!(payload["usage"]["prompt_tokens"], 5);
        assert_eq!(payload["usage"]["completion_tokens"], 3);
        assert_eq!(payload["usage"]["total_tokens"], 8);
    }

    #[test]
    fn invoke_against_mock_server_uses_anthropic_messages_protocol_when_configured() {
        let server = spawn_mock_server(
            200,
            serde_json::json!({
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "anthropic reply"
                }],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 9,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 2
                }
            }),
        );

        let client = HttpModelBridgeClient::new_with_protocol(
            server.address.clone(),
            Some("sk-ant-test".to_string()),
            "claude-sonnet-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );

        let response = client
            .invoke(ModelInvocationRequest {
                provider: "anthropic".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect("invoke should succeed");

        assert!(response.ok);
        let payload: serde_json::Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "anthropic reply");
        assert_eq!(payload["finish_reason"], "end_turn");
        assert_eq!(payload["usage"]["prompt_tokens"], 9);
        assert_eq!(payload["usage"]["completion_tokens"], 5);
        assert!(payload["usage"]["prompt_tokens_details"].is_null());

        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive request");
        assert_eq!(recorded.method, "POST");
        assert_eq!(recorded.path, "/v1/messages");
        assert_eq!(recorded.authorization, None);
        assert_eq!(recorded.x_api_key.as_deref(), Some("sk-ant-test"));
        assert_eq!(recorded.anthropic_version.as_deref(), Some("2023-06-01"));

        let body: serde_json::Value =
            serde_json::from_str(&recorded.body).expect("body should be json");
        assert_eq!(body["model"], "claude-sonnet-test");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn invoke_against_mock_server_handles_rejection() {
        let server = spawn_mock_server(
            400,
            serde_json::json!({
                "error": {
                    "message": "bad request",
                    "type": "invalid_request_error",
                    "code": "invalid_request",
                }
            }),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            Some("sk-test".to_string()),
            "gpt-4".to_string(),
        );

        let error = client
            .invoke(ModelInvocationRequest {
                provider: "openai".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect_err("rejected request should return error");

        match error {
            BridgeClientError::HttpStatusFailed {
                layer,
                code,
                http_status,
                message,
            } => {
                assert_eq!(layer, BridgeErrorLayer::RemoteBusiness);
                assert_eq!(code, Some(-32006));
                assert_eq!(http_status, 400);
                assert!(message.contains("bad request"), "message was: {message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn invoke_against_unreachable_server_returns_transport_error() {
        // Bind and immediately drop to get an unreachable port.
        let address = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind should succeed");
            let addr = listener.local_addr().expect("address should exist");
            drop(listener);
            addr
        };

        let client = HttpModelBridgeClient::new(
            format!("http://{address}/v1"),
            Some("sk-test".to_string()),
            "gpt-4".to_string(),
        );

        let error = client
            .invoke(ModelInvocationRequest {
                provider: "openai".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect_err("unreachable server should return error");

        match error {
            BridgeClientError::CallFailed { layer, .. } => {
                assert_eq!(layer, BridgeErrorLayer::Transport);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn invoke_without_api_key_omits_authorization_header() {
        let server = spawn_mock_server(
            200,
            serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "no auth needed"
                    }
                }]
            }),
        );

        let client = HttpModelBridgeClient::new(
            server.address.clone(),
            None, // no API key
            "local-model".to_string(),
        );

        let response = client
            .invoke(ModelInvocationRequest {
                provider: "openai".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            })
            .expect("invoke should succeed without API key");

        assert!(response.ok);
        assert_eq!(response.payload, "no auth needed");

        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("should receive request");
        assert!(
            recorded.authorization.is_none(),
            "authorization header should be absent"
        );
    }
}
