use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use magi_core::WorkspaceId;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use magi_bridge_client::ModelInvocationRequest;

use crate::{errors::ApiError, state::ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/changes/diff", get(get_diff))
        .route("/changes/approve", post(approve_change))
        .route("/changes/revert", post(revert_change))
        .route("/changes/approve-all", post(approve_all_changes))
        .route("/changes/revert-all", post(revert_all_changes))
        .route("/changes/revert-mission", post(revert_mission_changes))
        .route("/files/content", get(get_file_content))
        .route("/filesystem/list", get(list_filesystem))
        .route("/tunnel/start", post(start_tunnel))
        .route("/tunnel/stop", post(stop_tunnel))
        .route("/tunnel/status", get(tunnel_status))
        .route("/lan-access", get(lan_access_status))
        .route("/prompt/enhance", post(enhance_prompt))
}

fn resolve_workspace_root(
    state: &ApiState,
    workspace_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    let ws_id = match workspace_id.filter(|s| !s.is_empty()) {
        Some(id) => WorkspaceId::new(id),
        None => state
            .workspace_registry
            .active_workspace_id()
            .ok_or_else(|| {
                ApiError::InvalidInput("未指定 workspace_id 且没有活动 workspace".to_string())
            })?,
    };
    let workspaces = state.workspace_registry.workspaces();
    let workspace = workspaces
        .iter()
        .find(|w| w.workspace_id == ws_id)
        .ok_or_else(|| ApiError::not_found("workspace 不存在", ws_id.as_str()))?;
    Ok(PathBuf::from(workspace.root_path.as_str()))
}

fn safe_relative_path(file_path: &str) -> Result<&str, ApiError> {
    let path = Path::new(file_path);
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(ApiError::InvalidInput(
                "路径不允许包含 ..".to_string(),
            ));
        }
        if matches!(component, std::path::Component::RootDir) {
            return Err(ApiError::InvalidInput(
                "路径不允许为绝对路径".to_string(),
            ));
        }
    }
    Ok(file_path)
}

fn run_git(workspace_root: &Path, args: &[&str]) -> Result<String, ApiError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ApiError::internal_assembly("执行 git 命令失败", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(ApiError::internal_assembly("git 命令执行出错", stderr))
    }
}

// ─── Changes ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffQuery {
    file_path: Option<String>,
    #[allow(dead_code)]
    session_id: Option<String>,
    workspace_id: Option<String>,
}

async fn get_diff(
    State(state): State<ApiState>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = resolve_workspace_root(&state, query.workspace_id.as_deref())?;
    let diff = match query.file_path.as_deref() {
        Some(fp) => {
            let rel = safe_relative_path(fp)?;
            run_git(&root, &["diff", "HEAD", "--", rel])?
        }
        None => run_git(&root, &["diff", "HEAD"])?,
    };
    Ok(Json(serde_json::json!({
        "diff": diff,
        "filePath": query.file_path,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveChangeRequest {
    file_path: String,
    workspace_id: Option<String>,
}

async fn approve_change(
    State(state): State<ApiState>,
    Json(request): Json<ApproveChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = resolve_workspace_root(&state, request.workspace_id.as_deref())?;
    let rel = safe_relative_path(&request.file_path)?;
    run_git(&root, &["add", "--", rel])?;
    Ok(Json(serde_json::json!({
        "approved": true,
        "filePath": request.file_path,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertChangeRequest {
    file_path: String,
    workspace_id: Option<String>,
}

async fn revert_change(
    State(state): State<ApiState>,
    Json(request): Json<RevertChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = resolve_workspace_root(&state, request.workspace_id.as_deref())?;
    let rel = safe_relative_path(&request.file_path)?;
    run_git(&root, &["restore", "--source=HEAD", "--staged", "--worktree", "--", rel])?;
    Ok(Json(serde_json::json!({
        "reverted": true,
        "filePath": request.file_path,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveAllRequest {
    workspace_id: Option<String>,
}

async fn approve_all_changes(
    State(state): State<ApiState>,
    Json(request): Json<ApproveAllRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = resolve_workspace_root(&state, request.workspace_id.as_deref())?;
    run_git(&root, &["add", "-A"])?;
    Ok(Json(serde_json::json!({ "approved": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertAllRequest {
    workspace_id: Option<String>,
}

async fn revert_all_changes(
    State(state): State<ApiState>,
    Json(request): Json<RevertAllRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = resolve_workspace_root(&state, request.workspace_id.as_deref())?;
    run_git(&root, &["restore", "--source=HEAD", "--staged", "--worktree", "--", "."])?;
    Ok(Json(serde_json::json!({ "reverted": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertMissionRequest {
    mission_id: String,
    workspace_id: Option<String>,
}

async fn revert_mission_changes(
    State(state): State<ApiState>,
    Json(request): Json<RevertMissionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = resolve_workspace_root(&state, request.workspace_id.as_deref())?;

    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("revert mission", "task_store 未配置"))?;
    let mission_id = magi_core::MissionId::new(&request.mission_id);
    let tasks = task_store.get_tasks_by_mission(&mission_id);

    let mut reverted_files: Vec<String> = Vec::new();
    for task in &tasks {
        for output_ref in &task.output_refs {
            if let Some(rel) = output_ref.strip_prefix("file:") {
                if safe_relative_path(rel).is_ok() {
                    let _ = run_git(&root, &["restore", "--source=HEAD", "--staged", "--worktree", "--", rel]);
                    reverted_files.push(rel.to_string());
                }
            }
        }
    }

    if reverted_files.is_empty() {
        run_git(&root, &["restore", "--source=HEAD", "--staged", "--worktree", "--", "."])?;
    }

    Ok(Json(serde_json::json!({
        "reverted": true,
        "missionId": request.mission_id,
        "revertedFiles": reverted_files,
    })))
}

// ─── Files ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileContentQuery {
    file_path: Option<String>,
    session_id: Option<String>,
    workspace_id: Option<String>,
}

async fn get_file_content(
    State(state): State<ApiState>,
    Query(query): Query<FileContentQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let content = if let Some(ref path) = query.file_path {
        let root = resolve_workspace_root(&state, query.workspace_id.as_deref())?;
        let rel = safe_relative_path(path)?;
        let file_path = root.join(rel);
        std::fs::read_to_string(&file_path)
            .map_err(|e| ApiError::internal_assembly("读取文件内容失败", e))?
    } else {
        String::new()
    };
    Ok(Json(serde_json::json!({
        "content": content,
        "filePath": query.file_path,
        "sessionId": query.session_id,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesystemListQuery {
    path: Option<String>,
    workspace_id: Option<String>,
}

async fn list_filesystem(
    State(state): State<ApiState>,
    Query(query): Query<FilesystemListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = match query.path.as_deref().filter(|value| !value.trim().is_empty()) {
        Some(p) => {
            let p_path = Path::new(p);
            if p_path.is_absolute() {
                p_path.to_path_buf()
            } else {
                let root = resolve_workspace_root(&state, query.workspace_id.as_deref())?;
                root.join(safe_relative_path(p)?)
            }
        }
        None => {
            if let Ok(root) = resolve_workspace_root(&state, query.workspace_id.as_deref()) {
                root
            } else {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
                PathBuf::from(home)
            }
        }
    };
    let entries: Vec<serde_json::Value> = std::fs::read_dir(path)
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .map(|e| {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    serde_json::json!({
                        "name": e.file_name().to_string_lossy(),
                        "path": e.path().to_string_lossy(),
                        "isDirectory": is_dir,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(serde_json::json!({ "entries": entries })))
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
    Ok(Json(serde_json::to_value(&tunnel_state).unwrap_or_default()))
}

async fn stop_tunnel(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tunnel_state = state.tunnel_manager.stop().await;
    Ok(Json(serde_json::to_value(&tunnel_state).unwrap_or_default()))
}

async fn tunnel_status(
    State(state): State<ApiState>,
) -> Json<serde_json::Value> {
    let tunnel_state = state.tunnel_manager.get_state().await;
    Json(serde_json::to_value(&tunnel_state).unwrap_or_default())
}

async fn lan_access_status(
    State(_state): State<ApiState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let ip = resolve_preferred_lan_ipv4();
    let port = 38123;

    // 从 query 参数或 state 获取 workspaceId/sessionId，构造完整的 web 访问 URL
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
    candidates.into_iter().next().map(|(ip, _)| ip).unwrap_or_else(fallback_udp_ip)
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
    if iface_lower.starts_with("en") || iface_lower.starts_with("eth") || iface_lower.starts_with("wlan") || iface_lower.contains("wi-fi") {
        score += 50;
    }
    // 排除虚拟网卡
    if iface_lower.starts_with("bridge") || iface_lower.starts_with("docker") || iface_lower.starts_with("veth")
        || iface_lower.starts_with("utun") || iface_lower.starts_with("tun") || iface_lower.starts_with("tap")
        || iface_lower.starts_with("vmnet") || iface_lower.starts_with("lo") {
        score -= 100;
    }
    // 优先常见私网段
    if addr.starts_with("192.168.") { score += 30; }
    else if addr.starts_with("10.") { score += 20; }
    else if addr.starts_with("172.") { score += 10; } // 简化判断
    else { score -= 20; }
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
}

async fn enhance_prompt(
    State(state): State<ApiState>,
    Json(request): Json<EnhancePromptRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(client) = state.model_bridge_client() else {
        return Err(ApiError::InvalidInput(
            "当前未配置可用模型，无法增强提示词".to_string(),
        ));
    };

    let invocation = ModelInvocationRequest {
        provider: "default".to_string(),
        prompt: format!(
            "请优化以下用户 prompt，使其更清晰、具体、可执行。只输出优化后的 prompt，不要添加额外解释。\n\n原始 prompt:\n{}",
            request.prompt
        ),
        messages: None,
        tools: None,
    };

    match client.invoke(invocation) {
        Ok(response) if response.ok => Ok(Json(serde_json::json!({
            "enhancedPrompt": response.payload.trim(),
        }))),
        Ok(response) => Err(ApiError::InvalidInput(
            response.payload.trim().to_string(),
        )),
        Err(error) => Err(ApiError::InvalidInput(format!(
            "增强提示词失败: {error}"
        ))),
    }
}
