use crate::WorkspaceStore;
use magi_core::{
    DomainError, DomainResult, HostPath, ProjectDirectoryBinding, ProjectSourceFolderGrant,
    WorkspaceId,
};
use std::path::PathBuf;

impl WorkspaceStore {
    /// 对面向用户的单目录资源请求解析明确目标。多目录旧请求没有目录身份时拒绝猜测。
    pub fn resolve_source_folder(
        &self,
        workspace_id: &WorkspaceId,
        source_folder_id: Option<&str>,
    ) -> DomainResult<(crate::WorkspaceSourceFolder, PathBuf)> {
        let project = self.project(workspace_id).ok_or(DomainError::NotFound {
            entity: "workspace",
        })?;
        let id = match source_folder_id {
            Some(id) if !id.trim().is_empty() => id.trim(),
            Some(_) => {
                return Err(DomainError::Validation {
                    message: "源文件夹 ID 不能为空".into(),
                });
            }
            None if project.source_folders.len() == 1 => &project.primary_source_folder_id,
            None => {
                return Err(DomainError::Validation {
                    message: "多源目录项目必须指定 sourceFolderId".into(),
                });
            }
        };
        let folder = project.source_folder(id).ok_or(DomainError::NotFound {
            entity: "workspace_source_folder",
        })?;
        let path = HostPath::canonicalize(folder.native_path())
            .map_err(|_| DomainError::Validation {
                message: "源文件夹不可用".into(),
            })?
            .into_path_buf();
        if !path.is_dir() {
            return Err(DomainError::Validation {
                message: "源文件夹不是目录".into(),
            });
        }
        if path != folder.native_path() {
            return Err(DomainError::InvalidState {
                message: "源文件夹原登记路径已变化，请移除后重新添加".into(),
            });
        }
        Ok((folder.clone(), path))
    }

    /// 只在新输入/新授权的接纳入口捕获。已有任务必须恢复原绑定，不重新捕获当前成员。
    pub fn capture_directory_binding(
        &self,
        workspace_id: &WorkspaceId,
        default_source_folder_id: Option<&str>,
        authorized_source_folder_ids: Option<&[String]>,
    ) -> DomainResult<ProjectDirectoryBinding> {
        let project = self.project(workspace_id).ok_or(DomainError::NotFound {
            entity: "workspace",
        })?;
        let default_id = default_source_folder_id.unwrap_or(&project.primary_source_folder_id);
        let ids = authorized_source_folder_ids
            .map(Vec::from)
            .unwrap_or_else(|| {
                project
                    .source_folders
                    .iter()
                    .map(|folder| folder.source_folder_id.clone())
                    .collect()
            });
        let mut grants = Vec::new();
        for id in ids {
            let folder = project.source_folder(&id).ok_or(DomainError::NotFound {
                entity: "workspace_source_folder",
            })?;
            // 捕获成员身份不准备执行资源。离线目录仍可用于纯对话和队列事实；
            // 实际读取、写入与恢复执行必须经过 validate_directory_binding。
            let path = HostPath::from_path(folder.native_path());
            grants.push(ProjectSourceFolderGrant {
                source_folder_id: id,
                source_path_ref: path.to_path_ref().as_str().into(),
            });
        }
        let binding = ProjectDirectoryBinding {
            workspace_id: workspace_id.clone(),
            project_revision: project.revision,
            default_source_folder_id: default_id.into(),
            source_folders: grants,
        };
        binding.validate()?;
        self.validate_directory_membership(&binding)?;
        Ok(binding)
    }

    /// 输入接纳/排队恢复只核对成员身份，不为纯对话准备或创建磁盘资源。
    pub fn validate_directory_membership(
        &self,
        binding: &ProjectDirectoryBinding,
    ) -> DomainResult<()> {
        binding.validate()?;
        let project = self
            .project(&binding.workspace_id)
            .ok_or(DomainError::NotFound {
                entity: "workspace",
            })?;
        if binding.project_revision > project.revision {
            return Err(DomainError::InvalidState {
                message: "目录绑定的项目修订超前".into(),
            });
        }
        for grant in &binding.source_folders {
            let folder = project
                .source_folder(&grant.source_folder_id)
                .ok_or_else(|| DomainError::InvalidState {
                    message: format!("源文件夹已撤销: {}", grant.source_folder_id),
                })?;
            let current = HostPath::canonicalize(folder.native_path())
                .unwrap_or_else(|_| HostPath::from_path(folder.native_path()));
            if current.to_path_ref().as_str() != grant.source_path_ref {
                return Err(DomainError::InvalidState {
                    message: format!("源文件夹路径已变化: {}", grant.source_folder_id),
                });
            }
        }
        Ok(())
    }

    /// 实际执行和恢复重新核对成员与目录可用性，撤销、缺失均不回落其他目录。
    pub fn validate_directory_binding(
        &self,
        binding: &ProjectDirectoryBinding,
    ) -> DomainResult<()> {
        self.validate_directory_membership(binding)?;
        for grant in &binding.source_folders {
            let root = HostPath::from_path_ref(&grant.source_path_ref).map_err(|_| {
                DomainError::Validation {
                    message: "目录绑定路径无效".into(),
                }
            })?;
            if !root.as_path().is_dir() {
                return Err(DomainError::Validation {
                    message: format!("源文件夹不可用: {}", grant.source_folder_id),
                });
            }
        }
        Ok(())
    }

    pub fn resolve_bound_source_directory(
        &self,
        binding: &ProjectDirectoryBinding,
        source_folder_id: Option<&str>,
    ) -> DomainResult<PathBuf> {
        self.validate_directory_binding(binding)?;
        let grant = binding.grant(source_folder_id)?;
        HostPath::from_path_ref(&grant.source_path_ref)
            .map(HostPath::into_path_buf)
            .map_err(|_| DomainError::Validation {
                message: "源文件夹绑定路径无效".into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceProjectEdit;

    #[test]
    fn directory_binding_does_not_expand_or_follow_primary_and_revocation_is_immediate() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let store = WorkspaceStore::new();
        let id = WorkspaceId::new("project");
        let original = store.register_native_path(id.clone(), a.clone()).unwrap();
        let frozen = store.capture_directory_binding(&id, None, None).unwrap();
        let mut second = original.primary_source_folder().clone();
        second.source_folder_id = "second".into();
        let host = HostPath::canonicalize(&b).unwrap();
        second.path = magi_core::AbsolutePath::new(host.display_string());
        second.path_ref = host.to_path_ref().as_str().into();
        let mut members = original.source_folders.clone();
        members.push(second.clone());
        store
            .edit_project(
                &id,
                1,
                WorkspaceProjectEdit {
                    name: None,
                    source_folders: members,
                    primary_source_folder_id: "second".into(),
                },
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            store.resolve_bound_source_directory(&frozen, None).unwrap(),
            HostPath::canonicalize(&a).unwrap().into_path_buf()
        );
        assert!(
            store
                .resolve_bound_source_directory(&frozen, Some("second"))
                .is_err()
        );
        assert_eq!(
            store
                .capture_directory_binding(&id, None, None)
                .unwrap()
                .default_source_folder_id,
            "second"
        );
        let restored: ProjectDirectoryBinding =
            serde_json::from_value(serde_json::to_value(&frozen).unwrap()).unwrap();
        assert_eq!(
            store
                .resolve_bound_source_directory(&restored, None)
                .unwrap(),
            HostPath::canonicalize(&a).unwrap().into_path_buf()
        );
        store
            .edit_project(
                &id,
                2,
                WorkspaceProjectEdit {
                    name: None,
                    source_folders: vec![second],
                    primary_source_folder_id: "second".into(),
                },
                |_| Ok(()),
            )
            .unwrap();
        assert!(
            store
                .resolve_bound_source_directory(&restored, None)
                .is_err()
        );
    }

    #[test]
    #[cfg(unix)]
    fn replaced_source_root_cannot_be_recaptured_or_resolved() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let elsewhere = fixture.path().join("elsewhere");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let store = WorkspaceStore::new();
        let id = WorkspaceId::new("replaced-source");
        store
            .register_native_path(id.clone(), source.clone())
            .unwrap();
        let frozen = store.capture_directory_binding(&id, None, None).unwrap();
        std::fs::remove_dir(&source).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &source).unwrap();
        assert!(store.capture_directory_binding(&id, None, None).is_err());
        assert!(store.resolve_source_folder(&id, None).is_err());
        assert!(store.validate_directory_binding(&frozen).is_err());
    }

    #[test]
    fn missing_bound_directory_never_falls_back_or_gets_recreated() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::create_dir_all(&path).unwrap();
        let store = WorkspaceStore::new();
        let id = WorkspaceId::new("project");
        store
            .register_native_path(id.clone(), path.clone())
            .unwrap();
        let frozen = store.capture_directory_binding(&id, None, None).unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        assert!(store.resolve_bound_source_directory(&frozen, None).is_err());
        assert!(!path.exists());
    }
}
