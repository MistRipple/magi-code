use crate::types::{UsageAggregateSnapshot, UsageEvent, UsageSubject, WorkspaceUsageSnapshot};
use std::collections::HashMap;

/// 同一内存统计投影按真实所有者分桶；持久事实仍归 daemon 用量账本。
pub struct InMemoryLedgerStore {
    events: HashMap<UsageSubject, Vec<UsageEvent>>,
    snapshots: HashMap<UsageSubject, UsageAggregateSnapshot>,
    indices: HashMap<UsageSubject, LedgerIndex>,
    workspace_snapshot: Option<WorkspaceUsageSnapshot>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LedgerIndex {
    pub last_committed_ledger_seq: u64,
    pub processed_event_ids: Vec<String>,
}

impl Default for InMemoryLedgerStore {
    fn default() -> Self {
        Self::new()
    }
}
impl InMemoryLedgerStore {
    pub fn new() -> Self {
        Self {
            events: HashMap::new(),
            snapshots: HashMap::new(),
            indices: HashMap::new(),
            workspace_snapshot: None,
        }
    }
    pub(crate) fn append_event(&mut self, event: UsageEvent) {
        self.events
            .entry(event.owner.subject())
            .or_default()
            .push(event);
    }
    pub(crate) fn read_events(&self, subject: &UsageSubject) -> Vec<UsageEvent> {
        self.events.get(subject).cloned().unwrap_or_default()
    }
    pub(crate) fn read_snapshot(&self, subject: &UsageSubject) -> UsageAggregateSnapshot {
        self.snapshots.get(subject).cloned().unwrap_or_default()
    }
    pub(crate) fn write_snapshot(
        &mut self,
        subject: UsageSubject,
        snapshot: UsageAggregateSnapshot,
    ) {
        self.snapshots.insert(subject, snapshot);
    }
    pub(crate) fn read_index(&self, subject: &UsageSubject) -> LedgerIndex {
        self.indices.get(subject).cloned().unwrap_or_default()
    }
    pub(crate) fn write_index(&mut self, subject: UsageSubject, index: LedgerIndex) {
        self.indices.insert(subject, index);
    }
    pub(crate) fn subjects(&self) -> Vec<UsageSubject> {
        self.events.keys().cloned().collect()
    }
    pub fn read_workspace_snapshot(&self, workspace_id: &str) -> WorkspaceUsageSnapshot {
        self.workspace_snapshot
            .clone()
            .unwrap_or_else(|| WorkspaceUsageSnapshot::empty(workspace_id))
    }
    pub fn write_workspace_snapshot(&mut self, snapshot: WorkspaceUsageSnapshot) {
        self.workspace_snapshot = Some(snapshot);
    }
}
