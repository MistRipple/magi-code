//! 文件变更投影：把 magi-snapshot 提供的 pending changes 转成前端 DTO。
//!
//! 唯一真相源 = magi-snapshot（BlobStore + BaselineIndex + ChangeLog）。
//! 本模块只做：
//!   1. 鉴权（路径安全 + 会话归属）；
//!   2. DTO 适配（PendingChange → PendingChangeDto，补 contributor / execution_group 元信息）。
//!
//! 不再读 git，也不再写 git。

use crate::{errors::ApiError, state::ApiState};
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_snapshot::{ChangeKind, ContentKind, PendingChange, SourceKind};
use serde::Serialize;
use std::fmt::Display;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct SessionChangeScope {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub workspace_root: PathBuf,
    pub execution_group_id: String,
    pub contributors: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct WorkspaceChangeScope {
    pub workspace_id: WorkspaceId,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChangesStateDto {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    pub pending_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPendingChangesProjection {
    pub pending_changes: Vec<PendingChangeDto>,
    pub state: PendingChangesStateDto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChangeDto {
    pub session_id: String,
    pub workspace_id: String,
    pub workspace_path: String,
    pub file_path: String,
    pub snapshot_id: String,
    pub updated_at: UtcMillis,
    #[serde(rename = "type")]
    pub r#type: String,
    pub additions: usize,
    pub deletions: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub diff: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_content: Option<String>,
    pub preview_absolute_path: String,
    pub preview_can_open_workspace_file: bool,
    pub content_kind: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub source_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub has_error: bool,
    pub revertible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<String>,
    pub execution_group_id: String,
}

/// 解析当前会话的变更 scope：仅保留路径安全/归属/归因相关字段。
/// 真正的"哪些文件被改"由 SnapshotSession 提供，无需在这里圈白名单。
pub(crate) fn resolve_session_change_scope(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&str>,
    execution_group_id_override: Option<&str>,
) -> Result<SessionChangeScope, ApiError> {
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let ownership = state.session_store.execution_ownership(session_id);

    let bound_workspace_id = ownership
        .as_ref()
        .and_then(|ownership| ownership.workspace_id.clone())
        .or_else(|| state.session_workspace_id(&session))
        .ok_or_else(|| {
            ApiError::InvalidInput("当前会话未绑定 workspace，不能执行变更操作".to_string())
        })?;
    if let Some(requested_workspace_id) = workspace_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && requested_workspace_id != bound_workspace_id.as_str()
    {
        return Err(ApiError::InvalidInput(format!(
            "会话 {} 不属于 workspace {}",
            session_id, requested_workspace_id
        )));
    }

    let mission_id = ownership
        .as_ref()
        .and_then(|ownership| ownership.mission_id.clone());
    let execution_group_id = execution_group_id_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            mission_id
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| Some(session_execution_group_id(session_id)))
        })
        .expect("session execution group must always have a fallback");

    let workspace_root = resolve_workspace_root(state, &bound_workspace_id)?;
    let contributors = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
        .map(|chain| {
            let mut workers = chain
                .branches
                .into_iter()
                .map(|branch| branch.worker_id.to_string())
                .chain(
                    chain
                        .active_worker_bindings
                        .into_iter()
                        .map(|worker| worker.to_string()),
                )
                .filter(|worker| !worker.trim().is_empty())
                .collect::<Vec<_>>();
            workers.sort();
            workers.dedup();
            workers
        })
        .unwrap_or_default();

    Ok(SessionChangeScope {
        session_id: session_id.clone(),
        workspace_id: bound_workspace_id,
        workspace_root,
        execution_group_id,
        contributors,
    })
}

pub(crate) fn resolve_workspace_change_scope(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<WorkspaceChangeScope, ApiError> {
    let workspace_root = resolve_workspace_root(state, workspace_id)?;
    Ok(WorkspaceChangeScope {
        workspace_id: workspace_id.clone(),
        workspace_root,
    })
}

/// 取出会话的 pending changes（来自 SnapshotSession）。
/// 取出当前会话的 pending changes，并携带快照账本状态。
///
/// 状态只表达产品可理解的阶段：ready / not_ready / unavailable / error。
/// 具体底层错误不进入用户态 payload，详细信息留给日志和服务端错误链路。
pub(crate) fn collect_session_pending_changes_with_state(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&str>,
) -> Result<SessionPendingChangesProjection, ApiError> {
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let ownership = state.session_store.execution_ownership(session_id);
    let bound_workspace_id = ownership
        .as_ref()
        .and_then(|ownership| ownership.workspace_id.clone())
        .or_else(|| state.session_workspace_id(&session));
    if let Some(requested_workspace_id) = workspace_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && bound_workspace_id
            .as_ref()
            .is_some_and(|bound| bound.as_str() != requested_workspace_id)
    {
        return Err(ApiError::InvalidInput(format!(
            "会话 {} 不属于 workspace {}",
            session_id, requested_workspace_id
        )));
    }
    let Some(bound_workspace_id) = bound_workspace_id else {
        return Ok(SessionPendingChangesProjection {
            pending_changes: Vec::new(),
            state: pending_changes_state(
                "unavailable",
                Some(session_id),
                None,
                None,
                0,
                Some("workspace_unavailable"),
            ),
        });
    };
    let Some(workspace_root) = state.workspace_root_path(&Some(bound_workspace_id.clone())) else {
        return Ok(SessionPendingChangesProjection {
            pending_changes: Vec::new(),
            state: pending_changes_state(
                "unavailable",
                Some(session_id),
                Some(&bound_workspace_id),
                None,
                0,
                Some("workspace_unavailable"),
            ),
        });
    };
    if state
        .snapshot_session(session_id, &workspace_root)
        .is_none()
    {
        return Ok(SessionPendingChangesProjection {
            pending_changes: Vec::new(),
            state: pending_changes_state(
                "not_ready",
                Some(session_id),
                Some(&bound_workspace_id),
                Some(&workspace_root),
                0,
                Some("changes_preparing"),
            ),
        });
    }
    let scope = resolve_session_change_scope(state, session_id, workspace_id, None)?;
    let Some(snapshot_session) = state.snapshot_session(&scope.session_id, &scope.workspace_root)
    else {
        return Ok(SessionPendingChangesProjection {
            pending_changes: Vec::new(),
            state: pending_changes_state(
                "not_ready",
                Some(&scope.session_id),
                Some(&scope.workspace_id),
                Some(&scope.workspace_root),
                0,
                Some("changes_preparing"),
            ),
        });
    };
    let pending = match snapshot_session.pending_changes() {
        Ok(pending) => pending,
        Err(_) => {
            return Ok(SessionPendingChangesProjection {
                pending_changes: Vec::new(),
                state: pending_changes_state(
                    "error",
                    Some(&scope.session_id),
                    Some(&scope.workspace_id),
                    Some(&scope.workspace_root),
                    0,
                    Some("changes_unavailable"),
                ),
            });
        }
    };
    let mut pending_changes = pending
        .into_iter()
        .map(|change| convert_pending(&scope, change))
        .collect::<Vec<_>>();
    pending_changes.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    Ok(SessionPendingChangesProjection {
        state: pending_changes_state(
            "ready",
            Some(&scope.session_id),
            Some(&scope.workspace_id),
            Some(&scope.workspace_root),
            pending_changes.len(),
            None,
        ),
        pending_changes,
    })
}

fn convert_pending(scope: &SessionChangeScope, change: PendingChange) -> PendingChangeDto {
    let execution_group_id = change
        .execution_group_id
        .clone()
        .unwrap_or_else(|| scope.execution_group_id.clone());
    let absolute_path = scope.workspace_root.join(&change.path);
    let preview_can_open_workspace_file = matches!(
        change.change_kind,
        ChangeKind::Added | ChangeKind::Modified | ChangeKind::Renamed
    ) && absolute_path.is_file();

    let (additions, deletions) = count_diff_lines(change.unified_diff.as_deref().unwrap_or(""));
    let r#type = change_kind_to_string(change.change_kind);
    let content_kind = content_kind_to_string(change.content_kind);
    let source_kind = source_kind_to_string(change.source);

    PendingChangeDto {
        session_id: scope.session_id.as_str().to_string(),
        workspace_id: scope.workspace_id.as_str().to_string(),
        workspace_path: scope.workspace_root.to_string_lossy().to_string(),
        file_path: change.path.clone(),
        snapshot_id: format!("{}:{}", execution_group_id, change.path),
        updated_at: UtcMillis(change.timestamp_ms),
        r#type,
        additions,
        deletions,
        diff: String::new(),
        original_content: None,
        preview_content: None,
        preview_absolute_path: absolute_path.to_string_lossy().to_string(),
        preview_can_open_workspace_file,
        content_kind,
        size: change.size,
        mime: change.mime,
        source_kind,
        old_path: change.old_path,
        has_error: change
            .error
            .as_deref()
            .is_some_and(|error| !error.trim().is_empty()),
        revertible: change.revertible,
        symlink_target: change.symlink_target,
        head_summary: change.head_summary,
        tail_summary: change.tail_summary,
        tool_call_id: change.tool_call_id,
        worker_id: change.worker_id,
        contributors: scope.contributors.clone(),
        execution_group_id,
    }
}

pub(crate) fn pending_changes_state(
    status: &str,
    session_id: Option<&SessionId>,
    workspace_id: Option<&WorkspaceId>,
    workspace_root: Option<&Path>,
    pending_count: usize,
    reason_code: Option<&str>,
) -> PendingChangesStateDto {
    PendingChangesStateDto {
        status: status.to_string(),
        reason_code: reason_code.map(ToString::to_string),
        session_id: session_id.map(|id| id.as_str().to_string()),
        workspace_id: workspace_id.map(|id| id.as_str().to_string()),
        workspace_path: workspace_root.map(|path| path.to_string_lossy().to_string()),
        pending_count,
    }
}

fn change_kind_to_string(k: ChangeKind) -> String {
    match k {
        ChangeKind::Added => "add",
        ChangeKind::Modified => "modify",
        ChangeKind::Deleted => "delete",
        ChangeKind::Renamed => "rename",
    }
    .to_string()
}

fn content_kind_to_string(k: ContentKind) -> String {
    match k {
        ContentKind::Text => "text",
        ContentKind::LargeText => "large_text",
        ContentKind::Binary => "binary",
        ContentKind::Symlink => "symlink",
        ContentKind::Special => "special",
    }
    .to_string()
}

fn source_kind_to_string(k: SourceKind) -> String {
    match k {
        SourceKind::Tool => "tool",
        SourceKind::Watcher => "watcher",
        SourceKind::External => "external",
        SourceKind::Baseline => "baseline",
    }
    .to_string()
}

fn count_diff_lines(diff: &str) -> (usize, usize) {
    let mut adds = 0usize;
    let mut dels = 0usize;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if line.starts_with('+') {
            adds += 1;
        } else if line.starts_with('-') {
            dels += 1;
        }
    }
    (adds, dels)
}

pub(crate) fn safe_relative_path(file_path: &str) -> Result<&str, ApiError> {
    let bytes = file_path.as_bytes();
    let has_windows_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let has_unc_prefix = file_path.starts_with(r"\\");
    if has_windows_prefix || has_unc_prefix {
        return Err(ApiError::InvalidInput("路径不允许为绝对路径".to_string()));
    }
    let path = Path::new(file_path);
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(ApiError::InvalidInput("路径不允许包含 ..".to_string()));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(ApiError::InvalidInput("路径不允许为绝对路径".to_string()));
            }
            std::path::Component::CurDir | std::path::Component::Normal(_) => {}
        }
    }
    Ok(file_path)
}

/// 把工作区相对路径或工作区下的绝对路径安全解析为工作区内的真实文件路径。
///
/// 返回值：`(canonical_absolute_path, workspace_relative_path)`。
/// - 相对路径：拒绝 `..` 与绝对前缀，再拼到 `workspace_root` 上。
/// - 绝对路径：直接使用。
///
/// 解析后必须 canonicalize 且仍位于 `workspace_root` 之内，否则视为越界并拒绝。
pub(crate) fn safe_workspace_path(
    workspace_root: &Path,
    file_path: &str,
) -> Result<(PathBuf, String), ApiError> {
    let trimmed = file_path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidInput("文件路径不能为空".to_string()));
    }
    let decoded_path = trimmed
        .starts_with("mhp1:")
        .then(|| magi_core::HostPath::from_path_ref(trimmed))
        .transpose()
        .map_err(|_| ApiError::InvalidInput("路径引用无效".to_string()))?
        .map(magi_core::HostPath::into_path_buf);
    let candidate = decoded_path
        .as_deref()
        .unwrap_or_else(|| Path::new(trimmed));
    let candidate_abs = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        let rel = safe_relative_path(trimmed)?;
        workspace_root.join(rel)
    };
    let canonical_root = magi_core::HostPath::canonicalize(workspace_root)
        .map(magi_core::HostPath::into_path_buf)
        .map_err(|e| path_access_error("规范化工作区根目录失败", workspace_root, e))?;
    let canonical_path = magi_core::HostPath::canonicalize(&candidate_abs)
        .map(magi_core::HostPath::into_path_buf)
        .map_err(|e| path_access_error("规范化文件路径失败", &candidate_abs, e))?;
    let relative = canonical_path
        .strip_prefix(&canonical_root)
        .map_err(|_| ApiError::InvalidInput("路径越出工作区边界".to_string()))?
        .to_string_lossy()
        .into_owned();
    Ok((canonical_path, relative))
}

fn path_access_error(context: &'static str, path: &Path, error: impl Display) -> ApiError {
    tracing::warn!(
        context,
        path = %path.display(),
        error = %error,
        "workspace path access failed"
    );
    ApiError::InvalidInput("路径不可读取或不存在".to_string())
}

fn resolve_workspace_root(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<PathBuf, ApiError> {
    let workspaces = state.workspace_registry.workspaces();
    let workspace = workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == *workspace_id)
        .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace_id.as_str()))?;
    Ok(workspace.native_root_path())
}

fn session_execution_group_id(session_id: &SessionId) -> String {
    format!("session:{}", session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use magi_core::{AbsolutePath, ExecutionOwnership, MissionId, SessionId, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_REPO_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let unique = TEST_REPO_COUNTER.fetch_add(1, Ordering::Relaxed);
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

    fn register_workspace_and_session(
        state: &ApiState,
        workspace_id: &str,
        session_id: &str,
        root: &Path,
        mission_id: Option<&str>,
    ) -> (WorkspaceId, SessionId) {
        let ws = WorkspaceId::new(workspace_id);
        let sid = SessionId::new(session_id);
        state
            .workspace_registry
            .register(
                ws.clone(),
                AbsolutePath::new(root.to_string_lossy().to_string()),
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
        (ws, sid)
    }

    #[tokio::test]
    async fn collect_pending_changes_returns_empty_without_workspace_binding() {
        let state = build_state();
        let session_id = SessionId::new("session-no-workspace");
        state
            .session_store
            .create_session(session_id.clone(), "未绑定 workspace 的会话")
            .expect("session should create");
        let projection = collect_session_pending_changes_with_state(&state, &session_id, None)
            .expect("missing workspace binding should be treated as empty");
        assert!(projection.pending_changes.is_empty());
        assert_eq!(projection.state.status, "unavailable");
        assert_eq!(
            projection.state.reason_code.as_deref(),
            Some("workspace_unavailable")
        );
    }

    #[tokio::test]
    async fn collect_pending_changes_returns_empty_when_snapshot_not_started() {
        // SnapshotLifecycleObserver 是 tokio::spawn 启动的，bootstrap 等只读路径
        // 可能在快照账本就绪之前就来取 pending changes。语义上等价于"暂无变更"，
        // 而不是直接 400，避免与异步生命周期形成竞态。
        let state = build_state();
        let root = unique_temp_dir("magi-change-projection-no-snapshot");
        let (_, sid) = register_workspace_and_session(
            &state,
            "ws-no-snapshot",
            "sess-no-snapshot",
            &root,
            None,
        );
        let projection =
            collect_session_pending_changes_with_state(&state, &sid, Some("ws-no-snapshot"))
                .expect("snapshot not yet started should be treated as empty");
        assert!(projection.pending_changes.is_empty());
        assert_eq!(projection.state.status, "not_ready");
        assert_eq!(
            projection.state.reason_code.as_deref(),
            Some("changes_preparing")
        );
    }

    #[tokio::test]
    async fn collect_pending_changes_ignores_snapshot_from_other_workspace_root() {
        let state = build_state();
        let root = unique_temp_dir("magi-change-projection-correct-workspace");
        let other_root = unique_temp_dir("magi-change-projection-wrong-workspace");
        let (_, sid) = register_workspace_and_session(
            &state,
            "ws-correct-snapshot-root",
            "sess-cross-root",
            &root,
            None,
        );
        state
            .snapshot_manager
            .start_session(sid.as_str().to_string(), other_root)
            .await
            .expect("wrong root snapshot can exist before projection resolves scope");

        let projection = collect_session_pending_changes_with_state(
            &state,
            &sid,
            Some("ws-correct-snapshot-root"),
        )
        .expect("wrong workspace snapshot should be treated as not ready");

        assert!(projection.pending_changes.is_empty());
        assert_eq!(projection.state.status, "not_ready");
        assert_eq!(
            projection.state.reason_code.as_deref(),
            Some("changes_preparing")
        );
    }

    #[tokio::test]
    async fn collect_pending_changes_projects_added_modified_deleted_files() {
        let state = build_state();
        let root = unique_temp_dir("magi-change-projection-snapshot");
        fs::write(root.join("alpha.txt"), "alpha\n").expect("alpha should write");
        fs::write(root.join("beta.txt"), "beta\n").expect("beta should write");
        let (_, sid) = register_workspace_and_session(
            &state,
            "ws-snapshot",
            "sess-snapshot",
            &root,
            Some("mission-x"),
        );
        // 等观察者异步启动 session 后做修改，统一靠 reconcile() 拉齐。
        let snap = state
            .snapshot_manager
            .start_session(sid.as_str().to_string(), root.clone())
            .await
            .expect("snapshot session should start");

        // 修改/新增/删除三种类型。
        fs::write(root.join("alpha.txt"), "alpha changed\n").expect("alpha modify");
        fs::write(root.join("gamma.txt"), "gamma\n").expect("gamma add");
        fs::remove_file(root.join("beta.txt")).expect("beta delete");
        snap.reconcile().expect("reconcile should succeed");

        let projection =
            collect_session_pending_changes_with_state(&state, &sid, Some("ws-snapshot"))
                .expect("pending changes should collect");
        assert_eq!(projection.state.status, "ready");
        assert_eq!(projection.state.pending_count, 3);
        let by_path: std::collections::BTreeMap<_, _> = projection
            .pending_changes
            .iter()
            .map(|c| (c.file_path.clone(), c.clone()))
            .collect();
        assert_eq!(
            by_path.get("alpha.txt").map(|c| c.r#type.as_str()),
            Some("modify")
        );
        assert_eq!(
            by_path.get("gamma.txt").map(|c| c.r#type.as_str()),
            Some("add")
        );
        assert_eq!(
            by_path.get("beta.txt").map(|c| c.r#type.as_str()),
            Some("delete")
        );

        // execution_group_id 来自 mission_id。
        for change in &projection.pending_changes {
            assert_eq!(change.execution_group_id, "mission-x");
            assert_eq!(change.content_kind, "text");
        }
    }

    #[tokio::test]
    async fn collect_pending_changes_projects_rename_old_path() {
        let state = build_state();
        let root = unique_temp_dir("magi-change-projection-rename");
        fs::write(root.join("before.txt"), "same\n").expect("before should write");
        let (_, sid) = register_workspace_and_session(
            &state,
            "ws-rename",
            "sess-rename",
            &root,
            Some("mission-rename"),
        );
        let snap = state
            .snapshot_manager
            .start_session(sid.as_str().to_string(), root.clone())
            .await
            .expect("snapshot session should start");

        fs::rename(root.join("before.txt"), root.join("after.txt")).expect("file should rename");
        snap.reconcile().expect("reconcile should succeed");

        let projection =
            collect_session_pending_changes_with_state(&state, &sid, Some("ws-rename"))
                .expect("pending changes should collect");
        let changes = projection.pending_changes;
        assert_eq!(projection.state.status, "ready");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].file_path, "after.txt");
        assert_eq!(changes[0].old_path.as_deref(), Some("before.txt"));
        assert_eq!(changes[0].r#type, "rename");
        assert_eq!(changes[0].execution_group_id, "mission-rename");
    }

    #[test]
    fn pending_change_projection_redacts_snapshot_error_detail() {
        let scope = SessionChangeScope {
            session_id: SessionId::new("session-change-error"),
            workspace_id: WorkspaceId::new("workspace-change-error"),
            workspace_root: unique_temp_dir("magi-change-projection-error"),
            execution_group_id: "execution-error".to_string(),
            contributors: Vec::new(),
        };
        let dto = convert_pending(
            &scope,
            PendingChange {
                path: "restricted.txt".to_string(),
                change_kind: ChangeKind::Modified,
                old_path: None,
                source: SourceKind::Watcher,
                tool_call_id: None,
                worker_id: None,
                execution_group_id: None,
                content_kind: ContentKind::Text,
                size: 0,
                mime: None,
                error: Some("read failed: permission denied".to_string()),
                revertible: false,
                symlink_target: None,
                original_content: None,
                preview_content: None,
                head_summary: None,
                tail_summary: None,
                unified_diff: None,
                timestamp_ms: 1,
            },
        );

        assert!(dto.has_error);
        let value = serde_json::to_value(dto).expect("pending change dto should serialize");
        assert_eq!(value["hasError"], serde_json::json!(true));
        assert!(value.get("error").is_none());
    }

    #[test]
    fn pending_change_summary_omits_heavy_payload() {
        let scope = SessionChangeScope {
            session_id: SessionId::new("session-change-summary"),
            workspace_id: WorkspaceId::new("workspace-change-summary"),
            workspace_root: unique_temp_dir("magi-change-projection-summary"),
            execution_group_id: "execution-summary".to_string(),
            contributors: Vec::new(),
        };
        let dto = convert_pending(
            &scope,
            PendingChange {
                path: "summary.txt".to_string(),
                change_kind: ChangeKind::Modified,
                old_path: None,
                source: SourceKind::Tool,
                tool_call_id: None,
                worker_id: None,
                execution_group_id: None,
                content_kind: ContentKind::Text,
                size: 16,
                mime: Some("text/plain".to_string()),
                error: None,
                revertible: true,
                symlink_target: None,
                original_content: Some("old\n".to_string()),
                preview_content: Some("new\n".to_string()),
                head_summary: None,
                tail_summary: None,
                unified_diff: Some("@@ -1 +1 @@\n-old\n+new\n".to_string()),
                timestamp_ms: 1,
            },
        );

        assert_eq!(dto.additions, 1);
        assert_eq!(dto.deletions, 1);
        assert!(dto.diff.is_empty());
        assert!(dto.original_content.is_none());
        assert!(dto.preview_content.is_none());
        let value = serde_json::to_value(dto).expect("summary dto should serialize");
        assert!(value.get("diff").is_none());
        assert!(value.get("originalContent").is_none());
        assert!(value.get("previewContent").is_none());
    }

    #[test]
    fn safe_relative_path_rejects_parent_and_root() {
        assert!(safe_relative_path("../etc/passwd").is_err());
        assert!(safe_relative_path("/etc/passwd").is_err());
        assert!(safe_relative_path(r"C:\Windows\System32").is_err());
        assert!(safe_relative_path(r"\\server\share\file.txt").is_err());
        assert_eq!(safe_relative_path("foo/bar.txt").unwrap(), "foo/bar.txt");
    }

    #[test]
    fn safe_workspace_path_rejects_outside_workspace() {
        let root = unique_temp_dir("magi-change-projection-safe");
        let outside = unique_temp_dir("magi-change-projection-outside");
        let outside_file = outside.join("secret.txt");
        fs::write(&outside_file, "off-limits\n").expect("outside file should write");
        let abs = outside_file.to_string_lossy().into_owned();
        let err = safe_workspace_path(&root, &abs).unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
    }

    #[test]
    fn safe_workspace_path_accepts_host_path_ref() {
        let root = unique_temp_dir("magi-change-projection-path-ref");
        let file = root.join("path-ref.txt");
        fs::write(&file, "path-ref\n").expect("file should write");
        let path_ref = magi_core::HostPath::from_path(file.clone())
            .to_path_ref()
            .as_str()
            .to_string();

        let (resolved, relative) = safe_workspace_path(&root, &path_ref).expect("path ref");
        assert_eq!(resolved, file.canonicalize().unwrap());
        assert_eq!(relative, "path-ref.txt");
    }

    #[test]
    fn safe_workspace_path_uses_public_missing_path_error() {
        let root = unique_temp_dir("magi-change-projection-missing-path");
        let err = safe_workspace_path(&root, "missing.txt").unwrap_err();

        match err {
            ApiError::InvalidInput(message) => {
                assert_eq!(message, "路径不可读取或不存在");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
