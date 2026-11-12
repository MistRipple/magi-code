use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::scope_binding::require_registered_workspace_binding;
use crate::session_activity::session_running_task_count;
use crate::{errors::ApiError, state::ApiState};

mod project_management;

static WORKSPACE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces/register", post(register_workspace))
        .route(
            "/workspaces/create",
            post(project_management::create_project),
        )
        .route(
            "/workspaces/update",
            post(project_management::update_project),
        )
        .route("/workspaces/remove", post(remove_workspace))
        .route("/workspaces/pick", get(pick_workspace))
        .route("/workspaces/sessions", get(workspace_sessions))
        .route("/sessions/personal", get(personal_sessions))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceDto {
    workspace_id: String,
    root_path: String,
    root_path_ref: String,
    name: Option<String>,
    is_active: bool,
    revision: u64,
    primary_source_folder_id: String,
    source_folders: Vec<WorkspaceSourceFolderDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSourceFolderDto {
    source_folder_id: String,
    path: String,
    path_ref: String,
    name: Option<String>,
    is_primary: bool,
}

fn workspace_dto(state: &ApiState, workspace: &magi_workspace::WorkspaceRecord) -> WorkspaceDto {
    let active_id = state.workspace_registry.active_workspace_id();
    WorkspaceDto {
        workspace_id: workspace.workspace_id.to_string(),
        root_path: workspace.primary_source_folder().path.to_string(),
        root_path_ref: workspace.primary_source_folder().path_ref.clone(),
        name: workspace.name.clone(),
        is_active: active_id.as_ref() == Some(&workspace.workspace_id),
        revision: workspace.revision,
        primary_source_folder_id: workspace.primary_source_folder_id.clone(),
        source_folders: workspace
            .source_folders
            .iter()
            .map(|folder| WorkspaceSourceFolderDto {
                source_folder_id: folder.source_folder_id.clone(),
                path: folder.path.to_string(),
                path_ref: folder.path_ref.clone(),
                name: folder.name.clone(),
                is_primary: workspace.primary_source_folder_id == folder.source_folder_id,
            })
            .collect(),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceListResponse {
    workspaces: Vec<WorkspaceDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSessionDto {
    session_id: String,
    workspace_id: Option<String>,
    title: String,
    status: String,
    created_at: u64,
    updated_at: u64,
    message_count: usize,
    is_running: bool,
    running_task_count: usize,
    has_unread_completion: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSessionsResponse {
    runtime_epoch: String,
    event_stream_next_sequence: u64,
    workspace: WorkspaceDto,
    sessions: Vec<WorkspaceSessionDto>,
    unavailable_sessions: Vec<UnavailableSessionDto>,
}

/// 启动时因持久化文件损坏而无法加载的会话；原文件保留在 `sourcePath`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnavailableSessionDto {
    session_id: String,
    source_path: String,
    reason: String,
}

fn unavailable_session_entries(
    state: &ApiState,
    workspace_id: Option<&str>,
) -> Vec<UnavailableSessionDto> {
    state
        .session_store
        .unavailable_sessions(workspace_id)
        .into_iter()
        .map(|session| UnavailableSessionDto {
            session_id: session.session_id.to_string(),
            source_path: session.source_path,
            reason: session.reason,
        })
        .collect()
}

/// 个人会话目录与项目会话目录使用同一读模型，但没有 workspace 绑定。
/// 这是独立端点，避免客户端把空 workspaceId 当作“所有项目”的模糊筛选。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonalSessionsResponse {
    runtime_epoch: String,
    event_stream_next_sequence: u64,
    sessions: Vec<WorkspaceSessionDto>,
    unavailable_sessions: Vec<UnavailableSessionDto>,
}

fn session_directory_entry(
    state: &ApiState,
    session: &magi_session_store::SessionRecord,
    workspace_id: Option<&str>,
) -> WorkspaceSessionDto {
    let sidecar = state.session_store.runtime_sidecar(&session.session_id);
    let running_task_count = session_running_task_count(sidecar.as_ref());
    WorkspaceSessionDto {
        session_id: session.session_id.to_string(),
        workspace_id: workspace_id.map(str::to_string),
        title: session.title.clone(),
        status: format!("{:?}", session.status),
        created_at: session.created_at.0,
        updated_at: session.updated_at.0,
        message_count: session.message_count.unwrap_or(0),
        is_running: running_task_count > 0,
        running_task_count,
        has_unread_completion: session.has_unread_completion(),
    }
}

async fn personal_sessions(State(state): State<ApiState>) -> Json<PersonalSessionsResponse> {
    let event_stream_next_sequence = state.event_bus.snapshot().next_sequence;
    let sessions = state
        .session_records_for_workspace(None)
        .iter()
        .map(|session| session_directory_entry(&state, session, None))
        .collect();
    Json(PersonalSessionsResponse {
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence,
        sessions,
        unavailable_sessions: unavailable_session_entries(&state, None),
    })
}

async fn list_workspaces(State(state): State<ApiState>) -> Json<WorkspaceListResponse> {
    let workspaces = state
        .workspace_registry
        .workspaces()
        .into_iter()
        .map(|w| workspace_dto(&state, &w))
        .collect();
    Json(WorkspaceListResponse { workspaces })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterWorkspaceRequest {
    path: String,
}

async fn register_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RegisterWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let canonical_path = canonical_workspace_path(&request.path)?;
    let _navigation_guard = state.lock_session_navigation().await;

    if let Some(state_root) = state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        && magi_runtime_state::state_roots_overlap(state_root, &canonical_path)
            .map_err(|error| ApiError::internal_assembly("校验 workspace 状态边界失败", error))?
    {
        return Err(ApiError::InvalidInput(
            "工作区路径不能包含 Magi 全局状态目录".to_string(),
        ));
    }

    // 已注册过的 workspace：复用已有记录，仍异步刷新索引。
    if let Some(workspace) = registered_workspace_for_path(&state, &canonical_path) {
        if workspace.source_folders.len() != 1 {
            return Err(ApiError::Conflict(
                "该文件夹已属于多目录项目，请按项目身份打开".into(),
            ));
        }
        state.restore_workspace_history_for_api(&workspace)?;
        let workspace_id = workspace.workspace_id.clone();
        state.schedule_workspace_code_index(workspace_id, canonical_path.clone());
        return Ok(Json(serde_json::json!({
            "workspaceId": workspace.workspace_id.to_string(),
            "registered": false,
            "reused": true
        })));
    }

    // 新 workspace：先同步完成注册（快），再异步构建索引。
    let workspace_id =
        magi_workspace::resolve_or_create_workspace_identity(&canonical_path, new_workspace_id())
            .map_err(|error| ApiError::internal_assembly("工作区身份初始化失败", error))?;
    match state
        .workspace_registry
        .register_native_path_with_persistence(
            workspace_id.clone(),
            canonical_path.clone(),
            |snapshot| state.persist_workspace_snapshot(snapshot),
        ) {
        Ok(project) => {
            state.restore_workspace_history_for_api(&project)?;
        }
        Err(error) => {
            // 并发竞态：另一个请求先注册了同一路径。
            if let Some(workspace) = registered_workspace_for_path(&state, &canonical_path) {
                let wid = workspace.workspace_id.clone();
                state.schedule_workspace_code_index(wid, canonical_path.clone());
                return Ok(Json(serde_json::json!({
                    "workspaceId": workspace.workspace_id.to_string(),
                    "registered": false,
                    "reused": true
                })));
            }
            return Err(ApiError::internal_assembly("工作区注册失败", error));
        }
    }

    // 注册事务已先完成持久提交；索引放到后台不阻塞响应。
    publish_workspace_registry_changed(&state);
    state.schedule_workspace_code_index(workspace_id.clone(), canonical_path.clone());
    Ok(Json(serde_json::json!({
        "workspaceId": workspace_id.to_string(),
        "registered": true
    })))
}

fn publish_workspace_registry_changed(state: &ApiState) {
    state
        .event_bus
        .publish(magi_event_bus::EventEnvelope::domain(
            magi_core::EventId::unique("workspace-registry-changed"),
            "workspace.registry.changed",
            serde_json::json!({}),
        ));
}

fn new_workspace_id() -> magi_core::WorkspaceId {
    let nonce = WORKSPACE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    magi_core::WorkspaceId::new(format!(
        "workspace-{}-{nonce}",
        magi_core::UtcMillis::now().0
    ))
}

pub(crate) fn canonical_workspace_path(raw_path: &str) -> Result<PathBuf, ApiError> {
    let trimmed_path = raw_path.trim();
    if trimmed_path.is_empty() {
        return Err(ApiError::InvalidInput("工作区路径不能为空".to_string()));
    }
    let path = magi_core::HostPath::resolve_native_input(
        trimmed_path,
        std::env::current_dir().ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
    .map(magi_core::HostPath::into_path_buf)
    .map_err(|_| ApiError::InvalidInput("工作区路径不可访问".to_string()))?;
    let canonical_path = magi_core::HostPath::canonicalize(&path)
        .map(magi_core::HostPath::into_path_buf)
        .map_err(|_| ApiError::InvalidInput("工作区路径不可访问".to_string()))?;
    if !canonical_path.is_dir() {
        return Err(ApiError::InvalidInput("工作区路径必须是目录".to_string()));
    }
    Ok(canonical_path)
}

pub(crate) fn registered_workspace_for_path(
    state: &ApiState,
    canonical_path: &Path,
) -> Option<magi_workspace::WorkspaceRecord> {
    state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace_root_matches(workspace, canonical_path))
}

fn workspace_root_matches(
    workspace: &magi_workspace::WorkspaceRecord,
    canonical_path: &Path,
) -> bool {
    workspace
        .source_folder_roots()
        .into_iter()
        .any(|stored_path| {
            magi_core::HostPath::canonicalize(&stored_path)
                .map(magi_core::HostPath::into_path_buf)
                .map(|path| path == canonical_path)
                .unwrap_or_else(|_| stored_path == canonical_path)
        })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveWorkspaceRequest {
    workspace_id: String,
}

async fn remove_workspace(
    State(state): State<ApiState>,
    Json(request): Json<RemoveWorkspaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = magi_core::WorkspaceId::new(request.workspace_id.trim());
    let _project_edit = state.try_project_directory_edit(&workspace_id)?;
    let _navigation_guard = state.lock_session_navigation().await;
    let _project_guards =
        project_management::acquire_project_edit_guards(&state, &workspace_id, false).await?;
    if !state
        .workspace_registry
        .workspaces()
        .iter()
        .any(|workspace| workspace.workspace_id == workspace_id)
    {
        return Err(ApiError::InvalidInput(format!(
            "workspace 不存在: {workspace_id}"
        )));
    }
    // 先撤销外部调用准入，令牌写盘失败时保留工作区注册事实。
    state
        .mcp_service
        .revoke_workspace(&state, workspace_id.as_str())
        .await
        .map_err(|error| ApiError::internal_assembly("吊销工作区 MCP 令牌失败", error))?;
    // 令牌吊销可能更新外部会话的运行 sidecar。必须在工作区仍注册时完成一次
    // checkpoint，再提交 deregister；否则后续维护 flush 无法解析原工作区路径，
    // 会把历史会话的持久化失败当成周期性运行时错误。
    state.persist_session_state_checkpoint("workspace_remove")?;
    if let Some(current) = state
        .session_store
        .current_session()
        .filter(|session| session.workspace_id.as_deref() == Some(workspace_id.as_str()))
    {
        state.session_store.clear_current_session();
        if let Err(error) = state.persist_session_navigation_state_for_api(None) {
            let _ = state
                .session_store
                .select_current_session(&current.session_id);
            return Err(error);
        }
    }
    state
        .workspace_registry
        .deregister_with_persistence(&workspace_id, |snapshot| {
            state.persist_workspace_snapshot(snapshot)
        })
        .map_err(|e| ApiError::internal_assembly("工作区移除失败", e))?;
    publish_workspace_registry_changed(&state);
    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn pick_workspace(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"workspaces": state.workspace_registry.workspaces()
        .iter().map(|workspace| workspace_dto(&state, workspace)).collect::<Vec<_>>() }),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceSessionsQuery {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
}

async fn workspace_sessions(
    State(state): State<ApiState>,
    Query(query): Query<WorkspaceSessionsQuery>,
) -> Result<Json<WorkspaceSessionsResponse>, ApiError> {
    let requested_workspace_id = match query
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|workspace_path| !workspace_path.is_empty())
    {
        Some(_) => {
            require_registered_workspace_binding(
                &state,
                query.workspace_id.as_deref(),
                query.workspace_path.as_deref(),
            )?
            .workspace_id
        }
        None => {
            let workspace_id = query
                .workspace_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))?;
            magi_core::WorkspaceId::new(workspace_id)
        }
    };
    let workspace = state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace.workspace_id == requested_workspace_id)
        .ok_or_else(|| {
            ApiError::InvalidInput(format!("workspace 不存在: {requested_workspace_id}"))
        })?;
    let workspace_dto = workspace_dto(&state, &workspace);
    let scoped_workspace_id = workspace.workspace_id.to_string();
    // 目录游标必须先于 SessionStore 快照读取。所有会话目录变更都先提交
    // SessionStore、再发布事件，因此这个顺序保证：游标一旦包含某个事件，
    // 同一响应里的目录快照也一定包含该事件对应的事实。客户端可据此拒绝
    // 晚到的旧响应，避免旧空列表覆盖刚接受的新会话。
    let event_stream_next_sequence = state.event_bus.snapshot().next_sequence;

    let scoped_sessions = state
        .session_records_for_workspace(Some(scoped_workspace_id.as_str()))
        .iter()
        .map(|session| session_directory_entry(&state, session, Some(&scoped_workspace_id)))
        .collect::<Vec<_>>();

    Ok(Json(WorkspaceSessionsResponse {
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence,
        workspace: workspace_dto,
        sessions: scoped_sessions,
        unavailable_sessions: unavailable_session_entries(&state, Some(&scoped_workspace_id)),
    }))
}

#[cfg(test)]
mod tests;
