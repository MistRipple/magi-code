//! 运行期贡献的唯一目录。安装包声明授权上界；运行中的实例提交有版本的完整快照。
use super::*;
use magi_app_server_protocol::generated::{
    PluginRuntimeCatalog, PluginRuntimeContributionKind as Kind,
};

// 动态发现目录可能包含数百个工具；独立于安装清单预算，并为 4 MiB RPC 帧留出余量。
const MAX_RUNTIME_CATALOG_BYTES: usize = 2 * 1024 * 1024;

pub(super) struct RuntimeCatalog {
    lease: String,
    digest: String,
    revision: u64,
    ready: bool,
    revoked: bool,
    catalog: PluginRuntimeCatalog,
}

impl PluginManager {
    pub fn begin_runtime(
        &mut self,
        id: &str,
        digest: &str,
        lease: &str,
    ) -> Result<(), PluginError> {
        self.ensure_editing_admitted(id)?;
        if !self.is_active(id)? || self.package(id)?.digest() != digest {
            return Err(conflict("实例不是当前授权的插件版本"));
        }
        self.runtime_catalogs.insert(
            id.into(),
            RuntimeCatalog {
                lease: lease.into(),
                digest: digest.into(),
                revision: 0,
                ready: false,
                revoked: false,
                catalog: PluginRuntimeCatalog::default(),
            },
        );
        Ok(())
    }

    pub fn check_runtime(&self, id: &str, lease: &str) -> Result<(), PluginError> {
        self.ensure_editing_admitted(id)?;
        let runtime = self
            .runtime_catalogs
            .get(id)
            .ok_or_else(|| conflict("宿主实例已失效"))?;
        if runtime.revoked
            || runtime.lease != lease
            || self.package(id)?.digest() != runtime.digest
            || !self.is_active(id)?
        {
            return Err(conflict("宿主实例已失效或授权已撤销"));
        }
        Ok(())
    }

    pub fn ready_runtime(&mut self, id: &str, lease: &str) -> Result<(), PluginError> {
        self.check_runtime(id, lease)?;
        self.runtime_catalogs
            .get_mut(id)
            .expect("checked runtime")
            .ready = true;
        Ok(())
    }

    /// 当前安装版本的活动实例是否已就绪；不能用监管板的迟到 Running
    /// 推导新版本已经启动，旧实例在停用／升级后永久失效。
    pub fn runtime_ready(&self, id: &str) -> Result<bool, PluginError> {
        let Some(runtime) = self.runtime_catalogs.get(id) else {
            return Ok(false);
        };
        Ok(runtime.ready && !runtime.revoked && self.check_runtime(id, &runtime.lease).is_ok())
    }

    /// 老实例清理不能删除同插件新实例的目录。
    pub fn end_runtime(&mut self, id: &str, lease: &str) -> bool {
        if self
            .runtime_catalogs
            .get(id)
            .is_some_and(|runtime| runtime.lease == lease)
        {
            self.runtime_catalogs.remove(id);
            true
        } else {
            false
        }
    }

    /// 撤权或停用一经提交，旧租约永久失效；重新激活只能接受新实例。
    /// 保留失效租约直到实例退出，以便 daemon 仍能按租约清理其模型来源。
    pub(super) fn invalidate_inactive_runtimes(&mut self) {
        for (id, runtime) in &mut self.runtime_catalogs {
            runtime.revoked |= !self.state.plugins.get(id).is_some_and(|installed| {
                installed.active && installed.enabled && installed.digest == runtime.digest
            });
        }
    }

    pub fn replace_runtime_catalog(
        &mut self,
        id: &str,
        lease: &str,
        expected: u64,
        catalog: PluginRuntimeCatalog,
    ) -> Result<u64, PluginError> {
        self.check_runtime(id, lease)?;
        let runtime = &self.runtime_catalogs[id];
        if runtime.revision != expected {
            return Err(conflict("运行目录版本冲突"));
        }
        if serde_json::to_vec(&catalog)
            .map_or(true, |bytes| bytes.len() > MAX_RUNTIME_CATALOG_BYTES)
        {
            return Err(invalid("运行目录超过 2 MiB"));
        }
        if catalog.tools.len() > 1024
            || catalog.commands.len() > 128
            || catalog.model_sources.len() > 8
        {
            return Err(invalid(
                "运行目录数量超过工具 1024 / 命令 128 / 模型来源 8 的上限",
            ));
        }
        let package = self.package(id)?;
        let allowed = &package
            .manifest()
            .extension
            .as_ref()
            .ok_or_else(|| invalid("未声明扩展"))?
            .runtime_contributions;
        for (kind, present) in [
            (Kind::Tools, !catalog.tools.is_empty()),
            (Kind::Commands, !catalog.commands.is_empty()),
            (Kind::ModelSources, !catalog.model_sources.is_empty()),
            (Kind::Settings, catalog.settings_schema.is_some()),
        ] {
            if present && !allowed.contains(&kind) {
                return Err(invalid(format!("未声明运行期贡献 {kind:?}")));
            }
        }
        let manifest = merge(package.manifest(), &catalog);
        crate::package::validate_runtime_manifest(&manifest)?;
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| conflict("运行目录版本耗尽"))?;
        let runtime = self.runtime_catalogs.get_mut(id).expect("checked runtime");
        runtime.catalog = catalog;
        runtime.revision = revision;
        Ok(revision)
    }

    /// 绑定处理器也由同一个目录裁决，不允许从名字猜出授权。
    pub fn runtime_registration_allowed(
        &self,
        id: &str,
        lease: &str,
        kind: &str,
        name: &str,
    ) -> Result<bool, PluginError> {
        self.check_runtime(id, lease)?;
        let catalog = &self.runtime_catalogs[id].catalog;
        Ok(match kind {
            "tool" => {
                catalog.tools.iter().any(|tool| tool.id == name)
                    && self.permission_allowed(id, PluginPermissionKind::Tools, name)?
            }
            "command" => catalog.commands.iter().any(|command| command.id == name),
            _ => false,
        })
    }

    /// 对外只投影已完成初始化的目录；绑定阶段可读取自己尚未发布的声明。
    pub fn effective_manifest(&self, id: &str) -> Result<PluginManifest, PluginError> {
        self.runtime_manifest(id, false)
    }

    pub fn runtime_manifest(
        &self,
        id: &str,
        include_starting: bool,
    ) -> Result<PluginManifest, PluginError> {
        let package = self.package(id)?;
        Ok(match self.runtime_catalogs.get(id) {
            Some(runtime)
                if !runtime.revoked
                    && (runtime.ready || include_starting)
                    && runtime.digest == package.digest()
                    && self.is_active(id)? =>
            {
                merge(package.manifest(), &runtime.catalog)
            }
            _ => package.manifest().clone(),
        })
    }
}

fn merge(base: &PluginManifest, catalog: &PluginRuntimeCatalog) -> PluginManifest {
    let mut manifest = base.clone();
    manifest.contributions.tools.extend(catalog.tools.clone());
    manifest
        .contributions
        .commands
        .extend(catalog.commands.clone());
    manifest
        .contributions
        .model_sources
        .extend(catalog.model_sources.clone());
    if let Some(schema) = &catalog.settings_schema {
        manifest.settings_schema = schema.clone();
    }
    manifest
}
