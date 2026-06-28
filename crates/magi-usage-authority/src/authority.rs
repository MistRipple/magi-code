use magi_core::UtcMillis;

use crate::ledger_store::InMemoryLedgerStore;
use crate::model_identity::build_model_resolution_identity;
use crate::reducer::{
    rebuild_session_snapshot_from_events, rebuild_workspace_snapshot_from_sessions,
};
use crate::types::{
    ExecutionBindingIdentity, SessionUsageSnapshot, UsageCallIdentity, UsageCallRecordInput,
    UsageCallStatus, UsageEvent, UsageEventType, UsageEventUsageDelta, UsagePhase, UsageSourceRole,
    WorkspaceUsageSnapshot,
};

fn build_event_id(
    workspace_id: &str,
    session_id: &str,
    call_id: &str,
    template_id: &str,
    binding_revision: u32,
    timestamp: u64,
) -> String {
    format!("{workspace_id}:{session_id}:{call_id}:{template_id}:{binding_revision}:{timestamp}")
}

pub struct UsageAuthority {
    store: InMemoryLedgerStore,
}

impl Default for UsageAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageAuthority {
    pub fn new() -> Self {
        Self {
            store: InMemoryLedgerStore::new(),
        }
    }

    pub fn append_call_record(&mut self, input: UsageCallRecordInput) -> u64 {
        let mut index = self.store.read_session_index(&input.session_id);
        let ledger_seq = index.last_committed_ledger_seq + 1;
        let event_timestamp = input.timestamp.unwrap_or_else(|| UtcMillis::now().0);
        let event_id = input
            .event_id
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                build_event_id(
                    &input.workspace_id,
                    &input.session_id,
                    &input.call_identity.call_id,
                    &input.execution_binding.template_id,
                    input.execution_binding.binding_revision,
                    event_timestamp,
                )
            });

        if index.processed_event_ids.contains(&event_id) {
            return ledger_seq - 1;
        }

        let model_identity = build_model_resolution_identity(
            &input.model_config,
            &input.execution_binding,
            Some(&input.model_config.model),
            None,
        );

        let event = UsageEvent {
            event_id: event_id.clone(),
            ledger_seq,
            workspace_id: input.workspace_id.clone(),
            session_id: input.session_id.clone(),
            turn_id: input.turn_id.clone(),
            dispatch_wave_id: input.dispatch_wave_id.clone(),
            assignment_id: input.assignment_id.clone(),
            timestamp: event_timestamp,
            event_type: if input.status == UsageCallStatus::Failed {
                UsageEventType::LlmCallFailed
            } else {
                UsageEventType::LlmCallCompleted
            },
            execution_binding: Some(input.execution_binding),
            model_identity: Some(model_identity),
            call_identity: Some(input.call_identity),
            usage_delta: Some(UsageEventUsageDelta {
                raw_input_tokens: input.usage.input_tokens,
                raw_output_tokens: input.usage.output_tokens,
                context_window_tokens: input.usage.total_tokens,
                cache_read_tokens: input.usage.cache_read_tokens,
                cache_write_tokens: input.usage.cache_write_tokens,
                cache_read_included_in_input: input.usage.cache_read_included_in_input,
            }),
            status: Some(input.status),
            error_code: input.error_code,
        };

        self.store.append_session_event(event);
        index.last_committed_ledger_seq = ledger_seq;
        let max_ids = 5000;
        if index.processed_event_ids.len() >= max_ids {
            let drain_count = index.processed_event_ids.len() - max_ids + 1;
            index.processed_event_ids.drain(..drain_count);
        }
        index.processed_event_ids.push(event_id);
        self.store.write_session_index(&input.session_id, index);

        let session_snapshot =
            self.rebuild_session_snapshot_internal(&input.workspace_id, &input.session_id);
        self.rebuild_workspace_snapshot(&input.workspace_id);

        session_snapshot.last_applied_ledger_seq
    }

    pub fn rebuild_session_snapshot(
        &mut self,
        workspace_id: &str,
        session_id: &str,
    ) -> SessionUsageSnapshot {
        self.rebuild_session_snapshot_internal(workspace_id, session_id)
    }

    pub fn rebuild_workspace_snapshot(&mut self, workspace_id: &str) -> WorkspaceUsageSnapshot {
        let session_ids = self.store.list_session_ids();
        let session_snapshots: Vec<SessionUsageSnapshot> = session_ids
            .iter()
            .map(|sid| self.store.read_session_snapshot(workspace_id, sid))
            .collect();
        let snapshot = rebuild_workspace_snapshot_from_sessions(workspace_id, &session_snapshots);
        self.store.write_workspace_snapshot(snapshot.clone());
        snapshot
    }

    pub fn get_session_snapshot(
        &mut self,
        workspace_id: &str,
        session_id: &str,
    ) -> SessionUsageSnapshot {
        let snapshot = self.store.read_session_snapshot(workspace_id, session_id);
        if snapshot.version > 0 || snapshot.last_applied_ledger_seq > 0 {
            return snapshot;
        }
        self.rebuild_session_snapshot(workspace_id, session_id)
    }

    pub fn get_workspace_snapshot(&mut self, workspace_id: &str) -> WorkspaceUsageSnapshot {
        let snapshot = self.store.read_workspace_snapshot(workspace_id);
        if snapshot.version > 0 || snapshot.updated_at > 0 {
            return snapshot;
        }
        self.rebuild_workspace_snapshot(workspace_id)
    }

    pub fn reset_session(&mut self, workspace_id: &str, session_id: &str) {
        let mut index = self.store.read_session_index(session_id);
        let ledger_seq = index.last_committed_ledger_seq + 1;
        let event_id = format!("reset:{workspace_id}:{session_id}:{ledger_seq}");
        let event = UsageEvent {
            event_id: event_id.clone(),
            ledger_seq,
            workspace_id: workspace_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: None,
            dispatch_wave_id: None,
            assignment_id: None,
            timestamp: UtcMillis::now().0,
            event_type: UsageEventType::SessionReset,
            execution_binding: None,
            model_identity: None,
            call_identity: None,
            usage_delta: None,
            status: None,
            error_code: None,
        };
        self.store.append_session_event(event);
        index.last_committed_ledger_seq = ledger_seq;
        index.processed_event_ids.push(event_id);
        self.store.write_session_index(session_id, index);
        self.rebuild_session_snapshot_internal(workspace_id, session_id);
        self.rebuild_workspace_snapshot(workspace_id);
    }

    pub fn reset_workspace(&mut self, workspace_id: &str) {
        let session_ids = self.store.list_session_ids();
        for sid in session_ids {
            self.reset_session(workspace_id, &sid);
        }
    }

    fn rebuild_session_snapshot_internal(
        &mut self,
        workspace_id: &str,
        session_id: &str,
    ) -> SessionUsageSnapshot {
        let events = self.store.read_session_events(session_id);
        let snapshot = rebuild_session_snapshot_from_events(workspace_id, session_id, &events);
        self.store.write_session_snapshot(snapshot.clone());
        snapshot
    }
}

pub fn build_execution_binding_identity(
    template_id: &str,
    engine_id: &str,
    binding_revision: u32,
    role: UsageSourceRole,
) -> ExecutionBindingIdentity {
    ExecutionBindingIdentity {
        template_id: template_id.to_string(),
        engine_id: engine_id.to_string(),
        binding_revision,
        role,
    }
}

pub fn build_usage_call_identity(
    call_id: &str,
    parent_call_id: Option<&str>,
    source: UsageSourceRole,
    phase: Option<UsagePhase>,
) -> UsageCallIdentity {
    UsageCallIdentity {
        call_id: call_id.to_string(),
        parent_call_id: parent_call_id.map(|s| s.to_string()),
        source,
        phase: phase.unwrap_or(UsagePhase::Unknown),
    }
}
