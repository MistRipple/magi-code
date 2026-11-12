use super::*;

/// 退避表中第 `attempt` 次重试的基础等待时间。
pub(super) fn base_retry_delay(attempt: usize) -> Duration {
    let index = attempt
        .saturating_sub(1)
        .min(MODEL_PROVIDER_RETRY_DELAYS_MILLIS.len().saturating_sub(1));
    Duration::from_millis(MODEL_PROVIDER_RETRY_DELAYS_MILLIS[index])
}

/// 服务商重试等待的唯一计算（流式与非流式共用）：优先遵守服务商给出的 Retry-After；
/// 否则按退避表并加入 ±25% 随机抖动，避免多个子代理在同一时刻集中重试。
pub(super) fn provider_retry_delay(attempt: usize, retry_after: Option<Duration>) -> Duration {
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

pub(super) fn retry_jitter_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(SEQUENCE.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

pub(super) fn empty_stream_retry_delay(attempt: usize) -> Duration {
    let index = attempt.saturating_sub(1).min(
        MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS
            .len()
            .saturating_sub(1),
    );
    Duration::from_millis(MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS[index])
}

pub(super) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
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

pub(super) fn retryable_http_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 529) || status >= 500
}

/// 额度耗尽、欠费这类 429 不会因为等待而恢复，重试只会白白消耗时间。
pub(super) fn is_permanent_quota_exhaustion(status: u16, detail: &str) -> bool {
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
pub(super) fn retryable_http_response(status: u16, body: &str) -> bool {
    retryable_http_status(status) && !is_permanent_quota_exhaustion(status, body)
}

/// 暂态故障的文本特征：服务商用 200 + 错误体、或仅在消息里说明「过载 / 暂不可用」，
/// 没有可依据的状态码时靠它识别。
pub(super) fn is_transient_provider_error_text(normalized: &str) -> bool {
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

pub(super) fn retryable_bridge_error(error: &BridgeClientError) -> bool {
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

pub(super) fn is_timeout_error(error: &BridgeClientError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

/// 连接成功但上游没有返回任何内容（空流）：只给少量重试机会。
/// 返回 HTML 等非事件流内容通常是地址配错或网关登录页，重试没有意义，不在此列。
pub(super) fn retryable_empty_stream_error(error: &BridgeClientError) -> bool {
    matches!(
        error,
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32007),
            message,
        } if message.contains("empty stream response")
    )
}

pub(super) fn provider_stream_idle_timeout_from_env(value: Option<&str>) -> Duration {
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

pub(super) fn provider_stream_idle_timeout() -> Duration {
    provider_stream_idle_timeout_from_env(
        env::var(OPENAI_STREAM_IDLE_TIMEOUT_MS_ENV).ok().as_deref(),
    )
}

pub(super) fn sleep_retry_delay_cancellable(
    delay: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> bool {
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
