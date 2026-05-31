use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{DomainError, UtcMillis, WorkspaceId};
use magi_knowledge_store::{
    KnowledgeKind, KnowledgeQuery, KnowledgeRecord,
    code_scanner::{
        CodeIndexScanOutcome, CodeIndexScanStatus, CodeIndexSummary,
        ingest_workspace_code_index_in_workspace,
    },
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/knowledge", get(get_project_knowledge))
        .route("/knowledge/clear", post(clear_knowledge))
        .route(
            "/knowledge/items",
            get(list_knowledge_items).post(add_knowledge_item),
        )
        .route("/knowledge/items/search", get(search_knowledge_items))
        .route("/knowledge/items/update", post(update_knowledge_item))
        .route("/knowledge/items/delete", post(delete_knowledge_item))
}

const MIN_LEARNING_CONTENT_LENGTH: usize = 12;
const MAX_TAGS: usize = 8;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum KnowledgeKindParam {
    Adr,
    Faq,
    Learning,
}

impl KnowledgeKindParam {
    fn into_domain(self) -> KnowledgeKind {
        match self {
            Self::Adr => KnowledgeKind::Adr,
            Self::Faq => KnowledgeKind::Faq,
            Self::Learning => KnowledgeKind::Learning,
        }
    }
}

fn kind_to_lower(kind: KnowledgeKind) -> &'static str {
    match kind {
        KnowledgeKind::Adr => "adr",
        KnowledgeKind::Faq => "faq",
        KnowledgeKind::Learning => "learning",
        KnowledgeKind::CodeIndex => "codeIndex",
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeMutationResponse {
    success: bool,
    workspace_id: String,
    workspace_path: String,
    knowledge_count: usize,
}

fn registered_workspace_path(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<String, ApiError> {
    state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace.workspace_id == *workspace_id)
        .map(|workspace| workspace.root_path.to_string())
        .ok_or_else(|| ApiError::not_found("工作区不存在", workspace_id.as_str()))
}

fn mutation_response(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<KnowledgeMutationResponse, ApiError> {
    let workspace_path = registered_workspace_path(state, workspace_id)?;
    Ok(KnowledgeMutationResponse {
        success: true,
        workspace_id: workspace_id.as_str().to_string(),
        workspace_path,
        knowledge_count: state
            .knowledge_store
            .list()
            .into_iter()
            .filter(|record| {
                record.workspace_id.as_ref() == Some(workspace_id)
                    && record.kind != KnowledgeKind::CodeIndex
            })
            .count(),
    })
}

fn normalize_text(value: impl Into<String>, field: &str) -> Result<String, ApiError> {
    let normalized = value.into().trim().to_string();
    if normalized.is_empty() {
        return Err(ApiError::InvalidInput(format!("{field} 不能为空")));
    }
    Ok(normalized)
}

fn normalize_learning_content(value: impl Into<String>) -> Result<String, ApiError> {
    let content = normalize_text(value, "经验内容")?;
    if content.chars().count() < MIN_LEARNING_CONTENT_LENGTH {
        return Err(ApiError::InvalidInput(format!(
            "经验内容不能少于 {MIN_LEARNING_CONTENT_LENGTH} 个字符"
        )));
    }
    Ok(content)
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn normalize_workspace_id(value: Option<&str>) -> Option<WorkspaceId> {
    value
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty())
        .map(WorkspaceId::new)
}

fn require_workspace_id(value: Option<&str>) -> Result<WorkspaceId, ApiError> {
    normalize_workspace_id(value)
        .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))
}

fn require_registered_workspace_id(
    state: &ApiState,
    value: Option<&str>,
) -> Result<WorkspaceId, ApiError> {
    let workspace_id = require_workspace_id(value)?;
    if state
        .workspace_registry
        .workspaces()
        .into_iter()
        .any(|workspace| workspace.workspace_id == workspace_id)
    {
        Ok(workspace_id)
    } else {
        Err(ApiError::not_found("工作区不存在", workspace_id.as_str()))
    }
}

fn require_registered_workspace_binding(
    state: &ApiState,
    value: Option<&str>,
) -> Result<(WorkspaceId, String), ApiError> {
    let workspace_id = require_workspace_id(value)?;
    let workspace_path = registered_workspace_path(state, &workspace_id)?;
    Ok((workspace_id, workspace_path))
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_string();
        if !tag.is_empty() && !normalized.contains(&tag) {
            normalized.push(tag);
        }
        if normalized.len() >= MAX_TAGS {
            break;
        }
    }
    normalized
}

fn knowledge_item_json(record: &KnowledgeRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.knowledge_id,
        "kind": kind_to_lower(record.kind),
        "title": record.title,
        "content": record.content,
        "context": record.source_ref,
        "tags": record.tags,
        "createdAt": record.updated_at.0,
        "updatedAt": record.updated_at.0,
    })
}

fn title_from_learning(content: &str) -> String {
    let mut title = content.chars().take(80).collect::<String>();
    if content.chars().count() > 80 {
        title.push('…');
    }
    if title.trim().is_empty() {
        "Learning".to_string()
    } else {
        title
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeWorkspaceRequest {
    workspace_id: Option<String>,
}

async fn clear_knowledge(
    State(state): State<ApiState>,
    Json(request): Json<KnowledgeWorkspaceRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_registered_workspace_id(&state, request.workspace_id.as_deref())?;
    state.knowledge_store.clear_workspace(&workspace_id);
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeItemsQuery {
    kind: Option<KnowledgeKindParam>,
    workspace_id: Option<String>,
}

async fn list_knowledge_items(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) =
        require_registered_workspace_binding(&state, query.workspace_id.as_deref())?;
    let kq = KnowledgeQuery {
        kind: query.kind.map(KnowledgeKindParam::into_domain),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: 1000,
    };
    let result = state.knowledge_store.query(&kq);
    let items = result
        .matches
        .iter()
        .filter(|m| m.record.kind != KnowledgeKind::CodeIndex)
        .map(|m| knowledge_item_json(&m.record))
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "items": items
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchQuery {
    kind: Option<KnowledgeKindParam>,
    workspace_id: Option<String>,
    q: Option<String>,
}

async fn search_knowledge_items(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) =
        require_registered_workspace_binding(&state, query.workspace_id.as_deref())?;
    let text = query
        .q
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    let kq = KnowledgeQuery {
        kind: query.kind.map(KnowledgeKindParam::into_domain),
        text,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: 100,
    };
    let result = state.knowledge_store.query(&kq);
    let results = result
        .matches
        .iter()
        .filter(|m| m.record.kind != KnowledgeKind::CodeIndex)
        .map(|m| {
            let mut value = knowledge_item_json(&m.record);
            if let Some(object) = value.as_object_mut() {
                object.insert("score".to_string(), serde_json::json!(m.score));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "results": results
    })))
}

async fn get_project_knowledge(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) =
        require_registered_workspace_binding(&state, query.workspace_id.as_deref())?;
    let scan_outcome = ensure_workspace_code_index(&state, &workspace_id)?;

    let kq = KnowledgeQuery {
        kind: None,
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: 3000,
    };
    let result = state.knowledge_store.query(&kq);
    let items = result
        .matches
        .iter()
        .filter(|m| m.record.kind != KnowledgeKind::CodeIndex)
        .map(|m| knowledge_item_json(&m.record))
        .collect::<Vec<_>>();

    let code_index_summary = state
        .knowledge_store
        .code_index_summary_for_workspace(&workspace_id);
    let code_index_status = code_index_status_json(code_index_summary.as_ref(), &scan_outcome);
    let code_index = code_index_summary
        .map(|summary| {
            serde_json::json!({
                "files": summary.files.iter().map(|f| serde_json::json!({
                    "path": f.path,
                    "lines": f.lines,
                    "size": f.size,
                })).collect::<Vec<_>>(),
                "techStack": summary.tech_stack,
                "entryPoints": summary.entry_points,
            })
        })
        .unwrap_or(serde_json::Value::Null);

    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "items": items,
        "codeIndex": code_index,
        "codeIndexStatus": code_index_status,
    })))
}

fn ensure_workspace_code_index(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<CodeIndexScanOutcome, ApiError> {
    if state
        .knowledge_store
        .code_index_summary_for_workspace(workspace_id)
        .is_some_and(|summary| !summary.files.is_empty())
    {
        return Ok(CodeIndexScanOutcome::indexed_existing());
    }

    let Some(workspace) = state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace.workspace_id == *workspace_id)
    else {
        return Err(ApiError::not_found("工作区不存在", workspace_id.as_str()));
    };

    let outcome = ingest_workspace_code_index_in_workspace(
        &state.knowledge_store,
        workspace_id,
        &PathBuf::from(workspace.root_path.as_str()),
    );
    state.persist_knowledge_state_for_api()?;
    Ok(outcome)
}

fn code_index_status_json(
    summary: Option<&CodeIndexSummary>,
    scan_outcome: &CodeIndexScanOutcome,
) -> serde_json::Value {
    if summary.is_some_and(|summary| !summary.files.is_empty()) {
        return serde_json::json!({
            "status": CodeIndexScanStatus::Indexed.as_str(),
            "reasonCode": serde_json::Value::Null,
        });
    }
    serde_json::json!({
        "status": scan_outcome.status.as_str(),
        "reasonCode": scan_outcome.reason_code.as_ref().map(|reason| reason.as_str()),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddKnowledgeItemRequest {
    kind: KnowledgeKindParam,
    workspace_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    context: Option<String>,
}

async fn add_knowledge_item(
    State(state): State<ApiState>,
    Json(request): Json<AddKnowledgeItemRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_registered_workspace_id(&state, request.workspace_id.as_deref())?;
    let kind = request.kind.into_domain();
    let (id_prefix, title, content, source_ref) = match kind {
        KnowledgeKind::Adr => {
            let title = normalize_text(
                request
                    .title
                    .ok_or_else(|| ApiError::InvalidInput("ADR 标题 不能为空".to_string()))?,
                "ADR 标题",
            )?;
            let content = normalize_text(request.content, "ADR 内容")?;
            ("adr", title, content, None)
        }
        KnowledgeKind::Faq => {
            let title = normalize_text(
                request
                    .title
                    .ok_or_else(|| ApiError::InvalidInput("FAQ 问题 不能为空".to_string()))?,
                "FAQ 问题",
            )?;
            let content = normalize_text(request.content, "FAQ 答案")?;
            ("faq", title, content, None)
        }
        KnowledgeKind::Learning => {
            let content = normalize_learning_content(request.content)?;
            let title = match request.title {
                Some(value) => normalize_text(value, "经验标题")?,
                None => title_from_learning(&content),
            };
            let source_ref = normalize_optional_text(request.context);
            ("learning", title, content, source_ref)
        }
        KnowledgeKind::CodeIndex => {
            return Err(ApiError::InvalidInput("kind 不支持 codeIndex".to_string()));
        }
    };
    let record = KnowledgeRecord {
        knowledge_id: format!("{id_prefix}-{}", UtcMillis::now().0),
        kind,
        title,
        content,
        tags: normalize_tags(request.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKnowledgeItemRequest {
    workspace_id: Option<String>,
    knowledge_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    context: Option<String>,
}

async fn update_knowledge_item(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeItemRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_registered_workspace_id(&state, request.workspace_id.as_deref())?;
    let existing = state
        .knowledge_store
        .get(&request.knowledge_id)
        .ok_or_else(|| ApiError::not_found("知识记录不存在", &request.knowledge_id))?;
    if existing.workspace_id.as_ref() != Some(&workspace_id) {
        return Err(ApiError::not_found("知识记录不存在", &request.knowledge_id));
    }
    if existing.kind == KnowledgeKind::CodeIndex {
        return Err(ApiError::InvalidInput(
            "代码索引记录不能直接修改".to_string(),
        ));
    }
    let kind = existing.kind;
    let next_content = match request.content {
        Some(content) if kind == KnowledgeKind::Learning => normalize_learning_content(content)?,
        Some(content) => normalize_text(content, "知识内容")?,
        None => existing.content,
    };
    let next_title = match request.title {
        Some(title) => normalize_text(title, "知识标题")?,
        None if kind == KnowledgeKind::Learning => title_from_learning(&next_content),
        None => existing.title,
    };
    let next_source_ref = request
        .context
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or(existing.source_ref);
    let updated = KnowledgeRecord {
        knowledge_id: existing.knowledge_id,
        kind,
        title: next_title,
        content: next_content,
        tags: request.tags.map(normalize_tags).unwrap_or(existing.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref: next_source_ref,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(updated);
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteKnowledgeItemRequest {
    workspace_id: Option<String>,
    knowledge_id: String,
}

async fn delete_knowledge_item(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeItemRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_registered_workspace_id(&state, request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.knowledge_id, &workspace_id)
        .map_err(|error| map_knowledge_delete_error(error, &request.knowledge_id))?;
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

fn map_knowledge_delete_error(error: DomainError, id: &str) -> ApiError {
    match error {
        DomainError::NotFound { .. } | DomainError::InvalidState { .. } => {
            ApiError::not_found("知识记录不存在", id)
        }
        other => ApiError::internal_assembly("删除知识记录失败", other),
    }
}

#[cfg(test)]
mod tests {
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
            updated_at: UtcMillis::now(),
        });
    }

    fn register_test_workspace(state: &ApiState, workspace_id: &WorkspaceId) {
        let root = std::env::temp_dir().join(format!("magi-knowledge-{}", workspace_id.as_str()));
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new(root.to_string_lossy().to_string()),
            )
            .expect("workspace should register");
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

    #[tokio::test]
    async fn project_knowledge_returns_workspace_scoped_code_index() {
        let knowledge_store = KnowledgeStore::new();
        let workspace_a = WorkspaceId::new("workspace-knowledge-a");
        let workspace_b = WorkspaceId::new("workspace-knowledge-b");
        insert_code_index(&knowledge_store, &workspace_a, "src/a.rs");
        insert_code_index(&knowledge_store, &workspace_b, "src/b.rs");
        let state = state_with_knowledge_store(knowledge_store);
        register_test_workspace(&state, &workspace_a);
        register_test_workspace(&state, &workspace_b);

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
    async fn project_knowledge_lazily_indexes_registered_workspace_when_missing() {
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
            .with_state(state)
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
        let files = payload["codeIndex"]["files"]
            .as_array()
            .expect("code index files should exist");

        assert!(
            files
                .iter()
                .any(|file| file["path"].as_str() == Some("src/main.rs")),
            "knowledge endpoint should index the requested registered workspace"
        );
        assert_eq!(payload["codeIndexStatus"]["status"], "indexed");
    }

    #[tokio::test]
    async fn project_knowledge_reports_empty_workspace_without_persisting_zero_index() {
        let state = state_with_knowledge_store(KnowledgeStore::new());
        let workspace_id = WorkspaceId::new("workspace-empty-index");
        let root =
            std::env::temp_dir().join(format!("magi-knowledge-empty-index-{}", UtcMillis::now().0));
        fs::create_dir_all(root.join(".magi")).expect("workspace dir should create");
        fs::write(root.join(".magi/sessions.json"), "{}\n").expect("ignored file should write");
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

        assert!(payload["codeIndex"].is_null());
        assert_eq!(payload["codeIndexStatus"]["status"], "empty");
        assert_eq!(
            payload["codeIndexStatus"]["reasonCode"],
            "no_indexable_files"
        );
        assert!(
            state
                .knowledge_store
                .code_index_summary_for_workspace(&workspace_id)
                .is_none(),
            "空 workspace 不应被持久化为成功的 0 文件代码索引"
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
}
