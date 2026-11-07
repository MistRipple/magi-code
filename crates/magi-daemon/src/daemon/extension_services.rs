//! 受信扩展反向调用 daemon 的服务入口：身份来自宿主连接，数据沿现有唯一写入口提交。
use magi_event_bus::InMemoryEventBus;
use magi_extension_host::{
    HostService,
    peer::BoxFuture,
    protocol::{ErrorKind, RpcError},
};
use magi_plugin_system::PluginManager;

use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) type NativeService =
    Arc<dyn Fn(String, String, Value, Arc<AtomicBool>) -> Result<Value, RpcError> + Send + Sync>;

pub(super) struct PluginHostService {
    pub manager: Arc<Mutex<PluginManager>>,
    pub events: Arc<OnceLock<Arc<InMemoryEventBus>>>,
    pub digest: String,
    pub native: Arc<OnceLock<NativeService>>,
    pub operations: Arc<OnceLock<magi_api::PluginOperations>>,
    pub blobs: Arc<Mutex<HashMap<String, Blob>>>,
    /// 主对话模型目录所在的设置事实源；插件贡献的数据源写入其运行期 section。
    pub settings: Arc<OnceLock<Arc<magi_settings_store::SettingsStore>>>,
}

pub(super) struct Blob {
    owner: String,
    expires: std::time::Instant,
    content_type: String,
    value: Value,
    bytes: usize,
}

impl PluginHostService {
    fn catalog_changed(&self) {
        if let Some(events) = self.events.get() {
            events.publish(magi_event_bus::EventEnvelope::domain(
                magi_core::EventId::unique("plugin-runtime-catalog"),
                "plugin.registry.changed",
                json!({}),
            ));
        }
    }

    fn service_admitted(&self, plugin: &str, method: &str, params: &Value) -> Result<(), RpcError> {
        self.admitted(plugin)?;
        if let Some(lease) = params.get("_instance").and_then(Value::as_str) {
            self.manager
                .lock()
                .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?
                .check_runtime(plugin, lease)
                .map_err(|error| RpcError::new(ErrorKind::StaleGeneration, error.to_string()))?;
        }
        if matches!(method, "modelSource.set" | "modelSource.clear") {
            // 主对话内容会发往插件提供的端点，所以数据源必须逐项授权。
            let target = params
                .get("sourceId")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "sourceId 必须是字符串"))?;
            let manager = self
                .manager
                .lock()
                .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?;
            if !manager
                .permission_allowed(
                    plugin,
                    magi_plugin_system::PluginPermissionKind::ModelSources,
                    target,
                )
                .map_err(|error| RpcError::new(ErrorKind::PermissionDenied, error.to_string()))?
            {
                return Err(RpcError::new(
                    ErrorKind::PermissionDenied,
                    format!("缺少 modelSources:[{target}] 授权"),
                ));
            }
            return Ok(());
        }
        if !matches!(method, "model.invoke" | "credential.ref") {
            return Ok(());
        }
        // models 授权只描述 Magi 管理的模型连接，不是 Node 的网络隔离。
        // 默认连接使用固定目标 default，角色连接使用原生 roleId；不接受通配提权。
        let reference_role = params
            .get("credentialRef")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix("model-role:"));
        let target = params
            .get("roleId")
            .and_then(Value::as_str)
            .or(reference_role)
            .unwrap_or("default");
        let manager = self
            .manager
            .lock()
            .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?;
        if !manager
            .permission_allowed(
                plugin,
                magi_plugin_system::PluginPermissionKind::Models,
                target,
            )
            .map_err(|error| RpcError::new(ErrorKind::PermissionDenied, error.to_string()))?
        {
            return Err(RpcError::new(
                ErrorKind::PermissionDenied,
                format!("缺少 models:[{target}] 授权"),
            ));
        }
        Ok(())
    }

    fn admitted(&self, plugin: &str) -> Result<(), RpcError> {
        let manager = self
            .manager
            .lock()
            .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?;
        let package = manager
            .package(plugin)
            .map_err(|e| RpcError::new(ErrorKind::PermissionDenied, e.to_string()))?;
        if package.digest() != self.digest {
            return Err(RpcError::new(ErrorKind::StaleGeneration, "插件包已替换"));
        }
        manager
            .ensure_editing_admitted(plugin)
            .map_err(|e| RpcError::new(ErrorKind::Conflict, e.to_string()))?;
        if !manager
            .is_active(plugin)
            .map_err(|e| RpcError::new(ErrorKind::PermissionDenied, e.to_string()))?
        {
            return Err(RpcError::new(
                ErrorKind::PermissionDenied,
                "插件未激活或已撤权",
            ));
        }
        Ok(())
    }
}

impl HostService for PluginHostService {
    fn instance_started(&self, plugin_id: &str, lease: &str) -> Result<(), RpcError> {
        self.manager
            .lock()
            .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?
            .begin_runtime(plugin_id, &self.digest, lease)
            .map_err(|error| RpcError::new(ErrorKind::PermissionDenied, error.to_string()))
    }
    fn instance_ready(&self, plugin_id: &str, lease: &str) -> Result<(), RpcError> {
        self.manager
            .lock()
            .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?
            .ready_runtime(plugin_id, lease)
            .map_err(|error| RpcError::new(ErrorKind::StaleGeneration, error.to_string()))?;
        self.catalog_changed();
        Ok(())
    }
    fn instance_ended(&self, plugin_id: &str, lease: &str) {
        let removed = self.manager.lock().is_ok_and(|mut manager| {
            if !manager.end_runtime(plugin_id, lease) {
                return false;
            }
            // 与实例准入、目录写入持同一把锁，避免删除旧租约后误清理刚启动的新来源。
            if let Some(settings) = self.settings.get() {
                magi_conversation_runtime::main_model_sources::clear_plugin_sources(
                    settings, plugin_id,
                );
            }
            true
        });
        if removed {
            self.catalog_changed();
        }
    }

    fn request_scoped(
        &self,
        plugin_id: String,
        method: String,
        params: Value,
        cancelled: Arc<AtomicBool>,
    ) -> BoxFuture<Result<Value, RpcError>> {
        if let Err(error) = self.service_admitted(&plugin_id, &method, &params) {
            return Box::pin(async { Err(error) });
        }
        if cancelled.load(Ordering::Acquire) {
            return Box::pin(async { Err(RpcError::new(ErrorKind::Cancelled, "调用已取消")) });
        }
        if matches!(
            method.as_str(),
            "plugins.preview"
                | "plugins.install"
                | "plugins.setEnabled"
                | "plugins.remove"
                | "plugins.authorize"
                | "plugins.compatibility"
                | "plugins.setVersionExemption"
        ) {
            let operation = self.request_with_identity(plugin_id, method, params);
            return Box::pin(async move {
                tokio::select! {
                    result = operation => result,
                    _ = async {
                        while !cancelled.load(Ordering::Acquire) {
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                        }
                    } => Err(RpcError::new(ErrorKind::Cancelled, "插件管理调用已取消")),
                }
            });
        }
        if !matches!(
            method.as_str(),
            "model.invoke" | "task.spawn" | "ask.user" | "notify.user" | "tool.call"
        ) {
            return self.request_with_identity(plugin_id, method, params);
        }
        let params = native_params(params);
        let native = self.native.get().cloned();
        Box::pin(async move {
            let native =
                native.ok_or_else(|| RpcError::new(ErrorKind::HostGone, "原生业务服务未就绪"))?;
            tokio::task::spawn_blocking(move || native(plugin_id, method, params, cancelled))
                .await
                .map_err(|e| RpcError::new(ErrorKind::Internal, e.to_string()))?
        })
    }

    fn configuration(
        &self,
        plugin_id: String,
    ) -> BoxFuture<Result<Option<serde_json::Value>, RpcError>> {
        let manager = Arc::clone(&self.manager);
        let digest = self.digest.clone();
        Box::pin(async move {
            let manager = manager
                .lock()
                .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?;
            if manager
                .package(&plugin_id)
                .map_err(|error| RpcError::new(ErrorKind::Internal, error.to_string()))?
                .digest()
                != digest
            {
                return Err(RpcError::new(ErrorKind::StaleGeneration, "插件包已替换"));
            }
            if !manager
                .permission_allowed(
                    &plugin_id,
                    magi_plugin_system::PluginPermissionKind::Storage,
                    "__settings",
                )
                .map_err(|error| RpcError::new(ErrorKind::PermissionDenied, error.to_string()))?
            {
                return Ok(None);
            }
            manager
                .read_settings(&plugin_id)
                .map(|resource| {
                    Some(serde_json::json!({"version":resource.version,"value":resource.value}))
                })
                .map_err(|error| RpcError::new(ErrorKind::PermissionDenied, error.to_string()))
        })
    }
    fn request_with_identity(
        &self,
        plugin_id: String,
        method: String,
        mut params: serde_json::Value,
    ) -> BoxFuture<Result<serde_json::Value, RpcError>> {
        let manager = Arc::clone(&self.manager);
        let events = Arc::clone(&self.events);
        let digest = self.digest.clone();
        let blobs = Arc::clone(&self.blobs);
        let settings = self.settings.get().cloned();
        let operations = self.operations.get().cloned();
        if let Err(error) = self.service_admitted(&plugin_id, &method, &params) {
            return Box::pin(async { Err(error) });
        }
        if matches!(
            method.as_str(),
            "plugins.preview"
                | "plugins.install"
                | "plugins.setEnabled"
                | "plugins.remove"
                | "plugins.authorize"
                | "plugins.compatibility"
                | "plugins.setVersionExemption"
        ) {
            let operations = self.operations.get().cloned();
            let digest = self.digest.clone();
            let lease = params
                .get("_instance")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(object) = params.as_object_mut() {
                object.remove("_instance");
                object.remove("_context");
            }
            return Box::pin(async move {
                let operations = operations
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "插件管理服务未就绪"))?;
                operations
                    .call(&plugin_id, &digest, lease.as_deref(), &method, params)
                    .await
                    .map_err(|error| {
                        let kind = match &error {
                            magi_api::ApiError::Forbidden(_) => ErrorKind::PermissionDenied,
                            magi_api::ApiError::Conflict(_) => ErrorKind::Conflict,
                            magi_api::ApiError::InvalidInput(_) => ErrorKind::InvalidParams,
                            _ => ErrorKind::Blocked,
                        };
                        RpcError::new(kind, error.message())
                    })
            });
        }
        if matches!(method.as_str(), "session.snapshot" | "credential.ref") {
            if method == "session.snapshot" {
                let owner = params
                    .pointer("/_context/owner/sessionId")
                    .and_then(Value::as_str);
                let requested = params.get("sessionId").and_then(Value::as_str);
                if owner.is_none() || requested.is_some_and(|session| Some(session) != owner) {
                    return Box::pin(async {
                        Err(RpcError::new(
                            ErrorKind::PermissionDenied,
                            "快照只能读取当前调用归属的会话",
                        ))
                    });
                }
            }
            let params = native_params(params);
            let native = self.native.get().cloned();
            return Box::pin(async move {
                let native = native
                    .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "原生业务服务未就绪"))?;
                native(plugin_id, method, params, Arc::default())
            });
        }
        let lease = params
            .get("_instance")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(object) = params.as_object_mut() {
            object.remove("_instance");
            object.remove("_context");
        }
        Box::pin(async move {
            let map_error = |error: magi_plugin_system::PluginError| {
                let kind = match error {
                    magi_plugin_system::PluginError::NotAuthorized(_) => {
                        ErrorKind::PermissionDenied
                    }
                    magi_plugin_system::PluginError::Conflict(_)
                    | magi_plugin_system::PluginError::Resource(
                        magi_plugin_system::ResourceError::Conflict { .. },
                    ) => ErrorKind::Conflict,
                    magi_plugin_system::PluginError::InvalidInput(_)
                    | magi_plugin_system::PluginError::InvalidPackage(_) => {
                        ErrorKind::InvalidParams
                    }
                    magi_plugin_system::PluginError::NotFound(_) => ErrorKind::MethodNotFound,
                    _ => ErrorKind::Internal,
                };
                RpcError::new(kind, error.to_string())
            };
            let mut manager_guard = manager
                .lock()
                .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器不可用"))?;
            if manager_guard
                .package(&plugin_id)
                .map_err(map_error)?
                .digest()
                != digest
            {
                return Err(RpcError::new(ErrorKind::StaleGeneration, "插件包已替换"));
            }
            manager_guard
                .ensure_editing_admitted(&plugin_id)
                .map_err(map_error)?;
            if let Some(lease) = &lease {
                manager_guard
                    .check_runtime(&plugin_id, lease)
                    .map_err(map_error)?;
            }
            let result = match method.as_str() {
                "plugins.list" => {
                    if params.as_object().is_none_or(|object| !object.is_empty()) {
                        return Err(RpcError::new(
                            ErrorKind::InvalidParams,
                            "plugins.list 不接受参数",
                        ));
                    }
                    // 复用既有目录 DTO，配置和资源不在此投影中；每次读取当前 manager，
                    // 不在 Node 端建立安装状态缓存或另一份插件清单。
                    let mut list = manager_guard.projection().map_err(map_error)?;
                    if let Some(operations) = &operations {
                        operations.decorate_runtime(&mut list);
                    }
                    return serde_json::to_value(list)
                        .map_err(|error| RpcError::new(ErrorKind::Internal, error.to_string()));
                }
                "catalog.replace" => {
                    #[derive(serde::Deserialize)]
                    #[serde(rename_all = "camelCase", deny_unknown_fields)]
                    struct Replace {
                        expected_revision: u64,
                        catalog: magi_app_server_protocol::generated::PluginRuntimeCatalog,
                    }
                    let input: Replace = serde_json::from_value(params).map_err(|error| {
                        RpcError::new(ErrorKind::InvalidParams, error.to_string())
                    })?;
                    let lease = lease.as_deref().ok_or_else(|| {
                        RpcError::new(ErrorKind::StaleGeneration, "目录写入需要实例租约")
                    })?;
                    let previous = manager_guard
                        .runtime_manifest(&plugin_id, true)
                        .map_err(map_error)?
                        .contributions
                        .model_sources;
                    let revision = manager_guard
                        .replace_runtime_catalog(
                            &plugin_id,
                            lease,
                            input.expected_revision,
                            input.catalog,
                        )
                        .map_err(map_error)?;
                    let current = manager_guard
                        .runtime_manifest(&plugin_id, true)
                        .map_err(map_error)?
                        .contributions
                        .model_sources;
                    if let Some(settings) = &settings {
                        for source in previous {
                            if !current.iter().any(|item| item.id == source.id) {
                                magi_conversation_runtime::main_model_sources::clear_plugin_source(
                                    settings, &plugin_id, &source.id,
                                );
                            }
                        }
                    }
                    if let Some(events) = events.get() {
                        events.publish(magi_event_bus::EventEnvelope::domain(
                            magi_core::EventId::unique("plugin-runtime-catalog"),
                            "plugin.registry.changed",
                            json!({}),
                        ));
                    }
                    return Ok(json!({"revision": revision}));
                }
                "registration.check" => {
                    let lease = lease.as_deref().ok_or_else(|| {
                        RpcError::new(ErrorKind::StaleGeneration, "绑定需要实例租约")
                    })?;
                    let kind = params
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !manager_guard
                        .runtime_registration_allowed(&plugin_id, lease, kind, name)
                        .map_err(map_error)?
                    {
                        return Err(RpcError::new(
                            ErrorKind::RegistrationDenied,
                            "贡献未在当前运行目录登记或未授权",
                        ));
                    }
                    return Ok(json!({}));
                }
                "settings.describe" => {
                    return Ok(
                        json!({"schema": manager_guard.runtime_manifest(&plugin_id, true).map_err(map_error)?.settings_schema}),
                    );
                }
                "blob.put" => {
                    #[derive(serde::Deserialize)]
                    #[serde(rename_all = "camelCase", deny_unknown_fields)]
                    struct Put {
                        value: Value,
                        content_type: Option<String>,
                    }
                    let input: Put = serde_json::from_value(params)
                        .map_err(|e| RpcError::new(ErrorKind::InvalidParams, e.to_string()))?;
                    let bytes = serde_json::to_vec(&input.value)
                        .map_err(|e| RpcError::new(ErrorKind::InvalidParams, e.to_string()))?
                        .len();
                    if bytes > 2 * 1024 * 1024 {
                        return Err(RpcError::new(ErrorKind::Backpressure, "blob 最大 2 MiB"));
                    }
                    let mut blobs = blobs
                        .lock()
                        .map_err(|_| RpcError::new(ErrorKind::Internal, "blob store 不可用"))?;
                    blobs.retain(|_, blob| blob.expires > std::time::Instant::now());
                    if blobs.len() >= 64
                        || blobs.values().map(|b| b.bytes).sum::<usize>() + bytes > 16 * 1024 * 1024
                    {
                        return Err(RpcError::new(ErrorKind::Backpressure, "blob 配额已满"));
                    }
                    let id = magi_core::EventId::unique("blob").to_string();
                    blobs.insert(
                        id.clone(),
                        Blob {
                            owner: plugin_id.clone(),
                            expires: std::time::Instant::now()
                                + std::time::Duration::from_secs(300),
                            content_type: input
                                .content_type
                                .unwrap_or_else(|| "application/json".into()),
                            value: input.value,
                            bytes,
                        },
                    );
                    Ok(json!({"ref": id, "bytes": bytes, "expiresInMs": 300000}))
                }
                "blob.get" => {
                    #[derive(serde::Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Get {
                        r#ref: String,
                    }
                    let input: Get = serde_json::from_value(params)
                        .map_err(|e| RpcError::new(ErrorKind::InvalidParams, e.to_string()))?;
                    let mut blobs = blobs
                        .lock()
                        .map_err(|_| RpcError::new(ErrorKind::Internal, "blob store 不可用"))?;
                    blobs.retain(|_, blob| blob.expires > std::time::Instant::now());
                    let blob = blobs.get(&input.r#ref).ok_or_else(|| {
                        RpcError::new(ErrorKind::BlobExpired, "blob 不存在或已过期")
                    })?;
                    if blob.owner != plugin_id {
                        return Err(RpcError::new(
                            ErrorKind::PermissionDenied,
                            "blob 不属于当前插件",
                        ));
                    }
                    Ok(json!({"value": blob.value, "contentType": blob.content_type}))
                }
                "resource.read" => {
                    let id = params
                        .get("resourceId")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            RpcError::new(ErrorKind::InvalidParams, "resourceId 必须是字符串")
                        })?;
                    manager_guard
                        .read_resource(&plugin_id, id)
                        .map(|r| serde_json::json!({"version": r.version, "value": r.value}))
                }
                "resource.write" => {
                    let id = params
                        .get("resourceId")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            RpcError::new(ErrorKind::InvalidParams, "resourceId 必须是字符串")
                        })?;
                    let version = params
                        .get("expectedVersion")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            RpcError::new(ErrorKind::InvalidParams, "expectedVersion 必须是整数")
                        })?;
                    let value = params
                        .get("value")
                        .cloned()
                        .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "value 缺失"))?;
                    let events = events
                        .get()
                        .ok_or_else(|| RpcError::new(ErrorKind::Internal, "事件服务未装配"))?;
                    magi_api::write_plugin_resource_with_event(
                        &mut manager_guard,
                        events,
                        &plugin_id,
                        id,
                        version,
                        value,
                    )
                    .map(|r| serde_json::json!({"version": r.version, "value": r.value}))
                }
                "modelSource.set" | "modelSource.clear" => {
                    let settings = settings
                        .ok_or_else(|| RpcError::new(ErrorKind::Internal, "设置服务未装配"))?;
                    let mut input = params;
                    let source_id = input
                        .as_object_mut()
                        .and_then(|object| object.remove("sourceId"))
                        .and_then(|value| value.as_str().map(str::to_string))
                        .ok_or_else(|| {
                            RpcError::new(ErrorKind::InvalidParams, "sourceId 必须是字符串")
                        })?;
                    let title = manager_guard
                        .runtime_manifest(&plugin_id, true)
                        .map_err(map_error)?
                        .contributions
                        .model_sources
                        .iter()
                        .find(|source| source.id == source_id)
                        .map(|source| source.title.clone())
                        .ok_or_else(|| {
                            RpcError::new(
                                ErrorKind::InvalidParams,
                                format!("模型数据源 {source_id} 未在清单中声明"),
                            )
                        })?;
                    if method == "modelSource.clear" {
                        magi_conversation_runtime::main_model_sources::clear_plugin_source(
                            &settings, &plugin_id, &source_id,
                        );
                    } else {
                        magi_conversation_runtime::main_model_sources::set_plugin_source(
                            &settings, &plugin_id, &source_id, &title, &input,
                        )
                        .map_err(|message| RpcError::new(ErrorKind::InvalidParams, message))?;
                    }
                    return Ok(json!({}));
                }
                "settings.read" => manager_guard
                    .read_settings(&plugin_id)
                    .map(|r| serde_json::json!({"version": r.version, "value": r.value})),
                "settings.write" => {
                    let version = params
                        .get("expectedVersion")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            RpcError::new(ErrorKind::InvalidParams, "expectedVersion 必须是整数")
                        })?;
                    let value = params
                        .get("value")
                        .cloned()
                        .ok_or_else(|| RpcError::new(ErrorKind::InvalidParams, "value 缺失"))?;
                    let events = events
                        .get()
                        .ok_or_else(|| RpcError::new(ErrorKind::Internal, "事件服务未装配"))?;
                    magi_api::write_plugin_settings_with_event(
                        &mut manager_guard,
                        events,
                        &plugin_id,
                        version,
                        value,
                    )
                    .map(|r| serde_json::json!({"version": r.version, "value": r.value}))
                }
                _ => {
                    return Err(RpcError::new(
                        ErrorKind::MethodNotFound,
                        format!("未知宿主服务 {method}"),
                    ));
                }
            };
            result.map_err(map_error)
        })
    }
}

// 实例租约由本层校验；原生业务 DTO 只接收业务字段和独立的调用归属。
fn native_params(mut params: Value) -> Value {
    if let Some(object) = params.as_object_mut() {
        object.remove("_instance");
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_plugin_system::{PluginPackage, PluginSource};
    use serde_json::json;
    use std::io::Write;

    fn package() -> PluginPackage {
        let manifest = json!({
            "sdkVersion": 2, "id": "acme.services", "version": "1.0.0", "name": "Services", "description": "",
            "extension": {"hooks": []}, "dataSchemaVersion": 1,
            "permissions": [
                {"kind": "extension", "targets": ["full-trust"]},
                {"kind": "storage", "targets": ["notes", "__settings"]},
                {"kind": "modelSources", "targets": ["proxy"]}
            ],
            "settingsSchema": {"type": "object", "properties": {"enabled": {"type": "boolean"}}, "additionalProperties": false},
            "contributions": {"resources": [{"id": "notes", "title": "Notes", "description": ""}], "commands": [{"id": "status", "title": "Status", "description": ""}], "modelSources": [{"id": "proxy", "title": "Acme Proxy", "description": ""}]}
        });
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(serde_json::to_string(&manifest).unwrap().as_bytes())
            .unwrap();
        zip.start_file(
            "extension/index.mjs",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            br#"export default async function activate(api) {
            await api.command('status', async () => 'ready');
            const snapshot = await api.resource.read('notes');
            await api.resource.write('notes', snapshot.version, {text: 'from node'});
            const settings = await api.settings.read();
            await api.settings.write(settings.version, {enabled: true});
            await api.modelSource.set('proxy', {
              baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'plugin-secret', apiProtocol: 'openai_chat',
              models: [{id: 'proxy-model', displayName: 'Proxy Model'}],
            });
        }"#,
        )
        .unwrap();
        PluginPackage::from_archive(&zip.finish().unwrap().into_inner()).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, Arc<PluginHostService>) {
        let dir = tempfile::tempdir().unwrap();
        let package = package();
        let mut manager = PluginManager::open(dir.path().join("plugins")).unwrap();
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "test.zip".into(),
                },
            )
            .unwrap();
        manager.enable("acme.services").unwrap();
        manager
            .authorize("acme.services", package.manifest().permissions.clone())
            .unwrap();
        manager.activate("acme.services").unwrap();
        let events = Arc::new(std::sync::OnceLock::new());
        events.set(Arc::new(InMemoryEventBus::new(32))).unwrap();
        let service = Arc::new(PluginHostService {
            manager: Arc::new(Mutex::new(manager)),
            events,
            digest: package.digest().into(),
            native: Arc::default(),
            operations: Arc::default(),
            blobs: Arc::default(),
            settings: Arc::new(std::sync::OnceLock::from(Arc::new(
                magi_settings_store::SettingsStore::new(),
            ))),
        });
        (dir, service)
    }

    async fn call(
        service: &PluginHostService,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        service
            .request_with_identity("acme.services".into(), method.into(), params)
            .await
    }

    #[tokio::test]
    async fn plugin_directory_reads_current_manager_and_rejects_disabled_instances() {
        let (_dir, service) = fixture();
        let listed = call(&service, "plugins.list", json!({})).await.unwrap();
        assert_eq!(listed["plugins"][0]["manifest"]["id"], "acme.services");
        assert_eq!(listed["plugins"][0]["active"], true);
        assert!(listed["plugins"][0].get("settings").is_none());
        assert!(
            call(&service, "plugins.list", json!({"unknown": true}))
                .await
                .is_err()
        );
        service
            .manager
            .lock()
            .unwrap()
            .deactivate("acme.services")
            .unwrap();
        assert!(call(&service, "plugins.list", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn services_share_versions_authorization_and_event_publication_with_the_manager() {
        let (_dir, service) = fixture();
        let mut feed = service.events.get().unwrap().subscribe();
        assert_eq!(
            call(&service, "resource.read", json!({"resourceId": "notes"}))
                .await
                .unwrap()["version"],
            0
        );
        assert_eq!(
            call(
                &service,
                "resource.write",
                json!({"resourceId": "notes", "expectedVersion": 0, "value": {"text": "one"}})
            )
            .await
            .unwrap()["version"],
            1
        );
        let event = feed.recv().await.unwrap();
        assert_eq!(event.event_type, "plugin.resource.updated");
        assert_eq!(event.payload["value"], json!({"text": "one"}));
        assert_eq!(
            service
                .manager
                .lock()
                .unwrap()
                .read_resource("acme.services", "notes")
                .unwrap()
                .version,
            1
        );
        assert_eq!(
            call(
                &service,
                "resource.write",
                json!({"resourceId": "notes", "expectedVersion": 0, "value": null})
            )
            .await
            .unwrap_err()
            .kind(),
            ErrorKind::Conflict
        );
        assert_eq!(
            call(
                &service,
                "settings.write",
                json!({"expectedVersion": 0, "value": {"enabled": "wrong"}})
            )
            .await
            .unwrap_err()
            .kind(),
            ErrorKind::InvalidParams
        );
        assert_eq!(
            call(&service, "resource.read", json!({"resourceId": "unowned"}))
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::MethodNotFound
        );
        let drain =
            PluginManager::begin_deactivation(Arc::clone(&service.manager), "acme.services")
                .unwrap();
        assert_eq!(
            call(
                &service,
                "resource.write",
                json!({"resourceId": "notes", "expectedVersion": 1, "value": null})
            )
            .await
            .unwrap_err()
            .kind(),
            ErrorKind::Conflict
        );
        drop(drain);
        service
            .manager
            .lock()
            .unwrap()
            .authorize("acme.services", vec![])
            .unwrap();
        assert_eq!(
            call(&service, "resource.read", json!({"resourceId": "notes"}))
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn snapshots_require_the_signed_session_owner() {
        let (_dir, service) = fixture();
        let denied = service
            .request_scoped(
                "acme.services".into(),
                "session.snapshot".into(),
                json!({
                    "_context": {"owner": {"sessionId": "signed-session"}},
                    "sessionId": "other-session"
                }),
                Arc::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::PermissionDenied);
        let missing_owner = service
            .request_scoped(
                "acme.services".into(),
                "session.snapshot".into(),
                json!({"sessionId": "signed-session"}),
                Arc::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(missing_owner.kind(), ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn native_services_receive_owner_context_without_transport_instance_metadata() {
        let (_dir, service) = fixture();
        service.instance_started("acme.services", "active").unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received = Arc::clone(&calls);
        assert!(
            service
                .native
                .set(Arc::new(move |_, _, params, _| {
                    assert!(params.get("_instance").is_none());
                    assert_eq!(
                        params.pointer("/_context/owner/sessionId"),
                        Some(&json!("signed"))
                    );
                    received.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({}))
                }))
                .is_ok()
        );
        let params = json!({"_instance": "active", "_context": {"owner": {"sessionId": "signed"}}});
        for method in [
            "session.snapshot",
            "notify.user",
            "ask.user",
            "task.spawn",
            "tool.call",
        ] {
            service
                .request_scoped(
                    "acme.services".into(),
                    method.into(),
                    params.clone(),
                    Arc::default(),
                )
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        service.instance_ended("acme.services", "active");
        assert!(
            service
                .request_scoped(
                    "acme.services".into(),
                    "notify.user".into(),
                    params,
                    Arc::default()
                )
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn model_services_require_an_explicit_models_grant() {
        let (_dir, service) = fixture();
        let denied = service
            .request_scoped(
                "acme.services".into(),
                "model.invoke".into(),
                json!({"prompt":"hello"}),
                Arc::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn blob_service_is_bounded_and_expires_entries() {
        let (_dir, service) = fixture();
        let stored = call(&service, "blob.put", json!({"value": {"ok": true}}))
            .await
            .unwrap();
        let fetched = call(
            &service,
            "blob.get",
            json!({"ref": stored["ref"].as_str().unwrap()}),
        )
        .await
        .unwrap();
        assert_eq!(fetched["value"], json!({"ok": true}));
        let foreign = service
            .request_with_identity(
                "other.plugin".into(),
                "blob.get".into(),
                json!({"ref": stored["ref"]}),
            )
            .await
            .unwrap_err();
        assert_eq!(foreign.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            call(
                &service,
                "blob.put",
                json!({"value": "x".repeat(2 * 1024 * 1024 + 1)})
            )
            .await
            .unwrap_err()
            .kind(),
            ErrorKind::Backpressure
        );
    }

    #[tokio::test]
    async fn real_node_activation_uses_the_same_resource_and_settings_store() {
        use magi_extension_host::instance::{LaunchConfig, launch};
        let (dir, service) = fixture();
        let package = package();
        let entry = dir.path().join("index.mjs");
        let runtime = dir.path().join("host.mjs");
        std::fs::write(&entry, package.extension_entry().unwrap()).unwrap();
        std::fs::write(&runtime, magi_extension_host::HOST_RUNTIME_SOURCE).unwrap();
        let node = std::env::var_os("MAGI_EXTENSION_NODE")
            .map(std::path::PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("Node required");
        let mut config = LaunchConfig::new(node, runtime, entry, dir.path().join("sock"));
        config.plugin_id = "acme.services".into();
        config.commands = vec!["status".into()];
        config.digest = package.digest().into();
        config.service = Some(service.clone());
        let group = magi_process::ManagedProcessGroup::new();
        let instance = launch(&group, config, 1).await.unwrap();
        let manager = service.manager.lock().unwrap();
        assert_eq!(
            manager
                .read_resource("acme.services", "notes")
                .unwrap()
                .value,
            json!({"text": "from node"})
        );
        assert_eq!(
            manager.read_settings("acme.services").unwrap().value,
            json!({"enabled": true})
        );
        drop(manager);
        // 真实 Node 激活期间提交的数据源进入主对话目录，并带上清单里的标题。
        let settings = service.settings.get().unwrap();
        let catalog = magi_conversation_runtime::main_model_sources::catalog(settings).unwrap();
        let group = &catalog["groups"][0];
        assert_eq!(group["sourceId"], "plugin/acme.services/proxy");
        assert_eq!(group["kind"], "plugin");
        assert_eq!(group["name"], "Acme Proxy");
        assert_eq!(group["models"][0]["displayName"], "Proxy Model");
        assert!(!catalog.to_string().contains("plugin-secret"));
        instance.stop().await;
        assert!(
            magi_conversation_runtime::main_model_sources::sources(settings)
                .unwrap()
                .is_empty()
        );
    }

    fn source_input() -> serde_json::Value {
        json!({
            "sourceId": "proxy", "baseUrl": "https://proxy.example/v1", "apiKey": "k",
            "apiProtocol": "openai_chat", "models": [{"id": "m1"}]
        })
    }

    #[tokio::test]
    async fn stale_instance_requests_and_cleanup_cannot_change_new_model_sources() {
        use magi_conversation_runtime::main_model_sources as sources;
        let (_dir, service) = fixture();
        let settings = service.settings.get().unwrap();
        service.instance_started("acme.services", "old").unwrap();
        let mut input = source_input();
        input["_instance"] = json!("old");
        // 准入后尚未执行的写入也必须在真正提交时重新核对租约。
        let delayed =
            service.request_with_identity("acme.services".into(), "modelSource.set".into(), input);
        service.instance_started("acme.services", "new").unwrap();
        let mut input = source_input();
        input["_instance"] = json!("new");
        input["models"] = json!([{"id": "new-model"}]);
        call(&service, "modelSource.set", input).await.unwrap();
        assert!(delayed.await.is_err());
        service.instance_ended("acme.services", "old");
        assert!(
            sources::effective_config(
                settings,
                &json!({
                    "sourceId": "plugin/acme.services/proxy", "model": "new-model"
                })
            )
            .is_ok()
        );
        service
            .manager
            .lock()
            .unwrap()
            .authorize("acme.services", vec![])
            .unwrap();
        service.instance_ended("acme.services", "new");
        assert!(sources::sources(settings).unwrap().is_empty());
    }

    #[tokio::test]
    async fn model_sources_are_declared_authorized_validated_and_revoked_with_the_instance() {
        use magi_conversation_runtime::main_model_sources as sources;
        let (_dir, service) = fixture();
        service.instance_started("acme.services", "first").unwrap();
        let settings = Arc::clone(service.settings.get().unwrap());
        call(&service, "modelSource.set", source_input())
            .await
            .unwrap();
        let resolved = sources::effective_config(
            &settings,
            &json!({"sourceId": "plugin/acme.services/proxy", "model": "m1"}),
        )
        .unwrap();
        assert_eq!(resolved["baseUrl"], "https://proxy.example/v1");

        // 未在清单声明、无授权、地址无效，都在写入口被拒绝，目录不变。
        let mut undeclared = source_input();
        undeclared["sourceId"] = json!("other");
        assert_eq!(
            call(&service, "modelSource.set", undeclared)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
        let mut bad = source_input();
        bad["baseUrl"] = json!("not a url");
        assert_eq!(
            call(&service, "modelSource.set", bad)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidParams
        );
        let mut extra = source_input();
        extra["unexpected"] = json!(1);
        assert_eq!(
            call(&service, "modelSource.set", extra)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidParams
        );
        assert_eq!(sources::sources(&settings).unwrap().len(), 1);

        // 整体替换：新的模型列表取代旧的，旧模型不再可选。
        let mut replaced = source_input();
        replaced["models"] = json!([{"id": "m2"}]);
        call(&service, "modelSource.set", replaced).await.unwrap();
        assert!(
            sources::effective_config(
                &settings,
                &json!({"sourceId": "plugin/acme.services/proxy", "model": "m1"}),
            )
            .is_err()
        );

        // 实例退出即撤销；已选用它的会话得到“不可用”，不会回退。
        service.instance_ended("acme.services", "first");
        let error = sources::effective_config(
            &settings,
            &json!({"sourceId": "plugin/acme.services/proxy", "model": "m2"}),
        )
        .unwrap_err();
        assert!(error.contains("插件数据源当前不可用"), "{error}");
        assert!(sources::sources(&settings).unwrap().is_empty());

        // 撤权后写入口关闭。
        service.instance_started("acme.services", "second").unwrap();
        call(&service, "modelSource.set", source_input())
            .await
            .unwrap();
        service
            .manager
            .lock()
            .unwrap()
            .authorize("acme.services", vec![])
            .unwrap();
        assert_eq!(
            call(&service, "modelSource.clear", json!({"sourceId": "proxy"}))
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
    }
}
