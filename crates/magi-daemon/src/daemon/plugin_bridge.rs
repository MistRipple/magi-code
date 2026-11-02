//! daemon 对插件系统的装配。
//!
//! `magi-plugin-system` 拥有安装、授权、作用域与资源的事实；本模块只持有插件管理器、
//! 扩展总线与受信 Node 扩展运行时，并把它们接到 daemon 的执行链路上。这里不保存任何插件
//! 状态，也不另建权限判定。

use super::config::DaemonError;
use magi_event_bus::InMemoryEventBus;
use magi_plugin_system::PluginManager;
use magi_process::ManagedProcessGroup;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// 插件管理器与受信扩展运行时的 daemon 侧持有者。
#[derive(Clone)]
pub(super) struct PluginServices {
    manager: Arc<Mutex<PluginManager>>,
    /// 受信扩展钩子的唯一分发入口。
    extension_bus: Arc<magi_extension_host::bus::ExtensionBus>,
    extensions: super::extension_runtime::ExtensionRuntime,
}

impl PluginServices {
    pub(super) fn open(
        state_root: &Path,
        processes: ManagedProcessGroup,
    ) -> Result<Self, DaemonError> {
        let manager = PluginManager::open(state_root.join("plugins"))
            .map_err(|error| DaemonError::internal(format!("加载插件状态失败: {error}")))?;
        let manager = Arc::new(Mutex::new(manager));
        let extension_bus = Arc::<magi_extension_host::bus::ExtensionBus>::default();
        let extensions = super::extension_runtime::ExtensionRuntime::new(
            Arc::clone(&manager),
            Arc::clone(&extension_bus),
            processes,
            state_root,
        );
        Ok(Self {
            manager,
            extension_bus,
            extensions,
        })
    }

    /// 启动受信扩展协调器：收敛一次，之后随插件注册表事件持续收敛。
    pub(super) fn start_extensions(&self, events: Arc<InMemoryEventBus>) {
        super::extension_bridge::spawn_event_forwarder(
            Arc::clone(&self.extension_bus),
            Arc::clone(&events),
        );
        self.extensions.start(events);
    }

    /// 扩展宿主运行状态的只读来源，供插件列表投影合并。
    pub(super) fn extension_status(&self) -> Arc<dyn magi_api::ExtensionStatusSource> {
        Arc::new(self.extensions.clone())
    }

    pub(super) fn extension_bus(&self) -> Arc<magi_extension_host::bus::ExtensionBus> {
        Arc::clone(&self.extension_bus)
    }

    pub(super) fn manager(&self) -> Arc<Mutex<PluginManager>> {
        Arc::clone(&self.manager)
    }
}
