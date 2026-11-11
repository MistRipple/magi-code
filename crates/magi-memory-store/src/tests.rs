use super::*;
use magi_core::{SessionId, UtcMillis};

#[test]
fn query_and_history_use_deterministic_tie_breakers() {
    let store = MemoryStore::new();
    let session_id = SessionId::new("session-1");
    let created_at = UtcMillis(42);

    store.append(MemoryRecord {
        memory_id: "memory-b".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Recent,
        content: "content-b".to_string(),
        provenance: None,
        created_at,
    });
    store.append(MemoryRecord {
        memory_id: "memory-a".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Recent,
        content: "content-a".to_string(),
        provenance: None,
        created_at,
    });
    store.apply_extraction(MemoryExtractionApplyRequest {
        extraction_id: "extract-b".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source-b".to_string()),
        summary: "summary-b".to_string(),
        memories: Vec::new(),
        created_at,
    });
    store.apply_extraction(MemoryExtractionApplyRequest {
        extraction_id: "extract-a".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("source-a".to_string()),
        summary: "summary-a".to_string(),
        memories: Vec::new(),
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

    let extraction_ids = store
        .extraction_results_for_session(&session_id)
        .into_iter()
        .map(|record| record.extraction_id)
        .collect::<Vec<_>>();
    assert_eq!(
        extraction_ids,
        vec!["extract-a".to_string(), "extract-b".to_string()]
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

    // 构造损坏的内部记录以验证审计查询；生产写入只有 apply_extraction。
    store.extraction_results.write().unwrap().insert(
        "extract-1".into(),
        MemoryExtractionRecord {
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
        },
    );

    store.append(MemoryRecord {
        memory_id: "memory-wrong-session".to_string(),
        session_id: SessionId::new("session-2"),
        layer: MemoryLayer::Recent,
        content: "other session".to_string(),
        provenance: Some(MemoryProvenance {
            source: "extraction".to_string(),
            extracted_from: Some("extract-1".to_string()),
        }),
        created_at: UtcMillis(11),
    });
    store.append(MemoryRecord {
        memory_id: "memory-provenance-mismatch".to_string(),
        session_id: session_id.clone(),
        layer: MemoryLayer::Durable,
        content: "wrong provenance".to_string(),
        provenance: None,
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
