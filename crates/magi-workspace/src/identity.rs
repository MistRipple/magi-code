use magi_core::{DomainError, DomainResult, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::BufReader,
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

/// session projection 只需要读出身份头部：`durable.sessions[*].workspaceId`。
///
/// 单个会话的 projection 里有完整的 turn 历史，可以达到数百 MB。这里只声明需要的字段，
/// 其余内容由 serde 直接跳过，不构造 `serde_json::Value` 树，也不整份读入内存。
#[derive(Deserialize)]
struct ProjectionIdentityHeader {
    durable: Option<ProjectionDurableHeader>,
}

#[derive(Deserialize)]
struct ProjectionDurableHeader {
    sessions: Option<Vec<ProjectionSessionHeader>>,
}

#[derive(Deserialize)]
struct ProjectionSessionHeader {
    #[serde(rename = "workspaceId")]
    workspace_id: Option<serde_json::Value>,
}

/// 只解析 projection 的身份头，其余（可能数百 MB 的历史）由 serde 直接跳过。
fn read_projection_workspace_id(path: &Path) -> DomainResult<WorkspaceId> {
    let file = fs::File::open(path)
        .map_err(|error| persistence_error("读取 workspace session projection", path, error))?;
    let header: ProjectionIdentityHeader =
        serde_json::from_reader(BufReader::with_capacity(1 << 20, file))
            .map_err(|error| persistence_error("解析 workspace session projection", path, error))?;
    let sessions = header
        .durable
        .and_then(|durable| durable.sessions)
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
        .workspace_id
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DomainError::Validation {
            message: format!(
                "workspace session projection 缺少 workspaceId: {}",
                path.display()
            ),
        })?;
    Ok(WorkspaceId::new(workspace_id))
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
    paths
        .iter()
        .map(|path| read_projection_workspace_id(path))
        .collect()
}

/// 首次注册时的身份推断：manifest 优先；没有 manifest 时采用工作区里已有 session
/// projection 的归属（拷贝来的项目）。多个归属并存直接拒绝。
///
/// 这个成本随 projection 大小增长，所以只允许出现在注册路径；每次保存与会话导航的
/// 热路径只走 `verify_or_create_workspace_identity`，不读取 projection。
fn infer_workspace_identity_locked(workspace_root: &Path) -> DomainResult<Option<WorkspaceId>> {
    if let Some(manifest_id) = read_manifest(&identity_path(workspace_root))? {
        return Ok(Some(manifest_id));
    }
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
    Ok(projection_ids.into_iter().next())
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
        infer_workspace_identity_locked(workspace_root)?.unwrap_or(proposed_workspace_id);
    if !identity_path(workspace_root).exists() {
        write_manifest_locked(workspace_root, &identity)?;
    }
    Ok(identity)
}

/// 校验已注册工作区与项目本地 manifest 一致；缺少 manifest 时以注册归属初始化。
/// session projection 与工作区归属的一致性由启动时的 projection 加载逐个强校验，这里不读取它们。
pub fn verify_or_create_workspace_identity(
    workspace_root: &Path,
    expected_workspace_id: &WorkspaceId,
) -> DomainResult<()> {
    let _guard = identity_write_lock()
        .lock()
        .expect("workspace identity write lock poisoned");
    if let Some(actual_workspace_id) = read_manifest(&identity_path(workspace_root))?
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

    fn write_projection(root: &Path, session_id: &str, workspace_id: Option<&str>) {
        let projection_root = root.join(".magi").join("session-projections");
        fs::create_dir_all(&projection_root).expect("projection root");
        let value = serde_json::json!({
            "canonical_event_seq": 0,
            "durable": { "sessions": [{ "sessionId": session_id, "workspaceId": workspace_id }] }
        });
        fs::write(
            projection_root.join(format!("{session_id}.json")),
            serde_json::to_vec(&value).expect("projection json"),
        )
        .expect("projection write");
    }

    #[test]
    fn existing_projection_initializes_stable_identity_on_registration() {
        let root = tempfile::tempdir().expect("workspace root");
        write_projection(root.path(), "session-existing", Some("workspace-existing"));
        let resolved = resolve_or_create_workspace_identity(
            root.path(),
            WorkspaceId::new("workspace-proposed"),
        )
        .expect("projection identity");
        assert_eq!(resolved.as_str(), "workspace-existing");
    }

    #[test]
    fn mixed_projection_identities_are_rejected_on_registration() {
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
    fn projection_header_parse_skips_large_unrelated_history() {
        let root = tempfile::tempdir().expect("workspace root");
        let projection_root = root.path().join(".magi").join("session-projections");
        fs::create_dir_all(&projection_root).expect("projection root");
        let turns: Vec<serde_json::Value> = (0..50_000)
            .map(|index| serde_json::json!({ "turnId": index, "text": "x".repeat(64) }))
            .collect();
        let value = serde_json::json!({
            "durable": {
                "sessions": [{ "sessionId": "session-big", "workspaceId": "workspace-big" }],
                "canonical_turns": turns,
            }
        });
        fs::write(
            projection_root.join("session-big.json"),
            serde_json::to_vec(&value).expect("json"),
        )
        .expect("write");
        let ids = projection_workspace_ids(root.path()).expect("big projection");
        assert_eq!(ids, HashSet::from([WorkspaceId::new("workspace-big")]));
    }

    #[test]
    fn existing_manifest_wins_over_the_proposed_identity() {
        let root = tempfile::tempdir().expect("workspace root");
        resolve_or_create_workspace_identity(root.path(), WorkspaceId::new("workspace-existing"))
            .expect("first registration");
        let resolved = resolve_or_create_workspace_identity(
            root.path(),
            WorkspaceId::new("workspace-proposed"),
        )
        .expect("second registration");
        assert_eq!(resolved.as_str(), "workspace-existing");
    }

    #[test]
    fn registered_identity_mismatch_is_rejected_without_rewrite() {
        let root = tempfile::tempdir().expect("workspace root");
        resolve_or_create_workspace_identity(root.path(), WorkspaceId::new("workspace-existing"))
            .expect("registration");
        let error = verify_or_create_workspace_identity(
            root.path(),
            &WorkspaceId::new("workspace-registered"),
        )
        .expect_err("registered mismatch must fail");
        assert!(error.to_string().contains("全局注册表"));
        assert_eq!(
            read_manifest(&identity_path(root.path()))
                .expect("manifest")
                .as_ref()
                .map(WorkspaceId::as_str),
            Some("workspace-existing")
        );
    }

    #[test]
    fn identity_check_ignores_session_projection_history() {
        let root = tempfile::tempdir().expect("workspace root");
        let projection_root = root.path().join(".magi").join("session-projections");
        fs::create_dir_all(&projection_root).expect("projection root");
        // 身份核对不读取 projection：即使文件内容无法解析也不影响结果。
        fs::write(projection_root.join("session-broken.json"), b"not json").expect("write");
        verify_or_create_workspace_identity(root.path(), &WorkspaceId::new("workspace-a"))
            .expect("manifest-only verify");
    }
}
