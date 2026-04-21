use super::{MemoryStore, PreferenceMemoryRecord};
use magi_core::SessionId;

impl MemoryStore {
    pub fn upsert_preference(&self, record: PreferenceMemoryRecord) {
        self.preferences
            .write()
            .expect("memory preferences write lock poisoned")
            .insert(record.preference_id.clone(), record);
    }

    pub fn preferences_for_session(&self, session_id: &SessionId) -> Vec<PreferenceMemoryRecord> {
        let mut records = self
            .preferences
            .read()
            .expect("memory preferences read lock poisoned")
            .values()
            .filter(|entry| &entry.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.updated_at
                .0
                .cmp(&right.updated_at.0)
                .then_with(|| left.preference_id.cmp(&right.preference_id))
        });
        records
    }
}
