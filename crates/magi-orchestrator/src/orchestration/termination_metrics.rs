use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminationMetricsRecord {
    pub timestamp: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub mode: String,
    pub final_status: String,
    pub reason: String,
    pub rounds: u32,
    pub duration_ms: u64,
    pub token_used: u64,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_vector: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_required: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_or_pending_required: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_trace: Option<Value>,
}

pub trait TerminationMetricsRepository: Send + Sync {
    fn append(&self, record: &TerminationMetricsRecord);
    fn read_all(&self) -> Vec<TerminationMetricsRecord>;
    fn storage_path(&self) -> &Path;
}

pub struct FileTerminationMetricsRepository {
    metrics_path: PathBuf,
}

impl FileTerminationMetricsRepository {
    pub fn new(workspace_root: &str) -> Self {
        Self {
            metrics_path: Path::new(workspace_root)
                .join(".magi")
                .join("metrics")
                .join("termination.jsonl"),
        }
    }
}

impl TerminationMetricsRepository for FileTerminationMetricsRepository {
    fn append(&self, record: &TerminationMetricsRecord) {
        if let Some(parent) = self.metrics_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.metrics_path)
        else {
            return;
        };
        let _ = writeln!(file, "{}", line);
    }

    fn read_all(&self) -> Vec<TerminationMetricsRecord> {
        let Ok(content) = fs::read_to_string(&self.metrics_path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn storage_path(&self) -> &Path {
        &self.metrics_path
    }
}

pub struct InMemoryTerminationMetricsRepository {
    records: std::sync::Mutex<Vec<TerminationMetricsRecord>>,
}

impl InMemoryTerminationMetricsRepository {
    pub fn new() -> Self {
        Self {
            records: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Default for InMemoryTerminationMetricsRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminationMetricsRepository for InMemoryTerminationMetricsRepository {
    fn append(&self, record: &TerminationMetricsRecord) {
        if let Ok(mut records) = self.records.lock() {
            records.push(record.clone());
        }
    }

    fn read_all(&self) -> Vec<TerminationMetricsRecord> {
        self.records
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    fn storage_path(&self) -> &Path {
        Path::new(":memory:")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(session_id: &str) -> TerminationMetricsRecord {
        TerminationMetricsRecord {
            timestamp: "2026-04-21T00:00:00Z".to_string(),
            session_id: session_id.to_string(),
            plan_id: None,
            turn_id: None,
            mode: "standard".to_string(),
            final_status: "completed".to_string(),
            reason: "done".to_string(),
            rounds: 5,
            duration_ms: 10000,
            token_used: 5000,
            evidence_ids: vec![],
            progress_vector: None,
            review_state: None,
            blocker_state: None,
            budget_state: None,
            required_total: Some(3),
            failed_required: Some(0),
            running_or_pending_required: Some(0),
            shadow: None,
            decision_trace: None,
        }
    }

    #[test]
    fn in_memory_append_and_read() {
        let repo = InMemoryTerminationMetricsRepository::new();
        repo.append(&make_record("s1"));
        repo.append(&make_record("s2"));
        let records = repo.read_all();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].session_id, "s1");
        assert_eq!(records[1].session_id, "s2");
    }

    #[test]
    fn in_memory_empty() {
        let repo = InMemoryTerminationMetricsRepository::new();
        assert!(repo.read_all().is_empty());
    }

    #[test]
    fn file_repo_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo =
            FileTerminationMetricsRepository::new(&dir.path().to_string_lossy());
        repo.append(&make_record("s1"));
        repo.append(&make_record("s2"));
        let records = repo.read_all();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn file_repo_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let repo =
            FileTerminationMetricsRepository::new(&dir.path().to_string_lossy());
        assert!(repo.read_all().is_empty());
    }
}
