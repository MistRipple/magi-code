use std::collections::HashMap;

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn random_suffix() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    now_millis().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    format!("{:x}", hasher.finish() & 0xFFFFFF)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchAckState {
    Pending,
    Acked,
    Nacked,
}

#[derive(Clone, Debug)]
pub struct DispatchExecutionProtocolState {
    pub task_id: String,
    pub batch_id: String,
    pub worker: String,
    pub dispatch_attempt_id: String,
    pub idempotency_key: String,
    pub lease_id: String,
    pub lease_expire_at: u64,
    pub heartbeat_at: u64,
    pub ack_state: DispatchAckState,
    pub created_at: u64,
    pub ack_at: Option<u64>,
    pub nack_reason: Option<String>,
    pub timeout_triggered: bool,
}

#[derive(Clone, Debug)]
pub struct DispatchProtocolTimeoutPayload {
    pub state: DispatchExecutionProtocolState,
    pub reason_code: String,
}

#[derive(Clone, Debug)]
pub struct DispatchProtocolManagerConfig {
    pub ack_timeout_ms: u64,
    pub lease_ttl_ms: u64,
}

pub struct DispatchProtocolManager {
    config: DispatchProtocolManagerConfig,
    states: HashMap<String, DispatchExecutionProtocolState>,
}

impl DispatchProtocolManager {
    pub fn new(config: DispatchProtocolManagerConfig) -> Self {
        Self {
            config,
            states: HashMap::new(),
        }
    }

    pub fn register(
        &mut self,
        task_id: &str,
        batch_id: Option<&str>,
        worker: &str,
    ) -> &DispatchExecutionProtocolState {
        let now = now_millis();
        let suffix = random_suffix();
        let dispatch_attempt_id = format!("dispatch-attempt-{task_id}-{now}-{suffix}");
        let state = DispatchExecutionProtocolState {
            task_id: task_id.to_string(),
            batch_id: batch_id.unwrap_or("unknown-batch").to_string(),
            worker: worker.to_string(),
            idempotency_key: dispatch_attempt_id.clone(),
            lease_id: format!("lease-{task_id}-{now}-{suffix}"),
            lease_expire_at: now + self.config.lease_ttl_ms,
            heartbeat_at: now,
            ack_state: DispatchAckState::Pending,
            created_at: now,
            ack_at: None,
            nack_reason: None,
            timeout_triggered: false,
            dispatch_attempt_id,
        };
        self.states.insert(task_id.to_string(), state);
        self.states.get(task_id).unwrap()
    }

    pub fn mark_ack(&mut self, task_id: &str, worker_id: Option<&str>) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return;
        }
        let Some(state) = self.states.get_mut(task_id) else {
            return;
        };
        if let Some(wid) = worker_id {
            if state.worker != wid {
                tracing::warn!(
                    task_id,
                    expected = %state.worker,
                    actual = %wid,
                    "Dispatch.Protocol.ACK.Worker不一致"
                );
            }
        }
        let now = now_millis();
        state.ack_state = DispatchAckState::Acked;
        state.ack_at = Some(now);
        state.heartbeat_at = now;
        state.lease_expire_at = now + self.config.lease_ttl_ms;
    }

    pub fn mark_nack(&mut self, task_id: &str, reason: &str) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return;
        }
        let Some(state) = self.states.get_mut(task_id) else {
            return;
        };
        state.ack_state = DispatchAckState::Nacked;
        state.nack_reason = Some(reason.to_string());
        state.lease_expire_at = now_millis();
    }

    pub fn update_heartbeat(&mut self, task_id: &str, worker_id: &str, timestamp: u64) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return;
        }
        let Some(state) = self.states.get_mut(task_id) else {
            return;
        };
        if state.worker != worker_id {
            tracing::warn!(
                task_id,
                expected = %state.worker,
                actual = %worker_id,
                "Dispatch.Protocol.Heartbeat.Worker不一致"
            );
            return;
        }
        let hb_at = if timestamp > 0 { timestamp } else { now_millis() };
        if state.ack_state == DispatchAckState::Pending {
            state.ack_state = DispatchAckState::Acked;
            state.ack_at = Some(hb_at);
        }
        state.heartbeat_at = hb_at;
        state.lease_expire_at = hb_at + self.config.lease_ttl_ms;
    }

    pub fn clear(&mut self, task_id: &str) {
        let task_id = task_id.trim();
        if !task_id.is_empty() {
            self.states.remove(task_id);
        }
    }

    pub fn clear_by_batch(&mut self, batch_id: &str) {
        self.states.retain(|_, state| state.batch_id != batch_id);
    }

    pub fn clear_all(&mut self) {
        self.states.clear();
    }

    /// 检查所有租约，返回超时事件列表。
    /// 调用方应在编排循环的每次迭代中调用此方法。
    pub fn check_leases(&mut self) -> Vec<DispatchProtocolTimeoutPayload> {
        if self.states.is_empty() {
            return Vec::new();
        }
        let now = now_millis();
        let mut timeouts = Vec::new();
        let mut to_remove = Vec::new();

        for state in self.states.values_mut() {
            if state.timeout_triggered {
                continue;
            }
            let reason_code = if state.ack_state == DispatchAckState::Pending
                && now.saturating_sub(state.created_at) > self.config.ack_timeout_ms
            {
                Some("ack-timeout".to_string())
            } else if state.ack_state == DispatchAckState::Nacked {
                Some(format!(
                    "nack:{}",
                    state.nack_reason.as_deref().unwrap_or("unknown")
                ))
            } else if state.ack_state == DispatchAckState::Acked && state.lease_expire_at <= now {
                Some("lease-expired".to_string())
            } else {
                None
            };

            if let Some(reason_code) = reason_code {
                state.timeout_triggered = true;
                timeouts.push(DispatchProtocolTimeoutPayload {
                    state: state.clone(),
                    reason_code,
                });
                to_remove.push(state.task_id.clone());
            }
        }

        for task_id in to_remove {
            self.states.remove(&task_id);
        }

        timeouts
    }

    pub fn get_state(&self, task_id: &str) -> Option<&DispatchExecutionProtocolState> {
        self.states.get(task_id)
    }

    pub fn state_count(&self) -> usize {
        self.states.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> DispatchProtocolManagerConfig {
        DispatchProtocolManagerConfig {
            ack_timeout_ms: 5_000,
            lease_ttl_ms: 30_000,
        }
    }

    #[test]
    fn register_creates_state() {
        let mut mgr = DispatchProtocolManager::new(make_config());
        let state = mgr.register("task-1", Some("batch-1"), "worker-a");
        assert_eq!(state.task_id, "task-1");
        assert_eq!(state.batch_id, "batch-1");
        assert_eq!(state.worker, "worker-a");
        assert_eq!(state.ack_state, DispatchAckState::Pending);
        assert!(!state.timeout_triggered);
        assert_eq!(mgr.state_count(), 1);
    }

    #[test]
    fn mark_ack_updates_state() {
        let mut mgr = DispatchProtocolManager::new(make_config());
        mgr.register("task-1", None, "worker-a");
        mgr.mark_ack("task-1", Some("worker-a"));
        let state = mgr.get_state("task-1").unwrap();
        assert_eq!(state.ack_state, DispatchAckState::Acked);
        assert!(state.ack_at.is_some());
    }

    #[test]
    fn mark_nack_triggers_timeout() {
        let mut mgr = DispatchProtocolManager::new(make_config());
        mgr.register("task-1", None, "worker-a");
        mgr.mark_nack("task-1", "worker busy");
        let timeouts = mgr.check_leases();
        assert_eq!(timeouts.len(), 1);
        assert!(timeouts[0].reason_code.starts_with("nack:"));
        assert_eq!(mgr.state_count(), 0);
    }

    #[test]
    fn heartbeat_promotes_pending_to_acked() {
        let mut mgr = DispatchProtocolManager::new(make_config());
        mgr.register("task-1", None, "worker-a");
        let now = now_millis();
        mgr.update_heartbeat("task-1", "worker-a", now);
        let state = mgr.get_state("task-1").unwrap();
        assert_eq!(state.ack_state, DispatchAckState::Acked);
    }

    #[test]
    fn heartbeat_rejected_for_wrong_worker() {
        let mut mgr = DispatchProtocolManager::new(make_config());
        mgr.register("task-1", None, "worker-a");
        let now = now_millis();
        mgr.update_heartbeat("task-1", "worker-b", now);
        let state = mgr.get_state("task-1").unwrap();
        assert_eq!(state.ack_state, DispatchAckState::Pending);
    }

    #[test]
    fn clear_by_batch_removes_matching() {
        let mut mgr = DispatchProtocolManager::new(make_config());
        mgr.register("task-1", Some("batch-1"), "worker-a");
        mgr.register("task-2", Some("batch-1"), "worker-b");
        mgr.register("task-3", Some("batch-2"), "worker-c");
        mgr.clear_by_batch("batch-1");
        assert_eq!(mgr.state_count(), 1);
        assert!(mgr.get_state("task-3").is_some());
    }

    #[test]
    fn lease_expired_timeout() {
        let config = DispatchProtocolManagerConfig {
            ack_timeout_ms: 5_000,
            lease_ttl_ms: 0,
        };
        let mut mgr = DispatchProtocolManager::new(config);
        mgr.register("task-1", None, "worker-a");
        mgr.mark_ack("task-1", None);
        // lease_ttl_ms=0 意味着租约立即过期
        if let Some(state) = mgr.states.get_mut("task-1") {
            state.lease_expire_at = now_millis().saturating_sub(1);
        }
        let timeouts = mgr.check_leases();
        assert_eq!(timeouts.len(), 1);
        assert_eq!(timeouts[0].reason_code, "lease-expired");
    }
}
