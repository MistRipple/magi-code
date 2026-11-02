//! 准入关闭：停用与升级共用的“关闭准入 → 提交生命周期事实 → 撤销标记”流程。
//!
//! 插件不再有需要等待结算的在途调用（QuickJS 后台已移除）；宿主进程的停止由 daemon 的
//! ExtensionRuntime 在生命周期事实提交后收敛。这里只保证同一插件的停用、升级与激活互斥。

use super::*;

/// 关闭准入期间的排空标记。请求取消或超时丢弃它时，标记自动撤销。
///
/// 一个标记可以覆盖同一插件的多个作用域（升级需要同时封住全部激活作用域）。
pub struct PluginDrainGuard {
    manager: Arc<Mutex<PluginManager>>,
    plugin_id: String,
    scopes: Vec<String>,
    committed: bool,
}

impl PluginDrainGuard {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// 生命周期事实已提交，撤销排空标记。调用方须持有管理器锁。
    pub fn commit(mut self, manager: &mut PluginManager) {
        manager.release_drain(&self.plugin_id, &self.scopes);
        self.committed = true;
    }
}

impl Drop for PluginDrainGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(mut manager) = self.manager.lock() {
            manager.release_drain(&self.plugin_id, &self.scopes);
        }
    }
}

impl PluginManager {
    /// 关闭单个作用域的准入；调用方提交生命周期事实后丢弃或提交返回的标记。
    pub fn begin_deactivation(
        manager: Arc<Mutex<Self>>,
        id: &str,
        scope: &str,
    ) -> Result<PluginDrainGuard, PluginError> {
        let mut guard = manager
            .lock()
            .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))?;
        guard.package(id)?;
        validate_scope_key(scope)?;
        let installed = guard
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if !installed.active_scopes.contains(scope) {
            return Err(conflict("插件作用域未激活"));
        }
        guard.reserve_drain(id, &[scope.to_owned()])?;
        drop(guard);
        Ok(PluginDrainGuard {
            manager,
            plugin_id: id.to_owned(),
            scopes: vec![scope.to_owned()],
            committed: false,
        })
    }

    /// 关闭整个插件（所有工作区与应用级作用域）的准入，作为升级切换的前置条件。
    pub fn begin_upgrade_drain(
        manager: Arc<Mutex<Self>>,
        id: &str,
    ) -> Result<PluginDrainGuard, PluginError> {
        let mut guard = manager
            .lock()
            .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))?;
        if !guard.state.plugins.contains_key(id) {
            return Err(conflict("插件未安装，不能升级"));
        }
        let scopes = vec![PLUGIN_WIDE_DRAIN_SCOPE.to_owned()];
        guard.reserve_drain(id, &scopes)?;
        drop(guard);
        Ok(PluginDrainGuard {
            manager,
            plugin_id: id.to_owned(),
            scopes,
            committed: false,
        })
    }

    pub(super) fn reserve_drain(&mut self, id: &str, scopes: &[String]) -> Result<(), PluginError> {
        if self.draining_scopes.keys().any(|(plugin, scope)| {
            plugin == id
                && (scope == PLUGIN_WIDE_DRAIN_SCOPE
                    || scopes.iter().any(|requested| {
                        requested == PLUGIN_WIDE_DRAIN_SCOPE || requested == scope
                    }))
        }) {
            return Err(conflict("插件作用域已经在排空"));
        }
        let started = UtcMillis::now();
        for scope in scopes {
            self.draining_scopes
                .insert((id.to_owned(), scope.clone()), started);
        }
        Ok(())
    }

    pub(super) fn release_drain(&mut self, id: &str, scopes: &[String]) {
        for scope in scopes {
            self.draining_scopes.remove(&(id.to_owned(), scope.clone()));
        }
    }

    /// 应用级排空与整插件升级排空都会关闭新的激活和运行准入。
    pub(super) fn scope_is_draining(&self, id: &str, scope: &str) -> bool {
        let draining = |key: &str| {
            self.draining_scopes
                .contains_key(&(id.to_owned(), key.to_owned()))
        };
        draining(scope) || draining(PLUGIN_WIDE_DRAIN_SCOPE)
    }

    /// 独立入口（UI、HTTP）的写入准入。关闭准入期间不接受资源与设置编辑。
    pub fn ensure_editing_admitted(&self, id: &str, scope: &str) -> Result<(), PluginError> {
        self.ensure_scope_not_draining(id, scope)
    }

    pub(super) fn ensure_scope_not_draining(
        &self,
        id: &str,
        scope: &str,
    ) -> Result<(), PluginError> {
        if self.scope_is_draining(id, scope) {
            Err(PluginError::Conflict("插件作用域正在排空".into()))
        } else {
            Ok(())
        }
    }
}
