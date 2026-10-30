use super::WorkspaceStore;
use crate::{WorkspaceDurableState, WorkspaceRecord, WorkspaceSourceFolder};
use magi_core::{DomainError, DomainResult, UtcMillis, WorkspaceId};

/// 管理命令一次提交完整成员集合和主要目录，移除主要目录必须显式选择替代项。
/// 调用方负责本机路径规范化和在途任务/产物检查；Store 负责身份、CAS 和原子提交。
#[derive(Clone, Debug)]
pub struct WorkspaceProjectEdit {
    pub name: Option<String>,
    pub source_folders: Vec<WorkspaceSourceFolder>,
    pub primary_source_folder_id: String,
}

impl WorkspaceStore {
    pub fn project(&self, workspace_id: &WorkspaceId) -> Option<WorkspaceRecord> {
        self.read_state()
            .workspaces
            .iter()
            .find(|project| &project.workspace_id == workspace_id)
            .cloned()
    }

    /// 持久提交成功之后才发布内存事实。写入失败不需要用第二个 mutation 回滚，
    /// 其他读取者也看不到尚未持久化的项目。
    pub fn register_project(
        &self,
        mut project: WorkspaceRecord,
        persist: impl FnOnce(&WorkspaceDurableState) -> DomainResult<()>,
    ) -> DomainResult<WorkspaceRecord> {
        normalize_source_paths(&mut project.source_folders);
        project.validate()?;
        let mut state = self.write_state();
        if state
            .workspaces
            .iter()
            .any(|existing| existing.workspace_id == project.workspace_id)
        {
            return Err(DomainError::AlreadyExists {
                entity: "workspace",
            });
        }
        if state
            .workspaces
            .iter()
            .flat_map(WorkspaceRecord::source_folder_roots)
            .any(|root| project.source_folder_roots().contains(&root))
        {
            return Err(DomainError::AlreadyExists {
                entity: "workspace_root_path",
            });
        }
        validate_project_roots(&project, &state.workspaces)?;
        let mut next = state.durable_state();
        if next.active_workspace_id.is_none() {
            next.active_workspace_id = Some(project.workspace_id.clone());
        }
        next.workspaces.push(project.clone());
        persist(&next)?;
        state.active_workspace_id = next.active_workspace_id;
        state.workspaces = next.workspaces;
        Ok(project)
    }

    pub fn edit_project(
        &self,
        workspace_id: &WorkspaceId,
        expected_revision: u64,
        mut edit: WorkspaceProjectEdit,
        persist: impl FnOnce(&WorkspaceDurableState) -> DomainResult<()>,
    ) -> DomainResult<WorkspaceRecord> {
        normalize_source_paths(&mut edit.source_folders);
        let mut state = self.write_state();
        let index = state
            .workspaces
            .iter()
            .position(|existing| &existing.workspace_id == workspace_id)
            .ok_or(DomainError::NotFound {
                entity: "workspace",
            })?;
        let current = &state.workspaces[index];
        if current.revision != expected_revision {
            return Err(DomainError::InvalidState {
                message: "项目已变化，请刷新后重试".into(),
            });
        }
        // 目录移动必须移除旧身份再添加新身份，旧任务不能通过复用 ID 获得新路径。
        for folder in &edit.source_folders {
            if current
                .source_folder(&folder.source_folder_id)
                .is_some_and(|old| old.native_path() != folder.native_path())
            {
                return Err(DomainError::Validation {
                    message: "源文件夹 ID 不能改绑其他目录".into(),
                });
            }
        }
        let mut updated = current.clone();
        updated.name = edit.name.map(|name| name.trim().to_string());
        updated.source_folders = edit.source_folders;
        updated.primary_source_folder_id = edit.primary_source_folder_id;
        updated.revision =
            updated
                .revision
                .checked_add(1)
                .ok_or_else(|| DomainError::InvalidState {
                    message: "项目修订已溢出".into(),
                })?;
        updated.updated_at = UtcMillis::now();
        updated.validate()?;
        validate_project_roots(&updated, &state.workspaces)?;
        let mut next = state.durable_state();
        next.workspaces[index] = updated.clone();
        persist(&next)?;
        state.workspaces = next.workspaces;
        Ok(updated)
    }
}

fn normalize_source_paths(folders: &mut [WorkspaceSourceFolder]) {
    for folder in folders {
        if let Ok(path) = magi_core::HostPath::canonicalize(folder.native_path()) {
            folder.path = magi_core::AbsolutePath::new(path.display_string());
            folder.path_ref = path.to_path_ref().as_str().into();
        }
    }
}

fn validate_project_roots(
    project: &WorkspaceRecord,
    projects: &[WorkspaceRecord],
) -> DomainResult<()> {
    let roots = project.source_folder_roots();
    for (index, root) in roots.iter().enumerate() {
        let overlaps = |other: &std::path::Path| root.starts_with(other) || other.starts_with(root);
        if roots[..index].iter().any(|other| overlaps(other))
            || projects
                .iter()
                .filter(|other| other.workspace_id != project.workspace_id)
                .flat_map(WorkspaceRecord::source_folder_roots)
                .any(|other| overlaps(&other))
        {
            return Err(DomainError::Validation {
                message: "源文件夹不能与已注册目录重叠".into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{AbsolutePath, HostPath, WorkspaceLifecycleStatus};

    fn folder(id: &str, path: &std::path::Path) -> WorkspaceSourceFolder {
        let host = HostPath::from_path(path.to_path_buf());
        WorkspaceSourceFolder {
            source_folder_id: id.into(),
            path: AbsolutePath::new(host.display_string()),
            path_ref: host.to_path_ref().as_str().into(),
            name: None,
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }
    }

    fn project(id: &str, folders: Vec<WorkspaceSourceFolder>) -> WorkspaceRecord {
        WorkspaceRecord {
            workspace_id: WorkspaceId::new(id),
            name: Some("Project".into()),
            primary_source_folder_id: folders[0].source_folder_id.clone(),
            source_folders: folders,
            revision: 1,
            worktree_root: None,
            status: WorkspaceLifecycleStatus::Registered,
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }
    }

    #[test]
    fn project_cas_and_failed_persistence_preserve_all_facts() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new();
        let original = project(
            "project",
            vec![
                folder("a", &root.path().join("a")),
                folder("b", &root.path().join("b")),
            ],
        );
        let id = original.workspace_id.clone();
        store
            .register_project(original.clone(), |_| Ok(()))
            .unwrap();
        let before = serde_json::to_value(store.durable_state()).unwrap();
        let edit = WorkspaceProjectEdit {
            name: Some("Changed".into()),
            source_folders: original.source_folders.clone(),
            primary_source_folder_id: "b".into(),
        };
        let result = store.edit_project(&id, 1, edit.clone(), |_| {
            Err(DomainError::Persistence {
                message: "injected write failure".into(),
            })
        });
        assert!(result.is_err());
        assert_eq!(serde_json::to_value(store.durable_state()).unwrap(), before);
        let updated = store
            .edit_project(&id, 1, edit.clone(), |_| Ok(()))
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.primary_source_folder_id, "b");
        assert_eq!(updated.workspace_id, id);
        assert!(
            store
                .edit_project(&id, 1, edit, |_| panic!("stale mutation must not persist"))
                .is_err()
        );
        let encoded = serde_json::to_value(&updated).unwrap();
        assert!(encoded.get("rootPath").is_none());
        let restored: WorkspaceRecord = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored.primary_source_folder_id, "b");
    }

    #[test]
    fn project_roots_cannot_overlap_or_rebind_stable_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new();
        let original = project(
            "project",
            vec![
                folder("a", &root.path().join("a")),
                folder("b", &root.path().join("b")),
            ],
        );
        store
            .register_project(original.clone(), |_| Ok(()))
            .unwrap();
        let id = original.workspace_id.clone();
        let edit = |folders, primary: &str| WorkspaceProjectEdit {
            name: None,
            source_folders: folders,
            primary_source_folder_id: primary.into(),
        };
        for (folders, primary) in [
            (vec![folder("a", &root.path().join("elsewhere"))], "a"),
            (
                vec![
                    folder("a", &root.path().join("a")),
                    folder("c", &root.path().join("a/nested")),
                ],
                "a",
            ),
            (original.source_folders.clone(), "missing"),
            (vec![], "a"),
        ] {
            assert!(
                store
                    .edit_project(&id, 1, edit(folders, primary), |_| panic!(
                        "invalid edit must not persist"
                    ))
                    .is_err()
            );
        }
        let other = project("other", vec![folder("c", &root.path().join("a"))]);
        assert!(
            store
                .register_project(other, |_| panic!("overlap must not persist"))
                .is_err()
        );
        let retained = store.project(&id).unwrap();
        assert_eq!(retained.revision, 1);
        assert_eq!(retained.source_folders, original.source_folders);
    }

    #[test]
    fn failed_project_registration_and_removal_never_publish_partial_state() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new();
        let project = project("project", vec![folder("a", &root.path().join("a"))]);
        let failure = || DomainError::Persistence {
            message: "injected failure".into(),
        };
        assert!(
            store
                .register_project(project.clone(), |_| Err(failure()))
                .is_err()
        );
        assert!(store.is_empty());
        assert!(store.active_workspace_id().is_none());
        store.register_project(project.clone(), |_| Ok(())).unwrap();
        assert!(
            store
                .deregister_with_persistence(&project.workspace_id, |_| Err(failure()))
                .is_err()
        );
        assert!(store.project(&project.workspace_id).is_some());
        assert_eq!(store.active_workspace_id(), Some(project.workspace_id));
    }

    #[test]
    fn legacy_single_root_has_deterministic_identity_and_rejects_mixed_formats() {
        let legacy = serde_json::json!({ "workspaceId":"legacy", "name":"history", "rootPath":"/old",
            "rootPathRef":"mhp1:legacy", "worktreeRoot":null, "status":"Registered", "createdAt":1, "updatedAt":2 });
        let first: WorkspaceRecord = serde_json::from_value(legacy.clone()).unwrap();
        let second: WorkspaceRecord = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(first.source_folders, second.source_folders);
        assert_eq!(first.primary_source_folder_id, "legacy-source-0");
        let mut mixed = serde_json::to_value(first).unwrap();
        mixed["rootPath"] = legacy["rootPath"].clone();
        assert!(serde_json::from_value::<WorkspaceRecord>(mixed).is_err());
    }
}
