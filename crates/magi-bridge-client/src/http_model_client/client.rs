use super::*;

/// 通过 HTTP 调用已配置的模型提供方。
///
/// HTTP I/O 放在独立线程执行，避免与调用方已经存在的 tokio runtime 冲突。
pub struct HttpModelBridgeClient {
    pub(super) base_url: String,
    pub(super) api_key: Option<String>,
    pub(super) model: String,
    pub(super) protocol: HttpModelBridgeProtocol,
    /// 推理强度配置：构造期注入；调用时透传到协议层 request body。
    /// `None` 表示未配置（保留协议默认行为）。
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    pub(super) reasoning_capability: Option<crate::reasoning::ReasoningCapability>,
    pub(super) temperature: Option<f64>,
    pub(super) stop: Vec<String>,
}

#[derive(Clone)]
pub(super) struct HttpModelRequest {
    pub(super) url: String,
    pub(super) body: serde_json::Value,
    pub(super) headers: Vec<(String, String)>,
    pub(super) tool_name_codec: ProviderToolNameCodec,
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
    pub(super) fn chat_completions_url(&self) -> Result<String, BridgeClientError> {
        build_protocol_endpoint_url(&self.base_url, "/v1/chat/completions").map_err(|reason| {
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: None,
                message: format!("invalid base_url: {reason}"),
            }
        })
    }

    pub(super) fn provider_family(&self) -> ProviderFamily {
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

    pub(super) fn request_headers(&self) -> Vec<(String, String)> {
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
    pub(super) fn build_http_request(
        &self,
        request: &ModelInvocationRequest,
        stream: bool,
    ) -> Result<HttpModelRequest, BridgeClientError> {
        self.build_http_request_with_output_limit(request, stream, None)
    }

    pub(super) fn build_http_request_with_output_limit(
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
    pub(super) fn prepare_prompt_ir(
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

    pub(super) fn adapt_request(
        &self,
        params: &PromptIr,
    ) -> Result<AdaptedRequest, BridgeClientError> {
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
    pub(super) fn build_request_body(&self, request: &ModelInvocationRequest) -> serde_json::Value {
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

    pub(super) fn provider_request_key(&self) -> String {
        format!(
            "{protocol}|{base_url}|{model}",
            protocol = self.protocol.label(),
            base_url = normalize_provider_base_url(&self.base_url),
            model = self.model.trim(),
        )
    }

    pub(super) fn forced_tool_choice_capability(&self) -> ForcedToolChoiceCapability {
        forced_tool_choice_capability(&self.provider_request_key())
    }

    pub(super) fn mark_forced_tool_choice_capability(
        &self,
        capability: ForcedToolChoiceCapability,
    ) {
        record_forced_tool_choice_capability(self.provider_request_key(), capability);
    }

    pub(super) fn invoke_streaming_observed(
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
    pub(super) fn invoke_cancellable_with_output_limit(
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
    pub(super) fn parse_success_payload(
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
