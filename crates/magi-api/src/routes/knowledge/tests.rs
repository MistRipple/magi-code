use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use magi_core::AbsolutePath;
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_knowledge_store::KnowledgeStore;
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use std::{fs, sync::Arc};
use tower::ServiceExt;

fn state_with_knowledge_store(knowledge_store: KnowledgeStore) -> ApiState {
    ApiState::new(
        "magi-test",
        Arc::new(InMemoryEventBus::new(32)),
        Arc::new(SessionStore::default()),
        Arc::new(WorkspaceStore::default()),
        Arc::new(GovernanceService::default()),
    )
    .with_knowledge_store(Arc::new(knowledge_store))
}

fn insert_code_index(store: &KnowledgeStore, workspace_id: &WorkspaceId, path: &str) {
    let summary = magi_knowledge_store::code_scanner::CodeIndexSummary {
        files: vec![magi_knowledge_store::code_scanner::CodeIndexFile {
            path: path.to_string(),
            lines: Some(10),
            size: Some(100),
        }],
        tech_stack: vec!["Rust".to_string()],
        entry_points: vec![path.to_string()],
        last_indexed: UtcMillis::now().0,
    };
    store.upsert(KnowledgeRecord {
        knowledge_id: format!("project-code-index:{}", workspace_id.as_str()),
        kind: KnowledgeKind::CodeIndex,
        title: format!("Project Code Index: {path}"),
        content: serde_json::to_string(&summary).expect("summary should serialize"),
        tags: vec!["rust".to_string()],
        workspace_id: Some(workspace_id.clone()),
        source_ref: Some(path.to_string()),
        created_at: UtcMillis::now(),
        updated_at: UtcMillis::now(),
    });
}

fn register_test_workspace(state: &ApiState, workspace_id: &WorkspaceId) -> PathBuf {
    let root = std::env::temp_dir().join(format!("magi-knowledge-{}", workspace_id.as_str()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("workspace dir should create");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new(root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");
    root
}

async fn read_json(response: axum::response::Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    serde_json::from_slice(&body).expect("payload should deserialize")
}

fn assert_workspace_binding(payload: &serde_json::Value, workspace_id: &WorkspaceId) {
    assert_eq!(payload["workspaceId"], workspace_id.as_str());
    let workspace_path = payload["workspacePath"]
        .as_str()
        .expect("workspacePath should be a string");
    let expected_suffix = format!("magi-knowledge-{}", workspace_id.as_str());
    assert!(
        workspace_path.ends_with(&expected_suffix),
        "payload must carry canonical workspace path, got {workspace_path}"
    );
}

async fn wait_for_index_build(state: &ApiState, workspace_id: &WorkspaceId) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.knowledge_store.workspace_index_building(workspace_id) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background code index build should finish");
}

#[test]
fn new_knowledge_id_keeps_same_millisecond_writes_unique() {
    let first = new_knowledge_id("learning");
    let second = new_knowledge_id("learning");

    assert_ne!(first, second);
    assert!(first.starts_with("learning-"));
    assert!(second.starts_with("learning-"));
}

#[test]
fn knowledge_requests_reject_legacy_snake_case_fields() {
    serde_json::from_value::<KnowledgeWorkspaceRequest>(serde_json::json!({
        "workspace_id": "workspace-knowledge"
    }))
    .expect_err("knowledge workspace request 不得继续接受 snake_case 请求字段");

    serde_json::from_value::<AddKnowledgeItemRequest>(serde_json::json!({
        "kind": "learning",
        "workspace_path": "/tmp/magi-knowledge",
        "content": "legacy payload"
    }))
    .expect_err("knowledge mutation request 不得继续接受 snake_case 请求字段");

    let request = serde_json::from_value::<AddKnowledgeItemRequest>(serde_json::json!({
        "kind": "learning",
        "workspacePath": "/tmp/magi-knowledge",
        "content": "canonical payload"
    }))
    .expect("canonical camelCase knowledge request");
    assert_eq!(
        request.workspace_path.as_deref(),
        Some("/tmp/magi-knowledge")
    );
}

#[tokio::test]
async fn project_knowledge_returns_workspace_scoped_code_index() {
    let knowledge_store = KnowledgeStore::new();
    let workspace_a = WorkspaceId::new("workspace-knowledge-a");
    let workspace_b = WorkspaceId::new("workspace-knowledge-b");
    insert_code_index(&knowledge_store, &workspace_a, "src/a.rs");
    insert_code_index(&knowledge_store, &workspace_b, "src/b.rs");
    let state = state_with_knowledge_store(knowledge_store);
    let root_a = register_test_workspace(&state, &workspace_a);
    let root_b = register_test_workspace(&state, &workspace_b);
    fs::create_dir_all(root_a.join("src")).expect("workspace a source dir should create");
    fs::create_dir_all(root_b.join("src")).expect("workspace b source dir should create");
    fs::write(root_a.join("src/a.rs"), "pub fn workspace_a() {}\n")
        .expect("workspace a source should write");
    fs::write(root_b.join("src/b.rs"), "pub fn workspace_b() {}\n")
        .expect("workspace b source should write");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-knowledge-a")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_a);
    assert_eq!(payload["codeIndex"]["files"][0]["path"], "src/a.rs");
    assert!(payload["items"].is_array(), "items 字段必须存在且为数组");
}

#[tokio::test]
async fn knowledge_graph_is_workspace_scoped_and_supports_query_limits() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_a = WorkspaceId::new("workspace-graph-a");
    let workspace_b = WorkspaceId::new("workspace-graph-b");
    let root_a = register_test_workspace(&state, &workspace_a);
    let root_b = register_test_workspace(&state, &workspace_b);
    fs::create_dir_all(root_a.join("src")).expect("workspace a source dir should create");
    fs::create_dir_all(root_b.join("src")).expect("workspace b source dir should create");
    fs::write(
        root_a.join("src/a.ts"),
        "import { beta } from './b';\nexport function alpha() { return beta(); }\n",
    )
    .expect("workspace a entry source should write");
    fs::write(
        root_a.join("src/b.ts"),
        "export function beta() { return 1; }\n",
    )
    .expect("workspace a dependency source should write");
    fs::write(
        root_b.join("src/other.ts"),
        "export function other() { return 2; }\n",
    )
    .expect("workspace b source should write");

    let first_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge/graph?workspaceId=workspace-graph-a")
                .body(Body::empty())
                .expect("graph request should build"),
        )
        .await
        .expect("graph route should respond");
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_payload = read_json(first_response).await;
    assert_eq!(first_payload["status"], "indexing");

    wait_for_index_build(&state, &workspace_a).await;
    let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/knowledge/graph?workspaceId=workspace-graph-a&focus=file:src/a.ts&depth=1&direction=forward&maxNodes=1&maxEdges=1")
                    .body(Body::empty())
                    .expect("bounded graph request should build"),
            )
            .await
            .expect("bounded graph route should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_a);
    assert_eq!(payload["status"], "ready");
    assert_eq!(payload["nodes"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["edges"].as_array().map(Vec::len), Some(0));
    assert_eq!(payload["truncated"], true);
    let node_ids = payload["nodes"]
        .as_array()
        .expect("graph nodes should be an array")
        .iter()
        .filter_map(|node| node["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        node_ids.iter().all(|id| !id.contains("workspace-graph-b")),
        "graph must not expose another workspace"
    );
}

#[tokio::test]
async fn knowledge_relation_routes_round_trip_with_workspace_scope() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-relation-api");
    let root = register_test_workspace(&state, &workspace_id);
    state.knowledge_store.upsert(KnowledgeRecord {
        knowledge_id: "adr-relation-api".into(),
        kind: KnowledgeKind::Adr,
        title: "Relation API".into(),
        content: "API relation test".into(),
        tags: Vec::new(),
        workspace_id: Some(workspace_id.clone()),
        source_ref: None,
        created_at: UtcMillis(1),
        updated_at: UtcMillis(1),
    });

    let create_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/knowledge/relations")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "scope": "workspace",
                        "workspaceId": workspace_id.as_str(),
                        "workspacePath": root.to_string_lossy(),
                        "source": { "kind": "knowledge", "knowledgeId": "adr-relation-api" },
                        "kind": "applies_to",
                        "target": { "kind": "file", "path": "src/feature.ts" },
                        "origin": "explicit_user",
                        "confidence": 0.9,
                        "evidence": ["manual association"],
                    })
                    .to_string(),
                ))
                .expect("relation create request should build"),
        )
        .await
        .expect("relation create route should respond");
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_payload = read_json(create_response).await;
    let relation_id = create_payload["relation"]["relationId"]
        .as_str()
        .expect("created relation should have an id")
        .to_string();
    assert_eq!(
        create_payload["relation"]["source"]["knowledgeId"],
        "adr-relation-api"
    );

    let list_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/knowledge/relations?scope=workspace&workspaceId={}&workspacePath={}",
                    workspace_id.as_str(),
                    root.to_string_lossy()
                ))
                .body(Body::empty())
                .expect("relation list request should build"),
        )
        .await
        .expect("relation list route should respond");
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_payload = read_json(list_response).await;
    assert_eq!(list_payload["relations"].as_array().map(Vec::len), Some(1));

    let update_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/knowledge/relations/update")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "scope": "workspace",
                        "workspaceId": workspace_id.as_str(),
                        "workspacePath": root.to_string_lossy(),
                        "relationId": relation_id,
                        "source": { "kind": "knowledge", "knowledgeId": "adr-relation-api" },
                        "kind": "references",
                        "target": {
                            "kind": "symbol",
                            "path": "src/feature.ts",
                            "qualifiedName": "feature",
                            "symbolKind": "function"
                        },
                        "origin": "explicit_user",
                        "status": "active",
                        "confidence": 1.0,
                        "evidence": ["updated"],
                    })
                    .to_string(),
                ))
                .expect("relation update request should build"),
        )
        .await
        .expect("relation update route should respond");
    assert_eq!(update_response.status(), StatusCode::OK);
    let update_payload = read_json(update_response).await;
    assert_eq!(update_payload["relation"]["kind"], "references");

    let delete_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/knowledge/relations/delete")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "scope": "workspace",
                        "workspaceId": workspace_id.as_str(),
                        "workspacePath": root.to_string_lossy(),
                        "relationId": relation_id,
                    })
                    .to_string(),
                ))
                .expect("relation delete request should build"),
        )
        .await
        .expect("relation delete route should respond");
    assert_eq!(delete_response.status(), StatusCode::OK);
    assert!(
        state
            .knowledge_store
            .list_relations(&workspace_id)
            .is_empty()
    );
}

#[tokio::test]
async fn project_knowledge_resolves_workspace_from_registered_path_when_query_id_is_stale() {
    let knowledge_store = KnowledgeStore::new();
    let workspace_b = WorkspaceId::new("workspace-knowledge-path-b");
    insert_code_index(&knowledge_store, &workspace_b, "src/b.rs");
    let state = state_with_knowledge_store(knowledge_store);
    let root_b = register_test_workspace(&state, &workspace_b);
    fs::create_dir_all(root_b.join("src")).expect("workspace source dir should create");
    fs::write(root_b.join("src/b.rs"), "pub fn workspace_b() {}\n")
        .expect("workspace source should write");

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/knowledge?workspaceId=workspace-stale-query&workspacePath={}",
                    root_b.to_string_lossy()
                ))
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_b);
    assert_eq!(payload["codeIndex"]["files"][0]["path"], "src/b.rs");
}

#[tokio::test]
async fn knowledge_mutation_resolves_workspace_from_registered_path_when_payload_id_is_stale() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_b = WorkspaceId::new("workspace-knowledge-mutation-path-b");
    let root_b = register_test_workspace(&state, &workspace_b);

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/knowledge/items")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "kind": "learning",
                        "workspaceId": "workspace-stale-payload",
                        "workspacePath": root_b.to_string_lossy(),
                        "content": "Knowledge writes must follow the registered workspace path.",
                        "tags": ["scope"],
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_b);
    assert_eq!(payload["knowledgeCount"], 1);
    assert!(
        state
            .knowledge_store
            .query(&KnowledgeQuery {
                kind: Some(KnowledgeKind::Learning),
                text: Some("registered workspace path".to_string()),
                tags: vec![],
                workspace_id: Some(workspace_b),
                limit: 10,
            })
            .matches
            .iter()
            .any(|entry| entry.record.title.contains("Knowledge writes")),
        "mutation should write into the workspace resolved from workspacePath"
    );
}

#[tokio::test]
async fn project_knowledge_rejects_unknown_workspace() {
    let knowledge_store = KnowledgeStore::new();
    knowledge_store.upsert(KnowledgeRecord {
        knowledge_id: "learning-global".to_string(),
        kind: KnowledgeKind::Learning,
        title: "Legacy global learning".to_string(),
        content: "legacy global learning should stay hidden".to_string(),
        tags: vec![],
        workspace_id: None,
        source_ref: None,
        created_at: UtcMillis::now(),
        updated_at: UtcMillis::now(),
    });
    let state = state_with_knowledge_store(knowledge_store);

    let response = routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-missing")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn project_knowledge_schedules_missing_index_without_blocking_response() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-lazy-index");
    let root =
        std::env::temp_dir().join(format!("magi-knowledge-lazy-index-{}", UtcMillis::now().0));
    fs::create_dir_all(root.join("src")).expect("workspace dir should create");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("source file should write");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new(root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-lazy-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert!(payload["codeIndex"].is_null());
    assert_eq!(payload["codeIndexStatus"]["status"], "indexing");

    wait_for_index_build(&state, &workspace_id).await;
    assert!(state.knowledge_store.workspace_index_ready(&workspace_id));

    let ready_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-lazy-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("ready route should respond");
    let ready_payload = read_json(ready_response).await;
    let files = ready_payload["codeIndex"]["files"]
        .as_array()
        .expect("code index files should exist after background build");
    assert!(
        files
            .iter()
            .any(|file| file["path"].as_str() == Some("src/main.rs")),
        "background index should include the workspace source file"
    );
    assert_eq!(ready_payload["codeIndexStatus"]["status"], "indexed");
}

#[tokio::test]
async fn knowledge_relations_keep_same_paths_in_distinct_source_folders() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let fixture = tempfile::tempdir().unwrap();
    let a = fixture.path().join("a");
    let b = fixture.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let id = WorkspaceId::new("relation-source-boundary");
    state
        .workspace_registry
        .register_native_path(id.clone(), a)
        .unwrap();
    let project = crate::routes::test_project_fixtures::add_source(&state, id.as_str(), "b", &b);
    state.knowledge_store.upsert(KnowledgeRecord {
        knowledge_id: "shared-adr".into(),
        kind: KnowledgeKind::Adr,
        title: "Shared decision".into(),
        content: "Applies independently to each source".into(),
        tags: vec![],
        workspace_id: Some(id.clone()),
        source_ref: None,
        created_at: UtcMillis(1),
        updated_at: UtcMillis(1),
    });
    let body = |source: &str, relation: &str| {
        serde_json::json!({
            "workspaceId": id.as_str(), "sourceFolderId": source, "relationId": relation,
            "source": {"kind":"knowledge", "knowledgeId":"shared-adr"},
            "kind":"applies_to", "target":{"kind":"file", "path":"src/index.ts"}
        })
    };
    let first = &project.primary_source_folder_id;
    let mut ambiguous = body(first, "ambiguous-relation");
    ambiguous.as_object_mut().unwrap().remove("sourceFolderId");
    assert!(matches!(
        add_knowledge_relation(
            State(state.clone()),
            Json(serde_json::from_value(ambiguous).unwrap()),
        )
        .await,
        Err(ApiError::InvalidInput(_))
    ));
    assert!(matches!(
        list_knowledge_relations(
            State(state.clone()),
            Query(serde_json::from_value(serde_json::json!({"workspaceId": id})).unwrap()),
        )
        .await,
        Err(ApiError::InvalidInput(_))
    ));
    for source in [first.as_str(), "b"] {
        let relation_id = format!("relation-{source}");
        let response = add_knowledge_relation(
            State(state.clone()),
            Json(serde_json::from_value(body(source, &relation_id)).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(response["relation"]["codeScope"]["sourceFolderId"], source);
        let response = list_knowledge_relations(
            State(state.clone()),
            Query(
                serde_json::from_value(serde_json::json!({
                    "workspaceId": id.as_str(), "sourceFolderId": source
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(response["relations"].as_array().unwrap().len(), 1);
        assert_eq!(response["relations"][0]["relationId"], relation_id);
    }
    let first_relation = format!("relation-{first}");
    for update in [false, true] {
        let request = Json(serde_json::from_value(body("b", &first_relation)).unwrap());
        let result = if update {
            update_knowledge_relation(State(state.clone()), request).await
        } else {
            add_knowledge_relation(State(state.clone()), request).await
        };
        assert!(result.is_err(), "不能用另一目录覆盖原关系");
    }
    let deletion = |source: &str, relation: &str| {
        Json(
            serde_json::from_value(serde_json::json!({
                "workspaceId": id.as_str(), "sourceFolderId": source, "relationId": relation
            }))
            .unwrap(),
        )
    };
    assert!(
        delete_knowledge_relation(State(state.clone()), deletion("b", &first_relation))
            .await
            .is_err()
    );
    let _ = delete_knowledge_relation(State(state.clone()), deletion(first, &first_relation))
        .await
        .unwrap();
    assert!(
        state
            .knowledge_store
            .for_source_folder("b", &b)
            .unwrap()
            .relation("relation-b", &id)
            .is_some()
    );
}

#[tokio::test]
async fn project_knowledge_keeps_source_indexes_and_revocation_separate() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let fixture = tempfile::tempdir().unwrap();
    let a = fixture.path().join("a");
    let b = fixture.path().join("b");
    for (root, content) in [
        (&a, "export function onlyAlpha() {}\n"),
        (&b, "export function onlyBeta() {}\n"),
    ] {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/index.ts"), content).unwrap();
    }
    let id = WorkspaceId::new("knowledge-multi-source");
    let original = state
        .workspace_registry
        .register_native_path(id.clone(), a.clone())
        .unwrap();
    let a_id = original.primary_source_folder_id.clone();
    let mut second = original.primary_source_folder().clone();
    second.source_folder_id = "b".into();
    let host = magi_core::HostPath::canonicalize(&b).unwrap();
    second.path = AbsolutePath::new(host.display_string());
    second.path_ref = host.to_path_ref().as_str().into();
    let mut members = original.source_folders.clone();
    members.push(second.clone());
    state
        .workspace_registry
        .edit_project(
            &id,
            1,
            magi_workspace::WorkspaceProjectEdit {
                name: Some("multi-source".into()),
                source_folders: members,
                primary_source_folder_id: "b".into(),
            },
            |_| Ok(()),
        )
        .unwrap();
    let request = |source: Option<&str>| {
        Request::builder()
            .uri(format!(
                "/knowledge?workspaceId={}{}",
                id.as_str(),
                source
                    .map(|id| format!("&sourceFolderId={id}"))
                    .unwrap_or_default()
            ))
            .body(Body::empty())
            .unwrap()
    };
    let response = routes()
        .with_state(state.clone())
        .oneshot(request(None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    for source in [a_id.as_str(), "b"] {
        let response = routes()
            .with_state(state.clone())
            .oneshot(request(Some(source)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let root = if source == "b" { &b } else { &a };
        let view = state
            .knowledge_store
            .for_source_folder(source, root)
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while view.workspace_index_building(&id) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let response = routes()
            .with_state(state.clone())
            .oneshot(request(Some(source)))
            .await
            .unwrap();
        let payload = read_json(response).await;
        assert_eq!(payload["sourceFolderId"], source);
        assert_eq!(payload["codeIndex"]["files"][0]["path"], "src/index.ts");
        assert_eq!(payload["codeIndex"]["files"][0]["sourceFolderId"], source);
        let nodes = payload["graph"]["nodes"].as_array().unwrap();
        assert!(
            nodes
                .iter()
                .any(|node| node["metadata"]["sourceFolderId"] == source)
        );
        assert!(!nodes.iter().any(|node| node["label"]
            == if source == "b" {
                "onlyAlpha"
            } else {
                "onlyBeta"
            }));
    }
    fs::remove_dir_all(&b).unwrap();
    let response = routes()
        .with_state(state.clone())
        .oneshot(request(Some("b")))
        .await
        .unwrap();
    let payload = read_json(response).await;
    assert_eq!(payload["codeIndexStatus"]["status"], "failed");
    assert!(payload["codeIndex"].is_null());
    assert!(!b.exists());
    // 撤销成员后，旧缓存仍不能被旧目录 ID 重新取出。
    state
        .workspace_registry
        .edit_project(
            &id,
            2,
            magi_workspace::WorkspaceProjectEdit {
                name: Some("renamed".into()),
                source_folders: original.source_folders,
                primary_source_folder_id: a_id.clone(),
            },
            |_| Ok(()),
        )
        .unwrap();
    let response = routes()
        .with_state(state.clone())
        .oneshot(request(Some("b")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    state.knowledge_store.clear_workspace(&id);
}

#[tokio::test]
async fn project_knowledge_revalidates_ready_index_when_workspace_root_disappears() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-stale-ready-index");
    let root = std::env::temp_dir().join(format!(
        "magi-knowledge-stale-ready-index-{}",
        UtcMillis::now().0
    ));
    fs::create_dir_all(root.join("src")).expect("workspace dir should create");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("source file should write");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new(root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");

    let first_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-stale-ready-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(first_response.status(), StatusCode::OK);
    let first_payload = read_json(first_response).await;
    assert_eq!(first_payload["codeIndexStatus"]["status"], "indexing");
    wait_for_index_build(&state, &workspace_id).await;

    let ready_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-stale-ready-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("ready route should respond");
    let ready_payload = read_json(ready_response).await;
    assert_eq!(ready_payload["codeIndexStatus"]["status"], "indexed");
    assert!(
        state.knowledge_store.workspace_index_ready(&workspace_id),
        "initial request should build a ready runtime index"
    );

    fs::remove_dir_all(&root).expect("workspace dir should remove");

    let second_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-stale-ready-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(second_response.status(), StatusCode::OK);
    let second_payload = read_json(second_response).await;
    assert!(second_payload["codeIndex"].is_null());
    assert_eq!(second_payload["codeIndexStatus"]["status"], "failed");
    assert_eq!(
        second_payload["codeIndexStatus"]["reasonCode"],
        "workspace_missing"
    );
    assert!(
        !state.knowledge_store.workspace_index_ready(&workspace_id),
        "missing workspace root should clear the stale runtime index"
    );
    assert!(
        state
            .knowledge_store
            .code_index_summary_for_workspace(&workspace_id)
            .is_none(),
        "missing workspace root should clear the stale durable code index summary"
    );
}

#[tokio::test]
async fn project_knowledge_reports_queryable_empty_runtime_without_persisting_zero_index() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-empty-index");
    let root =
        std::env::temp_dir().join(format!("magi-knowledge-empty-index-{}", UtcMillis::now().0));
    fs::create_dir_all(root.join(".magi")).expect("workspace dir should create");
    fs::write(root.join(".magi/unrelated.json"), "{}\n").expect("ignored file should write");
    state
        .workspace_registry
        .register(
            workspace_id.clone(),
            AbsolutePath::new(root.to_string_lossy().to_string()),
        )
        .expect("workspace should register");

    let response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-empty-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("route should respond");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;

    assert_eq!(payload["codeIndexStatus"]["status"], "indexing");
    wait_for_index_build(&state, &workspace_id).await;

    let settled_response = routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/knowledge?workspaceId=workspace-empty-index")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("settled route should respond");
    let payload = read_json(settled_response).await;

    assert_eq!(
        payload["codeIndex"]["files"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(payload["codeIndexStatus"]["status"], "empty");
    assert_eq!(
        payload["codeIndexStatus"]["reasonCode"],
        "no_indexable_files"
    );
    assert!(
        state
            .knowledge_store
            .code_index_summary_for_workspace(&workspace_id)
            .is_some_and(|summary| summary.files.is_empty()),
        "空 workspace 应提供可查询的空运行时索引"
    );
    assert!(
        state.knowledge_store.list().into_iter().all(|record| {
            record.kind != KnowledgeKind::CodeIndex
                || record.workspace_id.as_ref() != Some(&workspace_id)
        }),
        "空 workspace 不应持久化伪造的 0 文件代码索引知识记录"
    );
}

#[tokio::test]
async fn unified_knowledge_routes_round_trip() {
    let state = state_with_knowledge_store(KnowledgeStore::new());
    let workspace_id = WorkspaceId::new("workspace-unified-knowledge");
    register_test_workspace(&state, &workspace_id);
    let app = || routes().with_state(state.clone());

    let post_json = |uri: &'static str, body: serde_json::Value| {
        let body_str = body.to_string();
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .expect("request should build")
    };

    let get_uri = |uri: &str| {
        Request::builder()
            .method("GET")
            .uri(uri.to_string())
            .body(Body::empty())
            .expect("request should build")
    };

    // add adr
    let response = app()
        .oneshot(post_json(
            "/knowledge/items",
            serde_json::json!({
                "kind": "adr",
                "workspaceId": workspace_id.as_str(),
                "title": "Adopt Rust workspace",
                "content": "We adopt Rust workspace.",
                "tags": ["arch"],
            }),
        ))
        .await
        .expect("add adr should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    assert_eq!(payload["knowledgeCount"], 1);

    // add faq
    let response = app()
        .oneshot(post_json(
            "/knowledge/items",
            serde_json::json!({
                "kind": "faq",
                "workspaceId": workspace_id.as_str(),
                "title": "How to build?",
                "content": "Run cargo build.",
                "tags": [],
            }),
        ))
        .await
        .expect("add faq should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    assert_eq!(payload["knowledgeCount"], 2);

    // add learning
    let response = app()
        .oneshot(post_json(
            "/knowledge/items",
            serde_json::json!({
                "kind": "learning",
                "workspaceId": workspace_id.as_str(),
                "content": "Always trim user input before normalization.",
                "context": "input-pipeline",
                "tags": ["lesson"],
            }),
        ))
        .await
        .expect("add learning should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    assert_eq!(payload["knowledgeCount"], 3);

    // list (no kind)
    let response = app()
        .oneshot(get_uri(&format!(
            "/knowledge/items?workspaceId={}",
            workspace_id.as_str()
        )))
        .await
        .expect("list should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    let items = payload["items"].as_array().expect("items array");
    assert_eq!(items.len(), 3);
    let kinds: Vec<&str> = items
        .iter()
        .map(|i| i["kind"].as_str().expect("kind str"))
        .collect();
    assert!(kinds.contains(&"adr"));
    assert!(kinds.contains(&"faq"));
    assert!(kinds.contains(&"learning"));
    for item in items {
        let context = &item["context"];
        if item["kind"] == "learning" {
            assert_eq!(context, "input-pipeline");
        } else {
            assert!(context.is_null(), "non-learning context should be null");
        }
    }

    // list filtered
    let response = app()
        .oneshot(get_uri(&format!(
            "/knowledge/items?kind=adr&workspaceId={}",
            workspace_id.as_str()
        )))
        .await
        .expect("filtered list should respond");
    let payload = read_json(response).await;
    let items = payload["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "adr");
    let learning_id = state
        .knowledge_store
        .list()
        .into_iter()
        .find(|r| r.kind == KnowledgeKind::Learning)
        .expect("learning record exists")
        .knowledge_id;

    // search
    let response = app()
        .oneshot(get_uri(&format!(
            "/knowledge/items/search?q=cargo&workspaceId={}",
            workspace_id.as_str()
        )))
        .await
        .expect("search should respond");
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    let results = payload["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|r| r["kind"] == "faq"),
        "search should hit faq"
    );

    // update
    let response = app()
        .oneshot(post_json(
            "/knowledge/items/update",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "knowledgeId": learning_id,
                "tags": ["lesson", "verified"],
            }),
        ))
        .await
        .expect("update should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    assert_eq!(payload["knowledgeCount"], 3);

    // delete
    let response = app()
        .oneshot(post_json(
            "/knowledge/items/delete",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
                "knowledgeId": learning_id,
            }),
        ))
        .await
        .expect("delete should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    assert_eq!(payload["knowledgeCount"], 2);

    // confirm count after delete
    let response = app()
        .oneshot(get_uri(&format!(
            "/knowledge/items?workspaceId={}",
            workspace_id.as_str()
        )))
        .await
        .expect("final list should respond");
    let payload = read_json(response).await;
    assert_eq!(payload["items"].as_array().unwrap().len(), 2);

    // clear
    let response = app()
        .oneshot(post_json(
            "/knowledge/clear",
            serde_json::json!({
                "workspaceId": workspace_id.as_str(),
            }),
        ))
        .await
        .expect("clear should respond");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_json(response).await;
    assert_workspace_binding(&payload, &workspace_id);
    assert_eq!(payload["knowledgeCount"], 0);
}
