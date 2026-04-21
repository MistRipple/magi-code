use std::collections::{HashMap, HashSet};

pub struct DispatchExecutionWorkerResolution {
    pub ok: bool,
    pub selected_worker: Option<String>,
    pub degraded: bool,
    pub routing_reason: String,
    pub error: Option<String>,
}

impl DispatchExecutionWorkerResolution {
    fn success(worker: impl Into<String>, degraded: bool, reason: impl Into<String>) -> Self {
        Self {
            ok: true,
            selected_worker: Some(worker.into()),
            degraded,
            routing_reason: reason.into(),
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            selected_worker: None,
            degraded: false,
            routing_reason: String::new(),
            error: Some(error.into()),
        }
    }
}

struct RuntimeUnavailable {
    until: u64,
    reason: String,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct DispatchRoutingService {
    _worker_slots: Vec<String>,
    fallback_priority: HashMap<String, Vec<String>>,
    available_workers: HashSet<String>,
    runtime_unavailable: HashMap<String, RuntimeUnavailable>,
    runtime_unavailable_cooldown_ms: u64,
}

impl DispatchRoutingService {
    pub fn new(
        worker_slots: Vec<String>,
        fallback_priority: HashMap<String, Vec<String>>,
        runtime_unavailable_cooldown_ms: u64,
    ) -> Self {
        let available_workers: HashSet<String> = worker_slots.iter().cloned().collect();
        Self {
            _worker_slots: worker_slots,
            fallback_priority,
            available_workers,
            runtime_unavailable: HashMap::new(),
            runtime_unavailable_cooldown_ms,
        }
    }

    pub fn set_available_workers(&mut self, workers: HashSet<String>) {
        self.available_workers = workers;
    }

    pub fn mark_worker_runtime_unavailable(&mut self, worker: &str, reason: impl Into<String>) {
        self.runtime_unavailable.insert(
            worker.to_string(),
            RuntimeUnavailable {
                until: now_millis() + self.runtime_unavailable_cooldown_ms,
                reason: reason.into(),
            },
        );
    }

    pub fn clear_worker_runtime_unavailable(&mut self, worker: &str) {
        self.runtime_unavailable.remove(worker);
    }

    pub fn clear_all_runtime_unavailable(&mut self) {
        self.runtime_unavailable.clear();
    }

    pub fn should_mark_runtime_unavailable(error_message: &str) -> bool {
        let normalized = error_message.to_lowercase();
        if normalized.is_empty() {
            return false;
        }
        const PATTERNS: &[&str] = &[
            "unauthorized", "forbidden", "invalid api key", "api key", "auth",
            "permission", "quota", "billing", "payment", "rate limit", "limit",
            "insufficient", "suspended", "disabled", "timeout", "timed out",
            "network", "connection", "fetch failed", "socket", "econnreset",
            "econnrefused", "enotfound", "eai_again", "tls", "certificate",
            "overloaded", "service unavailable", "502", "503", "504",
        ];
        PATTERNS.iter().any(|p| normalized.contains(p))
    }

    pub fn resolve_execution_worker(
        &mut self,
        preferred_worker: &str,
        busy_workers: Option<&HashSet<String>>,
        excluded_workers: Option<&HashSet<String>>,
        allow_busy_fallback: bool,
    ) -> DispatchExecutionWorkerResolution {
        let empty = HashSet::new();
        let busy = busy_workers.unwrap_or(&empty);
        let excluded = excluded_workers.unwrap_or(&empty);

        self.cleanup_expired_unavailable();

        let is_busy = busy.contains(preferred_worker);
        let is_excluded = excluded.contains(preferred_worker);
        let is_unavailable = !self.is_worker_available(preferred_worker);

        if !is_busy && !is_excluded && !is_unavailable {
            return DispatchExecutionWorkerResolution::success(
                preferred_worker,
                false,
                format!("执行前校验通过，继续由 {preferred_worker} 执行"),
            );
        }

        let preferred_reason = if is_unavailable {
            self.get_runtime_unavailable_reason(preferred_worker)
                .unwrap_or_else(|| "当前不可用".to_string())
        } else if is_busy {
            "当前 worker lane 忙碌".to_string()
        } else {
            "被调度器排除".to_string()
        };

        if is_busy && !allow_busy_fallback {
            return DispatchExecutionWorkerResolution::failure(format!(
                "任务目标 Worker {preferred_worker} 忙碌（{preferred_reason}），且当前策略不允许忙碌时降级"
            ));
        }

        let fallback = self
            .fallback_priority
            .get(preferred_worker)
            .and_then(|fallbacks| {
                fallbacks.iter().find(|w| {
                    !busy.contains(w.as_str())
                        && !excluded.contains(w.as_str())
                        && self.is_worker_available(w)
                })
            })
            .cloned();

        match fallback {
            Some(fallback_worker) => DispatchExecutionWorkerResolution::success(
                &fallback_worker,
                true,
                format!(
                    "目标 Worker {preferred_worker} 当前不可执行（{preferred_reason}），执行时降级到 {fallback_worker}"
                ),
            ),
            None => DispatchExecutionWorkerResolution::failure(format!(
                "任务目标 Worker {preferred_worker} 不可执行（{preferred_reason}），且无可用降级 Worker"
            )),
        }
    }

    fn is_worker_available(&self, worker: &str) -> bool {
        if !self.available_workers.contains(worker) {
            return false;
        }
        !self.runtime_unavailable.contains_key(worker)
    }

    fn get_runtime_unavailable_reason(&self, worker: &str) -> Option<String> {
        let status = self.runtime_unavailable.get(worker)?;
        let now = now_millis();
        if now >= status.until {
            return None;
        }
        let remain_seconds = (status.until - now) / 1000 + 1;
        Some(format!("{}（冷却 {}s）", status.reason, remain_seconds))
    }

    fn cleanup_expired_unavailable(&mut self) {
        let now = now_millis();
        self.runtime_unavailable.retain(|_, v| now < v.until);
    }
}
