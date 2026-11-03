//! daemon 对插件系统的装配。
//!
//! `magi-plugin-system` 拥有安装、授权与资源的事实；本模块只持有插件管理器、
//! 扩展总线与受信 Node 扩展运行时，并把它们接到 daemon 的执行链路上。这里不保存任何插件
//! 状态，也不另建权限判定。

use super::config::DaemonError;
use magi_event_bus::InMemoryEventBus;
use magi_plugin_system::{PluginError, PluginManager};
use magi_process::ManagedProcessGroup;
use magi_tool_runtime::{
    PluginToolCatalogEntry, PluginToolCatalogProvider, PluginToolExecutor, ToolExecutionContext,
};
use serde_json::json;
use std::time::Duration;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_PLUGIN_INVOCATION: AtomicU64 = AtomicU64::new(1);

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

    pub(super) fn bind_settings(&self, settings: &Arc<magi_settings_store::SettingsStore>) {
        self.extensions.bind_settings(settings);
    }

    pub(super) fn bind_native(
        &self,
        dispatcher: &Arc<magi_conversation_runtime::task_execution_dispatcher::LlmTaskDispatcher>,
    ) {
        self.extensions.bind_native(dispatcher);
    }

    /// 扩展宿主运行状态的只读来源，供插件列表投影合并。
    pub(super) fn extension_status(&self) -> Arc<dyn magi_api::ExtensionStatusSource> {
        Arc::new(self.extensions.clone())
    }

    pub(super) fn extension_bus(&self) -> Arc<magi_extension_host::bus::ExtensionBus> {
        Arc::clone(&self.extension_bus)
    }

    /// 模型可见的插件工具目录：与执行共用 `PluginManager` 的授权判定。
    pub(super) fn tool_catalog_provider(&self) -> PluginToolCatalogProvider {
        let manager = Arc::clone(&self.manager);
        Arc::new(move || {
            let Ok(manager) = manager.lock() else {
                return Vec::new();
            };
            manager
                .tool_contributions()
                .unwrap_or_default()
                .into_iter()
                .map(|(manifest, tool)| PluginToolCatalogEntry {
                    model_tool_name: magi_plugin_system::model_tool_name(&manifest.id, &tool.id),
                    plugin_id: manifest.id,
                    contribution_id: tool.id,
                    title: tool.title,
                    description: tool.description,
                    input_schema: tool.input_schema,
                    read_only: tool.read_only,
                })
                .collect()
        })
    }

    /// 执行模型发起的插件工具调用。对话循环在阻塞线程上同步调用，这里用 daemon 的
    /// tokio Handle 等待扩展宿主；目录、授权与参数校验都已在 `prepare_tool_call` 完成。
    pub(super) fn tool_executor(&self, handle: tokio::runtime::Handle) -> PluginToolExecutor {
        let manager = Arc::clone(&self.manager);
        let bus = Arc::clone(&self.extension_bus);
        Arc::new(move |model_tool_name, arguments, context, cancelled| {
            execute_tool(
                &manager,
                &bus,
                &handle,
                model_tool_name,
                arguments,
                context,
                cancelled,
            )
        })
    }

    /// 用户命令的执行端口：对话执行器在阻塞线程上同步调用。
    pub(super) fn command_executor(
        &self,
        handle: tokio::runtime::Handle,
    ) -> Arc<dyn magi_conversation_runtime::plugin_command::PluginCommandExecutor> {
        Arc::new(NodeCommandExecutor {
            manager: Arc::clone(&self.manager),
            bus: Arc::clone(&self.extension_bus),
            handle,
        })
    }

    pub(super) fn manager(&self) -> Arc<Mutex<PluginManager>> {
        Arc::clone(&self.manager)
    }
}

pub(super) struct NodeCommandExecutor {
    manager: Arc<Mutex<PluginManager>>,
    bus: Arc<magi_extension_host::bus::ExtensionBus>,
    handle: tokio::runtime::Handle,
}

impl NodeCommandExecutor {
    #[cfg(test)]
    pub(super) fn for_test(
        manager: Arc<Mutex<PluginManager>>,
        bus: Arc<magi_extension_host::bus::ExtensionBus>,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            manager,
            bus,
            handle,
        }
    }
}

impl magi_conversation_runtime::plugin_command::PluginCommandExecutor for NodeCommandExecutor {
    fn invoke(
        &self,
        invocation: magi_conversation_runtime::plugin_command::PluginCommandInvocation,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<magi_conversation_runtime::plugin_command::PluginCommandResult, String> {
        use magi_conversation_runtime::plugin_command::PluginCommandResult;
        let selected = magi_plugin_system::ContributionRef::parse("命令", &invocation.command_id)?;
        // 与目录同一判定：命令必须来自当前激活的插件。
        let digest = {
            let manager = self
                .manager
                .lock()
                .map_err(|_| "插件管理器不可用".to_string())?;
            let available = manager
                .command_contributions()
                .map_err(|error| error.to_string())?
                .into_iter()
                .any(|(manifest, command)| {
                    manifest.id == selected.plugin_id && command.id == selected.contribution_id
                });
            if !available {
                return Err("插件命令未在当前全局状态激活".into());
            }
            manager
                .package(&selected.plugin_id)
                .map_err(|error| error.to_string())?
                .digest()
                .to_owned()
        };
        let instance = self
            .bus
            .instance(&selected.plugin_id)
            .ok_or_else(|| "扩展宿主未运行".to_string())?;
        if instance.digest != digest {
            return Err("扩展宿主不是当前授权的版本".into());
        }
        let ctx = json!({
            "job": invocation.job,
            "invocationId": format!("cmd_{}", NEXT_PLUGIN_INVOCATION.fetch_add(1, Ordering::Relaxed)),
            "owner": {"sessionId": invocation.session_id, "turnId": invocation.turn_id},
            "binding": {"workspaceId": invocation.workspace_id},
        });
        let wait_cancelled = async {
            while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        let timeout = invocation
            .job
            .as_ref()
            .map(|job| Duration::from_millis(job.budget.timeout_ms as u64))
            .unwrap_or(COMMAND_TIMEOUT);
        let value = self
            .handle
            .block_on(instance.invoke_command(
                &selected.contribution_id,
                json!({"text": invocation.input}),
                ctx,
                timeout,
                wait_cancelled,
            ))
            .map_err(|error| error.to_string())?;
        let content = value
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if value.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            return Err(content);
        }
        if content.trim().is_empty() || content.len() > 1_048_576 {
            return Err("插件命令返回文本为空或超出上限".into());
        }
        Ok(PluginCommandResult { content })
    }
}

/// 单次插件命令的上限；命令可能比工具更久，但仍受期限与停止取消约束。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// 单次插件工具调用的上限；超时后宿主收到取消信号。
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn execute_tool(
    manager: &Mutex<PluginManager>,
    bus: &magi_extension_host::bus::ExtensionBus,
    handle: &tokio::runtime::Handle,
    model_tool_name: &str,
    arguments: &str,
    context: &ToolExecutionContext,
    cancelled: &std::sync::atomic::AtomicBool,
) -> (String, magi_core::ExecutionResultStatus) {
    use magi_core::ExecutionResultStatus::{Cancelled, Failed, Indeterminate, Rejected, Succeeded};
    let failure = |status, code: &str, message: &str| {
        (
            json!({
                "tool": model_tool_name,
                "status": match status {
                    Rejected => "rejected",
                    Cancelled => "cancelled",
                    Indeterminate => "indeterminate",
                    _ => "failed",
                },
                "error_code": code,
                "error": message,
            })
            .to_string(),
            status,
        )
    };
    let prepared = match manager
        .lock()
        .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))
        .and_then(|manager| manager.prepare_tool_call(model_tool_name, arguments))
    {
        Ok(prepared) => prepared,
        Err(error) => {
            let status = match error {
                PluginError::NotFound(_)
                | PluginError::NotAuthorized(_)
                | PluginError::InvalidInput(_) => Rejected,
                _ => Failed,
            };
            return failure(status, "plugin_tool_unavailable", &error.to_string());
        }
    };
    let Some(instance) = bus.instance(&prepared.plugin_id) else {
        return failure(Failed, "plugin_host_unavailable", "扩展宿主未运行");
    };
    // 校验后升级换包会让旧宿主继续存在一小段时间；摘要不符的宿主不得执行新授权的调用。
    if instance.digest != prepared.digest {
        return failure(Failed, "plugin_host_stale", "扩展宿主不是当前授权的版本");
    }
    let invocation_id = format!(
        "tool_{}",
        NEXT_PLUGIN_INVOCATION.fetch_add(1, Ordering::Relaxed)
    );
    let ctx = json!({
        "invocationId": format!("{invocation_id}.{}", model_tool_name),
        "owner": {
            "sessionId": context.session_id.as_ref().map(ToString::to_string),
            "taskId": context.task_id.as_ref().map(ToString::to_string),
            "workerId": context.worker_id.as_ref().map(ToString::to_string),
        },
        "binding": {"workspaceId": context.workspace_id.as_ref().map(ToString::to_string)},
    });
    // 开始前已取消则不启动调用；运行中由标志轮询转成宿主取消。
    if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return failure(Cancelled, "tool_cancelled", "调用已取消，工具未执行");
    }
    let wait_cancelled = async {
        while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let read_only = prepared.tool.read_only;
    let result = handle.block_on(instance.invoke_tool(
        &prepared.tool.id,
        prepared.input,
        ctx,
        TOOL_CALL_TIMEOUT,
        wait_cancelled,
    ));
    match result {
        Ok(value) => {
            let is_error = value.get("isError").and_then(|v| v.as_bool()) == Some(true);
            let output = value
                .get("output")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if is_error {
                (
                    json!({"tool": model_tool_name, "status": "failed", "error_code": "plugin_tool_error", "error": output}).to_string(),
                    Failed,
                )
            } else {
                (
                    json!({"tool": model_tool_name, "status": "succeeded", "output": output})
                        .to_string(),
                    Succeeded,
                )
            }
        }
        Err(error) if error.kind() == magi_extension_host::protocol::ErrorKind::Cancelled => {
            // 只读工具取消后没有副作用；可能写入的工具无法确认是否已生效，不得自动重试。
            let status = if read_only { Cancelled } else { Indeterminate };
            failure(status, "tool_cancelled", "调用已取消")
        }
        Err(error)
            if matches!(
                error.kind(),
                magi_extension_host::protocol::ErrorKind::DeadlineExceeded
                    | magi_extension_host::protocol::ErrorKind::HostGone
                    | magi_extension_host::protocol::ErrorKind::ProtocolError
                    | magi_extension_host::protocol::ErrorKind::Internal
            ) =>
        {
            failure(
                if read_only { Failed } else { Indeterminate },
                "plugin_execution_unconfirmed",
                &error.to_string(),
            )
        }
        Err(error) => failure(
            Failed,
            &format!("plugin_{:?}", error.kind()).to_lowercase(),
            &error.to_string(),
        ),
    }
}
