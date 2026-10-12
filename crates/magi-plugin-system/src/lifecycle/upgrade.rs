//! 升级：候选包预览、数据格式转换的暂存、可恢复的升级操作记录与包切换。
//!
//! 包切换（`state.json` 提交）是唯一提交点；见 `PluginManager::upgrade`。

use super::*;

/// 可恢复的升级操作记录。包切换（`state.json` 提交）是唯一提交点：
/// 记录存在时，若已安装摘要等于 `to_digest` 则重放暂存数据，否则丢弃暂存。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpgradeOperation {
    plugin_id: String,
    from_digest: String,
    to_digest: String,
    staged_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedEntry {
    scope: String,
    resource_id: String,
    resource: PluginResource,
}

impl PluginManager {
    /// 排空后的版本切换。调用方须持有覆盖该插件全部作用域的升级排空标记
    /// （[`PluginManager::begin_upgrade_drain`]）并已等待在途运行结算。
    ///
    /// 授权绑定到具体包摘要，不跨版本继承：切换后保留各作用域的启用意愿，
    /// 但所有激活与授权清空，须用户对新包重新授权后激活。不保留双版本并发或
    /// 自动回退；数据格式版本变化需要尚未定案的转换器，直接拒绝。
    pub fn upgrade(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
        drain: &PluginDrainGuard,
        converted: Option<ConvertedData>,
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
        if self.plugin_lease_count(id) > 0 {
            return Err(conflict("插件仍有在途运行，不能切换"));
        }
        // 数据格式版本变化且已有持久数据时，必须带着新版自己转换出的数据切换；
        // 没有数据则无需转换。转换结果与升级前读取的数据逐条版本核对，不一致整体作废。
        let staged = self.converted_entries(package, converted)?;
        let enabled_scopes = current.enabled_scopes.clone();
        let relative = self.write_version(package)?;
        let operation = match staged {
            Some(entries) => match self.stage_upgrade(current.digest.clone(), package, &entries) {
                Ok(operation) => Some(operation),
                Err(error) => {
                    let _ = fs::remove_file(self.root.join(&relative));
                    return Err(error);
                }
            },
            None => None,
        };
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
            if let Some(operation) = &operation {
                self.discard_upgrade_operation(operation);
            }
            return Err(error);
        }
        // 提交点已过：暂存数据必须应用；失败则保留记录并拒绝激活，由重试或重启恢复。
        if let Some(operation) = operation {
            self.pending_upgrade = Some(operation);
            self.apply_pending_upgrade()?;
        }
        cleanup_unreferenced_packages(&self.root, &self.state)?;
        Ok(())
    }

    /// 升级前读取需要转换的数据。无需转换时返回 `None`。
    pub fn plan_data_migration(
        &self,
        package: &PluginPackage,
    ) -> Result<Option<DataMigrationPlan>, PluginError> {
        let manifest = package.manifest();
        let Some(current) = self.state.plugins.get(&manifest.id) else {
            return Ok(None);
        };
        if current.digest == package.digest() {
            return Ok(None);
        }
        let installed = self.package(&manifest.id)?;
        let from = installed.manifest().data_schema_version;
        if from == manifest.data_schema_version || !self.resources.has_data(&manifest.id) {
            return Ok(None);
        }
        let declared = manifest
            .data_migration
            .as_ref()
            .is_some_and(|migration| migration.from_versions.contains(&from));
        if !declared {
            return Err(conflict(format!(
                "新版插件的数据格式版本为 {}，插件已有数据格式 {from} 的数据，但新版没有声明 dataMigration 可从该版本转换；升级已拒绝",
                manifest.data_schema_version
            )));
        }
        Ok(Some(DataMigrationPlan {
            plugin_id: manifest.id.clone(),
            from_version: from,
            to_version: manifest.data_schema_version,
            entries: self
                .resources
                .entries(&manifest.id)
                .into_iter()
                .map(|(scope, resource_id, resource)| MigrationEntry {
                    scope,
                    resource_id,
                    resource,
                })
                .collect(),
        }))
    }

    /// 校验转换结果并生成待暂存的完整条目；不需要转换时返回 `None`。
    fn converted_entries(
        &self,
        package: &PluginPackage,
        converted: Option<ConvertedData>,
    ) -> Result<Option<Vec<StagedEntry>>, PluginError> {
        let Some(plan) = self.plan_data_migration(package)? else {
            return Ok(None);
        };
        let Some(converted) = converted else {
            return Err(conflict(
                "升级需要数据转换结果，但调用方没有提供；升级已拒绝",
            ));
        };
        if converted.plan_fingerprint != plan.fingerprint() {
            return Err(conflict("转换期间插件数据发生变化，升级已作废，请重新升级"));
        }
        let settings_validator = jsonschema::validator_for(&package.manifest().settings_schema)
            .map_err(|error| invalid(format!("插件设置 schema 无效：{error}")))?;
        let declared_resources = &package.manifest().contributions.resources;
        let mut staged = Vec::new();
        for (entry, value) in converted.entries {
            let Some(value) = value else { continue };
            if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 4 * 1024 * 1024) {
                return Err(invalid("转换结果超出资源快照上限"));
            }
            if entry.resource_id == SETTINGS_RESOURCE_ID {
                if !settings_validator.is_valid(&value) {
                    return Err(invalid("转换后的设置不符合新版清单声明的 schema"));
                }
            } else if !declared_resources
                .iter()
                .any(|resource| resource.id == entry.resource_id)
            {
                return Err(invalid(format!(
                    "转换结果保留了新版未声明的资源 `{}`；请返回 {{drop:true}} 丢弃它",
                    entry.resource_id
                )));
            }
            staged.push(StagedEntry {
                scope: entry.scope,
                resource_id: entry.resource_id,
                resource: PluginResource {
                    version: entry.resource.version.saturating_add(1),
                    value,
                },
            });
        }
        Ok(Some(staged))
    }

    fn operation_path(&self) -> PathBuf {
        self.root.join(UPGRADE_OPERATION_FILE)
    }

    /// 写暂存数据与操作记录。此时旧版本与旧数据都未被触碰，崩溃只会留下可丢弃的暂存。
    fn stage_upgrade(
        &self,
        from_digest: String,
        package: &PluginPackage,
        entries: &[StagedEntry],
    ) -> Result<UpgradeOperation, PluginError> {
        if self.operation_path().exists() {
            return Err(conflict("存在未完成的升级操作，请先恢复后再升级"));
        }
        let id = &package.manifest().id;
        let staged_path = format!("staging/{id}/{}.json", package.digest());
        let target = self.root.join(&staged_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(
            &target,
            serde_json::to_vec_pretty(entries)
                .map_err(|error| PluginError::CorruptState(error.to_string()))?,
        )?;
        let operation = UpgradeOperation {
            plugin_id: id.clone(),
            from_digest,
            to_digest: package.digest().to_owned(),
            staged_path,
        };
        if let Err(error) = write_atomic(
            &self.operation_path(),
            serde_json::to_vec_pretty(&operation)
                .map_err(|error| PluginError::CorruptState(error.to_string()))?,
        ) {
            let _ = fs::remove_file(target);
            return Err(error.into());
        }
        Ok(operation)
    }

    fn discard_upgrade_operation(&self, operation: &UpgradeOperation) {
        let _ = fs::remove_file(self.root.join(&operation.staged_path));
        let _ = fs::remove_file(self.operation_path());
    }

    /// 把暂存数据整体替换进资源存储（幂等），成功后清除记录。
    pub(super) fn apply_pending_upgrade(&mut self) -> Result<(), PluginError> {
        let Some(operation) = self.pending_upgrade.clone() else {
            return Ok(());
        };
        let bytes = fs::read(self.root.join(&operation.staged_path))?;
        let entries: Vec<StagedEntry> = serde_json::from_slice(&bytes)
            .map_err(|error| PluginError::CorruptState(format!("暂存数据损坏：{error}")))?;
        let entries = entries
            .into_iter()
            .map(|entry| (entry.scope, entry.resource_id, entry.resource))
            .collect::<Vec<_>>();
        self.resources
            .replace_plugin(&operation.plugin_id, &entries)?;
        self.discard_upgrade_operation(&operation);
        self.pending_upgrade = None;
        Ok(())
    }

    /// 启动恢复：以包切换是否已提交决定重放还是丢弃暂存数据。
    pub(super) fn recover_upgrade_operation(&mut self) -> Result<(), PluginError> {
        let bytes = match fs::read(self.operation_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let operation: UpgradeOperation = serde_json::from_slice(&bytes)
            .map_err(|error| PluginError::CorruptState(format!("升级操作记录损坏：{error}")))?;
        let committed = self
            .state
            .plugins
            .get(&operation.plugin_id)
            .is_some_and(|plugin| plugin.digest == operation.to_digest);
        if committed {
            self.pending_upgrade = Some(operation);
            self.apply_pending_upgrade()
        } else {
            // 提交前崩溃：旧版本与旧数据完整，暂存数据不可见，直接丢弃。
            self.discard_upgrade_operation(&operation);
            Ok(())
        }
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
        let convertible = manifest
            .data_migration
            .as_ref()
            .is_some_and(|migration| migration.from_versions.contains(&from_version));
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
            data_schema_blocked: needs_conversion && !convertible,
            data_conversion: needs_conversion && convertible,
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
