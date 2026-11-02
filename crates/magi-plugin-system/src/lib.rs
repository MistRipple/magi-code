//! daemon 插件领域：包、授权、作用域与生命周期。
//!
//! 插件的可执行部分只有受信 Node 扩展，由 `magi-extension-host` 承载；这里只拥有包校验、
//! 授权、启用/激活/升级状态与插件资源存储。SDK 1（QuickJS 后台）已移除，其包在校验阶段被拒绝。

pub mod catalog;
mod lifecycle;
mod package;
pub mod resource;

pub use catalog::{
    CatalogEntry, CatalogError, MAX_CATALOG_BYTES, ParsedCatalog, PluginCatalog, RevokedEntry,
    TrustedKey,
};
pub use lifecycle::{
    ActivePlugin, InstalledPlugin, PluginDrainGuard, PluginManager, PluginManagerState,
    PluginSource, RetiredBuiltin,
};
pub use magi_app_server_protocol::{
    PluginManifest, PluginPermission, PluginPermissionKind, PluginScopeKind,
};
pub use package::{MAX_PACKAGE_BYTES, PluginPackage, validate_manifest};
pub use resource::{PluginResource, PluginResourceStore, ResourceError};

/// 插件全局生效：启用、授权、激活和资源都只有这一个作用域键，与调用发生在哪个工作区无关。
pub const APPLICATION_SCOPE: &str = "application";

/// `plugin/{插件}/{贡献}` 形式的贡献身份：技能、视图等贡献共用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionRef {
    pub plugin_id: String,
    pub contribution_id: String,
}

impl ContributionRef {
    pub fn new(plugin_id: impl Into<String>, contribution_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            contribution_id: contribution_id.into(),
        }
    }

    /// 解析贡献身份；`kind` 只用于错误文案（例如“命令”“工作流”）。
    pub fn parse(kind: &str, identity: &str) -> Result<Self, String> {
        identity
            .strip_prefix("plugin/")
            .and_then(|rest| rest.rsplit_once('/'))
            .filter(|(plugin, contribution)| !plugin.is_empty() && !contribution.is_empty())
            .map(|(plugin, contribution)| Self::new(plugin, contribution))
            .ok_or_else(|| format!("插件{kind}身份必须为 plugin/{{插件}}/{{贡献}}"))
    }

    pub fn identity(&self) -> String {
        format!("plugin/{}/{}", self.plugin_id, self.contribution_id)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("插件包无效：{0}")]
    InvalidPackage(String),
    #[error("插件状态冲突：{0}")]
    Conflict(String),
    #[error("插件能力未授权：{0}")]
    NotAuthorized(String),
    #[error("插件对象不存在：{0}")]
    NotFound(String),
    #[error("插件调用参数无效：{0}")]
    InvalidInput(String),
    #[error("插件存储失败：{0}")]
    Storage(#[from] std::io::Error),
    #[error(transparent)]
    Resource(#[from] ResourceError),
    #[error("插件状态损坏：{0}")]
    CorruptState(String),
}

pub(crate) fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::InvalidPackage(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contribution_identity_is_strictly_namespaced() {
        assert_eq!(
            ContributionRef::parse("命令", "plugin/acme.tools/open"),
            Ok(ContributionRef::new("acme.tools", "open"))
        );
        assert_eq!(
            ContributionRef::new("acme.tools", "open").identity(),
            "plugin/acme.tools/open"
        );
        assert!(ContributionRef::parse("命令", "acme.tools/open").is_err());
        assert!(ContributionRef::parse("命令", "plugin//open").is_err());
        assert!(ContributionRef::parse("命令", "plugin/acme.tools/").is_err());
    }
}
