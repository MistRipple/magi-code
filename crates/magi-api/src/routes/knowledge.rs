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
        let project =
            crate::routes::test_project_fixtures::add_source(&state, id.as_str(), "b", &b);
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
}
