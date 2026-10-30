//! 将旧项目本地 projection 单向迁移至按项目 ID 固定的状态根。
//! 原文件保留，事务清单先落盘；布局写屏障让旧 daemon 拒绝写入新状态。
use super::{DaemonError, StateRepository};
use magi_workspace::WorkspaceDurableState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectStorageMigration {
    schema_version: u32,
    registry: WorkspaceDurableState,
    copies: Vec<ProjectStorageCopy>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectStorageCopy {
    #[serde(default)]
    kind: ProjectStorageCopyKind,
    workspace_id: String,
    relative_path: PathBuf,
    source: PathBuf,
    sha256: Vec<u8>,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProjectStorageCopyKind {
    #[default]
    Projection,
    Memory,
}

impl StateRepository {
    pub(crate) fn project_storage_root(&self, workspace_id: &str) -> PathBuf {
        magi_core::paths::project_state_root(
            &self.state_root,
            &magi_core::WorkspaceId::new(workspace_id),
        )
    }

    pub(crate) fn migrate_project_storage(&self) -> Result<(), DaemonError> {
        let _guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let journal_path = self.state_root.join("project-storage-migration.json");
        let registry_path = self.state_root.join("workspaces.json");
        let migration = if journal_path.exists() {
            self.read_json_strict::<ProjectStorageMigration>(&journal_path)?
        } else {
            let raw = match fs::read(&registry_path) {
                Ok(raw) => raw,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let value: serde_json::Value = serde_json::from_slice(&raw)?;
            let registry: WorkspaceDurableState = serde_json::from_slice(&raw)?;
            let mut copies = Vec::new();
            let mut has_migration = false;
            let mut has_legacy = false;
            for project in value
                .get("workspaces")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = project
                    .get("workspaceId")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| DaemonError::internal("旧项目缺少 workspaceId"))?;
                let record = registry
                    .workspaces
                    .iter()
                    .find(|record| record.workspace_id.as_str() == id)
                    .ok_or_else(|| DaemonError::internal("旧项目身份无法解析"))?;
                let legacy = project.get("rootPath").is_some();
                let historical_source = record.source_folder(&format!("{id}-source-0"));
                if !legacy {
                    // 旧源目录可在全新 state root 重新注册。注册入口已写成新格式，
                    // 原始源目录身份仍可证明历史来源；沿同一迁移事务导入一次。
                    // 一旦固定项目根存在，它就是唯一事实源，不能再用保留的旧文件
                    // 恢复已删除会话、覆盖新历史或把后来添加的成员当作历史来源。
                    if historical_source.is_none()
                        || self.project_storage_root(id).try_exists()?
                        || !historical_source
                            .expect("checked historical source")
                            .native_path()
                            .join(".magi/session-projections")
                            .try_exists()?
                    {
                        continue;
                    }
                }
                has_migration = true;
                has_legacy |= legacy;
                let root = historical_source
                    .ok_or_else(|| DaemonError::internal("旧项目缺少原源文件夹身份"))?
                    .native_path();
                // 缺失目录不能因恢复而被创建；历史迁移仍可从 canonical events 恢复。
                if root.is_dir() {
                    magi_workspace::verify_or_create_workspace_identity(
                        &root,
                        &record.workspace_id,
                    )
                    .map_err(|error| {
                        DaemonError::internal(format!("校验旧项目身份失败: {error}"))
                    })?;
                }
                let magi = root.join(".magi");
                let projections = magi.join("session-projections");
                if projections.exists() {
                    for entry in fs::read_dir(&projections)? {
                        let path = entry?.path();
                        if path.extension().and_then(|extension| extension.to_str()) != Some("json")
                        {
                            continue;
                        }
                        let content = fs::read(&path)?;
                        // 迁移只认项目归属，不解析或更改 canonical 历史；损坏单会话在原加载入口报告。
                        copies.push(ProjectStorageCopy {
                            kind: ProjectStorageCopyKind::Projection,
                            workspace_id: id.into(),
                            relative_path: PathBuf::from("session-projections").join(
                                path.file_name().ok_or_else(|| {
                                    DaemonError::internal("旧 projection 文件名无效")
                                })?,
                            ),
                            source: path,
                            sha256: Sha256::digest(&content).to_vec(),
                        });
                    }
                }
                let meta = magi.join("session-workspace-meta.json");
                if meta.exists() {
                    copies.push(ProjectStorageCopy {
                        kind: ProjectStorageCopyKind::Projection,
                        workspace_id: id.into(),
                        relative_path: PathBuf::from("session-workspace-meta.json"),
                        sha256: Sha256::digest(fs::read(&meta)?).to_vec(),
                        source: meta,
                    });
                }
                let memory_root = magi_core::paths::project_memory_root(
                    &self.state_root,
                    &magi_core::WorkspaceRootPath::new(root.to_string_lossy()),
                );
                if memory_root.is_dir() {
                    for entry in fs::read_dir(&memory_root)? {
                        let path = entry?.path();
                        // ProjectMemory 的公开磁盘合同是平铺的 Markdown 索引/条目。
                        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                            continue;
                        }
                        let name = path
                            .file_name()
                            .ok_or_else(|| DaemonError::internal("旧记忆文件名无效"))?;
                        copies.push(ProjectStorageCopy {
                            kind: ProjectStorageCopyKind::Memory,
                            workspace_id: id.into(),
                            relative_path: PathBuf::from(name),
                            sha256: Sha256::digest(fs::read(&path)?).to_vec(),
                            source: path,
                        });
                    }
                }
            }
            if !has_migration {
                return Ok(());
            }
            let backup = self.state_root.join("workspaces.pre-multi-root.json");
            if has_legacy && !backup.exists() {
                magi_core::fs_atomic::write_atomic(&backup, &raw)?;
            }
            let migration = ProjectStorageMigration {
                schema_version: 1,
                registry,
                copies,
            };
            self.write_json_atomically_locked(journal_path.clone(), &migration)?;
            migration
        };
        if migration.schema_version != 1 {
            return Err(DaemonError::internal("不支持的项目存储迁移版本"));
        }
        // 热注册期间失败的迁移可能晚于其它项目管理请求恢复；清单只描述复制来源，
        // 不能把它保存的旧 registry 快照重新覆盖到当前项目事实。
        let registry: WorkspaceDurableState = self.read_json_strict(&registry_path)?;
        for copy in &migration.copies {
            let record = registry
                .workspaces
                .iter()
                .find(|project| project.workspace_id.as_str() == copy.workspace_id)
                .ok_or_else(|| DaemonError::internal("迁移引用未知项目"))?;
            let root = record
                .source_folder(&format!("{}-source-0", copy.workspace_id))
                .ok_or_else(|| DaemonError::internal("迁移缺少原源文件夹身份"))?
                .native_path();
            let relative = &copy.relative_path;
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(DaemonError::internal("项目存储迁移包含无效路径"));
            }
            let target = match copy.kind {
                ProjectStorageCopyKind::Projection => {
                    if relative.components().count() > 2
                        || !(relative == &PathBuf::from("session-workspace-meta.json")
                            || (relative.starts_with("session-projections")
                                && relative.components().count() == 2))
                        || copy.source != root.join(".magi").join(relative)
                    {
                        return Err(DaemonError::internal("项目投影迁移包含无效路径"));
                    }
                    self.project_storage_root(&copy.workspace_id)
                        .join(".magi")
                        .join(relative)
                }
                ProjectStorageCopyKind::Memory => {
                    let legacy = magi_core::paths::project_memory_root(
                        &self.state_root,
                        &magi_core::WorkspaceRootPath::new(root.to_string_lossy()),
                    );
                    if relative.components().count() != 1
                        || relative
                            .extension()
                            .and_then(|extension| extension.to_str())
                            != Some("md")
                        || copy.source != legacy.join(relative)
                    {
                        return Err(DaemonError::internal("项目记忆迁移包含无效路径"));
                    }
                    self.project_storage_root(&copy.workspace_id)
                        .join("memory")
                        .join(relative)
                }
            };
            let source = fs::read(&copy.source)?;
            if Sha256::digest(&source).as_slice() != copy.sha256 {
                return Err(DaemonError::internal(format!(
                    "迁移源文件已变化，原文件保留: {}",
                    copy.source.display()
                )));
            }
            match fs::read(&target) {
                Ok(existing) if existing == source => continue,
                Ok(_) => {
                    return Err(DaemonError::internal(format!(
                        "迁移目标与原文件冲突，拒绝覆盖: {}",
                        target.display()
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            fs::create_dir_all(target.parent().expect("target has parent"))?;
            magi_core::fs_atomic::write_atomic(&target, &source)?;
        }
        self.write_json_atomically_locked(registry_path, &registry)?;
        Self::remove_file_durable(&journal_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{AbsolutePath, SessionId, WorkspaceId};
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;

    fn legacy_fixture(root: &std::path::Path) -> (StateRepository, PathBuf, String, Vec<u8>) {
        let repository = StateRepository::new(root.join("state"));
        let source = root.join("source");
        fs::create_dir_all(source.join(".magi/session-projections")).unwrap();
        let source = source.canonicalize().unwrap();
        let projects = WorkspaceStore::new();
        let id = WorkspaceId::new("project/mixed:身份");
        let record = projects
            .register(id.clone(), AbsolutePath::new(source.to_string_lossy()))
            .unwrap();
        let mut legacy = serde_json::to_value(projects.durable_state()).unwrap();
        let folder = record.primary_source_folder();
        legacy["workspaces"][0] = serde_json::json!({ "workspaceId":id, "name":"old project", "rootPath":folder.path,
            "rootPathRef":folder.path_ref,"status":"Registered","worktreeRoot":null,"createdAt":1,"updatedAt":2 });
        fs::create_dir_all(&repository.state_root).unwrap();
        fs::write(
            repository.state_root.join("workspaces.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();
        let sessions = SessionStore::new();
        let session_id = SessionId::new("old-session");
        sessions
            .create_session_for_workspace(session_id.clone(), "kept history", Some(id.to_string()))
            .unwrap();
        let projection = super::super::SessionProjectionSnapshot {
            canonical_event_seq: 0,
            durable: sessions
                .durable_state()
                .durable_state_for_session(&session_id),
            sidecar: None,
        };
        let content = serde_json::to_vec_pretty(&projection).unwrap();
        fs::write(
            source.join(".magi/session-projections/old-session.json"),
            &content,
        )
        .unwrap();
        (repository, source, id.to_string(), content)
    }

    #[test]
    fn project_storage_migration_restarts_without_source_directory_and_preserves_identity() {
        let root = tempfile::tempdir().unwrap();
        let (repository, source, id, content) = legacy_fixture(root.path());
        let roots = vec![(id.clone(), source.clone())];
        repository.verify_state_layout(&roots).unwrap();
        repository.migrate_project_storage().unwrap();
        repository.migrate_project_storage().unwrap();
        let target = repository
            .project_storage_root(&id)
            .join(".magi/session-projections/old-session.json");
        assert_eq!(fs::read(&target).unwrap(), content);
        assert_eq!(
            fs::read(source.join(".magi/session-projections/old-session.json")).unwrap(),
            content
        );
        let registry = repository.load_workspace_durable_state().unwrap();
        assert_eq!(registry.workspaces[0].workspace_id.as_str(), id);
        let raw: serde_json::Value = serde_json::from_slice(
            &fs::read(repository.state_root.join("workspaces.json")).unwrap(),
        )
        .unwrap();
        assert!(raw["workspaces"][0].get("rootPath").is_none());
        fs::remove_dir_all(&source).unwrap();
        let restarted = StateRepository::new(repository.state_root.clone());
        let (durable, _) = restarted.load_session_projections(&roots).unwrap();
        assert_eq!(durable.sessions.len(), 1);
        assert_eq!(durable.sessions[0].title, "kept history");
        assert_eq!(
            durable.sessions[0].workspace_id.as_deref(),
            Some(id.as_str())
        );
        assert!(!source.exists());
    }

    #[test]
    fn project_storage_imports_reregistered_portable_history_only_into_fresh_storage() {
        let root = tempfile::tempdir().unwrap();
        let (repository, source, id, content) = legacy_fixture(root.path());
        // 与 /workspaces/register 一样：从源目录历史解析稳定身份，写入新格式注册表。
        let identity = magi_workspace::resolve_or_create_workspace_identity(
            &source,
            WorkspaceId::new("must-not-replace-existing-identity"),
        )
        .unwrap();
        assert_eq!(identity.as_str(), id);
        let registered = WorkspaceStore::new();
        registered
            .register_native_path_with_persistence(identity, source.clone(), |state| {
                repository
                    .save_workspace_durable_state(state)
                    .map_err(|error| magi_core::DomainError::Persistence {
                        message: error.to_string(),
                    })
            })
            .unwrap();
        let roots = vec![(id.clone(), source.clone())];
        repository.verify_state_layout(&roots).unwrap();
        repository.migrate_project_storage().unwrap();
        let target = repository
            .project_storage_root(&id)
            .join(".magi/session-projections/old-session.json");
        assert_eq!(fs::read(&target).unwrap(), content);
        let (restored, _) = repository.load_session_projections(&roots).unwrap();
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(
            restored.sessions[0].workspace_id.as_deref(),
            Some(id.as_str())
        );
        assert!(
            !repository
                .state_root
                .join("workspaces.pre-multi-root.json")
                .exists()
        );

        // 用户在新状态根删除历史后，旧源目录仍保留迁移备份，不能再次复活。
        fs::remove_file(&target).unwrap();
        let restarted = StateRepository::new(repository.state_root.clone());
        restarted.migrate_project_storage().unwrap();
        assert!(
            restarted
                .load_session_projections(&roots)
                .unwrap()
                .0
                .sessions
                .is_empty()
        );
        assert!(!target.exists());
        assert_eq!(
            fs::read(source.join(".magi/session-projections/old-session.json")).unwrap(),
            content
        );
    }

    #[test]
    fn project_storage_portable_import_does_not_guess_edited_source_identity() {
        let root = tempfile::tempdir().unwrap();
        let (repository, source, id, _) = legacy_fixture(root.path());
        let store = WorkspaceStore::new();
        let record = store
            .register_native_path(WorkspaceId::new(&id), source)
            .unwrap();
        let mut changed = record.clone();
        changed.source_folders[0].source_folder_id = "removed-source-replacement".into();
        changed.primary_source_folder_id = "removed-source-replacement".into();
        let mut renamed_source = record.clone();
        renamed_source.source_folders[0].source_folder_id = "new-source".into();
        renamed_source.primary_source_folder_id = "new-source".into();
        for project in [changed, renamed_source] {
            let mut snapshot = store.durable_state();
            snapshot.workspaces = vec![project];
            repository.save_workspace_durable_state(&snapshot).unwrap();
            repository.migrate_project_storage().unwrap();
            assert!(!repository.project_storage_root(&id).exists());
        }
    }

    #[test]
    fn project_storage_does_not_follow_primary_folder_or_project_name() {
        let root = tempfile::tempdir().unwrap();
        let (repository, source, id, content) = legacy_fixture(root.path());
        let legacy_memory = magi_core::paths::project_memory_root(
            &repository.state_root,
            &magi_core::WorkspaceRootPath::new(source.to_string_lossy()),
        );
        fs::create_dir_all(&legacy_memory).unwrap();
        fs::write(
            legacy_memory.join("MEMORY.md"),
            "# project memory\n- [rules](rules.md)\n",
        )
        .unwrap();
        let memory_entry = b"---\nname: retained\ndescription: migration\nkind: project\n---\nRetained across directory changes.\n";
        fs::write(legacy_memory.join("rules.md"), memory_entry).unwrap();
        repository
            .verify_state_layout(&[(id.clone(), source.clone())])
            .unwrap();
        repository.migrate_project_storage().unwrap();
        let store = WorkspaceStore::from_persisted_parts(
            repository.load_workspace_durable_state().unwrap(),
            Default::default(),
        );
        let current = store.project(&WorkspaceId::new(&id)).unwrap();
        let second_root = root.path().join("second-source");
        fs::create_dir_all(&second_root).unwrap();
        let host = magi_core::HostPath::from_path(second_root.clone());
        let mut second = current.primary_source_folder().clone();
        second.source_folder_id = "second".into();
        second.path = AbsolutePath::new(host.display_string());
        second.path_ref = host.to_path_ref().as_str().into();
        store
            .edit_project(
                &current.workspace_id,
                current.revision,
                magi_workspace::WorkspaceProjectEdit {
                    name: Some("renamed".into()),
                    source_folders: vec![second],
                    primary_source_folder_id: "second".into(),
                },
                |snapshot| {
                    repository
                        .save_workspace_durable_state(snapshot)
                        .map_err(|error| magi_core::DomainError::Persistence {
                            message: error.to_string(),
                        })
                },
            )
            .unwrap();
        fs::remove_dir_all(&source).unwrap();
        let restarted = StateRepository::new(repository.state_root.clone());
        restarted.migrate_project_storage().unwrap();
        let current = restarted
            .load_workspace_durable_state()
            .unwrap()
            .workspaces
            .remove(0);
        assert_eq!(current.name.as_deref(), Some("renamed"));
        assert_eq!(
            current.native_root_path(),
            second_root.canonicalize().unwrap()
        );
        let memory_root =
            magi_core::paths::project_state_root(&restarted.state_root, &current.workspace_id)
                .join("memory");
        assert_eq!(
            fs::read(memory_root.join("rules.md")).unwrap(),
            memory_entry
        );
        assert_eq!(
            fs::read(legacy_memory.join("rules.md")).unwrap(),
            memory_entry
        );
        let (durable, _) = restarted
            .load_session_projections(&[(id.clone(), current.native_root_path())])
            .unwrap();
        assert_eq!(durable.sessions.len(), 1);
        assert_eq!(durable.sessions[0].session_id.as_str(), "old-session");
        assert_eq!(
            fs::read(
                restarted
                    .project_storage_root(&id)
                    .join(".magi/session-projections/old-session.json")
            )
            .unwrap(),
            content
        );
        assert!(!second_root.join(".magi/session-projections").exists());
    }

    #[test]
    fn project_storage_migration_conflict_retains_journal_and_resumes_after_repair() {
        let root = tempfile::tempdir().unwrap();
        let (repository, source, id, content) = legacy_fixture(root.path());
        let target = repository
            .project_storage_root(&id)
            .join(".magi/session-projections/old-session.json");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"existing data").unwrap();
        repository
            .verify_state_layout(&[(id.clone(), source.clone())])
            .unwrap();
        assert!(repository.migrate_project_storage().is_err());
        assert_eq!(fs::read(&target).unwrap(), b"existing data");
        assert!(
            repository
                .state_root
                .join("project-storage-migration.json")
                .exists()
        );
        let old: serde_json::Value = serde_json::from_slice(
            &fs::read(repository.state_root.join("workspaces.json")).unwrap(),
        )
        .unwrap();
        assert!(old["workspaces"][0].get("rootPath").is_some());
        // 热注册后迁移失败期间，另一个项目仍可正常注册；恢复旧清单不能回滚它。
        let store = WorkspaceStore::from_persisted_parts(
            repository.load_workspace_durable_state().unwrap(),
            Default::default(),
        );
        let another = root.path().join("another");
        fs::create_dir_all(&another).unwrap();
        store
            .register_native_path_with_persistence(
                WorkspaceId::new("registered-after-failure"),
                another,
                |snapshot| {
                    repository
                        .save_workspace_durable_state(snapshot)
                        .map_err(|error| magi_core::DomainError::Persistence {
                            message: error.to_string(),
                        })
                },
            )
            .unwrap();
        // 模拟用户修复冲突后重启；已写入的相同内容可幂等跳过。
        fs::write(&target, &content).unwrap();
        StateRepository::new(repository.state_root.clone())
            .migrate_project_storage()
            .unwrap();
        assert!(
            !repository
                .state_root
                .join("project-storage-migration.json")
                .exists()
        );
        assert_eq!(fs::read(&target).unwrap(), content);
        assert_eq!(
            repository
                .load_workspace_durable_state()
                .unwrap()
                .workspaces
                .len(),
            2
        );
        assert!(
            source
                .join(".magi/session-projections/old-session.json")
                .exists()
        );
    }
}
