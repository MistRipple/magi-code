use axum::{
    Json, Router,
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use magi_bridge_client::ModelInvocationRequest;
use magi_conversation_runtime::session_turn_execution::BUSINESS_MODEL_PROVIDER;
use magi_conversation_runtime::task_execution_dispatcher::{RoleTarget, resolve_target_for_role};
use magi_snapshot::SnapshotSession;

use super::session_scope::{
    parse_session_id, require_registered_workspace_binding,
    resolve_optional_session_workspace_scope,
};
use crate::{
    change_projection::{
        SessionChangeScope, WorkspaceChangeScope, pending_changes_state,
        resolve_session_change_scope, resolve_workspace_change_scope, safe_relative_path,
        safe_workspace_path,
    },
    errors::ApiError,
    state::ApiState,
    tunnel::RemoteAccessBinding,
};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/changes/diff", get(get_diff))
        .route("/changes/approve", post(approve_change))
        .route("/changes/revert", post(revert_change))
        .route("/changes/approve-all", post(approve_all_changes))
        .route("/changes/revert-all", post(revert_all_changes))
        .route(
            "/changes/revert-execution-group",
            post(revert_execution_group_changes),
        )
        .route("/files/content", get(get_file_content))
        .route("/files/raw", get(get_file_raw))
        .route("/filesystem/list", get(list_filesystem))
        .route("/filesystem/browse", get(browse_filesystem))
        .route("/tunnel/start", post(start_tunnel))
        .route("/tunnel/stop", post(stop_tunnel))
        .route("/tunnel/status", get(tunnel_status))
        .route("/lan-access", get(lan_access_status))
        .route("/prompt/enhance", post(enhance_prompt))
}

async fn require_snapshot_session(
    state: &ApiState,
    scope: &SessionChangeScope,
) -> Result<Arc<SnapshotSession>, ApiError> {
    state
        .ensure_snapshot_session(&scope.session_id, &scope.workspace_root)
        .await
}

fn workspace_path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn file_access_error(action: &'static str, path: &Path, error: impl Display) -> ApiError {
    tracing::warn!(
        action,
        path = %path.display(),
        error = %error,
        "file access failed"
    );
    ApiError::InvalidInput("文件不可读取或不存在".to_string())
}

fn directory_access_error(action: &'static str, path: &Path, error: impl Display) -> ApiError {
    tracing::warn!(
        action,
        path = %path.display(),
        error = %error,
        "directory access failed"
    );
    ApiError::InvalidInput("目录不可读取或不存在".to_string())
}

fn session_scope_binding(scope: &SessionChangeScope) -> serde_json::Value {
    serde_json::json!({
        "sessionId": scope.session_id.as_str(),
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "executionGroupId": scope.execution_group_id,
    })
}

fn workspace_scope_binding(scope: &WorkspaceChangeScope) -> serde_json::Value {
    serde_json::json!({
        "sessionId": serde_json::Value::Null,
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "executionGroupId": serde_json::Value::Null,
    })
}

fn resolve_workspace_change_scope_from_request(
    state: &ApiState,
    workspace_id: Option<&str>,
    workspace_path: Option<&str>,
) -> Result<WorkspaceChangeScope, ApiError> {
    let binding = require_registered_workspace_binding(state, workspace_id, workspace_path)?;
    resolve_workspace_change_scope(state, &binding.workspace_id)
}

fn resolve_session_change_scope_from_request(
    state: &ApiState,
    session_id: &magi_core::SessionId,
    workspace_id: Option<&str>,
    workspace_path: Option<&str>,
    execution_group_id: Option<&str>,
) -> Result<SessionChangeScope, ApiError> {
    let request_scope = resolve_optional_session_workspace_scope(
        state,
        Some(session_id.as_str()),
        workspace_id,
        workspace_path,
    )?;
    let resolved_workspace_id = request_scope
        .workspace_id()
        .map(|workspace_id| workspace_id.as_str().to_string());
    resolve_session_change_scope(
        state,
        session_id,
        resolved_workspace_id.as_deref(),
        execution_group_id,
    )
}

// ─── Changes ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffQuery {
    file_path: Option<String>,
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    execution_group_id: Option<String>,
}

async fn get_diff(
    State(state): State<ApiState>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 没有 sessionId 时不再回退到 git diff —— 全局变更视图已不再属于本系统职责。
    let (diff, binding, pending_changes_state) = match query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(_) => {
            let session_id = parse_session_id(query.session_id.as_deref())?;
            let scope = resolve_session_change_scope_from_request(
                &state,
                &session_id,
                query.workspace_id.as_deref(),
                query.workspace_path.as_deref(),
                query.execution_group_id.as_deref(),
            )?;
            let snapshot = require_snapshot_session(&state, &scope).await?;
            let pending = snapshot
                .pending_changes()
                .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
            let pending_state = pending_changes_state(
                "ready",
                Some(&scope.session_id),
                Some(&scope.workspace_id),
                Some(&scope.workspace_root),
                pending.len(),
                None,
            );
            let diff = match query.file_path.as_deref() {
                Some(fp) => {
                    let rel = safe_relative_path(fp)?;
                    pending
                        .iter()
                        .find(|c| c.path == rel)
                        .and_then(|c| c.unified_diff.clone())
                        .unwrap_or_default()
                }
                None => {
                    let exec_group = query
                        .execution_group_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| scope.execution_group_id.clone());
                    pending
                        .iter()
                        .filter(|c| {
                            c.execution_group_id
                                .as_deref()
                                .map(|id| id == exec_group)
                                .unwrap_or(true)
                        })
                        .filter_map(|c| c.unified_diff.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };
            (diff, session_scope_binding(&scope), pending_state)
        }
        None => {
            // 无 session 调用：仅做一次 workspace 校验，统一返回空 diff，
            // 不再读 git 来伪装出全局变更。
            let scope = resolve_workspace_change_scope_from_request(
                &state,
                query.workspace_id.as_deref(),
                query.workspace_path.as_deref(),
            )?;
            let pending_state = pending_changes_state(
                "unavailable",
                None,
                Some(&scope.workspace_id),
                Some(&scope.workspace_root),
                0,
                Some("session_unbound"),
            );
            (
                String::new(),
                workspace_scope_binding(&scope),
                pending_state,
            )
        }
    };
    let mut payload = serde_json::json!({
        "diff": diff,
        "filePath": query.file_path,
        "pendingChangesState": pending_changes_state,
    });
    if let Some(object) = payload.as_object_mut()
        && let Some(binding) = binding.as_object()
    {
        object.extend(binding.clone());
    }
    Ok(Json(payload))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveChangeRequest {
    file_path: String,
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
}

async fn approve_change(
    State(state): State<ApiState>,
    Json(request): Json<ApproveChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope = resolve_session_change_scope_from_request(
        &state,
        &session_id,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        None,
    )?;
    let rel = safe_relative_path(&request.file_path)?.to_string();
    let snapshot = require_snapshot_session(&state, &scope).await?;
    snapshot
        .approve(&[rel])
        .map_err(|e| ApiError::internal_assembly("approve 变更失败", e))?;
    Ok(Json(serde_json::json!({
        "approved": true,
        "filePath": request.file_path,
        "sessionId": scope.session_id.as_str(),
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "executionGroupId": scope.execution_group_id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertChangeRequest {
    file_path: String,
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
}

async fn revert_change(
    State(state): State<ApiState>,
    Json(request): Json<RevertChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope = resolve_session_change_scope_from_request(
        &state,
        &session_id,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        None,
    )?;
    let rel = safe_relative_path(&request.file_path)?.to_string();
    let snapshot = require_snapshot_session(&state, &scope).await?;
    snapshot
        .revert(&[rel])
        .map_err(|e| ApiError::internal_assembly("revert 变更失败", e))?;
    Ok(Json(serde_json::json!({
        "reverted": true,
        "filePath": request.file_path,
        "sessionId": scope.session_id.as_str(),
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "executionGroupId": scope.execution_group_id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveAllRequest {
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
}

async fn approve_all_changes(
    State(state): State<ApiState>,
    Json(request): Json<ApproveAllRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope = resolve_session_change_scope_from_request(
        &state,
        &session_id,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        None,
    )?;
    let snapshot = require_snapshot_session(&state, &scope).await?;
    let pending = snapshot
        .pending_changes()
        .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
    let paths: Vec<String> = pending.iter().map(|c| c.path.clone()).collect();
    snapshot
        .approve(&paths)
        .map_err(|e| ApiError::internal_assembly("approve 全部变更失败", e))?;
    Ok(Json(serde_json::json!({
        "approved": true,
        "approvedFiles": paths,
        "sessionId": scope.session_id.as_str(),
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "executionGroupId": scope.execution_group_id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertAllRequest {
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
}

async fn revert_all_changes(
    State(state): State<ApiState>,
    Json(request): Json<RevertAllRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope = resolve_session_change_scope_from_request(
        &state,
        &session_id,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        None,
    )?;
    let snapshot = require_snapshot_session(&state, &scope).await?;
    let pending = snapshot
        .pending_changes()
        .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
    let paths: Vec<String> = pending.iter().map(|c| c.path.clone()).collect();
    snapshot
        .revert(&paths)
        .map_err(|e| ApiError::internal_assembly("revert 全部变更失败", e))?;
    Ok(Json(serde_json::json!({
        "reverted": true,
        "revertedFiles": paths,
        "sessionId": scope.session_id.as_str(),
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "executionGroupId": scope.execution_group_id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertExecutionGroupRequest {
    execution_group_id: String,
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
}

async fn revert_execution_group_changes(
    State(state): State<ApiState>,
    Json(request): Json<RevertExecutionGroupRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope = resolve_session_change_scope_from_request(
        &state,
        &session_id,
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        Some(request.execution_group_id.as_str()),
    )?;
    if scope.execution_group_id != request.execution_group_id {
        return Err(ApiError::InvalidInput(format!(
            "执行分组 {} 不属于当前会话 {}",
            request.execution_group_id, scope.session_id
        )));
    }
    let snapshot = require_snapshot_session(&state, &scope).await?;
    let pending = snapshot
        .pending_changes()
        .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
    let paths: Vec<String> = pending
        .iter()
        .filter(|c| {
            c.execution_group_id
                .as_deref()
                .map(|id| id == request.execution_group_id)
                .unwrap_or(false)
        })
        .map(|c| c.path.clone())
        .collect();
    snapshot
        .revert(&paths)
        .map_err(|e| ApiError::internal_assembly("revert 执行分组失败", e))?;

    Ok(Json(serde_json::json!({
        "reverted": true,
        "executionGroupId": request.execution_group_id,
        "revertedFiles": paths,
        "sessionId": scope.session_id.as_str(),
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
    })))
}

// ─── Files ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileContentQuery {
    file_path: Option<String>,
    session_id: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    execution_group_id: Option<String>,
}

async fn get_file_content(
    State(state): State<ApiState>,
    Query(query): Query<FileContentQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = query
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("文件路径不能为空".to_string()))?;
    let (absolute_path, binding) = if query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        let session_id = parse_session_id(query.session_id.as_deref())?;
        let scope = resolve_session_change_scope_from_request(
            &state,
            &session_id,
            query.workspace_id.as_deref(),
            query.workspace_path.as_deref(),
            query.execution_group_id.as_deref(),
        )?;
        let (absolute, _relative) = safe_workspace_path(&scope.workspace_root, path)?;
        (absolute, session_scope_binding(&scope))
    } else {
        let scope = resolve_workspace_change_scope_from_request(
            &state,
            query.workspace_id.as_deref(),
            query.workspace_path.as_deref(),
        )?;
        let (absolute, _) = safe_workspace_path(&scope.workspace_root, path)?;
        (absolute, workspace_scope_binding(&scope))
    };
    let content = std::fs::read_to_string(&absolute_path)
        .map_err(|e| file_access_error("读取文件内容失败", &absolute_path, e))?;
    let mut payload = serde_json::json!({
        "content": content,
        "filePath": query.file_path,
    });
    if let Some(object) = payload.as_object_mut()
        && let Some(binding) = binding.as_object()
    {
        object.extend(binding.clone());
    }
    Ok(Json(payload))
}

/// 按文件扩展名推断图片 MIME 类型；非图片返回 None（用于白名单拦截）。
fn image_mime_for_path(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// 返回图片文件原始字节流（带 Content-Type），供前端 `<img src>` 直接预览。
///
/// 与 `/files/content`（仅 UTF-8 文本）职责分离：图片是二进制，read_to_string
/// 会乱码/报错。仅服务图片扩展名白名单——非图片返回 415，避免该端点被当作任意
/// 文件下载通道。路径解析、工作区越界防护完全复用 content 端点的同一套逻辑。
async fn get_file_raw(
    State(state): State<ApiState>,
    Query(query): Query<FileContentQuery>,
) -> Result<Response, ApiError> {
    let path = query
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("文件路径不能为空".to_string()))?;

    let absolute_path = if query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        let session_id = parse_session_id(query.session_id.as_deref())?;
        let scope = resolve_session_change_scope_from_request(
            &state,
            &session_id,
            query.workspace_id.as_deref(),
            query.workspace_path.as_deref(),
            query.execution_group_id.as_deref(),
        )?;
        let (absolute, _relative) = safe_workspace_path(&scope.workspace_root, path)?;
        absolute
    } else {
        let scope = resolve_workspace_change_scope_from_request(
            &state,
            query.workspace_id.as_deref(),
            query.workspace_path.as_deref(),
        )?;
        let (absolute, _) = safe_workspace_path(&scope.workspace_root, path)?;
        absolute
    };

    let mime = image_mime_for_path(&absolute_path)
        .ok_or_else(|| ApiError::InvalidInput("仅支持图片文件预览".to_string()))?;

    let bytes = std::fs::read(&absolute_path)
        .map_err(|e| file_access_error("读取文件内容失败", &absolute_path, e))?;

    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemListQuery {
    path: Option<String>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    #[serde(default)]
    show_hidden: Option<String>,
}

fn show_hidden_enabled(value: Option<&str>) -> bool {
    value
        .map(str::trim)
        .is_some_and(|value| matches!(value, "1" | "true" | "yes" | "on"))
}

fn canonical_directory_path(
    path: PathBuf,
    error_context: &'static str,
) -> Result<PathBuf, ApiError> {
    let canonical = path
        .canonicalize()
        .map_err(|e| directory_access_error(error_context, &path, e))?;
    if !canonical.is_dir() {
        return Err(ApiError::InvalidInput("路径不是目录".to_string()));
    }
    Ok(canonical)
}

fn read_directory_entries(
    path: &Path,
    show_hidden: bool,
) -> Result<Vec<serde_json::Value>, ApiError> {
    if !path.is_dir() {
        return Err(ApiError::InvalidInput("路径不是目录".to_string()));
    }
    let entries = std::fs::read_dir(path)
        .map_err(|e| directory_access_error("读取目录失败", path, e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            show_hidden
                || !entry
                    .file_name()
                    .to_string_lossy()
                    .as_ref()
                    .starts_with('.')
        })
        .map(|entry| {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            serde_json::json!({
                "name": entry.file_name().to_string_lossy(),
                "path": entry.path().to_string_lossy(),
                "isDirectory": is_dir,
            })
        })
        .collect();
    Ok(entries)
}

fn directory_parent(path: &Path, boundary: Option<&Path>) -> String {
    let parent = path
        .parent()
        .filter(|parent| boundary.is_none_or(|boundary| parent.starts_with(boundary)))
        .unwrap_or(path);
    parent.to_string_lossy().to_string()
}

async fn list_filesystem(
    State(state): State<ApiState>,
    Query(query): Query<FilesystemListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = resolve_workspace_change_scope_from_request(
        &state,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let canonical_workspace_root =
        canonical_directory_path(scope.workspace_root.clone(), "规范化工作区根目录失败")?;
    let path = match query
        .path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(p) => safe_workspace_path(&scope.workspace_root, p)?.0,
        None => canonical_workspace_root.clone(),
    };
    let path = canonical_directory_path(path, "规范化目录失败")?;
    let show_hidden = show_hidden_enabled(query.show_hidden.as_deref());
    let entries = read_directory_entries(&path, show_hidden)?;
    Ok(Json(serde_json::json!({
        "workspaceId": scope.workspace_id.as_str(),
        "workspacePath": workspace_path_string(&scope.workspace_root),
        "path": path.to_string_lossy(),
        "parent": directory_parent(&path, Some(&canonical_workspace_root)),
        "entries": entries,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemBrowseQuery {
    path: Option<String>,
    #[serde(default)]
    show_hidden: Option<String>,
}

async fn browse_filesystem(
    Query(query): Query<FilesystemBrowseQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let raw_path = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"))
        });
    let path = canonical_directory_path(raw_path, "规范化目录失败")?;
    let show_hidden = show_hidden_enabled(query.show_hidden.as_deref());
    let entries = read_directory_entries(&path, show_hidden)?;
    Ok(Json(serde_json::json!({
        "path": path.to_string_lossy(),
        "parent": directory_parent(&path, None),
        "entries": entries,
    })))
}

// ─── Tunnel ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTunnelRequest {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    session_id: Option<String>,
}

async fn start_tunnel(
    State(state): State<ApiState>,
    Json(request): Json<StartTunnelRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let binding = RemoteAccessBinding::new(
        request.workspace_id.as_deref(),
        request.workspace_path.as_deref(),
        request.session_id.as_deref(),
    );
    let tunnel_state = state.tunnel_manager.start(binding).await;
    Ok(Json(
        serde_json::to_value(&tunnel_state).unwrap_or_default(),
    ))
}

async fn stop_tunnel(State(state): State<ApiState>) -> Result<Json<serde_json::Value>, ApiError> {
    let tunnel_state = state.tunnel_manager.stop().await;
    Ok(Json(
        serde_json::to_value(&tunnel_state).unwrap_or_default(),
    ))
}

async fn tunnel_status(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let tunnel_state = state.tunnel_manager.get_state().await;
    Json(serde_json::to_value(&tunnel_state).unwrap_or_default())
}

async fn lan_access_status(
    State(state): State<ApiState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let ip = resolve_preferred_lan_ipv4();
    let port = state.tunnel_manager.local_port().await;

    let binding = RemoteAccessBinding::new(
        query.get("workspaceId").map(String::as_str),
        query.get("workspacePath").map(String::as_str),
        query.get("sessionId").map(String::as_str),
    );
    let url = binding.web_access_url(&format!("http://{}:{}/web.html", ip, port), None);

    Json(serde_json::json!({
        "enabled": true,
        "url": url,
        "ip": ip,
        "port": port,
    }))
}

/// 获取首选的局域网 IPv4 地址（遍历网卡接口 + 评分）
fn resolve_preferred_lan_ipv4() -> String {
    use std::process::Command;

    // 通过 ifconfig (macOS/Linux) 获取所有 IPv4 地址
    let output = Command::new("ifconfig")
        .output()
        .or_else(|_| Command::new("ip").args(["addr", "show"]).output());

    let text = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return fallback_udp_ip(),
    };

    let mut candidates: Vec<(String, i32)> = Vec::new();

    // 解析 ifconfig 输出中的 inet 行
    let mut current_iface = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // 新接口行（不以空白开头）
        if !line.starts_with(' ') && !line.starts_with('\t') && line.contains(':') {
            current_iface = line.split(':').next().unwrap_or("").trim().to_string();
        }
        // inet 地址行
        if let Some(addr) = extract_ipv4_from_line(trimmed) {
            if addr == "127.0.0.1" || addr == "0.0.0.0" {
                continue;
            }
            let score = score_lan_candidate(&current_iface, &addr);
            candidates.push((addr, score));
        }
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates
        .into_iter()
        .next()
        .map(|(ip, _)| ip)
        .unwrap_or_else(fallback_udp_ip)
}

fn extract_ipv4_from_line(line: &str) -> Option<String> {
    // 匹配 "inet 192.168.1.100" 或 "inet addr:192.168.1.100"
    if !line.starts_with("inet ") && !line.contains("inet ") {
        return None;
    }
    for part in line.split_whitespace() {
        if part.contains('.') && part.chars().all(|c| c.is_ascii_digit() || c == '.') {
            let segments: Vec<&str> = part.split('.').collect();
            if segments.len() == 4 && segments.iter().all(|s| s.parse::<u8>().is_ok()) {
                return Some(part.to_string());
            }
        }
        // "addr:x.x.x.x" 格式
        if let Some(addr) = part.strip_prefix("addr:") {
            let segments: Vec<&str> = addr.split('.').collect();
            if segments.len() == 4 && segments.iter().all(|s| s.parse::<u8>().is_ok()) {
                return Some(addr.to_string());
            }
        }
    }
    None
}

fn score_lan_candidate(iface: &str, addr: &str) -> i32 {
    let mut score = 0i32;
    // 优先物理网卡
    let iface_lower = iface.to_lowercase();
    if iface_lower.starts_with("en")
        || iface_lower.starts_with("eth")
        || iface_lower.starts_with("wlan")
        || iface_lower.contains("wi-fi")
    {
        score += 50;
    }
    // 排除虚拟网卡
    if iface_lower.starts_with("bridge")
        || iface_lower.starts_with("docker")
        || iface_lower.starts_with("veth")
        || iface_lower.starts_with("utun")
        || iface_lower.starts_with("tun")
        || iface_lower.starts_with("tap")
        || iface_lower.starts_with("vmnet")
        || iface_lower.starts_with("lo")
    {
        score -= 100;
    }
    // 优先常见私网段
    if addr.starts_with("192.168.") {
        score += 30;
    } else if addr.starts_with("10.") {
        score += 20;
    } else if addr.starts_with("172.") {
        score += 10;
    }
    // 简化判断
    else {
        score -= 20;
    }
    score
}

fn fallback_udp_ip() -> String {
    use std::net::UdpSocket;
    if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if ip != "0.0.0.0" && ip != "127.0.0.1" {
                    return ip;
                }
            }
        }
    }
    "127.0.0.1".to_string()
}

// ─── Prompt Enhance ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnhancePromptRequest {
    prompt: String,
    #[serde(default)]
    skill_name: Option<String>,
    #[serde(default)]
    skill_description: Option<String>,
}

fn build_enhance_prompt_instruction(
    prompt: &str,
    skill_name: Option<&str>,
    skill_description: Option<&str>,
) -> String {
    let mut sections = Vec::new();
    sections.push(
        "请优化以下用户 prompt，使其更清晰、具体、可执行。只输出优化后的 prompt，不要添加额外解释。"
            .to_string(),
    );
    sections.push(
        "要求：\n- 如果原文已经足够清晰，不要无意义扩写\n- 保留用户原始意图与语言风格\n- 如果存在当前技能上下文，请保留该技能的任务边界，不要改写成泛化闲聊或无关任务"
            .to_string(),
    );
    let skill_name = skill_name.map(str::trim).filter(|value| !value.is_empty());
    let skill_description = skill_description
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if skill_name.is_some() || skill_description.is_some() {
        let mut skill_section = String::from("当前技能上下文：");
        if let Some(name) = skill_name {
            skill_section.push_str(&format!("\n- 名称：/{}", name));
        }
        if let Some(description) = skill_description {
            skill_section.push_str(&format!("\n- 说明：{}", description));
        }
        sections.push(skill_section);
    }
    sections.push(format!("原始 prompt:\n{}", prompt.trim()));
    sections.join("\n\n")
}

async fn enhance_prompt(
    State(state): State<ApiState>,
    Json(request): Json<EnhancePromptRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(ApiError::InvalidInput("提示词不能为空".to_string()));
    }
    let Some(client) =
        resolve_target_for_role(Some(&state.settings_store), None, RoleTarget::Auxiliary)
            .ok()
            .flatten()
    else {
        return Err(ApiError::InvalidInput(
            "辅助模型未配置，无法增强提示词；请在设置中配置 auxiliary 模型".to_string(),
        ));
    };

    let invocation = ModelInvocationRequest {
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt: build_enhance_prompt_instruction(
            prompt,
            request.skill_name.as_deref(),
            request.skill_description.as_deref(),
        ),
        messages: None,
        tools: None,
        tool_choice: None,
    };

    let response = match client.invoke(invocation) {
        Ok(resp) if resp.ok => resp,
        Ok(resp) => {
            tracing::warn!(
                payload = %resp.payload.trim(),
                "prompt enhance auxiliary model returned non-ok response"
            );
            return Err(ApiError::model_invocation_failed(
                "辅助模型返回失败",
                "辅助模型返回非成功状态",
            ));
        }
        Err(error) => {
            return Err(ApiError::model_invocation_failed("辅助模型调用失败", error));
        }
    };

    let payload = response.parse_chat_payload();
    let Some(content) = payload
        .content
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
    else {
        return Err(ApiError::InvalidInput("辅助模型返回内容为空".to_string()));
    };

    Ok(Json(serde_json::json!({
        "enhancedPrompt": content,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_projection::collect_session_pending_changes_with_state;
    use crate::state::ApiState;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use magi_core::{
        AbsolutePath, ExecutionOwnership, MissionId, SessionId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let unique = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "{}-{}-{}-{}",
            prefix,
            std::process::id(),
            UtcMillis::now().0,
            unique
        ));
        fs::create_dir_all(&dir).expect("temp dir should create");
        dir
    }

    fn build_state() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        )
        .with_task_store(Arc::new(TaskStore::new()))
    }

    fn build_state_with_workspace_root(root: &Path, workspace_id: &str) -> ApiState {
        let state = build_state();
        state
            .workspace_registry
            .register(
                WorkspaceId::new(workspace_id),
                AbsolutePath::new(root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
        state
    }

    async fn read_json_response(response: axum::response::Response) -> serde_json::Value {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&body).expect("payload should deserialize")
    }

    /// 注册 workspace + session（可选 mission）。session 创建后立即在 SnapshotManager
    /// 中拉起 SnapshotSession，并完成首次 baseline 扫描；调用方随后做文件改动 + reconcile。
    async fn register_workspace_and_snapshot(
        state: &ApiState,
        workspace_id: &str,
        session_id: &str,
        root: &Path,
        mission_id: Option<&str>,
    ) -> Arc<SnapshotSession> {
        let ws = WorkspaceId::new(workspace_id);
        let sid = SessionId::new(session_id);
        state
            .workspace_registry
            .register(
                ws.clone(),
                AbsolutePath::new(root.to_string_lossy().as_ref()),
            )
            .expect("workspace should register");
        state
            .session_store
            .create_session_for_workspace(sid.clone(), session_id, Some(workspace_id.to_string()))
            .expect("session should create");
        if let Some(mid) = mission_id {
            state.session_store.bind_execution_ownership(
                sid.clone(),
                ExecutionOwnership {
                    session_id: Some(sid.clone()),
                    workspace_id: Some(ws.clone()),
                    mission_id: Some(MissionId::new(mid)),
                    ..ExecutionOwnership::default()
                },
            );
        }
        state
            .snapshot_manager
            .start_session(session_id.to_string(), root.to_path_buf())
            .await
            .expect("snapshot session should start")
    }

    #[tokio::test]
    async fn lan_access_uses_current_daemon_port() {
        let root = unique_temp_dir("magi-changes-route-lan-access");
        let state =
            build_state_with_workspace_root(&root, "workspace-lan-access").with_tunnel_port(39219);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/lan-access?workspaceId=workspace-lan-access&workspacePath=%2Ftmp%2Fmagi%20test&sessionId=session-lan-access")
                    .method("GET")
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
        assert_eq!(payload["port"], 39219);
        assert!(
            payload["url"]
                .as_str()
                .expect("url should be string")
                .contains(":39219/web.html?workspaceId=workspace-lan-access")
        );
        let url = payload["url"].as_str().expect("url should be string");
        assert!(url.contains("workspacePath=%2Ftmp%2Fmagi%20test"));
        assert!(url.contains("sessionId=session-lan-access"));
    }

    #[tokio::test]
    async fn filesystem_list_is_workspace_bound_and_filters_hidden_entries() {
        let root = unique_temp_dir("magi-filesystem-list-bound");
        fs::write(root.join("visible.txt"), "visible\n").expect("visible file should write");
        fs::write(root.join(".hidden"), "hidden\n").expect("hidden file should write");
        fs::create_dir_all(root.join("src")).expect("src dir should create");
        let state = build_state_with_workspace_root(&root, "workspace-filesystem-list");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/filesystem/list?workspaceId=workspace-filesystem-list")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["workspaceId"], "workspace-filesystem-list");
        assert!(
            payload["workspacePath"]
                .as_str()
                .is_some_and(|path| path.contains("magi-filesystem-list-bound")),
            "filesystem payload must carry workspace path"
        );
        assert!(
            payload["path"]
                .as_str()
                .is_some_and(|path| path.contains("magi-filesystem-list-bound")),
            "filesystem payload must carry listed path"
        );
        let entries = payload["entries"].as_array().expect("entries array");
        assert!(
            entries.iter().any(|entry| entry["name"] == "visible.txt"),
            "visible file should be listed"
        );
        assert!(
            entries.iter().all(|entry| entry["name"] != ".hidden"),
            "hidden file should be filtered by default"
        );

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/filesystem/list?workspaceId=workspace-filesystem-list&showHidden=1")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        let entries = payload["entries"].as_array().expect("entries array");
        assert!(
            entries.iter().any(|entry| entry["name"] == ".hidden"),
            "showHidden=1 should include hidden entries"
        );
    }

    #[tokio::test]
    async fn filesystem_list_resolves_workspace_from_registered_path_when_query_id_is_stale() {
        let root = unique_temp_dir("magi-filesystem-list-path-binding");
        fs::write(root.join("from-path.txt"), "workspace path wins\n")
            .expect("workspace file should write");
        let state = build_state_with_workspace_root(&root, "workspace-filesystem-path");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/filesystem/list?workspaceId=workspace-stale-query&workspacePath={}",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["workspaceId"], "workspace-filesystem-path");
        assert_eq!(payload["workspacePath"], root.to_string_lossy().as_ref());
        let entries = payload["entries"].as_array().expect("entries array");
        assert!(
            entries.iter().any(|entry| entry["name"] == "from-path.txt"),
            "filesystem list must read the workspace resolved from workspacePath"
        );
    }

    #[tokio::test]
    async fn filesystem_list_rejects_missing_workspace_and_outside_path() {
        let root = unique_temp_dir("magi-filesystem-list-secure");
        let outside = unique_temp_dir("magi-filesystem-list-outside");
        let state = build_state_with_workspace_root(&root, "workspace-filesystem-secure");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/filesystem/list")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/filesystem/list?workspaceId=workspace-filesystem-secure&path={}",
                        outside.to_string_lossy()
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn filesystem_browse_lists_picker_directory_without_workspace_scope() {
        let root = unique_temp_dir("magi-filesystem-browse-picker");
        fs::write(root.join("visible.txt"), "visible\n").expect("visible file should write");
        fs::write(root.join(".hidden"), "hidden\n").expect("hidden file should write");
        fs::create_dir_all(root.join("workspace-candidate"))
            .expect("workspace candidate dir should create");
        let state = build_state();

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/filesystem/browse?path={}",
                        root.to_string_lossy()
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert!(
            payload["workspaceId"].is_null(),
            "browse payload must not claim a workspace binding"
        );
        assert!(
            payload["path"]
                .as_str()
                .is_some_and(|path| path.contains("magi-filesystem-browse-picker")),
            "browse payload must carry listed path"
        );
        let entries = payload["entries"].as_array().expect("entries array");
        assert!(
            entries
                .iter()
                .any(|entry| entry["name"] == "workspace-candidate"),
            "directory candidates should be listed"
        );
        assert!(
            entries.iter().all(|entry| entry["name"] != ".hidden"),
            "hidden entries should be filtered by default"
        );

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/filesystem/browse?path={}&showHidden=1",
                        root.to_string_lossy()
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        let entries = payload["entries"].as_array().expect("entries array");
        assert!(
            entries.iter().any(|entry| entry["name"] == ".hidden"),
            "showHidden=1 should include hidden entries"
        );
    }

    #[tokio::test]
    async fn filesystem_browse_uses_public_missing_directory_error() {
        let root = unique_temp_dir("magi-filesystem-browse-missing");
        let missing = root.join("missing");
        let state = build_state();

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/filesystem/browse?path={}",
                        missing.to_string_lossy()
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = read_json_response(response).await;
        assert_eq!(payload["message"], "目录不可读取或不存在");
    }

    #[tokio::test]
    async fn get_diff_returns_empty_without_session_scope() {
        let root = unique_temp_dir("magi-changes-route-no-session-diff");
        fs::write(root.join("notes.txt"), "hello\n").expect("workspace file should write");
        let state = build_state_with_workspace_root(&root, "workspace-no-session");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/diff?workspaceId=workspace-no-session")
                    .method("GET")
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
        assert_eq!(payload["diff"], "");
        assert!(payload["filePath"].is_null());
    }

    #[tokio::test]
    async fn get_diff_resolves_workspace_from_registered_path_when_query_id_is_stale() {
        let root = unique_temp_dir("magi-changes-route-path-diff");
        fs::write(root.join("notes.txt"), "hello\n").expect("workspace file should write");
        let state = build_state_with_workspace_root(&root, "workspace-path-diff");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/changes/diff?workspaceId=workspace-stale-query&workspacePath={}",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["workspaceId"], "workspace-path-diff");
        assert_eq!(payload["workspacePath"], root.to_string_lossy().as_ref());
        assert_eq!(payload["diff"], "");
        assert_eq!(
            payload["pendingChangesState"]["reasonCode"],
            "session_unbound"
        );
    }

    #[tokio::test]
    async fn get_diff_rejects_missing_workspace_without_session() {
        let root = unique_temp_dir("magi-changes-route-no-session-missing-workspace");
        fs::write(root.join("notes.txt"), "hello\n").expect("workspace file should write");
        let state = build_state_with_workspace_root(&root, "workspace-no-session-missing");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/diff")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = read_json_response(response).await;
        assert!(
            payload["message"]
                .as_str()
                .unwrap_or_default()
                .contains("workspaceId 不能为空")
        );
    }

    #[tokio::test]
    async fn get_diff_returns_unified_diff_for_session_file() {
        let state = build_state();
        let root = unique_temp_dir("magi-changes-route-session-diff");
        fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
        let snap = register_workspace_and_snapshot(
            &state,
            "ws-session-diff",
            "sess-session-diff",
            &root,
            Some("mission-diff"),
        )
        .await;
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
        snap.reconcile().expect("reconcile should succeed");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/diff?sessionId=sess-session-diff&workspaceId=ws-session-diff&filePath=alpha.txt")
                    .method("GET")
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
        assert_eq!(payload["workspaceId"], "ws-session-diff");
        assert_eq!(payload["pendingChangesState"]["status"], "ready");
        assert_eq!(payload["pendingChangesState"]["pendingCount"], 1);
        assert!(
            payload["workspacePath"]
                .as_str()
                .is_some_and(|path| path.contains("magi-changes-route-session-diff")),
            "diff payload must carry canonical workspace path"
        );
        assert_eq!(payload["sessionId"], "sess-session-diff");
        assert_eq!(payload["executionGroupId"], "mission-diff");
        let diff = payload["diff"].as_str().unwrap_or_default();
        assert!(diff.contains("alpha"), "diff should mention path: {}", diff);
        assert!(
            diff.contains("-alpha"),
            "diff should contain old line marker: {}",
            diff
        );
        assert!(
            diff.contains("+alpha changed"),
            "diff should contain new line marker: {}",
            diff
        );
    }

    #[tokio::test]
    async fn get_diff_resolves_session_workspace_from_registered_path_when_query_id_is_stale() {
        let state = build_state();
        let root = unique_temp_dir("magi-changes-route-session-path-diff");
        fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
        let snap = register_workspace_and_snapshot(
            &state,
            "ws-session-path-diff",
            "sess-session-path-diff",
            &root,
            None,
        )
        .await;
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
        snap.reconcile().expect("reconcile should succeed");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/changes/diff?sessionId=sess-session-path-diff&workspaceId=workspace-stale-query&workspacePath={}&filePath=alpha.txt",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["sessionId"], "sess-session-path-diff");
        assert_eq!(payload["workspaceId"], "ws-session-path-diff");
        assert_eq!(payload["workspacePath"], root.to_string_lossy().as_ref());
        assert!(
            payload["diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("+alpha changed")),
            "session diff must use the workspace resolved from workspacePath"
        );
    }

    #[tokio::test]
    async fn get_diff_lazily_starts_snapshot_session_for_bound_session() {
        let root = unique_temp_dir("magi-changes-route-lazy-snapshot");
        fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
        let state = build_state_with_workspace_root(&root, "ws-lazy-snapshot");
        let session_id = SessionId::new("sess-lazy-snapshot");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "lazy snapshot",
                Some("ws-lazy-snapshot".to_string()),
            )
            .expect("session should create");
        assert!(
            state.snapshot_session(&session_id).is_none(),
            "测试前不应手动启动快照账本"
        );

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/changes/diff?sessionId=sess-lazy-snapshot&workspaceId=ws-lazy-snapshot")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["pendingChangesState"]["status"], "ready");
        assert_eq!(payload["pendingChangesState"]["pendingCount"], 0);
        assert!(
            state.snapshot_session(&session_id).is_some(),
            "显式变更路由必须在账本缺失时按会话/工作区启动快照账本"
        );
    }

    #[tokio::test]
    async fn approve_all_clears_pending_changes() {
        let state = build_state();
        let root = unique_temp_dir("magi-changes-route-approve-all");
        fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
        let snap = register_workspace_and_snapshot(
            &state,
            "ws-approve-all",
            "sess-approve-all",
            &root,
            None,
        )
        .await;
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
        snap.reconcile().expect("reconcile should succeed");

        let before_projection = collect_session_pending_changes_with_state(
            &state,
            &SessionId::new("sess-approve-all"),
            Some("ws-approve-all"),
        )
        .expect("pending changes should collect before approval");
        let before = before_projection.pending_changes;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].file_path, "alpha.txt");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/changes/approve-all")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "sess-approve-all",
                            "workspaceId": "ws-approve-all"
                        })
                        .to_string(),
                    ))
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
        assert_eq!(payload["workspaceId"], "ws-approve-all");
        assert!(
            payload["workspacePath"]
                .as_str()
                .is_some_and(|path| path.contains("magi-changes-route-approve-all")),
            "approve payload must carry canonical workspace path"
        );
        assert_eq!(payload["sessionId"], "sess-approve-all");
        assert_eq!(payload["executionGroupId"], "session:sess-approve-all");

        let after_projection = collect_session_pending_changes_with_state(
            &state,
            &SessionId::new("sess-approve-all"),
            Some("ws-approve-all"),
        )
        .expect("pending changes should collect after approval");
        let after = after_projection.pending_changes;
        assert!(
            after.is_empty(),
            "approved files should disappear from pending changes"
        );
    }

    #[tokio::test]
    async fn revert_all_removes_added_file() {
        let state = build_state();
        let root = unique_temp_dir("magi-changes-route-revert-all");
        let snap = register_workspace_and_snapshot(
            &state,
            "ws-revert-all",
            "sess-revert-all",
            &root,
            None,
        )
        .await;
        fs::create_dir_all(root.join("tmp")).expect("tmp dir should create");
        fs::write(root.join("tmp/added.txt"), "new file\n").expect("added file should write");
        snap.reconcile().expect("reconcile should succeed");

        let before_projection = collect_session_pending_changes_with_state(
            &state,
            &SessionId::new("sess-revert-all"),
            Some("ws-revert-all"),
        )
        .expect("pending changes should collect");
        let before = before_projection.pending_changes;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].file_path, "tmp/added.txt");
        assert_eq!(before[0].r#type, "add");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert-all")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "sess-revert-all",
                            "workspaceId": "ws-revert-all"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(!root.join("tmp/added.txt").exists());
    }

    #[tokio::test]
    async fn revert_change_restores_modified_file_to_baseline() {
        let state = build_state();
        let root = unique_temp_dir("magi-changes-route-revert-single");
        fs::write(root.join("alpha.txt"), "original\n").expect("alpha should write");
        let snap = register_workspace_and_snapshot(
            &state,
            "ws-revert-single",
            "sess-revert-single",
            &root,
            None,
        )
        .await;
        fs::write(root.join("alpha.txt"), "modified\n").expect("alpha modify");
        snap.reconcile().expect("reconcile should succeed");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "sess-revert-single",
                            "workspaceId": "ws-revert-single",
                            "filePath": "alpha.txt"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let restored = fs::read_to_string(root.join("alpha.txt")).expect("alpha should read");
        assert_eq!(restored, "original\n");
    }

    #[tokio::test]
    async fn revert_execution_group_rejects_cross_session_mission() {
        let state = build_state();
        let root = unique_temp_dir("magi-changes-route-revert-group");
        fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
        let _snap = register_workspace_and_snapshot(
            &state,
            "ws-revert-group",
            "sess-revert-group",
            &root,
            Some("mission-a"),
        )
        .await;

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert-execution-group")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "sess-revert-group",
                            "workspaceId": "ws-revert-group",
                            "executionGroupId": "mission-b"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("payload should deserialize");
        let message = payload["message"].as_str().unwrap_or_default();
        assert!(message.contains("不属于当前会话"));
    }

    #[tokio::test]
    async fn get_file_content_accepts_absolute_path_within_workspace() {
        let root = unique_temp_dir("magi-changes-route-content-inside");
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha should write");
        let absolute_path = root.join("alpha.txt").to_string_lossy().into_owned();
        let state = build_state_with_workspace_root(&root, "workspace-absolute-content");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/content?workspaceId=workspace-absolute-content&filePath={}",
                        absolute_path
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "absolute path should be accepted"
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("payload should deserialize");
        assert_eq!(payload["content"], "alpha changed\n");
    }

    #[tokio::test]
    async fn get_file_content_resolves_workspace_from_registered_path_when_query_id_is_stale() {
        let root = unique_temp_dir("magi-changes-route-content-path-binding");
        fs::write(root.join("alpha.txt"), "alpha from bound path\n").expect("alpha should write");
        let state = build_state_with_workspace_root(&root, "workspace-content-path");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/content?workspaceId=workspace-stale-query&workspacePath={}&filePath=alpha.txt",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json_response(response).await;
        assert_eq!(payload["workspaceId"], "workspace-content-path");
        assert_eq!(payload["workspacePath"], root.to_string_lossy().as_ref());
        assert_eq!(payload["content"], "alpha from bound path\n");
    }

    #[tokio::test]
    async fn get_file_content_rejects_absolute_path_outside_workspace() {
        let root = unique_temp_dir("magi-changes-route-content-inside-2");
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha should write");
        let outside_dir = unique_temp_dir("magi-changes-route-content-outside");
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, "off-limits\n").expect("outside file should write");
        let outside_path = outside_file.to_string_lossy().into_owned();
        let state = build_state_with_workspace_root(&root, "workspace-outside-content");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/content?workspaceId=workspace-outside-content&filePath={}",
                        outside_path
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("payload should deserialize");
        let message = payload["message"].as_str().unwrap_or_default();
        assert!(message.contains("路径越出工作区边界"));
    }

    #[tokio::test]
    async fn get_file_content_rejects_missing_workspace_without_session() {
        let root = unique_temp_dir("magi-changes-route-content-missing-workspace");
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha should write");
        let state = build_state_with_workspace_root(&root, "workspace-content-missing-scope");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/files/content?filePath=alpha.txt")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = read_json_response(response).await;
        let message = payload["message"].as_str().unwrap_or_default();
        assert!(message.contains("workspaceId 不能为空"));
    }

    #[tokio::test]
    async fn get_file_content_uses_public_unreadable_file_error() {
        let root = unique_temp_dir("magi-changes-route-content-binary");
        fs::write(root.join("binary.bin"), [0xff, 0xfe, 0xfd]).expect("binary should write");
        let state = build_state_with_workspace_root(&root, "workspace-content-binary");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/files/content?workspaceId=workspace-content-binary&filePath=binary.bin")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = read_json_response(response).await;
        assert_eq!(payload["message"], "文件不可读取或不存在");
        let text = payload.to_string();
        assert!(
            !text.contains("invalid utf-8") && !text.contains("stream did not contain valid UTF-8"),
            "file read detail should stay out of response: {text}"
        );
    }

    #[tokio::test]
    async fn get_file_raw_requires_workspace_and_serves_workspace_image() {
        let root = unique_temp_dir("magi-changes-route-raw-image");
        fs::write(root.join("image.png"), b"not-a-real-png").expect("image should write");
        let state = build_state_with_workspace_root(&root, "workspace-raw-image");

        let missing_scope = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/files/raw?filePath=image.png")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");
        assert_eq!(missing_scope.status(), StatusCode::BAD_REQUEST);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/files/raw?workspaceId=workspace-raw-image&filePath=image.png")
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("image/png"))
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        assert_eq!(&body[..], b"not-a-real-png");
    }

    #[tokio::test]
    async fn get_file_raw_resolves_workspace_from_registered_path_when_query_id_is_stale() {
        let root = unique_temp_dir("magi-changes-route-raw-path-image");
        fs::write(root.join("image.png"), b"not-a-real-png").expect("image should write");
        let state = build_state_with_workspace_root(&root, "workspace-raw-path-image");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/files/raw?workspaceId=workspace-stale-query&workspacePath={}&filePath=image.png",
                        urlencoding::encode(root.to_string_lossy().as_ref())
                    ))
                    .method("GET")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("image/png"))
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        assert_eq!(&body[..], b"not-a-real-png");
    }
}
