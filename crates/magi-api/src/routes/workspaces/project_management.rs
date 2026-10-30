use axum::{Json, extract::State};
use magi_core::{AbsolutePath, HostPath, UtcMillis, WorkspaceId, WorkspaceLifecycleStatus};
use magi_workspace::{WorkspaceProjectEdit, WorkspaceRecord, WorkspaceSourceFolder};
use serde::Deserialize;
use std::path::PathBuf;

use super::{new_workspace_id, publish_workspace_registry_changed, workspace_dto};
use crate::{errors::ApiError, session_activity::session_running_task_count, state::ApiState};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SourceFolderInput {
    #[serde(default)]
    pub source_folder_id: Option<String>,
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateProjectRequest {
    #[serde(default)]
    pub name: Option<String>,
    pub source_folders: Vec<SourceFolderInput>,
    #[serde(default)]
    pub primary_source_folder_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateProjectRequest {
    pub workspace_id: String,
    pub expected_revision: u64,
    #[serde(default)]
    pub name: Option<String>,
    pub source_folders: Vec<SourceFolderInput>,
    pub primary_source_folder_id: String,
}

fn normalize_name(name: Option<String>) -> Option<String> {
    name.map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn canonical_source_path(state: &ApiState, path: &str) -> Result<(PathBuf, String), ApiError> {
    let canonical = super::canonical_workspace_path(path)?;
    if let Some(state_root) = state
        .runtime_persistence()
        .and_then(|persistence| persistence.state_root())
        && magi_runtime_state::state_roots_overlap(state_root, &canonical)
            .map_err(|error| ApiError::internal_assembly("校验项目状态边界失败", error))?
    {
        return Err(ApiError::InvalidInput(
            "源文件夹不能包含 Magi 全局状态目录".into(),
        ));
    }
    let host = HostPath::from_path(canonical.clone());
    Ok((canonical, host.to_path_ref().as_str().to_string()))
}

fn build_source_folder(
    state: &ApiState,
    workspace_id: &WorkspaceId,
    input: SourceFolderInput,
    existing: Option<&WorkspaceSourceFolder>,
) -> Result<WorkspaceSourceFolder, ApiError> {
    // 既有成员允许离线改名/移除；任何可用路径都必须仍解析为原物理目录。
    let (path, path_ref) = match existing {
        Some(folder) if input.path == folder.path_ref || input.path == folder.path.as_str() => {
            let old = folder.native_path();
            if std::fs::canonicalize(&old).is_ok_and(|canonical| canonical != old) {
                return Err(ApiError::Conflict(
                    "源文件夹原规范路径已变化，请移除后重新添加该目录".into(),
                ));
            }
            (old, folder.path_ref.clone())
        }
        _ => canonical_source_path(state, &input.path)?,
    };
    let id = existing
        .map(|folder| folder.source_folder_id.clone())
        .unwrap_or_else(|| {
            magi_core::EventId::unique(format!("{workspace_id}-source")).to_string()
        });
    let now = UtcMillis::now();
    Ok(WorkspaceSourceFolder {
        source_folder_id: id,
        path: AbsolutePath::new(HostPath::from_path(path).display_string()),
        path_ref,
        name: normalize_name(input.name),
        created_at: existing.map(|folder| folder.created_at).unwrap_or(now),
        updated_at: now,
    })
}

fn project_json(state: &ApiState, project: &WorkspaceRecord) -> serde_json::Value {
    serde_json::json!({
        "workspace": workspace_dto(state, project),
        "workspaceId": project.workspace_id,
        "revision": project.revision,
    })
}

fn source_folder_overlap_error(error: magi_core::DomainError) -> ApiError {
    match error {
        magi_core::DomainError::AlreadyExists {
            entity: "workspace_root_path",
        } => ApiError::Conflict("源文件夹已属于另一个项目".into()),
        magi_core::DomainError::InvalidState { message } => ApiError::Conflict(message),
        magi_core::DomainError::Validation { message } => ApiError::InvalidInput(message),
        other => ApiError::internal_assembly("项目目录校验失败", other),
    }
}

/// 项目成员变化只能发生在明确静止的项目上。检查和提交在同一导航边界内完成，
/// 并尝试锁住每个会话，避免刚通过检查的输入在 mutation 中途被接纳。
pub(super) async fn acquire_project_edit_guards(
    state: &ApiState,
    workspace_id: &WorkspaceId,
    protect_artifacts: bool,
) -> Result<Vec<crate::state::SessionTurnCommitGuard>, ApiError> {
    // 不能使用侧栏列表：空会话、外部 MCP 会话与归档会话同样拥有资源。
    let sessions = state
        .session_store
        .sessions_for_workspace(workspace_id.as_str());
    let mut guards = Vec::with_capacity(sessions.len());
    for session in &sessions {
        guards.push(state.try_lock_session_turn_commit(&session.session_id)?);
        if session_running_task_count(
            state
                .session_store
                .runtime_sidecar(&session.session_id)
                .as_ref(),
        ) > 0
            || state
                .mcp_service
                .external_turns
                .in_flight(&session.session_id)
                > 0
            || state
                .terminal_sessions
                .has_running_for_session(session.session_id.as_str())
            || state.has_active_session_tool_execution(&session.session_id)
            || state.session_store.pending_input_count(&session.session_id) > 0
            || !state
                .session_store
                .pending_tool_approvals(&session.session_id)
                .map_err(|error| ApiError::internal_assembly("读取待审批状态失败", error))?
                .is_empty()
            || !state
                .session_store
                .pending_user_questions(&session.session_id)
                .map_err(|error| ApiError::internal_assembly("读取待提问状态失败", error))?
                .is_empty()
        {
            return Err(ApiError::Conflict(
                "项目仍有运行任务、待处理输入或交互，完成后才能修改源文件夹".into(),
            ));
        }
    }
    for session in sessions.iter().filter(|_| protect_artifacts) {
        if !state
            .session_isolations
            .for_session(session.session_id.as_str())
            .is_empty()
        {
            return Err(ApiError::Conflict(
                "项目仍有隔离副本或未合入成果，合入或丢弃后才能修改源文件夹".into(),
            ));
        }
        if state
            .session_code_contexts
            .for_session(session.session_id.as_str())
            .iter()
            .any(|context| {
                context.agent_worktrees.iter().any(|agent| {
                    agent.active || (!agent.applied && !agent.changed_paths.is_empty())
                })
            })
        {
            return Err(ApiError::Conflict(
                "项目仍有子代理执行副本或未采用成果，处理后才能修改源文件夹".into(),
            ));
        }
        if !state
            .workspace_git_coordinator
            .execution_roots(session.session_id.as_str())
            .is_empty()
        {
            return Err(ApiError::Conflict(
                "项目仍有执行资源占用，结算后才能修改源文件夹".into(),
            ));
        }
        for snapshot in state
            .snapshot_manager
            .sessions_for_session(session.session_id.as_str())
        {
            if !snapshot
                .pending_changes()
                .map_err(|error| ApiError::internal_assembly("读取项目待处理变更失败", error))?
                .is_empty()
            {
                return Err(ApiError::Conflict(
                    "项目仍有待处理变更，批准或回退后才能修改源文件夹".into(),
                ));
            }
        }
    }
    if state.task_store().is_some_and(|tasks| {
        tasks.all_tasks().iter().any(|task| {
            matches!(
                task.status,
                magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
            ) && task
                .executor_binding
                .as_ref()
                .and_then(|binding| binding.project_directories.as_ref())
                .is_some_and(|binding| &binding.workspace_id == workspace_id)
        })
    }) {
        return Err(ApiError::Conflict(
            "项目仍有未结算任务，完成后才能修改源文件夹".into(),
        ));
    }
    Ok(guards)
}

pub(crate) async fn create_project(
    State(state): State<ApiState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let CreateProjectRequest {
        name,
        source_folders,
        primary_source_folder_id,
    } = request;
    if source_folders.is_empty() {
        return Err(ApiError::InvalidInput("项目至少需要一个源文件夹".into()));
    }
    let _navigation_guard = state.lock_session_navigation().await;
    let mut workspace_id = new_workspace_id();
    let mut folders = Vec::with_capacity(source_folders.len());
    let mut draft_ids = std::collections::HashMap::new();
    let mut historical_source = None;
    for input in source_folders {
        let draft = input.source_folder_id.clone();
        let folder = build_source_folder(&state, &workspace_id, input, None)?;
        if let Some(identity) =
            magi_workspace::existing_workspace_identity(&folder.native_path())
                .map_err(|error| ApiError::Conflict(format!("读取源文件夹历史身份失败: {error}")))?
        {
            if historical_source.is_some() {
                return Err(ApiError::Conflict(
                    "多个源文件夹包含旧项目身份，不能自动合并历史；请先分别恢复项目".into(),
                ));
            }
            historical_source = Some((folders.len(), identity));
        }
        if let Some(draft) = draft
            && (draft.trim().is_empty()
                || draft_ids
                    .insert(draft, folder.source_folder_id.clone())
                    .is_some())
        {
            return Err(ApiError::InvalidInput(
                "源文件夹草稿身份必须非空且唯一".into(),
            ));
        }
        folders.push(folder);
    }
    if let Some((index, identity)) = historical_source {
        workspace_id = identity;
        let folder = &mut folders[index];
        let original_id = std::mem::replace(
            &mut folder.source_folder_id,
            format!("{workspace_id}-source-0"),
        );
        for id in draft_ids.values_mut().filter(|id| **id == original_id) {
            *id = folder.source_folder_id.clone();
        }
        magi_workspace::verify_or_create_workspace_identity(&folder.native_path(), &workspace_id)
            .map_err(|error| ApiError::Conflict(format!("校验项目历史身份失败: {error}")))?;
    }
    let primary = match primary_source_folder_id {
        Some(id) if draft_ids.contains_key(&id) => draft_ids[&id].clone(),
        Some(_) => {
            return Err(ApiError::InvalidInput(
                "主要源文件夹必须属于项目成员".into(),
            ));
        }
        None if folders.len() == 1 => folders[0].source_folder_id.clone(),
        None => {
            return Err(ApiError::InvalidInput(
                "多目录项目必须显式选择主要源文件夹".into(),
            ));
        }
    };
    let now = UtcMillis::now();
    let project = WorkspaceRecord {
        workspace_id: workspace_id.clone(),
        name: normalize_name(name),
        source_folders: folders,
        primary_source_folder_id: primary,
        revision: 1,
        worktree_root: None,
        status: WorkspaceLifecycleStatus::Registered,
        created_at: now,
        updated_at: now,
    };
    let project = state
        .workspace_registry
        .register_project(project, |snapshot| {
            state.persist_workspace_snapshot(snapshot)
        })
        .map_err(source_folder_overlap_error)?;
    state.restore_workspace_history_for_api(&project)?;
    publish_workspace_registry_changed(&state);
    for folder in &project.source_folders {
        state.schedule_workspace_code_index(project.workspace_id.clone(), folder.native_path());
    }
    Ok(Json(project_json(&state, &project)))
}

pub(crate) async fn update_project(
    State(state): State<ApiState>,
    Json(request): Json<UpdateProjectRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace_id = WorkspaceId::new(request.workspace_id.trim());
    let _project_edit = state.try_project_directory_edit(&workspace_id)?;
    let _navigation_guard = state.lock_session_navigation().await;
    let current = state
        .workspace_registry
        .project(&workspace_id)
        .ok_or_else(|| ApiError::not_found("项目不存在", workspace_id.as_str()))?;
    if current.revision != request.expected_revision {
        return Err(ApiError::Conflict("项目已变化，请刷新后重试".into()));
    }
    if request.source_folders.is_empty() {
        return Err(ApiError::InvalidInput("项目至少需要一个源文件夹".into()));
    }
    let mut folders = Vec::with_capacity(request.source_folders.len());
    let mut draft_ids = std::collections::HashMap::new();
    for input in request.source_folders {
        let existing = input
            .source_folder_id
            .as_deref()
            .and_then(|id| current.source_folder(id));
        let draft = input.source_folder_id.clone();
        let folder = build_source_folder(&state, &workspace_id, input, existing)?;
        if let Some(draft) = draft
            && (draft.trim().is_empty()
                || draft_ids
                    .insert(draft, folder.source_folder_id.clone())
                    .is_some())
        {
            return Err(ApiError::InvalidInput(
                "源文件夹草稿身份必须非空且唯一".into(),
            ));
        }
        folders.push(folder);
    }
    let primary = draft_ids
        .get(&request.primary_source_folder_id)
        .cloned()
        .ok_or_else(|| ApiError::InvalidInput("主要源文件夹必须属于项目成员".into()))?;
    let scope_changed = primary != current.primary_source_folder_id
        || folders.len() != current.source_folders.len()
        || folders.iter().any(|folder| {
            current
                .source_folder(&folder.source_folder_id)
                .is_none_or(|old| old.native_path() != folder.native_path())
        });
    let _edit_guards = if scope_changed {
        acquire_project_edit_guards(&state, &workspace_id, true).await?
    } else {
        Vec::new()
    };
    let project = state
        .workspace_registry
        .edit_project(
            &workspace_id,
            request.expected_revision,
            WorkspaceProjectEdit {
                name: normalize_name(request.name),
                source_folders: folders,
                primary_source_folder_id: primary,
            },
            |snapshot| state.persist_workspace_snapshot(snapshot),
        )
        .map_err(source_folder_overlap_error)?;
    publish_workspace_registry_changed(&state);
    for folder in &project.source_folders {
        state.schedule_workspace_code_index(project.workspace_id.clone(), folder.native_path());
    }
    Ok(Json(project_json(&state, &project)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        ApiState::new(
            "project-management-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    fn update_request(project: &WorkspaceRecord) -> UpdateProjectRequest {
        UpdateProjectRequest {
            workspace_id: project.workspace_id.to_string(),
            expected_revision: project.revision,
            name: project.name.clone(),
            source_folders: project
                .source_folders
                .iter()
                .map(|folder| SourceFolderInput {
                    source_folder_id: Some(folder.source_folder_id.clone()),
                    path: folder.path_ref.clone(),
                    name: folder.name.clone(),
                })
                .collect(),
            primary_source_folder_id: project.primary_source_folder_id.clone(),
        }
    }

    #[tokio::test]
    async fn project_creation_preserves_one_portable_identity_and_rejects_multiple_histories() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let other = root.path().join("other");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let identity = WorkspaceId::new("portable-project");
        magi_workspace::resolve_or_create_workspace_identity(&old, identity.clone()).unwrap();
        let request = || CreateProjectRequest {
            name: Some("Restored with another primary".into()),
            source_folders: vec![
                SourceFolderInput {
                    source_folder_id: Some("old".into()),
                    path: old.display().to_string(),
                    name: None,
                },
                SourceFolderInput {
                    source_folder_id: Some("other".into()),
                    path: other.display().to_string(),
                    name: None,
                },
            ],
            primary_source_folder_id: Some("other".into()),
        };
        let state = test_state();
        let created = create_project(State(state.clone()), Json(request()))
            .await
            .unwrap()
            .0;
        assert_eq!(created["workspaceId"], identity.as_str());
        let project = state.workspace_registry.project(&identity).unwrap();
        assert_eq!(
            project.source_folders[0].source_folder_id,
            "portable-project-source-0"
        );
        assert_eq!(
            project.primary_source_folder().native_path(),
            other.canonicalize().unwrap()
        );
        assert_eq!(
            magi_workspace::existing_workspace_identity(&other).unwrap(),
            None
        );

        magi_workspace::resolve_or_create_workspace_identity(
            &other,
            WorkspaceId::new("second-history"),
        )
        .unwrap();
        let fresh = test_state();
        let events = fresh.event_bus.snapshot().next_sequence;
        assert!(matches!(
            create_project(State(fresh.clone()), Json(request())).await,
            Err(ApiError::Conflict(_))
        ));
        assert!(fresh.workspace_registry.is_empty());
        assert_eq!(fresh.event_bus.snapshot().next_sequence, events);
    }

    #[tokio::test]
    async fn project_edit_rejects_empty_session_admission_and_pending_inputs_atomically() {
        let state = test_state();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let id = WorkspaceId::new("project-management-admission");
        let project = state
            .workspace_registry
            .register_native_path(id.clone(), std::fs::canonicalize(first.path()).unwrap())
            .unwrap();
        let session = magi_core::SessionId::new("empty-project-session");
        state
            .session_store
            .create_session_for_workspace(session.clone(), "empty", Some(id.to_string()))
            .unwrap();
        let request = || {
            let mut request = update_request(&project);
            request.source_folders.push(SourceFolderInput {
                source_folder_id: Some("draft".into()),
                path: second.path().display().to_string(),
                name: None,
            });
            request
        };
        let session_guard = state.lock_session_turn_commit(&session).await;
        assert!(matches!(
            update_project(State(state.clone()), Json(request())).await,
            Err(ApiError::Conflict(_))
        ));
        drop(session_guard);
        let admission = state.try_project_directory_admission(&id).unwrap();
        assert!(matches!(
            update_project(State(state.clone()), Json(request())).await,
            Err(ApiError::Conflict(_))
        ));
        drop(admission);
        let input = magi_session_store::SessionInput {
            input_id: "input-pending-project-edit".into(),
            session_id: session.clone(),
            request_id: "pending-project-edit".into(),
            user_message_id: "message-pending-project-edit".into(),
            request_fingerprint: "fingerprint-pending-project-edit".into(),
            accepted_at: UtcMillis::now(),
            revision: 1,
            state: magi_session_store::SessionInputState::Pending,
            attached_turn_id: None,
            consumption: None,
            interruption: None,
            payload: serde_json::json!({}),
        };
        state
            .session_store
            .commit_input(input.clone(), None)
            .unwrap();
        let events = state.event_bus.snapshot().next_sequence;
        assert!(matches!(
            update_project(State(state.clone()), Json(request())).await,
            Err(ApiError::Conflict(_))
        ));
        assert_eq!(state.workspace_registry.project(&id).unwrap().revision, 1);
        assert_eq!(state.event_bus.snapshot().next_sequence, events);
        // 改名不改变授权或执行默认值，可以在原队列仍存在时提交。
        let mut rename = update_request(&project);
        rename.name = Some("Renamed".into());
        let renamed = update_project(State(state.clone()), Json(rename))
            .await
            .unwrap()
            .0;
        assert_eq!(renamed["workspace"]["name"], "Renamed");
        assert_eq!(
            state.session_store.input_for_id(&session, &input.input_id),
            Some(input)
        );
        let mut stale = request();
        stale.expected_revision = 1;
        assert!(matches!(
            update_project(State(state), Json(stale)).await,
            Err(ApiError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn removed_source_identity_cannot_be_reintroduced_by_client_or_restore_old_grants() {
        let state = test_state();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let id = WorkspaceId::new("project-management-revocation");
        state
            .workspace_registry
            .register_native_path(id.clone(), std::fs::canonicalize(first.path()).unwrap())
            .unwrap();
        let original = crate::routes::test_project_fixtures::add_source(
            &state,
            id.as_str(),
            "old-source",
            second.path(),
        );
        let grant = state
            .workspace_registry
            .capture_directory_binding(&id, Some("old-source"), None)
            .unwrap();
        let mut remove = update_request(&original);
        remove
            .source_folders
            .retain(|folder| folder.source_folder_id.as_deref() != Some("old-source"));
        let _ = update_project(State(state.clone()), Json(remove))
            .await
            .unwrap();
        let after_remove = state.workspace_registry.project(&id).unwrap();
        let mut add = update_request(&after_remove);
        add.source_folders.push(SourceFolderInput {
            source_folder_id: Some("old-source".into()),
            path: second.path().display().to_string(),
            name: None,
        });
        add.primary_source_folder_id = "old-source".into();
        let _ = update_project(State(state.clone()), Json(add))
            .await
            .unwrap();
        let readded = state.workspace_registry.project(&id).unwrap();
        assert!(readded.source_folder("old-source").is_none());
        assert_ne!(readded.primary_source_folder_id, "old-source");
        assert!(
            state
                .workspace_registry
                .validate_directory_binding(&grant)
                .is_err()
        );
        assert!(first.path().is_dir() && second.path().is_dir());
    }

    #[tokio::test]
    async fn project_edit_offline_member_and_write_failure_preserve_authoritative_state() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let state_root = root.path().join("state");
        std::fs::create_dir_all(&source).unwrap();
        let workspace_file = root.path().join("registry/workspaces.json");
        let state = test_state().with_runtime_persistence(Arc::new(
            crate::state::RuntimeStatePersistence::new(
                state_root,
                workspace_file.clone(),
                root.path().join("knowledge.json"),
            ),
        ));
        let id = WorkspaceId::new("project-management-persistence");
        let project = state
            .workspace_registry
            .register_native_path_with_persistence(
                id.clone(),
                std::fs::canonicalize(&source).unwrap(),
                |snapshot| state.persist_workspace_snapshot(snapshot),
            )
            .unwrap();
        let before = std::fs::read(&workspace_file).unwrap();
        std::fs::remove_dir_all(&source).unwrap();
        let mut rename = update_request(&project);
        rename.name = Some("Offline project".into());
        let _ = update_project(State(state.clone()), Json(rename))
            .await
            .unwrap();
        assert!(!source.exists());
        let offline = state.workspace_registry.project(&id).unwrap();
        assert_eq!(offline.revision, 2);
        assert_ne!(std::fs::read(&workspace_file).unwrap(), before);
        let saved = std::fs::read(&workspace_file).unwrap();
        std::fs::remove_file(&workspace_file).unwrap();
        std::fs::remove_dir(workspace_file.parent().unwrap()).unwrap();
        std::fs::write(workspace_file.parent().unwrap(), b"blocked registry").unwrap();
        let events = state.event_bus.snapshot().next_sequence;
        let mut failing = update_request(&offline);
        failing.name = Some("Uncommitted name".into());
        assert!(
            update_project(State(state.clone()), Json(failing))
                .await
                .is_err()
        );
        let retained = state.workspace_registry.project(&id).unwrap();
        assert_eq!(retained.name.as_deref(), Some("Offline project"));
        assert_eq!(retained.revision, 2);
        assert_eq!(state.event_bus.snapshot().next_sequence, events);
        let recovered: magi_workspace::WorkspaceDurableState =
            serde_json::from_slice(&saved).unwrap();
        assert_eq!(recovered.workspaces[0].revision, 2);
        assert!(!source.exists());
    }

    #[tokio::test]
    async fn create_and_update_project_preserves_source_ids_and_primary_selection() {
        let state = test_state();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, symbol) in [
            (first.path(), "source_main"),
            (second.path(), "source_docs"),
        ] {
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join("src/index.ts"),
                format!("export function {symbol}() {{}}\n"),
            )
            .unwrap();
        }
        let created = create_project(
            State(state.clone()),
            Json(CreateProjectRequest {
                name: Some("双目录项目".into()),
                source_folders: vec![
                    SourceFolderInput {
                        source_folder_id: Some("main".into()),
                        path: first.path().display().to_string(),
                        name: Some("主仓库".into()),
                    },
                    SourceFolderInput {
                        source_folder_id: Some("docs".into()),
                        path: second.path().display().to_string(),
                        name: Some("文档".into()),
                    },
                ],
                primary_source_folder_id: Some("main".into()),
            }),
        )
        .await
        .unwrap()
        .0;
        let workspace_id = created["workspaceId"].as_str().unwrap().to_string();
        let revision = created["revision"].as_u64().unwrap();
        let main_id = created["workspace"]["sourceFolders"][0]["sourceFolderId"]
            .as_str()
            .unwrap()
            .to_string();
        let docs_id = created["workspace"]["sourceFolders"][1]["sourceFolderId"]
            .as_str()
            .unwrap()
            .to_string();
        let updated = update_project(
            State(state.clone()),
            Json(UpdateProjectRequest {
                workspace_id: workspace_id.clone(),
                expected_revision: revision,
                name: Some("改名后的项目".into()),
                source_folders: vec![
                    SourceFolderInput {
                        source_folder_id: Some(main_id.clone()),
                        path: first.path().display().to_string(),
                        name: None,
                    },
                    SourceFolderInput {
                        source_folder_id: Some(docs_id.clone()),
                        path: second.path().display().to_string(),
                        name: Some("资料".into()),
                    },
                ],
                primary_source_folder_id: docs_id.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(updated["workspace"]["primarySourceFolderId"], docs_id);
        assert_eq!(updated["revision"], revision + 1);
        let project = state
            .workspace_registry
            .project(&WorkspaceId::new(workspace_id))
            .unwrap();
        assert_eq!(project.source_folders.len(), 2);
        assert_eq!(project.source_folders[0].source_folder_id, main_id);
        assert_eq!(project.source_folders[1].source_folder_id, docs_id);
        assert_eq!(project.primary_source_folder_id, docs_id);
        assert_eq!(
            project.source_folder(&docs_id).unwrap().name.as_deref(),
            Some("资料")
        );
        for (folder, expected, other) in [
            (&project.source_folders[0], "source_main", "source_docs"),
            (&project.source_folders[1], "source_docs", "source_main"),
        ] {
            let store = state
                .knowledge_store
                .for_source_folder(&folder.source_folder_id, &folder.native_path())
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while store.workspace_index_building(&project.workspace_id) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                store
                    .find_symbol_definitions(&project.workspace_id, expected, 10)
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                store
                    .find_symbol_definitions(&project.workspace_id, other, 10)
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(
            !state
                .knowledge_store
                .workspace_index_ready(&project.workspace_id)
        );
        let unrelated = tempfile::tempdir().unwrap();
        assert!(
            !state.schedule_workspace_code_index(
                project.workspace_id,
                unrelated.path().to_path_buf()
            )
        );
    }

    #[tokio::test]
    async fn create_project_rejects_overlapping_registered_source_folder() {
        let state = test_state();
        let root = tempfile::tempdir().unwrap();
        let _ = create_project(
            State(state.clone()),
            Json(CreateProjectRequest {
                name: None,
                source_folders: vec![SourceFolderInput {
                    source_folder_id: None,
                    path: root.path().display().to_string(),
                    name: None,
                }],
                primary_source_folder_id: None,
            }),
        )
        .await
        .unwrap();
        let error = create_project(
            State(state),
            Json(CreateProjectRequest {
                name: None,
                source_folders: vec![SourceFolderInput {
                    source_folder_id: None,
                    path: root.path().display().to_string(),
                    name: None,
                }],
                primary_source_folder_id: None,
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ApiError::Conflict(_)));
    }
}
