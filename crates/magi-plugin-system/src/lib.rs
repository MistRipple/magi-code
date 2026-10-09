//! daemon 插件领域：包、授权、作用域与生命周期。
//! 执行由 magi-plugin-runtime 承载，外部 DTO 来自唯一 App Server schema。

pub mod engine;
mod lifecycle;
mod package;
pub mod resource;
pub mod workflow;
pub mod workflow_runtime;

pub use engine::{
    EngineEvent, EngineReadiness, PluginSessionEngine, SessionEngineAdapter, SessionEngineRequest,
};
pub use lifecycle::{
    ActivePlugin, InstalledPlugin, PluginManager, PluginManagerState, PluginSource,
};
pub use magi_app_server_protocol::{
    PluginManifest, PluginPermission, PluginPermissionKind, PluginScopeKind,
};
pub use package::{MAX_PACKAGE_BYTES, PluginPackage, validate_manifest};
pub use resource::{PluginResource, PluginResourceStore, ResourceError};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("插件包无效：{0}")]
    InvalidPackage(String),
    #[error("插件状态冲突：{0}")]
    Conflict(String),
    #[error("插件能力未授权：{0}")]
    NotAuthorized(String),
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
