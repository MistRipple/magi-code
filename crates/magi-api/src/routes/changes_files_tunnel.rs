use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use magi_bridge_client::{ModelInvocationRequest, SHADOW_MODEL_PROVIDER};

use super::session_scope::parse_session_id;
use crate::{
    change_projection::{
        SessionChangeScope, resolve_session_change_scope, resolve_workspace_root_or_active,
        run_git_add_files, run_git_diff, run_git_restore_files, safe_relative_path,
        session_change_scope_allows_path,
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

fn require_session_scoped_file<'a>(
    scope: &SessionChangeScope,
    file_path: &'a str,
) -> Result<&'a str, ApiError> {
    let rel = safe_relative_path(file_path)?;
    if !session_change_scope_allows_path(scope, rel) {
        return Err(ApiError::InvalidInput(format!(
            "文件 {} 不属于当前会话 {} 的执行变更集合",
            rel, scope.session_id
        )));
    }
    Ok(rel)
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
    let diff = if query
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
        match query.file_path.as_deref() {
            Some(fp) => {
                let rel = require_session_scoped_file(&scope, fp)?;
                run_git_diff(&scope.workspace_root, &["diff", "HEAD", "--", rel])?
            }
            None => {
                let files = scope
                    .allowed_files
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                if files.is_empty() {
                    String::new()
                } else {
                    let mut args = vec!["diff", "HEAD", "--"];
                    args.extend(files);
                    run_git_diff(&scope.workspace_root, &args)?
                }
            }
        }
    } else {
        let root = resolve_workspace_root_or_active(&state, query.workspace_id.as_deref())?;
        match query.file_path.as_deref() {
            Some(fp) => {
                let rel = safe_relative_path(fp)?;
                run_git_diff(&root, &["diff", "HEAD", "--", rel])?
            }
            None => run_git_diff(&root, &["diff", "HEAD"])?,
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
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let rel = require_session_scoped_file(&scope, &request.file_path)?;
    run_git_add_files(&scope.workspace_root, &[rel.to_string()])?;
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
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let rel = require_session_scoped_file(&scope, &request.file_path)?;
    run_git_restore_files(&scope.workspace_root, &[rel.to_string()])?;
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
    let scope =
        resolve_session_change_scope(&state, &session_id, request.workspace_id.as_deref(), None)?;
    let approved_files = scope.allowed_files.iter().cloned().collect::<Vec<_>>();
    run_git_add_files(&scope.workspace_root, &approved_files)?;
    Ok(Json(serde_json::json!({ "approved": true })))
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
    let reverted_files = scope.allowed_files.iter().cloned().collect::<Vec<_>>();
    run_git_restore_files(&scope.workspace_root, &reverted_files)?;
    Ok(Json(serde_json::json!({ "reverted": true })))
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
    let reverted_files = scope.allowed_files.iter().cloned().collect::<Vec<_>>();
    run_git_restore_files(&scope.workspace_root, &reverted_files)?;

    Ok(Json(serde_json::json!({
        "reverted": true,
        "executionGroupId": request.execution_group_id,
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
    execution_group_id: Option<String>,
}

async fn get_file_content(
    State(state): State<ApiState>,
    Query(query): Query<FileContentQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let content = if let Some(ref path) = query.file_path {
        let file_path = if query
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
            let rel = require_session_scoped_file(&scope, path)?;
            scope.workspace_root.join(rel)
        } else {
            let root = resolve_workspace_root_or_active(&state, query.workspace_id.as_deref())?;
            let rel = safe_relative_path(path)?;
            root.join(rel)
        };
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
        provider: SHADOW_MODEL_PROVIDER.to_string(),
        prompt: format!(
            "请优化以下用户 prompt，使其更清晰、具体、可执行。只输出优化后的 prompt，不要添加额外解释。\n\n原始 prompt:\n{}",
            request.prompt
        ),
        messages: None,
        tools: None,
        tool_choice: None,
    };

    match client.invoke(invocation) {
        Ok(response) if response.ok => Ok(Json(serde_json::json!({
            "enhancedPrompt": response.payload.trim(),
        }))),
        Ok(response) => Err(ApiError::InvalidInput(response.payload.trim().to_string())),
        Err(error) => Err(ApiError::InvalidInput(format!("增强提示词失败: {error}"))),
    }
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
        AbsolutePath, ExecutionOwnership, MissionId, SessionId, TaskId, TaskKind, TaskStatus,
        UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{ActiveExecutionTurn, ActiveExecutionTurnItem, SessionStore};
    use magi_workspace::WorkspaceStore;
    use std::fs;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    static TEST_REPO_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn build_test_repo() -> String {
        let unique_suffix = TEST_REPO_COUNTER.fetch_add(1, Ordering::Relaxed);
        let repo_root = std::env::temp_dir().join(format!(
            "magi-changes-route-test-{}-{}-{}",
            std::process::id(),
            UtcMillis::now().0,
            unique_suffix
        ));
        fs::create_dir_all(&repo_root).expect("repo root should create");
        Command::new("git")
            .args(["init"])
            .current_dir(&repo_root)
            .output()
            .expect("git init should run");
        Command::new("git")
            .args(["config", "user.email", "codex@example.com"])
            .current_dir(&repo_root)
            .output()
            .expect("git email config should run");
        Command::new("git")
            .args(["config", "user.name", "Codex"])
            .current_dir(&repo_root)
            .output()
            .expect("git name config should run");
        fs::write(repo_root.join("a.txt"), "alpha\n").expect("a.txt should write");
        fs::write(repo_root.join("b.txt"), "beta\n").expect("b.txt should write");
        Command::new("git")
            .args(["add", "--", "a.txt", "b.txt"])
            .current_dir(&repo_root)
            .output()
            .expect("git add should run");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo_root)
            .output()
            .expect("git commit should run");
        fs::write(repo_root.join("a.txt"), "alpha changed\n").expect("a.txt should update");
        fs::write(repo_root.join("b.txt"), "beta changed\n").expect("b.txt should update");
        repo_root.to_string_lossy().to_string()
    }

    fn build_state_with_repo(repo_root: &str) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let task_store = Arc::new(TaskStore::new());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_task_store(Arc::clone(&task_store));

        let workspace_id = WorkspaceId::new("workspace-session-scope");
        state
            .workspace_registry
            .register(workspace_id.clone(), AbsolutePath::new(repo_root))
            .expect("workspace should register");

        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        state
            .session_store
            .create_session_for_workspace(
                session_a.clone(),
                "会话 A",
                Some(workspace_id.to_string()),
            )
            .expect("session a should create");
        state
            .session_store
            .create_session_for_workspace(
                session_b.clone(),
                "会话 B",
                Some(workspace_id.to_string()),
            )
            .expect("session b should create");

        state.session_store.bind_execution_ownership(
            session_a.clone(),
            ExecutionOwnership {
                session_id: Some(session_a),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(MissionId::new("mission-a")),
                ..ExecutionOwnership::default()
            },
        );
        state.session_store.bind_execution_ownership(
            session_b.clone(),
            ExecutionOwnership {
                session_id: Some(session_b),
                workspace_id: Some(workspace_id),
                mission_id: Some(MissionId::new("mission-b")),
                ..ExecutionOwnership::default()
            },
        );

        task_store.insert_task(magi_core::Task {
            task_id: TaskId::new("task-a"),
            mission_id: MissionId::new("mission-a"),
            root_task_id: TaskId::new("task-a"),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "A".to_string(),
            goal: "A".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: vec!["file:a.txt".to_string()],
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        });
        task_store.insert_task(magi_core::Task {
            task_id: TaskId::new("task-b"),
            mission_id: MissionId::new("mission-b"),
            root_task_id: TaskId::new("task-b"),
            parent_task_id: None,
            kind: TaskKind::Action,
            title: "B".to_string(),
            goal: "B".to_string(),
            status: TaskStatus::Completed,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: vec!["file:b.txt".to_string()],
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        });

        state
    }

    fn build_state_with_workspace_root(root: &str, workspace_id: &str) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        );
        state
            .workspace_registry
            .register(WorkspaceId::new(workspace_id), AbsolutePath::new(root))
            .expect("workspace should register");
        state
    }

    fn build_state_with_plain_session_repo(
        repo_root: &str,
        session_id: SessionId,
        workspace_id: WorkspaceId,
    ) -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::clone(&session_store),
            workspace_store,
            governance,
        )
        .with_task_store(Arc::new(TaskStore::new()));
        state
            .workspace_registry
            .register(workspace_id.clone(), AbsolutePath::new(repo_root))
            .expect("workspace should register");
        state
            .session_store
            .create_session_for_workspace(
                session_id,
                "普通文件工具会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state
    }

    fn upsert_plain_session_file_tool_turn(
        state: &ApiState,
        session_id: &SessionId,
        tool_name: &str,
        call_id: &str,
        arguments: serde_json::Value,
        result: serde_json::Value,
    ) {
        let now = UtcMillis::now();
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: format!("turn-{call_id}"),
                    turn_seq: now.0,
                    accepted_at: now,
                    completed_at: Some(now),
                    status: "completed".to_string(),
                    user_message: Some("文件工具".to_string()),
                    items: vec![ActiveExecutionTurnItem {
                        item_id: format!("turn-item-tool-{call_id}"),
                        item_seq: 1,
                        lane_id: None,
                        lane_seq: None,
                        kind: "tool_call_result".to_string(),
                        status: "completed".to_string(),
                        source: "session".to_string(),
                        title: Some(tool_name.to_string()),
                        content: None,
                        task_id: None,
                        worker_id: None,
                        role_id: None,
                        tool_call_id: Some(call_id.to_string()),
                        tool_name: Some(tool_name.to_string()),
                        tool_status: Some("completed".to_string()),
                        tool_arguments: Some(arguments.to_string()),
                        tool_result: Some(result.to_string()),
                        tool_error: None,
                        request_id: None,
                        user_message_id: None,
                        placeholder_message_id: None,
                        timeline_entry_id: None,
                        thread_visible: true,
                        worker_visible: false,
                    }],
                    worker_lanes: Vec::new(),
                },
            )
            .expect("canonical file tool turn should upsert");
    }

    #[tokio::test]
    async fn lan_access_uses_current_daemon_port() {
        let state =
            build_state_with_workspace_root("/tmp", "workspace-lan-access").with_tunnel_port(39219);

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
    async fn get_diff_returns_empty_for_non_git_workspace() {
        let unique_suffix = TEST_REPO_COUNTER.fetch_add(1, Ordering::Relaxed);
        let workspace_root = std::env::temp_dir().join(format!(
            "magi-changes-non-git-workspace-{}-{}-{}",
            std::process::id(),
            UtcMillis::now().0,
            unique_suffix
        ));
        fs::create_dir_all(&workspace_root).expect("workspace root should create");
        fs::write(workspace_root.join("notes.txt"), "not under git\n")
            .expect("workspace file should write");
        let state = build_state_with_workspace_root(
            workspace_root.to_string_lossy().as_ref(),
            "workspace-non-git-diff",
        );

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/diff?workspaceId=workspace-non-git-diff")
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
    async fn approve_all_changes_is_limited_to_current_session_files() {
        let repo_root = build_test_repo();
        let state = build_state_with_repo(&repo_root);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/approve-all")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-a",
                            "workspaceId": "workspace-session-scope"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);

        let staged = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&repo_root)
            .output()
            .expect("git diff --cached should run");
        let staged_paths = String::from_utf8_lossy(&staged.stdout);
        assert!(staged_paths.contains("a.txt"));
        assert!(!staged_paths.contains("b.txt"));
    }

    #[tokio::test]
    async fn approve_all_changes_clears_pending_changes_for_current_session() {
        let repo_root = build_test_repo();
        let state = build_state_with_repo(&repo_root);

        let before = collect_session_pending_changes(
            &state,
            &SessionId::new("session-a"),
            Some("workspace-session-scope"),
        )
        .expect("pending changes should collect before approval");
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].file_path, "a.txt");

        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/changes/approve-all")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-a",
                            "workspaceId": "workspace-session-scope"
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
            &SessionId::new("session-a"),
            Some("workspace-session-scope"),
        )
        .expect("pending changes should collect after approval");
        assert!(
            after.is_empty(),
            "approved files should disappear from pending changes"
        );
    }

    #[tokio::test]
    async fn revert_all_changes_removes_plain_session_canonical_file_change() {
        let repo_root = build_test_repo();
        let session_id = SessionId::new("session-plain-canonical-revert");
        let workspace_id = WorkspaceId::new("workspace-plain-canonical-revert");
        let state = build_state_with_plain_session_repo(
            &repo_root,
            session_id.clone(),
            workspace_id.clone(),
        );
        fs::create_dir_all(Path::new(&repo_root).join("tmp")).expect("tmp dir should create");
        fs::write(
            Path::new(&repo_root).join("tmp/plain-session-new.txt"),
            "new file\n",
        )
        .expect("untracked file should write");
        upsert_plain_session_file_tool_turn(
            &state,
            &session_id,
            "file_write",
            "call-plain-session-write",
            serde_json::json!({
                "path": Path::new(&repo_root).join("tmp/plain-session-new.txt").to_string_lossy().to_string(),
                "content": "new file\n",
            }),
            serde_json::json!({
                "tool": "file_write",
                "status": "succeeded",
                "path": Path::new(&repo_root).join("tmp/plain-session-new.txt").to_string_lossy().to_string(),
            }),
        );

        let before =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("plain session canonical file change should collect");
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].file_path, "tmp/plain-session-new.txt");

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert-all")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": session_id.as_str(),
                            "workspaceId": workspace_id.as_str()
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            !Path::new(&repo_root)
                .join("tmp/plain-session-new.txt")
                .exists()
        );
    }

    #[tokio::test]
    async fn revert_execution_group_rejects_cross_session_mission() {
        let repo_root = build_test_repo();
        let state = build_state_with_repo(&repo_root);

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert-execution-group")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-a",
                            "workspaceId": "workspace-session-scope",
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
    async fn revert_change_removes_untracked_session_file() {
        let repo_root = build_test_repo();
        let state = build_state_with_repo(&repo_root);
        fs::create_dir_all(Path::new(&repo_root).join("tmp")).expect("tmp dir should create");
        fs::write(
            Path::new(&repo_root).join("tmp/new-single-a.txt"),
            "new file\n",
        )
        .expect("untracked file should write");

        state
            .task_store()
            .expect("task store should exist")
            .insert_task(magi_core::Task {
                task_id: TaskId::new("task-a-untracked-single"),
                mission_id: MissionId::new("mission-a"),
                root_task_id: TaskId::new("task-a-untracked-single"),
                parent_task_id: None,
                kind: TaskKind::Action,
                title: "A-untracked-single".to_string(),
                goal: "A-untracked-single".to_string(),
                status: TaskStatus::Completed,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: vec!["file:tmp/new-single-a.txt".to_string()],
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            });

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-a",
                            "workspaceId": "workspace-session-scope",
                            "filePath": "tmp/new-single-a.txt"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(!Path::new(&repo_root).join("tmp/new-single-a.txt").exists());
    }

    #[tokio::test]
    async fn revert_all_changes_removes_untracked_session_file() {
        let repo_root = build_test_repo();
        let state = build_state_with_repo(&repo_root);
        fs::create_dir_all(Path::new(&repo_root).join("tmp")).expect("tmp dir should create");
        fs::write(Path::new(&repo_root).join("tmp/new-a.txt"), "new file\n")
            .expect("untracked file should write");

        state
            .task_store()
            .expect("task store should exist")
            .insert_task(magi_core::Task {
                task_id: TaskId::new("task-a-untracked"),
                mission_id: MissionId::new("mission-a"),
                root_task_id: TaskId::new("task-a-untracked"),
                parent_task_id: None,
                kind: TaskKind::Action,
                title: "A-untracked".to_string(),
                goal: "A-untracked".to_string(),
                status: TaskStatus::Completed,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                context_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: vec!["file:tmp/new-a.txt".to_string()],
                evidence_refs: Vec::new(),
                retry_count: 0,
                repair_count: 0,
                decision_payload: None,
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            });

        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/changes/revert-all")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "session-a",
                            "workspaceId": "workspace-session-scope"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(!Path::new(&repo_root).join("tmp/new-a.txt").exists());
    }
}
