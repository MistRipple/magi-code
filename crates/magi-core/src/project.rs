//! 任务接纳时冻结的项目目录授权。项目成员事实仍由 WorkspaceStore 拥有；
//! 本类型是原输入/Task/审批恢复的一部分，不能用它绕过当前成员和权限检查。
use crate::{DomainError, DomainResult, HostPath, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSourceFolderGrant {
    pub source_folder_id: String,
    /// 接纳时的规范本机路径，保留非 UTF-8 路径且避免目录 ID 被改绑后静默迁移任务。
    pub source_path_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDirectoryBinding {
    pub workspace_id: WorkspaceId,
    pub project_revision: u64,
    pub default_source_folder_id: String,
    pub source_folders: Vec<ProjectSourceFolderGrant>,
}

/// 可信执行入口根据原隔离/代理注册表生成的目录投影，不是另一份项目注册表。
/// 每个获授源目录必须明确提供执行根，不能在部分准备失败时回落到源目录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDirectoryExecutionRoot {
    pub source_folder_id: String,
    pub source_root_ref: String,
    pub execution_root_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDirectoryExecutionMap {
    pub workspace_id: WorkspaceId,
    pub default_source_folder_id: String,
    pub roots: Vec<ProjectDirectoryExecutionRoot>,
}

impl ProjectDirectoryExecutionMap {
    pub fn from_binding(
        binding: &ProjectDirectoryBinding,
        mut resolve: impl FnMut(&ProjectSourceFolderGrant) -> DomainResult<PathBuf>,
    ) -> DomainResult<Self> {
        binding.validate()?;
        let roots = binding
            .source_folders
            .iter()
            .map(|grant| {
                Ok(ProjectDirectoryExecutionRoot {
                    source_folder_id: grant.source_folder_id.clone(),
                    source_root_ref: grant.source_path_ref.clone(),
                    execution_root_ref: HostPath::from_path(resolve(grant)?)
                        .to_path_ref()
                        .as_str()
                        .into(),
                })
            })
            .collect::<DomainResult<Vec<_>>>()?;
        let map = Self {
            workspace_id: binding.workspace_id.clone(),
            default_source_folder_id: binding.default_source_folder_id.clone(),
            roots,
        };
        map.validate_for_binding(binding)?;
        Ok(map)
    }

    pub fn validate(&self) -> DomainResult<()> {
        let binding = ProjectDirectoryBinding {
            workspace_id: self.workspace_id.clone(),
            project_revision: 1,
            default_source_folder_id: self.default_source_folder_id.clone(),
            source_folders: self
                .roots
                .iter()
                .map(|root| ProjectSourceFolderGrant {
                    source_folder_id: root.source_folder_id.clone(),
                    source_path_ref: root.source_root_ref.clone(),
                })
                .collect(),
        };
        binding.validate()?;
        let mut executions = std::collections::HashSet::new();
        for root in &self.roots {
            let execution = root.execution_root()?;
            if !execution.is_absolute() || !executions.insert(execution.clone()) {
                return Err(DomainError::Validation {
                    message: "执行映射包含重复或非绝对的目录".into(),
                });
            }
            for other in &self.roots {
                if root.source_folder_id == other.source_folder_id {
                    continue;
                }
                let source = other.source_root()?;
                let other_execution = other.execution_root()?;
                if execution.starts_with(&source)
                    || source.starts_with(&execution)
                    || execution.starts_with(&other_execution)
                    || other_execution.starts_with(&execution)
                {
                    return Err(DomainError::Validation {
                        message: "不同源文件夹的执行目录不能重叠或落入其它源目录".into(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn validate_for_binding(&self, binding: &ProjectDirectoryBinding) -> DomainResult<()> {
        self.validate()?;
        binding.validate()?;
        if self.workspace_id != binding.workspace_id
            || self.default_source_folder_id != binding.default_source_folder_id
            || self.roots.len() != binding.source_folders.len()
            || self.roots.iter().any(|root| {
                !binding.source_folders.iter().any(|grant| {
                    root.source_folder_id == grant.source_folder_id
                        && root.source_root_ref == grant.source_path_ref
                })
            })
        {
            return Err(DomainError::Validation {
                message: "实际执行目录必须与本次项目授权逐项匹配".into(),
            });
        }
        Ok(())
    }

    pub fn root(
        &self,
        source_folder_id: Option<&str>,
    ) -> DomainResult<&ProjectDirectoryExecutionRoot> {
        self.validate()?;
        let id = source_folder_id.unwrap_or(&self.default_source_folder_id);
        self.roots
            .iter()
            .find(|root| root.source_folder_id == id)
            .ok_or_else(|| DomainError::Validation {
                message: format!("源文件夹未获本次执行授权: {id}"),
            })
    }

    /// 只投影完整路径前缀；源目录中的符号链接仍由访问权限层检查其真实目标。
    pub fn map_source_path(&self, path: &Path) -> DomainResult<PathBuf> {
        self.validate()?;
        let mut matched = None;
        for root in &self.roots {
            let source = root.source_root()?;
            if path.starts_with(&source)
                && matched
                    .as_ref()
                    .is_none_or(|(previous, _): &(PathBuf, PathBuf)| {
                        source.components().count() > previous.components().count()
                    })
            {
                matched = Some((source, root.execution_root()?));
            }
        }
        Ok(match matched {
            Some((source, execution)) => {
                execution.join(path.strip_prefix(&source).expect("matched prefix"))
            }
            None => path.to_path_buf(),
        })
    }
}

impl ProjectDirectoryExecutionRoot {
    pub fn source_root(&self) -> DomainResult<PathBuf> {
        Self::decode_root(&self.source_root_ref)
    }

    pub fn execution_root(&self) -> DomainResult<PathBuf> {
        Self::decode_root(&self.execution_root_ref)
    }

    fn decode_root(path_ref: &str) -> DomainResult<PathBuf> {
        HostPath::from_path_ref(path_ref)
            .map(HostPath::into_path_buf)
            .map_err(|_| DomainError::Validation {
                message: "项目执行映射包含无效本机路径".into(),
            })
    }
}

impl ProjectDirectoryBinding {
    pub fn validate(&self) -> DomainResult<()> {
        let mut ids = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        if self.workspace_id.as_str().trim().is_empty()
            || self.project_revision == 0
            || self.source_folders.is_empty()
            || !self
                .source_folders
                .iter()
                .any(|folder| folder.source_folder_id == self.default_source_folder_id)
        {
            return Err(DomainError::Validation {
                message: "项目目录绑定缺少身份、修订或默认目录".into(),
            });
        }
        for grant in &self.source_folders {
            let path = HostPath::from_path_ref(&grant.source_path_ref).map_err(|_| {
                DomainError::Validation {
                    message: "项目目录绑定包含无效本机路径".into(),
                }
            })?;
            if grant.source_folder_id.trim().is_empty()
                || !path.as_path().is_absolute()
                || !ids.insert(&grant.source_folder_id)
                || !paths.insert(path.into_path_buf())
            {
                return Err(DomainError::Validation {
                    message: "项目目录绑定包含重复或无效目录".into(),
                });
            }
        }
        Ok(())
    }

    pub fn grant(&self, source_folder_id: Option<&str>) -> DomainResult<&ProjectSourceFolderGrant> {
        self.validate()?;
        let id = source_folder_id.unwrap_or(&self.default_source_folder_id);
        self.source_folders
            .iter()
            .find(|folder| folder.source_folder_id == id)
            .ok_or_else(|| DomainError::Validation {
                message: format!("源文件夹未获本次授权: {id}"),
            })
    }

    pub fn validate_inherited_from(&self, parent: &Self) -> DomainResult<()> {
        self.validate()?;
        parent.validate()?;
        if self.workspace_id != parent.workspace_id
            || self.project_revision != parent.project_revision
            || self
                .source_folders
                .iter()
                .any(|grant| !parent.source_folders.contains(grant))
        {
            return Err(DomainError::Validation {
                message: "子任务的项目目录授权不能超出父任务".into(),
            });
        }
        Ok(())
    }
}
