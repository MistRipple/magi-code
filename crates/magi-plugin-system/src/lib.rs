//! daemon 插件领域：包、授权、作用域与生命周期。
//! 执行由 magi-plugin-runtime 承载，外部 DTO 来自唯一 App Server schema。

mod blocking;
pub mod catalog;
pub mod command;
pub mod data_migration;
pub mod engine;
mod lifecycle;
mod package;
pub mod resource;
mod session_engine;
pub mod workflow;
mod workflow_factory;
pub mod workflow_runtime;
use sha2::Digest;
use std::sync::atomic::{AtomicU64, Ordering};

pub use blocking::run_invocation_blocking;
pub use catalog::{
    CatalogEntry, CatalogError, MAX_CATALOG_BYTES, ParsedCatalog, PluginCatalog, TrustedKey,
};
pub use command::{
    PluginCommandExecutor, PluginCommandInvocation, PluginCommandResult, PluginCommandRunner,
};
pub use data_migration::{
    ConvertedData, DataMigrationPlan, MigrationEntry, PluginDataMigrator, WorkerDataMigrator,
};
pub use engine::{
    EngineEvent, EngineReadiness, PluginSessionEngine, SessionEngineAdapter, SessionEngineRequest,
};
pub use lifecycle::{
    ActivePlugin, InstalledPlugin, PLUGIN_CANCEL_SETTLE_TIMEOUT, PLUGIN_DRAIN_TIMEOUT,
    PluginDrainGuard, PluginManager, PluginManagerState, PluginRuntimeLease, PluginSource,
    PreparedToolCall,
};
pub use magi_app_server_protocol::{
    PluginManifest, PluginPermission, PluginPermissionKind, PluginScopeKind,
};
pub use magi_plugin_runtime::RunCancellation;
pub use package::{MAX_PACKAGE_BYTES, PluginPackage, validate_manifest};
pub use resource::{PluginResource, PluginResourceStore, ResourceError};
pub use session_engine::{
    NativeSessionEngineFactory, PluginSessionEngineFactory, SessionEngineFactory,
    SessionEngineInvocationSpec, SessionEngineModelClient, SessionEngineRouter,
};
pub use workflow_factory::{
    PluginWorkflowCoreFactory, WorkflowCoreFactory, WorkflowCoreInvocationSpec, WorkflowCoreRouter,
};

/// 插件全局生效：启用、授权、激活和资源都只有这一个作用域键，与调用发生在哪个工作区无关。
pub const APPLICATION_SCOPE: &str = "application";

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(0);

/// 为一次宿主发起的插件调用签发唯一身份：毫秒时间戳加进程内单调序号，跨重启也不重复。
pub fn next_invocation_id(kind: &str) -> String {
    format!(
        "{kind}-{}-{}",
        magi_core::UtcMillis::now().0,
        NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed)
    )
}

/// `plugin/{插件}/{贡献}` 形式的贡献身份：命令、工作流和会话引擎共用。
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

/// 暴露给模型的工具名。模型接口通常只接受 `[a-zA-Z0-9_-]{1,64}`，而插件 ID 必含 `.`，
/// 因此清洗字符后追加原始身份的摘要：不同的 (插件, 贡献) 即使清洗后相同也不会冲突，
/// 并且名称总在长度上限内。
pub fn model_tool_name(plugin_id: &str, contribution_id: &str) -> String {
    const MAX_LEN: usize = 64;
    const DIGEST_HEX: usize = 12;
    let safe = |value: &str| -> String {
        value
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let digest = sha2::Sha256::digest(format!("{plugin_id}\u{1f}{contribution_id}").as_bytes());
    let suffix = &format!("{digest:x}")[..DIGEST_HEX];
    let readable = format!("plugin__{}__{}", safe(plugin_id), safe(contribution_id));
    let keep = MAX_LEN - DIGEST_HEX - 1;
    let readable: String = readable.chars().take(keep).collect();
    format!("{readable}_{suffix}")
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

    #[test]
    fn model_tool_names_are_valid_unique_and_bounded() {
        let valid = |name: &str| {
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        };
        let name = model_tool_name("example.board", "board-summary");
        assert!(valid(&name), "{name}");
        assert!(
            name.starts_with("plugin__example_board__board-summary_"),
            "{name}"
        );
        // 清洗后相同的两个身份仍然不同。
        assert_ne!(model_tool_name("a.b", "c"), model_tool_name("a_b", "c"),);
        // 超长身份被截断但仍合法，并保持唯一。
        let long_a = model_tool_name(&format!("acme.{}", "x".repeat(90)), &"y".repeat(64));
        let long_b = model_tool_name(&format!("acme.{}", "x".repeat(91)), &"y".repeat(64));
        assert!(valid(&long_a) && valid(&long_b));
        assert_ne!(long_a, long_b);
        assert_eq!(
            name,
            model_tool_name("example.board", "board-summary"),
            "同一身份的名称必须稳定"
        );
    }
}
