//! 网络模式下的限流与认证失败退避（进程内、有界）。
//!
//! - **认证失败退避**：同一来源在窗口内连续失败达到阈值后，短时间内一律拒绝（429），
//!   即使之后拿着正确令牌也要等退避结束——防止在线猜测。
//! - **令牌调用限流**：同一令牌在窗口内的请求数有上限。
//!
//! 来源键由调用方决定（经隧道时取 `CF-Connecting-IP`，取不到就退化为统一的 `unknown`，
//! 这样最坏情况是所有未识别来源共享一个桶，而不是绕过限制）。内存占用有上限，
//! 超限时丢弃最旧的桶，不会被海量来源撑爆。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// 统计窗口。
pub const WINDOW_MS: u64 = 60_000;
/// 窗口内允许的认证失败次数，达到后开始退避。
pub const MAX_AUTH_FAILURES: usize = 10;
/// 触发退避后的拒绝时长。
pub const LOCKOUT_MS: u64 = 60_000;
/// 每个令牌在窗口内的请求上限。
pub const MAX_REQUESTS_PER_WINDOW: usize = 300;
/// 最多同时跟踪的桶数量。
const MAX_TRACKED_KEYS: usize = 4096;

#[derive(Default)]
struct Bucket {
    events: VecDeque<u64>,
    locked_until_ms: u64,
}

#[derive(Default)]
pub struct RateLimiter {
    failures: Mutex<HashMap<String, Bucket>>,
    requests: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn prune(bucket: &mut Bucket, now_ms: u64) {
        while bucket
            .events
            .front()
            .is_some_and(|at| now_ms.saturating_sub(*at) >= WINDOW_MS)
        {
            bucket.events.pop_front();
        }
    }

    fn evict_if_full(map: &mut HashMap<String, Bucket>, now_ms: u64) {
        if map.len() < MAX_TRACKED_KEYS {
            return;
        }
        map.retain(|_, bucket| {
            Self::prune(bucket, now_ms);
            !bucket.events.is_empty() || bucket.locked_until_ms > now_ms
        });
        if map.len() >= MAX_TRACKED_KEYS
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, bucket)| bucket.events.back().copied().unwrap_or(0))
                .map(|(key, _)| key.clone())
        {
            map.remove(&oldest);
        }
    }

    /// 该来源当前是否处于退避中。
    pub fn is_locked_out(&self, source: &str, now_ms: u64) -> bool {
        self.failures
            .lock()
            .expect("rate limiter poisoned")
            .get(source)
            .is_some_and(|bucket| bucket.locked_until_ms > now_ms)
    }

    /// 记录一次认证失败；达到阈值时进入退避。
    pub fn record_auth_failure(&self, source: &str, now_ms: u64) {
        let mut map = self.failures.lock().expect("rate limiter poisoned");
        Self::evict_if_full(&mut map, now_ms);
        let bucket = map.entry(source.to_string()).or_default();
        Self::prune(bucket, now_ms);
        bucket.events.push_back(now_ms);
        if bucket.events.len() >= MAX_AUTH_FAILURES {
            bucket.locked_until_ms = now_ms.saturating_add(LOCKOUT_MS);
            bucket.events.clear();
        }
    }

    /// 认证成功后清掉该来源的失败计数。
    pub fn record_auth_success(&self, source: &str) {
        self.failures
            .lock()
            .expect("rate limiter poisoned")
            .remove(source);
    }

    /// 记录一次令牌请求；超过窗口上限返回 `false`（应回 429）。
    pub fn allow_request(&self, token_id: &str, now_ms: u64) -> bool {
        let mut map = self.requests.lock().expect("rate limiter poisoned");
        Self::evict_if_full(&mut map, now_ms);
        let bucket = map.entry(token_id.to_string()).or_default();
        Self::prune(bucket, now_ms);
        if bucket.events.len() >= MAX_REQUESTS_PER_WINDOW {
            return false;
        }
        bucket.events.push_back(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_failures_lock_the_source_out_until_the_backoff_ends() {
        let limiter = RateLimiter::new();
        for i in 0..MAX_AUTH_FAILURES {
            assert!(!limiter.is_locked_out("1.2.3.4", i as u64));
            limiter.record_auth_failure("1.2.3.4", i as u64);
        }
        assert!(limiter.is_locked_out("1.2.3.4", 1_000));
        assert!(!limiter.is_locked_out("5.6.7.8", 1_000), "其它来源不受影响");
        assert!(limiter.is_locked_out("1.2.3.4", LOCKOUT_MS - 1));
        assert!(!limiter.is_locked_out("1.2.3.4", LOCKOUT_MS + MAX_AUTH_FAILURES as u64));
    }

    #[test]
    fn failures_outside_the_window_do_not_accumulate() {
        let limiter = RateLimiter::new();
        for i in 0..(MAX_AUTH_FAILURES - 1) {
            limiter.record_auth_failure("a", (i as u64) * WINDOW_MS);
        }
        assert!(!limiter.is_locked_out("a", (MAX_AUTH_FAILURES as u64) * WINDOW_MS));
    }

    #[test]
    fn success_clears_the_failure_count() {
        let limiter = RateLimiter::new();
        for _ in 0..(MAX_AUTH_FAILURES - 1) {
            limiter.record_auth_failure("a", 10);
        }
        limiter.record_auth_success("a");
        limiter.record_auth_failure("a", 11);
        assert!(!limiter.is_locked_out("a", 12));
    }

    #[test]
    fn per_token_requests_are_capped_per_window_and_recover() {
        let limiter = RateLimiter::new();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(limiter.allow_request("t1", 100));
        }
        assert!(!limiter.allow_request("t1", 200));
        assert!(limiter.allow_request("t2", 200), "其它令牌独立计数");
        assert!(limiter.allow_request("t1", 100 + WINDOW_MS));
    }

    #[test]
    fn tracked_keys_are_bounded() {
        let limiter = RateLimiter::new();
        for i in 0..(MAX_TRACKED_KEYS + 500) {
            limiter.record_auth_failure(&format!("src-{i}"), 10);
        }
        assert!(limiter.failures.lock().unwrap().len() <= MAX_TRACKED_KEYS);
    }
}
