use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use magi_core::UtcMillis;
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

fn mutation_response(state: &ApiState) -> KnowledgeMutationResponse {
    KnowledgeMutationResponse {
        success: true,
        knowledge_count: state.knowledge_store.list().len(),
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
    value.map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
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

async fn clear_knowledge(
    State(state): State<ApiState>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    state.knowledge_store.clear();
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeListQuery {
    #[allow(dead_code)]
    session_id: Option<String>,
    #[allow(dead_code)]
    workspace_id: Option<String>,
}

async fn list_adrs(
    State(state): State<ApiState>,
    Query(_query): Query<KnowledgeListQuery>,
) -> Json<serde_json::Value> {
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Adr),
        text: None,
        tags: vec![],
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Json(serde_json::json!({
        "adrs": result.matches.iter().map(|m| knowledge_record_json(&m.record)).collect::<Vec<_>>(),
    }))
}

async fn get_project_knowledge(
    State(state): State<ApiState>,
    Query(_query): Query<KnowledgeListQuery>,
) -> Json<serde_json::Value> {
    let adrs_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Adr),
        text: None,
        tags: vec![],
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
        .code_index_summary()
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

    Json(serde_json::json!({
        "adrs": adrs,
        "faqs": faqs,
        "learnings": learnings,
        "codeIndex": code_index,
    }))
}

async fn list_faqs(
    State(state): State<ApiState>,
    Query(_query): Query<KnowledgeListQuery>,
) -> Json<serde_json::Value> {
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: None,
        tags: vec![],
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Json(serde_json::json!({
        "faqs": result.matches.iter().map(|m| knowledge_record_json(&m.record)).collect::<Vec<_>>(),
    }))
}

async fn list_learnings(
    State(state): State<ApiState>,
    Query(_query): Query<KnowledgeListQuery>,
) -> Json<serde_json::Value> {
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Learning),
        text: None,
        tags: vec![],
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Json(serde_json::json!({
        "learnings": result.matches.iter().map(|m| learning_record_json(&m.record)).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchQuery {
    q: Option<String>,
    #[allow(dead_code)]
    session_id: Option<String>,
    #[allow(dead_code)]
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
) -> Json<serde_json::Value> {
    search_knowledge(&state, KnowledgeKind::Adr, query.text())
}

async fn search_faqs(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Json<serde_json::Value> {
    search_knowledge(&state, KnowledgeKind::Faq, query.text())
}

async fn search_learnings(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Json<serde_json::Value> {
    search_knowledge(&state, KnowledgeKind::Learning, query.text())
}

fn search_knowledge(
    state: &ApiState,
    kind: KnowledgeKind,
    text: Option<String>,
) -> Json<serde_json::Value> {
    let kq = KnowledgeQuery {
        kind: Some(kind),
        text,
        tags: vec![],
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
    let id = format!("adr-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Adr,
        title: normalize_text(request.adr.title, "ADR 标题")?,
        content: normalize_text(request.adr.content, "ADR 内容")?,
        tags: normalize_tags(request.adr.tags),
        source_ref: None,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKnowledgeRequest {
    id: String,
    updates: KnowledgeUpdates,
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
    update_knowledge_record(&state, &request.id, &request.updates)?;
    Ok(Json(mutation_response(&state)))
}

async fn update_faq(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    update_knowledge_record(&state, &request.id, &request.updates)?;
    Ok(Json(mutation_response(&state)))
}

async fn update_learning(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    update_knowledge_record(&state, &request.id, &request.updates)?;
    Ok(Json(mutation_response(&state)))
}

fn update_knowledge_record(
    state: &ApiState,
    id: &str,
    updates: &KnowledgeUpdates,
) -> Result<(), ApiError> {
    let existing = state
        .knowledge_store
        .get(id)
        .ok_or_else(|| ApiError::not_found("知识记录不存在", id))?;
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
        tags: updates.tags.clone().map(normalize_tags).unwrap_or(existing.tags),
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
    let id = format!("faq-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Faq,
        title: normalize_text(request.faq.title, "FAQ 问题")?,
        content: normalize_text(request.faq.content, "FAQ 答案")?,
        tags: normalize_tags(request.faq.tags),
        source_ref: None,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddLearningRequest {
    learning: LearningInput,
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
    let content = normalize_learning_content(request.learning.content)?;
    let id = format!("learning-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Learning,
        title: title_from_learning(&content),
        content,
        tags: normalize_tags(request.learning.tags),
        source_ref: normalize_optional_text(request.learning.context),
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteKnowledgeRequest {
    id: String,
}

async fn delete_adr(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    state
        .knowledge_store
        .delete(&request.id)
        .map_err(|e| ApiError::internal_assembly("删除 ADR 失败", e))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

async fn delete_faq(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    state
        .knowledge_store
        .delete(&request.id)
        .map_err(|e| ApiError::internal_assembly("删除 FAQ 失败", e))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

async fn delete_learning(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    state
        .knowledge_store
        .delete(&request.id)
        .map_err(|e| ApiError::internal_assembly("删除 Learning 失败", e))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}
