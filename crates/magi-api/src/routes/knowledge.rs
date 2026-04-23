use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{UtcMillis, WorkspaceId};
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

fn resolve_workspace_id(
    state: &ApiState,
    workspace_id: Option<&str>,
) -> Result<WorkspaceId, ApiError> {
    if let Some(workspace_id) = workspace_id
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty())
    {
        let workspace_id = WorkspaceId::new(workspace_id);
        let exists = state
            .workspace_registry
            .workspaces()
            .iter()
            .any(|workspace| workspace.workspace_id == workspace_id);
        if !exists {
            return Err(ApiError::not_found(
                "workspace 不存在",
                workspace_id.as_str(),
            ));
        }
        return Ok(workspace_id);
    }
    state
        .workspace_registry
        .active_workspace_id()
        .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))
}

fn mutation_response(state: &ApiState, workspace_id: &WorkspaceId) -> KnowledgeMutationResponse {
    KnowledgeMutationResponse {
        success: true,
        knowledge_count: state
            .knowledge_store
            .query(&KnowledgeQuery {
                kind: None,
                text: None,
                tags: vec![],
                workspace_id: Some(workspace_id.clone()),
                limit: usize::MAX,
            })
            .total_matches,
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WorkspaceScopedMutationRequest {
    workspace_id: Option<String>,
}

async fn clear_knowledge(
    State(state): State<ApiState>,
    Json(request): Json<WorkspaceScopedMutationRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    state.knowledge_store.clear_workspace(&workspace_id);
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
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
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, query.workspace_id.as_deref())?;
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Adr),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Ok(Json(serde_json::json!({
        "adrs": result.matches.iter().map(|m| serde_json::json!({
            "id": m.record.knowledge_id,
            "title": m.record.title,
            "content": m.record.content,
            "tags": m.record.tags,
            "updatedAt": m.record.updated_at.0,
        })).collect::<Vec<_>>(),
    })))
}

async fn get_project_knowledge(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, query.workspace_id.as_deref())?;
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
        .map(|m| {
            serde_json::json!({
                "id": m.record.knowledge_id,
                "title": m.record.title,
                "content": m.record.content,
                "tags": m.record.tags,
                "updatedAt": m.record.updated_at.0,
            })
        })
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
        .map(|m| {
            serde_json::json!({
                "id": m.record.knowledge_id,
                "title": m.record.title,
                "content": m.record.content,
                "tags": m.record.tags,
                "updatedAt": m.record.updated_at.0,
            })
        })
        .collect::<Vec<_>>();

    let learnings_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Learning),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 1000,
    };
    let learnings_result = state.knowledge_store.query(&learnings_query);
    let learnings = learnings_result
        .matches
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.record.knowledge_id,
                "content": m.record.content,
                "context": m.record.source_ref,
                "tags": m.record.tags,
                "createdAt": m.record.updated_at.0,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "adrs": adrs,
        "faqs": faqs,
        "learnings": learnings,
        "codeIndex": null,
    })))
}

async fn list_faqs(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, query.workspace_id.as_deref())?;
    let query = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 1000,
    };
    let result = state.knowledge_store.query(&query);
    Ok(Json(serde_json::json!({
        "faqs": result.matches.iter().map(|m| serde_json::json!({
            "id": m.record.knowledge_id,
            "title": m.record.title,
            "content": m.record.content,
            "tags": m.record.tags,
            "updatedAt": m.record.updated_at.0,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchFaqsQuery {
    q: Option<String>,
    #[allow(dead_code)]
    session_id: Option<String>,
    #[allow(dead_code)]
    workspace_id: Option<String>,
}

async fn search_faqs(
    State(state): State<ApiState>,
    Query(query): Query<SearchFaqsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, query.workspace_id.as_deref())?;
    let kq = KnowledgeQuery {
        kind: Some(KnowledgeKind::Faq),
        text: query.q,
        tags: vec![],
        workspace_id: Some(workspace_id),
        limit: 100,
    };
    let result = state.knowledge_store.query(&kq);
    Ok(Json(serde_json::json!({
        "results": result.matches.iter().map(|m| serde_json::json!({
            "id": m.record.knowledge_id,
            "title": m.record.title,
            "content": m.record.content,
            "score": m.score,
        })).collect::<Vec<_>>(),
    })))
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
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    let id = format!("adr-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Adr,
        title: request.adr.title,
        content: request.adr.content,
        tags: request.adr.tags,
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
}

async fn update_adr(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    update_knowledge_record(&state, &request.id, &request.updates, &workspace_id)?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn update_faq(
    State(state): State<ApiState>,
    Json(request): Json<UpdateKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    update_knowledge_record(&state, &request.id, &request.updates, &workspace_id)?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

fn update_knowledge_record(
    state: &ApiState,
    id: &str,
    updates: &KnowledgeUpdates,
    workspace_id: &WorkspaceId,
) -> Result<(), ApiError> {
    let existing = state
        .knowledge_store
        .get(id)
        .ok_or_else(|| ApiError::not_found("知识记录不存在", id))?;
    if existing.workspace_id.as_ref() != Some(workspace_id) {
        return Err(ApiError::InvalidInput(format!(
            "知识记录 {} 不属于 workspace {}",
            id,
            workspace_id.as_str()
        )));
    }
    let updated = KnowledgeRecord {
        knowledge_id: existing.knowledge_id,
        kind: existing.kind,
        title: updates.title.clone().unwrap_or(existing.title),
        content: updates.content.clone().unwrap_or(existing.content),
        tags: updates.tags.clone().unwrap_or(existing.tags),
        workspace_id: existing.workspace_id,
        source_ref: existing.source_ref,
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
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    let id = format!("faq-{}", UtcMillis::now().0);
    let record = KnowledgeRecord {
        knowledge_id: id,
        kind: KnowledgeKind::Faq,
        title: request.faq.title,
        content: request.faq.content,
        tags: request.faq.tags,
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
struct DeleteKnowledgeRequest {
    id: String,
    workspace_id: Option<String>,
}

async fn delete_adr(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.id, &workspace_id)
        .map_err(|e| ApiError::internal_assembly("删除 ADR 失败", e))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn delete_faq(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.id, &workspace_id)
        .map_err(|e| ApiError::internal_assembly("删除 FAQ 失败", e))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

async fn delete_learning(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let workspace_id = resolve_workspace_id(&state, request.workspace_id.as_deref())?;
    state
        .knowledge_store
        .delete_in_workspace(&request.id, &workspace_id)
        .map_err(|e| ApiError::internal_assembly("删除 Learning 失败", e))?;
    state.persist_knowledge_state()?;
    Ok(Json(mutation_response(&state, &workspace_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::AbsolutePath;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    #[tokio::test]
    async fn project_knowledge_is_scoped_to_requested_workspace() {
        let state = test_state();
        let workspace_a = WorkspaceId::new("workspace-a");
        let workspace_b = WorkspaceId::new("workspace-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-knowledge-a"),
            )
            .expect("workspace a should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-knowledge-b"),
            )
            .expect("workspace b should register");

        state.knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "adr-a".to_string(),
            kind: KnowledgeKind::Adr,
            title: "ADR A".to_string(),
            content: "content-a".to_string(),
            tags: vec![],
            workspace_id: Some(workspace_a.clone()),
            source_ref: None,
            updated_at: UtcMillis(1),
        });
        state.knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "adr-b".to_string(),
            kind: KnowledgeKind::Adr,
            title: "ADR B".to_string(),
            content: "content-b".to_string(),
            tags: vec![],
            workspace_id: Some(workspace_b.clone()),
            source_ref: None,
            updated_at: UtcMillis(2),
        });

        let app = routes().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge?workspaceId=workspace-a")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("response should return");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("body should be json");
        let adrs = payload["adrs"].as_array().expect("adrs should be an array");
        assert_eq!(adrs.len(), 1);
        assert_eq!(adrs[0]["title"], "ADR A");
    }

    #[tokio::test]
    async fn clear_knowledge_only_clears_target_workspace() {
        let state = test_state();
        let workspace_a = WorkspaceId::new("workspace-a");
        let workspace_b = WorkspaceId::new("workspace-b");
        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-knowledge-a"),
            )
            .expect("workspace a should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-knowledge-b"),
            )
            .expect("workspace b should register");

        state.knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "faq-a".to_string(),
            kind: KnowledgeKind::Faq,
            title: "FAQ A".to_string(),
            content: "content-a".to_string(),
            tags: vec![],
            workspace_id: Some(workspace_a.clone()),
            source_ref: None,
            updated_at: UtcMillis(1),
        });
        state.knowledge_store.upsert(KnowledgeRecord {
            knowledge_id: "faq-b".to_string(),
            kind: KnowledgeKind::Faq,
            title: "FAQ B".to_string(),
            content: "content-b".to_string(),
            tags: vec![],
            workspace_id: Some(workspace_b.clone()),
            source_ref: None,
            updated_at: UtcMillis(2),
        });

        let app = routes().with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/knowledge/clear")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "workspaceId": "workspace-a" }).to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("response should return");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.knowledge_store.get("faq-a").is_none());
        assert!(state.knowledge_store.get("faq-b").is_some());
    }
}
