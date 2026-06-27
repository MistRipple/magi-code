use crate::{EventCategory, EventContext, EventEnvelope};
use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const AUDIT_USAGE_LEDGER_SCHEMA_VERSION: &str = "audit-usage-ledger-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditUsageLedgerEntry {
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: UtcMillis,
    pub sequence: u64,
    pub context: EventContext,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditUsageLedgerSnapshot {
    pub schema_version: String,
    pub next_sequence: u64,
    pub audit_entries: Vec<AuditUsageLedgerEntry>,
    pub usage_entries: Vec<AuditUsageLedgerEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuditUsageLedgerStatus {
    pub schema_version: String,
    pub next_sequence: u64,
    pub audit_count: usize,
    pub usage_count: usize,
    pub persistence_path: Option<PathBuf>,
    pub last_persist_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum AuditUsageLedgerError {
    #[error("审计/用量账本 JSON 处理失败: {0}")]
    Json(#[from] serde_json::Error),
    #[error("审计/用量账本 IO 失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("审计/用量账本 schema 版本不匹配: expected {expected}, actual {actual}")]
    SchemaMismatch { expected: String, actual: String },
}

impl Default for AuditUsageLedgerSnapshot {
    fn default() -> Self {
        Self {
            schema_version: AUDIT_USAGE_LEDGER_SCHEMA_VERSION.to_string(),
            next_sequence: 1,
            audit_entries: Vec::new(),
            usage_entries: Vec::new(),
        }
    }
}

impl AuditUsageLedgerSnapshot {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        let mut snapshot = Self::default();
        for event in events {
            snapshot.record_event(event);
        }
        snapshot.normalize()
    }

    pub fn record_event(&mut self, event: &EventEnvelope) {
        let entry = AuditUsageLedgerEntry::from_event(event);
        match event.category {
            EventCategory::Audit => self.audit_entries.push(entry),
            EventCategory::Usage => self.usage_entries.push(entry),
            _ => return,
        }
        self.next_sequence = self.next_sequence.max(event.sequence.saturating_add(1));
    }

    pub fn normalize(mut self) -> Self {
        normalize_entries(&mut self.audit_entries);
        normalize_entries(&mut self.usage_entries);

        let highest_sequence = self
            .audit_entries
            .iter()
            .chain(self.usage_entries.iter())
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(0);
        self.next_sequence = self
            .next_sequence
            .max(highest_sequence.saturating_add(1))
            .max(1);
        self
    }

    pub fn export_json(&self) -> Result<String, AuditUsageLedgerError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn import_json(value: &str) -> Result<Self, AuditUsageLedgerError> {
        let snapshot: Self = serde_json::from_str(value)?;
        snapshot.validate_schema()?;
        Ok(snapshot.normalize())
    }

    pub fn persist_to_path(&self, path: impl AsRef<Path>) -> Result<(), AuditUsageLedgerError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = self.export_json()?;
        magi_core::fs_atomic::write_atomic(path, json.as_bytes())?;
        Ok(())
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, AuditUsageLedgerError> {
        let value = fs::read_to_string(path)?;
        Self::import_json(&value)
    }

    pub fn validate_schema(&self) -> Result<(), AuditUsageLedgerError> {
        if self.schema_version != AUDIT_USAGE_LEDGER_SCHEMA_VERSION {
            return Err(AuditUsageLedgerError::SchemaMismatch {
                expected: AUDIT_USAGE_LEDGER_SCHEMA_VERSION.to_string(),
                actual: self.schema_version.clone(),
            });
        }
        Ok(())
    }

    pub fn audit_count(&self) -> usize {
        self.audit_entries.len()
    }

    pub fn usage_count(&self) -> usize {
        self.usage_entries.len()
    }

    pub fn status(
        &self,
        persistence_path: Option<&Path>,
        last_persist_error: Option<String>,
    ) -> AuditUsageLedgerStatus {
        AuditUsageLedgerStatus {
            schema_version: self.schema_version.clone(),
            next_sequence: self.next_sequence,
            audit_count: self.audit_count(),
            usage_count: self.usage_count(),
            persistence_path: persistence_path.map(Path::to_path_buf),
            last_persist_error,
        }
    }
}

impl AuditUsageLedgerEntry {
    fn from_event(event: &EventEnvelope) -> Self {
        Self {
            event_id: event.event_id.to_string(),
            event_type: event.event_type.clone(),
            occurred_at: event.occurred_at,
            sequence: event.sequence,
            context: EventContext {
                workspace_id: event.workspace_id.clone(),
                session_id: event.session_id.clone(),
                mission_id: event.mission_id.clone(),
                assignment_id: event.assignment_id.clone(),
                task_id: event.task_id.clone(),
            },
            payload: event.payload.clone(),
        }
    }
}

fn normalize_entries(entries: &mut Vec<AuditUsageLedgerEntry>) {
    let mut seen = HashSet::<String>::new();
    entries.retain(|entry| seen.insert(entry.event_id.clone()));
    entries.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then(left.event_id.cmp(&right.event_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventCategory, EventEnvelope};
    use magi_core::EventId;
    use serde_json::json;
    use std::fs;

    fn event(category: EventCategory, event_type: &str, sequence: u64) -> EventEnvelope {
        let mut event = match category {
            EventCategory::Domain => EventEnvelope::domain(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Audit => EventEnvelope::audit(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Usage => EventEnvelope::usage(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::Projection => EventEnvelope::projection(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
            EventCategory::System => EventEnvelope::system(
                EventId::new(format!("e-{sequence}")),
                event_type,
                json!({"sequence": sequence}),
            ),
        };
        event.sequence = sequence;
        event
    }

    #[test]
    fn 从事件导出账本时只收口审计与用量事件() {
        let snapshot = AuditUsageLedgerSnapshot::from_events(&[
            event(EventCategory::Domain, "mission.created", 1),
            event(EventCategory::Audit, "ledger.audit.recorded", 2),
            event(EventCategory::Usage, "ledger.usage.recorded", 3),
        ]);

        assert_eq!(snapshot.audit_count(), 1);
        assert_eq!(snapshot.usage_count(), 1);
        assert_eq!(snapshot.next_sequence, 4);
        assert_eq!(
            snapshot.audit_entries[0].event_type,
            "ledger.audit.recorded"
        );
        assert_eq!(
            snapshot.usage_entries[0].event_type,
            "ledger.usage.recorded"
        );
    }

    #[test]
    fn 账本导入导出往返保持稳定() {
        let snapshot = AuditUsageLedgerSnapshot::from_events(&[
            event(EventCategory::Audit, "ledger.audit.recorded", 2),
            event(EventCategory::Usage, "ledger.usage.recorded", 3),
        ]);

        let json = snapshot.export_json().expect("json export");
        let restored = AuditUsageLedgerSnapshot::import_json(&json).expect("json import");

        assert_eq!(restored.schema_version, AUDIT_USAGE_LEDGER_SCHEMA_VERSION);
        assert_eq!(restored.audit_count(), 1);
        assert_eq!(restored.usage_count(), 1);
        assert_eq!(restored.next_sequence, 4);
    }

    #[test]
    fn 账本可落盘并恢复() {
        let snapshot = AuditUsageLedgerSnapshot::from_events(&[
            event(EventCategory::Audit, "ledger.audit.recorded", 2),
            event(EventCategory::Usage, "ledger.usage.recorded", 3),
        ]);
        let base = std::env::temp_dir().join(format!(
            "magi-event-bus-ledger-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::create_dir_all(&base).expect("create temp dir");
        let path = base.join("audit-usage-ledger.json");

        snapshot.persist_to_path(&path).expect("persist");
        let restored = AuditUsageLedgerSnapshot::load_from_path(&path).expect("restore");

        assert_eq!(restored.audit_count(), 1);
        assert_eq!(restored.usage_count(), 1);
        assert_eq!(restored.next_sequence, 4);
    }
}
