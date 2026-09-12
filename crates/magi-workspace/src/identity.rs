use magi_core::{DomainError, DomainResult, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const WORKSPACE_IDENTITY_SCHEMA_VERSION: u32 = 1;
const WORKSPACE_IDENTITY_FILE_NAME: &str = "workspace-identity.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceIdentityManifest {
    schema_version: u32,
    workspace_id: WorkspaceId,
}

fn identity_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn identity_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".magi")
        .join(WORKSPACE_IDENTITY_FILE_NAME)
}

fn persistence_error(action: &str, path: &Path, error: impl std::fmt::Display) -> DomainError {
    DomainError::Persistence {
        message: format!("{action} {} 失败: {error}", path.display()),
    }
}

fn read_manifest(path: &Path) -> DomainResult<Option<WorkspaceId>> {
    let content = match fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(persistence_error("读取 workspace identity", path, error)),
    };
    let manifest: WorkspaceIdentityManifest = serde_json::from_slice(&content)
        .map_err(|error| persistence_error("解析 workspace identity", path, error))?;
    if manifest.schema_version != WORKSPACE_IDENTITY_SCHEMA_VERSION
        || manifest.workspace_id.as_str().trim().is_empty()
    {
        return Err(DomainError::Validation {
            message: format!("workspace identity 内容无效: {}", path.display()),
        });
    }
    Ok(Some(manifest.workspace_id))
}

fn projection_workspace_ids(workspace_root: &Path) -> DomainResult<HashSet<WorkspaceId>> {
    let projection_root = workspace_root.join(".magi").join("session-projections");
    let entries = match fs::read_dir(&projection_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => {
            return Err(persistence_error(
                "读取 workspace session projection 目录",
                &projection_root,
                error,
            ));
        }
    };

    let mut paths = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                persistence_error("读取 workspace session projection", &projection_root, error)
            })
        })
        .collect::<DomainResult<Vec<_>>>()?;
    paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    paths.sort();

    let mut workspace_ids = HashSet::new();
    for path in paths {
        let content = fs::read(&path).map_err(|error| {
            persistence_error("读取 workspace session projection", &path, error)
        })?;
        let value: serde_json::Value = serde_json::from_slice(&content).map_err(|error| {
            persistence_error("解析 workspace session projection", &path, error)
        })?;
        let sessions = value
            .get("durable")
            .and_then(|durable| durable.get("sessions"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| DomainError::Validation {
                message: format!(
                    "workspace session projection 缺少 sessions: {}",
                    path.display()
                ),
            })?;
        if sessions.len() != 1 {
            return Err(DomainError::Validation {
                message: format!(
                    "workspace session projection 必须只包含一个 session: {}",
                    path.display()
                ),
            });
        }
        let workspace_id = sessions[0]
            .get("workspaceId")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| DomainError::Validation {
                message: format!(
                    "workspace session projection 缺少 workspaceId: {}",
                    path.display()
                ),
            })?;
        workspace_ids.insert(WorkspaceId::new(workspace_id));
    }
    Ok(workspace_ids)
}

fn inspect_workspace_identity_locked(workspace_root: &Path) -> DomainResult<Option<WorkspaceId>> {
    let path = identity_path(workspace_root);
    let manifest_id = read_manifest(&path)?;
    let projection_ids = projection_workspace_ids(workspace_root)?;
    if projection_ids.len() > 1 {
        let mut ids = projection_ids
            .iter()
            .map(|id| id.as_str().to_string())
            .collect::<Vec<_>>();
        ids.sort();
        return Err(DomainError::Validation {
            message: format!(
                "同一工作区包含多个 workspace identity，拒绝自动合并: {} ({})",
                workspace_root.display(),
                ids.join(", ")
            ),
        });
    }
    let projection_id = projection_ids.into_iter().next();
    match (manifest_id, projection_id) {
        (Some(manifest_id), Some(projection_id)) if manifest_id != projection_id => {
            Err(DomainError::Validation {
                message: format!(
                    "workspace identity 与 session projection 归属不一致: {} != {} ({})",
                    manifest_id,
                    projection_id,
                    workspace_root.display()
                ),
            })
        }
        (Some(manifest_id), _) => Ok(Some(manifest_id)),
        (None, Some(projection_id)) => Ok(Some(projection_id)),
        (None, None) => Ok(None),
    }
}

fn write_manifest_locked(workspace_root: &Path, workspace_id: &WorkspaceId) -> DomainResult<()> {
    let path = identity_path(workspace_root);
    let parent = path.parent().expect("workspace identity must have parent");
    fs::create_dir_all(parent)
        .map_err(|error| persistence_error("创建 workspace identity 目录", parent, error))?;
    let content = serde_json::to_vec_pretty(&WorkspaceIdentityManifest {
        schema_version: WORKSPACE_IDENTITY_SCHEMA_VERSION,
        workspace_id: workspace_id.clone(),
    })
    .map_err(|error| persistence_error("序列化 workspace identity", &path, error))?;
    magi_core::fs_atomic::write_atomic(&path, content)
        .map_err(|error| persistence_error("写入 workspace identity", &path, error))
}

/// 为首次注册解析稳定 workspace identity。
///
/// 已有 manifest 或 session projection 决定 identity；只有全新工作区才采用
/// `proposed_workspace_id`。最终结果始终写入项目本地 manifest，后续状态根不得重建 ID。
pub fn resolve_or_create_workspace_identity(
    workspace_root: &Path,
    proposed_workspace_id: WorkspaceId,
) -> DomainResult<WorkspaceId> {
    let _guard = identity_write_lock()
        .lock()
        .expect("workspace identity write lock poisoned");
    let identity =
        inspect_workspace_identity_locked(workspace_root)?.unwrap_or(proposed_workspace_id);
    if !identity_path(workspace_root).exists() {
        write_manifest_locked(workspace_root, &identity)?;
    }
    Ok(identity)
}

/// 校验已注册工作区与项目本地 identity 一致；缺少 manifest 时只允许以相同归属初始化。
pub fn verify_or_create_workspace_identity(
    workspace_root: &Path,
    expected_workspace_id: &WorkspaceId,
) -> DomainResult<()> {
    let _guard = identity_write_lock()
        .lock()
        .expect("workspace identity write lock poisoned");
    if let Some(actual_workspace_id) = inspect_workspace_identity_locked(workspace_root)?
        && &actual_workspace_id != expected_workspace_id
    {
        return Err(DomainError::Validation {
            message: format!(
                "全局注册表与项目 workspace identity 不一致: {} != {} ({})",
                expected_workspace_id,
                actual_workspace_id,
                workspace_root.display()
            ),
        });
    }
    if !identity_path(workspace_root).exists() {
        write_manifest_locked(workspace_root, expected_workspace_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_projection(root: &Path, session_id: &str, workspace_id: Option<&str>) {
        let projection_root = root.join(".magi").join("session-projections");
        fs::create_dir_all(&projection_root).expect("projection root");
        let value = serde_json::json!({
            "canonical_event_seq": 0,
            "durable": {
                "current_session_id": null,
                "sessions": [{
                    "sessionId": session_id,
                    "workspaceId": workspace_id,
                }],
                "timeline": [],
                "canonical_turns": [],
                "notifications": [],
                "goals": [],
                "plans": [],
                "thread_registry": [],
                "thread_context_checkpoints": [],
            }
        });
        fs::write(
            projection_root.join(format!("{session_id}.json")),
            serde_json::to_vec_pretty(&value).expect("projection json"),
        )
        .expect("projection write");
    }

    #[test]
    fn new_workspace_persists_proposed_identity() {
        let root = tempfile::tempdir().expect("workspace root");
        let workspace_id = WorkspaceId::new("workspace-new");
        let resolved = resolve_or_create_workspace_identity(root.path(), workspace_id.clone())
            .expect("new identity");
        assert_eq!(resolved, workspace_id);
        verify_or_create_workspace_identity(root.path(), &workspace_id)
            .expect("persisted identity");
    }

    #[test]
    fn existing_projection_initializes_stable_identity() {
        let root = tempfile::tempdir().expect("workspace root");
        write_projection(root.path(), "session-existing", Some("workspace-existing"));
        let resolved = resolve_or_create_workspace_identity(
            root.path(),
            WorkspaceId::new("workspace-proposed"),
        )
        .expect("projection identity");
        assert_eq!(resolved.as_str(), "workspace-existing");
        verify_or_create_workspace_identity(root.path(), &resolved).expect("stable identity");
    }

    #[test]
    fn mixed_projection_identities_are_rejected_without_manifest() {
        let root = tempfile::tempdir().expect("workspace root");
        write_projection(root.path(), "session-a", Some("workspace-a"));
        write_projection(root.path(), "session-b", Some("workspace-b"));
        let error = resolve_or_create_workspace_identity(
            root.path(),
            WorkspaceId::new("workspace-proposed"),
        )
        .expect_err("mixed identities must fail");
        assert!(error.to_string().contains("多个 workspace identity"));
        assert!(!identity_path(root.path()).exists());
    }

    #[test]
    fn registered_identity_mismatch_is_rejected_without_rewrite() {
        let root = tempfile::tempdir().expect("workspace root");
        write_projection(root.path(), "session-existing", Some("workspace-existing"));
        let error = verify_or_create_workspace_identity(
            root.path(),
            &WorkspaceId::new("workspace-registered"),
        )
        .expect_err("registered mismatch must fail");
        assert!(error.to_string().contains("全局注册表"));
        assert!(!identity_path(root.path()).exists());
    }
}
