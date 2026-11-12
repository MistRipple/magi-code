use super::*;

pub(super) type HttpPostResult = Result<(u16, String, Option<Duration>), BridgeClientError>;

pub(super) enum HttpPostMessage {
    Headers(u16),
    Done(HttpPostResult),
}

pub(super) type StreamingHttpResult = Result<ModelResponse, BridgeClientError>;

pub(super) type ToolChoiceFallbackHttpResult = Result<
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
pub(super) struct TransportRetryControls<'a> {
    pub(super) on_retry: &'a dyn Fn(&ModelRetryRuntimeEvent),
    pub(super) is_cancelled: &'a dyn Fn() -> bool,
    pub(super) observer: &'a ModelTransportObserver<'a>,
}

pub(super) fn receive_cancellable_http_result(
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

pub(super) fn execute_cancellable_http_post(
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

pub(super) async fn async_http_post_io(
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

pub(super) fn execute_cancellable_http_post_with_retries(
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
