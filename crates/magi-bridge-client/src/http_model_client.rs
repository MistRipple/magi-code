use crate::protocol::{ProviderFamily, SseLineParser, StreamAccumulator, parse_stream_event};
use crate::types::{
    BridgeClientError, BridgeErrorLayer, BridgeResponse, ChatCompletionPayload, ChatToolCall,
    ChatToolFunction, ModelBridgeClient, ModelInvocationRequest, ModelStreamEvent,
};
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::io::Read;
use std::sync::mpsc;

const OPENAI_BASE_URL_ENV: &str = "MAGI_OPENAI_COMPAT_BASE_URL";
const OPENAI_API_KEY_ENV: &str = "MAGI_OPENAI_COMPAT_API_KEY";
const OPENAI_MODEL_ENV: &str = "MAGI_OPENAI_COMPAT_MODEL";
const OPENAI_CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

/// A `ModelBridgeClient` implementation that makes direct HTTP calls to an
/// OpenAI-compatible API endpoint, bypassing the JSON-RPC subprocess loopback.
///
/// HTTP calls are dispatched on a dedicated thread to avoid conflicts with
/// tokio async runtimes that may be active in the calling context.
pub struct HttpModelBridgeClient {
    base_url: String,
    api_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    enable_thinking: Option<bool>,
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

    /// Create with explicit configuration.
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Self {
        Self {
            base_url,
            api_key,
            model,
            reasoning_effort: None,
            enable_thinking: None,
        }
    }

    pub fn with_generation_options(
        mut self,
        reasoning_effort: Option<String>,
        enable_thinking: Option<bool>,
    ) -> Self {
        self.reasoning_effort = reasoning_effort
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.enable_thinking = enable_thinking;
        self
    }

    fn chat_completions_url(&self) -> Result<String, BridgeClientError> {
        build_openai_chat_completions_url(&self.base_url).map_err(|reason| {
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("invalid base_url: {reason}"),
            }
        })
    }

    /// Build the JSON body for a chat completions request.
    pub(crate) fn build_request_body(&self, request: &ModelInvocationRequest) -> serde_json::Value {
        self.build_request_body_with_stream(request, false)
    }

    pub(crate) fn build_request_body_with_stream(
        &self,
        request: &ModelInvocationRequest,
        stream: bool,
    ) -> serde_json::Value {
        let messages = if let Some(ref msgs) = request.messages {
            serde_json::to_value(msgs).unwrap_or_else(|_| json!([]))
        } else {
            json!([{ "role": "user", "content": request.prompt }])
        };
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": stream,
        });
        if let Some(ref reasoning_effort) = self.reasoning_effort {
            body["reasoning_effort"] = json!(reasoning_effort);
        }
        if let Some(enable_thinking) = self.enable_thinking {
            body["enable_thinking"] = json!(enable_thinking);
            body["thinking"] = json!({
                "type": if enable_thinking { "enabled" } else { "disabled" },
            });
            if enable_thinking {
                body["max_tokens"] = json!(32_768);
            }
        }
        if let Some(ref tools) = request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::to_value(tools).unwrap_or_else(|_| json!([]));
                body["tool_choice"] = request
                    .tool_choice
                    .as_ref()
                    .and_then(|choice| serde_json::to_value(choice).ok())
                    .unwrap_or_else(|| json!("auto"));
            }
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
    api_key: Option<String>,
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

        if let Some(ref key) = api_key {
            req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
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

enum StreamThreadEvent {
    Chunk(ModelStreamEvent),
    Finished(Result<BridgeResponse, BridgeClientError>),
}

fn build_transport_error(message: impl Into<String>) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: message.into(),
    }
}

fn build_remote_business_error(message: impl Into<String>) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32006),
        message: message.into(),
    }
}

fn build_protocol_error(message: impl Into<String>) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32007),
        message: message.into(),
    }
}

fn convert_stream_tool_call(tool_call: crate::llm_types::ToolCall) -> ChatToolCall {
    ChatToolCall {
        id: tool_call.id,
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: tool_call.name,
            arguments: tool_call
                .raw_arguments
                .unwrap_or_else(|| tool_call.arguments.to_string()),
        },
    }
}

fn build_stream_bridge_response(
    content: String,
    finish_reason: String,
    tool_calls: Vec<crate::llm_types::ToolCall>,
    usage: crate::llm_types::LlmUsage,
) -> Result<BridgeResponse, BridgeClientError> {
    let payload = ChatCompletionPayload {
        content: (!content.is_empty()).then_some(content),
        finish_reason: (!finish_reason.is_empty()).then_some(finish_reason),
        usage: Some(serde_json::to_value(usage).map_err(|error| {
            build_protocol_error(format!("serialize stream usage failed: {error}"))
        })?),
        tool_calls: tool_calls
            .into_iter()
            .map(convert_stream_tool_call)
            .collect(),
    };
    let payload = serde_json::to_string(&payload).map_err(|error| {
        build_protocol_error(format!("serialize stream payload failed: {error}"))
    })?;
    Ok(BridgeResponse { ok: true, payload })
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

        let url = self.chat_completions_url()?;
        let body = self.build_request_body(&request);

        let (status, response_body) = execute_http_post(url, body, self.api_key.clone())?;
        parse_openai_chat_completion_response(status, &response_body)
    }

    fn invoke_stream(
        &self,
        request: ModelInvocationRequest,
        on_event: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<BridgeResponse, BridgeClientError> {
        if request.prompt.trim().is_empty() && request.messages.is_none() {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32002),
                message: "empty prompt".to_string(),
            });
        }

        let url = self.chat_completions_url()?;
        let body = self.build_request_body_with_stream(&request, true);
        let api_key = self.api_key.clone();
        let (sender, receiver) = mpsc::channel();

        std::thread::spawn(move || {
            let result = execute_openai_stream_request(url, body, api_key, &sender);
            let _ = sender.send(StreamThreadEvent::Finished(result));
        });

        loop {
            match receiver.recv().map_err(|error| {
                build_transport_error(format!("stream channel closed unexpectedly: {error}"))
            })? {
                StreamThreadEvent::Chunk(event) => on_event(event),
                StreamThreadEvent::Finished(result) => return result,
            }
        }
    }
}

fn parse_openai_chat_completion_response(
    status: u16,
    response_body: &str,
) -> Result<BridgeResponse, BridgeClientError> {
    if !(200..300).contains(&status) {
        if let Ok(error_envelope) =
            serde_json::from_str::<OpenAiCompatibleErrorEnvelope>(response_body)
        {
            return Err(build_remote_business_error(format!(
                "provider rejected request: {} (http_status={status})",
                error_envelope.error.message
            )));
        }
        return Err(build_remote_business_error(format!(
            "provider rejected request: http_status={status}, body={}",
            truncate_body(response_body)
        )));
    }

    let envelope: OpenAiCompatibleChatCompletionEnvelope = serde_json::from_str(response_body)
        .map_err(|error| {
            build_protocol_error(format!(
                "provider response invalid: decode chat completion failed: {error}"
            ))
        })?;

    let payload = select_openai_bridge_payload(envelope.choices, envelope.usage)
        .map_err(|reason| build_protocol_error(format!("provider response invalid: {reason}")))?;

    Ok(BridgeResponse { ok: true, payload })
}

fn execute_openai_stream_request(
    url: String,
    body: serde_json::Value,
    api_key: Option<String>,
    sender: &mpsc::Sender<StreamThreadEvent>,
) -> Result<BridgeResponse, BridgeClientError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|error| build_transport_error(format!("HTTP client build failed: {error}")))?;

    let mut req_builder = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream, application/json")
        .json(&body);

    if let Some(ref key) = api_key {
        req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
    }

    let mut response = req_builder
        .send()
        .map_err(|error| build_transport_error(format!("provider transport failed: {error}")))?;

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !content_type.contains("text/event-stream") {
        let response_body = response.text().map_err(|error| {
            build_transport_error(format!("reading response body failed: {error}"))
        })?;
        return parse_openai_chat_completion_response(status, &response_body);
    }

    if !(200..300).contains(&status) {
        let response_body = response.text().map_err(|error| {
            build_transport_error(format!("reading error stream failed: {error}"))
        })?;
        return parse_openai_chat_completion_response(status, &response_body);
    }

    let mut parser = SseLineParser::new();
    let mut accumulator = StreamAccumulator::new();
    let mut chunk = [0_u8; 8192];

    loop {
        let read = response.read(&mut chunk).map_err(|error| {
            build_transport_error(format!("reading event stream failed: {error}"))
        })?;
        if read == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&chunk[..read]);
        let events = parser.feed(&text);
        for event in events {
            let chunks = parse_stream_event(ProviderFamily::OpenAiChat, &event);
            for stream_chunk in &chunks {
                match stream_chunk.kind {
                    crate::llm_types::LlmStreamChunkType::ContentDelta
                    | crate::llm_types::LlmStreamChunkType::ContentStart => {
                        if let Some(delta) = stream_chunk
                            .content
                            .clone()
                            .filter(|delta| !delta.is_empty())
                        {
                            let _ = sender.send(StreamThreadEvent::Chunk(
                                ModelStreamEvent::ContentDelta { delta },
                            ));
                        }
                    }
                    crate::llm_types::LlmStreamChunkType::Thinking => {
                        if let Some(delta) = stream_chunk
                            .thinking
                            .clone()
                            .filter(|delta| !delta.is_empty())
                        {
                            let _ = sender.send(StreamThreadEvent::Chunk(
                                ModelStreamEvent::ThinkingDelta { delta },
                            ));
                        }
                    }
                    _ => {}
                }
            }
            accumulator.apply_all(&chunks);
        }
    }

    let adapted = accumulator.finalize();
    build_stream_bridge_response(
        adapted.content,
        adapted.stop_reason,
        adapted.tool_calls,
        adapted.usage,
    )
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
        let (content, tool_calls) = match self.message {
            Some(message) => (
                message
                    .content
                    .map(OpenAiCompatibleMessageContent::into_text)
                    .filter(|content| !content.trim().is_empty())
                    .or(message.reasoning_content.filter(|s| !s.trim().is_empty()))
                    .or(message.refusal),
                message.tool_calls,
            ),
            None => (None, Vec::new()),
        };

        OpenAiCompatibleSuccessPayload {
            content: content.or(self.text),
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
    finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<OpenAiCompatibleUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiCompatibleToolCall>,
}

impl OpenAiCompatibleSuccessPayload {
    fn into_bridge_payload(self) -> Result<String, String> {
        if self.content.is_none() && self.tool_calls.is_empty() {
            return Err("missing message.content/text or message.tool_calls".to_string());
        }

        if self.finish_reason.is_none() && self.usage.is_none() && self.tool_calls.is_empty() {
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

fn build_openai_chat_completions_url(base_url: &str) -> Result<String, String> {
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return Err("base_url must not be empty".to_string());
    }
    if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
        return Err("base_url must start with http:// or https://".to_string());
    }
    if normalized.ends_with(OPENAI_CHAT_COMPLETIONS_PATH) {
        return Ok(normalized.to_string());
    }
    if normalized.ends_with("/chat/completions") {
        return Ok(normalized.to_string());
    }
    if normalized.ends_with("/v1") {
        return Ok(format!("{normalized}/chat/completions"));
    }

    Ok(format!("{normalized}{OPENAI_CHAT_COMPLETIONS_PATH}"))
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
    fn build_request_body_includes_thinking_options_when_enabled() {
        let client = HttpModelBridgeClient::new(
            "https://api.example.com/v1".to_string(),
            None,
            "reasoning-model".to_string(),
        )
        .with_generation_options(Some("medium".to_string()), Some(true));

        let body = client.build_request_body(&ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "hello world".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        });

        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["max_tokens"], 32_768);
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
