use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use magi_core::{UtcMillis, WorkspaceId};
use magi_knowledge_store::{KnowledgeKind, KnowledgeQuery, KnowledgeRecord};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{errors::ApiError, state::ApiState};

const KNOWLEDGE_CODE_INDEX_SCAN_FILE_LIMIT: usize = 2_000;
const KNOWLEDGE_CODE_INDEX_MAX_FILE_BYTES: u64 = 1_000_000;

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

fn workspace_root_path(state: &ApiState, workspace_id: &WorkspaceId) -> Option<PathBuf> {
    state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| &workspace.workspace_id == workspace_id)
        .map(|workspace| PathBuf::from(workspace.root_path.to_string()))
}

fn is_ignored_knowledge_scan_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git"
            | ".magi"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".svelte-kit"
            | "coverage"
            | ".turbo"
    )
}

fn infer_language_from_path(path: &str) -> Option<&'static str> {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("Rust"),
        "ts" | "tsx" => Some("TypeScript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
        "svelte" => Some("Svelte"),
        "vue" => Some("Vue"),
        "py" => Some("Python"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "swift" => Some("Swift"),
        "c" | "h" => Some("C"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("C++"),
        "cs" => Some("C#"),
        "php" => Some("PHP"),
        "rb" => Some("Ruby"),
        "md" | "mdx" => Some("Markdown"),
        "json" => Some("JSON"),
        "toml" => Some("TOML"),
        "yaml" | "yml" => Some("YAML"),
        "css" => Some("CSS"),
        "scss" | "sass" => Some("Sass"),
        "html" => Some("HTML"),
        "sql" => Some("SQL"),
        "sh" | "bash" | "zsh" => Some("Shell"),
        _ => None,
    }
}

fn is_entry_point_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let file_name = Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    matches!(
        file_name,
        "Cargo.toml"
            | "package.json"
            | "pnpm-workspace.yaml"
            | "vite.config.ts"
            | "vite.config.js"
            | "svelte.config.js"
            | "README.md"
            | "AGENTS.md"
    ) || normalized.ends_with("/src/main.rs")
        || normalized.ends_with("/src/lib.rs")
        || normalized.ends_with("/src/main.ts")
        || normalized.ends_with("/src/main.tsx")
        || normalized.ends_with("/src/App.svelte")
        || normalized.ends_with("/src/App.tsx")
}

fn relative_workspace_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .filter(|relative| !relative.is_empty())
}

fn count_file_lines(path: &Path, metadata: &fs::Metadata) -> usize {
    if metadata.len() > KNOWLEDGE_CODE_INDEX_MAX_FILE_BYTES {
        return 0;
    }
    let Ok(content) = fs::read_to_string(path) else {
        return 0;
    };
    content.lines().count()
}

fn scan_workspace_code_files(root: &Path) -> Vec<(String, usize)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if files.len() >= KNOWLEDGE_CODE_INDEX_SCAN_FILE_LIMIT {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                if !is_ignored_knowledge_scan_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let Some(relative_path) = relative_workspace_path(root, &path) else {
                continue;
            };
            if infer_language_from_path(&relative_path).is_none() {
                continue;
            }
            let lines = count_file_lines(&path, &metadata);
            files.push((relative_path, lines));
            if files.len() >= KNOWLEDGE_CODE_INDEX_SCAN_FILE_LIMIT {
                break;
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn build_code_index_snapshot(state: &ApiState, workspace_id: &WorkspaceId) -> serde_json::Value {
    let code_query = KnowledgeQuery {
        kind: Some(KnowledgeKind::CodeIndex),
        text: None,
        tags: vec![],
        workspace_id: Some(workspace_id.clone()),
        limit: usize::MAX,
    };
    let code_records = state.knowledge_store.query(&code_query);
    let mut files: BTreeMap<String, usize> = BTreeMap::new();
    let mut tech_stack: BTreeSet<String> = BTreeSet::new();
    let mut entry_points: BTreeSet<String> = BTreeSet::new();

    for item in &code_records.matches {
        let Some(source) = state.knowledge_store.code_source(&item.record.knowledge_id) else {
            continue;
        };
        let path = source.path.trim();
        if path.is_empty() {
            continue;
        }
        let line_count = match (source.start_line, source.end_line) {
            (Some(start), Some(end)) if end >= start => end - start + 1,
            _ => 0,
        };
        files
            .entry(path.to_string())
            .and_modify(|existing| *existing = (*existing).max(line_count))
            .or_insert(line_count);
        if let Some(language) = source
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            tech_stack.insert(language.to_string());
        } else if let Some(language) = infer_language_from_path(path) {
            tech_stack.insert(language.to_string());
        }
        if is_entry_point_path(path) {
            entry_points.insert(path.to_string());
        }
    }

    if files.is_empty() {
        if let Some(root) = workspace_root_path(state, workspace_id) {
            for (path, lines) in scan_workspace_code_files(&root) {
                if let Some(language) = infer_language_from_path(&path) {
                    tech_stack.insert(language.to_string());
                }
                if is_entry_point_path(&path) {
                    entry_points.insert(path.clone());
                }
                files.insert(path, lines);
            }
        }
    }

    serde_json::json!({
        "files": files
            .into_iter()
            .map(|(path, lines)| serde_json::json!({
                "path": path,
                "lines": lines,
            }))
            .collect::<Vec<_>>(),
        "techStack": tech_stack.into_iter().collect::<Vec<_>>(),
        "entryPoints": entry_points.into_iter().collect::<Vec<_>>(),
    })
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
        workspace_id: Some(workspace_id.clone()),
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
        "codeIndex": build_code_index_snapshot(&state, &workspace_id),
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
