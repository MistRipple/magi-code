//! 插件级准入关闭：生命周期事务排空同一插件的唯一后台，不保留安装范围。
use super::*;
pub struct PluginDrainGuard {
    manager: Arc<Mutex<PluginManager>>,
    plugin_id: String,
    committed: bool,
}
impl PluginDrainGuard {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
    pub fn commit(mut self, manager: &mut PluginManager) {
        manager.release_drain(&self.plugin_id);
        self.committed = true;
    }
}
impl Drop for PluginDrainGuard {
    fn drop(&mut self) {
        if !self.committed
            && let Ok(mut manager) = self.manager.lock()
        {
            manager.release_drain(&self.plugin_id);
        }
    }
}
impl PluginManager {
    pub fn is_draining(&self, id: &str) -> bool {
        self.pending_reset.as_deref() == Some(id) || self.draining.contains(id)
    }
    pub fn begin_deactivation(
        manager: Arc<Mutex<Self>>,
        id: &str,
    ) -> Result<PluginDrainGuard, PluginError> {
        {
            let guard = manager.lock().map_err(|_| conflict("插件管理器不可用"))?;
            if !guard.state.plugins.get(id).is_some_and(|p| p.active) {
                return Err(conflict("插件未激活"));
            }
        }
        Self::begin_upgrade_drain(manager, id)
    }
    pub fn begin_upgrade_drain(
        manager: Arc<Mutex<Self>>,
        id: &str,
    ) -> Result<PluginDrainGuard, PluginError> {
        let mut guard = manager.lock().map_err(|_| conflict("插件管理器不可用"))?;
        if !guard.state.plugins.contains_key(id) {
            return Err(conflict("插件未安装"));
        }
        if guard.is_draining(id) {
            return Err(conflict("插件已经在排空"));
        }
        guard.draining.insert(id.to_owned());
        drop(guard);
        Ok(PluginDrainGuard {
            manager,
            plugin_id: id.to_owned(),
            committed: false,
        })
    }
    pub(super) fn release_drain(&mut self, id: &str) {
        self.draining.remove(id);
    }
    pub fn ensure_editing_admitted(&self, id: &str) -> Result<(), PluginError> {
        self.ensure_not_draining(id)
    }
    pub(super) fn ensure_not_draining(&self, id: &str) -> Result<(), PluginError> {
        if self.is_draining(id) {
            Err(conflict("插件生命周期事务正在排空"))
        } else {
            Ok(())
        }
    }
}
