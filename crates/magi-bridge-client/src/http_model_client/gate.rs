use super::*;

pub(super) struct ModelProviderGate {
    pub(super) slots: Mutex<HashMap<String, Arc<ModelProviderSlot>>>,
}

pub(super) struct ModelProviderSlot {
    pub(super) in_flight: Mutex<usize>,
    pub(super) available: Condvar,
}

pub(super) struct ModelProviderPermit {
    pub(super) slot: Arc<ModelProviderSlot>,
}

impl ModelProviderGate {
    #[cfg(test)]
    pub(super) fn acquire(&self, key: &str) -> ModelProviderPermit {
        let cancellation = AtomicBool::new(false);
        self.acquire_cancellable(key, &cancellation)
            .expect("non-cancellable provider permit acquisition cannot be cancelled")
    }

    pub(super) fn acquire_cancellable(
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

pub(super) fn model_provider_gate() -> &'static ModelProviderGate {
    static GATE: OnceLock<ModelProviderGate> = OnceLock::new();
    GATE.get_or_init(|| ModelProviderGate {
        slots: Mutex::new(HashMap::new()),
    })
}

pub(super) fn shared_streaming_http_client() -> Result<reqwest::Client, BridgeClientError> {
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

pub(super) fn shared_http_runtime() -> Result<&'static tokio::runtime::Runtime, BridgeClientError> {
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

pub(super) fn shared_request_http_client() -> Result<reqwest::Client, BridgeClientError> {
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

pub(super) fn normalize_provider_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

pub(super) fn forced_tool_choice_capability(provider_key: &str) -> ForcedToolChoiceCapability {
    FORCED_TOOL_CHOICE_CAPABILITIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("forced tool choice capability registry lock poisoned")
        .get(provider_key)
        .copied()
        .unwrap_or(ForcedToolChoiceCapability::Unknown)
}

pub(super) fn record_forced_tool_choice_capability(
    provider_key: String,
    capability: ForcedToolChoiceCapability,
) {
    FORCED_TOOL_CHOICE_CAPABILITIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("forced tool choice capability registry lock poisoned")
        .insert(provider_key, capability);
}
