use crate::{
    CodeIndexIngestion, CodeIndexSource, CodeIndexSymbol, CodeSymbolKind, KnowledgeAuditLink,
    KnowledgeGovernanceLink, KnowledgeGovernanceOutcome, KnowledgeKind, KnowledgeQuery,
    KnowledgeRecord, KnowledgeStore,
    code_scanner::{CodeIndexFile, CodeIndexScanStatus, CodeIndexSummary},
};
use magi_core::{UtcMillis, WorkspaceId};

#[test]
fn business_knowledge_query_matches_natural_chinese_phrases() {
    let store = KnowledgeStore::new();
    store.upsert(KnowledgeRecord {
        knowledge_id: "faq-refresh-token".to_string(),
        kind: KnowledgeKind::Faq,
        title: "登录失败后如何刷新令牌".to_string(),
        content: "先刷新令牌，再重试原请求。".to_string(),
        tags: vec!["登录".to_string(), "令牌".to_string()],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis(10),
        updated_at: UtcMillis(10),
    });

    let result = store.query(&KnowledgeQuery {
        kind: None,
        text: Some("登录失败时怎么刷新令牌".to_string()),
        tags: vec![],
        workspace_id: None,
        limit: 5,
    });

    assert_eq!(result.total_matches, 1);
    assert_eq!(result.matches[0].record.knowledge_id, "faq-refresh-token");
    assert!(
        result.matches[0]
            .matched_terms
            .iter()
            .any(|term| term == "登录" || term == "刷新" || term == "令牌")
    );
}

#[test]
fn business_knowledge_title_match_outranks_newer_content_match() {
    let store = KnowledgeStore::new();
    store.upsert(KnowledgeRecord {
        knowledge_id: "adr-title-match".to_string(),
        kind: KnowledgeKind::Adr,
        title: "架构决策".to_string(),
        content: "事件事实生成只读投影。".to_string(),
        tags: vec![],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis(10),
        updated_at: UtcMillis(10),
    });
    store.upsert(KnowledgeRecord {
        knowledge_id: "adr-content-match".to_string(),
        kind: KnowledgeKind::Adr,
        title: "运行态设计".to_string(),
        content: "这里讨论架构方案。".to_string(),
        tags: vec![],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis(20),
        updated_at: UtcMillis(20),
    });

    let result = store.query(&KnowledgeQuery {
        kind: Some(KnowledgeKind::Adr),
        text: Some("架构".to_string()),
        tags: vec![],
        workspace_id: None,
        limit: 5,
    });

    assert_eq!(result.matches.len(), 2);
    assert_eq!(result.matches[0].record.knowledge_id, "adr-title-match");
    assert!(result.matches[0].score > result.matches[1].score);
}

#[test]
fn business_knowledge_tag_match_outranks_content_match() {
    let store = KnowledgeStore::new();
    store.upsert(KnowledgeRecord {
        knowledge_id: "faq-tag-match".to_string(),
        kind: KnowledgeKind::Faq,
        title: "请求失败排查".to_string(),
        content: "检查认证状态。".to_string(),
        tags: vec!["令牌".to_string()],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis(10),
        updated_at: UtcMillis(10),
    });
    store.upsert(KnowledgeRecord {
        knowledge_id: "faq-content-match".to_string(),
        kind: KnowledgeKind::Faq,
        title: "认证排查".to_string(),
        content: "失败时刷新令牌。".to_string(),
        tags: vec![],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis(20),
        updated_at: UtcMillis(20),
    });

    let result = store.query(&KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: Some("令牌".to_string()),
        tags: vec![],
        workspace_id: None,
        limit: 5,
    });

    assert_eq!(result.matches.len(), 2);
    assert_eq!(result.matches[0].record.knowledge_id, "faq-tag-match");
    assert!(result.matches[0].score > result.matches[1].score);
}

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
        created_at: updated_at,
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
        created_at: updated_at,
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
fn upsert_preserves_original_creation_time() {
    let store = KnowledgeStore::new();
    let base = KnowledgeRecord {
        knowledge_id: "kb-created-at".to_string(),
        kind: KnowledgeKind::Learning,
        title: "Initial".to_string(),
        content: "initial".to_string(),
        tags: vec![],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis(10),
        updated_at: UtcMillis(10),
    };
    store.upsert(base.clone());
    store.upsert(KnowledgeRecord {
        title: "Updated".to_string(),
        content: "updated".to_string(),
        created_at: UtcMillis(20),
        updated_at: UtcMillis(20),
        ..base
    });

    let record = store.get("kb-created-at").expect("record should exist");
    assert_eq!(record.created_at, UtcMillis(10));
    assert_eq!(record.updated_at, UtcMillis(20));
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
        created_at: UtcMillis(50),
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
        created_at: UtcMillis(200),
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
        created_at: UtcMillis(1),
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
        created_at: UtcMillis(2),
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
        created_at: UtcMillis(3),
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
}

#[test]
fn workspace_code_index_health_uses_runtime_index_metadata() {
    let root = std::env::temp_dir().join(format!(
        "magi-ks-index-health-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("create temp workspace");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn health_probe_symbol() -> bool { true }\n",
    )
    .expect("write source");

    let store = KnowledgeStore::new();
    let workspace_id = WorkspaceId::new("workspace-index-health");
    let outcome = store.build_workspace_index(&workspace_id, &root);
    let summary = outcome
        .summary
        .expect("workspace should produce code index");
    let health = store
        .workspace_code_index_health(&workspace_id)
        .expect("runtime health should exist");

    assert!(health.is_ready);
    assert_eq!(health.file_count, 1);
    assert_eq!(health.last_indexed, Some(summary.last_indexed));
    assert_eq!(health.cache_status, "ready");
    assert!(health.cache_error_code.is_none());

    let _ = std::fs::remove_dir_all(root);
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
        created_at: UtcMillis(1),
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
        created_at: UtcMillis(2),
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

#[test]
fn clear_workspace_cancels_active_index_build_until_worker_finishes() {
    let store = KnowledgeStore::new();
    let workspace_id = WorkspaceId::new("workspace-cancel-index-build");
    let root = std::env::temp_dir().join(format!(
        "magi-knowledge-cancel-build-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    std::fs::create_dir_all(root.join("src")).expect("create temp workspace");
    std::fs::write(root.join("src/lib.rs"), "pub fn stale_build() {}\n").expect("write source");

    assert!(store.begin_workspace_index_build(&workspace_id));
    store.clear_workspace(&workspace_id);
    assert!(
        store.workspace_index_building(&workspace_id),
        "clear must keep the active worker occupied until it reaches its completion boundary"
    );
    store.build_workspace_index(&workspace_id, &root);
    store.upsert(KnowledgeRecord {
        knowledge_id: "knowledge-after-clear".to_string(),
        kind: KnowledgeKind::Learning,
        title: "Knowledge after clear".to_string(),
        content: "This record was created after the clear request.".to_string(),
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        source_ref: None,
        created_at: UtcMillis::now(),
        updated_at: UtcMillis::now(),
    });
    assert!(
        store
            .code_index_summary_for_workspace(&workspace_id)
            .is_some()
    );
    assert!(
        store.finish_workspace_index_build(&workspace_id),
        "the worker completion must report that its derived result should be discarded"
    );
    assert!(!store.workspace_index_building(&workspace_id));
    assert!(
        store
            .code_index_summary_for_workspace(&workspace_id)
            .is_none()
    );
    assert!(
        store.get("knowledge-after-clear").is_some(),
        "discarding a cancelled derived index must preserve knowledge created after clear"
    );

    let _ = std::fs::remove_dir_all(root);
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
fn workspace_code_search_does_not_cross_workspace_indexes() {
    use crate::local_search_engine::SearchOptions;
    use std::fs;

    let base = std::env::temp_dir().join(format!(
        "magi-ks-index-scope-test-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    let _ = fs::remove_dir_all(&base);
    let root_a = base.join("workspace-a");
    let root_b = base.join("workspace-b");
    fs::create_dir_all(root_a.join("src")).expect("create workspace a src");
    fs::create_dir_all(root_b.join("src")).expect("create workspace b src");
    fs::write(
        root_a.join("src/alpha.rs"),
        "pub fn exclusive_alpha_index_probe() -> bool { true }\n",
    )
    .expect("write workspace a source");
    fs::write(
        root_b.join("src/beta.rs"),
        "pub fn exclusive_beta_index_probe() -> bool { true }\n",
    )
    .expect("write workspace b source");

    let store = KnowledgeStore::new();
    let workspace_a = WorkspaceId::new("ws-index-scope-a");
    let workspace_b = WorkspaceId::new("ws-index-scope-b");

    store.build_workspace_index(&workspace_a, &root_a);
    store.build_workspace_index(&workspace_b, &root_b);
    assert!(store.workspace_index_ready(&workspace_a));
    assert!(store.workspace_index_ready(&workspace_b));

    let alpha_results = store
        .search_workspace_code(
            &workspace_a,
            "exclusive alpha index probe",
            SearchOptions::default(),
        )
        .expect("workspace a engine ready");
    assert!(
        alpha_results
            .iter()
            .any(|result| result.file_path.contains("alpha.rs")),
        "workspace a 应命中 alpha.rs，实际: {:?}",
        alpha_results
            .iter()
            .map(|result| &result.file_path)
            .collect::<Vec<_>>()
    );

    let beta_results_from_a = store
        .search_workspace_code(
            &workspace_a,
            "exclusive beta index probe",
            SearchOptions::default(),
        )
        .expect("workspace a engine ready");
    assert!(
        beta_results_from_a
            .iter()
            .all(|result| !result.file_path.contains("beta.rs")),
        "workspace a 不应返回 workspace b 的 beta.rs，实际: {:?}",
        beta_results_from_a
            .iter()
            .map(|result| &result.file_path)
            .collect::<Vec<_>>()
    );

    let beta_results = store
        .search_workspace_code(
            &workspace_b,
            "exclusive beta index probe",
            SearchOptions::default(),
        )
        .expect("workspace b engine ready");
    assert!(
        beta_results
            .iter()
            .any(|result| result.file_path.contains("beta.rs")),
        "workspace b 应命中 beta.rs，实际: {:?}",
        beta_results
            .iter()
            .map(|result| &result.file_path)
            .collect::<Vec<_>>()
    );

    let alpha_results_from_b = store
        .search_workspace_code(
            &workspace_b,
            "exclusive alpha index probe",
            SearchOptions::default(),
        )
        .expect("workspace b engine ready");
    assert!(
        alpha_results_from_b
            .iter()
            .all(|result| !result.file_path.contains("alpha.rs")),
        "workspace b 不应返回 workspace a 的 alpha.rs，实际: {:?}",
        alpha_results_from_b
            .iter()
            .map(|result| &result.file_path)
            .collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn workspace_code_search_cache_respects_search_options() {
    use crate::local_search_engine::SearchOptions;
    use std::fs;

    let base = std::env::temp_dir().join(format!(
        "magi-ks-cache-options-test-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src")).expect("create temp project dir");
    fs::write(
        base.join("src/alpha.rs"),
        "pub fn shared_cache_probe_alpha() -> bool { true }\n",
    )
    .expect("write alpha source");
    fs::write(
        base.join("src/beta.rs"),
        "pub fn shared_cache_probe_beta() -> bool { true }\n",
    )
    .expect("write beta source");

    let store = KnowledgeStore::new();
    let workspace_id = WorkspaceId::new("ws-cache-options-test");
    store.build_workspace_index(&workspace_id, &base);

    let first = store
        .search_workspace_code(
            &workspace_id,
            "shared cache probe",
            SearchOptions {
                max_results: Some(1),
                ..SearchOptions::default()
            },
        )
        .expect("engine ready");
    assert_eq!(first.len(), 1);

    let second = store
        .search_workspace_code(
            &workspace_id,
            "shared cache probe",
            SearchOptions {
                max_results: Some(10),
                ..SearchOptions::default()
            },
        )
        .expect("engine ready");
    assert!(
        second.len() >= 2,
        "不同 max_results 不应复用只含 1 条结果的缓存，实际: {:?}",
        second.iter().map(|r| &r.file_path).collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn workspace_index_rebuild_keeps_queryable_empty_runtime_when_workspace_has_no_indexable_files() {
    use crate::local_search_engine::SearchOptions;
    use std::fs;

    let base = std::env::temp_dir().join(format!(
        "magi-ks-index-clear-test-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src")).expect("create temp project dir");
    let source_path = base.join("src/auth.rs");
    fs::write(
        &source_path,
        "pub fn authenticate_user(token: &str) -> bool {\n    !token.is_empty()\n}\n",
    )
    .expect("write source file");

    let store = KnowledgeStore::new();
    let workspace_id = WorkspaceId::new("ws-index-clear-test");

    store.build_workspace_index(&workspace_id, &base);
    assert!(store.workspace_index_ready(&workspace_id));
    assert!(
        store
            .code_index_summary_for_workspace(&workspace_id)
            .is_some()
    );

    fs::remove_file(source_path).expect("remove source file");
    let outcome = store.build_workspace_index(&workspace_id, &base);

    assert_eq!(outcome.status, CodeIndexScanStatus::Empty);
    assert!(!store.workspace_index_ready(&workspace_id));
    assert!(
        store
            .search_workspace_code(&workspace_id, "authenticate", SearchOptions::default())
            .is_some_and(|results| results.is_empty())
    );
    assert!(
        store
            .code_index_summary_for_workspace(&workspace_id)
            .is_some_and(|summary| summary.files.is_empty()),
        "空 workspace 应保留空摘要，不能复活旧文件"
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
    let summary = store
        .code_index_summary_for_workspace(&workspace_id)
        .expect("runtime code index summary should exist");
    assert!(
        summary.files.iter().any(|file| file.path == "src/added.rs"),
        "知识库代码索引摘要必须跟随运行时增量索引更新，实际: {:?}",
        summary
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn workspace_code_search_supports_concurrent_queries() {
    use crate::local_search_engine::SearchOptions;
    use std::fs;

    let base = std::env::temp_dir().join(format!(
        "magi-ks-concurrent-search-{}-{}",
        std::process::id(),
        UtcMillis::now().0
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src")).expect("create src");
    fs::write(
        base.join("src/lib.rs"),
        "pub fn concurrent_search_probe() -> bool { true }\n",
    )
    .expect("write source");

    let store = std::sync::Arc::new(KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("ws-concurrent-search");
    store.build_workspace_index(&workspace_id, &base);
    let workers = (0..16)
        .map(|_| {
            let store = store.clone();
            let workspace_id = workspace_id.clone();
            std::thread::spawn(move || {
                store
                    .search_workspace_code(
                        &workspace_id,
                        "concurrent search probe",
                        SearchOptions::default(),
                    )
                    .expect("engine ready")
            })
        })
        .collect::<Vec<_>>();

    for worker in workers {
        assert!(
            worker
                .join()
                .expect("search worker")
                .iter()
                .any(|result| result.file_path == "src/lib.rs")
        );
    }
    let stats = store
        .workspace_index_stats(&workspace_id)
        .expect("workspace stats");
    assert_eq!(stats.query_count, 16);

    let _ = fs::remove_dir_all(&base);
}
