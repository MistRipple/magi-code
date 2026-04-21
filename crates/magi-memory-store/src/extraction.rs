use super::{
    ExtractedMemory, MemoryExtractionApplyRequest, MemoryExtractionLinkage,
    MemoryExtractionRecord, MemoryExtractionVerification, MemoryProvenance, MemoryRecord,
    MemoryStore,
};
use std::collections::HashSet;

const EXTRACTION_PROVENANCE_SOURCE: &str = "extraction";

impl MemoryStore {
    pub fn apply_extraction(
        &self,
        request: MemoryExtractionApplyRequest,
    ) -> MemoryExtractionLinkage {
        let MemoryExtractionApplyRequest {
            extraction_id,
            session_id,
            source_ref,
            summary,
            memories,
            created_at,
        } = request;

        let produced_records =
            normalize_extracted_memories(&session_id, &extraction_id, memories);
        {
            let mut entries = self
                .entries
                .write()
                .expect("memory store write lock poisoned");
            for record in &produced_records {
                entries.insert(record.memory_id.clone(), record.clone());
            }
        }

        let extraction = normalize_extraction_record(MemoryExtractionRecord {
            extraction_id: extraction_id.clone(),
            session_id,
            source_ref,
            summary,
            produced_memory_ids: produced_records
                .iter()
                .map(|record| record.memory_id.clone())
                .collect(),
            created_at,
        });
        self.insert_extraction_record(extraction.clone());

        MemoryExtractionLinkage {
            extraction,
            produced_records,
        }
    }

    pub fn record_extraction(&self, record: MemoryExtractionRecord) {
        self.insert_extraction_record(normalize_extraction_record(record));
    }

    pub fn extraction_linkage(&self, extraction_id: &str) -> Option<MemoryExtractionLinkage> {
        let extraction = self
            .extraction_results
            .read()
            .expect("memory extraction results read lock poisoned")
            .get(extraction_id)
            .cloned()?;

        let mut produced_records = self.resolve_records(&extraction.produced_memory_ids);
        sort_memory_records(&mut produced_records);
        Some(MemoryExtractionLinkage {
            extraction,
            produced_records,
        })
    }

    pub fn verify_extraction_linkage(
        &self,
        extraction_id: &str,
    ) -> Option<MemoryExtractionVerification> {
        let extraction = self
            .extraction_results
            .read()
            .expect("memory extraction results read lock poisoned")
            .get(extraction_id)
            .cloned()?;

        let entries = self
            .entries
            .read()
            .expect("memory store read lock poisoned");

        let mut resolved_memory_ids = Vec::new();
        let mut missing_memory_ids = Vec::new();
        let mut provenance_mismatch_memory_ids = Vec::new();
        let mut session_mismatch_memory_ids = Vec::new();

        for memory_id in &extraction.produced_memory_ids {
            match entries.get(memory_id) {
                Some(record) => {
                    if record.session_id != extraction.session_id {
                        session_mismatch_memory_ids.push(record.memory_id.clone());
                        continue;
                    }

                    if !has_extraction_provenance(record, extraction_id) {
                        provenance_mismatch_memory_ids.push(record.memory_id.clone());
                        continue;
                    }

                    resolved_memory_ids.push(record.memory_id.clone());
                }
                None => missing_memory_ids.push(memory_id.clone()),
            }
        }

        let produced_memory_ids: HashSet<_> =
            extraction.produced_memory_ids.iter().cloned().collect();
        let mut dangling_memory_ids = Vec::new();
        for record in entries.values().filter(|record| {
            record
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.extracted_from.as_deref())
                == Some(extraction_id)
        }) {
            if record.session_id != extraction.session_id {
                session_mismatch_memory_ids.push(record.memory_id.clone());
            }

            if !produced_memory_ids.contains(record.memory_id.as_str()) {
                dangling_memory_ids.push(record.memory_id.clone());
            }
        }

        normalize_string_list(&mut resolved_memory_ids);
        normalize_string_list(&mut missing_memory_ids);
        normalize_string_list(&mut provenance_mismatch_memory_ids);
        normalize_string_list(&mut dangling_memory_ids);
        normalize_string_list(&mut session_mismatch_memory_ids);

        Some(MemoryExtractionVerification {
            extraction_id: extraction.extraction_id.clone(),
            produced_memory_ids: extraction.produced_memory_ids,
            resolved_memory_ids: resolved_memory_ids.clone(),
            missing_memory_ids: missing_memory_ids.clone(),
            provenance_mismatch_memory_ids: provenance_mismatch_memory_ids.clone(),
            dangling_memory_ids: dangling_memory_ids.clone(),
            session_mismatch_memory_ids: session_mismatch_memory_ids.clone(),
            is_consistent: missing_memory_ids.is_empty()
                && provenance_mismatch_memory_ids.is_empty()
                && dangling_memory_ids.is_empty()
                && session_mismatch_memory_ids.is_empty(),
        })
    }

    fn insert_extraction_record(&self, record: MemoryExtractionRecord) {
        self.extraction_results
            .write()
            .expect("memory extraction results write lock poisoned")
            .insert(record.extraction_id.clone(), record);
    }

    fn resolve_records(&self, memory_ids: &[String]) -> Vec<MemoryRecord> {
        let entries = self
            .entries
            .read()
            .expect("memory store read lock poisoned");
        memory_ids
            .iter()
            .filter_map(|memory_id| entries.get(memory_id).cloned())
            .collect()
    }
}

fn normalize_extraction_record(mut record: MemoryExtractionRecord) -> MemoryExtractionRecord {
    normalize_string_list(&mut record.produced_memory_ids);
    record
}

fn normalize_extracted_memories(
    session_id: &magi_core::SessionId,
    extraction_id: &str,
    memories: Vec<ExtractedMemory>,
) -> Vec<MemoryRecord> {
    let mut records = memories
        .into_iter()
        .map(|memory| MemoryRecord {
            memory_id: memory.memory_id,
            session_id: session_id.clone(),
            layer: memory.layer,
            content: memory.content,
            provenance: Some(MemoryProvenance {
                source: EXTRACTION_PROVENANCE_SOURCE.to_string(),
                extracted_from: Some(extraction_id.to_string()),
            }),
            compacted: false,
            created_at: memory.created_at,
        })
        .collect::<Vec<_>>();

    // Group duplicate ids together first so dedup keeps a deterministic winner.
    records.sort_by(|left, right| {
        left.memory_id
            .cmp(&right.memory_id)
            .then_with(|| left.created_at.0.cmp(&right.created_at.0))
            .then_with(|| left.layer.cmp(&right.layer))
            .then_with(|| left.content.cmp(&right.content))
    });
    records.dedup_by(|left, right| left.memory_id == right.memory_id);
    sort_memory_records(&mut records);
    records
}

fn has_extraction_provenance(record: &MemoryRecord, extraction_id: &str) -> bool {
    record.provenance.as_ref().is_some_and(|provenance| {
        provenance.source == EXTRACTION_PROVENANCE_SOURCE
            && provenance.extracted_from.as_deref() == Some(extraction_id)
    })
}

fn sort_memory_records(records: &mut [MemoryRecord]) {
    records.sort_by(|left, right| {
        left.created_at
            .0
            .cmp(&right.created_at.0)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn normalize_string_list(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}
