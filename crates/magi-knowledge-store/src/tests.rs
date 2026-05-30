use crate::{
    CodeIndexIngestion, CodeIndexSource, CodeIndexSymbol, CodeSymbolKind, KnowledgeAuditLink,
    KnowledgeGovernanceLink, KnowledgeGovernanceOutcome, KnowledgeKind, KnowledgeQuery,
    KnowledgeRecord, KnowledgeStore,
    code_scanner::{CodeIndexFile, CodeIndexSummary},
};
use magi_core::{UtcMillis, WorkspaceId};

#[test]
fn list_uses_deterministic_tie_breaker() {
    let store = KnowledgeStore::new();
    let updated_at = UtcMillis(42);

    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-b".to_string(),
        kind: KnowledgeKind::Faq,
        title: "B".to_string(),
        content: "content".to_string(),
        tags: vec!["tag".to_string()],
        workspace_id: None,
        source_ref: Some("ref-b".to_string()),
        updated_at,
    });
    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-a".to_string(),
        kind: KnowledgeKind::Faq,
        title: "A".to_string(),
        content: "content".to_string(),
        tags: vec!["tag".to_string()],
        workspace_id: None,
        source_ref: Some("ref-a".to_string()),
        updated_at,
    });

    let knowledge_ids = store
        .list()
        .into_iter()
        .map(|record| record.knowledge_id)
        .collect::<Vec<_>>();

    assert_eq!(knowledge_ids, vec!["kb-a".to_string(), "kb-b".to_string()]);
}

#[test]
fn code_index_ingestion_persists_source_and_governance_sidecars() {
    let store = KnowledgeStore::new();
    store.ingest_code_index(sample_code_index("kb-code-1"));

    let record = store
        .get("kb-code-1")
        .expect("code index record should be stored");
    assert_eq!(record.kind, KnowledgeKind::CodeIndex);
    assert_eq!(record.tags, vec!["rust".to_string(), "search".to_string()]);
    assert_eq!(record.source_ref.as_deref(), Some("src/query.rs#execute"));

    let indexed_terms = store.indexed_terms("kb-code-1");
    assert!(indexed_terms.contains(&"execute".to_string()));
    assert!(indexed_terms.contains(&"query".to_string()));
    assert!(indexed_terms.contains(&"abc123".to_string()));
    assert!(indexed_terms.contains(&"allowed".to_string()));
    assert!(indexed_terms.contains(&"read".to_string()));

    let source = store
        .code_source("kb-code-1")
        .expect("code index source should be retained");
    assert_eq!(source.path, "crates/magi-knowledge-store/src/query.rs");
    assert_eq!(
        source.symbol.as_ref().map(|symbol| symbol.name.as_str()),
        Some("execute")
    );

    let audit = store
        .audit_link("kb-code-1")
        .expect("audit link should be retained");
    assert_eq!(audit.audit_event_id, "audit-knowledge-index-1");
    assert_eq!(audit.sequence, Some(17));

    let governance = store
        .governance_link("kb-code-1")
        .expect("governance link should be retained");
    assert_eq!(governance.outcome, KnowledgeGovernanceOutcome::Allowed);
    assert_eq!(
        governance.policy_refs,
        vec![
            "governance.safe_read".to_string(),
            "knowledge.code_index".to_string()
        ]
    );
}

#[test]
fn query_and_governed_output_close_the_code_index_loop() {
    let store = KnowledgeStore::new();
    store.ingest_code_index(sample_code_index("kb-code-1"));
    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-faq-1".to_string(),
        kind: KnowledgeKind::Faq,
        title: "FAQ".to_string(),
        content: "Human guidance without source metadata".to_string(),
        tags: vec!["faq".to_string()],
        workspace_id: None,
        source_ref: None,
        updated_at: UtcMillis(50),
    });

    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::CodeIndex),
        text: Some("execute query.rs safe_read".to_string()),
        tags: vec![],
        workspace_id: None,
        limit: 10,
    };

    let result = store.query(&query);
    assert_eq!(result.total_matches, 1);
    assert_eq!(result.matches[0].record.knowledge_id, "kb-code-1");
    assert!(
        result.matches[0]
            .matched_terms
            .contains(&"execute".to_string())
    );
    assert!(
        result.matches[0]
            .matched_terms
            .contains(&"query".to_string())
    );
    assert!(
        result.matches[0]
            .matched_terms
            .contains(&"safe".to_string())
    );

    let governed_query = store.governed_query(&query);
    assert_eq!(governed_query.total_matches, 1);
    assert!(!governed_query.truncated);
    assert_eq!(governed_query.results.len(), 1);

    let record = &governed_query.results[0];
    assert_eq!(record.knowledge_id, "kb-code-1");
    assert_eq!(record.source_ref.as_deref(), Some("src/query.rs#execute"));
    assert_eq!(
        record
            .code_source
            .as_ref()
            .map(|source| source.path.as_str()),
        Some("crates/magi-knowledge-store/src/query.rs")
    );
    assert_eq!(
        record
            .code_source
            .as_ref()
            .and_then(|source| source.symbol.as_ref())
            .map(|symbol| symbol.kind.clone()),
        Some(CodeSymbolKind::Function)
    );
    assert_eq!(
        record
            .audit_link
            .as_ref()
            .map(|audit| audit.audit_event_id.as_str()),
        Some("audit-knowledge-index-1")
    );
    assert_eq!(
        record
            .governance_link
            .as_ref()
            .map(|governance| governance.outcome.clone()),
        Some(KnowledgeGovernanceOutcome::Allowed)
    );
}

#[test]
fn plain_upsert_overwrites_old_code_index_sidecars() {
    let store = KnowledgeStore::new();
    store.ingest_code_index(sample_code_index("kb-shared"));

    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-shared".to_string(),
        kind: KnowledgeKind::Faq,
        title: "Shared FAQ".to_string(),
        content: "Fallback human-authored note".to_string(),
        tags: vec!["faq".to_string()],
        workspace_id: None,
        source_ref: None,
        updated_at: UtcMillis(200),
    });

    assert!(store.code_source("kb-shared").is_none());
    assert!(store.audit_link("kb-shared").is_none());
    assert!(store.governance_link("kb-shared").is_none());

    let outputs = store.governed_output(&KnowledgeQuery {
        kind: None,
        text: Some("fallback".to_string()),
        tags: vec![],
        workspace_id: None,
        limit: 10,
    });
    let overwritten = outputs
        .into_iter()
        .find(|record| record.knowledge_id == "kb-shared")
        .expect("overwritten record should still be queryable");
    assert!(overwritten.code_source.is_none());
    assert!(overwritten.audit_link.is_none());
    assert!(overwritten.governance_link.is_none());
}

fn sample_code_index(knowledge_id: &str) -> CodeIndexIngestion {
    CodeIndexIngestion {
        knowledge_id: knowledge_id.to_string(),
        title: "Lookup helper".to_string(),
        content: "Ranks candidates for knowledge retrieval.".to_string(),
        tags: vec!["Search".to_string(), "Rust".to_string()],
        source_ref: Some("src/query.rs#execute".to_string()),
        updated_at: UtcMillis(100),
        source: CodeIndexSource {
            path: "crates/magi-knowledge-store/src/query.rs".to_string(),
            language: Some("Rust".to_string()),
            repo_ref: Some("magi-rust-rewrite".to_string()),
            commit_ref: Some("abc123".to_string()),
            start_line: Some(8),
            end_line: Some(77),
            symbol: Some(CodeIndexSymbol {
                name: "execute".to_string(),
                kind: CodeSymbolKind::Function,
                container: Some("KnowledgeQueryService".to_string()),
                signature: Some("execute(entries, index_terms, query)".to_string()),
            }),
        },
        audit: Some(KnowledgeAuditLink {
            audit_event_id: "audit-knowledge-index-1".to_string(),
            trail_ref: Some("knowledge.ingest".to_string()),
            sequence: Some(17),
        }),
        governance: Some(KnowledgeGovernanceLink {
            outcome: KnowledgeGovernanceOutcome::Allowed,
            policy_refs: vec![
                "knowledge.code_index".to_string(),
                "governance.safe_read".to_string(),
            ],
            rationale: Some("Repository-derived symbol summary".to_string()),
            audit_event_id: Some("audit-knowledge-index-1".to_string()),
        }),
    }
}

#[test]
fn query_filters_records_by_workspace_id() {
    let store = KnowledgeStore::new();
    let workspace_a = WorkspaceId::new("workspace-a");
    let workspace_b = WorkspaceId::new("workspace-b");

    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-a".to_string(),
        kind: KnowledgeKind::Faq,
        title: "Workspace A".to_string(),
        content: "alpha".to_string(),
        tags: vec!["faq".to_string()],
        workspace_id: Some(workspace_a.clone()),
        source_ref: None,
        updated_at: UtcMillis(1),
    });
    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-b".to_string(),
        kind: KnowledgeKind::Faq,
        title: "Workspace B".to_string(),
        content: "beta".to_string(),
        tags: vec!["faq".to_string()],
        workspace_id: Some(workspace_b.clone()),
        source_ref: None,
        updated_at: UtcMillis(2),
    });
    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-global".to_string(),
        kind: KnowledgeKind::Faq,
        title: "Legacy Global".to_string(),
        content: "legacy".to_string(),
        tags: vec!["faq".to_string()],
        workspace_id: None,
        source_ref: None,
        updated_at: UtcMillis(3),
    });

    let scoped = store.query(&KnowledgeQuery {
        kind: None,
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_a),
        limit: 10,
    });

    assert_eq!(scoped.total_matches, 1);
    assert_eq!(scoped.records[0].knowledge_id, "kb-a");
}

#[test]
fn project_code_index_summary_is_scoped_by_workspace() {
    let store = KnowledgeStore::new();
    let workspace_a = WorkspaceId::new("workspace-a");
    let workspace_b = WorkspaceId::new("workspace-b");

    store.ingest_code_index_in_workspace(
        workspace_a.clone(),
        project_code_index_summary("src/a.rs", UtcMillis(10)),
    );
    store.ingest_code_index_in_workspace(
        workspace_b.clone(),
        project_code_index_summary("src/b.rs", UtcMillis(20)),
    );

    assert!(store.get("project-code-index").is_none());
    let summary_a = store
        .code_index_summary_for_workspace(&workspace_a)
        .expect("workspace a summary should exist");
    let summary_b = store
        .code_index_summary_for_workspace(&workspace_b)
        .expect("workspace b summary should exist");

    assert_eq!(summary_a.files[0].path, "src/a.rs");
    assert_eq!(summary_b.files[0].path, "src/b.rs");
    assert!(store.code_index_summary().is_none());
}

#[test]
fn clear_workspace_and_delete_in_workspace_are_scoped() {
    let store = KnowledgeStore::new();
    let workspace_a = WorkspaceId::new("workspace-a");
    let workspace_b = WorkspaceId::new("workspace-b");

    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-a".to_string(),
        kind: KnowledgeKind::Faq,
        title: "Workspace A".to_string(),
        content: "alpha".to_string(),
        tags: vec![],
        workspace_id: Some(workspace_a.clone()),
        source_ref: None,
        updated_at: UtcMillis(1),
    });
    store.upsert(KnowledgeRecord {
        knowledge_id: "kb-b".to_string(),
        kind: KnowledgeKind::Faq,
        title: "Workspace B".to_string(),
        content: "beta".to_string(),
        tags: vec![],
        workspace_id: Some(workspace_b.clone()),
        source_ref: None,
        updated_at: UtcMillis(2),
    });

    store
        .delete_in_workspace("kb-a", &workspace_a)
        .expect("workspace owned record should delete");
    assert!(store.get("kb-a").is_none());
    assert!(store.get("kb-b").is_some());

    store.clear_workspace(&workspace_b);
    assert!(store.get("kb-b").is_none());
}

fn project_code_index_summary(path: &str, updated_at: UtcMillis) -> CodeIndexIngestion {
    let summary = CodeIndexSummary {
        files: vec![CodeIndexFile {
            path: path.to_string(),
            lines: Some(12),
            size: Some(240),
        }],
        tech_stack: vec!["Rust".to_string()],
        entry_points: vec![path.to_string()],
        last_indexed: updated_at.0,
    };
    CodeIndexIngestion {
        knowledge_id: "project-code-index".to_string(),
        title: format!("Project Code Index: {path}"),
        content: serde_json::to_string(&summary).expect("summary should serialize"),
        tags: vec!["Rust".to_string()],
        source_ref: Some(path.to_string()),
        updated_at,
        source: CodeIndexSource {
            path: path.to_string(),
            language: Some("Rust".to_string()),
            repo_ref: None,
            commit_ref: None,
            start_line: Some(1),
            end_line: Some(12),
            symbol: None,
        },
        audit: None,
        governance: None,
    }
}

#[test]
fn workspace_code_index_builds_and_searches_real_symbols() {
    use crate::local_search_engine::SearchOptions;
    use std::fs;

    // 在临时目录造一个含已知符号的小项目
    let base = std::env::temp_dir().join(format!("magi-ks-index-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src")).expect("create temp project dir");
    fs::write(
        base.join("src/auth.rs"),
        "pub fn authenticate_user(token: &str) -> bool {\n    !token.is_empty()\n}\n",
    )
    .expect("write source file");
    fs::write(
        base.join("src/util.rs"),
        "pub fn unrelated_helper() -> u32 {\n    42\n}\n",
    )
    .expect("write source file");

    let store = KnowledgeStore::new();
    let workspace_id = WorkspaceId::new("ws-index-test");

    // 装配前：引擎未就绪、检索为 None
    assert!(!store.workspace_index_ready(&workspace_id));
    assert!(
        store
            .search_workspace_code(&workspace_id, "authenticate", SearchOptions::default())
            .is_none()
    );

    // 构建索引
    store.build_workspace_index(&workspace_id, &base);

    // 装配后：引擎就绪、检索命中真实文件
    assert!(store.workspace_index_ready(&workspace_id));
    let results = store
        .search_workspace_code(&workspace_id, "authenticate user", SearchOptions::default())
        .expect("engine ready, results present");
    assert!(
        results.iter().any(|r| r.file_path.contains("auth.rs")),
        "search 应命中 auth.rs，实际: {:?}",
        results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn watcher_incrementally_refreshes_index_on_file_change() {
    use crate::local_search_engine::SearchOptions;
    use std::fs;

    // 需要 tokio 运行时（watcher 内部 spawn 去抖任务）。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let base = std::env::temp_dir().join(format!(
        "magi-ks-watch-test-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src")).expect("create temp project dir");
    fs::write(
        base.join("src/seed.rs"),
        "pub fn seed_symbol() -> u32 { 1 }\n",
    )
    .expect("write seed file");

    let store = KnowledgeStore::new();
    let workspace_id = WorkspaceId::new("ws-watch-test");

    // 在 tokio 上下文里构建索引——此时 watcher 一并启动。
    let _guard = runtime.enter();
    store.build_workspace_index(&workspace_id, &base);
    assert!(store.workspace_index_ready(&workspace_id));

    // 初始检索：新符号尚不存在。
    let before = store
        .search_workspace_code(
            &workspace_id,
            "freshly added symbol",
            SearchOptions::default(),
        )
        .expect("engine ready");
    assert!(
        !before.iter().any(|r| r.file_path.contains("added.rs")),
        "改动前不应命中 added.rs"
    );

    // 让 OS 文件监听注册稳定后再写入（避免刚 watch 就漏掉首个事件）。
    std::thread::sleep(std::time::Duration::from_millis(400));
    fs::write(
        base.join("src/added.rs"),
        "pub fn freshly_added_symbol() -> u32 { 42 }\n",
    )
    .expect("write added file");

    // 轮询等待 watcher 去抖 + 转发 + 增量更新落地（最多 ~5s，避免固定 sleep 抖动）。
    let mut hit = false;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let after = store
            .search_workspace_code(
                &workspace_id,
                "freshly added symbol",
                SearchOptions::default(),
            )
            .expect("engine ready");
        if after.iter().any(|r| r.file_path.contains("added.rs")) {
            hit = true;
            break;
        }
    }
    assert!(hit, "watcher 增量更新后应命中 added.rs");

    let _ = fs::remove_dir_all(&base);
}
