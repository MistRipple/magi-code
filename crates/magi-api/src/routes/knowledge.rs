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
        .route("/knowledge/faqs", get(list_faqs))
        .route("/knowledge/faqs/search", get(search_faqs))
        .route("/knowledge/adr/add", post(add_adr))
        .route("/knowledge/adr/update", post(update_adr))
        .route("/knowledge/adr/delete", post(delete_adr))
        .route("/knowledge/faq/add", post(add_faq))
        .route("/knowledge/faq/update", post(update_faq))
        .route("/knowledge/faq/delete", post(delete_faq))
        .route("/knowledge/learning/delete", post(delete_learning))
}

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

async fn clear_knowledge(
    State(state): State<ApiState>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    state.knowledge_store.clear();
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct KnowledgeListQuery {
    session_id: Option<String>,
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
        "adrs": result.matches.iter().map(|m| serde_json::json!({
            "id": m.record.knowledge_id,
            "title": m.record.title,
            "content": m.record.content,
            "tags": m.record.tags,
            "updatedAt": m.record.updated_at.0,
        })).collect::<Vec<_>>(),
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
    let adrs = adrs_result.matches.iter().map(|m| serde_json::json!({
        "id": m.record.knowledge_id,
        "title": m.record.title,
        "content": m.record.content,
        "tags": m.record.tags,
        "updatedAt": m.record.updated_at.0,
    })).collect::<Vec<_>>();

    let faqs_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: None,
        tags: vec![],
        limit: 1000,
    };
    let faqs_result = state.knowledge_store.query(&faqs_query);
    let faqs = faqs_result.matches.iter().map(|m| serde_json::json!({
        "id": m.record.knowledge_id,
        "title": m.record.title,
        "content": m.record.content,
        "tags": m.record.tags,
        "updatedAt": m.record.updated_at.0,
    })).collect::<Vec<_>>();

    let learnings_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Learning),
        text: None,
        tags: vec![],
        limit: 1000,
    };
    let learnings_result = state.knowledge_store.query(&learnings_query);
    let learnings = learnings_result.matches.iter().map(|m| serde_json::json!({
        "id": m.record.knowledge_id,
        "content": m.record.content,
        "context": m.record.source_ref,
        "tags": m.record.tags,
        "createdAt": m.record.updated_at.0,
    })).collect::<Vec<_>>();

    Json(serde_json::json!({
        "adrs": adrs,
        "faqs": faqs,
        "learnings": learnings,
        "codeIndex": null,
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
        "faqs": result.matches.iter().map(|m| serde_json::json!({
            "id": m.record.knowledge_id,
            "title": m.record.title,
            "content": m.record.content,
            "tags": m.record.tags,
            "updatedAt": m.record.updated_at.0,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct SearchFaqsQuery {
    q: Option<String>,
    session_id: Option<String>,
    workspace_id: Option<String>,
}

async fn search_faqs(
    State(state): State<ApiState>,
    Query(query): Query<SearchFaqsQuery>,
) -> Json<serde_json::Value> {
    let kq = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: query.q,
        tags: vec![],
        limit: 100,
    };
    let result = state.knowledge_store.query(&kq);
    Json(serde_json::json!({
        "results": result.matches.iter().map(|m| serde_json::json!({
            "id": m.record.knowledge_id,
            "title": m.record.title,
            "content": m.record.content,
            "score": m.score,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct AddAdrRequest {
    adr: AdrInput,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
        title: request.adr.title,
        content: request.adr.content,
        tags: request.adr.tags,
        source_ref: None,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct UpdateKnowledgeRequest {
    id: String,
    updates: KnowledgeUpdates,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct KnowledgeUpdates {
    title: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
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

fn update_knowledge_record(
    state: &ApiState,
    id: &str,
    updates: &KnowledgeUpdates,
) -> Result<(), ApiError> {
    let existing = state
        .knowledge_store
        .get(id)
        .ok_or_else(|| ApiError::not_found("知识记录不存在", id))?;
    let updated = KnowledgeRecord {
        knowledge_id: existing.knowledge_id,
        kind: existing.kind,
        title: updates.title.clone().unwrap_or(existing.title),
        content: updates.content.clone().unwrap_or(existing.content),
        tags: updates.tags.clone().unwrap_or(existing.tags),
        source_ref: existing.source_ref,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(updated);
    state.persist_knowledge_state()?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct AddFaqRequest {
    faq: FaqInput,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
        title: request.faq.title,
        content: request.faq.content,
        tags: request.faq.tags,
        source_ref: None,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state)))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
