use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use magi_bridge_client::ModelInvocationRequest;
use magi_conversation_runtime::session_turn_execution::BUSINESS_MODEL_PROVIDER;
use magi_conversation_runtime::task_execution_dispatcher::{RoleTarget, resolve_target_for_role};
use magi_core::SessionId;
use magi_snapshot::SnapshotSession;

use super::session_scope::parse_session_id;
use crate::{
    change_projection::{
        resolve_session_change_scope, resolve_workspace_root_or_active, safe_relative_path,
        safe_workspace_path,
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
        .route("/filesystem/list", get(list_filesystem))
        .route("/tunnel/start", post(start_tunnel))
        .route("/tunnel/stop", post(stop_tunnel))
        .route("/tunnel/status", get(tunnel_status))
        .route("/lan-access", get(lan_access_status))
        .route("/prompt/enhance", post(enhance_prompt))
}

async fn require_snapshot_session(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<Arc<SnapshotSession>, ApiError> {
    state.snapshot_session(session_id).ok_or_else(|| {
        ApiError::InvalidInput(format!("会话 {} 的快照账本尚未就绪", session_id.as_str()))
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
    let diff = match query
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
            let snapshot = require_snapshot_session(&state, &session_id).await?;
            let pending = snapshot
                .pending_changes()
                .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
            match query.file_path.as_deref() {
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
            }
        }
        None => {
            // 无 session 调用：仅做一次 workspace 校验，统一返回空 diff，
            // 不再读 git 来伪装出全局变更。
            let _ = resolve_workspace_root_or_active(&state, query.workspace_id.as_deref())?;
            String::new()
        }
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
    session_id: Option<String>,
    workspace_id: Option<String>,
}

async fn approve_change(
    State(state): State<ApiState>,
    Json(request): Json<ApproveChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let _scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let rel = safe_relative_path(&request.file_path)?.to_string();
    let snapshot = require_snapshot_session(&state, &session_id).await?;
    snapshot
        .approve(&[rel])
        .map_err(|e| ApiError::internal_assembly("approve 变更失败", e))?;
    Ok(Json(serde_json::json!({
        "approved": true,
        "filePath": request.file_path,
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
    let _scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let rel = safe_relative_path(&request.file_path)?.to_string();
    let snapshot = require_snapshot_session(&state, &session_id).await?;
    snapshot
        .revert(&[rel])
        .map_err(|e| ApiError::internal_assembly("revert 变更失败", e))?;
    Ok(Json(serde_json::json!({
        "reverted": true,
        "filePath": request.file_path,
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
    let _scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let snapshot = require_snapshot_session(&state, &session_id).await?;
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
    let _scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let snapshot = require_snapshot_session(&state, &session_id).await?;
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
    let snapshot = require_snapshot_session(&state, &session_id).await?;
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
    let content = if let Some(ref path) = query.file_path {
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
            let root = resolve_workspace_root_or_active(&state, query.workspace_id.as_deref())?;
            let (absolute, _) = safe_workspace_path(&root, path)?;
            absolute
        };
        std::fs::read_to_string(&absolute_path)
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
    let path = match query
        .path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(p) => {
            let p_path = Path::new(p);
            if p_path.is_absolute() {
                p_path.to_path_buf()
            } else {
                let root = resolve_workspace_root_or_active(&state, query.workspace_id.as_deref())?;
                root.join(safe_relative_path(p)?)
            }
        }
        None => {
            if let Ok(root) =
                resolve_workspace_root_or_active(&state, query.workspace_id.as_deref())
            {
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
}
