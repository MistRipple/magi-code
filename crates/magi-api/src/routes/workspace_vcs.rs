//! 主对话 Git context 与结构化 branch/worktree API。
//!
//! 这里的 Git branch 与 conversation fork、任务执行 branch 完全独立。route 只接受已注册
//! workspace，并把 session 绑定到实际 repository/worktree；写操作还会检查运行中 turn、
//! session context revision 和 Git branch/HEAD/worktree CAS 前置条件。

use axum::{Json, Router, extract::State, routing::post};
use magi_core::{EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_git::{
    BranchCreateOptions, BranchDeleteOptions, BranchSwitchOptions, GitBranch, GitDirtySummary,
    GitError, GitObservation, GitPrecondition, MergeOptions, SessionCodeContext,
    WorktreeCreateOptions, WorktreeRemoveOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::{errors::ApiError, state::ApiState};

#[derive(Clone)]
struct WorkspaceGitScope {
    workspace_id: WorkspaceId,
    path: PathBuf,
}

struct PreparedMutation {
    precondition: GitPrecondition,
    _lease: magi_git::GitMutationLease,
}

fn resolve_registered_workspace(
    state: &ApiState,
    raw_path: &str,
) -> Result<WorkspaceGitScope, ApiError> {
    let path = super::workspaces::canonical_workspace_path(raw_path)?;
    let workspace = super::workspaces::registered_workspace_for_path(state, &path)
        .ok_or_else(|| ApiError::InvalidInput("工作区未注册，无法执行 Git 操作".to_string()))?;
    Ok(WorkspaceGitScope {
        workspace_id: workspace.workspace_id,
        path,
    })
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspace/vcs/status", post(git_status))
        .route("/workspace/vcs/context/accept", post(accept_git_context))
        .route("/workspace/vcs/branches", post(list_branches))
        .route("/workspace/vcs/branch/create", post(branch_create))
        .route("/workspace/vcs/branch/switch", post(branch_switch))
        .route("/workspace/vcs/merge/preview", post(merge_preview))
        .route("/workspace/vcs/merge", post(merge_branch))
        .route("/workspace/vcs/branch/delete", post(branch_delete))
        .route("/workspace/vcs/worktree/list", post(worktree_list))
        .route("/workspace/vcs/worktree/create", post(worktree_create))
        .route("/workspace/vcs/worktree/remove", post(worktree_remove))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestContext {
    workspace_path: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    expected_context_revision: Option<u64>,
    #[serde(flatten)]
    precondition: GitPrecondition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchListRequest {
    #[serde(flatten)]
    context: RequestContext,
    #[serde(default)]
    include_remote: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchCreateRequest {
    #[serde(flatten)]
    context: RequestContext,
    branch: String,
    #[serde(default)]
    start_point: Option<String>,
    /// 产品默认：创建后立即切换；只有显式传 false 才仅创建 ref。
    #[serde(default = "default_true")]
    switch: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchSwitchRequest {
    #[serde(flatten)]
    context: RequestContext,
    branch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeRequest {
    #[serde(flatten)]
    context: RequestContext,
    target: String,
    #[serde(default)]
    ff_only: bool,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchDeleteRequest {
    #[serde(flatten)]
    context: RequestContext,
    branch: String,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    confirm_force: bool,
    #[serde(default)]
    confirm_remote: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeCreateRequest {
    #[serde(flatten)]
    context: RequestContext,
    /// readOnly 创建 detached worktree；writable 创建唯一临时 branch。
    mode: WorktreeMode,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    allocation_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum WorktreeMode {
    ReadOnly,
    Writable,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRemoveRequest {
    #[serde(flatten)]
    context: RequestContext,
    path: PathBuf,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    confirm_force: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceWorktreeResponse {
    #[serde(flatten)]
    worktree: magi_git::GitWorktree,
    managed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusResponse {
    is_repo: bool,
    current_branch: Option<String>,
    head: Option<String>,
    observation: Option<GitObservation>,
    session_context: Option<SessionCodeContext>,
    context_drift: bool,
}

/// 保留原 UI 所需字段，同时返回结构化 branch/observation/session context。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BranchesResponse {
    is_repo: bool,
    current_branch: Option<String>,
    branches: Vec<String>,
    remote_branches: Vec<String>,
    structured_branches: Vec<GitBranch>,
    status: Option<WorkspaceVcsStatus>,
    observation: Option<GitObservation>,
    session_context: Option<SessionCodeContext>,
    context_drift: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceVcsStatus {
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    has_uncommitted: bool,
    staged: u64,
    unstaged: u64,
    untracked: u64,
    conflicted: u64,
    renamed: u64,
    deleted: u64,
    additions: u64,
    deletions: u64,
}

impl WorkspaceVcsStatus {
    fn from_observation(observation: &GitObservation) -> Self {
        Self {
            upstream: observation.upstream.clone(),
            ahead: observation.ahead,
            behind: observation.behind,
            has_uncommitted: observation.dirty.has_uncommitted,
            staged: observation.dirty.staged,
            unstaged: observation.dirty.unstaged,
            untracked: observation.dirty.untracked,
            conflicted: observation.dirty.conflicted,
            renamed: observation.dirty.renamed,
            deleted: observation.dirty.deleted,
            additions: observation.dirty.additions,
            deletions: observation.dirty.deletions,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitOperationResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    observation: Option<GitObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_context: Option<SessionCodeContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<GitOperationErrorDto>,
}

impl GitOperationResponse {
    fn rejected(error: GitError) -> Self {
        Self {
            ok: false,
            observation: None,
            session_context: None,
            data: None,
            error: Some(GitOperationErrorDto::from(error)),
        }
    }

    fn rejected_kind(kind: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            observation: None,
            session_context: None,
            data: None,
            error: Some(GitOperationErrorDto {
                kind: kind.to_string(),
                message: message.into(),
                dirty: None,
                conflicted_paths: Vec::new(),
                actual_branch: None,
                actual_head: None,
                actual_worktree_path: None,
                stdout: None,
                stderr: None,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitOperationErrorDto {
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<GitDirtySummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    conflicted_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_worktree_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
}

impl From<GitError> for GitOperationErrorDto {
    fn from(error: GitError) -> Self {
        let message = error.to_string();
        let mut dto = Self {
            kind: git_error_kind(&error).to_string(),
            message,
            dirty: None,
            conflicted_paths: Vec::new(),
            actual_branch: None,
            actual_head: None,
            actual_worktree_path: None,
            stdout: None,
            stderr: None,
        };
        match error {
            GitError::DirtyWorkspace { dirty, .. } => dto.dirty = Some(dirty),
            GitError::StaleContext {
                actual_branch,
                actual_head,
                actual_worktree_path,
                ..
            } => {
                dto.actual_branch = actual_branch;
                dto.actual_head = actual_head;
                dto.actual_worktree_path = Some(actual_worktree_path);
            }
            GitError::MergeConflict {
                conflicted_paths,
                stdout,
                stderr,
                ..
            } => {
                dto.conflicted_paths = conflicted_paths;
                dto.stdout = non_empty(stdout);
                dto.stderr = non_empty(stderr);
            }
            GitError::CommandFailed { stdout, stderr, .. } => {
                dto.stdout = non_empty(stdout);
                dto.stderr = non_empty(stderr);
            }
            _ => {}
        }
        dto
    }
}

fn git_error_kind(error: &GitError) -> &'static str {
    match error {
        GitError::NotRepository { .. } => "not_repository",
        GitError::InvalidInput { .. } => "invalid_input",
        GitError::DirtyWorkspace { .. } => "dirty_workspace",
        GitError::StaleContext { .. } => "stale_git_context",
        GitError::CurrentBranch { .. } => "current_branch",
        GitError::BranchInUse { .. } => "branch_in_use",
        GitError::ConfirmationRequired { .. } => "confirmation_required",
        GitError::MergeConflict { .. } => "merge_conflict",
        GitError::CommandFailed { .. } => "git_command_failed",
        GitError::Io(_) => "git_io_error",
    }
}

fn default_true() -> bool {
    true
}

async fn git_status(
    State(state): State<ApiState>,
    Json(request): Json<RequestContext>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.workspace_path)?;
    match state.git_service.observe(&scope.path).await {
        Ok(observation) => {
            let session_context = observe_session_context(
                &state,
                &scope,
                request.session_id.as_deref(),
                &observation,
                false,
            )?;
            let context_drift = session_context
                .as_ref()
                .is_some_and(SessionCodeContext::has_external_drift);
            Ok(Json(GitStatusResponse {
                is_repo: true,
                current_branch: observation.branch.clone(),
                head: observation.head.clone(),
                observation: Some(observation),
                session_context,
                context_drift,
            }))
        }
        Err(GitError::NotRepository { .. }) => Ok(Json(GitStatusResponse {
            is_repo: false,
            current_branch: None,
            head: None,
            observation: None,
            session_context: None,
            context_drift: false,
        })),
        Err(error) => Err(git_internal_error("读取 Git 状态", error)),
    }
}

async fn accept_git_context(
    State(state): State<ApiState>,
    Json(request): Json<RequestContext>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.workspace_path)?;
    let Some(session_id) = request.session_id.as_deref() else {
        return Err(ApiError::InvalidInput(
            "接受 Git context 必须提供 sessionId".to_string(),
        ));
    };
    if workspace_has_running_turn(&state, &scope.workspace_id) {
        return Ok(Json(GitOperationResponse::rejected_kind(
            "workspace_execution_active",
            "当前 workspace 有运行中的 turn/worker，不能接受新的 Git 基线",
        )));
    }
    if let Err(error) = state
        .session_code_contexts
        .validate_revision(session_id, request.expected_context_revision)
    {
        return Ok(Json(GitOperationResponse::rejected_kind(
            "stale_context_revision",
            error.to_string(),
        )));
    }
    let observation = match state.git_service.observe(&scope.path).await {
        Ok(observation) => observation,
        Err(error) => return Ok(Json(GitOperationResponse::rejected(error))),
    };
    let session_context =
        observe_session_context(&state, &scope, Some(session_id), &observation, true)?;
    publish_git_context_changed(&state, &scope, Some(session_id), &observation);
    super::knowledge::schedule_workspace_code_index(
        state.clone(),
        scope.workspace_id.clone(),
        scope.path.clone(),
    );
    Ok(Json(GitOperationResponse {
        ok: true,
        observation: Some(observation),
        session_context,
        data: None,
        error: None,
    }))
}

async fn list_branches(
    State(state): State<ApiState>,
    Json(request): Json<BranchListRequest>,
) -> Result<Json<BranchesResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let list = match state
        .git_service
        .branch_list(&scope.path, request.include_remote)
        .await
    {
        Ok(list) => list,
        Err(GitError::NotRepository { .. }) => {
            return Ok(Json(BranchesResponse {
                is_repo: false,
                current_branch: None,
                branches: Vec::new(),
                remote_branches: Vec::new(),
                structured_branches: Vec::new(),
                status: None,
                observation: None,
                session_context: None,
                context_drift: false,
            }));
        }
        Err(error) => return Err(git_internal_error("读取 Git 分支", error)),
    };
    let session_context = observe_session_context(
        &state,
        &scope,
        request.context.session_id.as_deref(),
        &list.observation,
        false,
    )?;
    let context_drift = session_context
        .as_ref()
        .is_some_and(SessionCodeContext::has_external_drift);
    let branches = list
        .branches
        .iter()
        .filter(|branch| !branch.is_remote)
        .map(|branch| branch.name.clone())
        .collect();
    let remote_branches = list
        .branches
        .iter()
        .filter(|branch| branch.is_remote)
        .map(|branch| branch.name.clone())
        .collect();
    Ok(Json(BranchesResponse {
        is_repo: true,
        current_branch: list.observation.branch.clone(),
        branches,
        remote_branches,
        structured_branches: list.branches,
        status: Some(WorkspaceVcsStatus::from_observation(&list.observation)),
        observation: Some(list.observation),
        session_context,
        context_drift,
    }))
}

async fn branch_create(
    State(state): State<ApiState>,
    Json(request): Json<BranchCreateRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    let result = state
        .git_service
        .branch_create(
            &scope.path,
            BranchCreateOptions {
                branch: request.branch,
                start_point: request.start_point,
                switch: request.switch,
                precondition: prepared.precondition.clone(),
            },
        )
        .await;
    finish_observation_mutation(
        &state,
        &scope,
        &request.context,
        &prepared.precondition,
        result,
    )
    .await
}

async fn branch_switch(
    State(state): State<ApiState>,
    Json(request): Json<BranchSwitchRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    let result = state
        .git_service
        .branch_switch(
            &scope.path,
            BranchSwitchOptions {
                branch: request.branch,
                precondition: prepared.precondition.clone(),
            },
        )
        .await;
    finish_observation_mutation(
        &state,
        &scope,
        &request.context,
        &prepared.precondition,
        result,
    )
    .await
}

async fn merge_preview(
    State(state): State<ApiState>,
    Json(request): Json<MergeRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    match state
        .git_service
        .merge_preview(&scope.path, &request.target, &prepared.precondition)
        .await
    {
        Ok(preview) => Ok(Json(GitOperationResponse {
            ok: true,
            observation: Some(preview.observation.clone()),
            session_context: state
                .session_code_contexts
                .get(request.context.session_id.as_deref().unwrap_or_default()),
            data: Some(
                serde_json::to_value(preview)
                    .map_err(|error| ApiError::internal_assembly("序列化 merge preview", error))?,
            ),
            error: None,
        })),
        Err(error) => Ok(Json(GitOperationResponse::rejected(error))),
    }
}

async fn merge_branch(
    State(state): State<ApiState>,
    Json(request): Json<MergeRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    if !request.confirm {
        return Ok(Json(GitOperationResponse::rejected_kind(
            "confirmation_required",
            "合并会改变当前分支，必须先展示 merge preview 并由用户确认",
        )));
    }
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    let result = state
        .git_service
        .merge(
            &scope.path,
            MergeOptions {
                target: request.target,
                ff_only: request.ff_only,
                precondition: prepared.precondition.clone(),
            },
        )
        .await;
    finish_observation_mutation(
        &state,
        &scope,
        &request.context,
        &prepared.precondition,
        result,
    )
    .await
}

async fn branch_delete(
    State(state): State<ApiState>,
    Json(request): Json<BranchDeleteRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    let result = state
        .git_service
        .branch_delete(
            &scope.path,
            BranchDeleteOptions {
                branch: request.branch,
                remote: request.remote,
                force: request.force,
                confirm_force: request.confirm_force,
                confirm_remote: request.confirm_remote,
                precondition: prepared.precondition.clone(),
            },
        )
        .await;
    finish_observation_mutation(
        &state,
        &scope,
        &request.context,
        &prepared.precondition,
        result,
    )
    .await
}

async fn worktree_list(
    State(state): State<ApiState>,
    Json(request): Json<RequestContext>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.workspace_path)?;
    match state.git_service.worktree_list(&scope.path).await {
        Ok(worktrees) => {
            let managed_root = managed_worktree_root(&state, &scope);
            let worktrees = worktrees
                .into_iter()
                .map(|worktree| WorkspaceWorktreeResponse {
                    managed: path_is_within(&worktree.path, &managed_root),
                    worktree,
                })
                .collect::<Vec<_>>();
            Ok(Json(GitOperationResponse {
                ok: true,
                observation: None,
                session_context: request
                    .session_id
                    .as_deref()
                    .and_then(|session_id| state.session_code_contexts.get(session_id)),
                data: Some(
                    serde_json::to_value(worktrees).map_err(|error| {
                        ApiError::internal_assembly("序列化 worktree 列表", error)
                    })?,
                ),
                error: None,
            }))
        }
        Err(error) => Ok(Json(GitOperationResponse::rejected(error))),
    }
}

async fn worktree_create(
    State(state): State<ApiState>,
    Json(request): Json<WorktreeCreateRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    let observation = match state.git_service.observe(&scope.path).await {
        Ok(value) => value,
        Err(error) => return Ok(Json(GitOperationResponse::rejected(error))),
    };
    let Some(base) = request.base.or(observation.head.clone()) else {
        return Ok(Json(GitOperationResponse::rejected_kind(
            "invalid_input",
            "仓库没有 HEAD，必须显式指定 worktree base commit",
        )));
    };
    let allocation_key = sanitize_allocation_key(
        request
            .allocation_key
            .as_deref()
            .or(request.context.session_id.as_deref())
            .unwrap_or("manual"),
    );
    let worktree_root = managed_worktree_root(&state, &scope);
    std::fs::create_dir_all(&worktree_root)
        .map_err(|error| ApiError::internal_assembly("创建 Magi worktree 管理目录", error))?;
    let path = worktree_root.join(format!("{}-{}", allocation_key, UtcMillis::now().0));
    let (branch, create_branch, detached) = match request.mode {
        WorktreeMode::ReadOnly => (None, false, true),
        WorktreeMode::Writable => {
            let branch = request.branch.or_else(|| {
                Some(format!(
                    "magi/agent/{}-{}",
                    allocation_key,
                    UtcMillis::now().0
                ))
            });
            (branch, true, false)
        }
    };
    let result = state
        .git_service
        .worktree_create(
            &scope.path,
            WorktreeCreateOptions {
                path,
                base,
                branch,
                create_branch,
                detached,
                precondition: prepared.precondition.clone(),
            },
        )
        .await;
    match result {
        Ok(worktree) => Ok(Json(GitOperationResponse {
            ok: true,
            observation: Some(observation),
            session_context: request
                .context
                .session_id
                .as_deref()
                .and_then(|session_id| state.session_code_contexts.get(session_id)),
            data: Some(
                serde_json::to_value(worktree)
                    .map_err(|error| ApiError::internal_assembly("序列化 worktree", error))?,
            ),
            error: None,
        })),
        Err(error) => Ok(Json(GitOperationResponse::rejected(error))),
    }
}

async fn worktree_remove(
    State(state): State<ApiState>,
    Json(request): Json<WorktreeRemoveRequest>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    let scope = resolve_registered_workspace(&state, &request.context.workspace_path)?;
    let managed_root = managed_worktree_root(&state, &scope);
    if !path_is_within(&request.path, &managed_root) {
        return Ok(Json(GitOperationResponse::rejected_kind(
            "unmanaged_worktree",
            "只能移除由 Magi 管理目录创建的 worktree",
        )));
    }
    let prepared = match prepare_mutation(&state, &scope, &request.context).await? {
        Ok(prepared) => prepared,
        Err(response) => return Ok(Json(response)),
    };
    let removed_path = request.path.clone();
    match state
        .git_service
        .worktree_remove(
            &scope.path,
            WorktreeRemoveOptions {
                path: request.path,
                force: request.force,
                confirm_force: request.confirm_force,
                precondition: prepared.precondition.clone(),
            },
        )
        .await
    {
        Ok(worktrees) => {
            let session_context = if let Some(session_id) = request.context.session_id.as_deref() {
                if let Some(agent) =
                    state
                        .session_code_contexts
                        .get(session_id)
                        .and_then(|context| {
                            context
                                .agent_worktrees
                                .into_iter()
                                .find(|agent| agent.path == removed_path)
                        })
                {
                    let context = state
                        .session_code_contexts
                        .release_agent_worktree(session_id, &agent.task_id)
                        .map_err(|error| {
                            ApiError::internal_assembly("释放 agent worktree context", error)
                        })?;
                    state.persist_session_git_contexts()?;
                    Some(context)
                } else {
                    state.session_code_contexts.get(session_id)
                }
            } else {
                None
            };
            Ok(Json(GitOperationResponse {
                ok: true,
                observation: state.git_service.observe(&scope.path).await.ok(),
                session_context,
                data: Some(
                    serde_json::to_value(worktrees).map_err(|error| {
                        ApiError::internal_assembly("序列化 worktree 列表", error)
                    })?,
                ),
                error: None,
            }))
        }
        Err(error) => Ok(Json(GitOperationResponse::rejected(error))),
    }
}

async fn prepare_mutation(
    state: &ApiState,
    scope: &WorkspaceGitScope,
    request: &RequestContext,
) -> Result<Result<PreparedMutation, GitOperationResponse>, ApiError> {
    if workspace_has_running_turn(state, &scope.workspace_id) {
        return Ok(Err(GitOperationResponse::rejected_kind(
            "workspace_execution_active",
            "当前 workspace 有运行中的 turn/worker，不能改变 Git refs、HEAD 或 worktree",
        )));
    }
    let observation = match state.git_service.observe(&scope.path).await {
        Ok(value) => value,
        Err(error) => return Ok(Err(GitOperationResponse::rejected(error))),
    };
    let session_context = observe_session_context(
        state,
        scope,
        request.session_id.as_deref(),
        &observation,
        false,
    )?;
    if let Some(session_id) = request.session_id.as_deref()
        && let Err(error) = state
            .session_code_contexts
            .validate_revision(session_id, request.expected_context_revision)
    {
        return Ok(Err(GitOperationResponse::rejected_kind(
            "stale_context_revision",
            error.to_string(),
        )));
    }
    let mut precondition = request.precondition.clone();
    if precondition.expected_branch.is_none()
        && precondition.expected_head.is_none()
        && precondition.expected_worktree_path.is_none()
    {
        precondition = session_context
            .as_ref()
            .map(SessionCodeContext::precondition)
            .unwrap_or_else(|| GitPrecondition {
                expected_branch: observation.branch,
                expected_head: observation.head,
                expected_worktree_path: Some(observation.worktree_path),
            });
    }
    let lease = match state
        .workspace_git_coordinator
        .begin_mutation(&observation.git_common_dir)
    {
        Ok(lease) => lease,
        Err(error) => {
            return Ok(Err(GitOperationResponse::rejected_kind(
                "workspace_git_lease_conflict",
                error.to_string(),
            )));
        }
    };
    Ok(Ok(PreparedMutation {
        precondition,
        _lease: lease,
    }))
}

async fn finish_observation_mutation(
    state: &ApiState,
    scope: &WorkspaceGitScope,
    request: &RequestContext,
    precondition: &GitPrecondition,
    result: Result<GitObservation, GitError>,
) -> Result<Json<GitOperationResponse>, ApiError> {
    match result {
        Ok(observation) => {
            let session_context = observe_session_context(
                state,
                scope,
                request.session_id.as_deref(),
                &observation,
                true,
            )?;
            let snapshot_baseline_status = if git_tree_changed(precondition, &observation) {
                if let Some(session_id) = request.session_id.as_deref() {
                    match state
                        .snapshot_manager
                        .rebase_session(session_id.to_string(), scope.path.clone())
                        .await
                    {
                        Ok(_) => Some("refreshed"),
                        Err(error) => {
                            tracing::warn!(
                                session_id,
                                workspace_id = %scope.workspace_id,
                                ?error,
                                "Git context 变化后重建 snapshot baseline 失败"
                            );
                            Some("failed")
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            publish_git_context_changed(state, scope, request.session_id.as_deref(), &observation);
            super::knowledge::schedule_workspace_code_index(
                state.clone(),
                scope.workspace_id.clone(),
                scope.path.clone(),
            );
            Ok(Json(GitOperationResponse {
                ok: true,
                observation: Some(observation),
                session_context,
                data: snapshot_baseline_status
                    .map(|status| serde_json::json!({ "snapshotBaselineStatus": status })),
                error: None,
            }))
        }
        Err(error) => {
            if matches!(error, GitError::MergeConflict { .. })
                && let Ok(observation) = state.git_service.observe(&scope.path).await
            {
                let session_context = observe_session_context(
                    state,
                    scope,
                    request.session_id.as_deref(),
                    &observation,
                    false,
                )?;
                publish_git_context_changed(
                    state,
                    scope,
                    request.session_id.as_deref(),
                    &observation,
                );
                return Ok(Json(GitOperationResponse {
                    ok: false,
                    observation: Some(observation),
                    session_context,
                    data: None,
                    error: Some(GitOperationErrorDto::from(error)),
                }));
            }
            Ok(Json(GitOperationResponse::rejected(error)))
        }
    }
}

fn git_tree_changed(precondition: &GitPrecondition, observation: &GitObservation) -> bool {
    precondition.expected_branch != observation.branch
        || precondition.expected_head != observation.head
        || precondition
            .expected_worktree_path
            .as_deref()
            .is_some_and(|path| !same_path(path, &observation.worktree_path))
}

fn observe_session_context(
    state: &ApiState,
    scope: &WorkspaceGitScope,
    session_id: Option<&str>,
    observation: &GitObservation,
    accept: bool,
) -> Result<Option<SessionCodeContext>, ApiError> {
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let typed_session_id = SessionId::new(session_id.to_string());
    let session = state
        .session_store
        .session(&typed_session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id))?;
    if session.workspace_id.as_deref() != Some(scope.workspace_id.as_str()) {
        return Err(ApiError::Conflict(
            "session 不属于请求中的 workspace，不能绑定 Git context".to_string(),
        ));
    }
    let runtime_roots = vec![scope.path.clone()];
    let context = if accept {
        state.session_code_contexts.accept(
            session_id,
            scope.workspace_id.as_str(),
            runtime_roots,
            observation,
        )
    } else {
        state.session_code_contexts.observe(
            session_id,
            scope.workspace_id.as_str(),
            runtime_roots,
            observation,
        )
    };
    state.persist_session_git_contexts()?;
    Ok(Some(context))
}

fn workspace_has_running_turn(state: &ApiState, workspace_id: &WorkspaceId) -> bool {
    state
        .session_store
        .sessions()
        .into_iter()
        .filter(|session| session.workspace_id.as_deref() == Some(workspace_id.as_str()))
        .any(|session| {
            state
                .session_store
                .runtime_sidecar(&session.session_id)
                .and_then(|sidecar| {
                    sidecar.current_turn.or_else(|| {
                        sidecar
                            .active_execution_chain
                            .and_then(|chain| chain.current_turn)
                    })
                })
                .is_some_and(|turn| !turn_status_is_terminal(&turn.status))
        })
}

fn turn_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "failed" | "cancelled" | "canceled" | "superseded"
    )
}

fn publish_git_context_changed(
    state: &ApiState,
    scope: &WorkspaceGitScope,
    session_id: Option<&str>,
    observation: &GitObservation,
) {
    let now = UtcMillis::now();
    let typed_session_id = session_id.map(|value| SessionId::new(value.to_string()));
    let context_revision = session_id
        .and_then(|value| state.session_code_contexts.get(value))
        .map(|context| context.context_revision);
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "workspace-git-context-changed-{}-{}",
                scope.workspace_id, now.0
            )),
            "workspace.git.context.changed",
            serde_json::json!({
                "workspace_id": scope.workspace_id,
                "session_id": typed_session_id,
                "repository_root": observation.repository_root,
                "worktree_path": observation.worktree_path,
                "branch": observation.branch,
                "head": observation.head,
                "context_revision": context_revision,
                "refresh_scopes": ["file_tree", "code_index", "knowledge", "context_cache"]
            }),
        )
        .with_context(EventContext {
            session_id: typed_session_id,
            workspace_id: Some(scope.workspace_id.clone()),
            ..EventContext::default()
        }),
    );
}

fn managed_worktree_root(state: &ApiState, scope: &WorkspaceGitScope) -> PathBuf {
    let state_root = state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        .map(Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|home| home.join(".magi")))
        .unwrap_or_else(std::env::temp_dir);
    state_root
        .join("worktrees")
        .join(scope.workspace_id.as_str())
}

fn sanitize_allocation_key(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "allocation".to_string()
    } else {
        sanitized.chars().take(80).collect()
    }
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let candidate = path
        .canonicalize()
        .or_else(|_| {
            path.parent()
                .ok_or_else(|| std::io::Error::other("path has no parent"))?
                .canonicalize()
                .map(|parent| parent.join(path.file_name().unwrap_or_default()))
        })
        .unwrap_or_else(|_| path.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    candidate.starts_with(root)
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_path_buf())
        == right.canonicalize().unwrap_or_else(|_| right.to_path_buf())
}

fn git_internal_error(context: &str, error: GitError) -> ApiError {
    ApiError::InternalAssemblyError(format!("{context}: {error}"))
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::{fs, sync::Arc};
    use tower::ServiceExt;

    fn git(path: &Path, args: &[&str]) {
        let output = magi_process::std_command("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git fixture command should start");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_api_fixture() -> (tempfile::TempDir, ApiState, String, String) {
        let repository = tempfile::tempdir().expect("Git fixture");
        git(repository.path(), &["init", "-b", "main"]);
        git(repository.path(), &["config", "user.name", "Magi Test"]);
        git(
            repository.path(),
            &["config", "user.email", "magi@example.test"],
        );
        fs::write(repository.path().join("README.md"), "base\n").expect("fixture file");
        git(repository.path(), &["add", "README.md"]);
        git(repository.path(), &["commit", "-m", "base"]);

        let workspace_id = WorkspaceId::new("workspace-git-api");
        let session_id = SessionId::new("session-git-api");
        let workspaces = Arc::new(WorkspaceStore::default());
        workspaces
            .register_native_path(workspace_id.clone(), repository.path().to_path_buf())
            .expect("register workspace");
        let sessions = Arc::new(SessionStore::default());
        sessions
            .create_session_for_workspace(
                session_id.clone(),
                "Git API",
                Some(workspace_id.to_string()),
            )
            .expect("create session");
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(64)),
            sessions,
            workspaces,
            Arc::new(GovernanceService::default()),
        );
        (
            repository,
            state,
            workspace_id.to_string(),
            session_id.to_string(),
        )
    }

    async fn post_json(state: ApiState, uri: &str, body: Value) -> Value {
        let response = routes()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json response")
    }

    #[test]
    fn allocation_key_is_path_safe_and_bounded() {
        assert_eq!(sanitize_allocation_key("session/a b"), "session-a-b");
        assert_eq!(sanitize_allocation_key("///"), "allocation");
        assert_eq!(sanitize_allocation_key(&"a".repeat(100)).len(), 80);
    }

    #[test]
    fn terminal_turn_status_is_explicit() {
        assert!(turn_status_is_terminal("completed"));
        assert!(turn_status_is_terminal("FAILED"));
        assert!(!turn_status_is_terminal("running"));
        assert!(!turn_status_is_terminal("blocked"));
    }

    #[test]
    fn git_error_dto_keeps_conflict_details() {
        let dto = GitOperationErrorDto::from(GitError::MergeConflict {
            target: "feature".to_string(),
            conflicted_paths: vec!["src/lib.rs".to_string()],
            stdout: "merge output".to_string(),
            stderr: "conflict".to_string(),
        });
        assert_eq!(dto.kind, "merge_conflict");
        assert_eq!(dto.conflicted_paths, vec!["src/lib.rs"]);
        assert_eq!(dto.stderr.as_deref(), Some("conflict"));
    }

    #[test]
    fn workspace_status_projects_structured_observation() {
        let observation = GitObservation {
            repository_root: PathBuf::from("/repo"),
            git_common_dir: PathBuf::from("/repo/.git"),
            worktree_path: PathBuf::from("/repo"),
            worktree_git_dir: PathBuf::from("/repo/.git"),
            branch: Some("main".to_string()),
            head: Some("abc".to_string()),
            upstream: Some("origin/main".to_string()),
            origin_url: None,
            ahead: 2,
            behind: 1,
            dirty: GitDirtySummary {
                has_uncommitted: true,
                staged: 1,
                unstaged: 2,
                ..GitDirtySummary::default()
            },
        };
        let status = WorkspaceVcsStatus::from_observation(&observation);
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert!(status.has_uncommitted);
        assert_eq!(status.staged, 1);
        assert_eq!(status.unstaged, 2);
    }

    #[tokio::test]
    async fn structured_api_binds_session_and_auto_switches_created_branch() {
        let (repository, state, _workspace_id, session_id) = git_api_fixture();
        let workspace_path = repository.path().to_string_lossy().to_string();
        state
            .ensure_snapshot_session(&SessionId::new(session_id.clone()), repository.path())
            .await
            .expect("snapshot baseline");
        let status = post_json(
            state.clone(),
            "/workspace/vcs/status",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
            }),
        )
        .await;
        assert_eq!(status["isRepo"], true);
        assert_eq!(status["currentBranch"], "main");
        assert_eq!(status["contextDrift"], false);

        let created = post_json(
            state.clone(),
            "/workspace/vcs/branch/create",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
                "expectedContextRevision": status["sessionContext"]["contextRevision"],
                "branch": "feature/api",
            }),
        )
        .await;
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(created["data"]["snapshotBaselineStatus"], "refreshed");
        assert_eq!(created["observation"]["branch"], "feature/api");
        assert_eq!(
            created["sessionContext"]["git"]["desiredRef"],
            "feature/api"
        );
        assert!(
            state
                .snapshot_session(&SessionId::new(session_id), repository.path())
                .expect("rebased snapshot")
                .pending_changes()
                .expect("pending changes")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn merge_conflict_refreshes_context_and_blocks_next_turn() {
        let (repository, state, workspace_id, session_id) = git_api_fixture();
        git(repository.path(), &["switch", "-c", "feature/conflict"]);
        fs::write(repository.path().join("README.md"), "feature\n").expect("feature edit");
        git(repository.path(), &["add", "README.md"]);
        git(repository.path(), &["commit", "-m", "feature conflict"]);
        git(repository.path(), &["switch", "main"]);
        fs::write(repository.path().join("README.md"), "main\n").expect("main edit");
        git(repository.path(), &["add", "README.md"]);
        git(repository.path(), &["commit", "-m", "main conflict"]);
        let workspace_path = repository.path().to_string_lossy().to_string();
        let status = post_json(
            state.clone(),
            "/workspace/vcs/status",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
            }),
        )
        .await;

        let conflicted = post_json(
            state.clone(),
            "/workspace/vcs/merge",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
                "expectedContextRevision": status["sessionContext"]["contextRevision"],
                "target": "feature/conflict",
                "confirm": true,
            }),
        )
        .await;
        assert_eq!(conflicted["ok"], false, "{conflicted}");
        assert_eq!(conflicted["error"]["kind"], "merge_conflict");
        assert_eq!(
            conflicted["error"]["conflictedPaths"],
            serde_json::json!(["README.md"])
        );
        assert_eq!(conflicted["observation"]["dirty"]["conflicted"], 1);
        assert_eq!(
            conflicted["sessionContext"]["git"]["dirty"]["conflictedPaths"],
            serde_json::json!(["README.md"])
        );

        let next_turn = state
            .ensure_session_code_context(
                &SessionId::new(session_id),
                &Some(WorkspaceId::new(workspace_id)),
            )
            .await
            .expect_err("unresolved conflict must block next turn");
        assert!(matches!(
            next_turn,
            ApiError::Conflict(message) if message.contains("merge conflict")
        ));
    }

    #[tokio::test]
    async fn structured_api_reports_dirty_and_external_head_drift() {
        let (repository, state, _workspace_id, session_id) = git_api_fixture();
        let workspace_path = repository.path().to_string_lossy().to_string();
        let status = post_json(
            state.clone(),
            "/workspace/vcs/status",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
            }),
        )
        .await;
        fs::write(repository.path().join("README.md"), "dirty\n").expect("dirty fixture");
        let dirty = post_json(
            state.clone(),
            "/workspace/vcs/branch/create",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
                "expectedContextRevision": status["sessionContext"]["contextRevision"],
                "branch": "feature/dirty",
            }),
        )
        .await;
        assert_eq!(dirty["ok"], false);
        assert_eq!(dirty["error"]["kind"], "dirty_workspace");

        git(repository.path(), &["restore", "README.md"]);
        fs::write(repository.path().join("external.txt"), "external\n").expect("external fixture");
        git(repository.path(), &["add", "external.txt"]);
        git(repository.path(), &["commit", "-m", "external"]);
        let stale = post_json(
            state.clone(),
            "/workspace/vcs/branch/create",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
                "branch": "feature/stale",
            }),
        )
        .await;
        assert_eq!(stale["ok"], false);
        assert_eq!(stale["error"]["kind"], "stale_git_context");
        assert!(stale["error"]["actualHead"].is_string());

        let drift = post_json(
            state.clone(),
            "/workspace/vcs/status",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
            }),
        )
        .await;
        assert_eq!(drift["contextDrift"], true);
        let accepted = post_json(
            state,
            "/workspace/vcs/context/accept",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
                "expectedContextRevision": drift["sessionContext"]["contextRevision"],
            }),
        )
        .await;
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(
            accepted["sessionContext"]["git"]["baseHead"],
            accepted["observation"]["head"]
        );
    }

    #[tokio::test]
    async fn structured_api_rejects_mutation_while_execution_lease_is_active() {
        let (repository, state, _workspace_id, session_id) = git_api_fixture();
        let workspace_path = repository.path().to_string_lossy().to_string();
        let status = post_json(
            state.clone(),
            "/workspace/vcs/status",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
            }),
        )
        .await;
        let git_common_dir = PathBuf::from(
            status["sessionContext"]["git"]["gitCommonDir"]
                .as_str()
                .expect("git common dir"),
        );
        state
            .workspace_git_coordinator
            .begin_execution(&session_id, &git_common_dir)
            .expect("execution lease");

        let blocked = post_json(
            state.clone(),
            "/workspace/vcs/branch/create",
            serde_json::json!({
                "workspacePath": workspace_path,
                "sessionId": session_id,
                "branch": "feature/blocked",
            }),
        )
        .await;
        assert_eq!(blocked["ok"], false);
        assert_eq!(blocked["error"]["kind"], "workspace_git_lease_conflict");
        state.workspace_git_coordinator.end_execution(&session_id);
    }

    #[allow(dead_code)]
    fn assert_response_payloads_are_serializable(
        preview: magi_git::GitMergePreview,
        worktree: magi_git::GitWorktree,
    ) {
        let _ = serde_json::to_value(preview).unwrap();
        let _ = serde_json::to_value(worktree).unwrap();
    }
}
