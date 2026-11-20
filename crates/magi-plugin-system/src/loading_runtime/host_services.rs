//! Source-save requests from the shared host. Runtime identity and source CAS
//! are checked before preparation and again before the authoritative commit.
use super::*;
use crate::lifecycle::{LoadingInstanceAction, LoadingInstanceTarget};
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::ErrorKind,
};
use serde::Deserialize;
use std::sync::Weak;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LoadingCaller {
    host_generation: u64,
    plugin_id: String,
    instance_id: String,
    activation_generation: u64,
    version: String,
    configuration_epoch: u64,
}

impl LoadingCaller {
    pub(super) fn validate(
        &self,
        runtime: &LoadingRuntime,
        owner: &PluginManager,
    ) -> Result<(), PluginError> {
        let current = owner.loading_instance_binding(&self.instance_id);
        if self.host_generation != runtime.host.generation()
            || !current.is_some_and(|activation| {
                activation.host_generation == self.host_generation
                    && activation.activation_generation == self.activation_generation
                    && activation.executable.plugin_id == self.plugin_id
                    && activation.executable.version == self.version
            })
        {
            return Err(PluginError::Conflict("加载配置请求的插件实例已失效".into()));
        }
        if !owner.loading_configuration_scope_current(&self.instance_id, self.configuration_epoch) {
            return Err(PluginError::Conflict(
                "加载配置请求的执行作用域已失效".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceSubmission {
    caller: LoadingCaller,
    expected_revision: u64,
    operation: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceSynchronization {
    caller: LoadingCaller,
    minimum_revision: u64,
    refresh: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfilePatchEditRequest {
    expected_revision: u64,
    id: String,
    name: String,
    #[serde(default)]
    next_config: Value,
    clear_override: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostRestartRequest {
    caller: LoadingCaller,
    minimum_revision: u64,
}

pub type LoadingHostRestartTrigger = Arc<dyn Fn(u64) -> Result<(), PluginError> + Send + Sync>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InstanceChange {
    pub(super) caller: LoadingCaller,
    pub(super) expected_revision: u64,
    pub(super) action: LoadingInstanceAction,
    pub(super) targets: Vec<LoadingInstanceTarget>,
    #[serde(default)]
    pub(super) activate_targets: Vec<LoadingInstanceTarget>,
    #[serde(default)]
    pub(super) entry_id: Option<String>,
}

fn rpc_error(error: LoadingRuntimeError) -> RpcError {
    match error {
        LoadingRuntimeError::Host(error) => error,
        _ => RpcError::new(ErrorKind::Conflict, error.to_string()),
    }
}

/// Construct before launching the host, then bind once before applying any
/// graph. A weak binding lets the process owner shut down without an RPC cycle.
pub struct LoadingHostServices {
    runtime: Mutex<Option<Weak<LoadingRuntime>>>,
    restart: Mutex<Option<LoadingHostRestartTrigger>>,
    delegate: Arc<dyn Incoming>,
}

impl LoadingHostServices {
    pub fn new(delegate: Arc<dyn Incoming>) -> Self {
        Self {
            runtime: Mutex::new(None),
            restart: Mutex::new(None),
            delegate,
        }
    }

    pub fn bind(&self, runtime: &Arc<LoadingRuntime>) -> Result<(), PluginError> {
        let mut binding = self
            .runtime
            .lock()
            .map_err(|_| PluginError::CorruptState("加载服务绑定锁已损坏".into()))?;
        if binding.is_some() {
            return Err(PluginError::Conflict(
                "加载服务不能重新绑定，请随宿主创建新实例".into(),
            ));
        }
        *binding = Some(Arc::downgrade(runtime));
        Ok(())
    }

    pub fn bind_restart(&self, trigger: LoadingHostRestartTrigger) -> Result<(), PluginError> {
        let mut restart = self
            .restart
            .lock()
            .map_err(|_| PluginError::CorruptState("加载重启绑定锁已损坏".into()))?;
        if restart.is_some() {
            return Err(PluginError::Conflict("加载重启触发器不能重复绑定".into()));
        }
        *restart = Some(trigger);
        Ok(())
    }

    fn runtime(&self) -> Option<Arc<LoadingRuntime>> {
        self.runtime.lock().ok()?.as_ref()?.upgrade()
    }

    fn restart(&self) -> Option<LoadingHostRestartTrigger> {
        self.restart.lock().ok()?.clone()
    }
}

impl Incoming for LoadingHostServices {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        if !matches!(
            method.as_str(),
            "loading.edit.submit"
                | "loading.profilePatch.edit"
                | "loading.sources.synchronize"
                | "loading.instances.change"
                | "loading.host.restart"
        ) {
            return self.delegate.request(method, params);
        }
        let runtime = self.runtime();
        let restart = self.restart();
        Box::pin(async move {
            let runtime = runtime
                .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "加载协调器未绑定或已关闭"))?;
            if method == "loading.profilePatch.edit" {
                let request: ProfilePatchEditRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let revision = runtime
                    .edit_profile_patch(
                        request.expected_revision,
                        request.id,
                        request.name,
                        request.next_config,
                        request.clear_override,
                    )
                    .await
                    .map_err(rpc_error)?;
                return Ok(json!({ "revision": revision }));
            }
            if method == "loading.host.restart" {
                let request: HostRestartRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let host_generation = runtime
                    .validate_host_restart(&request.caller, request.minimum_revision)
                    .map_err(rpc_error)?;
                let trigger = restart.ok_or_else(|| {
                    RpcError::new(ErrorKind::Conflict, "当前宿主未接入重启监管器")
                })?;
                trigger(host_generation)
                    .map_err(|error| RpcError::new(ErrorKind::Conflict, error.to_string()))?;
                return Ok(json!({ "hostGeneration": host_generation }));
            }
            if method == "loading.instances.change" {
                let request: InstanceChange = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let receipt = runtime
                    .change_loading_instances(&request)
                    .await
                    .map_err(rpc_error)?;
                return Ok(json!({ "revision": receipt["token"]["revision"] }));
            }
            if method == "loading.sources.synchronize" {
                let request: SourceSynchronization = serde_json::from_value(params)
                    .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
                let result = runtime
                    .synchronize_for_caller(
                        request.minimum_revision,
                        &request.caller,
                        request.refresh,
                    )
                    .await
                    .map_err(rpc_error)?;
                return Ok(json!({ "revision": result.revision }));
            }
            let request: SourceSubmission = serde_json::from_value(params)
                .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))?;
            let receipt = runtime
                .submit_source_edit_inner(
                    request.expected_revision,
                    request.operation,
                    Some(&request.caller),
                )
                .await
                .map_err(rpc_error)?;
            Ok(json!({ "result": receipt.result, "revision": receipt.revision }))
        })
    }

    fn notification(&self, method: String, params: Value) {
        if method == "plugin.status"
            && let Some(runtime) = self.runtime()
            && let Err(error) = runtime.observe_instance_status(&params)
        {
            self.delegate.notification(
                "host.diagnostic".into(),
                json!({
                    "hostGeneration": runtime.host.generation(), "type": "error",
                    "message": format!("无法接纳加载实例状态：{error}"),
                }),
            );
            runtime.host.invalidate();
        }
        self.delegate.notification(method, params);
    }
}
