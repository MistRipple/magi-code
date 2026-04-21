mod compaction;
mod extraction;
mod history;
mod preferences;
mod query;

#[cfg(test)]
mod tests;

use magi_core::{SessionId, UtcMillis};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MemoryLayer {
    Recent,
    Durable,
    Shared,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProvenance {
    pub source: String,
    pub extracted_from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub memory_id: String,
    pub session_id: SessionId,
    pub layer: MemoryLayer,
    pub content: String,
    pub provenance: Option<MemoryProvenance>,
    pub compacted: bool,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferenceMemoryRecord {
    pub preference_id: String,
    pub session_id: SessionId,
    pub key: String,
    pub value: String,
    pub provenance: Option<MemoryProvenance>,
    pub updated_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExtractionRecord {
    pub extraction_id: String,
    pub session_id: SessionId,
    pub source_ref: Option<String>,
    pub summary: String,
    pub produced_memory_ids: Vec<String>,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedMemory {
    pub memory_id: String,
    pub layer: MemoryLayer,
    pub content: String,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExtractionApplyRequest {
    pub extraction_id: String,
    pub session_id: SessionId,
    pub source_ref: Option<String>,
    pub summary: String,
    pub memories: Vec<ExtractedMemory>,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExtractionLinkage {
    pub extraction: MemoryExtractionRecord,
    pub produced_records: Vec<MemoryRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExtractionVerification {
    pub extraction_id: String,
    pub produced_memory_ids: Vec<String>,
    pub resolved_memory_ids: Vec<String>,
    pub missing_memory_ids: Vec<String>,
    pub provenance_mismatch_memory_ids: Vec<String>,
    pub dangling_memory_ids: Vec<String>,
    pub session_mismatch_memory_ids: Vec<String>,
    pub is_consistent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub session_id: SessionId,
    pub layer: Option<MemoryLayer>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCompactionSummary {
    pub session_id: SessionId,
    pub merged_ids: Vec<String>,
    pub retained_id: String,
    pub affected_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCompactionRecord {
    pub session_id: SessionId,
    pub summary: MemoryCompactionSummary,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryStore {
    entries: Arc<RwLock<HashMap<String, MemoryRecord>>>,
    preferences: Arc<RwLock<HashMap<String, PreferenceMemoryRecord>>>,
    extraction_results: Arc<RwLock<HashMap<String, MemoryExtractionRecord>>>,
    compaction_history: Arc<RwLock<Vec<MemoryCompactionRecord>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&self, record: MemoryRecord) {
        self.entries
            .write()
            .expect("memory store write lock poisoned")
            .insert(record.memory_id.clone(), record);
    }
}
