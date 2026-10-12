//! daemon 对插件系统的装配。
//!
//! `magi-plugin-system` 拥有安装、授权、作用域与资源的事实；本模块只把这些服务接到
//! daemon 的执行链路上：工具目录与执行、插件后台 SDK 的能力处理器、会话引擎路由、
//! 工作流核心工厂和显式命令执行。这里不保存任何插件状态，也不另建权限判定。

use super::config::DaemonError;
use magi_event_bus::InMemoryEventBus;
use magi_plugin_runtime::{
    CapabilityHandler, CapabilityRequest, ExecutionError, ExecutionErrorCode, Invocation,
    InvocationContribution, InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
};
use magi_plugin_system::{
    APPLICATION_SCOPE, NativeSessionEngineFactory, PluginCommandExecutor, PluginCommandRunner,
    PluginError, PluginManager, PluginRuntimeLease, PluginSessionEngineFactory,
    PluginWorkflowCoreFactory, SessionEngineFactory, SessionEngineRouter, WorkflowCoreFactory,
    WorkflowCoreRouter,
};
use magi_process::ManagedProcessGroup;
use magi_tool_runtime::{
    PluginToolCatalogEntry, PluginToolCatalogProvider, PluginToolExecutor, ToolExecutionContext,
};
use serde_json::{Value, json};
use std::{
    env,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

/// 插件管理器、受管 Worker 宿主和后台 SDK 能力处理器的 daemon 侧持有者。
#[derive(Clone)]
pub(super) struct PluginServices {
    manager: Arc<Mutex<PluginManager>>,
    host: PluginHost,
    handler: Arc<PluginCapabilityHandler>,
}

impl PluginServices {
    pub(super) fn open(
        state_root: &Path,
        event_bus: Arc<InMemoryEventBus>,
        processes: ManagedProcessGroup,
    ) -> Result<Self, DaemonError> {
        let manager = PluginManager::open(state_root.join("plugins"))
            .map_err(|error| DaemonError::internal(format!("加载插件状态失败: {error}")))?;
        let manager = Arc::new(Mutex::new(manager));
        Ok(Self {
            host: PluginHost::new(plugin_worker_path(), processes),
            handler: Arc::new(PluginCapabilityHandler {
                manager: Arc::clone(&manager),
                event_bus,
            }),
            manager,
        })
    }

    pub(super) fn manager(&self) -> Arc<Mutex<PluginManager>> {
        Arc::clone(&self.manager)
    }

    /// 模型可见的插件工具目录：按调用工作区的作用域读取激活注册表。
    pub(super) fn tool_catalog_provider(&self) -> PluginToolCatalogProvider {
        let manager = Arc::clone(&self.manager);
        Arc::new(move |_workspace_id| {
            let scope = APPLICATION_SCOPE;
            let Ok(manager) = manager.lock() else {
                return Vec::new();
            };
            manager
                .tool_contributions_for_scope(scope)
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

    pub(super) fn tool_executor(&self) -> PluginToolExecutor {
        let services = self.clone();
        Arc::new(move |model_tool_name, arguments, context| {
            services.execute_tool(model_tool_name, arguments, context)
        })
    }

    /// 执行一次模型发起的插件工具调用。目录、授权和参数校验都在
    /// `PluginManager::prepare_tool_call` 中完成，通过后才会启动 Worker。
    fn execute_tool(
        &self,
        model_tool_name: &str,
        arguments: &str,
        context: &ToolExecutionContext,
    ) -> (String, magi_core::ExecutionResultStatus) {
        use magi_core::ExecutionResultStatus::{Failed, Rejected, Succeeded};
        let failure = |status, code: &str, message: &str| {
            (
                json!({"tool": model_tool_name, "status": status_label(status), "error_code": code, "error": message})
                    .to_string(),
                status,
            )
        };
        let workspace_id = context.workspace_id.as_ref().map(|id| id.as_str());
        let scope = APPLICATION_SCOPE;
        let prepared = match self
            .lock_manager()
            .and_then(|manager| manager.prepare_tool_call(scope, model_tool_name, arguments))
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
        let cancellation = RunCancellation::default();
        let lease = match PluginRuntimeLease::acquire(
            Arc::clone(&self.manager),
            &prepared.plugin_id,
            prepared.package.digest(),
            scope,
            &cancellation,
        ) {
            Ok(lease) => lease,
            Err(error) => return failure(Rejected, "plugin_lease_rejected", &error.to_string()),
        };
        let invocation = Invocation {
            identity: InvocationIdentity {
                plugin_id: prepared.plugin_id,
                package_digest: prepared.package.digest().to_string(),
                instance_id: scope.to_owned(),
                invocation_id: magi_plugin_system::next_invocation_id("tool"),
                workspace_id: workspace_id.map(str::to_string),
                session_id: context.session_id.as_ref().map(ToString::to_string),
                turn_id: None,
                run_id: context.task_id.as_ref().map(ToString::to_string),
                attempt_id: None,
                contribution: Some(InvocationContribution {
                    kind: "tool".into(),
                    id: prepared.tool.id.clone(),
                }),
            },
            source: prepared.package.source().to_string(),
            input: prepared.input,
            limits: RuntimeLimits::default(),
        };
        match magi_plugin_system::run_invocation_blocking(
            self.host.clone(),
            self.handler.clone(),
            invocation,
            cancellation,
            lease,
        ) {
            Ok(value) => (value.to_string(), Succeeded),
            Err(error) => failure(
                Failed,
                &format!("plugin_{:?}", error.code).to_lowercase(),
                &error.message,
            ),
        }
    }

    fn lock_manager(&self) -> Result<std::sync::MutexGuard<'_, PluginManager>, PluginError> {
        self.manager
            .lock()
            .map_err(|_| PluginError::Conflict("插件管理器不可用".into()))
    }

    /// 升级时的数据转换执行者：与工具、命令共用同一个受管 Worker 宿主，但不开放任何 SDK 能力。
    pub(super) fn data_migrator(&self) -> Arc<dyn magi_plugin_system::PluginDataMigrator> {
        Arc::new(magi_plugin_system::WorkerDataMigrator::new(
            self.host.clone(),
        ))
    }

    pub(super) fn command_executor(&self) -> Arc<dyn PluginCommandExecutor> {
        Arc::new(PluginCommandRunner::new(
            self.manager(),
            self.host.clone(),
            self.handler.clone(),
            RuntimeLimits::default(),
        ))
    }

    pub(super) fn workflow_core_factory(
        &self,
    ) -> Result<Arc<dyn WorkflowCoreFactory>, DaemonError> {
        let router = WorkflowCoreRouter::new(vec![Arc::new(PluginWorkflowCoreFactory::new(
            self.manager(),
            self.host.clone(),
            self.handler.clone(),
            RuntimeLimits::default(),
        ))])
        .map_err(|error| DaemonError::internal(format!("注册工作流核心失败: {error}")))?;
        Ok(Arc::new(router))
    }

    /// 会话引擎的唯一路由：通用插件工厂加上需要受保护宿主资源的原生引擎。
    /// 原生引擎占用的命名空间会从通用工厂中排除，一个引擎身份只有一条执行路径。
    pub(super) fn session_engine_factory(
        &self,
        native: Option<(String, Arc<dyn SessionEngineFactory>)>,
    ) -> Result<Arc<dyn SessionEngineFactory>, DaemonError> {
        let native = native
            .map(|entry| NativeSessionEngineFactory::new(vec![entry]).map(Arc::new))
            .transpose()
            .map_err(|error| DaemonError::internal(format!("注册原生插件引擎失败: {error}")))?;
        let excluded = native
            .as_ref()
            .map(|factory| factory.namespaces().map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut factories: Vec<Arc<dyn SessionEngineFactory>> = vec![Arc::new(
            PluginSessionEngineFactory::new(
                self.manager(),
                self.host.clone(),
                self.handler.clone(),
                RuntimeLimits::default(),
            )
            .with_excluded_namespaces(excluded),
        )];
        if let Some(native) = native {
            factories.push(native);
        }
        let router = SessionEngineRouter::new(factories)
            .map_err(|error| DaemonError::internal(format!("注册会话引擎失败: {error}")))?;
        Ok(Arc::new(router))
    }
}

fn status_label(status: magi_core::ExecutionResultStatus) -> &'static str {
    match status {
        magi_core::ExecutionResultStatus::Rejected => "rejected",
        _ => "failed",
    }
}

/// Worker 随产品一起发行，位于 daemon 可执行文件旁；开发时可用环境变量覆盖。
fn plugin_worker_path() -> PathBuf {
    let worker_name = if cfg!(windows) {
        "magi-plugin-worker.exe"
    } else {
        "magi-plugin-worker"
    };
    env::var_os("MAGI_PLUGIN_WORKER_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join(worker_name)))
        })
        .unwrap_or_else(|| PathBuf::from(worker_name))
}

/// 插件后台 SDK 的能力入口。每个请求都先核对固定的调用身份（包摘要、作用域实例、
/// 当前激活与授权），再进入与 UI 相同的资源/设置服务，不保存另一份状态。
pub(super) struct PluginCapabilityHandler {
    manager: Arc<Mutex<PluginManager>>,
    event_bus: Arc<InMemoryEventBus>,
}

impl PluginCapabilityHandler {
    fn handle(&self, request: &CapabilityRequest) -> Result<Value, ExecutionError> {
        let scope = APPLICATION_SCOPE;
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| rejected("插件管理器不可用"))?;
        let plugin_id = request.identity.plugin_id.as_str();
        manager
            .validate_runtime_identity(
                plugin_id,
                &request.identity.package_digest,
                &request.identity.instance_id,
                scope,
            )
            .map_err(|_| rejected("插件执行身份已失效"))?;
        let arguments = &request.arguments;
        match request.operation.as_str() {
            "resource.read" => manager
                .read_resource(plugin_id, scope, required_str(arguments, "resourceId")?)
                .map(|resource| json!(resource))
                .map_err(capability_error),
            "resource.write" => magi_api::write_plugin_resource_with_event(
                &mut manager,
                &self.event_bus,
                plugin_id,
                scope,
                required_str(arguments, "resourceId")?,
                required_version(arguments)?,
                required_value(arguments)?,
            )
            .map(|resource| json!(resource))
            .map_err(capability_error),
            "settings.read" => manager
                .read_settings(plugin_id, scope)
                .map(|settings| json!(settings))
                .map_err(capability_error),
            "settings.write" => magi_api::write_plugin_settings_with_event(
                &mut manager,
                &self.event_bus,
                plugin_id,
                scope,
                required_version(arguments)?,
                required_value(arguments)?,
            )
            .map(|settings| json!(settings))
            .map_err(capability_error),
            _ => Err(rejected("插件能力未注册")),
        }
    }
}

impl CapabilityHandler for PluginCapabilityHandler {
    fn call(
        &self,
        request: CapabilityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ExecutionError>> + Send + '_>> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::Cancelled,
                    "插件能力调用已取消",
                ));
            }
            self.handle(&request)
        })
    }
}

fn rejected(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::CapabilityRejected, message)
}

/// 领域错误的文案由宿主生成，可以回给插件（例如版本冲突让插件重读后重试）；
/// 存储与状态损坏细节只留在宿主侧。
fn capability_error(error: PluginError) -> ExecutionError {
    match error {
        PluginError::Storage(_) | PluginError::CorruptState(_) => rejected("插件存储暂时不可用"),
        other => rejected(&other.to_string()),
    }
}

fn required_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, ExecutionError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| rejected(&format!("缺少参数 {key}")))
}

fn required_version(arguments: &Value) -> Result<u64, ExecutionError> {
    arguments
        .get("expectedVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| rejected("expectedVersion 必须是非负整数"))
}

fn required_value(arguments: &Value) -> Result<Value, ExecutionError> {
    arguments
        .get("value")
        .cloned()
        .ok_or_else(|| rejected("缺少参数 value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_arguments_must_be_explicit_so_a_missing_value_cannot_clobber_a_resource() {
        let arguments = json!({"resourceId": "board", "expectedVersion": 2});
        assert_eq!(required_str(&arguments, "resourceId").unwrap(), "board");
        assert_eq!(required_version(&arguments).unwrap(), 2);
        assert!(required_value(&arguments).is_err());
        assert!(required_version(&json!({"expectedVersion": -1})).is_err());
        assert!(required_str(&json!({"resourceId": ""}), "resourceId").is_err());
    }

    #[test]
    fn storage_failures_are_not_leaked_to_plugins_but_conflicts_are() {
        let storage = capability_error(PluginError::CorruptState("/secret/path".into()));
        assert!(!storage.message.contains("secret"));
        let conflict = capability_error(PluginError::Resource(
            magi_plugin_system::ResourceError::Conflict {
                expected: 1,
                actual: 3,
            },
        ));
        assert!(conflict.message.contains("actual=3"));
    }
}
