use super::*;
use magi_core::{SessionId, UtcMillis};

#[test]
fn query_and_history_use_deterministic_tie_breakers() {
    let store = MemoryStore::new();
    let session_id = SessionId::new("session-1");
    let created_at = UtcMillis(42);
    let updated_at = UtcMillis(43);

    store.append(MemoryRecord {
        memory_id: "memory-b".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Recent,
        content: "content-b".to_string(),
        provenance: None,
        compacted: false,
        created_at,
    });
    store.append(MemoryRecord {
        memory_id: "memory-a".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Recent,
        content: "content-a".to_string(),
        provenance: None,
        compacted: false,
        created_at,
    });
    store.upsert_preference(PreferenceMemoryRecord {
        preference_id: "pref-b".to_string(),
        session_id: session_id.clone(),
        key: "theme".to_string(),
        value: "dark".to_string(),
        provenance: None,
        updated_at,
    });
    store.upsert_preference(PreferenceMemoryRecord {
        preference_id: "pref-a".to_string(),
        session_id: session_id.clone(),
        key: "layout".to_string(),
        value: "dense".to_string(),
        provenance: None,
        updated_at,
    });
    store.record_extraction(MemoryExtractionRecord {
        extraction_id: "extract-b".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source-b".to_string()),
        summary: "summary-b".to_string(),
        produced_memory_ids: vec!["m-2".to_string(), "m-1".to_string()],
        created_at,
    });
    store.record_extraction(MemoryExtractionRecord {
        extraction_id: "extract-a".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source-a".to_string()),
        summary: "summary-a".to_string(),
        produced_memory_ids: vec!["m-1".to_string(), "m-2".to_string()],
        created_at,
    });

    let query_ids = store
        .query(&MemoryQuery {
            session_id: session_id.clone(),
            layer: None,
            limit: usize::MAX,
        })
        .into_iter()
        .map(|record| record.memory_id)
        .collect::<Vec<_>>();
    assert_eq!(
        query_ids,
        vec!["memory-a".to_string(), "memory-b".to_string()]
    );

    let preference_ids = store
        .preferences_for_session(&session_id)
        .into_iter()
        .map(|record| record.preference_id)
        .collect::<Vec<_>>();
    assert_eq!(
        preference_ids,
        vec!["pref-a".to_string(), "pref-b".to_string()]
    );

    let extraction_ids = store
        .extraction_results_for_session(&session_id)
        .into_iter()
        .map(|record| record.extraction_id)
        .collect::<Vec<_>>();
    assert_eq!(
        extraction_ids,
        vec!["extract-a".to_string(), "extract-b".to_string()]
    );

    let compaction = store.compact_session(&session_id, "memory-retained", "retained content");
    assert_eq!(compaction.session_id, session_id);
    assert_eq!(compaction.retained_id, "memory-retained");
    assert_eq!(store.compaction_history_for_session(&session_id).len(), 1);
}

#[test]
fn record_extraction_normalizes_duplicate_memory_ids() {
    let store = MemoryStore::new();
    let session_id = SessionId::new("session-1");

    store.record_extraction(MemoryExtractionRecord {
        extraction_id: "extract-1".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source".to_string()),
        summary: "summary".to_string(),
        produced_memory_ids: vec![
            "m-2".to_string(),
            "m-1".to_string(),
            "m-2".to_string(),
            "m-1".to_string(),
        ],
        created_at: UtcMillis(10),
    });

    let records = store.extraction_results_for_session(&session_id);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].produced_memory_ids,
        vec!["m-1".to_string(), "m-2".to_string()]
    );
}

#[test]
fn apply_extraction_writes_memory_records_and_verifies_closed_loop() {
    let store = MemoryStore::new();
    let session_id = SessionId::new("session-1");

    let linkage = store.apply_extraction(MemoryExtractionApplyRequest {
        extraction_id: "extract-1".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source-1".to_string()),
        summary: "summary".to_string(),
        memories: vec![
            ExtractedMemory {
                memory_id: "memory-b".to_string(),
                layer: MemoryLayer::Durable,
                content: "beta".to_string(),
                created_at: UtcMillis(20),
            },
            ExtractedMemory {
                memory_id: "memory-a".to_string(),
                layer: MemoryLayer::Recent,
                content: "alpha".to_string(),
                created_at: UtcMillis(20),
            },
            ExtractedMemory {
                memory_id: "memory-a".to_string(),
                layer: MemoryLayer::Recent,
                content: "alpha".to_string(),
                created_at: UtcMillis(20),
            },
            ExtractedMemory {
                memory_id: "memory-c".to_string(),
                layer: MemoryLayer::Shared,
                content: "gamma".to_string(),
                created_at: UtcMillis(19),
            },
        ],
        created_at: UtcMillis(30),
    });

    assert_eq!(
        linkage.extraction.produced_memory_ids,
        vec![
            "memory-a".to_string(),
            "memory-b".to_string(),
            "memory-c".to_string(),
        ]
    );
    assert_eq!(
        linkage
            .produced_records
            .iter()
            .map(|record| record.memory_id.clone())
            .collect::<Vec<_>>(),
        vec![
            "memory-c".to_string(),
            "memory-a".to_string(),
            "memory-b".to_string(),
        ]
    );

    let stored = store.get("memory-a").expect("applied memory to exist");
    assert_eq!(stored.session_id, session_id);
    assert_eq!(
        stored.provenance,
        Some(MemoryProvenance {
            source: "extraction".to_string(),
            extracted_from: Some("extract-1".to_string()),
        })
    );
    assert!(!stored.compacted);

    let stored_linkage = store
        .extraction_linkage("extract-1")
        .expect("stored linkage to exist");
    assert_eq!(stored_linkage, linkage);

    let verification = store
        .verify_extraction_linkage("extract-1")
        .expect("verification to exist");
    assert_eq!(
        verification.resolved_memory_ids,
        linkage.extraction.produced_memory_ids
    );
    assert!(verification.missing_memory_ids.is_empty());
    assert!(verification.provenance_mismatch_memory_ids.is_empty());
    assert!(verification.dangling_memory_ids.is_empty());
    assert!(verification.session_mismatch_memory_ids.is_empty());
    assert!(verification.is_consistent);
}

#[test]
fn verify_extraction_linkage_detects_missing_and_mismatched_records() {
    let store = MemoryStore::new();
    let session_id = SessionId::new("session-1");

    store.record_extraction(MemoryExtractionRecord {
        extraction_id: "extract-1".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source".to_string()),
        summary: "summary".to_string(),
        produced_memory_ids: vec![
            "memory-missing".to_string(),
            "memory-wrong-session".to_string(),
            "memory-provenance-mismatch".to_string(),
        ],
        created_at: UtcMillis(10),
    });

    store.append(MemoryRecord {
        memory_id: "memory-wrong-session".to_string(),
        session_id: SessionId::new("session-2"),
        layer: MemoryLayer::Recent,
        content: "other session".to_string(),
        provenance: Some(MemoryProvenance {
            source: "extraction".to_string(),
            extracted_from: Some("extract-1".to_string()),
        }),
        compacted: false,
        created_at: UtcMillis(11),
    });
    store.append(MemoryRecord {
        memory_id: "memory-provenance-mismatch".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Durable,
        content: "wrong provenance".to_string(),
        provenance: None,
        compacted: false,
        created_at: UtcMillis(12),
    });
    store.append(MemoryRecord {
        memory_id: "memory-dangling".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Shared,
        content: "dangling".to_string(),
        provenance: Some(MemoryProvenance {
            source: "extraction".to_string(),
            extracted_from: Some("extract-1".to_string()),
        }),
        compacted: false,
        created_at: UtcMillis(13),
    });

    let verification = store
        .verify_extraction_linkage("extract-1")
        .expect("verification to exist");
    assert_eq!(verification.resolved_memory_ids, Vec::<String>::new());
    assert_eq!(
        verification.missing_memory_ids,
        vec!["memory-missing".to_string()]
    );
    assert_eq!(
        verification.provenance_mismatch_memory_ids,
        vec!["memory-provenance-mismatch".to_string()]
    );
    assert_eq!(
        verification.dangling_memory_ids,
        vec!["memory-dangling".to_string()]
    );
    assert_eq!(
        verification.session_mismatch_memory_ids,
        vec!["memory-wrong-session".to_string()]
    );
    assert!(!verification.is_consistent);
}

#[test]
fn compaction_marks_existing_entries_and_retains_new_record() {
    let store = MemoryStore::new();
    let session_id = SessionId::new("session-1");

    store.append(MemoryRecord {
        memory_id: "memory-a".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Recent,
        content: "alpha".to_string(),
        provenance: None,
        compacted: false,
        created_at: UtcMillis(1),
    });
    store.append(MemoryRecord {
        memory_id: "memory-b".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Shared,
        content: "beta".to_string(),
        provenance: None,
        compacted: false,
        created_at: UtcMillis(2),
    });

    let summary = store.compact_session(&session_id, "memory-retained", "retained");

    assert_eq!(summary.affected_count, 2);
    assert!(store.get("memory-a").is_some_and(|record| record.compacted));
    assert!(store.get("memory-b").is_some_and(|record| record.compacted));

    let retained = store
        .get("memory-retained")
        .expect("retained memory to exist");
    assert_eq!(retained.layer, MemoryLayer::Durable);
    assert!(!retained.compacted);
    assert_eq!(retained.content, "retained");
}
