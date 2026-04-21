use super::{
    MemoryCompactionRecord, MemoryCompactionSummary, MemoryLayer, MemoryProvenance, MemoryRecord,
    MemoryStore,
};
use magi_core::{SessionId, UtcMillis};

impl MemoryStore {
    pub fn compact_session(
        &self,
        session_id: &SessionId,
        retained_id: impl Into<String>,
        content: impl Into<String>,
    ) -> MemoryCompactionSummary {
        let retained_id = retained_id.into();
        let mut state = self
            .entries
            .write()
            .expect("memory store write lock poisoned");
        let merged_ids: Vec<_> = state
            .values()
            .filter(|entry| &entry.session_id == session_id && !entry.compacted)
            .map(|entry| entry.memory_id.clone())
            .collect();
        for entry in state.values_mut() {
            if &entry.session_id == session_id {
                entry.compacted = true;
            }
        }
        state.insert(
            retained_id.clone(),
            MemoryRecord {
                memory_id: retained_id.clone(),
                session_id: session_id.clone(),
                layer: MemoryLayer::Durable,
                content: content.into(),
                provenance: Some(MemoryProvenance {
                    source: "compaction".to_string(),
                    extracted_from: Some("session-memory".to_string()),
                }),
                compacted: false,
                created_at: UtcMillis::now(),
            },
        );
        drop(state);

        let summary = MemoryCompactionSummary {
            session_id: session_id.clone(),
            affected_count: merged_ids.len(),
            merged_ids,
            retained_id,
        };
        self.compaction_history
            .write()
            .expect("memory compaction history write lock poisoned")
            .push(MemoryCompactionRecord {
                session_id: session_id.clone(),
                summary: summary.clone(),
                created_at: UtcMillis::now(),
            });
        summary
    }
}
