//! 升级：候选包预览与包切换。
//!
//! 包切换（`state.json` 提交）是唯一提交点；见 `PluginManager::upgrade`。

use super::*;

const RESET_JOURNAL: &str = "upgrade-data-reset.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetJournal {
    plugin_id: String,
    from_digest: String,
    to_digest: String,
    previous: Vec<(String, PluginResource)>,
}

impl PluginManager {
    /// reset 在包提交前应用；崩溃后用唯一安装事实决定回滚旧数据还是保留已提交清空。
    pub(super) fn recover_data_reset(&mut self) -> Result<(), PluginError> {
        let path = self.root.join(RESET_JOURNAL);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let journal: ResetJournal = serde_json::from_slice(&bytes)
            .map_err(|error| PluginError::CorruptState(error.to_string()))?;
        self.pending_reset = Some(journal.plugin_id.clone());
        let digest = &self
            .state
            .plugins
            .get(&journal.plugin_id)
            .or_else(|| {
                self.state
                    .removals
                    .get(&journal.plugin_id)
                    .map(|value| &value.installed)
            })
            .ok_or_else(|| PluginError::CorruptState("升级数据重置缺少安装事实".into()))?
            .digest;
        if digest == &journal.from_digest {
            self.resources
                .replace_plugin(&journal.plugin_id, &journal.previous)?;
        } else if digest != &journal.to_digest {
            return Err(PluginError::CorruptState("升级数据重置摘要不匹配".into()));
        }
        fs::remove_file(path)?;
        self.pending_reset = None;
        Ok(())
    }
}

impl PluginManager {
    /// 排空后的版本切换。调用方须持有覆盖该插件的升级排空标记
    /// （[`PluginManager::begin_upgrade_drain`]）。
    ///
    /// 授权绑定到具体包摘要，不跨版本继承：切换后保留全局启用意愿，
    /// 但所有激活与授权清空，须用户对新包重新授权后激活。不保留双版本并发或
    /// 自动回退；数据格式版本变化且已有持久数据时没有转换通道，切换后清空该插件的数据。
    pub fn upgrade(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
        drain: &PluginDrainGuard,
    ) -> Result<(), PluginError> {
        self.recover_data_reset()?;
        let id = package.manifest().id.as_str();
        ensure_builtin_source(id, &source)?;
        let current = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装，不能升级"))?;
        ensure_same_source(&current.source, &source)?;
        if current.digest == package.digest() {
            return Ok(());
        }
        if drain.plugin_id() != id || !self.is_draining(id) {
            return Err(conflict("升级必须先关闭该插件的准入"));
        }
        self.upgrade_from(package, source, current.clone())
    }

    /// 原宿主已在移除时排空；候选仍使用同一版本提交及数据重置实现。
    pub fn replace_removed(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
        scope: &PluginChangeScope,
    ) -> Result<(), PluginError> {
        self.recover_data_reset()?;
        let id = package.manifest().id.as_str();
        self.removed_for_change(id, Some(scope))?;
        let previous = self
            .state
            .removals
            .get(id)
            .ok_or_else(|| conflict("目标没有待恢复移除"))?
            .installed
            .clone();
        ensure_same_source(&previous.source, &source)?;
        if previous.digest == package.digest() {
            let mut next = self.state.clone();
            next.removals.remove(id);
            next.plugins.insert(id.into(), previous);
            return self.commit(next);
        }
        self.upgrade_from(package, source, previous)
    }

    fn upgrade_from(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
        current: InstalledPlugin,
    ) -> Result<(), PluginError> {
        let id = package.manifest().id.as_str();
        let old = PluginPackage::from_archive(&fs::read(self.root.join(&current.package_path))?)?;
        if old.digest() != current.digest || old.manifest().id != id {
            return Err(PluginError::CorruptState(
                "移除恢复原包身份或摘要无效".into(),
            ));
        }
        // 没有数据转换通道：数据格式版本变化时沿原提交恢复日志处理。
        let reset_data = old.manifest().data_schema_version
            != package.manifest().data_schema_version
            && self.resources.has_data(id);
        let previous_entries = reset_data.then(|| self.resources.entries(id));
        let enabled = current.enabled;
        let relative = self.write_version(package)?;
        let replacement = InstalledPlugin {
            id: id.to_owned(),
            version: package.manifest().version.clone(),
            digest: package.digest().to_owned(),
            source,
            package_path: relative.clone(),
            enabled,
            grants: Vec::new(),
            active: false,
            disabled_by: None,
        };
        let mut next = self.state.clone();
        next.removals.remove(id);
        next.plugins.insert(id.to_owned(), replacement);
        if reset_data {
            // 恢复日志先于数据变更；安装摘要是恢复时的唯一裁决依据。
            write_atomic(
                &self.root.join(RESET_JOURNAL),
                serde_json::to_vec(&ResetJournal {
                    plugin_id: id.to_owned(),
                    from_digest: current.digest,
                    to_digest: package.digest().to_owned(),
                    previous: previous_entries.unwrap_or_default(),
                })
                .map_err(|error| PluginError::CorruptState(error.to_string()))?,
            )?;
            self.pending_reset = Some(id.to_owned());
            self.resources.replace_plugin(id, &[])?;
        }
        if let Err(error) = self.commit(next) {
            if reset_data {
                self.recover_data_reset()?;
            }
            let _ = fs::remove_file(self.root.join(relative));
            return Err(error);
        }
        if reset_data {
            self.recover_data_reset()?;
        }
        cleanup_unreferenced_packages(&self.root, &self.state)?;
        Ok(())
    }

    /// 准备阶段：只比较候选包与已安装版本，不写入任何状态，也不激活任何贡献。
    ///
    /// 权限按（种类、目标集合）整体比较：目标范围扩大会同时表现为新增与移除，
    /// 界面据此要求重新授权。数据格式版本变化且已有持久数据时标记 `data_reset`，
    /// 与 [`PluginManager::upgrade`] 的清空条件一致。
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
                data_reset: false,
            });
        };
        let installed = self.package(&manifest.id)?;
        ensure_same_source(&current.source, source)?;
        let previous = &installed.manifest().permissions;
        let unchanged = current.digest == package.digest();
        let from_version = installed.manifest().data_schema_version;
        let schema_changed = from_version != manifest.data_schema_version;
        let data_reset = schema_changed && self.resources.has_data(&manifest.id);
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
            data_reset,
        })
    }
}

/// 上游来源身份不可由同名 Magi ID 冒用，版本是同一来源的可变部分。
fn ensure_same_source(current: &PluginSource, candidate: &PluginSource) -> Result<(), PluginError> {
    match (current, candidate) {
        (
            PluginSource::Upstream {
                ecosystem: a,
                package: b,
                github: first,
                ..
            },
            PluginSource::Upstream {
                ecosystem: c,
                package: d,
                github: second,
                ..
            },
        ) if a == c
            && b == d
            && first.as_ref().map(|s| &s.repository) == second.as_ref().map(|s| &s.repository) =>
        {
            Ok(())
        }
        (PluginSource::Upstream { .. }, _) | (_, PluginSource::Upstream { .. }) => {
            Err(conflict("插件 ID 已属于另一来源，不能作为升级替换"))
        }
        _ => Ok(()),
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
        serde_json::to_string(&(&permission.kind, targets)).unwrap_or_default()
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
