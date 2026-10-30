//! 工具参数只能选择可信入口已授予的源目录，不能创建或替换执行映射。
use crate::{ToolExecutionContext, ToolExecutionPolicy};
use magi_core::{DomainError, DomainResult, HostPath};
use std::path::{Path, PathBuf};

impl ToolExecutionContext {
    /// 预检、审批恢复和真正执行必须用同一份目标解析。相对路径与省略 cwd 都
    /// 从选中目录的执行根出发；省略目录 ID 使用接纳时冻结的默认目录。
    pub fn for_tool_arguments(&self, arguments: &str) -> DomainResult<Self> {
        let value = serde_json::from_str::<serde_json::Value>(arguments).ok();
        let requested = value
            .as_ref()
            .and_then(|value| value.get("source_folder_id"));
        let requested = match requested {
            None => None,
            Some(serde_json::Value::String(id)) if !id.trim().is_empty() => Some(id.as_str()),
            Some(_) => return Err(invalid("source_folder_id 必须是非空源目录 ID")),
        };
        let mut resolved = self.clone();
        let Some(map) = &self.project_directory_map else {
            if requested.is_some() {
                return Err(invalid("当前工具调用没有可信项目目录映射"));
            }
            return Ok(resolved);
        };
        map.validate()?;
        if self.workspace_id.as_ref() != Some(&map.workspace_id) {
            return Err(invalid("工具的项目身份与目录映射不匹配"));
        }
        for root in &map.roots {
            // 不能因副本消失而重建空目录，也不能跟随后来替换的符号链接。
            let execution = root.execution_root()?;
            let canonical =
                std::fs::canonicalize(&execution).map_err(|_| invalid("本次执行目录已不可用"))?;
            if !canonical.is_dir() || canonical != execution {
                return Err(invalid("本次执行目录身份已变化"));
            }
        }
        resolved.working_directory = Some(map.root(requested)?.execution_root()?);
        Ok(resolved)
    }

    /// 源目录路径策略投影到全部实际执行根。相对策略仍以冻结默认源目录为起点，
    /// 选择另一目录不会把同一条相对策略重新解释为另一份授权。
    pub fn project_execution_policy(
        &self,
        policy: &ToolExecutionPolicy,
    ) -> DomainResult<ToolExecutionPolicy> {
        let Some(map) = &self.project_directory_map else {
            return Ok(policy.clone());
        };
        map.validate()?;
        let default_source = map.root(None)?.source_root()?;
        let project = |paths: &[String]| -> DomainResult<Vec<String>> {
            paths
                .iter()
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    let source = crate::normalize_tool_policy_paths(
                        std::slice::from_ref(value),
                        Some(&default_source),
                    )
                    .into_iter()
                    .next()
                    .ok_or_else(|| invalid("路径策略包含无效本机路径"))?;
                    Ok(HostPath::from_path(map.map_source_path(&source)?)
                        .to_path_ref()
                        .as_str()
                        .to_string())
                })
                .collect()
        };
        let mut projected = policy.clone();
        projected.allowed_paths = project(&policy.allowed_paths)?;
        projected.denied_paths = project(&policy.denied_paths)?;
        projected.read_only_paths = project(&policy.read_only_paths)?;
        Ok(projected)
    }

    /// 默认目录范围包含本次授权的全部实际根，绝不包含它们的共同祖先。
    pub fn project_execution_roots(&self) -> DomainResult<Vec<PathBuf>> {
        self.project_directory_map
            .as_ref()
            .map(|map| {
                map.validate()?;
                map.roots.iter().map(|root| root.execution_root()).collect()
            })
            .unwrap_or_else(|| Ok(self.working_directory.iter().cloned().collect()))
    }

    /// 完全授权也不能写回被隔离的原目录。工具对外接收的绝对路径应指向实际
    /// 执行副本；不重写 Shell 命令字符串，防止预检和真正执行理解不同路径。
    pub fn references_retired_project_root(&self, path: &Path) -> DomainResult<bool> {
        let Some(map) = &self.project_directory_map else {
            return Ok(false);
        };
        let path = crate::canonicalize_tool_permission_path(path);
        for root in &map.roots {
            let source = root.source_root()?;
            let execution = root.execution_root()?;
            if source != execution && path.starts_with(&source) && !path.starts_with(&execution) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn invalid(message: &str) -> DomainError {
    DomainError::Validation {
        message: message.into(),
    }
}
