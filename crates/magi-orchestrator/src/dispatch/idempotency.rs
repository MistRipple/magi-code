use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchIdempotencyStatus {
    Dispatched,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchIdempotencyRecord {
    pub key: String,
    pub session_id: String,
    pub mission_id: String,
    pub task_id: String,
    pub worker: String,
    pub ownership: String,
    pub mode: String,
    pub task_name: String,
    pub routing_reason: String,
    pub degraded: bool,
    pub status: DispatchIdempotencyStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug)]
pub struct DispatchIdempotencyClaimInput {
    pub key: String,
    pub session_id: String,
    pub mission_id: String,
    pub task_id: String,
    pub worker: String,
    pub ownership: String,
    pub mode: String,
    pub task_name: String,
    pub routing_reason: String,
    pub degraded: bool,
    pub status: DispatchIdempotencyStatus,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct DispatchIdempotencyClaimResult {
    pub claimed: bool,
    pub record: DispatchIdempotencyRecord,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct DispatchIdempotencyStore {
    by_key: HashMap<String, DispatchIdempotencyRecord>,
    key_by_task_id: HashMap<String, String>,
    ttl_ms: u64,
    max_records: usize,
}

impl DispatchIdempotencyStore {
    pub fn new(ttl_ms: Option<u64>, max_records: Option<usize>) -> Self {
        Self {
            by_key: HashMap::new(),
            key_by_task_id: HashMap::new(),
            ttl_ms: ttl_ms.unwrap_or(24 * 60 * 60 * 1000).max(60_000),
            max_records: max_records.unwrap_or(5000).max(100),
        }
    }

    pub fn resolve_by_key(&mut self, key: &str) -> Option<DispatchIdempotencyRecord> {
        let normalized = key.trim();
        if normalized.is_empty() {
            return None;
        }
        let record = self.by_key.get(normalized)?;
        if self.is_expired(record) {
            let task_id = record.task_id.clone();
            self.by_key.remove(normalized);
            self.key_by_task_id.remove(&task_id);
            return None;
        }
        Some(record.clone())
    }

    pub fn resolve_by_task_id(&mut self, task_id: &str) -> Option<DispatchIdempotencyRecord> {
        let normalized = task_id.trim();
        if normalized.is_empty() {
            return None;
        }
        let key = self.key_by_task_id.get(normalized)?.clone();
        let record = self.by_key.get(&key)?;
        if self.is_expired(record) {
            self.by_key.remove(&key);
            self.key_by_task_id.remove(normalized);
            return None;
        }
        Some(record.clone())
    }

    pub fn claim_or_get(
        &mut self,
        input: DispatchIdempotencyClaimInput,
    ) -> DispatchIdempotencyClaimResult {
        let normalized_key = input.key.trim().to_string();
        if let Some(existing) = self.by_key.get(&normalized_key) {
            if !self.is_expired(existing) {
                return DispatchIdempotencyClaimResult {
                    claimed: false,
                    record: existing.clone(),
                };
            }
        }

        let now = now_millis();
        let record = DispatchIdempotencyRecord {
            key: normalized_key.clone(),
            session_id: input.session_id.trim().to_string(),
            mission_id: input.mission_id.trim().to_string(),
            task_id: input.task_id.trim().to_string(),
            worker: input.worker,
            ownership: Self::normalize_ownership(&input.ownership),
            mode: Self::normalize_mode(&input.mode),
            task_name: input.task_name.trim().to_string(),
            routing_reason: input.routing_reason.trim().to_string(),
            degraded: input.degraded,
            status: input.status,
            created_at: input.created_at.unwrap_or(now),
            updated_at: input.updated_at.unwrap_or(now),
        };

        self.key_by_task_id
            .insert(record.task_id.clone(), normalized_key.clone());
        self.by_key.insert(normalized_key, record.clone());
        self.prune();

        DispatchIdempotencyClaimResult {
            claimed: true,
            record,
        }
    }

    pub fn update_status_by_task_id(
        &mut self,
        task_id: &str,
        status: DispatchIdempotencyStatus,
    ) -> Option<DispatchIdempotencyRecord> {
        let normalized = task_id.trim();
        if normalized.is_empty() {
            return None;
        }
        let key = self.key_by_task_id.get(normalized)?.clone();
        let record = self.by_key.get_mut(&key)?;
        if record.status == status {
            return Some(record.clone());
        }
        record.status = status;
        record.updated_at = now_millis();
        Some(record.clone())
    }

    pub fn remove_by_task_id(&mut self, task_id: &str) -> bool {
        let normalized = task_id.trim();
        if normalized.is_empty() {
            return false;
        }
        let Some(key) = self.key_by_task_id.remove(normalized) else {
            return false;
        };
        self.by_key.remove(&key).is_some()
    }

    pub fn clear(&mut self) {
        self.by_key.clear();
        self.key_by_task_id.clear();
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    fn is_expired(&self, record: &DispatchIdempotencyRecord) -> bool {
        now_millis().saturating_sub(record.updated_at) > self.ttl_ms
    }

    fn prune(&mut self) {
        let now = now_millis();
        let expired_keys: Vec<String> = self
            .by_key
            .iter()
            .filter(|(_, r)| now.saturating_sub(r.updated_at) > self.ttl_ms)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired_keys {
            if let Some(record) = self.by_key.remove(&key) {
                self.key_by_task_id.remove(&record.task_id);
            }
        }

        if self.by_key.len() <= self.max_records {
            return;
        }

        let mut records: Vec<(String, u64)> = self
            .by_key
            .iter()
            .map(|(k, r)| (k.clone(), r.updated_at))
            .collect();
        records.sort_by_key(|(_, ts)| *ts);

        let overflow = self.by_key.len() - self.max_records;
        for (key, _) in records.into_iter().take(overflow) {
            if let Some(record) = self.by_key.remove(&key) {
                self.key_by_task_id.remove(&record.task_id);
            }
        }
    }

    fn normalize_ownership(input: &str) -> String {
        let normalized = input.trim().to_lowercase();
        if normalized.is_empty() {
            "general".to_string()
        } else {
            normalized
        }
    }

    fn normalize_mode(input: &str) -> String {
        let normalized = input.trim().to_lowercase();
        if normalized.is_empty() {
            "implement".to_string()
        } else {
            normalized
        }
    }
}

impl Default for DispatchIdempotencyStore {
    fn default() -> Self {
        Self::new(None, None)
    }
}
