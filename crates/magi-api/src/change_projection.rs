//! 文件变更投影：把 magi-snapshot 提供的 pending changes 转成前端 DTO。
//!
//! 唯一真相源 = magi-snapshot（BlobStore + BaselineIndex + ChangeLog）。
//! 本模块只做：
//!   1. 鉴权（路径安全 + 会话归属）；
//!   2. DTO 适配（PendingChange → PendingChangeDto，补 contributor / execution_group 元信息）。
//! 不再读 git，也不再写 git。

use crate::{errors::ApiError, state::ApiState};
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_snapshot::{ChangeKind, ContentKind, PendingChange, SourceKind};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct SessionChangeScope {
    pub session_id: SessionId,
    pub workspace_root: PathBuf,
    pub execution_group_id: String,
    pub contributors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChangeDto {
    pub file_path: String,
    pub snapshot_id: String,
    pub updated_at: UtcMillis,
    #[serde(rename = "type")]
    pub r#type: String,
    pub additions: usize,
    pub deletions: usize,
    pub diff: String,
    pub original_content: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    let execution_group_id = mission_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| session_execution_group_id(session_id));
    if let Some(requested_execution_group_id) = execution_group_id_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && requested_execution_group_id != execution_group_id
    {
        return Err(ApiError::InvalidInput(format!(
            "执行分组 {} 不属于当前会话 {}",
            requested_execution_group_id, session_id
        )));
    }

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
        workspace_root,
        execution_group_id,
        contributors,
    })
}

pub(crate) fn resolve_workspace_root_or_active(
    state: &ApiState,
    workspace_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    let ws_id = match workspace_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(id) => WorkspaceId::new(id),
        None => state
            .workspace_registry
            .active_workspace_id()
            .ok_or_else(|| {
                ApiError::InvalidInput("未指定 workspace_id 且没有活动 workspace".to_string())
            })?,
    };
    resolve_workspace_root(state, &ws_id)
}

/// 取出会话的 pending changes（来自 SnapshotSession）。
pub(crate) fn collect_session_pending_changes(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&str>,
) -> Result<Vec<PendingChangeDto>, ApiError> {
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
        return Ok(Vec::new());
    };
    if state
        .workspace_root_path(&Some(bound_workspace_id))
        .is_none()
    {
        return Ok(Vec::new());
    }
    // 快照账本启动是异步的（SnapshotLifecycleObserver::on_session_created 通过
    // tokio::spawn 触发 start_session）。在 bootstrap 等只读路径上，若账本尚未就绪，
    // 等价于"当前没有 pending changes"，直接返回空列表，避免与异步生命周期形成竞态。
    if state.snapshot_session(session_id).is_none() {
        return Ok(Vec::new());
    }
    let scope = resolve_session_change_scope(state, session_id, workspace_id, None)?;
    project_changes_from_snapshot(state, &scope)
}

/// 在已知 scope 的情况下投影：路由层（approve/revert）会先 resolve 一次 scope 再调用此函数。
pub(crate) fn project_changes_from_snapshot(
    state: &ApiState,
    scope: &SessionChangeScope,
) -> Result<Vec<PendingChangeDto>, ApiError> {
    let session = state.snapshot_session(&scope.session_id).ok_or_else(|| {
        ApiError::InvalidInput(format!(
            "会话 {} 的快照账本尚未就绪",
            scope.session_id.as_str()
        ))
    })?;
    let pending = session
        .pending_changes()
        .map_err(|e| ApiError::internal_assembly("读取快照变更失败", e))?;
    let mut out: Vec<PendingChangeDto> = pending
        .into_iter()
        .map(|change| convert_pending(scope, change))
        .collect();
    out.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    Ok(out)
}

fn convert_pending(scope: &SessionChangeScope, change: PendingChange) -> PendingChangeDto {
    let absolute_path = scope.workspace_root.join(&change.path);
    let preview_can_open_workspace_file = matches!(
        change.change_kind,
        ChangeKind::Added | ChangeKind::Modified | ChangeKind::Renamed
    ) && absolute_path.is_file();

    let unified_diff = change.unified_diff.clone().unwrap_or_default();
    let (additions, deletions) = count_diff_lines(&unified_diff);
    let r#type = change_kind_to_string(change.change_kind);
    let content_kind = content_kind_to_string(change.content_kind);
    let source_kind = source_kind_to_string(change.source);

    PendingChangeDto {
        file_path: change.path.clone(),
        snapshot_id: format!("{}:{}", scope.execution_group_id, change.path),
        updated_at: UtcMillis(change.timestamp_ms),
        r#type,
        additions,
        deletions,
        diff: unified_diff,
        original_content: change.original_content,
        preview_content: change.preview_content,
        preview_absolute_path: absolute_path.to_string_lossy().to_string(),
        preview_can_open_workspace_file,
        content_kind,
        size: change.size,
        mime: change.mime,
        source_kind,
        old_path: change.old_path,
        error: change.error,
        symlink_target: change.symlink_target,
        head_summary: change.head_summary,
        tail_summary: change.tail_summary,
        tool_call_id: change.tool_call_id,
        worker_id: change.worker_id,
        contributors: scope.contributors.clone(),
        execution_group_id: scope.execution_group_id.clone(),
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
    let path = Path::new(file_path);
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(ApiError::InvalidInput("路径不允许包含 ..".to_string()));
        }
        if matches!(component, std::path::Component::RootDir) {
            return Err(ApiError::InvalidInput("路径不允许为绝对路径".to_string()));
        }
    }
    Ok(file_path)
}

/// 把工作区相对路径或工作区下的绝对路径安全解析为工作区内的真实文件路径。
///
/// 返回值：`(canonical_absolute_path, workspace_relative_path)`。
/// - 相对路径：拒绝 `..` 与绝对前缀，再拼到 `workspace_root` 上。
/// - 绝对路径：直接使用。
/// 解析后必须 canonicalize 且仍位于 `workspace_root` 之内，否则视为越界并拒绝。
pub(crate) fn safe_workspace_path(
    workspace_root: &Path,
    file_path: &str,
) -> Result<(PathBuf, String), ApiError> {
    let trimmed = file_path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidInput("文件路径不能为空".to_string()));
    }
    let candidate = Path::new(trimmed);
    let candidate_abs = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        let rel = safe_relative_path(trimmed)?;
        workspace_root.join(rel)
    };
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|e| ApiError::internal_assembly("规范化工作区根目录失败", e))?;
    let canonical_path = candidate_abs
        .canonicalize()
        .map_err(|e| ApiError::internal_assembly("规范化文件路径失败", e))?;
    let relative = canonical_path
        .strip_prefix(&canonical_root)
        .map_err(|_| ApiError::InvalidInput("路径越出工作区边界".to_string()))?
        .to_string_lossy()
        .into_owned();
    Ok((canonical_path, relative))
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
    Ok(PathBuf::from(workspace.root_path.as_str()))
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
        let changes = collect_session_pending_changes(&state, &session_id, None)
            .expect("missing workspace binding should be treated as empty");
        assert!(changes.is_empty());
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
        let changes = collect_session_pending_changes(&state, &sid, Some("ws-no-snapshot"))
            .expect("snapshot not yet started should be treated as empty");
        assert!(changes.is_empty());
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

        let changes = collect_session_pending_changes(&state, &sid, Some("ws-snapshot"))
            .expect("pending changes should collect");
        let by_path: std::collections::BTreeMap<_, _> = changes
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
        for change in &changes {
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

        let changes = collect_session_pending_changes(&state, &sid, Some("ws-rename"))
            .expect("pending changes should collect");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].file_path, "after.txt");
        assert_eq!(changes[0].old_path.as_deref(), Some("before.txt"));
        assert_eq!(changes[0].r#type, "rename");
        assert_eq!(changes[0].execution_group_id, "mission-rename");
    }

    #[test]
    fn safe_relative_path_rejects_parent_and_root() {
        assert!(safe_relative_path("../etc/passwd").is_err());
        assert!(safe_relative_path("/etc/passwd").is_err());
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
}
