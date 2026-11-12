use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{DomainError, UtcMillis, WorkspaceId};
use magi_knowledge_store::{
    GraphDirection, GraphEdgeKind, GraphEdgeOrigin, GraphEdgeStatus, GraphNodeKind, GraphNodeRef,
    GraphQuery, KnowledgeKind, KnowledgeQuery, KnowledgeRecord, KnowledgeRelation,
    code_scanner::{
        CodeIndexScanOutcome, CodeIndexScanStatus, CodeIndexSummary, workspace_root_scan_failure,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::scope_binding as session_scope;
use crate::{dto::SessionScopeKindDto, errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/knowledge", get(get_project_knowledge))
        .route("/knowledge/graph", get(get_knowledge_graph))
        .route(
            "/knowledge/relations",
            get(list_knowledge_relations).post(add_knowledge_relation),
        )
        .route(
            "/knowledge/relations/update",
            post(update_knowledge_relation),
        )
        .route(
            "/knowledge/relations/delete",
            post(delete_knowledge_relation),
        )
        .route("/knowledge/reindex", post(reindex_knowledge))
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
const DEFAULT_KNOWLEDGE_RELATION_LIMIT: usize = 1_000;
const MAX_KNOWLEDGE_RELATION_LIMIT: usize = 1_000;
static KNOWLEDGE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

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

fn mutation_response(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<KnowledgeMutationResponse, ApiError> {
    let workspace_path = session_scope::registered_workspace_path(state, workspace_id)?;
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

fn require_registered_workspace_binding(
    state: &ApiState,
    workspace_id: Option<&str>,
    workspace_path: Option<&str>,
) -> Result<(WorkspaceId, String), ApiError> {
    let binding =
        session_scope::require_registered_workspace_binding(state, workspace_id, workspace_path)?;
    Ok((binding.workspace_id, binding.workspace_path))
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
        "createdAt": record.created_at.0,
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

fn new_knowledge_id(prefix: &str) -> String {
    let nonce = KNOWLEDGE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{prefix}-{}-{nonce}", UtcMillis::now().0)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeWorkspaceRequest {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeRelationsQuery {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn clear_knowledge(
    State(state): State<ApiState>,
    Json(request): Json<KnowledgeWorkspaceRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    if request.source_folder_id.is_some() {
        return Err(ApiError::InvalidInput(
            "清空知识是项目操作，请省略 sourceFolderId".into(),
        ));
    }
    let (workspace_id, _) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    state.knowledge_store.clear_workspace(&workspace_id);
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeItemsQuery {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    kind: Option<KnowledgeKindParam>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

async fn list_knowledge_items(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeItemsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeSearchQuery {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    kind: Option<KnowledgeKindParam>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeGraphQuery {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
    #[serde(default)]
    focus: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    node_kinds: Option<String>,
    #[serde(default)]
    edge_kinds: Option<String>,
    #[serde(default)]
    max_nodes: Option<usize>,
    #[serde(default)]
    max_edges: Option<usize>,
}

fn parse_graph_csv<T>(
    value: Option<&str>,
    field: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, ApiError> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            parse(item)
                .ok_or_else(|| ApiError::InvalidInput(format!("{field} 包含不支持的值：{item}")))
        })
        .collect()
}

async fn get_knowledge_graph(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeGraphQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let (index_store, index_root) =
        resolve_code_index_store(&state, &workspace_id, query.source_folder_id.as_deref())?;
    let load_state = ensure_workspace_code_index(&state, &workspace_id, &index_store, &index_root)?;
    let node_kinds = parse_graph_csv(
        query.node_kinds.as_deref(),
        "nodeKinds",
        GraphNodeKind::parse,
    )?;
    let edge_kinds = parse_graph_csv(
        query.edge_kinds.as_deref(),
        "edgeKinds",
        GraphEdgeKind::parse,
    )?;
    let direction = match query.direction.as_deref() {
        Some(value) => GraphDirection::parse(value)
            .ok_or_else(|| ApiError::InvalidInput("direction 包含不支持的值".to_string()))?,
        None => GraphDirection::default(),
    };
    let graph_query = GraphQuery {
        focus: query.focus.filter(|value| !value.trim().is_empty()),
        depth: query.depth.unwrap_or(1),
        direction,
        node_kinds,
        edge_kinds,
        max_nodes: query.max_nodes.unwrap_or(120),
        max_edges: query.max_edges.unwrap_or(240),
    };

    let graph =
        graph_payload_for_workspace(&index_store, &workspace_id, &load_state, &graph_query)?;
    let mut response = graph.as_object().cloned().ok_or_else(|| {
        ApiError::internal_assembly("知识图谱响应无效", "graph payload is not an object")
    })?;
    response.insert(
        "workspaceId".to_string(),
        serde_json::json!(workspace_id.as_str()),
    );
    response.insert(
        "workspacePath".to_string(),
        serde_json::json!(workspace_path),
    );
    response.insert(
        "sourceFolderId".into(),
        serde_json::json!(
            index_store
                .code_index_scope()
                .map(|scope| scope.source_folder_id.as_str())
        ),
    );
    Ok(Json(serde_json::Value::Object(response)))
}

fn empty_graph_payload(status: &str, reason_code: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "reasonCode": reason_code,
        "nodes": [],
        "edges": [],
        "stats": { "totalNodes": 0, "totalEdges": 0, "returnedNodes": 0, "returnedEdges": 0 },
        "truncated": false,
    })
}

fn graph_payload_for_workspace(
    index_store: &magi_knowledge_store::KnowledgeStore,
    workspace_id: &WorkspaceId,
    load_state: &WorkspaceCodeIndexLoadState,
    query: &magi_knowledge_store::GraphQuery,
) -> Result<serde_json::Value, ApiError> {
    let WorkspaceCodeIndexLoadState::Ready(outcome) = load_state else {
        return Ok(empty_graph_payload("indexing", None));
    };
    if outcome.status != CodeIndexScanStatus::Indexed {
        return Ok(empty_graph_payload(
            outcome.status.as_str(),
            outcome.reason_code.as_ref().map(|reason| reason.as_str()),
        ));
    }
    let graph = index_store
        .query_workspace_graph(workspace_id, query)
        .ok_or_else(|| {
            ApiError::internal_assembly("知识图谱索引不可用", "workspace graph index missing")
        })?;
    Ok(serde_json::json!({
        "status": "ready",
        "reasonCode": serde_json::Value::Null,
        "nodes": graph.nodes,
        "edges": graph.edges,
        "stats": graph.stats,
        "truncated": graph.truncated,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KnowledgeRelationRequest {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
    relation_id: Option<String>,
    source: GraphNodeRef,
    kind: GraphEdgeKind,
    target: GraphNodeRef,
    #[serde(default)]
    origin: Option<GraphEdgeOrigin>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    status: Option<GraphEdgeStatus>,
    #[serde(default)]
    evidence: Vec<String>,
}

fn relation_json(relation: &KnowledgeRelation) -> serde_json::Value {
    serde_json::to_value(relation).expect("knowledge relation should serialize")
}

fn relation_from_request(
    request: KnowledgeRelationRequest,
    workspace_id: WorkspaceId,
    existing: Option<&KnowledgeRelation>,
) -> Result<KnowledgeRelation, ApiError> {
    let now = UtcMillis::now();
    let relation_id = request
        .relation_id
        .or_else(|| existing.map(|relation| relation.relation_id.clone()))
        .unwrap_or_else(|| {
            format!(
                "relation-{}-{}",
                now.0,
                KNOWLEDGE_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
            )
        });
    let origin = request.origin.unwrap_or(GraphEdgeOrigin::ExplicitUser);
    let status = request.status.unwrap_or(GraphEdgeStatus::Active);
    if origin == GraphEdgeOrigin::Inferred
        && !matches!(
            existing.map(|relation| relation.origin),
            Some(GraphEdgeOrigin::Inferred)
        )
    {
        return Err(ApiError::InvalidInput(
            "自动推断关系只能由系统生成并通过待审阅流程更新".to_string(),
        ));
    }
    let reviewed_at = if status == GraphEdgeStatus::Candidate {
        None
    } else {
        existing
            .and_then(|relation| relation.reviewed_at)
            .or_else(|| (origin == GraphEdgeOrigin::Inferred).then_some(now))
    };
    Ok(KnowledgeRelation {
        code_scope: None,
        relation_id,
        workspace_id,
        source: request.source,
        kind: request.kind,
        target: request.target,
        origin,
        confidence: request.confidence,
        status,
        evidence: request.evidence,
        discovery_key: existing.and_then(|relation| relation.discovery_key.clone()),
        discovery_evidence: existing.and_then(|relation| relation.discovery_evidence.clone()),
        reviewed_at,
        created_at: existing.map(|relation| relation.created_at).unwrap_or(now),
        updated_at: now,
    })
}

async fn list_knowledge_relations(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeRelationsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_KNOWLEDGE_RELATION_LIMIT)
        .clamp(1, MAX_KNOWLEDGE_RELATION_LIMIT);
    let store = relation_store(&state, &workspace_id, query.source_folder_id.as_deref())?;
    let mut relations = store.list_relations(&workspace_id);
    let total_relations = relations.len();
    relations.sort_by(|left, right| {
        relation_status_priority(left.status)
            .cmp(&relation_status_priority(right.status))
            .then_with(|| {
                right
                    .confidence
                    .unwrap_or_default()
                    .total_cmp(&left.confidence.unwrap_or_default())
            })
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.relation_id.cmp(&right.relation_id))
    });
    relations.truncate(limit);
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "sourceFolderId": store.code_index_scope().map(|scope| scope.source_folder_id.as_str()),
        "relations": relations.iter().map(relation_json).collect::<Vec<_>>(),
        "totalRelations": total_relations,
        "truncated": total_relations > limit,
    })))
}

fn relation_status_priority(status: GraphEdgeStatus) -> u8 {
    match status {
        GraphEdgeStatus::Candidate => 0,
        GraphEdgeStatus::Dangling => 1,
        GraphEdgeStatus::Active => 2,
        GraphEdgeStatus::Rejected => 3,
    }
}

fn relation_store(
    state: &ApiState,
    workspace_id: &WorkspaceId,
    source_folder_id: Option<&str>,
) -> Result<magi_knowledge_store::KnowledgeStore, ApiError> {
    match source_folder_id {
        Some(_) => {
            resolve_code_index_store(state, workspace_id, source_folder_id).map(|(store, _)| store)
        }
        None => {
            let project = state
                .workspace_registry
                .project(workspace_id)
                .ok_or_else(|| ApiError::not_found("项目不存在", workspace_id.as_str()))?;
            if project.source_folders.len() > 1 {
                return Err(ApiError::InvalidInput(
                    "多源目录项目的知识关系必须指定 sourceFolderId".into(),
                ));
            }
            Ok(state.knowledge_store.as_ref().clone())
        }
    }
}

async fn add_knowledge_relation(
    State(state): State<ApiState>,
    Json(request): Json<KnowledgeRelationRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let store = relation_store(&state, &workspace_id, request.source_folder_id.as_deref())?;
    let mut relation = relation_from_request(request, workspace_id.clone(), None)?;
    relation.code_scope = store.code_index_scope().cloned();
    store
        .upsert_relation(relation.clone())
        .map_err(map_relation_error)?;
    state.persist_knowledge_state_for_api()?;
    Ok(Json(serde_json::json!({
        "success": true,
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "relation": relation_json(&relation),
    })))
}

async fn update_knowledge_relation(
    State(state): State<ApiState>,
    Json(request): Json<KnowledgeRelationRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let relation_id = request
        .relation_id
        .as_deref()
        .ok_or_else(|| ApiError::InvalidInput("relationId 不能为空".to_string()))?;
    let store = relation_store(&state, &workspace_id, request.source_folder_id.as_deref())?;
    let existing = store
        .relation(relation_id, &workspace_id)
        .ok_or_else(|| ApiError::not_found("关系不存在", relation_id))?;
    let mut relation = relation_from_request(request, workspace_id.clone(), Some(&existing))?;
    relation.code_scope = store.code_index_scope().cloned();
    store
        .replace_relation(relation.clone(), &workspace_id)
        .map_err(map_relation_error)?;
    state.persist_knowledge_state_for_api()?;
    Ok(Json(serde_json::json!({
        "success": true,
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "relation": relation_json(&relation),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteKnowledgeRelationRequest {
    #[serde(rename = "scope", default)]
    _scope: Option<SessionScopeKindDto>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
    relation_id: String,
}

async fn delete_knowledge_relation(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeRelationRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let store = relation_store(&state, &workspace_id, request.source_folder_id.as_deref())?;
    store
        .delete_relation_in_workspace(&request.relation_id, &workspace_id)
        .map_err(map_relation_error)?;
    state.persist_knowledge_state_for_api()?;
    Ok(Json(serde_json::json!({
        "success": true,
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "relationId": request.relation_id,
    })))
}

fn map_relation_error(error: DomainError) -> ApiError {
    match error {
        DomainError::NotFound {
            entity: "knowledge",
        } => ApiError::NotFound("关系引用的知识记录不存在".to_string()),
        DomainError::NotFound { .. } => ApiError::NotFound("关系不存在".to_string()),
        DomainError::InvalidState { message } | DomainError::Validation { message } => {
            ApiError::InvalidInput(message)
        }
        other => ApiError::internal_assembly("知识关系操作失败", other),
    }
}

async fn search_knowledge_items(
    State(state): State<ApiState>,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
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
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let (index_store, index_root) =
        resolve_code_index_store(&state, &workspace_id, query.source_folder_id.as_deref())?;
    let index_load_state =
        ensure_workspace_code_index(&state, &workspace_id, &index_store, &index_root)?;

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

    let code_index_summary = index_store.code_index_summary_for_workspace(&workspace_id);
    let code_index_status = code_index_status_json(code_index_summary.as_ref(), &index_load_state);
    let code_index = code_index_summary
        .map(|summary| {
            serde_json::json!({
                "files": summary.files.iter().map(|f| serde_json::json!({
                    "path": f.path,
                    "sourceFolderId": index_store.code_index_scope().map(|scope| scope.source_folder_id.as_str()),
                    "lines": f.lines,
                    "size": f.size,
                })).collect::<Vec<_>>(),
                "techStack": summary.tech_stack,
                "entryPoints": summary.entry_points,
            })
        })
        .unwrap_or(serde_json::Value::Null);

    let graph = graph_payload_for_workspace(
        &index_store,
        &workspace_id,
        &index_load_state,
        &magi_knowledge_store::GraphQuery::default(),
    )?;

    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "items": items,
        "sourceFolderId": index_store.code_index_scope().map(|scope| scope.source_folder_id.as_str()),
        "codeIndex": code_index,
        "codeIndexStatus": code_index_status,
        "graph": graph,
    })))
}

enum WorkspaceCodeIndexLoadState {
    Ready(CodeIndexScanOutcome),
    Indexing,
}

fn resolve_code_index_store(
    state: &ApiState,
    workspace_id: &WorkspaceId,
    source_folder_id: Option<&str>,
) -> Result<(magi_knowledge_store::KnowledgeStore, PathBuf), ApiError> {
    let project = state
        .workspace_registry
        .project(workspace_id)
        .ok_or_else(|| ApiError::not_found("项目不存在", workspace_id.as_str()))?;
    let Some(source_id) = source_folder_id else {
        if project.source_folders.len() != 1 {
            return Err(ApiError::InvalidInput(
                "多源目录项目必须指定 sourceFolderId".into(),
            ));
        }
        return Ok((
            state.knowledge_store.as_ref().clone(),
            project.native_root_path(),
        ));
    };
    let folder = project
        .source_folder(source_id)
        .ok_or_else(|| ApiError::InvalidInput("项目源文件夹不存在".into()))?;
    let root = folder.native_path();
    if std::fs::canonicalize(&root).is_ok_and(|canonical| canonical != root) {
        return Err(ApiError::Conflict("源文件夹原登记路径已变化".into()));
    }
    let store = state
        .knowledge_store
        .for_source_folder(source_id, &root)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    Ok((store, root))
}

fn ensure_workspace_code_index(
    state: &ApiState,
    workspace_id: &WorkspaceId,
    index_store: &magi_knowledge_store::KnowledgeStore,
    root: &std::path::Path,
) -> Result<WorkspaceCodeIndexLoadState, ApiError> {
    if workspace_root_scan_failure(root).is_some() {
        let outcome = index_store.build_workspace_index(workspace_id, root);
        state.persist_knowledge_state_for_api()?;
        return Ok(WorkspaceCodeIndexLoadState::Ready(outcome));
    }
    if index_store.workspace_index_building(workspace_id) {
        return Ok(WorkspaceCodeIndexLoadState::Indexing);
    }
    if index_store.workspace_index_ready(workspace_id) {
        return Ok(WorkspaceCodeIndexLoadState::Ready(
            CodeIndexScanOutcome::indexed_existing(),
        ));
    }
    if let Some(outcome) = index_store.workspace_index_outcome(workspace_id) {
        return Ok(WorkspaceCodeIndexLoadState::Ready(outcome));
    }
    state.schedule_code_index(
        workspace_id.clone(),
        root.to_path_buf(),
        index_store.clone(),
    );
    Ok(WorkspaceCodeIndexLoadState::Indexing)
}

fn code_index_status_json(
    summary: Option<&CodeIndexSummary>,
    load_state: &WorkspaceCodeIndexLoadState,
) -> serde_json::Value {
    if matches!(load_state, WorkspaceCodeIndexLoadState::Indexing) {
        return serde_json::json!({
            "status": "indexing",
            "reasonCode": serde_json::Value::Null,
        });
    }
    if summary.is_some_and(|summary| !summary.files.is_empty()) {
        return serde_json::json!({
            "status": CodeIndexScanStatus::Indexed.as_str(),
            "reasonCode": serde_json::Value::Null,
        });
    }
    let WorkspaceCodeIndexLoadState::Ready(scan_outcome) = load_state else {
        unreachable!("indexing state should return before scan outcome projection")
    };
    serde_json::json!({
        "status": scan_outcome.status.as_str(),
        "reasonCode": scan_outcome.reason_code.as_ref().map(|reason| reason.as_str()),
    })
}

async fn reindex_knowledge(
    State(state): State<ApiState>,
    Json(request): Json<KnowledgeWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, workspace_path) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
    let (index_store, index_root) =
        resolve_code_index_store(&state, &workspace_id, request.source_folder_id.as_deref())?;
    let scheduled = state.schedule_code_index(workspace_id.clone(), index_root, index_store);
    Ok(Json(serde_json::json!({
        "accepted": scheduled,
        "workspaceId": workspace_id.as_str(),
        "workspacePath": workspace_path,
        "status": "indexing",
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddKnowledgeItemRequest {
    kind: KnowledgeKindParam,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
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
    let (workspace_id, _) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
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
    let now = UtcMillis::now();
    let record = KnowledgeRecord {
        knowledge_id: new_knowledge_id(id_prefix),
        kind,
        title,
        content,
        tags: normalize_tags(request.tags),
        workspace_id: Some(workspace_id.clone()),
        source_ref,
        created_at: now,
        updated_at: now,
    };
    state.knowledge_store.upsert(record);
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateKnowledgeItemRequest {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
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
    let (workspace_id, _) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
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
        created_at: existing.created_at,
        updated_at: UtcMillis::now(),
    };
    state.knowledge_store.upsert(updated);
    state.persist_knowledge_state_for_api()?;
    Ok(Json(mutation_response(&state, &workspace_id)?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteKnowledgeItemRequest {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    knowledge_id: String,
}

async fn delete_knowledge_item(
    State(state): State<ApiState>,
    Json(request): Json<DeleteKnowledgeItemRequest>,
) -> Result<Json<KnowledgeMutationResponse>, ApiError> {
    let (workspace_id, _) = require_registered_workspace_binding(
        &state,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
    )?;
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
mod tests;
