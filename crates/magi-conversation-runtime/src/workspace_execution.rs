//! 一次资源准备的派生投影。目录、隔离副本、Git 与账本仍由原业务服务拥有。
use magi_core::{ProjectDirectoryBinding, ProjectDirectoryExecutionMap};
use magi_snapshot::SnapshotSession;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspacePreparationMode {
    Mapping,
    Execution,
}

#[derive(Clone, Default)]
pub struct WorkspaceExecutionResources {
    pub directories: Option<ProjectDirectoryExecutionMap>,
    pub snapshots: Vec<Arc<SnapshotSession>>,
}

impl WorkspaceExecutionResources {
    pub fn validate_for_binding(&self, binding: &ProjectDirectoryBinding) -> Result<(), String> {
        let map = self.directories.as_ref().ok_or("缺少完整项目执行映射")?;
        map.validate_for_binding(binding)
            .map_err(|error| error.to_string())?;
        let context = magi_tool_runtime::ToolExecutionContext {
            workspace_id: Some(binding.workspace_id.clone()),
            project_directory_map: Some(map.clone()),
            ..Default::default()
        };
        context
            .for_tool_arguments("{}")
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    pub fn from_snapshot(snapshot: Arc<SnapshotSession>) -> Self {
        Self {
            directories: None,
            snapshots: vec![snapshot],
        }
    }

    pub fn default_root(&self) -> Result<Option<PathBuf>, String> {
        if let Some(map) = &self.directories {
            return map
                .root(None)
                .and_then(|root| root.execution_root())
                .map(Some)
                .map_err(|error| error.to_string());
        }
        match self.snapshots.as_slice() {
            [] => Ok(None),
            [snapshot] => Ok(Some(snapshot.workspace_root().to_path_buf())),
            _ => Err("多目录账本缺少明确执行映射".into()),
        }
    }
}
