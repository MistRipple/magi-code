use super::{MemoryQuery, MemoryRecord, MemoryStore};
use magi_core::SessionId;

impl MemoryStore {
    pub fn get(&self, memory_id: &str) -> Option<MemoryRecord> {
        self.entries
            .read()
            .expect("memory store read lock poisoned")
            .get(memory_id)
            .cloned()
    }

    pub fn list_for_session(&self, session_id: &SessionId) -> Vec<MemoryRecord> {
        self.query(&MemoryQuery {
            session_id: session_id.clone(),
            layer: None,
            limit: usize::MAX,
        })
    }

    pub fn query(&self, query: &MemoryQuery) -> Vec<MemoryRecord> {
        let mut records: Vec<_> = self
            .entries
            .read()
            .expect("memory store read lock poisoned")
            .values()
            .filter(|entry| entry.session_id == query.session_id)
            .filter(|entry| query.layer.is_none_or(|layer| entry.layer == layer))
            .cloned()
            .collect();
        records.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.memory_id.cmp(&right.memory_id))
        });
        if query.limit < records.len() {
            records.truncate(query.limit);
        }
        records
    }
}
