use std::collections::HashMap;

use crate::types::{SessionUsageSnapshot, UsageEvent, WorkspaceUsageSnapshot};

pub struct InMemoryLedgerStore {
    session_events: HashMap<String, Vec<UsageEvent>>,
    session_snapshots: HashMap<String, SessionUsageSnapshot>,
    session_indices: HashMap<String, SessionLedgerIndex>,
    workspace_snapshot: Option<WorkspaceUsageSnapshot>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionLedgerIndex {
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
            session_events: HashMap::new(),
            session_snapshots: HashMap::new(),
            session_indices: HashMap::new(),
            workspace_snapshot: None,
        }
    }

    pub fn append_session_event(&mut self, event: UsageEvent) {
        self.session_events
            .entry(event.session_id.clone())
            .or_default()
            .push(event);
    }

    pub fn read_session_events(&self, session_id: &str) -> Vec<UsageEvent> {
        self.session_events
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn read_session_snapshot(
        &self,
        workspace_id: &str,
        session_id: &str,
    ) -> SessionUsageSnapshot {
        self.session_snapshots
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| SessionUsageSnapshot::empty(workspace_id, session_id))
    }

    pub fn write_session_snapshot(&mut self, snapshot: SessionUsageSnapshot) {
        self.session_snapshots
            .insert(snapshot.session_id.clone(), snapshot);
    }

    pub fn read_workspace_snapshot(&self, workspace_id: &str) -> WorkspaceUsageSnapshot {
        self.workspace_snapshot
            .clone()
            .unwrap_or_else(|| WorkspaceUsageSnapshot::empty(workspace_id))
    }

    pub fn write_workspace_snapshot(&mut self, snapshot: WorkspaceUsageSnapshot) {
        self.workspace_snapshot = Some(snapshot);
    }

    pub fn read_session_index(&self, session_id: &str) -> SessionLedgerIndex {
        self.session_indices
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn write_session_index(&mut self, session_id: &str, index: SessionLedgerIndex) {
        self.session_indices.insert(session_id.to_string(), index);
    }

    pub fn list_session_ids(&self) -> Vec<String> {
        self.session_events.keys().cloned().collect()
    }

    pub fn reset_session(&mut self, session_id: &str) {
        self.session_events.remove(session_id);
        self.session_snapshots.remove(session_id);
        self.session_indices.remove(session_id);
    }

    pub fn reset_all(&mut self) {
        self.session_events.clear();
        self.session_snapshots.clear();
        self.session_indices.clear();
        self.workspace_snapshot = None;
    }
}
