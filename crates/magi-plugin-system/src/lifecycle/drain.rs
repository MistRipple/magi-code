//! 准入排空与运行租约：停用与升级共用的“关闭准入 → 等待在途运行结算 → 到期取消”流程。

use super::*;

/// 一次插件 Worker 调用的内存运行租约。
///
/// 租约不进入持久化状态，只覆盖当前进程中的在途调用。释放或崩溃时由 Drop
/// 收口，生命周期操作据此拒绝在途停用，避免把旧 Worker 留在半失效状态。
/// 租约同时登记该调用的取消信号，排空到期后由生命周期所有者统一取消。
pub struct PluginRuntimeLease {
    manager: Arc<Mutex<PluginManager>>,
    key: (String, String),
    id: u64,
}

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

impl PluginRuntimeLease {
    pub fn acquire(
        manager: Arc<Mutex<PluginManager>>,
        id: &str,
        package_digest: &str,
        scope: &str,
        cancellation: &RunCancellation,
    ) -> Result<Self, PluginError> {
        let mut guard = manager
            .lock()
            .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))?;
        guard.validate_runtime_identity(id, package_digest, scope, scope)?;
        let key = (id.to_owned(), scope.to_owned());
        if guard.scope_is_draining(id, scope) {
            return Err(PluginError::Conflict("插件作用域正在排空".into()));
        }
        let id = guard.next_lease_id;
        guard.next_lease_id += 1;
        guard
            .runtime_leases
            .entry(key.clone())
            .or_default()
            .insert(id, cancellation.clone());
        drop(guard);
        Ok(Self { manager, key, id })
    }
}

impl Drop for PluginRuntimeLease {
    fn drop(&mut self) {
        let Ok(mut manager) = self.manager.lock() else {
            return;
        };
        let remove = match manager.runtime_leases.get_mut(&self.key) {
            Some(leases) => {
                leases.remove(&self.id);
                leases.is_empty()
            }
            None => false,
        };
        if remove {
            manager.runtime_leases.remove(&self.key);
        }
    }
}

impl PluginManager {
    /// 关闭单个作用域的准入并登记排空开始时间；调用方随后用
    /// [`PluginManager::settle_runtime_leases`] 等待在途运行结算。
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

    /// 等待排空范围内的在途运行结算：先给自然完成期，到期后统一取消，再给有限的
    /// 结算窗口。仍未结算时返回冲突，调用方保持激活状态，不在调用未结算时强行切换。
    pub async fn settle_runtime_leases(
        manager: &Arc<Mutex<Self>>,
        drain: &PluginDrainGuard,
    ) -> Result<(), PluginError> {
        Self::settle_runtime_leases_within(
            manager,
            drain,
            PLUGIN_DRAIN_TIMEOUT,
            PLUGIN_CANCEL_SETTLE_TIMEOUT,
        )
        .await
    }

    pub async fn settle_runtime_leases_within(
        manager: &Arc<Mutex<Self>>,
        drain: &PluginDrainGuard,
        drain_timeout: Duration,
        cancel_settle_timeout: Duration,
    ) -> Result<(), PluginError> {
        let lease_count = || -> Result<usize, PluginError> {
            let manager = manager
                .lock()
                .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))?;
            Ok(drain
                .scopes()
                .iter()
                .map(|scope| manager.runtime_lease_count_for_deactivation(drain.plugin_id(), scope))
                .sum())
        };
        if wait_until_settled(&lease_count, drain_timeout).await? {
            return Ok(());
        }
        {
            let manager = manager
                .lock()
                .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))?;
            for scope in drain.scopes() {
                manager.cancel_runtime_leases_for_deactivation(drain.plugin_id(), scope);
            }
        }
        if wait_until_settled(&lease_count, cancel_settle_timeout).await? {
            return Ok(());
        }
        Err(conflict("插件排空超时，已取消在途运行但尚未结算"))
    }

    pub fn runtime_lease_count(&self, id: &str, scope: &str) -> usize {
        self.runtime_leases
            .get(&(id.to_owned(), scope.to_owned()))
            .map_or(0, BTreeMap::len)
    }

    /// 插件在所有作用域上的在途运行数。
    pub fn plugin_lease_count(&self, id: &str) -> usize {
        self.runtime_leases
            .iter()
            .filter(|((plugin_id, _), _)| plugin_id == id)
            .map(|(_, leases)| leases.len())
            .sum()
    }

    /// 应用级停用与整插件升级覆盖共享该实例的所有工作区，排空等待必须统计插件的
    /// 全部作用域租约，不能只看某一条键。
    pub fn runtime_lease_count_for_deactivation(&self, id: &str, scope: &str) -> usize {
        if covers_every_scope(scope) {
            self.plugin_lease_count(id)
        } else {
            self.runtime_lease_count(id, scope)
        }
    }

    /// 排空到期后取消排空范围内的全部在途调用，返回被通知的调用数。
    /// 范围与 `runtime_lease_count_for_deactivation` 一致；租约仍由各调用自身
    /// 结算后释放，取消不等于已经完成。
    pub fn cancel_runtime_leases_for_deactivation(&self, id: &str, scope: &str) -> usize {
        let mut cancelled = 0;
        for ((plugin_id, lease_scope), leases) in &self.runtime_leases {
            if plugin_id != id || (!covers_every_scope(scope) && lease_scope != scope) {
                continue;
            }
            for cancellation in leases.values() {
                cancellation.cancel();
                cancelled += 1;
            }
        }
        cancelled
    }

    /// 应用级排空与整插件升级排空都会关闭新的激活和运行准入。
    pub(super) fn scope_is_draining(&self, id: &str, scope: &str) -> bool {
        let draining = |key: &str| {
            self.draining_scopes
                .contains_key(&(id.to_owned(), key.to_owned()))
        };
        draining(scope) || draining(PLUGIN_WIDE_DRAIN_SCOPE)
    }

    /// 独立入口（UI、HTTP）的写入准入。排空期间关闭资源与设置编辑；在途运行经 SDK
    /// 的写入不走这里，仍受运行租约和 `validate_runtime_identity` 约束。
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

/// 应用级与整插件排空都覆盖该插件的全部运行。
pub(super) fn covers_every_scope(scope: &str) -> bool {
    scope == "application" || scope == PLUGIN_WIDE_DRAIN_SCOPE
}

/// 轮询直到在途运行清零；期限内清零返回 `true`。
pub(super) async fn wait_until_settled(
    count: &(dyn Fn() -> Result<usize, PluginError> + Sync),
    timeout: Duration,
) -> Result<bool, PluginError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if count()? == 0 {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
