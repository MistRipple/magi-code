//! 管理调用的可恢复移除。原归档只保留引用；候选提交后即结束恢复边界。
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginChangeScope {
    pub plugin: String,
    pub digest: String,
    pub instance: String,
    pub change_id: String,
}

impl PluginChangeScope {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.change_id.is_empty()
            || self.change_id.len() > 128
            || !self
                .change_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
        {
            return Err(invalid("管理恢复边界身份无效"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingRemoval {
    pub scope: PluginChangeScope,
    pub installed: InstalledPlugin,
}

impl PluginManager {
    pub fn removed_for_change(
        &self,
        id: &str,
        scope: Option<&PluginChangeScope>,
    ) -> Result<bool, PluginError> {
        let Some(pending) = self.state.removals.get(id) else {
            return Ok(false);
        };
        if scope != Some(&pending.scope) {
            return Err(conflict("目标正在另一个管理调用的移除恢复边界内"));
        }
        Ok(true)
    }

    pub fn remove_for_change(
        &mut self,
        id: &str,
        scope: PluginChangeScope,
        drain: &PluginDrainGuard,
    ) -> Result<(), PluginError> {
        scope.validate()?;
        if id == scope.plugin || drain.plugin_id() != id || !self.is_draining(id) {
            return Err(conflict("管理移除必须先排空目标，且不能移除自身"));
        }
        if self.state.removals.len() >= 128 {
            return Err(conflict("未结算的管理移除过多，请先完成当前操作"));
        }
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("目标未安装"))?
            .clone();
        let mut next = self.state.clone();
        next.plugins.remove(id);
        next.removals
            .insert(id.into(), PendingRemoval { scope, installed });
        self.commit(next)
    }

    /// 成功调用确认尚未替换的独立卸载；失败调用仅恢复未提交候选的原安装。
    pub fn finish_change(
        &mut self,
        scope: &PluginChangeScope,
        commit: bool,
    ) -> Result<Vec<String>, PluginError> {
        scope.validate()?;
        let ids: Vec<_> = self
            .state
            .removals
            .iter()
            .filter(|(_, value)| &value.scope == scope)
            .map(|(id, _)| id.clone())
            .collect();
        self.settle_removals(ids, commit)
    }

    pub fn recover_instance_removals(
        &mut self,
        plugin: &str,
        instance: &str,
    ) -> Result<Vec<String>, PluginError> {
        let ids: Vec<_> = self
            .state
            .removals
            .iter()
            .filter(|(_, value)| value.scope.plugin == plugin && value.scope.instance == instance)
            .map(|(id, _)| id.clone())
            .collect();
        self.settle_removals(ids, false)
    }

    pub(super) fn recover_pending_removals(&mut self) -> Result<(), PluginError> {
        self.settle_removals(self.state.removals.keys().cloned().collect(), false)?;
        Ok(())
    }

    fn settle_removals(
        &mut self,
        ids: Vec<String>,
        commit: bool,
    ) -> Result<Vec<String>, PluginError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut next = self.state.clone();
        let mut restored = Vec::new();
        for id in ids {
            let removal = next
                .removals
                .remove(&id)
                .ok_or_else(|| conflict("移除恢复记录已变化"))?;
            if next.plugins.contains_key(&id) {
                return Err(PluginError::CorruptState(
                    "移除恢复目标已被其他安装占用".into(),
                ));
            }
            if !commit {
                validate_plugin_path(&removal.installed)?;
                let package = PluginPackage::from_archive(&fs::read(
                    self.root.join(&removal.installed.package_path),
                )?)?;
                if package.digest() != removal.installed.digest
                    || package.manifest().id != id
                    || removal.installed.id != id
                {
                    return Err(PluginError::CorruptState(
                        "移除恢复原包身份或摘要无效".into(),
                    ));
                }
                restored.push(id.clone());
                next.plugins.insert(id, removal.installed);
            }
        }
        self.commit(next)?;
        cleanup_unreferenced_packages(&self.root, &self.state)?;
        Ok(restored)
    }
}
