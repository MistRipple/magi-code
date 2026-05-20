use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams, ToolChoice, ToolDefinition,
    ToolInputSchema,
};
use crate::protocol::streaming::{SseLineParser, StreamAccumulator, parse_stream_event};
use crate::protocol::{AdaptedResponse, AnthropicMessagesAdapter, ProviderAdapter, ProviderFamily};
use crate::types::{
    BridgeClientError, BridgeErrorLayer, BridgeResponse, ModelBridgeClient, ModelInvocationRequest,
    ModelStreamingDelta,
};
use magi_usage_authority::ReasoningEffort;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::io::Read as IoRead;

const OPENAI_BASE_URL_ENV: &str = "MAGI_OPENAI_COMPAT_BASE_URL";
const OPENAI_API_KEY_ENV: &str = "MAGI_OPENAI_COMPAT_API_KEY";
const OPENAI_MODEL_ENV: &str = "MAGI_OPENAI_COMPAT_MODEL";
const OPENAI_CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

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

struct HttpModelRequest {
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
}

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
    /// `reasoning_effort = Some(_)` 时，无论模型是否原生支持，都会显式注入到协议层：
    /// - Chat Completions：写入顶层 `reasoning_effort` 字段。
    /// - Anthropic Messages：映射成 `thinking.budget_tokens` 并强制启用 thinking。
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

    fn openai_request_url(&self, endpoint_path: &str) -> Result<String, BridgeClientError> {
        build_protocol_endpoint_url(&self.base_url, endpoint_path).map_err(|reason| {
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("invalid base_url: {reason}"),
            }
        })
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
        let (url, body) = match self.protocol {
            HttpModelBridgeProtocol::ChatCompletions => {
                let url = self.openai_request_url(OPENAI_CHAT_COMPLETIONS_PATH)?;
                let mut body = self.build_chat_request_body(request);
                body["stream"] = json!(stream);
                (url, body)
            }
            HttpModelBridgeProtocol::AnthropicMessages => {
                let mut adapted = AnthropicMessagesAdapter
                    .build_request(
                        &llm_message_params_from_invocation(
                            request,
                            stream,
                            self.reasoning_effort,
                        ),
                        &self.model,
                    )
                    .map_err(|reason| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: format!("build anthropic request failed: {reason}"),
                    })?;
                let url = build_protocol_endpoint_url(&self.base_url, &adapted.url_path).map_err(
                    |reason| BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::Protocol,
                        code: None,
                        message: format!("invalid base_url: {reason}"),
                    },
                )?;
                headers.append(&mut adapted.extra_headers);
                (url, adapted.body)
            }
        };
        Ok(HttpModelRequest { url, body, headers })
    }

    #[cfg(test)]
    fn build_request_body(&self, request: &ModelInvocationRequest) -> serde_json::Value {
        self.build_http_request(request, false)
            .expect("model request should be buildable in tests")
            .body
    }

    /// 构造一次「最小可用」探针请求（用于连接测试）。
    ///
    /// 与生产链路共用 [`build_http_request`](Self::build_http_request)：
    /// - reasoning_effort、协议路由、认证头、Anthropic thinking 等约束完全一致；
    /// - 探针 body 与真实推理 body 走同一份字段集，永远不会因双轨实现漂移。
    pub fn build_probe_request(
        &self,
    ) -> Result<(String, serde_json::Value, Vec<(String, String)>), BridgeClientError> {
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

    /// Build the JSON body for a chat completions request.
    fn build_chat_request_body(&self, request: &ModelInvocationRequest) -> serde_json::Value {
        let messages = if let Some(ref msgs) = request.messages {
            serde_json::to_value(msgs).unwrap_or_else(|_| json!([]))
        } else {
            json!([{ "role": "user", "content": request.prompt }])
        };
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
        });
        if let Some(ref tools) = request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::to_value(tools).unwrap_or_else(|_| json!([]));
                body["tool_choice"] = request
                    .tool_choice
                    .as_ref()
                    .and_then(|tool_choice| serde_json::to_value(tool_choice).ok())
                    .unwrap_or_else(|| json!("auto"));
            }
        }
        if let Some(effort) = self.reasoning_effort {
            body["reasoning_effort"] = json!(reasoning_effort_label(effort));
        }
        body
    }
}

/// Execute a blocking HTTP POST on a dedicated thread so we never conflict
/// with a tokio runtime that may be active in the caller's context.
///
/// This avoids the "Cannot drop a runtime in a context where blocking is not
/// allowed" panic that `reqwest::blocking::Client` triggers when constructed
/// or dropped inside a `#[tokio::test]` or other async context.
fn execute_http_post(
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
) -> Result<(u16, String), BridgeClientError> {
    std::thread::spawn(move || {
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
        let response_body = response
            .text()
            .map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: format!("reading response body failed: {error}"),
            })?;

        Ok((status, response_body))
    })
    .join()
    .map_err(|_| BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: "HTTP request thread panicked".to_string(),
    })?
}

/// 流式 HTTP 线程发回的消息类型。
enum StreamMessage {
    /// LLM 增量快照——携带已累积的正文与上游 thinking。
    Chunk(ModelStreamingDelta),
    /// HTTP I/O 结束——携带最终结果。
    Done(Result<(u16, String), BridgeClientError>),
}

/// 执行流式 HTTP POST，通过 SSE 逐块读取 LLM 响应。
///
/// HTTP I/O 在独立线程完成（与 `execute_http_post` 一致），避免在 tokio
/// 异步运行时中创建 `reqwest::blocking::Client` 导致 panic。
/// 增量快照通过 channel 发回调用线程，由 `on_chunk` 回调处理。
fn execute_streaming_http_post(
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    provider_family: ProviderFamily,
    on_chunk: &dyn Fn(&ModelStreamingDelta),
) -> Result<(u16, String), BridgeClientError> {
    let (tx, rx) = std::sync::mpsc::channel::<StreamMessage>();

    std::thread::spawn(move || {
        let result = streaming_http_io(url, body, headers, provider_family, &tx);
        // 无论成功失败，都通过 Done 发送最终结果
        let _ = tx.send(StreamMessage::Done(result));
    });

    // 在调用线程上处理增量和最终结果
    for msg in rx {
        match msg {
            StreamMessage::Chunk(delta) => {
                on_chunk(&delta);
            }
            StreamMessage::Done(result) => {
                return result;
            }
        }
    }

    // channel 意外关闭（线程 panic）
    Err(BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: "streaming HTTP request thread terminated unexpectedly".to_string(),
    })
}

/// 独立线程内执行的流式 HTTP I/O 逻辑。
fn streaming_http_io(
    url: String,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
    provider_family: ProviderFamily,
    tx: &std::sync::mpsc::Sender<StreamMessage>,
) -> Result<(u16, String), BridgeClientError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        // 流式场景下 timeout 用于检测 LLM 长时间无输出（卡死），
        // 设 5 分钟：正常思考时间足够，真正无响应时能及时报错。
        .timeout(std::time::Duration::from_secs(300))
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

    let mut response = req_builder
        .send()
        .map_err(|error| BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: format!("provider transport failed: {error}"),
        })?;

    let status = response.status().as_u16();

    // 非 2xx 状态码时直接读取完整响应体
    if !(200..300).contains(&status) {
        let response_body = response
            .text()
            .map_err(|error| BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Transport,
                code: Some(-32005),
                message: format!("reading error response body failed: {error}"),
            })?;
        return Ok((status, response_body));
    }

    // 流式读取 SSE 事件
    let mut sse_parser = SseLineParser::new();
    let mut accumulator = StreamAccumulator::new();
    let mut chunk_buf = [0u8; 4096];
    // 用于累积跨 read 调用的不完整 UTF-8 字节，防止多字节字符（如中文 3 字节）
    // 被 4096 buffer 边界切割后 lossy 替换为 U+FFFD 导致数据损坏。
    let mut utf8_remainder: Vec<u8> = Vec::new();
    let mut last_content_delta_len = 0usize;
    let mut last_thinking_delta_len = 0usize;

    loop {
        let bytes_read =
            response
                .read(&mut chunk_buf)
                .map_err(|error| BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::Transport,
                    code: Some(-32005),
                    message: format!("reading stream chunk failed: {error}"),
                })?;

        if bytes_read == 0 {
            break;
        }

        // 将 remainder 与新读取的字节合并后做安全的 UTF-8 解码
        utf8_remainder.extend_from_slice(&chunk_buf[..bytes_read]);
        let (valid_str, consumed) = decode_utf8_safe(&utf8_remainder);
        if consumed > 0 {
            utf8_remainder.drain(..consumed);
        }
        if valid_str.is_empty() {
            continue;
        }

        let sse_events = sse_parser.feed(&valid_str);

        for sse_event in &sse_events {
            // 检测 [DONE] 标记
            if sse_event.data.trim() == "[DONE]" {
                continue;
            }

            let llm_chunks = parse_stream_event(provider_family, sse_event);
            accumulator.apply_all(&llm_chunks);
        }

        // 当正文或上游 thinking 有增长时通过 channel 发送完整快照。
        let accumulated_content = accumulator.accumulated_content();
        let accumulated_thinking = accumulator.accumulated_thinking();
        if accumulated_content.len() > last_content_delta_len
            || accumulated_thinking.len() > last_thinking_delta_len
        {
            last_content_delta_len = accumulated_content.len();
            last_thinking_delta_len = accumulated_thinking.len();
            // 若 receiver 已断开（调用方提前退出），静默忽略
            if tx
                .send(StreamMessage::Chunk(ModelStreamingDelta {
                    content: accumulated_content,
                    thinking: accumulated_thinking,
                }))
                .is_err()
            {
                break;
            }
        }
    }

    // 直接将 StreamAccumulator 转换为 BridgeResponse payload，
    // 跳过自构造 OpenAI JSON → 再反序列化的冗余链路。
    let adapted = accumulator.finalize();
    let payload = adapted_response_to_bridge_payload(&adapted);

    Ok((status, payload))
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

        let (status, response_body) =
            execute_http_post(http_request.url, http_request.body, http_request.headers)?;

        if !(200..300).contains(&status) {
            // Attempt to extract OpenAI-style error details
            if let Ok(error_envelope) =
                serde_json::from_str::<OpenAiCompatibleErrorEnvelope>(&response_body)
            {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::RemoteBusiness,
                    code: Some(-32006),
                    message: format!(
                        "provider rejected request: {} (http_status={status})",
                        error_envelope.error.message
                    ),
                });
            }
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32006),
                message: format!(
                    "provider rejected request: http_status={status}, body={}",
                    truncate_body(&response_body)
                ),
            });
        }

        let payload = self.parse_success_payload(&response_body)?;

        Ok(BridgeResponse { ok: true, payload })
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<BridgeResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }

        let http_request = self.build_http_request(&request, true)?;

        let (status, response_body) = execute_streaming_http_post(
            http_request.url,
            http_request.body,
            http_request.headers,
            self.provider_family(),
            on_delta,
        )?;

        if !(200..300).contains(&status) {
            if let Ok(error_envelope) =
                serde_json::from_str::<OpenAiCompatibleErrorEnvelope>(&response_body)
            {
                return Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::RemoteBusiness,
                    code: Some(-32006),
                    message: format!(
                        "provider rejected request: {} (http_status={status})",
                        error_envelope.error.message
                    ),
                });
            }
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32006),
                message: format!(
                    "provider rejected request: http_status={status}, body={}",
                    truncate_body(&response_body)
                ),
            });
        }

        // 流式路径的 response_body 已经是 BridgeResponse payload 格式，
        // 由 adapted_response_to_bridge_payload 直接生成，无需再反序列化。
        Ok(BridgeResponse {
            ok: true,
            payload: response_body,
        })
    }
}

impl HttpModelBridgeClient {
    fn parse_success_payload(&self, response_body: &str) -> Result<String, BridgeClientError> {
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

                select_openai_bridge_payload(envelope.choices, envelope.usage).map_err(|reason| {
                    BridgeClientError::CallFailed {
                        layer: BridgeErrorLayer::RemoteBusiness,
                        code: Some(-32007),
                        message: format!("provider response invalid: {reason}"),
                    }
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
                Ok(adapted_response_to_bridge_payload(&adapted))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// OpenAI response types -- mirrors model_loopback.rs for consistency
// ---------------------------------------------------------------------------

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
    #[serde(default)]
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
) -> Result<String, String> {
    if choices.is_empty() {
        return Err("missing choices[0]".to_string());
    }

    let mut invalid_choices = Vec::new();
    for (index, choice) in choices.into_iter().enumerate() {
        match choice.into_payload(usage.clone()).into_bridge_payload() {
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

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleErrorEnvelope {
    error: OpenAiCompatibleErrorBody,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompatibleErrorBody {
    message: String,
    #[serde(default)]
    #[allow(dead_code)]
    r#type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    code: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    param: Option<serde_json::Value>,
}

fn read_non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
fn build_openai_chat_completions_url(base_url: &str) -> Result<String, String> {
    build_protocol_endpoint_url(base_url, OPENAI_CHAT_COMPLETIONS_PATH)
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
                timeout_ms: None,
                stream_idle_timeout_ms: None,
                stream_hard_timeout_ms: None,
                retry_policy: None,
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
        timeout_ms: None,
        stream_idle_timeout_ms: None,
        stream_hard_timeout_ms: None,
        retry_policy: None,
        reasoning_effort,
    }
}

/// 将 `ReasoningEffort` 序列化为下游 Chat Completions API 接受的字符串字面量。
///
/// 四档粒度（`low | medium | high | xhigh`）原样透传，由下游模型/网关自行解释。
/// magi 不在这里做向下兼容降级——把"极高"偷偷映射成"high"会让用户看见的等级
/// 与真实生效等级永久错位（见 Task #59 回归）。Anthropic Messages 协议不消费
/// 该字符串，只读 `params.reasoning_effort` 枚举映射 budget tokens。
fn reasoning_effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    }
}

fn llm_message_from_chat_message(message: &crate::types::ChatMessage) -> LlmMessage {
    if let Some(tool_call_id) = message.tool_call_id.as_ref() {
        return LlmMessage {
            role: "user".to_string(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: tool_call_id.clone(),
                content: message.content.clone().unwrap_or_default(),
                is_error: false,
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
    for tool_call in &message.tool_calls {
        let input = serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
            .unwrap_or_else(|_| json!({}));
        blocks.push(LlmContentBlock::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.function.name.clone(),
            input,
        });
    }

    let content = if blocks.is_empty() || (blocks.len() == 1 && message.tool_calls.is_empty()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

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
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function(
                "classify_session_turn",
            )),
        });

        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            "classify_session_turn"
        );
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
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
                crate::types::ChatMessage {
                    role: "user".to_string(),
                    content: Some("你好".to_string()),
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
            }]),
            tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
        });

        assert_eq!(body["model"], "claude-sonnet-test");
        assert_eq!(body["system"], "系统约束");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "你好");
        assert_eq!(body["tools"][0]["name"], "shell_exec");
        assert_eq!(body["tools"][0]["input_schema"]["required"][0], "cmd");
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "shell_exec");
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
                "gpt-test".to_string(),
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
                // 选用不在 extended thinking 能力清单中的模型名，确保启用完全由配置驱动
                "claude-non-thinking-test".to_string(),
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
            "gpt-test".to_string(),
            HttpModelBridgeProtocol::ChatCompletions,
            Some(ReasoningEffort::Xhigh),
        );
        let (url, body, headers) = client
            .build_probe_request()
            .expect("probe request should build");

        assert_eq!(url, "https://api.example.com/v1/chat/completions");
        assert_eq!(body["model"], "gpt-test");
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
            "claude-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            Some(ReasoningEffort::Xhigh),
        );
        let (url, body, headers) = client
            .build_probe_request()
            .expect("probe request should build");

        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert_eq!(body["model"], "claude-test");
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

    fn spawn_mock_server(status: u16, response_body: serde_json::Value) -> MockHttpServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let address = listener.local_addr().expect("address should exist");
        let (sender, receiver) = mpsc::channel();

        let handle = std::thread::spawn(move || {
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

            let header_text =
                String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
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

            let body = String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
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

            let response_text = response_body.to_string();
            let response = format!(
                "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_text}",
                status_reason(status),
                response_text.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("should write response");
            stream.flush().expect("should flush");
        });

        MockHttpServer {
            address: format!("http://{address}/v1"),
            _handle: handle,
            request_receiver: receiver,
        }
    }

    fn status_reason(status: u16) -> &'static str {
        match status {
            200 => "OK",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
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
            429,
            serde_json::json!({
                "error": {
                    "message": "rate limited",
                    "type": "rate_limit_error",
                    "code": "too_many_requests",
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
            BridgeClientError::CallFailed {
                layer,
                code,
                message,
            } => {
                assert_eq!(layer, BridgeErrorLayer::RemoteBusiness);
                assert_eq!(code, Some(-32006));
                assert!(message.contains("rate limited"), "message was: {message}");
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
