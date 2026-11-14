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
    /// 停用、卸载与升级共用的准入关闭：之后新请求被拒绝，由调用方等待后台排空再提交。
    pub fn begin_drain(
        manager: Arc<Mutex<Self>>,
        id: &str,
    ) -> Result<PluginDrainGuard, PluginError> {
        Self::begin_drain_with_lease(manager, id, None)
    }
    /// 原实例发起的更新必须在关闭准入的同一锁内核对租约，避免准备完成后
    /// 恰好重启的旧请求排空并替换已经接管的新宿主。
    pub fn begin_runtime_upgrade_drain(
        manager: Arc<Mutex<Self>>,
        id: &str,
        lease: &str,
    ) -> Result<PluginDrainGuard, PluginError> {
        Self::begin_drain_with_lease(manager, id, Some(lease))
    }
    fn begin_drain_with_lease(
        manager: Arc<Mutex<Self>>,
        id: &str,
        lease: Option<&str>,
    ) -> Result<PluginDrainGuard, PluginError> {
        let mut guard = manager.lock().map_err(|_| conflict("插件管理器不可用"))?;
        if let Some(lease) = lease {
            guard.check_runtime(id, lease)?;
        }
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
