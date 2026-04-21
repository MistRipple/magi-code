use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces/register", post(register_workspace))
        .route("/workspaces/remove", post(remove_workspace))
        .route("/workspaces/rename", post(rename_workspace))
        .route("/workspaces/pick", get(pick_workspace))
        .route("/workspaces/sessions", get(workspace_sessions))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceDto {
    workspace_id: String,
    path: String,
    name: Option<String>,
    is_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceListResponse {
    workspaces: Vec<WorkspaceDto>,
}

async fn list_workspaces(
    State(state): State<ApiState>,
) -> Json<WorkspaceListResponse> {
    let workspaces = state
        .workspace_registry
        .workspaces()
        .into_iter()
        .map(|w| {
            let active_id = state.workspace_registry.active_workspace_id();
            WorkspaceDto {
                workspace_id: w.workspace_id.to_string(),
                path: w.root_path.to_string(),
                name: w.name.clone(),
                is_active: active_id.as_ref() == Some(&w.workspace_id),
            }
        })
        .collect();
    Json(WorkspaceListResponse { workspaces })
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct RegisterWorkspaceRequest {
    path: String,
    name: Option<String>,
}

async fn register_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RegisterWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = magi_core::WorkspaceId::new(format!(
        "workspace-{}",
        magi_core::UtcMillis::now().0
    ));
    let path = magi_core::AbsolutePath::new(&request.path);
    state
        .workspace_registry
        .register_with_name(workspace_id.clone(), path, request.name.clone())
        .map_err(|e| ApiError::internal_assembly("工作区注册失败", e))?;
    state.persist_workspace_durable_state()?;
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.to_string(),
        "registered": true
    })))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct RemoveWorkspaceRequest {
    workspace_id: String,
}

async fn remove_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RemoveWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = magi_core::WorkspaceId::new(&request.workspace_id);
    state
        .workspace_registry
        .deregister(&workspace_id)
        .map_err(|e| ApiError::internal_assembly("工作区移除失败", e))?;
    state.persist_workspace_durable_state()?;
    Ok(Json(serde_json::json!({ "removed": true })))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct RenameWorkspaceRequest {
    workspace_id: String,
    name: String,
}

async fn rename_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RenameWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = magi_core::WorkspaceId::new(&request.workspace_id);
    state
        .workspace_registry
        .rename(&workspace_id, &request.name)
        .map_err(|e| ApiError::internal_assembly("工作区重命名失败", e))?;
    state.persist_workspace_durable_state()?;
    Ok(Json(serde_json::json!({ "renamed": true })))
}

async fn pick_workspace(
    State(state): State<ApiState>,
) -> Json<serde_json::Value> {
    let workspaces = state.workspace_registry.workspaces();
    let active_id = state.workspace_registry.active_workspace_id();
    Json(serde_json::json!({
        "workspaces": workspaces.iter().map(|w| serde_json::json!({
            "workspaceId": w.workspace_id.to_string(),
            "path": w.root_path.to_string(),
            "name": w.name.clone(),
            "isActive": active_id.as_ref() == Some(&w.workspace_id),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSessionsQuery {
    workspace_id: Option<String>,
}

async fn workspace_sessions(
    State(state): State<ApiState>,
    Query(query): Query<WorkspaceSessionsQuery>,
) -> Json<serde_json::Value> {
    let sessions = state.session_store.sessions();
    let workspace_id = query
        .workspace_id
        .as_deref()
        .map(magi_core::WorkspaceId::new);
    let session_sidecars = state.session_store.execution_sidecar_exports();
    Json(serde_json::json!({
        "sessions": sessions.iter().filter(|session| {
            let Some(ref workspace_id) = workspace_id else {
                return true;
            };
            session_sidecars.iter().find(|sidecar| sidecar.session_id == session.session_id)
                .and_then(|sidecar| sidecar.ownership.workspace_id.as_ref())
                == Some(workspace_id)
        }).map(|s| serde_json::json!({
            "sessionId": s.session_id.to_string(),
            "title": s.title,
            "status": format!("{:?}", s.status),
            "createdAt": s.created_at.0,
        })).collect::<Vec<_>>(),
    }))
}
