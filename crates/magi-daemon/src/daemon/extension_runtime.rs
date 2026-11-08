//! 受信扩展的运行时协调：把“已安装、已授权、应用级激活”的 Node 扩展包变成受监管的宿主进程。
//!
//! 协调器只读取插件管理服务的事实（激活状态与不可变包），不持有第二份启用意愿。
//! 每次 `plugin.registry.changed` 事件后收敛到期望集合：
//! - 激活且声明了 `extension` 的包：登记到 ExtensionBus 并启动监管；
//! - 不再激活、被卸载或摘要变化：停止监管并注销登记（用户显式停用才解除 guard）；
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
        let _ = self
            .native
            .set(Arc::new(move |plugin, method, params, cancelled| {
                dispatcher
                    .upgrade()
                    .ok_or_else(|| {
                        magi_extension_host::protocol::RpcError::new(
                            magi_extension_host::protocol::ErrorKind::HostGone,
                            "原生执行器已退出",
                        )
                    })?
                    .extension_service(&plugin, &method, params, cancelled)
                    .map_err(|e| {
                        magi_extension_host::protocol::RpcError::new(
                            magi_extension_host::protocol::ErrorKind::Blocked,
                            e,
                        )
                    })
            }));
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
            .filter(|plugin| plugin.active)
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
        // 能激活就说明清单声明的权限已全部授权；contextRewrite 授权决定历史改写提案是否被采纳。
        self.bus.set_context_rewrite_authorized(
            id,
            package.manifest().permissions.iter().any(|permission| {
                permission.kind
                    == magi_app_server_protocol::generated::PluginPermissionKind::ContextRewrite
            }),
        );
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
        // 激活意味着清单声明的权限已全部授权，因此声明的工具都已获 `tools` 授权。
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
                runtime.set_status(
                    &plugin_id,
                    Some(runtime_status(&state, &guards, crashes_seen)),
                );
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
mod tests {
    use super::*;
    use magi_app_server_protocol::PluginManifest;
    use magi_extension_host::{
        bus::{BusPolicy, HookContext, Verdict},
        supervisor::SupervisorState,
    };
    use magi_plugin_system::PluginSource;

    #[test]
    fn node_is_resolved_from_the_override_or_next_to_the_daemon_and_never_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = dir.path().join("magi-daemon-app");
        std::fs::write(&daemon, b"").unwrap();
        // 没有随包 Node，也没有覆盖：即使 PATH 里有 node 也不使用。
        assert_eq!(resolve_node(None, Some(daemon.clone())), None);

        let name = if cfg!(windows) { "node.exe" } else { "node" };
        std::fs::create_dir_all(dir.path().join("node")).unwrap();
        let bundled = dir.path().join("node").join(name);
        std::fs::write(&bundled, b"").unwrap();
        assert_eq!(
            resolve_node(None, Some(daemon.clone())),
            Some(bundled.clone())
        );

        // 覆盖入口优先（开发/测试）；指向不存在的文件则不生效，回到随包 Node。
        let custom = dir.path().join("custom-node");
        std::fs::write(&custom, b"").unwrap();
        assert_eq!(
            resolve_node(Some(custom.clone().into()), Some(daemon.clone())),
            Some(custom)
        );
        assert_eq!(
            resolve_node(Some(dir.path().join("missing").into()), Some(daemon)),
            Some(bundled)
        );
    }
    use serde_json::json;
    use std::io::Write;

    const ENTRY: &str = r#"
export default async function activate(api) {
  await api.on('tool.call', async (event) => {
    if (event.input?.command?.includes('rm')) return { decision: 'block', reason: '禁止 rm' };
    return { decision: 'continue' };
  });
}
"#;

    /// 测试用 Node：优先 `MAGI_EXTENSION_NODE`（可指向随包的固定版本），否则 PATH 中的 node。
    fn test_node() -> PathBuf {
        std::env::var_os(EXTENSION_NODE_ENV)
            .map(PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("需要 Node")
    }

    fn package() -> PluginPackage {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Guard", "description": "",
            "extension": {"hooks": [{"name": "tool.call", "mode": "guard", "deadlineMs": 3000}]},
            "permissions": [{"kind": "extension", "targets": ["full-trust"]}],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type": "object", "additionalProperties": false},
            "contributions": {}
        }))
        .unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", ENTRY.as_bytes().to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        manager: Arc<Mutex<PluginManager>>,
        bus: Arc<ExtensionBus>,
        runtime: ExtensionRuntime,
    }

    fn fixture(node: Option<PathBuf>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(Mutex::new(
            PluginManager::open(dir.path().join("plugins")).unwrap(),
        ));
        let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
        let mut runtime = ExtensionRuntime::new(
            Arc::clone(&manager),
            Arc::clone(&bus),
            ManagedProcessGroup::new(),
            dir.path(),
        );
        runtime.node = node;
        Fixture {
            _dir: dir,
            manager,
            bus,
            runtime,
        }
    }

    #[test]
    #[cfg(unix)]
    fn materialized_native_helper_executes_and_assets_stay_non_executable() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = fixture(None);
        let manifest = serde_json::to_value(package().manifest()).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes, mode) in [
            (
                "manifest.json",
                serde_json::to_vec(&manifest).unwrap(),
                0o644,
            ),
            (
                "extension/index.mjs",
                b"export default () => {};".to_vec(),
                0o644,
            ),
            (
                "extension/upstream/native/helper",
                b"#!/bin/sh\nprintf native-helper-ok".to_vec(),
                0o755,
            ),
            (
                "extension/upstream/native/data",
                b"plain data".to_vec(),
                0o644,
            ),
        ] {
            writer
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default().unix_permissions(mode),
                )
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
        let (_, entry) = fixture.runtime.materialize("acme.guard", &package).unwrap();
        let helper = entry.parent().unwrap().join("upstream/native/helper");
        let result = std::process::Command::new(&helper).output().unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"native-helper-ok");
        assert_eq!(
            std::fs::metadata(entry.parent().unwrap().join("upstream/native/data"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        // 重用已落盘的同摘要目录时也恢复执行权限。
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o644)).unwrap();
        fixture.runtime.materialize("acme.guard", &package).unwrap();
        assert!(
            std::process::Command::new(helper)
                .status()
                .unwrap()
                .success()
        );
    }

    fn activate(fixture: &Fixture, package: &PluginPackage) {
        let mut manager = fixture.manager.lock().unwrap();
        manager
            .install(
                package,
                PluginSource::Local {
                    name: "guard.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.guard").unwrap();
        manager
            .authorize("acme.guard", package.manifest().permissions.clone())
            .unwrap();
        manager.activate("acme.guard").unwrap();
    }

    fn call(command: &str) -> serde_json::Value {
        json!({"input": {"command": command}})
    }

    const TOOL_ENTRY: &str = r#"
export default async function activate(api) {
  await api.tool('echo', async (input) => ({ echoed: input.text }));
  await api.tool('boom', async () => { throw new Error('工具失败'); });
  await api.command('hello', async (input, ctx) => input.text === 'context'
    ? JSON.stringify({owner:ctx.owner,execution:ctx.execution}) : `你好，${input.text || '世界'}`);
  await api.command('bad', async () => { throw new Error('命令失败'); });
  await api.command('wait', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
  await api.tool('slow', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
  await api.tool('slow-write', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
}
"#;

    fn tool_package() -> PluginPackage {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Tools", "description": "",
            "extension": {"hooks": []},
            "permissions": [
                {"kind": "extension", "targets": ["full-trust"]},
                {"kind": "tools", "targets": ["echo", "boom", "slow", "slow-write"]}
            ],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type": "object", "additionalProperties": false},
            "contributions": {"commands": [
                {"id": "hello", "title": "Hello", "description": "打招呼"},
                {"id": "bad", "title": "Bad", "description": "总是失败"},
                {"id": "wait", "title": "Wait", "description": "等待取消"}
            ], "tools": [
                {"id": "echo", "title": "Echo", "description": "回显", "readOnly": true,
                 "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}, "additionalProperties": false}},
                {"id": "boom", "title": "Boom", "description": "总是失败", "readOnly": true,
                 "inputSchema": {"type": "object"}},
                {"id": "slow", "title": "Slow", "description": "等待取消", "readOnly": true,
                 "inputSchema": {"type": "object"}},
                {"id": "slow-write", "title": "SlowWrite", "description": "等待取消", "readOnly": false,
                 "inputSchema": {"type": "object"}}
            ]}
        }))
        .unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", TOOL_ENTRY.as_bytes().to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    const ROUTE_ENTRY: &str = r#"
export default async function activate(api) {
  await api.command('noop', async () => 'ok');
  await api.route('/status', async (input) => ({ body: { echoed: input.query, headers: input.headers } }));
  await api.route('/hang', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return { body: 'aborted' };
  });
}
"#;

    fn route_package(id: &str, with_view: bool) -> PluginPackage {
        let views = if with_view {
            json!([{"id": "page", "title": "Page", "entry": "ui/index.html", "placements": ["settings"]}])
        } else {
            json!([])
        };
        // 包必须至少有一项贡献；无视图的包以一个命令满足，路由准入仍由「有无视图」决定。
        let commands = json!([{"id": "noop", "title": "Noop", "description": "占位命令"}]);
        let manifest: PluginManifest = serde_json::from_value(json!({
            "sdkVersion": 2, "id": id, "version": "1.0.0", "name": "Routes", "description": "",
            "extension": {"hooks": []},
            "permissions": [{"kind": "extension", "targets": ["full-trust"]}],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type": "object", "additionalProperties": false},
            "contributions": {"views": views, "commands": commands}
        }))
        .unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let mut files = vec![
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", ROUTE_ENTRY.as_bytes().to_vec()),
        ];
        if with_view {
            files.push(("ui/index.html", b"<!doctype html><title>x</title>".to_vec()));
        }
        for (name, bytes) in files {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn view_routes_follow_plugin_admission_and_are_cancelled_with_the_request() {
        use magi_api::PluginRouteExecutor;
        use magi_app_server_protocol::generated::{PluginRouteCallRequest, PluginRouteMethod};
        let fixture = fixture(Some(test_node()));
        let with_view = route_package("acme.guard", true);
        let without_view = route_package("acme.bare", false);
        {
            let mut manager = fixture.manager.lock().unwrap();
            for (package, name) in [(&with_view, "a.zip"), (&without_view, "b.zip")] {
                let id = package.manifest().id.clone();
                manager
                    .install(package, PluginSource::Local { name: name.into() })
                    .unwrap();
                manager.enable(&id).unwrap();
                manager
                    .authorize(&id, package.manifest().permissions.clone())
                    .unwrap();
                manager.activate(&id).unwrap();
            }
        }
        fixture.runtime.reconcile().await;
        for id in ["acme.guard", "acme.bare"] {
            fixture
                .runtime
                .running
                .lock()
                .await
                .get(id)
                .and_then(|entry| entry.supervisor.as_ref())
                .expect("已启动监管")
                .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
                .await;
        }
        let executor = super::super::plugin_bridge::NodeRouteExecutor::for_test(
            Arc::clone(&fixture.manager),
            Arc::clone(&fixture.bus),
        );
        let request = |path: &str, query: &str| PluginRouteCallRequest {
            method: PluginRouteMethod::GET,
            path: path.into(),
            query: Some(query.into()),
            headers: Some(json!({"x-panel-key": "panel-key"})),
            body: None,
        };
        // 有视图的插件：请求经 daemon 到达宿主里的处理器。
        let ok = executor
            .call("acme.guard".into(), request("/status", "a=1"))
            .await
            .unwrap();
        assert_eq!(ok.status, 200);
        let body = ok.body.unwrap();
        assert_eq!(body["echoed"], "a=1");
        assert_eq!(body["headers"]["x-panel-key"], "panel-key");
        // 没有视图的插件不开放后端路由，即使宿主里绑定了处理器。
        let refused = executor
            .call("acme.bare".into(), request("/status", ""))
            .await
            .unwrap_err();
        assert!(
            matches!(refused, magi_api::ApiError::Forbidden(_)),
            "{refused:?}"
        );
        // 未绑定的路径：明确 NotFound。
        let missing = executor
            .call("acme.guard".into(), request("/nothing", ""))
            .await
            .unwrap_err();
        assert!(
            matches!(missing, magi_api::ApiError::NotFound(_)),
            "{missing:?}"
        );
        // HTTP 请求被丢弃：宿主侧的处理器收到取消，而不是跑到期限。
        let hung = executor.call("acme.guard".into(), request("/hang", ""));
        let started = std::time::Instant::now();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(400), hung)
                .await
                .is_err(),
            "处理器在等待取消，调用不应自行结束"
        );
        // 取消后宿主立即可以服务下一次调用（旧调用没有占住处理器）。
        let after = executor
            .call("acme.guard".into(), request("/status", "b=2"))
            .await
            .unwrap();
        assert_eq!(after.body.unwrap()["echoed"], "b=2");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // 插件停用后：不再接受路由调用。
        fixture
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.guard")
            .unwrap();
        let stopped = executor
            .call("acme.guard".into(), request("/status", ""))
            .await
            .unwrap_err();
        assert!(
            matches!(stopped, magi_api::ApiError::Conflict(_)),
            "{stopped:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extension_commands_run_through_the_real_host_and_follow_stop_requests() {
        use magi_conversation_runtime::plugin_command::{
            PluginCommandExecutor, PluginCommandInvocation,
        };
        use std::sync::atomic::AtomicBool;
        let fixture = fixture(Some(test_node()));
        let package = tool_package();
        {
            let mut manager = fixture.manager.lock().unwrap();
            manager
                .install(
                    &package,
                    PluginSource::Local {
                        name: "c.zip".into(),
                    },
                )
                .unwrap();
            manager.enable("acme.guard").unwrap();
            manager
                .authorize("acme.guard", package.manifest().permissions.clone())
                .unwrap();
            manager.activate("acme.guard").unwrap();
            assert_eq!(manager.command_contributions().unwrap().len(), 3);
        }
        fixture.runtime.reconcile().await;
        fixture
            .runtime
            .running
            .lock()
            .await
            .get("acme.guard")
            .and_then(|entry| entry.supervisor.as_ref())
            .expect("已启动监管")
            .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
            .await;
        let executor = Arc::new(super::super::plugin_bridge::NodeCommandExecutor::for_test(
            Arc::clone(&fixture.manager),
            Arc::clone(&fixture.bus),
            tokio::runtime::Handle::current(),
        ));
        let call = |command: &'static str, flag: Arc<AtomicBool>| {
            let executor = Arc::clone(&executor);
            tokio::task::spawn_blocking(move || {
                executor.invoke(
                    PluginCommandInvocation {
                        job: None,
                        command_id: format!("plugin/acme.guard/{command}"),
                        turn_id: "t".into(),
                        context: magi_tool_runtime::ToolExecutionContext {
                            session_id: Some(magi_core::SessionId::new("s")),
                            ..Default::default()
                        },
                        input: "Magi".into(),
                    },
                    &flag,
                )
            })
        };
        let ok = call("hello", Arc::new(AtomicBool::new(false)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ok.content, "你好，Magi");
        let directory = std::fs::canonicalize(fixture._dir.path()).unwrap();
        let context_executor = Arc::clone(&executor);
        let expected_directory = directory.clone();
        let projected = tokio::task::spawn_blocking(move || {
            context_executor.invoke(
                PluginCommandInvocation {
                    command_id: "plugin/acme.guard/hello".into(),
                    turn_id: "context-turn".into(),
                    context: magi_tool_runtime::ToolExecutionContext {
                        session_id: Some(magi_core::SessionId::new("context-session")),
                        task_id: Some(magi_core::TaskId::new("context-task")),
                        worker_id: Some(magi_core::WorkerId::new("context-worker")),
                        working_directory: Some(directory),
                        ..Default::default()
                    },
                    input: "context".into(),
                    job: None,
                },
                &AtomicBool::new(false),
            )
        })
        .await
        .unwrap()
        .unwrap();
        let projected: serde_json::Value = serde_json::from_str(&projected.content).unwrap();
        assert_eq!(projected["owner"]["sessionId"], "context-session");
        assert_eq!(projected["owner"]["taskId"], "context-task");
        assert_eq!(projected["owner"]["workerId"], "context-worker");
        assert_eq!(
            projected["execution"]["workingDirectory"],
            expected_directory.to_str().unwrap()
        );
        assert_eq!(projected["execution"]["sourceFolders"], json!([]));
        let before_start = call("hello", Arc::new(AtomicBool::new(true)))
            .await
            .unwrap()
            .unwrap_err();
        assert!(before_start.contains("命令未执行"), "{before_start}");
        let error = call("bad", Arc::new(AtomicBool::new(false)))
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("命令失败"), "{error}");
        // 未声明的命令身份在到达宿主之前被拒绝。
        assert!(
            call("ghost", Arc::new(AtomicBool::new(false)))
                .await
                .unwrap()
                .is_err()
        );
        // 停止所属 Turn：运行中的命令立即返回错误，宿主收到取消。
        let flag = Arc::new(AtomicBool::new(false));
        let waiting = call("wait", Arc::clone(&flag));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let stopped = waiting.await.unwrap().unwrap_err();
        assert!(stopped.contains("取消"), "{stopped}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extension_tools_are_listed_only_when_authorized_and_execute_through_the_real_host() {
        use super::super::plugin_bridge::execute_tool;
        use magi_core::ExecutionResultStatus::{
            Cancelled, Failed, Indeterminate, Rejected, Succeeded,
        };
        use std::sync::atomic::AtomicBool;
        let fixture = fixture(Some(test_node()));
        let package = tool_package();
        {
            let mut manager = fixture.manager.lock().unwrap();
            manager
                .install(
                    &package,
                    PluginSource::Local {
                        name: "t.zip".into(),
                    },
                )
                .unwrap();
            manager.enable("acme.guard").unwrap();
            // 未授权时目录为空，也无法执行。
            assert!(manager.tool_contributions().unwrap().is_empty());
            manager
                .authorize("acme.guard", package.manifest().permissions.clone())
                .unwrap();
            manager.activate("acme.guard").unwrap();
            assert_eq!(manager.tool_contributions().unwrap().len(), 4);
        }
        fixture.runtime.reconcile().await;
        fixture
            .runtime
            .running
            .lock()
            .await
            .get("acme.guard")
            .and_then(|entry| entry.supervisor.as_ref())
            .expect("已启动监管")
            .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
            .await;

        let handle = tokio::runtime::Handle::current();
        let manager = Arc::clone(&fixture.manager);
        let bus = Arc::clone(&fixture.bus);
        let echo = magi_plugin_system::model_tool_name("acme.guard", "echo");
        let boom = magi_plugin_system::model_tool_name("acme.guard", "boom");
        let run_with = move |name: String, args: &'static str, flag: Arc<AtomicBool>| {
            let (manager, bus, handle) = (Arc::clone(&manager), Arc::clone(&bus), handle.clone());
            tokio::task::spawn_blocking(move || {
                execute_tool(
                    &manager,
                    &bus,
                    &handle,
                    &name,
                    args,
                    &magi_tool_runtime::ToolExecutionContext::default(),
                    &flag,
                )
            })
        };
        let run = |name: String, args: &'static str| {
            run_with(name, args, Arc::new(AtomicBool::new(false)))
        };
        let (ok, status) = run(echo.clone(), r#"{"text":"hi"}"#).await.unwrap();
        assert_eq!(status, Succeeded, "{ok}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&ok).unwrap()["output"]["echoed"],
            "hi"
        );
        // 参数不符合 inputSchema：不会到达宿主。
        let (_, status) = run(echo, r#"{"text":1}"#).await.unwrap();
        assert_eq!(status, Rejected);
        // 工具抛错是模型可见的工具失败。
        let (failed, status) = run(boom, "{}").await.unwrap();
        assert_eq!(status, Failed);
        assert!(failed.contains("工具失败"), "{failed}");
        // 用户停止：运行中的调用被取消，宿主收到取消信号；可能写入的工具记为 Indeterminate。
        for (tool, expected) in [("slow", Cancelled), ("slow-write", Indeterminate)] {
            let flag = Arc::new(AtomicBool::new(false));
            let call = run_with(
                magi_plugin_system::model_tool_name("acme.guard", tool),
                "{}",
                Arc::clone(&flag),
            );
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let started = std::time::Instant::now();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            let (payload, status) = call.await.unwrap();
            assert_eq!(status, expected, "{payload}");
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
        }
        // 已取消的调用不会启动。
        let flag = Arc::new(AtomicBool::new(true));
        let (_, status) = run_with(
            magi_plugin_system::model_tool_name("acme.guard", "echo"),
            r#"{"text":"x"}"#,
            flag,
        )
        .await
        .unwrap();
        assert_eq!(status, Cancelled);
        // 未知工具名被拒绝。
        let (_, status) = run("plugin__nope".into(), "{}").await.unwrap();
        assert_eq!(status, Rejected);
    }

    #[tokio::test]
    async fn activated_extension_packages_become_supervised_hosts_and_stop_on_deactivation() {
        let fixture = fixture(Some(test_node()));
        let package = package();
        activate(&fixture, &package);
        fixture.runtime.reconcile().await;
        fixture
            .runtime
            .running
            .lock()
            .await
            .get("acme.guard")
            .and_then(|entry| entry.supervisor.as_ref())
            .expect("已启动监管")
            .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
            .await;

        let ctx = HookContext::new("inv");
        let blocked = fixture
            .bus
            .invoke("tool.call", &ctx, call("rm -rf x"))
            .await
            .unwrap();
        assert!(
            matches!(blocked.verdict, Verdict::Blocked { .. }),
            "{blocked:?}"
        );
        let ok = fixture
            .bus
            .invoke("tool.call", &ctx, call("ls"))
            .await
            .unwrap();
        assert_eq!(ok.verdict, Verdict::Proceed(call("ls")));

        // 用户停用：先排空并回收宿主，再注销登记，之后才按变更后的策略放行。
        fixture
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.guard")
            .unwrap();
        fixture.runtime.reconcile().await;
        assert!(!fixture.runtime.is_running("acme.guard").await);
        let freed = fixture
            .bus
            .invoke("tool.call", &ctx, call("rm -rf x"))
            .await
            .unwrap();
        assert_eq!(freed.verdict, Verdict::Proceed(call("rm -rf x")));
    }

    fn model_source_package() -> PluginPackage {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Proxy", "description": "",
            "extension": {"hooks": []},
            "permissions": [
                {"kind": "extension", "targets": ["full-trust"]},
                {"kind": "modelSources", "targets": ["proxy"]}
            ],
            "dataSchemaVersion": 1,
            "settingsSchema": {"type": "object", "additionalProperties": false},
            "contributions": {"modelSources": [{"id": "proxy", "title": "Proxy", "description": ""}]}
        }))
        .unwrap();
        let entry = r#"
export default async function activate(api) {
  await api.modelSource.set('proxy', {
    baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'k', apiProtocol: 'openai_chat', models: [{ id: 'm1' }],
  });
}
"#;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", entry.as_bytes().to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    #[tokio::test]
    async fn plugin_model_sources_follow_the_supervised_instance_lifecycle() {
        let fixture = fixture(Some(test_node()));
        let settings = Arc::new(magi_settings_store::SettingsStore::new());
        fixture.runtime.bind_settings(&settings);
        activate(&fixture, &model_source_package());
        fixture.runtime.reconcile().await;
        fixture
            .runtime
            .running
            .lock()
            .await
            .get("acme.guard")
            .and_then(|entry| entry.supervisor.as_ref())
            .expect("已启动监管")
            .wait_for(|state| matches!(state, SupervisorState::Running { .. }))
            .await;
        let identities = |settings: &magi_settings_store::SettingsStore| {
            magi_conversation_runtime::main_model_sources::sources(settings)
                .unwrap()
                .into_iter()
                .map(|source| source.identity)
                .collect::<Vec<_>>()
        };
        assert_eq!(identities(&settings), ["plugin/acme.guard/proxy"]);

        // 用户停用插件：宿主回收后，数据源随之撤销。
        fixture
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.guard")
            .unwrap();
        fixture.runtime.reconcile().await;
        assert!(identities(&settings).is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dynamic_catalog_changes_reach_the_real_tool_entry_and_stop_with_the_host() {
        use super::super::plugin_bridge::execute_tool;
        use magi_core::ExecutionResultStatus::{Rejected, Succeeded};
        let fixture = fixture(Some(test_node()));
        let settings = Arc::new(magi_settings_store::SettingsStore::new());
        fixture.runtime.bind_settings(&settings);
        let manifest = json!({
            "sdkVersion": 2, "id": "acme.guard", "version": "1.0.0", "name": "Dynamic", "description": "",
            "extension": {"hooks": [], "runtimeContributions": ["tools", "modelSources"]},
            "permissions": [{"kind": "extension", "targets": ["full-trust"]}, {"kind": "tools", "targets": ["*"]}, {"kind": "modelSources", "targets": ["*"]}],
            "dataSchemaVersion": 1, "settingsSchema": {"type": "object"},
            "contributions": {"commands": [{"id": "configure", "title": "Configure", "description": ""}]}
        });
        let entry = br#"
export default async function activate(api) {
  let revision = 0, release;
  await api.command('configure', async ({ enabled }) => {
    await release?.();
    const tools = enabled ? [{ id: 'late', title: 'Late', description: '', readOnly: true,
      inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } }] : [];
    const modelSources = enabled ? [{ id: 'proxy', title: 'Proxy', description: '' }] : [];
    ({ revision } = await api.catalog.replace(revision, { tools, modelSources }));
    release = enabled ? await api.tool('late', async ({ text }) => ({ echoed: text })) : undefined;
    if (enabled) await api.modelSource.set('proxy', {
      baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'k', apiProtocol: 'openai_chat', models: [{ id: 'm1' }],
    });
    return 'updated';
  });
}
"#;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", entry.to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
        activate(&fixture, &package);
        fixture.runtime.reconcile().await;
        fixture
            .runtime
            .running
            .lock()
            .await
            .get("acme.guard")
            .and_then(|entry| entry.supervisor.as_ref())
            .unwrap()
            .wait_for(|state| {
                matches!(
                    state,
                    SupervisorState::Running { .. } | SupervisorState::Failed(_)
                )
            })
            .await;
        let instance = fixture.bus.instance("acme.guard").expect("真实宿主已就绪");
        assert!(
            fixture
                .manager
                .lock()
                .unwrap()
                .tool_contributions()
                .unwrap()
                .is_empty()
        );
        let configure = |enabled| {
            instance.invoke_command(
                "configure",
                json!({"enabled": enabled}),
                json!({}),
                std::time::Duration::from_secs(3),
                std::future::pending(),
            )
        };
        configure(true).await.unwrap();
        assert_eq!(
            fixture
                .manager
                .lock()
                .unwrap()
                .tool_contributions()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            magi_conversation_runtime::main_model_sources::sources(&settings)
                .unwrap()
                .len(),
            1
        );
        let run = |input: &'static str| {
            let manager = Arc::clone(&fixture.manager);
            let bus = Arc::clone(&fixture.bus);
            let handle = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                execute_tool(
                    &manager,
                    &bus,
                    &handle,
                    &magi_plugin_system::model_tool_name("acme.guard", "late"),
                    input,
                    &magi_tool_runtime::ToolExecutionContext::default(),
                    &std::sync::atomic::AtomicBool::new(false),
                )
            })
        };
        let (output, status) = run(r#"{"text":"live"}"#).await.unwrap();
        assert_eq!(status, Succeeded, "{output}");
        assert!(output.contains("live"));
        assert_eq!(run(r#"{"text":1}"#).await.unwrap().1, Rejected);
        configure(false).await.unwrap();
        assert!(
            fixture
                .manager
                .lock()
                .unwrap()
                .tool_contributions()
                .unwrap()
                .is_empty()
        );
        assert!(
            magi_conversation_runtime::main_model_sources::sources(&settings)
                .unwrap()
                .is_empty()
        );
        assert_eq!(run(r#"{"text":"removed"}"#).await.unwrap().1, Rejected);
        configure(true).await.unwrap();
        assert_eq!(run(r#"{"text":"rebound"}"#).await.unwrap().1, Succeeded);
        fixture
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.guard")
            .unwrap();
        fixture.runtime.reconcile().await;
        assert!(
            fixture
                .manager
                .lock()
                .unwrap()
                .tool_contributions()
                .unwrap()
                .is_empty()
        );
        assert!(!instance.is_alive());
    }

    #[tokio::test]
    async fn registry_change_events_drive_the_reconciler() {
        use magi_event_bus::EventEnvelope;
        let fixture = fixture(Some(test_node()));
        let events = Arc::new(InMemoryEventBus::new(32));
        fixture.runtime.start(Arc::clone(&events));
        activate(&fixture, &package());
        events.publish(EventEnvelope::domain(
            magi_core::EventId::unique("plugin-registry-changed"),
            "plugin.registry.changed",
            json!({}),
        ));
        let mut blocked = false;
        for _ in 0..100 {
            let outcome = fixture
                .bus
                .invoke("tool.call", &HookContext::new("inv"), call("rm x"))
                .await
                .unwrap();
            if matches!(outcome.verdict, Verdict::Blocked { ref reason, .. } if reason.contains("禁止 rm"))
            {
                blocked = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(blocked, "事件之后扩展必须被拉起并生效");
        fixture
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.guard")
            .unwrap();
        events.publish(EventEnvelope::domain(
            magi_core::EventId::unique("plugin-registry-changed"),
            "plugin.registry.changed",
            json!({}),
        ));
        for _ in 0..100 {
            if !fixture.runtime.is_running("acme.guard").await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("停用事件之后扩展必须被回收");
    }

    #[tokio::test]
    async fn without_a_node_runtime_guard_hooks_fail_closed_instead_of_silently_disappearing() {
        let fixture = fixture(None);
        let package = package();
        activate(&fixture, &package);
        fixture.runtime.reconcile().await;
        assert!(
            fixture
                .runtime
                .supervisor_state("acme.guard")
                .await
                .is_none()
        );
        let outcome = fixture
            .bus
            .invoke("tool.call", &HookContext::new("inv"), call("ls"))
            .await
            .unwrap();
        assert!(
            matches!(outcome.verdict, Verdict::Blocked { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn supervisor_states_map_to_user_visible_runtime_status_with_the_blocking_guards() {
        let guards = vec!["tool.call".to_string()];
        let running = runtime_status(&SupervisorState::Running { generation: 3 }, &guards, 1);
        assert_eq!(running.state, PluginRuntimeState::Running);
        assert_eq!(
            (running.generation, running.crashes),
            (3, 2),
            "代次推出的重启次数"
        );
        assert!(running.blocking.is_empty(), "运行中没有被阻止的动作");

        let restarting = runtime_status(
            &SupervisorState::Backoff {
                crashes: 2,
                reason: "宿主进程退出".into(),
            },
            &guards,
            0,
        );
        assert_eq!(restarting.state, PluginRuntimeState::Restarting);
        assert_eq!(restarting.reason.as_deref(), Some("宿主进程退出"));
        assert_eq!(
            restarting.blocking, guards,
            "不可用期间 guard 钩子在阻止动作"
        );

        let failed = runtime_status(&SupervisorState::Failed("崩溃过于频繁".into()), &guards, 6);
        assert_eq!(
            (failed.state, failed.crashes),
            (PluginRuntimeState::Failed, 6)
        );
        assert_eq!(failed.blocking, guards);
    }

    #[tokio::test]
    async fn runtime_status_follows_the_lifecycle_and_notifies_the_ui() {
        use magi_api::ExtensionStatusSource;
        let fixture = fixture(Some(test_node()));
        let events = Arc::new(InMemoryEventBus::new(32));
        let mut feed = events.subscribe();
        fixture.runtime.start(Arc::clone(&events));
        activate(&fixture, &package());
        fixture.runtime.reconcile().await;
        // 状态变化事件到达，状态最终为运行中，且没有被阻止的动作。
        let mut running = None;
        for _ in 0..100 {
            if let Some(status) = fixture.runtime.status("acme.guard")
                && status.state == PluginRuntimeState::Running
            {
                running = Some(status);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let running = running.expect("扩展应进入运行状态");
        assert!(running.blocking.is_empty());
        let mut saw_event = false;
        while let Ok(event) = feed.try_recv() {
            saw_event |= event.event_type == "plugin.extension.status.changed";
        }
        assert!(saw_event, "状态变化必须通知界面");
        // 停用后状态消失（界面回到普通的已停用显示）。
        fixture
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.guard")
            .unwrap();
        fixture.runtime.reconcile().await;
        assert!(fixture.runtime.status("acme.guard").is_none());
    }

    #[tokio::test]
    async fn a_missing_node_runtime_is_visible_as_failed_with_the_reason_and_blocking_guards() {
        use magi_api::ExtensionStatusSource;
        let fixture = fixture(None);
        activate(&fixture, &package());
        fixture.runtime.reconcile().await;
        let status = fixture
            .runtime
            .status("acme.guard")
            .expect("失败也必须可见");
        assert_eq!(status.state, PluginRuntimeState::Failed);
        assert!(status.reason.unwrap().contains("随包 Node"));
        assert_eq!(
            status.blocking,
            ["tool.call"],
            "用户要能看到哪些动作正被 guard 阻止"
        );
    }
}
