use super::{MemoryExtractionRecord, MemoryStore};
use magi_core::SessionId;

impl MemoryStore {
    pub fn extraction_results_for_session(
        &self,
        session_id: &SessionId,
    ) -> Vec<MemoryExtractionRecord> {
        let mut records = self
            .extraction_results
            .read()
            .expect("memory extraction results read lock poisoned")
            .values()
            .filter(|entry| &entry.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.extraction_id.cmp(&right.extraction_id))
        });
        records
    }
}
