//! 受信扩展的运行时协调：把“已安装、应用级启用”的 Node 扩展包变成受监管的宿主进程。
//!
//! 协调器只读取插件管理服务的事实（启用状态与不可变包），不持有第二份启用意愿。
//! 每次 `plugin.registry.changed` 事件后收敛到期望集合：
//! - 已启用且声明了 `extension` 的包：登记到 ExtensionBus 并启动监管；
//! - 被停用、被卸载或摘要变化：停止监管并注销登记（用户显式停用才解除 guard）；
//! - 找不到随包 Node：登记仍保留但不启动宿主，因此 guard 钩子 fail-closed。

use magi_app_server_protocol::generated::{PluginRuntimeState, PluginRuntimeStatus};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_extension_host::{
    HOST_RUNTIME_SOURCE,
    bus::ExtensionBus,
    instance::LaunchConfig,
    protocol::{HookMode, HookRegistration},
    supervisor::{PluginSupervisor, RestartPolicy, SupervisorState, supervise},
};

use magi_plugin_system::{PluginManager, PluginPackage};
use magi_process::ManagedProcessGroup;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// 开发与测试用的 Node 覆盖入口（`npm run` 的 `node scripts/node-runtime.mjs stage` 可准备固定版本 Node）。
/// 发行版不设置它：daemon 使用与自身可执行文件同级的 `node/` 目录里的随包 Node。
pub(super) const EXTENSION_NODE_ENV: &str = "MAGI_EXTENSION_NODE";

/// 随包 Node 相对 daemon 可执行文件的位置：`<daemon 目录>/node/node[.exe]`。
/// Desktop 与无头入口使用同一合同；永远不使用用户 PATH 里的 Node。
fn bundled_node(executable: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let path = executable.parent()?.join("node").join(name);
    path.is_file().then_some(path)
}

/// 兼容运行时目录的开发覆盖入口；发行版不设置：daemon 使用与自身可执行文件同级的 `compat/`。
pub(super) const EXTENSION_COMPAT_ENV: &str = "MAGI_EXTENSION_COMPAT";

/// 兼容运行时（翻译包的共享依赖、兼容层与准备脚本）相对 daemon 可执行文件的位置：`<daemon 目录>/compat/`。
fn bundled_compat(executable: &Path) -> Option<PathBuf> {
    let path = executable.parent()?.join("compat");
    path.join("cli.mjs").is_file().then_some(path)
}

pub(super) fn resolve_compat(
    configured: Option<std::ffi::OsString>,
    executable: Option<PathBuf>,
) -> Option<PathBuf> {
    configured
        .map(PathBuf::from)
        .filter(|path| path.join("cli.mjs").is_file())
        .or_else(|| executable.as_deref().and_then(bundled_compat))
}

fn resolve_node(
    configured: Option<std::ffi::OsString>,
    executable: Option<PathBuf>,
) -> Option<PathBuf> {
    configured
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| executable.as_deref().and_then(bundled_node))
}

struct Running {
    digest: String,
    supervisor: Option<PluginSupervisor>,
    quiesced: bool,
}

#[derive(Clone)]
pub(super) struct ExtensionRuntime {
    manager: Arc<Mutex<PluginManager>>,
    bus: Arc<ExtensionBus>,
    group: ManagedProcessGroup,
    root: PathBuf,
    node: Option<PathBuf>,
    compat: Option<PathBuf>,
    policy: RestartPolicy,
    running: Arc<tokio::sync::Mutex<HashMap<String, Running>>>,
    /// 各扩展的运行状态（监管状态的只读投影），界面经 API 读取；变化时发布事件。
    board: Arc<std::sync::RwLock<HashMap<String, PluginRuntimeStatus>>>,
    events: Arc<std::sync::OnceLock<Arc<InMemoryEventBus>>>,
    native: Arc<std::sync::OnceLock<super::extension_services::NativeService>>,
    operations: Arc<std::sync::OnceLock<magi_api::PluginOperations>>,
    settings: Arc<std::sync::OnceLock<Arc<magi_settings_store::SettingsStore>>>,
}

/// 把监管状态映射为界面可见的运行状态。`blocking` 是在宿主不可用期间正被 fail-closed 的
/// guard 钩子：用户据此知道为什么某些动作被阻止，以及该去停用哪个扩展。
fn runtime_status(
    state: &SupervisorState,
    guards: &[String],
    crashes_seen: u64,
) -> PluginRuntimeStatus {
    let (runtime_state, reason, generation, crashes) = match state {
        SupervisorState::Starting => (PluginRuntimeState::Starting, None, 0, crashes_seen),
        SupervisorState::Running { generation } => (
            PluginRuntimeState::Running,
            None,
            *generation,
            generation.saturating_sub(1).max(crashes_seen),
        ),
        SupervisorState::Backoff { crashes, reason } => (
            PluginRuntimeState::Restarting,
            Some(reason.clone()),
            0,
            *crashes as u64,
        ),
        SupervisorState::Failed(reason) => (
            PluginRuntimeState::Failed,
            Some(reason.clone()),
            0,
            crashes_seen,
        ),
        SupervisorState::Stopped => (PluginRuntimeState::Stopped, None, 0, crashes_seen),
    };
    let blocking = if runtime_state == PluginRuntimeState::Running {
        Vec::new()
    } else {
        guards.to_vec()
    };
    PluginRuntimeStatus {
        state: runtime_state,
        reason,
        generation,
        crashes,
        blocking,
        instance_id: None,
    }
}

impl ExtensionRuntime {
    pub(super) fn new(
        manager: Arc<Mutex<PluginManager>>,
        bus: Arc<ExtensionBus>,
        group: ManagedProcessGroup,
        state_root: &Path,
    ) -> Self {
        let node = resolve_node(
            std::env::var_os(EXTENSION_NODE_ENV),
            std::env::current_exe().ok(),
        );
        Self {
            manager,
            bus,
            group,
            root: state_root.join("extensions"),
            node,
            compat: resolve_compat(
                std::env::var_os(EXTENSION_COMPAT_ENV),
                std::env::current_exe().ok(),
            ),
            policy: RestartPolicy::default(),
            running: Arc::default(),
            board: Arc::default(),
            events: Arc::default(),
            native: Arc::default(),
            operations: Arc::default(),
            settings: Arc::default(),
        }
    }

    /// 翻译上游插件的工具链：随包 Node 与兼容运行时都具备时才有。
    pub(super) fn toolchain(&self) -> Option<magi_api::PluginToolchain> {
        Some(magi_api::PluginToolchain {
            node: self.node.clone()?,
            compat: self.compat.clone()?,
        })
    }

    /// 绑定主对话模型目录所在的设置事实源（插件贡献的数据源写入其运行期 section）。
    pub(super) fn bind_settings(&self, settings: &Arc<magi_settings_store::SettingsStore>) {
        let _ = self.settings.set(Arc::clone(settings));
    }

    pub(super) fn bind_operations(&self, operations: magi_api::PluginOperations) {
        let _ = self.operations.set(operations);
    }

    pub(super) fn bind_native(
        &self,
        dispatcher: &Arc<magi_conversation_runtime::task_execution_dispatcher::LlmTaskDispatcher>,
    ) {
        let dispatcher = Arc::downgrade(dispatcher);
        let _ = self.native.set(Arc::new(
            move |plugin, method, params, cancelled, progress| {
                dispatcher
                    .upgrade()
                    .ok_or_else(|| {
                        magi_extension_host::protocol::RpcError::new(
                            magi_extension_host::protocol::ErrorKind::HostGone,
                            "原生执行器已退出",
                        )
                    })?
                    .extension_service(&plugin, &method, params, cancelled, progress)
                    .map_err(|e| {
                        magi_extension_host::protocol::RpcError::new(
                            magi_extension_host::protocol::ErrorKind::Blocked,
                            e,
                        )
                    })
            },
        ));
    }

    fn set_status(&self, id: &str, status: Option<PluginRuntimeStatus>) {
        let changed = {
            let mut board = self.board.write().expect("status board poisoned");
            match status {
                Some(status) => board.insert(id.to_string(), status.clone()) != Some(status),
                None => board.remove(id).is_some(),
            }
        };
        if changed && let Some(events) = self.events.get() {
            events.publish(EventEnvelope::domain(
                magi_core::EventId::unique("plugin-extension-status-changed"),
                "plugin.extension.status.changed",
                serde_json::json!({}),
            ));
        }
    }

    /// 启动时收敛一次，之后随插件注册表事件持续收敛。
    pub(super) fn start(&self, events: Arc<InMemoryEventBus>) {
        let _ = self.events.set(Arc::clone(&events));
        let runtime = self.clone();
        let mut receiver = events.subscribe();
        tokio::spawn(async move {
            runtime.reconcile().await;
            // 请求取消或客户端断开也可能释放生命周期准入；周期收敛保证其后恢复，
            // 不把某个事件是否送达作为启用状态的第二份事实。
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let event = tokio::select! {
                    event = receiver.recv() => event,
                    _ = tick.tick() => { runtime.reconcile().await; continue; }
                };
                match event {
                    Ok(event)
                        if event.event_type == "plugin.resource.updated"
                            && event
                                .payload
                                .get("resourceId")
                                .and_then(serde_json::Value::as_str)
                                == Some("__settings") =>
                    {
                        if let Some(id) = event
                            .payload
                            .get("pluginId")
                            .and_then(serde_json::Value::as_str)
                        {
                            runtime.sync_config(id);
                        }
                    }
                    Ok(event) if event.event_type == "plugin.registry.changed" => {
                        runtime.reconcile().await;
                        runtime.notify_plugin_directory_changed();
                    }
                    Ok(event) if event.event_type == "plugin.extension.status.changed" => {
                        runtime.notify_plugin_directory_changed();
                    }
                    Ok(_) => {}
                    // 落后丢事件后无法知道漏了什么，直接再收敛一次。
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        runtime.reconcile().await;
                        let ids = runtime
                            .board
                            .read()
                            .expect("status board poisoned")
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>();
                        for id in ids {
                            runtime.sync_config(&id);
                        }
                        runtime.notify_plugin_directory_changed();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    fn notify_plugin_directory_changed(&self) {
        for id in self.desired_packages().keys() {
            if let Some(instance) = self.bus.instance(id) {
                let _ = instance.plugin_directory_changed();
            }
        }
    }

    fn sync_config(&self, id: &str) {
        let Some(instance) = self.bus.instance(id) else {
            return;
        };
        let Ok(manager) = self.manager.lock() else {
            return;
        };
        if manager
            .package(id)
            .is_ok_and(|package| package.digest() == instance.digest)
            && let Ok(resource) = manager.read_settings(id)
        {
            let _ = instance.update_config(resource.version, resource.value);
        }
    }

    #[cfg(test)]
    pub(super) async fn supervisor_state(
        &self,
        id: &str,
    ) -> Option<magi_extension_host::supervisor::SupervisorState> {
        let running = self.running.lock().await;
        running
            .get(id)?
            .supervisor
            .as_ref()
            .map(PluginSupervisor::state)
    }

    #[cfg(test)]
    pub(super) async fn is_running(&self, id: &str) -> bool {
        self.running.lock().await.contains_key(id)
    }

    pub(super) async fn reconcile(&self) {
        let desired = self.desired_packages();
        let mut running = self.running.lock().await;

        // 先停掉不再期望或摘要变化的实例。
        let stale: Vec<String> = running
            .iter()
            .filter(|(id, entry)| {
                !self
                    .manager
                    .lock()
                    .expect("plugin manager poisoned")
                    .is_draining(id)
                    && (entry.quiesced
                        || desired
                            .get(*id)
                            .is_none_or(|package| package.digest() != entry.digest))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some(entry) = running.remove(&id) {
                if let Some(supervisor) = entry.supervisor {
                    supervisor.stop().await;
                }
                self.bus.unregister_plugin(&id);
                self.set_status(&id, None);
            }
        }

        for (id, package) in desired {
            if running.contains_key(&id)
                || self
                    .manager
                    .lock()
                    .expect("plugin manager poisoned")
                    .is_draining(&id)
            {
                continue;
            }
            let entry = self.start_plugin(&id, &package);
            running.insert(id, entry);
        }
    }

    fn desired_packages(&self) -> HashMap<String, Arc<PluginPackage>> {
        let Ok(manager) = self.manager.lock() else {
            return HashMap::new();
        };
        manager
            .state()
            .plugins
            .values()
            .filter(|plugin| plugin.enabled)
            .filter_map(|plugin| manager.package(&plugin.id).ok())
            .filter(|package| package.manifest().extension.is_some())
            .map(|package| (package.manifest().id.clone(), package))
            .collect()
    }

    fn start_plugin(&self, id: &str, package: &PluginPackage) -> Running {
        let digest = package.digest().to_string();
        let guards: Vec<String> = package
            .manifest()
            .extension
            .iter()
            .flat_map(|extension| extension.hooks.iter())
            .filter(|hook| hook.mode == magi_app_server_protocol::generated::PluginHookMode::Guard)
            .map(|hook| hook.name.clone())
            .collect();
        let failed = |reason: String| {
            tracing::error!(plugin = %id, %reason, "受信扩展无法启动");
            self.set_status(
                id,
                Some(runtime_status(&SupervisorState::Failed(reason), &guards, 0)),
            );
            Running {
                digest: digest.clone(),
                supervisor: None,
                quiesced: false,
            }
        };
        let Some(extension) = package.manifest().extension.as_ref() else {
            return failed("包未声明 extension".into());
        };
        let hooks: Vec<HookRegistration> = extension
            .hooks
            .iter()
            .map(|hook| {
                use magi_app_server_protocol::generated::PluginHookMode;
                HookRegistration {
                    name: hook.name.clone(),
                    mode: match hook.mode {
                        PluginHookMode::Observe => HookMode::Observe,
                        PluginHookMode::Advise => HookMode::Advise,
                        PluginHookMode::Guard => HookMode::Guard,
                    },
                    deadline_ms: hook.deadline_ms.map(|ms| ms as u64),
                }
            })
            .collect();
        // 登记先于启动：宿主不可用期间 guard 钩子仍然生效（fail-closed）。
        if let Err(error) = self.bus.register_plugin(id, &hooks) {
            return failed(format!("钩子登记被拒绝: {error}"));
        }
        // 能启用就说明清单声明的权限随安装一并获得；contextRewrite 授权决定历史改写提案是否被采纳。
        self.bus.set_context_rewrite_authorized(
            id,
            package.manifest().permissions.iter().any(|permission| {
                permission.kind
                    == magi_app_server_protocol::generated::PluginPermissionKind::ContextRewrite
            }),
        );
        if let Err(error) = self
            .manager
            .lock()
            .expect("plugin manager poisoned")
            .ensure_host_supported(id)
        {
            return failed(error.to_string());
        }
        let Some(node) = self.node.clone() else {
            return failed(format!(
                "未找到随包 Node（应位于 daemon 同级的 node/ 目录；开发时可设置 {EXTENSION_NODE_ENV}）"
            ));
        };
        let (host_script, entry) = match self.materialize(id, package) {
            Ok(paths) => paths,
            Err(error) => return failed(format!("落盘扩展文件失败: {error}")),
        };
        let mut config = LaunchConfig::new(node, host_script, entry, self.root.join("sock"));
        if extension.compat.is_some() {
            // 翻译包的上游代码依赖共享运行时；找不到就明确失败，不让上游自己去解析用户机器上的包。
            let Some(compat) = self.compat.clone() else {
                return failed(format!(
                    "未找到兼容运行时（应位于 daemon 同级的 compat/ 目录；开发时可设置 {EXTENSION_COMPAT_ENV}）"
                ));
            };
            config.compat_root = Some(compat);
            let home = self.root.join("data").join(id);
            if let Err(error) = std::fs::create_dir_all(&home) {
                return failed(format!("创建插件私有数据目录失败: {error}"));
            }
            config.compat_home = Some(home);
        }
        config.plugin_id = id.to_string();
        config.version = package.manifest().version.clone();
        config.digest = digest.clone();
        config.hooks = hooks;
        // 启用意味着清单声明的权限已随安装获得，因此声明的工具都已获 `tools` 权限。
        config.tools = package
            .manifest()
            .contributions
            .tools
            .iter()
            .map(|tool| tool.id.clone())
            .collect();
        config.commands = package
            .manifest()
            .contributions
            .commands
            .iter()
            .map(|command| command.id.clone())
            .collect();
        config.grants = vec!["extension:full-trust".into()];
        let (config_value, config_revision) = self
            .manager
            .lock()
            .ok()
            .and_then(|manager| manager.read_settings(id).ok())
            .map(|resource| (resource.value, resource.version))
            .unwrap_or((serde_json::Value::Null, 0));
        config.config = config_value;
        config.config_revision = config_revision;
        config.service = Some(Arc::new(super::extension_services::PluginHostService {
            manager: Arc::clone(&self.manager),
            events: Arc::clone(&self.events),
            digest: digest.clone(),
            native: Arc::clone(&self.native),
            operations: Arc::clone(&self.operations),
            blobs: Arc::default(),
            images: Arc::default(),
            settings: Arc::clone(&self.settings),
        }));
        let supervisor = supervise(
            self.group.clone(),
            Arc::clone(&self.bus),
            config,
            self.policy,
        );
        // 监管状态的每次变化都同步到状态板并通知界面；崩溃次数在状态间保留。
        let mut changes = supervisor.subscribe();
        let runtime = self.clone();
        let plugin_id = id.to_string();
        tokio::spawn(async move {
            let mut crashes_seen = 0u64;
            loop {
                let state = changes.borrow_and_update().clone();
                match &state {
                    SupervisorState::Backoff { crashes, .. } => {
                        crashes_seen = crashes_seen.max(*crashes as u64);
                    }
                    // 监管只在最后一次崩溃后放弃重启，该次崩溃不再经过 Backoff，这里补上。
                    SupervisorState::Failed(_) => crashes_seen += 1,
                    _ => {}
                }
                let mut status = runtime_status(&state, &guards, crashes_seen);
                if matches!(state, SupervisorState::Running { .. }) {
                    status.instance_id = runtime
                        .bus
                        .instance(&plugin_id)
                        .map(|instance| instance.instance_id.clone());
                }
                runtime.set_status(&plugin_id, Some(status));
                if matches!(state, SupervisorState::Stopped | SupervisorState::Failed(_))
                    || changes.changed().await.is_err()
                {
                    return;
                }
            }
        });
        Running {
            digest,
            supervisor: Some(supervisor),
            quiesced: false,
        }
    }

    /// 把宿主运行时与扩展代码树按包摘要落盘（目录已存在则复用）。返回宿主脚本与入口路径。
    fn materialize(
        &self,
        id: &str,
        package: &PluginPackage,
    ) -> std::io::Result<(PathBuf, PathBuf)> {
        let host_digest = magi_core::hex_lower(Sha256::digest(HOST_RUNTIME_SOURCE.as_bytes()));
        let host_script = self
            .root
            .join("host")
            .join(format!("host-{}.mjs", &host_digest[..16]));
        write_if_absent(&host_script, HOST_RUNTIME_SOURCE.as_bytes())?;
        let package_root = self.root.join("packages").join(id).join(package.digest());
        let mut found_entry = false;
        for (name, bytes) in package.extension_files() {
            // 包校验已保证路径只含安全分量；这里再次按分量拼接，不信任字符串。
            let mut destination = package_root.clone();
            for part in name.split('/') {
                destination.push(part);
            }
            write_if_absent(&destination, bytes)?;
            #[cfg(unix)]
            if package.extension_file_is_executable(name) {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))?;
            }
            found_entry |= name == "extension/index.mjs";
        }
        if !found_entry {
            return Err(std::io::Error::other("包没有 extension/index.mjs"));
        }
        Ok((
            host_script,
            package_root.join("extension").join("index.mjs"),
        ))
    }
}

impl magi_api::ExtensionStatusSource for ExtensionRuntime {
    fn reconcile(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        let runtime = self.clone();
        Box::pin(async move {
            runtime.reconcile().await;
            Ok(())
        })
    }

    fn validate_settings(
        &self,
        plugin_id: String,
        value: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        let instance = self.bus.instance(&plugin_id);
        Box::pin(async move {
            let instance = instance.ok_or("扩展宿主未就绪，无法校验设置")?;
            instance
                .validate_settings(value)
                .await
                .map_err(|error| error.to_string())
        })
    }
    fn status(&self, plugin_id: &str) -> Option<PluginRuntimeStatus> {
        self.board.read().ok()?.get(plugin_id).cloned()
    }

    fn quiesce(
        &self,
        plugin_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        let runtime = self.clone();
        Box::pin(async move {
            // 回收任务独立于 HTTP future：客户端中断不能在旧宿主仍活着时释放互斥。
            tokio::spawn(async move {
                let mut running = runtime.running.lock().await;
                if let Some(entry) = running.get_mut(&plugin_id) {
                    entry.quiesced = true;
                    if let Some(supervisor) = entry.supervisor.take() {
                        supervisor.stop().await;
                    }
                }
                runtime.bus.detach(&plugin_id);
            })
            .await
            .map_err(|error| format!("扩展回收任务失败：{error}"))
        })
    }
}

fn write_if_absent(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.is_file() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("无父目录"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests;
