use super::*;

/// 流式 HTTP 线程发回的消息类型。
pub(super) enum StreamMessage {
    Observation(ModelTransportEvent),
    /// LLM 增量快照——携带已累积的正文与上游 thinking。
    Chunk(ModelStreamingDelta),
    /// HTTP I/O 结束——携带最终结果。
    Done(StreamingHttpResult),
}

pub(super) fn provider_stream_event_error(
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
pub(super) struct BodyStreamTiming {
    pub(super) first: Option<Duration>,
    pub(super) last: Option<Duration>,
    pub(super) response_received_at: Duration,
    pub(super) usage_output_tokens: Option<u64>,
    pub(super) reasoning_tokens: Option<u64>,
    pub(super) non_text_output: bool,
    pub(super) usage_observed: bool,
}

#[derive(Default)]
pub(super) struct StreamDeltaProgress {
    pub(super) content_len: usize,
    pub(super) thinking_len: usize,
    pub(super) tool_call_count: usize,
}

impl BodyStreamTiming {
    pub(super) fn observe(
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

    pub(super) fn finish(
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

pub(super) fn apply_provider_stream_event(
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

pub(super) fn is_responses_terminal_event(event: &crate::protocol::streaming::SseEvent) -> bool {
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
pub(super) fn new_suffix(full: &str, sent: usize) -> (String, bool) {
    match full.get(sent..) {
        Some(suffix) => (suffix.to_string(), false),
        None => (full.to_string(), true),
    }
}

pub(super) fn provider_stream_idle_timeout_error(
    stage: &str,
    timeout: Duration,
) -> BridgeClientError {
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
pub(super) fn execute_streaming_http_post(
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

pub(super) fn execute_streaming_http_post_with_retries(
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
pub(super) struct StreamingHttpIoRequest {
    pub(super) provider_key: String,
    pub(super) url: String,
    pub(super) body: serde_json::Value,
    pub(super) headers: Vec<(String, String)>,
    pub(super) provider_family: ProviderFamily,
    pub(super) tool_name_codec: ProviderToolNameCodec,
    pub(super) tx: mpsc::Sender<StreamMessage>,
    pub(super) cancellation_rx: tokio::sync::oneshot::Receiver<()>,
}

pub(super) async fn streaming_http_io(request: StreamingHttpIoRequest) -> StreamingHttpResult {
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
