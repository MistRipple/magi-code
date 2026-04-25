use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{DomainError, UtcMillis, WorkspaceId};
use magi_knowledge_store::{KnowledgeKind, KnowledgeQuery, KnowledgeRecord};
use serde::{Deserialize, Serialize};

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/knowledge", get(get_project_knowledge))
        .route("/knowledge/clear", post(clear_knowledge))
        .route("/knowledge/adrs", get(list_adrs))
        .route("/knowledge/adrs/search", get(search_adrs))
        .route("/knowledge/faqs", get(list_faqs))
        .route("/knowledge/faqs/search", get(search_faqs))
        .route("/knowledge/learnings", get(list_learnings))
        .route("/knowledge/learnings/search", get(search_learnings))
        .route("/knowledge/adr/add", post(add_adr))
        .route("/knowledge/adr/update", post(update_adr))
        .route("/knowledge/adr/delete", post(delete_adr))
        .route("/knowledge/faq/add", post(add_faq))
        .route("/knowledge/faq/update", post(update_faq))
        .route("/knowledge/faq/delete", post(delete_faq))
        .route("/knowledge/learning/add", post(add_learning))
        .route("/knowledge/learning/update", post(update_learning))
        .route("/knowledge/learning/delete", post(delete_learning))
}

const MIN_LEARNING_CONTENT_LENGTH: usize = 12;
const MAX_TAGS: usize = 8;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeMutationResponse {
    success: bool,
    knowledge_count: usize,
}

fn mutation_response(state: &ApiState, workspace_id: &WorkspaceId) -> KnowledgeMutationResponse {
    KnowledgeMutationResponse {
        success: true,
        knowledge_count: state
            .knowledge_store
            .list()
            .into_iter()
            .filter(|record| record.workspace_id.as_ref() == Some(workspace_id))
            .count(),
    }
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

fn knowledge_record_json(record: &KnowledgeRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.knowledge_id,
        "title": record.title,
        "content": record.content,
        "tags": record.tags,
        "updatedAt": record.updated_at.0,
    })
}

fn learning_record_json(record: &KnowledgeRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.knowledge_id,
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
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    state.knowledge_store.clear_workspace(&workspace_id);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeListQuery {
    workspace_id: Option<String>,
}

async fn list_adrs(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Adr),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Ok(Json(serde_json::json!({
        "adrs": result.matches.iter().map(|m| knowledge_record_json(&m.record)).collect::<Vec<_>>(),
    })))
}

async fn get_project_knowledge(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    let adrs_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Adr),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: 1000,
    };
    let adrs_result = state.knowledge_store.query(&adrs_query);
    let adrs = adrs_result
        .matches
        .iter()
        .map(|m| knowledge_record_json(&m.record))
        .collect::<Vec<_>>();

    let faqs_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: 1000,
    };
    let faqs_result = state.knowledge_store.query(&faqs_query);
    let faqs = faqs_result
        .matches
        .iter()
        .map(|m| knowledge_record_json(&m.record))
        .collect::<Vec<_>>();

    let learnings_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Learning),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: 1000,
    };
    let learnings_result = state.knowledge_store.query(&learnings_query);
    let learnings = learnings_result
        .matches
        .iter()
        .map(|m| learning_record_json(&m.record))
        .collect::<Vec<_>>();

    // 从知识存储中获取代码索引摘要
    let code_index = state
        .knowledge_store
        .code_index_summary_for_workspace(&workspace_id)
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
        "adrs": adrs,
        "faqs": faqs,
        "learnings": learnings,
        "codeIndex": code_index,
    })))
}

async fn list_faqs(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Ok(Json(serde_json::json!({
        "faqs": result.matches.iter().map(|m| knowledge_record_json(&m.record)).collect::<Vec<_>>(),
    })))
}

async fn list_learnings(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Learning),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Ok(Json(serde_json::json!({
        "learnings": result.matches.iter().map(|m| learning_record_json(&m.record)).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchQuery {
    q: Option<String>,
    workspace_id: Option<String>,
}

impl KnowledgeSearchQuery {
    fn text(self) -> Option<String> {
        self.q
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    }
}

async fn search_adrs(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    Ok(search_knowledge(
        &state,
        KnowledgeKind::Adr,
        query.text(),
        workspace_id,
    ))
}

async fn search_faqs(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    Ok(search_knowledge(
        &state,
        KnowledgeKind::Faq,
        query.text(),
        workspace_id,
    ))
}

async fn search_learnings(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    Ok(search_knowledge(
        &state,
        KnowledgeKind::Learning,
        query.text(),
        workspace_id,
    ))
}

fn search_knowledge(
    state: &ApiState,
    kind: KnowledgeKind,
    text: Option<String>,
    workspace_id: WorkspaceId,
) -> Json<serde_json::Value> {
    let kq = KnowledgeQuery {
        kind: Some(kind),
        text,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 100,
    };
    let result = state.knowledge_store.query(&kq);
    Json(serde_json::json!({
        "results": result.matches.iter().map(|m| {
            let mut value = if kind == KnowledgeKind::Learning {
                learning_record_json(&m.record)
            } else {
                knowledge_record_json(&m.record)
            };
            if let Some(object) = value.as_object_mut() {
                object.insert("score".to_string(), serde_json::json!(m.score));
            }
            value
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddAdrRequest {
    adr: AdrInput,
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdrInput {
    title: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

async fn add_adr(
    State(state): State<ApiState>,
    Json(request): Json<AddAdrRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    let id = format!("adr-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Adr,
        title: normalize_text(request.adr.title, "ADR 标题")?,
        content: normalize_text(request.adr.content, "ADR 内容")?,
        tags: normalize_tags(request.adr.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref: None,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKnowledgeRequest {
    id: String,
    updates: KnowledgeUpdates,
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeUpdates {
    title: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
    source_ref: Option<String>,
}

async fn update_adr(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    update_knowledge_record(&state, &workspace_id, &request.id, &request.updates)?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn update_faq(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    update_knowledge_record(&state, &workspace_id, &request.id, &request.updates)?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn update_learning(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    update_knowledge_record(&state, &workspace_id, &request.id, &request.updates)?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

fn update_knowledge_record(
    state: &ApiState,
    workspace_id: &WorkspaceId,
    id: &str,
    updates: &KnowledgeUpdates,
) -> Result<(), ApiError> {
    let existing = state
        .knowledge_store
        .get(id)
        .ok_or_else(|| ApiError::not_found("知识记录不存在", id))?;
    if existing.workspace_id.as_ref() != Some(workspace_id) {
        return Err(ApiError::not_found("知识记录不存在", id));
    }
    let kind = existing.kind;
    let next_content = match updates.content.clone() {
        Some(content) if kind == KnowledgeKind::Learning => normalize_learning_content(content)?,
        Some(content) => normalize_text(content, "知识内容")?,
        None => existing.content,
    };
    let next_title = match updates.title.clone() {
        Some(title) => normalize_text(title, "知识标题")?,
        None if kind == KnowledgeKind::Learning => title_from_learning(&next_content),
        None => existing.title,
    };
    let next_source_ref = updates
        .source_ref
        .clone()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or(existing.source_ref);
    let updated = KnowledgeRecord {
        knowledge_id: existing.knowledge_id,
        kind,
        title: next_title,
        content: next_content,
        tags: updates
            .tags
            .clone()
            .map(normalize_tags)
            .unwrap_or(existing.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref: next_source_ref,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(updated);
    state.persist_knowledge_state()?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddFaqRequest {
    faq: FaqInput,
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FaqInput {
    title: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

async fn add_faq(
    State(state): State<ApiState>,
    Json(request): Json<AddFaqRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    let id = format!("faq-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Faq,
        title: normalize_text(request.faq.title, "FAQ 问题")?,
        content: normalize_text(request.faq.content, "FAQ 答案")?,
        tags: normalize_tags(request.faq.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref: None,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddLearningRequest {
    learning: LearningInput,
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LearningInput {
    content: String,
    context: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

async fn add_learning(
    State(state): State<ApiState>,
    Json(request): Json<AddLearningRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    let content = normalize_learning_content(request.learning.content)?;
    let id = format!("learning-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Learning,
        title: title_from_learning(&content),
        content,
        tags: normalize_tags(request.learning.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref: normalize_optional_text(request.learning.context),
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteKnowledgeRequest {
    id: String,
    workspace_id: Option<String>,
}

async fn delete_adr(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.id, &workspace_id)
        .map_err(|error| map_knowledge_delete_error(error, &request.id))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn delete_faq(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.id, &workspace_id)
        .map_err(|error| map_knowledge_delete_error(error, &request.id))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn delete_learning(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = require_workspace_id(request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.id, &workspace_id)
        .map_err(|error| map_knowledge_delete_error(error, &request.id))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
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
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_knowledge_store::KnowledgeStore;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
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

    #[tokio::test]
    async fn project_knowledge_returns_workspace_scoped_code_index() {
        let knowledge_store = KnowledgeStore::new();
        let workspace_a = WorkspaceId::new("workspace-knowledge-a");
        let workspace_b = WorkspaceId::new("workspace-knowledge-b");
        insert_code_index(&knowledge_store, &workspace_a, "src/a.rs");
        insert_code_index(&knowledge_store, &workspace_b, "src/b.rs");
        let state = state_with_knowledge_store(knowledge_store);

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
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("payload should deserialize");
        assert_eq!(payload["codeIndex"]["files"][0]["path"], "src/a.rs");
    }
}
