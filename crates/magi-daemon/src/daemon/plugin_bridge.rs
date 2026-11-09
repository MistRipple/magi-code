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

    pub(super) fn bind_operations(&self, state: &magi_api::ApiState) {
        self.extensions
            .bind_operations(magi_api::PluginOperations::from_state(state));
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

    /// 模型可见的插件工具目录：与执行共用 `PluginManager` 的授权判定，
    /// 并且只列出当前宿主实例已绑定处理器的工具（作者可在运行期按配置上下架工具）。
    pub(super) fn tool_catalog_provider(&self) -> PluginToolCatalogProvider {
        let manager = Arc::clone(&self.manager);
        let bus = Arc::clone(&self.extension_bus);
        Arc::new(move || {
            let Ok(manager) = manager.lock() else {
                return Vec::new();
            };
            manager
                .tool_contributions()
                .unwrap_or_default()
                .into_iter()
                .filter(|(manifest, tool)| {
                    bus.instance(&manifest.id)
                        .is_some_and(|instance| instance.is_tool_bound(&tool.id))
                })
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

    /// 插件视图后端路由的执行端口：API 的 `/plugins/{id}/routes` 经此转交给扩展宿主。
    pub(super) fn route_executor(&self) -> Arc<dyn magi_api::PluginRouteExecutor> {
        Arc::new(NodeRouteExecutor {
            manager: Arc::clone(&self.manager),
            bus: Arc::clone(&self.extension_bus),
        })
    }

    pub(super) fn toolchain(&self) -> Option<magi_api::PluginToolchain> {
        self.extensions.toolchain()
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
        if cancelled.load(Ordering::SeqCst) {
            return Err("调用已取消，命令未执行".into());
        }
        let session_id = invocation
            .context
            .session_id
            .as_ref()
            .ok_or("插件命令缺少所属会话")?;
        let execution = execution_context(&invocation.context)?;
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
            "activity": "command",
            "invocationId": format!("cmd_{}", NEXT_PLUGIN_INVOCATION.fetch_add(1, Ordering::Relaxed)),
            "owner": {"sessionId": session_id, "turnId": invocation.turn_id,
                "taskId": invocation.context.task_id, "workerId": invocation.context.worker_id},
            "binding": {"workspaceId": invocation.context.workspace_id},
            "execution": execution,
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
        if cancelled.load(Ordering::SeqCst) {
            return Err("调用已取消，命令未执行".into());
        }
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

/// 单次视图后端路由调用的上限。路由里有账号测试、导入导出这类耗时操作，期限比工具宽，
/// 但仍有界；HTTP 请求被丢弃时宿主侧调用随即取消。
const ROUTE_CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// 视图后端路由的执行者。授权判定与工具、命令同源：插件必须在当前全局状态激活，
/// 宿主摘要必须是当前授权的包；路由本身只服务该插件自己的视图。
pub(super) struct NodeRouteExecutor {
    manager: Arc<Mutex<PluginManager>>,
    bus: Arc<magi_extension_host::bus::ExtensionBus>,
}

impl NodeRouteExecutor {
    #[cfg(test)]
    pub(super) fn for_test(
        manager: Arc<Mutex<PluginManager>>,
        bus: Arc<magi_extension_host::bus::ExtensionBus>,
    ) -> Self {
        Self { manager, bus }
    }
}

/// 外层 HTTP 请求被丢弃（视图关闭、连接断开）时通知宿主取消。
struct CancelOnDrop(Arc<tokio::sync::Notify>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl magi_api::PluginRouteExecutor for NodeRouteExecutor {
    fn call(
        &self,
        plugin_id: String,
        request: magi_app_server_protocol::generated::PluginRouteCallRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        magi_app_server_protocol::generated::PluginRouteCallResult,
                        magi_api::ApiError,
                    >,
                > + Send,
        >,
    > {
        use magi_api::ApiError;
        use magi_extension_host::protocol::ErrorKind;
        let manager = Arc::clone(&self.manager);
        let bus = Arc::clone(&self.bus);
        Box::pin(async move {
            validate_route_body(
                request.body.as_ref(),
                request.body_base64.as_deref(),
                2 * 1024 * 1024,
            )
            .map_err(ApiError::InvalidInput)?;
            if matches!(
                request.method,
                magi_app_server_protocol::generated::PluginRouteMethod::GET
                    | magi_app_server_protocol::generated::PluginRouteMethod::HEAD
            ) && (request.body.is_some() || request.body_base64.is_some())
            {
                return Err(ApiError::InvalidInput("GET/HEAD 请求不能携带正文".into()));
            }
            let digest = {
                let manager = manager.lock().map_err(|_| {
                    ApiError::internal_assembly("插件路由调用失败", "插件管理器不可用")
                })?;
                if !manager
                    .is_active(&plugin_id)
                    .map_err(|error| ApiError::Conflict(error.to_string()))?
                {
                    return Err(ApiError::Conflict("插件未激活".into()));
                }
                let package = manager
                    .package(&plugin_id)
                    .map_err(|error| ApiError::NotFound(error.to_string()))?;
                // 后端路由只为插件自己的视图存在；没有视图的插件不开放这个入口。
                if package.manifest().contributions.views.is_empty() {
                    return Err(ApiError::Forbidden("插件未声明视图，不开放后端路由".into()));
                }
                package.digest().to_owned()
            };
            let instance = bus
                .instance(&plugin_id)
                .ok_or_else(|| ApiError::Conflict("扩展宿主未运行".into()))?;
            if request
                .expected_digest
                .as_ref()
                .is_some_and(|expected| expected != &digest)
                || request
                    .expected_instance_id
                    .as_ref()
                    .is_some_and(|expected| expected != &instance.instance_id)
            {
                return Err(ApiError::Conflict(
                    "插件路由所属页面版本或实例已失效；未执行业务".into(),
                ));
            }
            if instance.digest != digest {
                return Err(ApiError::Conflict("扩展宿主不是当前授权的版本".into()));
            }
            let ctx = json!({
                "invocationId": format!("route_{}", NEXT_PLUGIN_INVOCATION.fetch_add(1, Ordering::Relaxed)),
                "owner": {},
                "binding": {},
            });
            let input = serde_json::to_value(&request)
                .map_err(|error| ApiError::internal_assembly("插件路由调用失败", error))?;
            if serde_json::to_vec(&input)
                .map_err(|error| ApiError::internal_assembly("插件路由调用失败", error))?
                .len()
                > 3 * 1024 * 1024 + 64 * 1024
            {
                return Err(ApiError::InvalidInput(
                    "插件路由请求超过消息大小上限".into(),
                ));
            }
            // 在独立任务里等待宿主：外层请求被丢弃时由 CancelOnDrop 触发取消，
            // 而不是让宿主侧的处理器白白跑到期限。
            let cancel = Arc::new(tokio::sync::Notify::new());
            let _guard = CancelOnDrop(Arc::clone(&cancel));
            let path = request.path;
            let calling_instance = Arc::clone(&instance);
            let task = tokio::spawn(async move {
                calling_instance
                    .invoke_route(&path, input, ctx, ROUTE_CALL_TIMEOUT, async move {
                        cancel.notified().await
                    })
                    .await
            });
            let value = task
                .await
                .map_err(|error| ApiError::internal_assembly("插件路由调用失败", error))?
                .map_err(|error| match error.kind() {
                    ErrorKind::MethodNotFound => ApiError::NotFound(error.message),
                    ErrorKind::Cancelled => ApiError::Conflict("路由调用已取消".into()),
                    ErrorKind::DeadlineExceeded => ApiError::Conflict("路由处理超时".into()),
                    _ => ApiError::internal_assembly("插件路由调用失败", error.message),
                })?;
            let status = value
                .get("status")
                .and_then(serde_json::Value::as_u64)
                .and_then(|status| u16::try_from(status).ok())
                .filter(|status| (100..=599).contains(status))
                .ok_or_else(|| {
                    ApiError::internal_assembly("插件路由调用失败", "宿主返回的状态码无效")
                })?;
            let result: magi_app_server_protocol::generated::PluginRouteCallResult =
                serde_json::from_value(value)
                    .map_err(|error| ApiError::internal_assembly("插件路由响应无效", error))?;
            validate_route_body(
                result.body.as_ref(),
                result.body_base64.as_deref(),
                16 * 1024 * 1024,
            )
            .map_err(|error| ApiError::internal_assembly("插件路由响应无效", error))?;
            // 大响应仍属最初的安装及实例；交付期间停用、升级或重启不能把旧字节交给新页面。
            {
                let manager = manager.lock().map_err(|_| {
                    ApiError::internal_assembly("插件路由调用失败", "插件管理器不可用")
                })?;
                if !manager
                    .is_active(&plugin_id)
                    .map_err(|error| ApiError::Conflict(error.to_string()))?
                    || manager
                        .package(&plugin_id)
                        .map_err(|error| ApiError::Conflict(error.to_string()))?
                        .digest()
                        != digest
                    || bus
                        .instance(&plugin_id)
                        .is_none_or(|current| !Arc::ptr_eq(&current, &instance))
                {
                    return Err(ApiError::Conflict(
                        "路由响应所属安装或实例已失效；未重复执行业务".into(),
                    ));
                }
            }
            Ok(magi_app_server_protocol::generated::PluginRouteCallResult {
                status,
                headers: route_response_headers(result.headers)
                    .map_err(|error| ApiError::internal_assembly("插件路由响应头无效", error))?,
                ..result
            })
        })
    }
}

fn validate_route_body(
    body: Option<&serde_json::Value>,
    bytes: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    use base64::Engine;
    if let Some(body) = body {
        let size = match body {
            serde_json::Value::String(text) => text.len(),
            value => serde_json::to_vec(value)
                .map_err(|_| "路由正文无法编码")?
                .len(),
        };
        if size > max_bytes {
            return Err("插件路由正文超过大小上限".into());
        }
    }
    if let Some(bytes) = bytes {
        if body.is_some_and(|body| !body.is_null()) {
            return Err("插件路由不能同时携带 JSON 与字节载荷".into());
        }
        let base64 = base64::engine::general_purpose::STANDARD;
        if bytes.len() > max_bytes.div_ceil(3) * 4 {
            return Err("插件路由二进制载荷超过大小上限".into());
        }
        let decoded = base64
            .decode(bytes)
            .map_err(|_| "插件路由 base64 载荷无效")?;
        if decoded.len() > max_bytes || base64.encode(&decoded) != bytes {
            return Err("插件路由 base64 载荷无效或超过大小上限".into());
        }
    }
    Ok(())
}

fn route_response_headers(
    headers: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>, String> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let Some(headers) = headers.as_object() else {
        return Err("响应头必须为对象".into());
    };
    if headers.len() > 64 {
        return Err("响应头数量超过上限".into());
    }
    let mut result = serde_json::Map::new();
    for (name, value) in headers {
        let name =
            axum::http::HeaderName::from_bytes(name.as_bytes()).map_err(|_| "响应头名称无效")?;
        let value = value.as_str().ok_or("响应头值必须为字符串")?;
        if value.len() > 8192 || axum::http::HeaderValue::from_str(value).is_err() {
            return Err("响应头值无效".into());
        }
        let key = name.as_str();
        // 字节已由桥解码，不能再携带 HTTP 压缩/长度；插件 Cookie 不进入 Magi 的可信 origin。
        if matches!(
            key,
            "set-cookie"
                | "host"
                | "origin"
                | "cookie"
                | "connection"
                | "content-length"
                | "content-encoding"
                | "transfer-encoding"
                | "upgrade"
                | "forwarded"
        ) || key.starts_with("proxy-")
            || key.starts_with("sec-")
            || key.starts_with("x-forwarded-")
        {
            continue;
        }
        result.insert(key.into(), value.into());
    }
    Ok(Some(result.into()))
}

/// 单次插件命令的上限；命令可能比工具更久，但仍受期限与停止取消约束。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// 单次插件工具调用的上限；超时后宿主收到取消信号。
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// 工具与命令共用的调用期执行事实。只暴露实际执行根，绝不暴露隔离前的源路径。
fn execution_context(context: &ToolExecutionContext) -> Result<serde_json::Value, String> {
    // 复核全部目录是否仍存在且身份未变；保留调用入口已选择的工具目录。
    context
        .for_tool_arguments("{}")
        .map_err(|error| error.to_string())?;
    let directory = |path: &Path| -> Result<String, String> {
        if !path.is_absolute()
            || !path.is_dir()
            || std::fs::canonicalize(path).map_err(|error| error.to_string())? != path
        {
            return Err("本次执行目录不可用或身份已变化".into());
        }
        path.to_str()
            .map(str::to_owned)
            .ok_or("插件执行目录必须可用 UTF-8 表达".into())
    };
    let working_directory = context
        .working_directory
        .as_deref()
        .map(directory)
        .transpose()?;
    let mut folders = Vec::new();
    if let Some(map) = &context.project_directory_map {
        for root in &map.roots {
            folders.push(json!({
                "sourceFolderId": root.source_folder_id,
                "workingDirectory": directory(&root.execution_root().map_err(|error| error.to_string())?)?,
            }));
        }
        if !folders
            .iter()
            .any(|folder| folder["workingDirectory"].as_str() == working_directory.as_deref())
        {
            return Err("插件执行目录不属于本次获授的项目目录".into());
        }
    } else if context.workspace_id.is_some() {
        return Err("插件项目调用缺少可信执行目录映射".into());
    }
    Ok(json!({
        "workingDirectory": working_directory,
        "sourceFolders": folders,
        "accessProfile": context.access_profile,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        HostPath, ProjectDirectoryExecutionMap, ProjectDirectoryExecutionRoot, WorkspaceId,
    };

    #[test]
    fn route_payload_validates_binary_before_dispatch_and_preserves_only_application_headers() {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.encode([0, 255, 128, 13, 10]);
        assert!(validate_route_body(None, Some(&bytes), 2 * 1024 * 1024).is_ok());
        assert!(validate_route_body(Some(&json!({})), Some(&bytes), 2 * 1024 * 1024).is_err());
        for value in ["?", "Zg=", "Zh==", "Zg==\n"] {
            assert!(
                validate_route_body(None, Some(value), 2 * 1024 * 1024).is_err(),
                "{value:?}"
            );
        }
        let too_large =
            base64::engine::general_purpose::STANDARD.encode(vec![0; 2 * 1024 * 1024 + 1]);
        assert!(validate_route_body(None, Some(&too_large), 2 * 1024 * 1024).is_err());
        assert!(validate_route_body(None, Some(&too_large), 16 * 1024 * 1024).is_ok());
        let text = json!("完整🙂".repeat(300_000));
        assert!(validate_route_body(Some(&text), None, 2 * 1024 * 1024).is_err());
        assert!(validate_route_body(Some(&text), None, 16 * 1024 * 1024).is_ok());
        let headers = route_response_headers(Some(json!({
            "Content-Disposition": "attachment; filename=report.zip", "ETag": "original",
            "Set-Cookie": "must-not-reach-magi=1", "Content-Length": "999", "Connection": "close"
        })))
        .unwrap()
        .unwrap();
        assert_eq!(
            headers,
            json!({"content-disposition":"attachment; filename=report.zip", "etag":"original"})
        );
        assert!(route_response_headers(Some(json!({"valid":"invalid\r\nvalue"}))).is_err());
        assert!(route_response_headers(Some(json!({"valid":42}))).is_err());
    }

    #[test]
    fn execution_projection_preserves_selected_copy_and_rejects_missing_or_forged_roots() {
        let work = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(work.path()).unwrap();
        let directories: Vec<_> = ["source-a", "source-b", "copy-a", "copy-b"]
            .into_iter()
            .map(|name| {
                let path = root.join(name);
                std::fs::create_dir(&path).unwrap();
                path
            })
            .collect();
        let path_ref = |path: &Path| {
            HostPath::from_path(path.to_path_buf())
                .to_path_ref()
                .as_str()
                .to_owned()
        };
        let context = ToolExecutionContext {
            workspace_id: Some(WorkspaceId::new("project")),
            working_directory: Some(directories[3].clone()),
            project_directory_map: Some(ProjectDirectoryExecutionMap {
                workspace_id: WorkspaceId::new("project"),
                default_source_folder_id: "a".into(),
                roots: (0..2)
                    .map(|i| ProjectDirectoryExecutionRoot {
                        source_folder_id: ["a", "b"][i].into(),
                        source_root_ref: path_ref(&directories[i]),
                        execution_root_ref: path_ref(&directories[i + 2]),
                    })
                    .collect(),
            }),
            ..Default::default()
        };
        let projection = execution_context(&context).unwrap();
        assert_eq!(
            projection["workingDirectory"],
            directories[3].to_str().unwrap()
        );
        assert_eq!(
            projection["sourceFolders"][0]["workingDirectory"],
            directories[2].to_str().unwrap()
        );
        assert!(!projection.to_string().contains("source-a"));
        assert!(!projection.to_string().contains("source-b"));
        let mut forged = context.clone();
        forged.working_directory = Some(directories[1].clone());
        assert!(execution_context(&forged).unwrap_err().contains("不属于"));
        forged.project_directory_map = None;
        assert!(execution_context(&forged).unwrap_err().contains("缺少"));
        std::fs::remove_dir(&directories[2]).unwrap();
        assert!(
            execution_context(&context).is_err(),
            "副本失效不能回落原目录"
        );
    }
}

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
    let execution = match execution_context(context) {
        Ok(execution) => execution,
        Err(error) => return failure(Rejected, "plugin_execution_context_invalid", &error),
    };
    let invocation_id = format!(
        "tool_{}",
        NEXT_PLUGIN_INVOCATION.fetch_add(1, Ordering::Relaxed)
    );
    let ctx = json!({
        "invocationId": format!("{invocation_id}.{}", model_tool_name),
        "activity": "tool",
        "owner": {
            "sessionId": context.session_id.as_ref().map(ToString::to_string),
            "taskId": context.task_id.as_ref().map(ToString::to_string),
            "workerId": context.worker_id.as_ref().map(ToString::to_string),
        },
        "binding": {"workspaceId": context.workspace_id.as_ref().map(ToString::to_string)},
        "execution": execution,
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
    // 作者在清单里声明的期限（最长 10 分钟）优先；未声明沿用默认的 60 秒。
    let timeout = prepared
        .tool
        .timeout_ms
        .and_then(|ms| u64::try_from(ms).ok())
        .map(Duration::from_millis)
        .unwrap_or(TOOL_CALL_TIMEOUT);
    let result = handle.block_on(instance.invoke_tool(
        &prepared.tool.id,
        prepared.input,
        ctx,
        timeout,
        wait_cancelled,
    ));
    match result {
        Ok(value) => {
            use magi_extension_host::protocol::{PluginToolCompletion, ToolInvocationResult};
            let result = serde_json::from_value::<ToolInvocationResult>(value)
                .map_err(|error| error.to_string())
                .and_then(|result| result.validate().map(|()| result).map_err(str::to_string));
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    return failure(
                        if read_only { Failed } else { Indeterminate },
                        "plugin_tool_result_invalid",
                        &error,
                    );
                }
            };
            if result.is_error {
                (
                    json!({"tool": model_tool_name, "status": "failed", "error_code": "plugin_tool_error", "error": result.output}).to_string(),
                    Failed,
                )
            } else {
                let mut payload = json!({"tool": model_tool_name, "status": "succeeded", "output": result.output});
                if let Some(content) = result.completion {
                    payload["pluginCompletion"] = serde_json::to_value(PluginToolCompletion {
                        plugin_id: prepared.plugin_id,
                        contribution_id: prepared.tool.id,
                        content,
                    })
                    .expect("tool completion serializes");
                }
                (payload.to_string(), Succeeded)
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
