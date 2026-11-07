//! HTTP 与扩展宿主共用的插件管理入口。只持有原有事实源与停止端口，
//! 不捕获完整 ApiState；停止端口使用弱引用，避免 daemon/runtime 引用环。
use super::*;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

#[derive(Clone)]
pub struct PluginOperations {
    manager: Arc<Mutex<PluginManager>>,
    events: Arc<InMemoryEventBus>,
    extension_status: Option<Weak<dyn crate::ExtensionStatusSource>>,
    pub(super) plugin_toolchain: Option<crate::PluginToolchain>,
    pub(super) processes: magi_process::ManagedProcessGroup,
}

pub(super) type OperationCaller<'a> = (&'a str, &'a str, Option<&'a str>, bool);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    fn package(id: &str, management: bool) -> PluginPackage {
        package_permissions(
            id,
            if management {
                &["install", "lifecycle"]
            } else {
                &[]
            },
        )
    }

    fn package_permissions(id: &str, management: &[&str]) -> PluginPackage {
        let mut permissions = vec![json!({"kind":"extension", "targets":["full-trust"]})];
        if !management.is_empty() {
            permissions.push(json!({"kind":"pluginManagement", "targets":management}));
        }
        let manifest = json!({
            "sdkVersion":2, "id":id, "version":"1.0.0", "name":id, "description":"", "dataSchemaVersion":1,
            "extension":{"hooks":[]}, "permissions":permissions,
            "settingsSchema":{"type":"object"},
            "contributions":{"commands":[{"id":"status", "title":"Status", "description":""}]}
        });
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", b"export default () => {};".to_vec()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    fn install(state: &ApiState, package: &PluginPackage, active: bool) {
        let id = &package.manifest().id;
        let mut manager = state.plugin_manager.lock().unwrap();
        manager
            .install(
                package,
                PluginSource::Local {
                    name: format!("{id}.zip"),
                },
            )
            .unwrap();
        if active {
            manager.enable(id).unwrap();
            manager
                .authorize(id, package.manifest().permissions.clone())
                .unwrap();
            manager.activate(id).unwrap();
            manager
                .begin_runtime(id, package.digest(), "current")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn compatibility_writes_require_independent_grant_risk_confirmation_and_current_lease() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package_permissions("acme.compatibility", &["compatibility"]);
        let readonly = package("acme.manager", true);
        install(&state, &caller, true);
        install(&state, &readonly, true);
        let mut operations = PluginOperations::from_state(&state);
        let compat = root.path().join("compat");
        std::fs::create_dir_all(compat.join("node_modules/@deepseek-ai/dsh-tools")).unwrap();
        std::fs::write(
            compat.join("node_modules/@deepseek-ai/dsh-tools/package.json"),
            br#"{"version":"0.2.2-rc.1"}"#,
        )
        .unwrap();
        std::fs::write(compat.join("package-lock.json"), b"{}").unwrap();
        operations.plugin_toolchain = Some(crate::PluginToolchain {
            node: root.path().join("node"),
            compat,
        });
        let current = operations
            .call(
                "acme.manager",
                readonly.digest(),
                Some("current"),
                "plugins.compatibility",
                json!({}),
            )
            .await
            .unwrap();
        let mut request = json!({"target":"example@1.2.3","runtimeVersion":current["runtimeVersion"],"expectedRuntimeId":current["runtimeId"],"expectedRevision":current["revision"],"enabled":true,"acceptRisk":false});
        assert!(
            operations
                .call(
                    "acme.manager",
                    readonly.digest(),
                    Some("current"),
                    "plugins.setVersionExemption",
                    request.clone()
                )
                .await
                .unwrap_err()
                .message()
                .contains("compatibility")
        );
        assert!(
            operations
                .call(
                    "acme.compatibility",
                    caller.digest(),
                    Some("current"),
                    "plugins.setVersionExemption",
                    request.clone()
                )
                .await
                .is_err()
        );
        request["acceptRisk"] = json!(true);
        assert!(
            operations
                .call(
                    "acme.compatibility",
                    caller.digest(),
                    Some("old"),
                    "plugins.setVersionExemption",
                    request.clone()
                )
                .await
                .is_err()
        );
        let updated = operations
            .call(
                "acme.compatibility",
                caller.digest(),
                Some("current"),
                "plugins.setVersionExemption",
                request.clone(),
            )
            .await
            .unwrap();
        assert_eq!(updated["exemptions"]["example@1.2.3"][0], "0.2.2-rc.1");
        assert!(
            operations
                .call(
                    "acme.compatibility",
                    caller.digest(),
                    Some("current"),
                    "plugins.setVersionExemption",
                    request
                )
                .await
                .is_err()
        );
        assert!(
            operations
                .call(
                    "acme.manager",
                    readonly.digest(),
                    None,
                    "plugins.compatibility",
                    json!({})
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn management_uses_real_lifecycle_and_cannot_grant_new_packages() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package("acme.manager", true);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &target, false);
        let operations = PluginOperations::from_state(&state);
        let invoke = |method: &'static str, params| {
            operations.call(
                "acme.manager",
                caller.digest(),
                Some("current"),
                method,
                params,
            )
        };
        let error = invoke(
            "plugins.setEnabled",
            json!({"pluginId":"acme.target", "enabled":true}),
        )
        .await
        .unwrap_err();
        assert!(!error.message().is_empty());
        assert!(
            !state
                .plugin_manager
                .lock()
                .unwrap()
                .is_active("acme.target")
                .unwrap(),
            "管理插件不能代授 full-trust"
        );
        state
            .plugin_manager
            .lock()
            .unwrap()
            .authorize("acme.target", target.manifest().permissions.clone())
            .unwrap();
        invoke(
            "plugins.setEnabled",
            json!({"pluginId":"acme.target", "enabled":true}),
        )
        .await
        .unwrap();
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .is_active("acme.target")
                .unwrap()
        );
        invoke(
            "plugins.setEnabled",
            json!({"pluginId":"acme.target", "enabled":false}),
        )
        .await
        .unwrap();
        assert!(
            !state
                .plugin_manager
                .lock()
                .unwrap()
                .is_active("acme.target")
                .unwrap()
        );
        invoke("plugins.remove", json!({"pluginId":"acme.target"}))
            .await
            .unwrap();
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .package("acme.target")
                .is_err()
        );
        assert!(
            invoke(
                "plugins.setEnabled",
                json!({"pluginId":"acme.manager", "enabled":false})
            )
            .await
            .is_err()
        );
        assert!(
            invoke("plugins.remove", json!({"pluginId":"acme.manager"}))
                .await
                .is_err()
        );
        assert!(
            invoke(
                "plugins.install",
                json!({"ecosystem":"dsh", "package":"sample", "version":"1.0.0"})
            )
            .await
            .unwrap_err()
            .message()
            .contains("摘要")
        );
        assert!(
            invoke(
                "plugins.setEnabled",
                json!({"pluginId":"acme.target", "enabled":true, "grants":[]})
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn background_management_requires_its_own_grant_and_current_instance() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package_permissions("acme.background", &["lifecycle", "background"]);
        let ordinary = package("acme.foreground", true);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &ordinary, true);
        install(&state, &target, false);
        let operations = PluginOperations::from_state(&state);
        let input = json!({"pluginId":"acme.target"});
        assert!(
            operations
                .call_background(
                    "acme.foreground",
                    ordinary.digest(),
                    Some("current"),
                    "plugins.remove",
                    input.clone()
                )
                .await
                .unwrap_err()
                .message()
                .contains("background")
        );
        assert!(
            operations
                .call_background(
                    "acme.background",
                    caller.digest(),
                    Some("old"),
                    "plugins.remove",
                    input.clone()
                )
                .await
                .is_err()
        );
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .package("acme.target")
                .is_ok()
        );
        operations
            .call_background(
                "acme.background",
                caller.digest(),
                Some("current"),
                "plugins.remove",
                input,
            )
            .await
            .unwrap();
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .package("acme.target")
                .is_err()
        );
        state
            .plugin_manager
            .lock()
            .unwrap()
            .deactivate("acme.background")
            .unwrap();
        assert!(
            operations
                .call_background(
                    "acme.background",
                    caller.digest(),
                    Some("current"),
                    "plugins.preview",
                    json!({})
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn background_caller_revoked_during_target_drain_cannot_commit_removal() {
        struct RevokeDuringDrain(Arc<Mutex<PluginManager>>);
        impl crate::ExtensionStatusSource for RevokeDuringDrain {
            fn status(
                &self,
                _: &str,
            ) -> Option<magi_app_server_protocol::generated::PluginRuntimeStatus> {
                None
            }
            fn quiesce(
                &self,
                _: String,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
            {
                let manager = self.0.clone();
                Box::pin(async move {
                    manager
                        .lock()
                        .unwrap()
                        .deactivate("acme.background")
                        .unwrap();
                    Ok(())
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package_permissions("acme.background", &["lifecycle", "background"]);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &target, true);
        let manager = state.plugin_manager.clone();
        let state = state.with_extension_status(Arc::new(RevokeDuringDrain(manager.clone())));
        let operations = PluginOperations::from_state(&state);
        assert!(
            operations
                .call_background(
                    "acme.background",
                    caller.digest(),
                    Some("current"),
                    "plugins.remove",
                    json!({"pluginId":"acme.target"})
                )
                .await
                .is_err()
        );
        assert!(manager.lock().unwrap().package("acme.target").is_ok());
    }

    #[tokio::test]
    async fn management_rejects_missing_permission_stale_digest_lease_and_revoked_caller() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let unprivileged = package("acme.unprivileged", false);
        let caller = package("acme.manager", true);
        install(&state, &unprivileged, true);
        install(&state, &caller, true);
        let operations = PluginOperations::from_state(&state);
        assert!(
            operations
                .call(
                    "acme.unprivileged",
                    unprivileged.digest(),
                    Some("current"),
                    "plugins.preview",
                    json!({})
                )
                .await
                .unwrap_err()
                .message()
                .contains("pluginManagement")
        );
        for (digest, lease) in [
            ("old", Some("current")),
            (caller.digest(), Some("old")),
            (caller.digest(), None),
        ] {
            assert!(
                operations
                    .call(
                        "acme.manager",
                        digest,
                        lease,
                        "plugins.remove",
                        json!({"pluginId":"acme.unprivileged"})
                    )
                    .await
                    .is_err()
            );
        }
        state
            .plugin_manager
            .lock()
            .unwrap()
            .deactivate("acme.manager")
            .unwrap();
        assert!(
            operations
                .call(
                    "acme.manager",
                    caller.digest(),
                    Some("current"),
                    "plugins.remove",
                    json!({"pluginId":"acme.unprivileged"})
                )
                .await
                .is_err()
        );
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .package("acme.unprivileged")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn management_reports_runtime_failure_instead_of_claiming_activation_applied() {
        use magi_app_server_protocol::generated::{PluginRuntimeState, PluginRuntimeStatus};
        struct FailedRuntime;
        impl crate::ExtensionStatusSource for FailedRuntime {
            fn status(&self, _: &str) -> Option<PluginRuntimeStatus> {
                Some(PluginRuntimeStatus {
                    state: PluginRuntimeState::Failed,
                    reason: Some("原包启动失败".into()),
                    generation: 1,
                    crashes: 1,
                    blocking: Vec::new(),
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path())
            .with_extension_status(Arc::new(FailedRuntime));
        let caller = package("acme.manager", true);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &target, false);
        let list = PluginOperations::from_state(&state).projection().unwrap();
        assert!(list["plugins"].as_array().unwrap().iter().any(|plugin| {
            plugin["runtime"]["state"] == "failed" && plugin["runtime"]["reason"] == "原包启动失败"
        }));
        state
            .plugin_manager
            .lock()
            .unwrap()
            .authorize("acme.target", target.manifest().permissions.clone())
            .unwrap();
        let error = PluginOperations::from_state(&state)
            .call(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                json!({"pluginId":"acme.target", "enabled":true}),
            )
            .await
            .unwrap_err();
        assert!(error.message().contains("原包启动失败"));
    }

    #[tokio::test]
    async fn delegated_authorization_requires_its_own_grant_and_exact_target_version() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package_permissions("acme.delegate", &["install", "lifecycle", "authorize"]);
        let readonly = package("acme.manager", true);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &readonly, true);
        install(&state, &target, false);
        let operations = PluginOperations::from_state(&state);
        let input = json!({"pluginId":"acme.target", "expectedDigest":target.digest(), "grants":target.manifest().permissions});
        assert!(
            operations
                .call(
                    "acme.manager",
                    readonly.digest(),
                    Some("current"),
                    "plugins.authorize",
                    input.clone()
                )
                .await
                .unwrap_err()
                .message()
                .contains("authorize")
        );
        let mut stale = input.clone();
        stale["expectedDigest"] = json!("old");
        assert!(
            operations
                .call(
                    "acme.delegate",
                    caller.digest(),
                    Some("current"),
                    "plugins.authorize",
                    stale
                )
                .await
                .unwrap_err()
                .message()
                .contains("版本")
        );
        let mut undeclared = input.clone();
        undeclared["grants"] = json!([{"kind":"models", "targets":["default"]}]);
        assert!(
            operations
                .call(
                    "acme.delegate",
                    caller.digest(),
                    Some("current"),
                    "plugins.authorize",
                    undeclared
                )
                .await
                .is_err()
        );
        operations
            .call(
                "acme.delegate",
                caller.digest(),
                Some("current"),
                "plugins.authorize",
                input,
            )
            .await
            .unwrap();
        assert!(
            !state
                .plugin_manager
                .lock()
                .unwrap()
                .is_active("acme.target")
                .unwrap()
        );
        operations
            .call(
                "acme.delegate",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                json!({"pluginId":"acme.target", "enabled":true, "expectedDigest":target.digest()}),
            )
            .await
            .unwrap();
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .is_active("acme.target")
                .unwrap()
        );
        let error = operations
            .call(
                "acme.delegate",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                json!({"pluginId":"acme.target", "enabled":false, "expectedDigest":"old"}),
            )
            .await
            .unwrap_err();
        assert!(error.message().contains("版本"));
        assert!(
            state
                .plugin_manager
                .lock()
                .unwrap()
                .is_active("acme.target")
                .unwrap(),
            "过期 profile 提案不能停止新版实例"
        );
        assert!(operations.call("acme.delegate", caller.digest(), Some("current"), "plugins.authorize", json!({"pluginId":"acme.delegate", "expectedDigest":caller.digest(), "grants":[]})).await.is_err());
    }
}

impl PluginOperations {
    pub fn from_state(state: &ApiState) -> Self {
        Self {
            manager: Arc::clone(&state.plugin_manager),
            events: Arc::clone(&state.event_bus),
            extension_status: state.extension_status.as_ref().map(Arc::downgrade),
            plugin_toolchain: state.plugin_toolchain.clone(),
            processes: state.managed_process_group().clone(),
        }
    }

    pub(super) fn locked(&self, action: &str) -> Result<MutexGuard<'_, PluginManager>, ApiError> {
        self.manager
            .lock()
            .map_err(|_| ApiError::internal_assembly(action, "插件锁已损坏"))
    }

    fn changed(&self) {
        self.events.publish(EventEnvelope::domain(
            EventId::unique("plugin-registry-changed"),
            "plugin.registry.changed",
            serde_json::json!({}),
        ));
    }

    pub(super) async fn quiesce(&self, id: &str) -> Result<(), ApiError> {
        let executable = self
            .locked("读取扩展宿主")?
            .package(id)
            .map_err(plugin_error)?
            .manifest()
            .extension
            .is_some();
        if executable && let Some(source) = &self.extension_status {
            source
                .upgrade()
                .ok_or_else(|| ApiError::Conflict("扩展监管器已退出".into()))?
                .quiesce(id.to_owned())
                .await
                .map_err(|reason| ApiError::Conflict(format!("扩展停止失败：{reason}")))?;
        }
        Ok(())
    }

    pub(super) async fn install_or_upgrade(
        &self,
        package: &PluginPackage,
        source: PluginSource,
        caller: Option<OperationCaller<'_>>,
    ) -> Result<(), ApiError> {
        let installed = self
            .locked("读取已安装插件")?
            .state()
            .plugins
            .contains_key(&package.manifest().id);
        if installed {
            self.upgrade(package, source, caller).await
        } else {
            {
                let mut manager = self.locked("安装插件")?;
                Self::check_caller(&manager, caller, "install")?;
                self.check_upstream_compatibility(&manager, package, &source)?;
                manager.install(package, source).map_err(plugin_error)?;
            }
            self.changed();
            Ok(())
        }
    }

    pub(super) async fn upgrade(
        &self,
        package: &PluginPackage,
        source: PluginSource,
        caller: Option<OperationCaller<'_>>,
    ) -> Result<(), ApiError> {
        self.check_upstream_compatibility(&*self.locked("检查升级兼容策略")?, package, &source)?;
        let drain =
            PluginManager::begin_upgrade_drain(Arc::clone(&self.manager), &package.manifest().id)
                .map_err(plugin_error)?;
        self.quiesce(&package.manifest().id).await?;
        let result = {
            let mut manager = self.locked("升级插件")?;
            Self::check_caller(&manager, caller, "install")?;
            self.check_upstream_compatibility(&manager, package, &source)?;
            let result = manager
                .upgrade(package, source, &drain)
                .map_err(plugin_error);
            if result.is_ok() {
                drain.commit(&mut manager);
            }
            result
        };
        self.changed();
        result
    }

    fn check_upstream_compatibility(
        &self,
        manager: &PluginManager,
        package: &PluginPackage,
        source: &PluginSource,
    ) -> Result<(), ApiError> {
        if matches!(source, PluginSource::Upstream { ecosystem, .. } if ecosystem == "dsh") {
            let (version, runtime) = self.dsh_runtime()?;
            manager
                .ensure_dsh_candidate_compatible(package, source, &version, &runtime)
                .map_err(plugin_error)?;
        }
        Ok(())
    }

    /// 写操作只能由获授权的活动宿主调用；代授新版本需要独立 authorize 委托。
    pub(super) fn admitted(
        &self,
        caller: &str,
        digest: &str,
        lease: Option<&str>,
        target: &str,
    ) -> Result<(), ApiError> {
        Self::check_caller(
            &*self.locked("检查插件管理授权")?,
            Some((caller, digest, lease, false)),
            target,
        )
    }

    fn check_caller(
        manager: &PluginManager,
        identity: Option<OperationCaller<'_>>,
        target: &str,
    ) -> Result<(), ApiError> {
        let Some((caller, digest, lease, background)) = identity else {
            return Ok(());
        };

        if manager.package(caller).map_err(plugin_error)?.digest() != digest
            || !manager.is_active(caller).map_err(plugin_error)?
        {
            return Err(ApiError::Forbidden(
                "插件管理调用的版本或激活状态已失效".into(),
            ));
        }
        manager
            .ensure_editing_admitted(caller)
            .map_err(plugin_error)?;
        manager
            .check_runtime(
                caller,
                lease.ok_or_else(|| ApiError::Forbidden("插件管理需要活动宿主租约".into()))?,
            )
            .map_err(plugin_error)?;
        if background
            && !manager
                .permission_allowed(
                    caller,
                    magi_plugin_system::PluginPermissionKind::PluginManagement,
                    "background",
                )
                .map_err(plugin_error)?
        {
            return Err(ApiError::Forbidden(
                "缺少 pluginManagement:[background] 后台管理授权".into(),
            ));
        }
        if target.is_empty() {
            return Ok(());
        }
        if !manager
            .permission_allowed(
                caller,
                magi_plugin_system::PluginPermissionKind::PluginManagement,
                target,
            )
            .map_err(plugin_error)?
        {
            return Err(ApiError::Forbidden(format!(
                "缺少 pluginManagement:[{target}] 授权"
            )));
        }
        Ok(())
    }

    pub async fn call(
        &self,
        caller: &str,
        digest: &str,
        lease: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, ApiError> {
        self.call_with_scope(caller, digest, lease, method, params, false)
            .await
    }

    pub async fn call_background(
        &self,
        caller: &str,
        digest: &str,
        lease: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, ApiError> {
        self.call_with_scope(caller, digest, lease, method, params, true)
            .await
    }

    async fn call_with_scope(
        &self,
        caller: &str,
        digest: &str,
        lease: Option<&str>,
        method: &str,
        params: Value,
        background: bool,
    ) -> Result<Value, ApiError> {
        let target = if method == "plugins.setVersionExemption" {
            "compatibility"
        } else if method == "plugins.compatibility" {
            ""
        } else if matches!(method, "plugins.preview" | "plugins.install") {
            "install"
        } else if method == "plugins.authorize" {
            "authorize"
        } else {
            "lifecycle"
        };
        Self::check_caller(
            &*self.locked("检查插件管理授权")?,
            Some((caller, digest, lease, background)),
            target,
        )?;
        match method {
            "plugins.compatibility" => {
                if params.as_object().is_none_or(|object| !object.is_empty()) {
                    return Err(ApiError::InvalidInput("兼容策略读取不接受参数".into()));
                }
                serde_json::to_value(self.dsh_compatibility()?)
                    .map_err(|error| ApiError::internal_assembly("编码兼容策略失败", error))
            }
            "plugins.setVersionExemption" => {
                let request: magi_app_server_protocol::generated::PluginDshVersionExemptionRequest =
                    serde_json::from_value(params)
                        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
                let (version, runtime) = self.dsh_runtime()?;
                let result = {
                    let mut manager = self.locked("提交版本豁免")?;
                    Self::check_caller(
                        &manager,
                        Some((caller, digest, lease, background)),
                        "compatibility",
                    )?;
                    manager
                        .set_dsh_exemption(&version, &runtime, &request)
                        .map_err(plugin_error)?
                };
                self.changed();
                serde_json::to_value(result)
                    .map_err(|error| ApiError::internal_assembly("编码兼容策略失败", error))
            }
            "plugins.preview" => {
                let request = serde_json::from_value(params)
                    .map_err(|e| ApiError::InvalidInput(format!("预览参数无效：{e}")))?;
                serde_json::to_value(upstream::preview_candidate(self, &request).await?)
                    .map_err(|e| ApiError::internal_assembly("编码插件预览失败", e))
            }
            "plugins.install" => {
                let request = serde_json::from_value(params)
                    .map_err(|e| ApiError::InvalidInput(format!("安装参数无效：{e}")))?;
                // 下载/翻译可能耗时；提交前再次核对发起宿主的版本、租约和授权。
                upstream::install_candidate(
                    self,
                    &request,
                    Some((caller, digest, lease, background)),
                )
                .await?;
                self.projection()
            }
            "plugins.authorize" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Authorize {
                    plugin_id: String,
                    expected_digest: String,
                    grants: Vec<magi_plugin_system::PluginPermission>,
                }
                let input: Authorize = serde_json::from_value(params)
                    .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
                if input.plugin_id == caller {
                    return Err(ApiError::Forbidden("管理插件不能自行改变自己的权限".into()));
                }
                self.authorize(
                    &input.plugin_id,
                    input.grants,
                    Some((caller, digest, lease, background)),
                    Some(&input.expected_digest),
                )
                .await?;
                self.projection()
            }
            "plugins.setEnabled" | "plugins.remove" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Input {
                    plugin_id: String,
                    enabled: Option<bool>,
                    expected_digest: Option<String>,
                }
                let input: Input = serde_json::from_value(params)
                    .map_err(|e| ApiError::InvalidInput(format!("管理参数无效：{e}")))?;
                if input.plugin_id == caller {
                    return Err(ApiError::Conflict(
                        "插件不能在自己的调用中停止、移除或重启自身，请使用 Magi 插件管理入口"
                            .into(),
                    ));
                }
                if method == "plugins.setEnabled" && input.enabled.is_none() {
                    return Err(ApiError::InvalidInput("启停操作需要 enabled".into()));
                }
                if method == "plugins.remove" || input.enabled == Some(false) {
                    let active = self
                        .locked("读取插件激活状态")?
                        .is_active(&input.plugin_id)
                        .map_err(plugin_error)?;
                    if active {
                        self.lifecycle(
                            "deactivate",
                            &input.plugin_id,
                            Some((caller, digest, lease, background)),
                            input.expected_digest.as_deref(),
                        )
                        .await?;
                    }
                    // 停止后发起方可能已撤权；后续写入仍核对同一身份。
                    Self::check_caller(
                        &*self.locked("检查插件管理授权")?,
                        Some((caller, digest, lease, background)),
                        target,
                    )?;
                    self.lifecycle(
                        "disable",
                        &input.plugin_id,
                        Some((caller, digest, lease, background)),
                        input.expected_digest.as_deref(),
                    )
                    .await?;
                    if method == "plugins.remove" {
                        self.lifecycle(
                            "uninstall",
                            &input.plugin_id,
                            Some((caller, digest, lease, background)),
                            input.expected_digest.as_deref(),
                        )
                        .await?;
                    }
                } else {
                    self.lifecycle(
                        "enable",
                        &input.plugin_id,
                        Some((caller, digest, lease, background)),
                        input.expected_digest.as_deref(),
                    )
                    .await?;
                    self.lifecycle(
                        "activate",
                        &input.plugin_id,
                        Some((caller, digest, lease, background)),
                        input.expected_digest.as_deref(),
                    )
                    .await?;
                    self.wait_running(&input.plugin_id, Some((caller, digest, lease, background)))
                        .await?;
                }
                self.projection()
            }
            _ => Err(ApiError::InvalidInput("未知插件管理操作".into())),
        }
    }

    /// SDK 版本取自真实随包 dsh-tools；指纹与译包 runtimeId 使用同一锁文件。
    fn dsh_runtime(&self) -> Result<(String, String), ApiError> {
        use sha2::{Digest, Sha256};
        let toolchain = self
            .plugin_toolchain
            .as_ref()
            .ok_or_else(|| ApiError::Conflict("DSH 兼容运行时不可用".into()))?;
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(
                toolchain
                    .compat
                    .join("node_modules/@deepseek-ai/dsh-tools/package.json"),
            )
            .map_err(|error| ApiError::internal_assembly("读取兼容 SDK 失败", error))?,
        )
        .map_err(|error| ApiError::internal_assembly("读取兼容 SDK 失败", error))?;
        let version = manifest
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::Conflict("DSH SDK 版本缺失".into()))?
            .to_owned();
        let lock = std::fs::read(toolchain.compat.join("package-lock.json"))
            .map_err(|error| ApiError::internal_assembly("读取兼容运行时摘要失败", error))?;
        let id = magi_core::hex_lower(Sha256::digest(lock))[..24].to_owned();
        Ok((version, id))
    }

    pub(super) fn dsh_compatibility(
        &self,
    ) -> Result<magi_app_server_protocol::generated::PluginDshCompatibilityProjection, ApiError>
    {
        let (version, runtime) = self.dsh_runtime()?;
        self.locked("读取兼容策略")?
            .dsh_compatibility(&version, &runtime)
            .map_err(plugin_error)
    }

    /// HTTP 与受信插件使用同一个生命周期实现。
    pub(super) async fn lifecycle(
        &self,
        action: &str,
        id: &str,
        caller: Option<OperationCaller<'_>>,
        expected_digest: Option<&str>,
    ) -> Result<(), ApiError> {
        {
            let manager = self.locked("检查插件管理准入")?;
            Self::check_caller(&manager, caller, "lifecycle")?;
            Self::check_target(&manager, id, expected_digest)?;
        }
        if action == "deactivate" {
            let drain = PluginManager::begin_deactivation(Arc::clone(&self.manager), id)
                .map_err(plugin_error)?;
            self.quiesce(id).await?;
            let result = {
                let mut manager = self.locked("停用插件")?;
                Self::check_target(&manager, id, expected_digest)?;
                Self::check_caller(&manager, caller, "lifecycle")?;
                let result = manager.deactivate(id).map_err(plugin_error);
                if result.is_ok() {
                    drain.commit(&mut manager);
                }
                result
            };
            self.changed();
            return result;
        }
        let result = {
            let mut manager = self.locked("管理插件")?;
            Self::check_target(&manager, id, expected_digest)?;
            Self::check_caller(&manager, caller, "lifecycle")?;
            match action {
                "enable" => manager.enable(id),
                "activate" => manager.activate(id).map(|_| ()),
                "disable" => manager.disable(id),
                "uninstall" => manager.uninstall(id),
                _ => return Err(ApiError::InvalidInput("未知生命周期操作".into())),
            }
            .map_err(plugin_error)
        };
        self.changed();
        result
    }

    pub(super) async fn authorize(
        &self,
        id: &str,
        grants: Vec<magi_plugin_system::PluginPermission>,
        caller: Option<OperationCaller<'_>>,
        expected: Option<&str>,
    ) -> Result<(), ApiError> {
        let active = {
            let manager = self.locked("读取插件授权")?;
            Self::check_caller(&manager, caller, "authorize")?;
            Self::check_target(&manager, id, expected)?;
            manager.is_active(id).map_err(plugin_error)?
        };
        let drain = if active {
            let drain = PluginManager::begin_deactivation(Arc::clone(&self.manager), id)
                .map_err(plugin_error)?;
            self.quiesce(id).await?;
            Some(drain)
        } else {
            None
        };
        let result = {
            let mut manager = self.locked("授权插件")?;
            Self::check_caller(&manager, caller, "authorize")?;
            Self::check_target(&manager, id, expected)?;
            let result = manager.authorize(id, grants).map_err(plugin_error);
            if result.is_ok() {
                if let Some(drain) = drain {
                    drain.commit(&mut manager);
                }
            }
            result
        };
        self.changed();
        result
    }

    fn check_target(
        manager: &PluginManager,
        id: &str,
        expected_digest: Option<&str>,
    ) -> Result<(), ApiError> {
        if let Some(expected) = expected_digest {
            if manager.package(id).map_err(plugin_error)?.digest() != expected {
                return Err(ApiError::Conflict(
                    "目标插件版本已变化，请刷新后重试".into(),
                ));
            }
        }
        Ok(())
    }

    /// 只合并监管器当前运行事实，HTTP 与宿主目录必须使用相同投影。
    pub fn decorate_runtime(&self, list: &mut magi_app_server_protocol::PluginList) {
        if let Some(source) = self.extension_status.as_ref().and_then(Weak::upgrade) {
            for plugin in &mut list.plugins {
                plugin.runtime = source.status(&plugin.manifest.id);
            }
        }
    }

    fn projection(&self) -> Result<Value, ApiError> {
        let mut list = self
            .locked("读取插件结果")?
            .projection()
            .map_err(plugin_error)?;
        self.decorate_runtime(&mut list);
        serde_json::to_value(list).map_err(|e| ApiError::internal_assembly("编码插件目录失败", e))
    }

    async fn wait_running(
        &self,
        id: &str,
        caller: Option<OperationCaller<'_>>,
    ) -> Result<(), ApiError> {
        use magi_app_server_protocol::generated::PluginRuntimeState;
        if self
            .locked("读取插件入口")?
            .package(id)
            .map_err(plugin_error)?
            .manifest()
            .extension
            .is_none()
        {
            return Ok(());
        }
        let Some(source) = &self.extension_status else {
            return Ok(());
        };
        let source = source
            .upgrade()
            .ok_or_else(|| ApiError::Conflict("扩展监管器已退出".into()))?;
        let wait = async {
            loop {
                {
                    let manager = self.locked("检查激活结果")?;
                    Self::check_caller(&manager, caller, "lifecycle")?;
                    if !manager.is_active(id).map_err(plugin_error)? {
                        return Err(ApiError::Conflict("目标插件已被停用或撤权".into()));
                    }
                }
                if let Some(status) = source.status(id) {
                    match status.state {
                        PluginRuntimeState::Running => {
                            if self
                                .locked("核对当前版本实例")?
                                .runtime_ready(id)
                                .map_err(plugin_error)?
                            {
                                return Ok(());
                            }
                        }
                        PluginRuntimeState::Failed => {
                            return Err(ApiError::Conflict(format!(
                                "插件激活失败：{}",
                                status.reason.unwrap_or_default()
                            )));
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(110), wait)
            .await
            .map_err(|_| {
                ApiError::Conflict("插件启动尚未确认，请在 Magi 插件管理查看运行状态".into())
            })?
    }
}
