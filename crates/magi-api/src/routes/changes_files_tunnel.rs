use axum::{
    Json, Router,
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use magi_bridge_client::ModelInvocationRequest;
use magi_conversation_runtime::session_turn_execution::BUSINESS_MODEL_PROVIDER;
use magi_conversation_runtime::task_execution_dispatcher::{RoleTarget, resolve_target_for_role};
use magi_snapshot::SnapshotSession;

use super::session_scope::{parse_session_id, require_workspace_id};
use crate::{
    change_projection::{
        SessionChangeScope, WorkspaceChangeScope, resolve_session_change_scope,
        resolve_workspace_change_scope_or_active, safe_relative_path, safe_workspace_path,
    },
    errors::ApiError,
    state::ApiState,
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

// ─── Changes ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffQuery {
    file_path: Option<String>,
    session_id: Option<String>,
    workspace_id: Option<String>,
    execution_group_id: Option<String>,
}

async fn get_diff(
    State(state): State<ApiState>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 没有 sessionId 时不再回退到 git diff —— 全局变更视图已不再属于本系统职责。
    let (diff, binding) = match query
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(_) => {
            let session_id = parse_session_id(query.session_id.as_deref())?;
            let scope = resolve_session_change_scope(
                &state,
                &session_id,
                query.workspace_id.as_deref(),
                query.execution_group_id.as_deref(),
            )?;
            let snapshot = require_snapshot_session(&state, &scope).await?;
            let pending = snapshot
                .pending_changes()
                .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
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
            (diff, session_scope_binding(&scope))
        }
        None => {
            // 无 session 调用：仅做一次 workspace 校验，统一返回空 diff，
            // 不再读 git 来伪装出全局变更。
            let scope =
                resolve_workspace_change_scope_or_active(&state, query.workspace_id.as_deref())?;
            (String::new(), workspace_scope_binding(&scope))
        }
    };
    let mut payload = serde_json::json!({
        "diff": diff,
        "filePath": query.file_path,
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
}

async fn approve_change(
    State(state): State<ApiState>,
    Json(request): Json<ApproveChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
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
}

async fn revert_change(
    State(state): State<ApiState>,
    Json(request): Json<RevertChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
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
}

async fn approve_all_changes(
    State(state): State<ApiState>,
    Json(request): Json<ApproveAllRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
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
}

async fn revert_all_changes(
    State(state): State<ApiState>,
    Json(request): Json<RevertAllRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
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
}

async fn revert_execution_group_changes(
    State(state): State<ApiState>,
    Json(request): Json<RevertExecutionGroupRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let scope = resolve_session_change_scope(
        &state,
        &session_id,
        request.workspace_id.as_deref(),
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
        let scope = resolve_session_change_scope(
            &state,
            &session_id,
            query.workspace_id.as_deref(),
            query.execution_group_id.as_deref(),
        )?;
        let (absolute, _relative) = safe_workspace_path(&scope.workspace_root, path)?;
        (absolute, session_scope_binding(&scope))
    } else {
        let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
        let scope = resolve_workspace_change_scope_or_active(&state, Some(workspace_id.as_str()))?;
        let (absolute, _) = safe_workspace_path(&scope.workspace_root, path)?;
        (absolute, workspace_scope_binding(&scope))
    };
    let content = std::fs::read_to_string(&absolute_path)
        .map_err(|e| ApiError::internal_assembly("读取文件内容失败", e))?;
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
        let scope = resolve_session_change_scope(
            &state,
            &session_id,
            query.workspace_id.as_deref(),
            query.execution_group_id.as_deref(),
        )?;
        let (absolute, _relative) = safe_workspace_path(&scope.workspace_root, path)?;
        absolute
    } else {
        let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
        let scope = resolve_workspace_change_scope_or_active(&state, Some(workspace_id.as_str()))?;
        let (absolute, _) = safe_workspace_path(&scope.workspace_root, path)?;
        absolute
    };

    let mime = image_mime_for_path(&absolute_path)
        .ok_or_else(|| ApiError::InvalidInput("仅支持图片文件预览".to_string()))?;

    let bytes = std::fs::read(&absolute_path)
        .map_err(|e| ApiError::internal_assembly("读取文件内容失败", e))?;

    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemListQuery {
    path: Option<String>,
    workspace_id: Option<String>,
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
        .map_err(|e| ApiError::internal_assembly(error_context, e))?;
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
        .map_err(|e| ApiError::internal_assembly("读取目录失败", e))?
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
    let workspace_id = require_workspace_id(query.workspace_id.as_deref())?;
    let scope = resolve_workspace_change_scope_or_active(&state, Some(workspace_id.as_str()))?;
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
}

async fn start_tunnel(
    State(state): State<ApiState>,
    Json(request): Json<StartTunnelRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws_id = request.workspace_id.as_deref();
    let tunnel_state = state.tunnel_manager.start(ws_id).await;
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

    // 从 query 参数获取 workspaceId，构造完整的 web 访问 URL
    let workspace_id = query.get("workspaceId").cloned().unwrap_or_default();
    let mut url = format!("http://{}:{}/web.html", ip, port);
    if !workspace_id.is_empty() {
        url = format!("{}?workspaceId={}", url, workspace_id);
    }

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
            return Err(ApiError::InvalidInput(format!(
                "辅助模型返回失败: {}",
                resp.payload.trim()
            )));
        }
        Err(error) => {
            return Err(ApiError::InvalidInput(format!("辅助模型调用失败: {error}")));
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
    use crate::change_projection::collect_session_pending_changes;
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
                    .uri("/lan-access?workspaceId=workspace-lan-access")
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

        let before = collect_session_pending_changes(
            &state,
            &SessionId::new("sess-approve-all"),
            Some("ws-approve-all"),
        )
        .expect("pending changes should collect before approval");
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

        let after = collect_session_pending_changes(
            &state,
            &SessionId::new("sess-approve-all"),
            Some("ws-approve-all"),
        )
        .expect("pending changes should collect after approval");
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

        let before = collect_session_pending_changes(
            &state,
            &SessionId::new("sess-revert-all"),
            Some("ws-revert-all"),
        )
        .expect("pending changes should collect");
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
}
