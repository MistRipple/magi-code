//! 升级：候选包预览与包切换。
//!
//! 包切换（`state.json` 提交）是唯一提交点；见 `PluginManager::upgrade`。

use super::*;

impl PluginManager {
    /// 排空后的版本切换。调用方须持有覆盖该插件全部作用域的升级排空标记
    /// （[`PluginManager::begin_upgrade_drain`]）。
    ///
    /// 授权绑定到具体包摘要，不跨版本继承：切换后保留各作用域的启用意愿，
    /// 但所有激活与授权清空，须用户对新包重新授权后激活。不保留双版本并发或
    /// 自动回退；已有持久数据的数据格式版本变化没有转换器，直接拒绝。
    pub fn upgrade(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
        drain: &PluginDrainGuard,
    ) -> Result<(), PluginError> {
        let id = package.manifest().id.as_str();
        ensure_builtin_source(id, &source)?;
        let current = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装，不能升级"))?;
        if current.digest == package.digest() {
            return Ok(());
        }
        if drain.plugin_id() != id || drain.scopes() != [PLUGIN_WIDE_DRAIN_SCOPE] {
            return Err(conflict("升级必须先关闭该插件全部作用域的准入"));
        }
        let from = self.package(id)?.manifest().data_schema_version;
        let to = package.manifest().data_schema_version;
        if from != to && self.resources.has_data(id) {
            return Err(conflict(format!(
                "新版插件的数据格式版本为 {to}，插件已有数据格式 {from} 的数据，当前没有数据转换通道；升级已拒绝"
            )));
        }
        let enabled_scopes = current.enabled_scopes.clone();
        let relative = self.write_version(package)?;
        let replacement = InstalledPlugin {
            id: id.to_owned(),
            version: package.manifest().version.clone(),
            digest: package.digest().to_owned(),
            source,
            package_path: relative.clone(),
            enabled_scopes,
            grants: BTreeMap::new(),
            active_scopes: BTreeSet::new(),
        };
        let mut next = self.state.clone();
        next.plugins.insert(id.to_owned(), replacement);
        if let Err(error) = self.commit(next) {
            let _ = fs::remove_file(self.root.join(relative));
            return Err(error);
        }
        cleanup_unreferenced_packages(&self.root, &self.state)?;
        Ok(())
    }

    /// 准备阶段：只比较候选包与已安装版本，不写入任何状态，也不激活任何贡献。
    ///
    /// 权限按（种类、作用域、目标集合）整体比较：目标范围扩大会同时表现为新增与移除，
    /// 界面据此要求重新授权。数据格式版本变化且已有持久数据时标记为阻塞，
    /// 与 [`PluginManager::upgrade`] 的拒绝条件一致。
    pub fn preview(
        &self,
        package: &PluginPackage,
        source: &PluginSource,
    ) -> Result<PluginCandidatePreview, PluginError> {
        let manifest = package.manifest();
        ensure_builtin_source(manifest.id.as_str(), source)?;
        let Some(current) = self.state.plugins.get(&manifest.id) else {
            return Ok(PluginCandidatePreview {
                manifest: manifest.clone(),
                digest: package.digest().to_owned(),
                operation: PluginCandidateOperation::Install,
                installed_version: None,
                installed_digest: None,
                added_permissions: manifest.permissions.clone(),
                removed_permissions: Vec::new(),
                data_schema_from: None,
                data_schema_blocked: false,
                data_conversion: false,
            });
        };
        let installed = self.package(&manifest.id)?;
        let previous = &installed.manifest().permissions;
        let unchanged = current.digest == package.digest();
        let from_version = installed.manifest().data_schema_version;
        let schema_changed = from_version != manifest.data_schema_version;
        let needs_conversion = schema_changed && self.resources.has_data(&manifest.id);
        Ok(PluginCandidatePreview {
            manifest: manifest.clone(),
            digest: package.digest().to_owned(),
            operation: if unchanged {
                PluginCandidateOperation::Unchanged
            } else {
                PluginCandidateOperation::Upgrade
            },
            installed_version: Some(current.version.clone()),
            installed_digest: Some(current.digest.clone()),
            added_permissions: permission_difference(&manifest.permissions, previous),
            removed_permissions: permission_difference(previous, &manifest.permissions),
            data_schema_from: schema_changed.then_some(installed.manifest().data_schema_version),
            data_schema_blocked: needs_conversion,
            data_conversion: false,
        })
    }
}

/// `left` 中不存在于 `right` 的权限；目标集合顺序无关。
fn permission_difference(
    left: &[PluginPermission],
    right: &[PluginPermission],
) -> Vec<PluginPermission> {
    let normalized = |permission: &PluginPermission| {
        let mut targets = permission.targets.clone();
        targets.sort();
        serde_json::to_string(&(&permission.kind, &permission.scope, targets)).unwrap_or_default()
    };
    let known = right
        .iter()
        .map(normalized)
        .collect::<std::collections::HashSet<_>>();
    left.iter()
        .filter(|permission| !known.contains(&normalized(permission)))
        .cloned()
        .collect()
}
