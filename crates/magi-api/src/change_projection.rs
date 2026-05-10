use crate::{errors::ApiError, state::ApiState};
use magi_core::{MissionId, SessionId, UtcMillis, WorkspaceId};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone)]
pub(crate) struct SessionChangeScope {
    pub session_id: SessionId,
    pub workspace_root: PathBuf,
    pub execution_group_id: String,
    pub allowed_files: BTreeSet<String>,
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
    pub original_content: String,
    pub preview_content: String,
    pub preview_absolute_path: String,
    pub preview_can_open_workspace_file: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<String>,
    pub execution_group_id: String,
}

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
    let mut allowed_files = match mission_id.as_ref() {
        Some(mission_id) => collect_mission_output_files(state, mission_id, &workspace_root)?,
        None => BTreeSet::new(),
    };
    collect_canonical_session_tool_files(state, session_id, &workspace_root, &mut allowed_files);
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
        allowed_files,
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
            .is_some_and(|bound_workspace_id| bound_workspace_id.as_str() != requested_workspace_id)
    {
        return Err(ApiError::InvalidInput(format!(
            "会话 {} 不属于 workspace {}",
            session_id, requested_workspace_id
        )));
    }
    if bound_workspace_id.is_none() {
        return Ok(Vec::new());
    }
    let scope = resolve_session_change_scope(state, session_id, workspace_id, None)?;
    pending_changes_for_scope(&scope)
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

pub(crate) fn run_git(workspace_root: &Path, args: &[&str]) -> Result<String, ApiError> {
    run_git_with_allowed_statuses(workspace_root, args, &[0])
}

pub(crate) fn run_git_diff(workspace_root: &Path, args: &[&str]) -> Result<String, ApiError> {
    run_git_readonly_with_empty_baseline(workspace_root, args, &[0])
}

pub(crate) fn run_git_restore_files(
    workspace_root: &Path,
    files: &[String],
) -> Result<(), ApiError> {
    let mut tracked_files = Vec::new();
    for file in files {
        if file_exists_in_head(workspace_root, file)? {
            tracked_files.push(file.clone());
            continue;
        }
        run_git_with_allowed_statuses(
            workspace_root,
            &["rm", "--cached", "--force", "--ignore-unmatch", "--", file],
            &[0],
        )?;
        remove_worktree_path(&workspace_root.join(file))?;
    }
    if !tracked_files.is_empty() {
        let mut args = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
        let file_refs = tracked_files.iter().map(String::as_str).collect::<Vec<_>>();
        args.extend(file_refs);
        run_git(workspace_root, &args)?;
    }
    Ok(())
}

pub(crate) fn run_git_add_files(workspace_root: &Path, files: &[String]) -> Result<(), ApiError> {
    if files.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "--"];
    let file_refs = files.iter().map(String::as_str).collect::<Vec<_>>();
    args.extend(file_refs);
    run_git(workspace_root, &args)?;
    Ok(())
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

fn collect_mission_output_files(
    state: &ApiState,
    mission_id: &MissionId,
    workspace_root: &Path,
) -> Result<BTreeSet<String>, ApiError> {
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("changes scope", "task_store 未配置"))?;
    let mut files = BTreeSet::new();
    for task in task_store.get_tasks_by_mission(mission_id) {
        for output_ref in &task.output_refs {
            collect_output_ref_files(&mut files, output_ref, &workspace_root);
        }
    }
    Ok(files)
}

fn session_execution_group_id(session_id: &SessionId) -> String {
    format!("session:{}", session_id)
}

fn collect_canonical_session_tool_files(
    state: &ApiState,
    session_id: &SessionId,
    workspace_root: &Path,
    files: &mut BTreeSet<String>,
) {
    for turn in state.session_store.canonical_turns_for_session(session_id) {
        for item in turn.items {
            let Some(tool) = item.tool.as_ref() else {
                continue;
            };
            collect_tool_call_files(
                files,
                &tool.name,
                tool.arguments.as_ref(),
                tool.result.as_ref(),
                workspace_root,
            );
        }
    }
}

fn collect_output_ref_files(files: &mut BTreeSet<String>, output_ref: &str, workspace_root: &Path) {
    if let Some(rel) = output_ref
        .strip_prefix("file:")
        .and_then(|path| normalize_output_file_path(path, workspace_root))
    {
        files.insert(rel);
        return;
    }

    let Ok(payload) = serde_json::from_str::<Value>(output_ref) else {
        return;
    };
    let Some(blocks) = payload.get("blocks").and_then(Value::as_array) else {
        return;
    };
    for block in blocks {
        let Some(tool_call) = block.get("toolCall").and_then(Value::as_object) else {
            continue;
        };
        let tool_name = tool_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = tool_call.get("arguments");
        let result = tool_call.get("result");
        collect_tool_call_files(files, tool_name, arguments, result, workspace_root);
    }
}

fn collect_tool_call_files(
    files: &mut BTreeSet<String>,
    tool_name: &str,
    arguments: Option<&Value>,
    result: Option<&Value>,
    workspace_root: &Path,
) {
    let result_payload = match result {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
        Some(value) if value.is_object() => Some(value.clone()),
        _ => None,
    };

    let mut insert_path = |path: Option<&str>| {
        if let Some(rel) = path.and_then(|value| normalize_output_file_path(value, workspace_root))
        {
            files.insert(rel);
        }
    };

    collect_result_changed_paths(&mut insert_path, result_payload.as_ref());

    match tool_name {
        "file_write" | "file_patch" | "file_remove" | "file_mkdir" => {
            insert_path(
                result_payload
                    .as_ref()
                    .and_then(|value| value.get("path"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        arguments
                            .and_then(|value| value.get("path"))
                            .and_then(Value::as_str)
                    }),
            );
        }
        "file_copy" => {
            insert_path(
                result_payload
                    .as_ref()
                    .and_then(|value| value.get("destination"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        arguments
                            .and_then(|value| value.get("destination"))
                            .or_else(|| arguments.and_then(|value| value.get("dst")))
                            .or_else(|| arguments.and_then(|value| value.get("dest")))
                            .or_else(|| arguments.and_then(|value| value.get("to")))
                            .and_then(Value::as_str)
                    }),
            );
        }
        "file_move" => {
            insert_path(
                result_payload
                    .as_ref()
                    .and_then(|value| value.get("source"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        arguments
                            .and_then(|value| value.get("source"))
                            .or_else(|| arguments.and_then(|value| value.get("src")))
                            .or_else(|| arguments.and_then(|value| value.get("from")))
                            .and_then(Value::as_str)
                    }),
            );
            insert_path(
                result_payload
                    .as_ref()
                    .and_then(|value| value.get("destination"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        arguments
                            .and_then(|value| value.get("destination"))
                            .or_else(|| arguments.and_then(|value| value.get("dst")))
                            .or_else(|| arguments.and_then(|value| value.get("dest")))
                            .or_else(|| arguments.and_then(|value| value.get("to")))
                            .and_then(Value::as_str)
                    }),
            );
        }
        _ => {}
    }
}

fn collect_result_changed_paths(
    insert_path: &mut impl FnMut(Option<&str>),
    result_payload: Option<&Value>,
) {
    let Some(payload) = result_payload else {
        return;
    };
    for field in [
        "changed_paths",
        "changedPaths",
        "changed_files",
        "changedFiles",
        "files",
        "paths",
    ] {
        let Some(values) = payload.get(field).and_then(Value::as_array) else {
            continue;
        };
        for value in values {
            if let Some(path) = value.as_str() {
                insert_path(Some(path));
                continue;
            }
            if let Some(path) = value
                .get("path")
                .or_else(|| value.get("file_path"))
                .or_else(|| value.get("filePath"))
                .and_then(Value::as_str)
            {
                insert_path(Some(path));
            }
        }
    }
}

fn normalize_output_file_path(path: &str, workspace_root: &Path) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = Path::new(trimmed);
    let relative = if candidate.is_absolute() {
        candidate
            .strip_prefix(workspace_root)
            .ok()?
            .to_string_lossy()
            .to_string()
    } else {
        trimmed.to_string()
    };
    safe_relative_path(&relative).ok().map(str::to_string)
}

pub(crate) fn session_change_scope_allows_path(
    scope: &SessionChangeScope,
    file_path: &str,
) -> bool {
    let Ok(rel) = safe_relative_path(file_path) else {
        return false;
    };
    scope
        .allowed_files
        .iter()
        .any(|allowed| scope_entry_allows_path(allowed, rel))
}

fn scope_entry_allows_path(scope_entry: &str, file_path: &str) -> bool {
    let normalized_entry = scope_entry.trim_end_matches('/');
    if normalized_entry.is_empty() {
        return false;
    }
    file_path == normalized_entry
        || file_path
            .strip_prefix(normalized_entry)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn pending_changes_for_scope(
    scope: &SessionChangeScope,
) -> Result<Vec<PendingChangeDto>, ApiError> {
    if scope.allowed_files.is_empty() {
        return Ok(Vec::new());
    }
    if !workspace_is_git_worktree(&scope.workspace_root) {
        return pending_changes_for_non_git_scope(scope);
    }

    let mut args = vec!["status", "--porcelain=v1", "--untracked-files=all", "--"];
    let file_refs = scope
        .allowed_files
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    args.extend(file_refs);
    let output = run_git(&scope.workspace_root, &args)?;

    let mut changes = output
        .lines()
        .filter_map(parse_git_status_line)
        .filter(|(status_code, _)| status_has_unapproved_changes(status_code))
        .filter(|(_, file_path)| session_change_scope_allows_path(scope, file_path))
        .map(|(status_code, file_path)| build_pending_change(scope, &status_code, &file_path))
        .collect::<Result<Vec<_>, _>>()?;
    changes.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    Ok(changes)
}

fn pending_changes_for_non_git_scope(
    scope: &SessionChangeScope,
) -> Result<Vec<PendingChangeDto>, ApiError> {
    let mut changes = scope
        .allowed_files
        .iter()
        .filter(|file_path| session_change_scope_allows_path(scope, file_path))
        .filter_map(|file_path| build_non_git_pending_change(scope, file_path).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    changes.sort_by(|left, right| left.file_path.cmp(&right.file_path));
    Ok(changes)
}

fn parse_git_status_line(line: &str) -> Option<(String, String)> {
    if line.len() < 4 {
        return None;
    }
    let status_code = line.get(..2)?.to_string();
    let path_segment = line.get(3..)?.trim();
    if path_segment.is_empty() {
        return None;
    }
    let file_path = path_segment
        .rsplit(" -> ")
        .next()
        .map(str::trim)
        .filter(|path| !path.is_empty())?
        .to_string();
    Some((status_code, file_path))
}

fn status_has_unapproved_changes(status_code: &str) -> bool {
    if status_code == "??" {
        return true;
    }
    let mut chars = status_code.chars();
    let _index_status = chars.next().unwrap_or(' ');
    let worktree_status = chars.next().unwrap_or(' ');
    worktree_status != ' '
}

fn build_pending_change(
    scope: &SessionChangeScope,
    status_code: &str,
    file_path: &str,
) -> Result<PendingChangeDto, ApiError> {
    let relative_path = safe_relative_path(file_path)?.to_string();
    let absolute_path = scope.workspace_root.join(&relative_path);
    let preview_can_open_workspace_file = absolute_path.is_file();
    let preview_content = read_file_lossy(&absolute_path);
    let original_content = read_head_file(&scope.workspace_root, &relative_path);
    let change_type = infer_change_type(
        status_code,
        &original_content,
        preview_can_open_workspace_file,
    );
    let diff = build_diff(
        &scope.workspace_root,
        &relative_path,
        &absolute_path,
        &change_type,
        preview_can_open_workspace_file,
    )?;
    let (additions, deletions) = match read_numstat(&scope.workspace_root, &relative_path)? {
        Some((additions, deletions)) => (additions, deletions),
        None => fallback_numstat(&change_type, &original_content, &preview_content),
    };
    let updated_at = file_updated_at(&absolute_path).unwrap_or_else(UtcMillis::now);

    Ok(PendingChangeDto {
        file_path: relative_path.clone(),
        snapshot_id: format!("{}:{}", scope.execution_group_id, relative_path),
        updated_at,
        r#type: change_type,
        additions,
        deletions,
        diff,
        original_content,
        preview_content,
        preview_absolute_path: absolute_path.to_string_lossy().to_string(),
        preview_can_open_workspace_file,
        contributors: scope.contributors.clone(),
        execution_group_id: scope.execution_group_id.clone(),
    })
}

fn infer_change_type(status_code: &str, original_content: &str, preview_exists: bool) -> String {
    if status_code == "??"
        || status_code.contains('A')
        || (!preview_exists && original_content.is_empty())
    {
        return "add".to_string();
    }
    if status_code.contains('D') || (!preview_exists && !original_content.is_empty()) {
        return "delete".to_string();
    }
    "modify".to_string()
}

fn build_non_git_pending_change(
    scope: &SessionChangeScope,
    file_path: &str,
) -> Result<Option<PendingChangeDto>, ApiError> {
    let relative_path = safe_relative_path(file_path)?.to_string();
    let absolute_path = scope.workspace_root.join(&relative_path);
    if !absolute_path.is_file() {
        return Ok(None);
    }
    let preview_content = read_file_lossy(&absolute_path);
    let additions = count_text_lines(&preview_content);
    let updated_at = file_updated_at(&absolute_path).unwrap_or_else(UtcMillis::now);
    Ok(Some(PendingChangeDto {
        file_path: relative_path.clone(),
        snapshot_id: format!("{}:{}", scope.execution_group_id, relative_path),
        updated_at,
        r#type: "add".to_string(),
        additions,
        deletions: 0,
        diff: build_added_file_diff(&relative_path, &preview_content),
        original_content: String::new(),
        preview_content,
        preview_absolute_path: absolute_path.to_string_lossy().to_string(),
        preview_can_open_workspace_file: true,
        contributors: scope.contributors.clone(),
        execution_group_id: scope.execution_group_id.clone(),
    }))
}

fn build_diff(
    workspace_root: &Path,
    file_path: &str,
    absolute_path: &Path,
    change_type: &str,
    preview_exists: bool,
) -> Result<String, ApiError> {
    if change_type == "add" && preview_exists {
        return run_git_with_allowed_statuses(
            workspace_root,
            &[
                "diff",
                "--no-index",
                "--",
                "/dev/null",
                absolute_path.to_string_lossy().as_ref(),
            ],
            &[0, 1],
        );
    }
    run_git(workspace_root, &["diff", "HEAD", "--", file_path])
}

fn build_added_file_diff(file_path: &str, content: &str) -> String {
    let mut diff = format!("--- /dev/null\n+++ b/{file_path}\n");
    for line in content.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    if content.is_empty() {
        diff.push_str("+\n");
    }
    diff
}

fn read_numstat(
    workspace_root: &Path,
    file_path: &str,
) -> Result<Option<(usize, usize)>, ApiError> {
    let output = run_git(
        workspace_root,
        &["diff", "--numstat", "HEAD", "--", file_path],
    )?;
    let Some(line) = output.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let mut parts = line.split('\t');
    let additions = parts.next().and_then(|value| value.parse::<usize>().ok());
    let deletions = parts.next().and_then(|value| value.parse::<usize>().ok());
    Ok(additions.zip(deletions))
}

fn fallback_numstat(
    change_type: &str,
    original_content: &str,
    preview_content: &str,
) -> (usize, usize) {
    match change_type {
        "add" => (count_text_lines(preview_content), 0),
        "delete" => (0, count_text_lines(original_content)),
        _ => (
            count_text_lines(preview_content),
            count_text_lines(original_content),
        ),
    }
}

fn read_head_file(workspace_root: &Path, file_path: &str) -> String {
    run_git_with_allowed_statuses(
        workspace_root,
        &["show", &format!("HEAD:{file_path}")],
        &[0],
    )
    .unwrap_or_default()
}

fn read_file_lossy(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

fn count_text_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count().max(1)
    }
}

fn file_updated_at(path: &Path) -> Option<UtcMillis> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(UtcMillis(duration.as_millis() as u64))
}

fn file_exists_in_head(workspace_root: &Path, file_path: &str) -> Result<bool, ApiError> {
    let output = Command::new("git")
        .args(["cat-file", "-e", &format!("HEAD:{file_path}")])
        .current_dir(workspace_root)
        .output()
        .map_err(|error| ApiError::internal_assembly("检查 HEAD 文件失败", error))?;
    Ok(output.status.success())
}

fn workspace_is_git_worktree(workspace_root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn remove_worktree_path(path: &Path) -> Result<(), ApiError> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)
            .map_err(|error| ApiError::internal_assembly("删除未跟踪目录失败", error))?;
    } else {
        fs::remove_file(path)
            .map_err(|error| ApiError::internal_assembly("删除未跟踪文件失败", error))?;
    }
    Ok(())
}

fn run_git_with_allowed_statuses(
    workspace_root: &Path,
    args: &[&str],
    allowed_statuses: &[i32],
) -> Result<String, ApiError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
        .map_err(|error| ApiError::internal_assembly("执行 git 命令失败", error))?;
    let status_code = output.status.code().unwrap_or(-1);
    if allowed_statuses.contains(&status_code) {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Err(ApiError::internal_assembly("git 命令执行出错", stderr))
}

fn run_git_readonly_with_empty_baseline(
    workspace_root: &Path,
    args: &[&str],
    allowed_statuses: &[i32],
) -> Result<String, ApiError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
        .map_err(|error| ApiError::internal_assembly("执行 git 命令失败", error))?;
    let status_code = output.status.code().unwrap_or(-1);
    if allowed_statuses.contains(&status_code) {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if is_missing_git_diff_baseline(&stderr) {
        return Ok(String::new());
    }
    Err(ApiError::internal_assembly("git 命令执行出错", stderr))
}

fn is_missing_git_diff_baseline(stderr: &str) -> bool {
    stderr.contains("Not a git repository")
        || stderr.contains("unknown revision or path not in the working tree")
        || stderr.contains("bad revision 'HEAD'")
        || stderr.contains("ambiguous argument 'HEAD'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use magi_core::{
        AbsolutePath, ExecutionOwnership, SessionId, TaskId, TaskKind, TaskStatus, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{ActiveExecutionTurn, ActiveExecutionTurnItem, SessionStore};
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_REPO_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn build_test_repo() -> String {
        let unique_suffix = TEST_REPO_COUNTER.fetch_add(1, Ordering::Relaxed);
        let repo_root = std::env::temp_dir().join(format!(
            "magi-change-projection-test-{}-{}-{}",
            std::process::id(),
            UtcMillis::now().0,
            unique_suffix
        ));
        fs::create_dir_all(repo_root.join("tmp")).expect("tmp dir should create");
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
        fs::write(repo_root.join("tracked-a.txt"), "alpha\n").expect("tracked a should write");
        fs::write(repo_root.join("tracked-b.txt"), "beta\n").expect("tracked b should write");
        Command::new("git")
            .args(["add", "--", "tracked-a.txt", "tracked-b.txt"])
            .current_dir(&repo_root)
            .output()
            .expect("git add should run");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo_root)
            .output()
            .expect("git commit should run");
        fs::write(repo_root.join("tracked-a.txt"), "alpha changed\n")
            .expect("tracked a should update");
        fs::write(repo_root.join("tmp/new-a.txt"), "new file\nsecond line\n")
            .expect("new file should write");
        repo_root.to_string_lossy().to_string()
    }

    fn build_state_with_repo(repo_root: &str, output_refs: Vec<String>) -> ApiState {
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

        let session_id = SessionId::new("session-a");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "会话 A",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state.session_store.bind_execution_ownership(
            session_id,
            ExecutionOwnership {
                session_id: Some(SessionId::new("session-a")),
                workspace_id: Some(workspace_id.clone()),
                mission_id: Some(MissionId::new("mission-a")),
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
            output_refs,
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        });

        state
    }

    fn build_state_with_session_workspace(
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

    fn canonical_file_tool_item(
        item_seq: usize,
        call_id: &str,
        tool_name: &str,
        arguments: Value,
        result: Value,
    ) -> ActiveExecutionTurnItem {
        ActiveExecutionTurnItem {
            item_id: format!("turn-item-tool-{call_id}"),
            item_seq,
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
        }
    }

    fn upsert_canonical_file_tool_turn(
        state: &ApiState,
        session_id: &SessionId,
        items: Vec<ActiveExecutionTurnItem>,
    ) {
        let now = UtcMillis::now();
        state
            .session_store
            .upsert_current_turn(
                session_id.clone(),
                ActiveExecutionTurn {
                    turn_id: "turn-session-file-tools".to_string(),
                    turn_seq: now.0,
                    accepted_at: now,
                    completed_at: Some(now),
                    status: "completed".to_string(),
                    user_message: Some("文件工具".to_string()),
                    items,
                    worker_lanes: Vec::new(),
                },
            )
            .expect("canonical file tool turn should upsert");
    }

    #[test]
    fn session_pending_changes_include_tracked_and_untracked_files() {
        let repo_root = build_test_repo();
        let state = build_state_with_repo(
            &repo_root,
            vec![
                "file:tracked-a.txt".to_string(),
                "file:tmp/new-a.txt".to_string(),
            ],
        );

        let changes = collect_session_pending_changes(
            &state,
            &SessionId::new("session-a"),
            Some("workspace-session-scope"),
        )
        .expect("pending changes should collect");

        assert_eq!(changes.len(), 2);
        let tracked = changes
            .iter()
            .find(|change| change.file_path == "tracked-a.txt")
            .expect("tracked change should exist");
        assert_eq!(tracked.r#type, "modify");
        assert!(tracked.diff.contains("alpha changed"));

        let added = changes
            .iter()
            .find(|change| change.file_path == "tmp/new-a.txt")
            .expect("new file change should exist");
        assert_eq!(added.r#type, "add");
        assert_eq!(added.additions, 2);
        assert!(added.diff.contains("new file"));
    }

    #[test]
    fn session_pending_changes_extract_files_from_tool_call_blocks() {
        let repo_root = build_test_repo();
        let output_ref = serde_json::json!({
            "blocks": [
                {
                    "type": "tool_call",
                    "toolCall": {
                        "name": "file_write",
                        "arguments": {
                            "path": Path::new(&repo_root).join("tmp/new-a.txt").to_string_lossy().to_string(),
                            "content": "new file\\nsecond line\\n",
                        },
                        "result": serde_json::json!({
                            "tool": "file_write",
                            "status": "succeeded",
                            "path": Path::new(&repo_root).join("tmp/new-a.txt").to_string_lossy().to_string(),
                        }).to_string(),
                    }
                },
                {
                    "type": "tool_call",
                    "toolCall": {
                        "name": "file_patch",
                        "arguments": {
                            "path": Path::new(&repo_root).join("tracked-a.txt").to_string_lossy().to_string(),
                        },
                        "result": serde_json::json!({
                            "tool": "file_patch",
                            "status": "succeeded",
                            "path": Path::new(&repo_root).join("tracked-a.txt").to_string_lossy().to_string(),
                        }).to_string(),
                    }
                }
            ]
        })
        .to_string();
        let state = build_state_with_repo(&repo_root, vec![output_ref]);

        let changes = collect_session_pending_changes(
            &state,
            &SessionId::new("session-a"),
            Some("workspace-session-scope"),
        )
        .expect("pending changes should collect from tool blocks");

        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tracked-a.txt")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tmp/new-a.txt")
        );
    }

    #[test]
    fn session_pending_changes_collects_canonical_file_tool_changes_without_execution_group() {
        let repo_root = build_test_repo();
        let session_id = SessionId::new("session-canonical-file-tools");
        let workspace_id = WorkspaceId::new("workspace-canonical-file-tools");
        let state = build_state_with_session_workspace(
            &repo_root,
            session_id.clone(),
            workspace_id.clone(),
        );
        let repo_path = Path::new(&repo_root);
        upsert_canonical_file_tool_turn(
            &state,
            &session_id,
            vec![
                canonical_file_tool_item(
                    1,
                    "call-file-write",
                    "file_write",
                    serde_json::json!({
                        "path": repo_path.join("tmp/new-a.txt").to_string_lossy().to_string(),
                        "content": "new file\nsecond line\n",
                    }),
                    serde_json::json!({
                        "tool": "file_write",
                        "status": "succeeded",
                        "path": repo_path.join("tmp/new-a.txt").to_string_lossy().to_string(),
                    }),
                ),
                canonical_file_tool_item(
                    2,
                    "call-file-patch",
                    "file_patch",
                    serde_json::json!({
                        "path": repo_path.join("tracked-a.txt").to_string_lossy().to_string(),
                    }),
                    serde_json::json!({
                        "tool": "file_patch",
                        "status": "succeeded",
                        "path": repo_path.join("tracked-a.txt").to_string_lossy().to_string(),
                    }),
                ),
            ],
        );

        let changes =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("canonical session file tools should collect pending changes");

        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tracked-a.txt")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tmp/new-a.txt")
        );
        assert!(
            changes
                .iter()
                .all(|change| change.execution_group_id == "session:session-canonical-file-tools")
        );
    }

    #[test]
    fn session_pending_changes_collects_non_git_file_tool_changes_without_warning_error() {
        let workspace_root = std::env::temp_dir().join(format!(
            "magi-change-projection-non-git-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::create_dir_all(workspace_root.join("tmp")).expect("workspace should create");
        let session_id = SessionId::new("session-canonical-non-git-file-tools");
        let workspace_id = WorkspaceId::new("workspace-canonical-non-git-file-tools");
        let state = build_state_with_session_workspace(
            &workspace_root.to_string_lossy(),
            session_id.clone(),
            workspace_id.clone(),
        );
        let target = workspace_root.join("tmp/new-a.txt");
        fs::write(&target, "new file\nsecond line\n").expect("new file should write");
        upsert_canonical_file_tool_turn(
            &state,
            &session_id,
            vec![canonical_file_tool_item(
                1,
                "call-file-write-non-git",
                "file_write",
                serde_json::json!({
                    "path": target.to_string_lossy().to_string(),
                    "content": "new file\nsecond line\n",
                }),
                serde_json::json!({
                    "tool": "file_write",
                    "status": "succeeded",
                    "path": target.to_string_lossy().to_string(),
                }),
            )],
        );

        let changes =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("non-git pending changes should not require git");

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].file_path, "tmp/new-a.txt");
        assert_eq!(changes[0].r#type, "add");
        assert_eq!(changes[0].additions, 2);
        assert!(changes[0].diff.contains("+new file"));
    }

    #[test]
    fn session_pending_changes_ignores_canonical_file_tools_outside_workspace() {
        let repo_root = build_test_repo();
        let session_id = SessionId::new("session-canonical-external-file");
        let workspace_id = WorkspaceId::new("workspace-canonical-external-file");
        let state = build_state_with_session_workspace(
            &repo_root,
            session_id.clone(),
            workspace_id.clone(),
        );
        let external_path = std::env::temp_dir().join(format!(
            "magi-change-projection-external-{}-{}.txt",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::write(&external_path, "outside workspace\n").expect("external file should write");
        upsert_canonical_file_tool_turn(
            &state,
            &session_id,
            vec![canonical_file_tool_item(
                1,
                "call-external-file-write",
                "file_write",
                serde_json::json!({
                    "path": external_path.to_string_lossy().to_string(),
                    "content": "outside workspace\n",
                }),
                serde_json::json!({
                    "tool": "file_write",
                    "status": "succeeded",
                    "path": external_path.to_string_lossy().to_string(),
                }),
            )],
        );

        let changes =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("outside workspace file should be ignored");

        assert!(
            changes.is_empty(),
            "canonical file tool paths outside the workspace must not enter the change scope"
        );
        let _ = fs::remove_file(external_path);
    }

    #[test]
    fn session_pending_changes_collects_canonical_add_modify_and_delete() {
        let repo_root = build_test_repo();
        let session_id = SessionId::new("session-canonical-all-file-actions");
        let workspace_id = WorkspaceId::new("workspace-canonical-all-file-actions");
        let state = build_state_with_session_workspace(
            &repo_root,
            session_id.clone(),
            workspace_id.clone(),
        );
        let repo_path = Path::new(&repo_root);
        fs::remove_file(repo_path.join("tracked-b.txt")).expect("tracked file should delete");
        upsert_canonical_file_tool_turn(
            &state,
            &session_id,
            vec![
                canonical_file_tool_item(
                    1,
                    "call-file-write-add",
                    "file_write",
                    serde_json::json!({
                        "path": repo_path.join("tmp/new-a.txt").to_string_lossy().to_string(),
                        "content": "new file\nsecond line\n",
                    }),
                    serde_json::json!({
                        "tool": "file_write",
                        "status": "succeeded",
                        "path": repo_path.join("tmp/new-a.txt").to_string_lossy().to_string(),
                    }),
                ),
                canonical_file_tool_item(
                    2,
                    "call-file-write-modify",
                    "file_write",
                    serde_json::json!({
                        "path": repo_path.join("tracked-a.txt").to_string_lossy().to_string(),
                        "content": "alpha changed\n",
                    }),
                    serde_json::json!({
                        "tool": "file_write",
                        "status": "succeeded",
                        "path": repo_path.join("tracked-a.txt").to_string_lossy().to_string(),
                    }),
                ),
                canonical_file_tool_item(
                    3,
                    "call-file-remove-delete",
                    "file_remove",
                    serde_json::json!({
                        "path": repo_path.join("tracked-b.txt").to_string_lossy().to_string(),
                    }),
                    serde_json::json!({
                        "tool": "file_remove",
                        "status": "succeeded",
                        "path": repo_path.join("tracked-b.txt").to_string_lossy().to_string(),
                    }),
                ),
            ],
        );

        let changes =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("canonical file actions should collect pending changes");

        assert_eq!(changes.len(), 3);
        assert_eq!(
            changes
                .iter()
                .find(|change| change.file_path == "tmp/new-a.txt")
                .expect("added file should be present")
                .r#type,
            "add"
        );
        assert_eq!(
            changes
                .iter()
                .find(|change| change.file_path == "tracked-a.txt")
                .expect("modified file should be present")
                .r#type,
            "modify"
        );
        let deleted = changes
            .iter()
            .find(|change| change.file_path == "tracked-b.txt")
            .expect("deleted file should be present");
        assert_eq!(deleted.r#type, "delete");
        assert!(deleted.deletions > 0);
    }

    #[test]
    fn session_pending_changes_include_children_under_directory_scope() {
        let repo_root = build_test_repo();
        let session_id = SessionId::new("session-canonical-directory-output");
        let workspace_id = WorkspaceId::new("workspace-canonical-directory-output");
        let state = build_state_with_session_workspace(
            &repo_root,
            session_id.clone(),
            workspace_id.clone(),
        );
        let repo_path = Path::new(&repo_root);
        let copied_dir = repo_path.join("tmp/copied-dir");
        fs::create_dir_all(&copied_dir).expect("copied dir should create");
        fs::write(copied_dir.join("child-a.txt"), "child a\n").expect("child a should write");
        fs::write(copied_dir.join("child-b.txt"), "child b\n").expect("child b should write");
        upsert_canonical_file_tool_turn(
            &state,
            &session_id,
            vec![canonical_file_tool_item(
                1,
                "call-file-copy-dir",
                "file_copy",
                serde_json::json!({
                    "source": repo_path.join("src-dir").to_string_lossy().to_string(),
                    "destination": copied_dir.to_string_lossy().to_string(),
                }),
                serde_json::json!({
                    "tool": "file_copy",
                    "status": "succeeded",
                    "destination": copied_dir.to_string_lossy().to_string(),
                    "is_directory": true,
                }),
            )],
        );

        let changes =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("directory scoped tool output should collect child files");

        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tmp/copied-dir/child-a.txt")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tmp/copied-dir/child-b.txt")
        );
    }

    #[test]
    fn session_pending_changes_collects_shell_exec_changed_paths() {
        let repo_root = build_test_repo();
        let session_id = SessionId::new("session-canonical-shell-changes");
        let workspace_id = WorkspaceId::new("workspace-canonical-shell-changes");
        let state = build_state_with_session_workspace(
            &repo_root,
            session_id.clone(),
            workspace_id.clone(),
        );
        let repo_path = Path::new(&repo_root);
        fs::remove_file(repo_path.join("tracked-b.txt")).expect("tracked file should delete");
        upsert_canonical_file_tool_turn(
            &state,
            &session_id,
            vec![canonical_file_tool_item(
                1,
                "call-shell-change",
                "shell_exec",
                serde_json::json!({
                    "command": "modify files",
                }),
                serde_json::json!({
                    "tool": "shell_exec",
                    "status": "succeeded",
                    "changed_paths": [
                        "tracked-a.txt",
                        "tracked-b.txt",
                        "tmp/new-a.txt"
                    ],
                }),
            )],
        );

        let changes =
            collect_session_pending_changes(&state, &session_id, Some(workspace_id.as_str()))
                .expect("shell changed paths should collect pending changes");

        assert_eq!(changes.len(), 3);
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tracked-a.txt" && change.r#type == "modify")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tracked-b.txt" && change.r#type == "delete")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.file_path == "tmp/new-a.txt" && change.r#type == "add")
        );
    }

    #[test]
    fn session_pending_changes_exclude_pure_staged_files() {
        let repo_root = build_test_repo();
        let repo_path = Path::new(&repo_root);
        Command::new("git")
            .args(["add", "--", "tracked-a.txt", "tmp/new-a.txt"])
            .current_dir(repo_path)
            .output()
            .expect("git add should run");
        let state = build_state_with_repo(
            &repo_root,
            vec![
                "file:tracked-a.txt".to_string(),
                "file:tmp/new-a.txt".to_string(),
            ],
        );

        let changes = collect_session_pending_changes(
            &state,
            &SessionId::new("session-a"),
            Some("workspace-session-scope"),
        )
        .expect("pending changes should collect");

        assert!(
            changes.is_empty(),
            "staged-only files should not remain pending"
        );
    }

    #[test]
    fn session_pending_changes_keep_mixed_staged_and_unstaged_files() {
        let repo_root = build_test_repo();
        let repo_path = Path::new(&repo_root);
        Command::new("git")
            .args(["add", "--", "tracked-a.txt"])
            .current_dir(repo_path)
            .output()
            .expect("git add should run");
        fs::write(repo_path.join("tracked-a.txt"), "alpha changed twice\n")
            .expect("tracked file should keep unstaged delta");
        let state = build_state_with_repo(&repo_root, vec!["file:tracked-a.txt".to_string()]);

        let changes = collect_session_pending_changes(
            &state,
            &SessionId::new("session-a"),
            Some("workspace-session-scope"),
        )
        .expect("pending changes should collect");

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].file_path, "tracked-a.txt");
        assert!(changes[0].diff.contains("alpha changed twice"));
    }

    #[test]
    fn session_pending_changes_returns_empty_without_execution_group() {
        let repo_root = build_test_repo();
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store.clone(),
            workspace_store.clone(),
            governance,
        )
        .with_task_store(Arc::new(TaskStore::new()));

        let workspace_id = WorkspaceId::new("workspace-session-empty-scope");
        state
            .workspace_registry
            .register(workspace_id.clone(), AbsolutePath::new(&repo_root))
            .expect("workspace should register");
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("session-empty-scope"),
                "空执行组会话",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");

        let changes = collect_session_pending_changes(
            &state,
            &SessionId::new("session-empty-scope"),
            Some(workspace_id.as_str()),
        )
        .expect("missing execution group should be treated as empty pending changes");

        assert!(changes.is_empty());
    }

    #[test]
    fn session_pending_changes_returns_empty_without_bound_workspace() {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store.clone(),
            workspace_store,
            governance,
        )
        .with_task_store(Arc::new(TaskStore::new()));

        let session_id = SessionId::new("session-no-workspace");
        state
            .session_store
            .create_session(session_id.clone(), "未绑定 workspace 的会话")
            .expect("session should create");
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                mission_id: Some(MissionId::new("mission-no-workspace")),
                ..ExecutionOwnership::default()
            },
        );

        let changes = collect_session_pending_changes(&state, &session_id, None)
            .expect("missing workspace binding should be treated as empty pending changes");

        assert!(changes.is_empty());
    }
}
