use crate::{
    BridgeResponse, ModelInvocationRequest, SHADOW_MODEL_PROVIDER,
    local_process_protocol::{
        BridgeServerKind, BridgeServerServiceCatalog, BridgeServerServiceDescriptor,
        LOCAL_BRIDGE_PROTOCOL_VERSION, LocalProcessBridgeRequest, LocalProcessBridgeRpcError,
        LocalProcessBridgeServerError, run_local_process_bridge_server,
    },
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::{env, fmt, process::Command, sync::Arc};
use thiserror::Error;

const OPENAI_COMPAT_PROVIDER: &str = "openai-compatible";
const OPENAI_PROVIDER_ALIAS: &str = "openai";
const OPENAI_BASE_URL_ENV: &str = "MAGI_OPENAI_COMPAT_BASE_URL";
const OPENAI_API_KEY_ENV: &str = "MAGI_OPENAI_COMPAT_API_KEY";
const OPENAI_MODEL_ENV: &str = "MAGI_OPENAI_COMPAT_MODEL";
const OPENAI_HTTP_EXECUTABLE: &str = "curl";
const OPENAI_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
const OPENAI_PROVIDER_UNAVAILABLE_CODE: i64 = -32003;
const OPENAI_PROVIDER_MISCONFIGURED_CODE: i64 = -32004;
const OPENAI_PROVIDER_TRANSPORT_CODE: i64 = -32005;
const OPENAI_PROVIDER_REJECTED_CODE: i64 = -32006;
const OPENAI_PROVIDER_INVALID_RESPONSE_CODE: i64 = -32007;

pub fn run_model_bridge_loopback_server() -> Result<(), LocalProcessBridgeServerError> {
    let shim = ModelServiceShim::from_env();
    run_local_process_bridge_server(
        BridgeServerKind::Model,
        "model.invoke",
        shim.service_catalog(),
        handle_model_invoke,
    )
}

#[derive(Clone)]
struct ModelServiceShim {
    registry: ModelProviderRegistry,
    http_executor: Arc<dyn OpenAiCompatibleHttpExecutor>,
}

impl fmt::Debug for ModelServiceShim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelServiceShim")
            .field("registry", &self.registry)
            .field("http_executor", &"dyn OpenAiCompatibleHttpExecutor")
            .finish()
    }
}

impl ModelServiceShim {
    fn from_env() -> Self {
        Self {
            registry: ModelProviderRegistry::from_env(),
            http_executor: Arc::new(CurlOpenAiCompatibleHttpExecutor),
        }
    }

    fn service_catalog(&self) -> BridgeServerServiceCatalog {
        BridgeServerServiceCatalog {
            protocol_version: LOCAL_BRIDGE_PROTOCOL_VERSION.to_string(),
            server_kind: BridgeServerKind::Model,
            services: self.registry.service_descriptors(),
        }
    }

    fn handle(
        &self,
        request: LocalProcessBridgeRequest,
    ) -> Result<serde_json::Value, LocalProcessBridgeRpcError> {
        let _request_id = request.id;
        let invocation: ModelInvocationRequest = match serde_json::from_value(request.params) {
            Ok(request) => request,
            Err(error) => {
                return Err(LocalProcessBridgeRpcError::invalid_params(
                    error.to_string(),
                ));
            }
        };
        let response = self.invoke(invocation)?;
        serde_json::to_value(response).map_err(|error| {
            LocalProcessBridgeRpcError::invalid_params(format!(
                "serialize model bridge response failed: {error}"
            ))
        })
    }

    fn invoke(
        &self,
        invocation: ModelInvocationRequest,
    ) -> Result<BridgeResponse, LocalProcessBridgeRpcError> {
        if invocation.prompt.trim().is_empty() {
            return Err(LocalProcessBridgeRpcError::remote_business(
                -32002,
                "empty prompt",
                None,
            ));
        }

        let provider = self
            .registry
            .resolve(invocation.provider.trim())
            .ok_or_else(|| {
                LocalProcessBridgeRpcError::remote_business(
                    -32001,
                    "unknown provider",
                    Some(json!({
                        "provider": invocation.provider,
                        "supported_providers": self.registry.provider_names(),
                    })),
                )
            })?;

        provider.invoke(&invocation.prompt, self.http_executor.as_ref())
    }
}

fn shadow_loopback_visible_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some(decomposition) = shadow_loopback_decomposition_response(trimmed) {
        return decomposition;
    }

    if let Some((_, task_prompt)) = trimmed.rsplit_once("--- Task ---") {
        let task_prompt = task_prompt.trim();
        if !task_prompt.is_empty() {
            return task_prompt.to_string();
        }
    }

    let chunks = trimmed
        .split("\n\n")
        .map(str::trim)
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>();

    if let Some(index) = chunks
        .iter()
        .rposition(|chunk| chunk.starts_with("执行:") || chunk.starts_with("继续:"))
    {
        return chunks[index..].join("\n\n");
    }

    if let Some(index) = chunks
        .iter()
        .position(|chunk| !shadow_loopback_instruction_chunk(chunk))
    {
        if index > 0 {
            return chunks[index..].join("\n\n");
        }
    }

    trimmed.to_string()
}

fn shadow_loopback_decomposition_response(prompt: &str) -> Option<String> {
    if !prompt.starts_with("请将以下任务分解为 2-5 个具体的子任务。") {
        return None;
    }
    let _task_text = prompt
        .rsplit_once("\n\n任务：")
        .map(|(_, task)| task.trim())
        .filter(|task| !task.is_empty())?;
    Some("分析目标与约束\n制定执行步骤\n汇总执行结果".to_string())
}

fn shadow_loopback_instruction_chunk(chunk: &str) -> bool {
    chunk.starts_with("--- 用户规则 ---")
        || chunk.starts_with("--- 安全防护 ---")
        || chunk.starts_with("--- Context ---")
}

fn handle_model_invoke(
    request: LocalProcessBridgeRequest,
) -> Result<serde_json::Value, LocalProcessBridgeRpcError> {
    ModelServiceShim::from_env().handle(request)
}

#[derive(Clone, Debug)]
struct ModelProviderRegistry {
    providers: Vec<ModelProvider>,
}

impl ModelProviderRegistry {
    fn from_env() -> Self {
        Self {
            providers: vec![
                ModelProvider::shadow(),
                ModelProvider::openai_compatible(ModelProviderRuntimeConfig::from_env()),
            ],
        }
    }

    fn service_descriptors(&self) -> Vec<BridgeServerServiceDescriptor> {
        self.providers
            .iter()
            .map(ModelProvider::service_descriptor)
            .collect()
    }

    fn resolve(&self, provider_name: &str) -> Option<&ModelProvider> {
        self.providers
            .iter()
            .find(|provider| provider.matches(provider_name))
    }

    fn provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|provider| provider.name.to_string())
            .collect()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SecretString(String);

impl SecretString {
    fn new(value: String) -> Self {
        Self(value)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(REDACTED)")
    }
}

#[derive(Clone, Debug)]
struct ModelProviderRuntimeConfig {
    base_url: Option<String>,
    api_key: Option<SecretString>,
    default_model: Option<String>,
}

impl ModelProviderRuntimeConfig {
    fn from_env() -> Self {
        Self {
            base_url: read_non_empty_env(OPENAI_BASE_URL_ENV),
            api_key: read_non_empty_env(OPENAI_API_KEY_ENV).map(SecretString::new),
            default_model: read_non_empty_env(OPENAI_MODEL_ENV),
        }
    }
}

#[derive(Clone, Debug)]
enum ModelProviderMode {
    ShadowLoopback,
    OpenAiCompatibleHttp(OpenAiCompatibleProviderRuntime),
}

#[derive(Clone, Debug)]
struct OpenAiCompatibleProviderRuntime {
    base_url: Option<String>,
    api_key: Option<SecretString>,
    default_model: Option<String>,
}

impl OpenAiCompatibleProviderRuntime {
    fn from_config(config: ModelProviderRuntimeConfig) -> Self {
        Self {
            base_url: config.base_url,
            api_key: config.api_key,
            default_model: config.default_model,
        }
    }

    fn missing_config_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.base_url.is_none() {
            keys.push(OPENAI_BASE_URL_ENV);
        }
        if self.api_key.is_none() {
            keys.push(OPENAI_API_KEY_ENV);
        }
        if self.default_model.is_none() {
            keys.push(OPENAI_MODEL_ENV);
        }
        keys
    }

    fn health_issues(&self) -> Vec<String> {
        let mut issues = self
            .missing_config_keys()
            .into_iter()
            .map(|key| format!("missing {key}"))
            .collect::<Vec<_>>();
        if let Some(base_url) = self.base_url.as_deref() {
            if let Err(reason) = build_openai_chat_completions_url(base_url) {
                issues.push(format!("invalid {OPENAI_BASE_URL_ENV}: {reason}"));
            }
        }
        issues
    }

    fn default_model_capability(&self) -> String {
        match self.default_model.as_deref() {
            Some(model) => format!("default_model:{model}"),
            None => "default_model:missing".to_string(),
        }
    }

    fn invoke(
        &self,
        provider: &'static str,
        service_health: Option<&'static str>,
        service_health_reason: Option<&str>,
        prompt: &str,
        executor: &dyn OpenAiCompatibleHttpExecutor,
    ) -> Result<BridgeResponse, LocalProcessBridgeRpcError> {
        let missing = self.missing_config_keys();
        if !missing.is_empty() {
            return Err(openai_provider_unavailable_error(
                provider,
                service_health,
                service_health_reason,
                &missing,
            ));
        }

        let request = self.build_request(provider, prompt)?;
        let api_key = self
            .api_key
            .as_ref()
            .expect("api key presence already checked")
            .expose();
        let response = executor
            .execute(&request, api_key)
            .map_err(|error| map_openai_executor_error(provider, &request, error))?;

        parse_openai_response(provider, &request, response)
    }

    fn build_request(
        &self,
        provider: &'static str,
        prompt: &str,
    ) -> Result<OpenAiCompatibleHttpRequest, LocalProcessBridgeRpcError> {
        let base_url = self
            .base_url
            .as_deref()
            .expect("base_url presence already checked");
        let model = self
            .default_model
            .as_ref()
            .expect("model presence already checked")
            .clone();
        let url = build_openai_chat_completions_url(base_url)
            .map_err(|reason| openai_provider_misconfigured_error(provider, base_url, &reason))?;
        let body = serde_json::to_string(&json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": prompt,
            }],
            "stream": false,
        }))
        .expect("openai-compatible request body should serialize");

        Ok(OpenAiCompatibleHttpRequest { url, model, body })
    }
}

#[derive(Clone, Debug)]
struct ModelProvider {
    name: &'static str,
    aliases: Vec<&'static str>,
    implementation_source: &'static str,
    capability_profile: &'static str,
    service_health: Option<&'static str>,
    service_health_reason: Option<String>,
    mode: ModelProviderMode,
}

impl ModelProvider {
    fn shadow() -> Self {
        Self {
            name: SHADOW_MODEL_PROVIDER,
            aliases: Vec::new(),
            implementation_source: "shadow-loopback",
            capability_profile: "model-invoke-v1",
            service_health: None,
            service_health_reason: None,
            mode: ModelProviderMode::ShadowLoopback,
        }
    }

    fn openai_compatible(config: ModelProviderRuntimeConfig) -> Self {
        let runtime = OpenAiCompatibleProviderRuntime::from_config(config);
        let issues = runtime.health_issues();
        let ready = issues.is_empty();

        Self {
            name: OPENAI_COMPAT_PROVIDER,
            aliases: vec![OPENAI_PROVIDER_ALIAS],
            implementation_source: "provider-http-smoke",
            capability_profile: "openai-compatible-chat-completions-v1",
            service_health: Some(if ready { "ready" } else { "degraded" }),
            service_health_reason: (!ready).then(|| issues.join("; ")),
            mode: ModelProviderMode::OpenAiCompatibleHttp(runtime),
        }
    }

    fn matches(&self, provider_name: &str) -> bool {
        provider_name == self.name || self.aliases.iter().any(|alias| provider_name == *alias)
    }

    fn service_descriptor(&self) -> BridgeServerServiceDescriptor {
        let mut capabilities = vec![
            format!("provider:{}", self.name),
            "prompt:required".to_string(),
            "response:bridge_response".to_string(),
            format!("implementation_source:{}", self.implementation_source),
            format!("capability_profile:{}", self.capability_profile),
        ];
        if !self.aliases.is_empty() {
            capabilities.push(format!("provider_aliases:{}", self.aliases.join(",")));
        }
        match &self.mode {
            ModelProviderMode::ShadowLoopback => {}
            ModelProviderMode::OpenAiCompatibleHttp(runtime) => {
                capabilities.push("provider_mode:http-smoke".to_string());
                capabilities.push("request_transport:curl-http".to_string());
                capabilities.push("request_path:chat_completions".to_string());
                capabilities.push(format!(
                    "base_url:{}",
                    if runtime.base_url.is_some() {
                        "configured"
                    } else {
                        "missing"
                    }
                ));
                capabilities.push(format!(
                    "api_key:{}",
                    if runtime.api_key.is_some() {
                        "configured"
                    } else {
                        "missing"
                    }
                ));
                capabilities.push(runtime.default_model_capability());
            }
        }

        BridgeServerServiceDescriptor {
            service_name: self.name.to_string(),
            shim_kind: format!("{}-shim", self.name),
            supported_operations: vec!["invoke_prompt".to_string()],
            capabilities,
            service_health: self.service_health.map(str::to_string),
            service_health_reason: self.service_health_reason.clone(),
            implementation_source: Some(self.implementation_source.to_string()),
            capability_profile: Some(self.capability_profile.to_string()),
            workspace_roots_source: None,
            manager_version: None,
            registry_profile: None,
            registry_manifest: None,
            selection_strategy: None,
            default_server: None,
            default_server_health: None,
            default_server_selection_key: None,
            default_route_status: None,
            default_route_target: None,
            selection_targets: None,
            selection_key: None,
            server_manifest: None,
            shell_manifest: None,
            shell_profile: None,
            command_capability_profiles: None,
            session_descriptor: None,
            workspace_context: None,
            context_resolution_boundary: None,
        }
    }

    fn invoke(
        &self,
        prompt: &str,
        executor: &dyn OpenAiCompatibleHttpExecutor,
    ) -> Result<BridgeResponse, LocalProcessBridgeRpcError> {
        match &self.mode {
            ModelProviderMode::ShadowLoopback => Ok(BridgeResponse {
                ok: true,
                payload: format!("shadow-model::{}", shadow_loopback_visible_prompt(prompt)),
            }),
            ModelProviderMode::OpenAiCompatibleHttp(runtime) => runtime.invoke(
                self.name,
                self.service_health,
                self.service_health_reason.as_deref(),
                prompt,
                executor,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenAiCompatibleHttpRequest {
    url: String,
    model: String,
    body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenAiCompatibleHttpResponse {
    status: u16,
    body: String,
}

trait OpenAiCompatibleHttpExecutor: Send + Sync {
    fn execute(
        &self,
        request: &OpenAiCompatibleHttpRequest,
        api_key: &str,
    ) -> Result<OpenAiCompatibleHttpResponse, OpenAiCompatibleHttpExecutorError>;
}

#[derive(Debug)]
struct CurlOpenAiCompatibleHttpExecutor;

impl OpenAiCompatibleHttpExecutor for CurlOpenAiCompatibleHttpExecutor {
    fn execute(
        &self,
        request: &OpenAiCompatibleHttpRequest,
        api_key: &str,
    ) -> Result<OpenAiCompatibleHttpResponse, OpenAiCompatibleHttpExecutorError> {
        let output = Command::new(OPENAI_HTTP_EXECUTABLE)
            .arg("-sS")
            .arg("-L")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("30")
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Accept: application/json")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-H")
            .arg(format!("Authorization: Bearer {api_key}"))
            .arg("-d")
            .arg(&request.body)
            .arg(&request.url)
            .arg("-w")
            .arg("\n__MAGI_STATUS__:%{http_code}")
            .output()
            .map_err(|error| OpenAiCompatibleHttpExecutorError::Transport {
                message: format!("spawn {OPENAI_HTTP_EXECUTABLE} failed: {error}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let mut details = stderr;
            if details.is_empty() {
                details = stdout;
            } else if !stdout.is_empty() {
                details.push_str("; stdout=");
                details.push_str(&stdout);
            }
            if details.is_empty() {
                details = format!(
                    "{OPENAI_HTTP_EXECUTABLE} exited with status {:?}",
                    output.status.code()
                );
            }
            return Err(OpenAiCompatibleHttpExecutorError::Transport { message: details });
        }

        let stdout = String::from_utf8(output.stdout).map_err(|error| {
            OpenAiCompatibleHttpExecutorError::Protocol {
                message: format!("provider response was not utf-8: {error}"),
            }
        })?;
        let (body, status_text) = stdout.rsplit_once("\n__MAGI_STATUS__:").ok_or_else(|| {
            OpenAiCompatibleHttpExecutorError::Protocol {
                message: "missing HTTP status footer in curl output".to_string(),
            }
        })?;
        let status = status_text.trim().parse::<u16>().map_err(|error| {
            OpenAiCompatibleHttpExecutorError::Protocol {
                message: format!("invalid HTTP status footer: {error}"),
            }
        })?;

        Ok(OpenAiCompatibleHttpResponse {
            status,
            body: body.to_string(),
        })
    }
}

#[derive(Debug, Error)]
enum OpenAiCompatibleHttpExecutorError {
    #[error("{message}")]
    Transport { message: String },
    #[error("{message}")]
    Protocol { message: String },
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<Value>,
    #[serde(default)]
    completion_tokens_details: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    function: OpenAiCompatibleToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OpenAiCompatibleToolFunction {
    name: String,
    #[serde(deserialize_with = "deserialize_openai_tool_arguments")]
    arguments: String,
}

fn deserialize_openai_tool_arguments<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawOpenAiToolArguments {
        String(String),
        Json(Value),
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
    r#type: Option<String>,
    #[serde(default)]
    code: Option<serde_json::Value>,
    #[serde(default)]
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

    Ok(format!("{normalized}{OPENAI_CHAT_COMPLETIONS_PATH}"))
}

fn parse_openai_response(
    provider: &str,
    request: &OpenAiCompatibleHttpRequest,
    response: OpenAiCompatibleHttpResponse,
) -> Result<BridgeResponse, LocalProcessBridgeRpcError> {
    if !(200..300).contains(&response.status) {
        return Err(openai_provider_rejected_error(
            provider,
            request,
            response.status,
            &response.body,
        ));
    }

    let OpenAiCompatibleChatCompletionEnvelope { usage, choices } =
        serde_json::from_str(&response.body).map_err(|error| {
            openai_provider_invalid_response_error(
                provider,
                request,
                response.status,
                format!("decode chat completion failed: {error}"),
                &response.body,
            )
        })?;

    let payload = select_openai_bridge_payload(choices, usage).map_err(|reason| {
        openai_provider_invalid_response_error(
            provider,
            request,
            response.status,
            reason,
            &response.body,
        )
    })?;

    Ok(BridgeResponse { ok: true, payload })
}

fn map_openai_executor_error(
    provider: &str,
    request: &OpenAiCompatibleHttpRequest,
    error: OpenAiCompatibleHttpExecutorError,
) -> LocalProcessBridgeRpcError {
    match error {
        OpenAiCompatibleHttpExecutorError::Transport { message } => {
            LocalProcessBridgeRpcError::remote_business(
                OPENAI_PROVIDER_TRANSPORT_CODE,
                "provider transport failed",
                Some(json!({
                    "provider": provider,
                    "endpoint": request.url,
                    "model": request.model,
                    "reason": message,
                })),
            )
        }
        OpenAiCompatibleHttpExecutorError::Protocol { message } => {
            LocalProcessBridgeRpcError::remote_business(
                OPENAI_PROVIDER_INVALID_RESPONSE_CODE,
                "provider response invalid",
                Some(json!({
                    "provider": provider,
                    "endpoint": request.url,
                    "model": request.model,
                    "reason": message,
                })),
            )
        }
    }
}

fn openai_provider_unavailable_error(
    provider: &str,
    service_health: Option<&str>,
    service_health_reason: Option<&str>,
    missing: &[&str],
) -> LocalProcessBridgeRpcError {
    LocalProcessBridgeRpcError::remote_business(
        OPENAI_PROVIDER_UNAVAILABLE_CODE,
        "provider unavailable",
        Some(json!({
            "provider": provider,
            "service_health": service_health,
            "service_health_reason": service_health_reason,
            "missing_config": missing,
        })),
    )
}

fn openai_provider_misconfigured_error(
    provider: &str,
    base_url: &str,
    reason: &str,
) -> LocalProcessBridgeRpcError {
    LocalProcessBridgeRpcError::remote_business(
        OPENAI_PROVIDER_MISCONFIGURED_CODE,
        "provider misconfigured",
        Some(json!({
            "provider": provider,
            "base_url": base_url,
            "reason": reason,
        })),
    )
}

fn openai_provider_rejected_error(
    provider: &str,
    request: &OpenAiCompatibleHttpRequest,
    status: u16,
    body: &str,
) -> LocalProcessBridgeRpcError {
    if let Ok(error) = serde_json::from_str::<OpenAiCompatibleErrorEnvelope>(body) {
        return LocalProcessBridgeRpcError::remote_business(
            OPENAI_PROVIDER_REJECTED_CODE,
            "provider rejected request",
            Some(json!({
                "provider": provider,
                "endpoint": request.url,
                "model": request.model,
                "http_status": status,
                "upstream_message": error.error.message,
                "upstream_type": error.error.r#type,
                "upstream_code": error.error.code,
                "upstream_param": error.error.param,
            })),
        );
    }

    LocalProcessBridgeRpcError::remote_business(
        OPENAI_PROVIDER_REJECTED_CODE,
        "provider rejected request",
        Some(json!({
            "provider": provider,
            "endpoint": request.url,
            "model": request.model,
            "http_status": status,
            "response_body": truncate_error_body(body),
        })),
    )
}

fn openai_provider_invalid_response_error(
    provider: &str,
    request: &OpenAiCompatibleHttpRequest,
    status: u16,
    reason: String,
    body: &str,
) -> LocalProcessBridgeRpcError {
    LocalProcessBridgeRpcError::remote_business(
        OPENAI_PROVIDER_INVALID_RESPONSE_CODE,
        "provider response invalid",
        Some(json!({
            "provider": provider,
            "endpoint": request.url,
            "model": request.model,
            "http_status": status,
            "reason": reason,
            "response_body": truncate_error_body(body),
        })),
    )
}

fn truncate_error_body(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    const MAX_CHARS: usize = 512;
    let mut collected = trimmed.chars().take(MAX_CHARS).collect::<String>();
    if trimmed.chars().count() > MAX_CHARS {
        collected.push_str("...");
    }
    Some(collected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct RecordingExecutor {
        requests: Mutex<Vec<OpenAiCompatibleHttpRequest>>,
        response: OpenAiCompatibleHttpResponse,
    }

    impl RecordingExecutor {
        fn new(response: OpenAiCompatibleHttpResponse) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response,
            }
        }

        fn requests(&self) -> Vec<OpenAiCompatibleHttpRequest> {
            self.requests.lock().expect("lock poisoned").clone()
        }
    }

    impl OpenAiCompatibleHttpExecutor for RecordingExecutor {
        fn execute(
            &self,
            request: &OpenAiCompatibleHttpRequest,
            _api_key: &str,
        ) -> Result<OpenAiCompatibleHttpResponse, OpenAiCompatibleHttpExecutorError> {
            self.requests
                .lock()
                .expect("lock poisoned")
                .push(request.clone());
            Ok(self.response.clone())
        }
    }

    #[derive(Debug)]
    struct FailingExecutor {
        message: &'static str,
    }

    impl OpenAiCompatibleHttpExecutor for FailingExecutor {
        fn execute(
            &self,
            _request: &OpenAiCompatibleHttpRequest,
            _api_key: &str,
        ) -> Result<OpenAiCompatibleHttpResponse, OpenAiCompatibleHttpExecutorError> {
            Err(OpenAiCompatibleHttpExecutorError::Transport {
                message: self.message.to_string(),
            })
        }
    }

    fn test_openai_shim(http_executor: Arc<dyn OpenAiCompatibleHttpExecutor>) -> ModelServiceShim {
        ModelServiceShim {
            registry: ModelProviderRegistry {
                providers: vec![
                    ModelProvider::shadow(),
                    ModelProvider::openai_compatible(ModelProviderRuntimeConfig {
                        base_url: Some("https://api.example.com/v1".to_string()),
                        api_key: Some(SecretString::new("test-key".to_string())),
                        default_model: Some("gpt-4.1-mini".to_string()),
                    }),
                ],
            },
            http_executor,
        }
    }

    #[test]
    fn model_handler_returns_bridge_response_payload() {
        let result = super::handle_model_invoke(LocalProcessBridgeRequest {
            id: Value::from(1),
            params: serde_json::json!({
                "provider": SHADOW_MODEL_PROVIDER,
                "prompt": "hello",
            }),
        })
        .expect("model invoke should serialize");
        let response: BridgeResponse =
            serde_json::from_value(result).expect("bridge response should decode");
        assert_eq!(response.payload, "shadow-model::hello");
    }

    #[test]
    fn shadow_loopback_visible_prompt_strips_prefixed_session_instructions() {
        let prompt = r#"--- 用户规则 ---
请始终简洁回答

--- 安全防护 ---
命中危险操作前先确认

执行: 整理任务输出

整理任务输出"#;

        assert_eq!(
            super::shadow_loopback_visible_prompt(prompt),
            "执行: 整理任务输出\n\n整理任务输出"
        );
    }

    #[test]
    fn shadow_loopback_visible_prompt_prefers_explicit_task_section() {
        let prompt = r#"--- Context ---
[knowledge] foo: bar

--- Task ---
执行: 汇总结果

汇总结果"#;

        assert_eq!(
            super::shadow_loopback_visible_prompt(prompt),
            "执行: 汇总结果\n\n汇总结果"
        );
    }

    #[test]
    fn shadow_loopback_visible_prompt_returns_clean_decomposition_lines() {
        let prompt = "请将以下任务分解为 2-5 个具体的子任务。每行一个子任务标题，不要编号，不要额外说明。\n\n任务：请分析并拆分这个复杂任务";

        assert_eq!(
            super::shadow_loopback_visible_prompt(prompt),
            "分析目标与约束\n制定执行步骤\n汇总执行结果"
        );
    }

    #[test]
    fn model_service_catalog_exposes_shadow_and_openai_compatible_provider_capability() {
        let catalog = ModelServiceShim::from_env().service_catalog();
        assert_eq!(catalog.server_kind, BridgeServerKind::Model);
        assert_eq!(catalog.services.len(), 2);
        assert!(catalog.services.iter().any(|service| {
            service
                .capabilities
                .contains(&"provider:shadow-model".to_string())
        }));
        let openai_service = catalog
            .services
            .iter()
            .find(|service| service.service_name == OPENAI_COMPAT_PROVIDER)
            .expect("openai-compatible service should exist");
        assert!(
            openai_service
                .capabilities
                .contains(&"provider_mode:http-smoke".to_string())
        );
        assert_eq!(
            openai_service.implementation_source.as_deref(),
            Some("provider-http-smoke")
        );
        assert_eq!(
            openai_service.capability_profile.as_deref(),
            Some("openai-compatible-chat-completions-v1")
        );
    }

    #[test]
    fn unknown_provider_returns_remote_business_error() {
        let error = super::handle_model_invoke(LocalProcessBridgeRequest {
            id: Value::from(1),
            params: serde_json::json!({
                "provider": "anthropic",
                "prompt": "hello",
            }),
        })
        .expect_err("unknown provider should remain remote business error");

        assert_eq!(error.code(), -32001);
    }

    #[test]
    fn empty_prompt_returns_remote_business_error() {
        let error = super::handle_model_invoke(LocalProcessBridgeRequest {
            id: Value::from(1),
            params: serde_json::json!({
                "provider": SHADOW_MODEL_PROVIDER,
                "prompt": "   ",
            }),
        })
        .expect_err("empty prompt should return remote business error");

        assert_eq!(error.code(), -32002);
    }

    #[test]
    fn openai_alias_maps_to_openai_compatible_provider_and_builds_request() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "hello from provider",
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor.clone());

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_PROVIDER_ALIAS.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("openai alias should resolve through HTTP smoke path");

        assert_eq!(response.payload, "hello from provider");

        let requests = executor.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(requests[0].model, "gpt-4.1-mini");

        let body: Value =
            serde_json::from_str(&requests[0].body).expect("request body should be json");
        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn openai_compatible_provider_reports_unavailable_without_config() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: "{}".to_string(),
        }));
        let shim = ModelServiceShim {
            registry: ModelProviderRegistry {
                providers: vec![
                    ModelProvider::shadow(),
                    ModelProvider::openai_compatible(ModelProviderRuntimeConfig {
                        base_url: None,
                        api_key: None,
                        default_model: None,
                    }),
                ],
            },
            http_executor: executor.clone(),
        };

        let error = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect_err("provider should stay unavailable without config");

        assert_eq!(error.code(), OPENAI_PROVIDER_UNAVAILABLE_CODE);
        assert!(executor.requests().is_empty());
    }

    #[test]
    fn openai_compatible_provider_rejected_response_preserves_upstream_details() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 429,
            body: serde_json::json!({
                "error": {
                    "message": "rate limited",
                    "type": "rate_limit_error",
                    "code": "too_many_requests",
                }
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let error = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect_err("upstream rejection should remain remote business error");

        assert_eq!(error.code(), OPENAI_PROVIDER_REJECTED_CODE);
        assert_eq!(error.message(), "provider rejected request");
        let data = error.data().expect("error data should exist");
        assert_eq!(data["http_status"], 429);
        assert_eq!(data["upstream_message"], "rate limited");
        assert_eq!(data["upstream_type"], "rate_limit_error");
        assert_eq!(data["upstream_code"], "too_many_requests");
    }

    #[test]
    fn openai_compatible_provider_transport_errors_are_mapped() {
        let shim = test_openai_shim(Arc::new(FailingExecutor {
            message: "connection refused",
        }));

        let error = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect_err("transport failures should stay remote business errors");

        assert_eq!(error.code(), OPENAI_PROVIDER_TRANSPORT_CODE);
        assert_eq!(error.message(), "provider transport failed");
        let data = error.data().expect("error data should exist");
        assert_eq!(data["reason"], "connection refused");
    }

    #[test]
    fn openai_compatible_provider_serializes_usage_finish_reason_and_tool_calls() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "usage": {
                    "prompt_tokens": 9,
                    "completion_tokens": 4,
                    "total_tokens": 13,
                    "prompt_tokens_details": {
                        "cached_tokens": 2,
                    },
                    "completion_tokens_details": {
                        "reasoning_tokens": 1,
                    }
                },
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": [{
                            "type": "text",
                            "text": "checking weather",
                        }],
                        "tool_calls": [{
                            "id": "call_weather_1",
                            "type": "function",
                            "function": {
                                "name": "weather.lookup",
                                "arguments": "{\"city\":\"Shanghai\"}",
                            }
                        }]
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("structured success payload should decode");

        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "checking weather");
        assert_eq!(payload["finish_reason"], "tool_calls");
        assert_eq!(payload["usage"]["prompt_tokens"], 9);
        assert_eq!(payload["usage"]["completion_tokens"], 4);
        assert_eq!(payload["usage"]["total_tokens"], 13);
        assert_eq!(
            payload["usage"]["prompt_tokens_details"]["cached_tokens"],
            2
        );
        assert_eq!(
            payload["usage"]["completion_tokens_details"]["reasoning_tokens"],
            1
        );
        assert_eq!(payload["tool_calls"][0]["id"], "call_weather_1");
        assert_eq!(payload["tool_calls"][0]["type"], "function");
        assert_eq!(
            payload["tool_calls"][0]["function"]["name"],
            "weather.lookup"
        );
        assert_eq!(
            payload["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Shanghai\"}"
        );
    }

    #[test]
    fn openai_compatible_provider_accepts_tool_call_only_success_payloads() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "usage": {
                    "total_tokens": 7,
                },
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "tool_calls": [{
                            "id": "call_lookup_1",
                            "type": "function",
                            "function": {
                                "name": "lookup",
                                "arguments": "{\"id\":42}",
                            }
                        }]
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("tool-call-only success payload should remain valid");

        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert!(payload.get("content").is_none());
        assert_eq!(payload["finish_reason"], "tool_calls");
        assert_eq!(payload["usage"]["total_tokens"], 7);
        assert_eq!(payload["tool_calls"][0]["function"]["name"], "lookup");
        assert_eq!(
            payload["tool_calls"][0]["function"]["arguments"],
            "{\"id\":42}"
        );
    }

    #[test]
    fn openai_compatible_provider_surfaces_refusal_as_content() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "usage": {
                    "total_tokens": 11,
                },
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "refusal": "I can't help with that request."
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("refusal-only success payload should remain bridgeable");

        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "I can't help with that request.");
        assert_eq!(payload["finish_reason"], "stop");
        assert_eq!(payload["usage"]["total_tokens"], 11);
        assert!(payload.get("tool_calls").is_none());
    }

    #[test]
    fn openai_compatible_provider_prefers_refusal_when_content_is_empty() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "usage": {
                    "total_tokens": 5,
                },
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "content": [{
                            "type": "output_text"
                        }],
                        "refusal": "I can't comply with that request."
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("empty content should fall back to refusal text");

        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        assert_eq!(payload["content"], "I can't comply with that request.");
        assert_eq!(payload["finish_reason"], "stop");
        assert_eq!(payload["usage"]["total_tokens"], 5);
    }

    #[test]
    fn openai_compatible_provider_tolerates_structured_tool_call_arguments() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "tool_calls": [{
                            "id": "call_lookup_object",
                            "type": "function",
                            "function": {
                                "name": "lookup.object",
                                "arguments": {
                                    "topic": "bridge",
                                    "limit": 2
                                },
                            }
                        }, {
                            "id": "call_lookup_array",
                            "type": "function",
                            "function": {
                                "name": "lookup.array",
                                "arguments": ["bridge", 2, true],
                            }
                        }]
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("structured tool arguments should decode");

        let payload: Value =
            serde_json::from_str(&response.payload).expect("payload should be json");
        let object_arguments = payload["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("object arguments should stay serialized as a string");
        let array_arguments = payload["tool_calls"][1]["function"]["arguments"]
            .as_str()
            .expect("array arguments should stay serialized as a string");

        assert_eq!(
            serde_json::from_str::<Value>(object_arguments)
                .expect("object arguments should stay valid json"),
            serde_json::json!({
                "topic": "bridge",
                "limit": 2
            })
        );
        assert_eq!(
            serde_json::from_str::<Value>(array_arguments)
                .expect("array arguments should stay valid json"),
            serde_json::json!(["bridge", 2, true])
        );
    }

    #[test]
    fn openai_compatible_provider_skips_unbridgeable_leading_choices() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "choices": [{
                    "message": {}
                }, {
                    "message": {
                        "content": "hello from fallback choice",
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let response = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect("later bridgeable choice should still succeed");

        assert_eq!(response.payload, "hello from fallback choice");
    }

    #[test]
    fn openai_compatible_provider_reports_when_all_choices_are_unbridgeable() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "choices": [{
                    "message": {}
                }, {
                    "message": {
                        "content": null,
                        "tool_calls": []
                    }
                }]
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let error = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect_err("all-unbridgeable choices should be rejected");

        assert_eq!(error.code(), OPENAI_PROVIDER_INVALID_RESPONSE_CODE);
        let data = error.data().expect("error data should exist");
        assert_eq!(
            data["reason"],
            "no bridgeable choices in response: choices[0]: missing message.content/text or message.tool_calls; choices[1]: missing message.content/text or message.tool_calls"
        );
    }

    #[test]
    fn openai_compatible_provider_invalid_success_payload_is_detected() {
        let executor = Arc::new(RecordingExecutor::new(OpenAiCompatibleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "choices": []
            })
            .to_string(),
        }));
        let shim = test_openai_shim(executor);

        let error = shim
            .invoke(ModelInvocationRequest {
                provider: OPENAI_COMPAT_PROVIDER.to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
            })
            .expect_err("invalid success payload should be rejected");

        assert_eq!(error.code(), OPENAI_PROVIDER_INVALID_RESPONSE_CODE);
        assert_eq!(error.message(), "provider response invalid");
    }
}
