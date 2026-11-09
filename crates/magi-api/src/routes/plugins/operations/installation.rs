use super::*;
use crate::plugin_installations::{InstallCandidate, InstallOwner, InstallationWaiter};
use magi_app_server_protocol::generated::{
    PluginBundleInstallApplication as Application, PluginBundleInstallError as InstallError,
    PluginBundleInstallRequest, PluginBundleInstallResult as InstallResult, PluginEcosystem,
    PluginUpstreamRequest,
};

pub(in crate::routes::plugins) fn owner(
    plugin: &str,
    digest: &str,
    lease: Option<&str>,
    background: bool,
) -> Result<InstallOwner, ApiError> {
    Ok(InstallOwner {
        plugin: plugin.into(),
        digest: digest.into(),
        background,
        lease: lease
            .ok_or_else(|| ApiError::Forbidden("在线安装任务需要活动宿主身份".into()))?
            .into(),
    })
}
impl InstallOwner {
    fn caller(&self) -> OperationCaller<'_> {
        (
            &self.plugin,
            &self.digest,
            Some(&self.lease),
            self.background,
        )
    }
}
fn failure(
    error: ApiError,
    installed: bool,
    bundle: &str,
    version: Option<String>,
    changed: Option<bool>,
) -> InstallResult {
    let code = if installed && matches!(error, ApiError::Forbidden(_)) {
        "authorization-required"
    } else {
        "magi-operation-failed"
    };
    InstallResult {
        application: Application::Failed,
        bundle: Some(bundle.into()),
        version,
        changed,
        installed: installed.then_some(true),
        stage: Some(if installed { "enable" } else { "install" }.into()),
        error: Some(InstallError {
            code: code.into(),
            message: error.message().into(),
            diagnostic: error.message().into(),
            incompatible: None,
        }),
    }
}
fn cancelled(bundle: &str) -> InstallResult {
    InstallResult {
        application: Application::Cancelled,
        bundle: Some(bundle.into()),
        version: None,
        changed: Some(false),
        installed: None,
        stage: Some("install".into()),
        error: None,
    }
}

fn bundle_label(input: &PluginBundleInstallRequest) -> String {
    input.package.clone().unwrap_or_else(|| {
        input
            .github
            .as_ref()
            .map(|s| format!("github:{}", s.repository))
            .unwrap_or_default()
    })
}

impl PluginOperations {
    pub fn installation_instance_ended(&self, plugin: &str, lease: &str) {
        self.installations.instance_ended(plugin, lease);
        match self.locked("恢复失效实例的未结算移除") {
            Ok(mut manager) => match manager.recover_instance_removals(plugin, lease) {
                Ok(ids) if !ids.is_empty() => self.changed(),
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "插件移除恢复尚未完成，记录保留"),
            },
            Err(error) => tracing::error!(error = error.message(), "插件移除恢复管理器不可用"),
        }
    }

    pub(super) async fn install_bundle(
        &self,
        owner: InstallOwner,
        input: PluginBundleInstallRequest,
    ) -> Result<InstallResult, ApiError> {
        self.install_bundle_prepared(owner, input, None).await
    }

    pub(in crate::routes::plugins) async fn install_bundle_prepared(
        &self,
        owner: InstallOwner,
        input: PluginBundleInstallRequest,
        prepared: Option<upstream::Translated>,
    ) -> Result<InstallResult, ApiError> {
        Self::check_caller(
            &*self.locked("检查安装任务准入")?,
            Some(owner.caller()),
            "install",
        )?;
        if let Some(change_id) = &input.change_id {
            Self::change_scope(owner.caller(), change_id.clone())?;
        }
        upstream::validate_source(input.package.as_deref(), input.github.as_ref())?;
        if !matches!(input.ecosystem, PluginEcosystem::Dsh) {
            return Err(ApiError::InvalidInput("该生态尚未提供在线安装".into()));
        }
        if input.github.is_none() {
            upstream::registry()?;
        }
        if input
            .version
            .as_ref()
            .is_some_and(|v| v.trim().is_empty() || v.len() > 128)
        {
            return Err(ApiError::InvalidInput(
                "npm 版本或范围必须为 1–128 个字符".into(),
            ));
        }
        let bundle = bundle_label(&input);
        let fingerprint =
            serde_json::to_string(&input).map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let (job, fresh) =
            self.installations
                .start(owner.clone(), &input.request_id, fingerprint)?;
        if fresh {
            let operations = self.clone();
            let job = job.clone();
            tokio::spawn(async move {
                let prepared = match prepared {
                    Some(value) => Ok(value),
                    None => tokio::select! {
                        biased;
                        _ = job.cancelled() => { job.finish(cancelled(&bundle)); return; }
                        result = tokio::time::timeout(Duration::from_secs(840), operations.prepare_bundle(&input)) => result
                            .unwrap_or_else(|_| Err(failure(ApiError::InvalidInput("安装准备超时，尚未提交版本".into()), false, &bundle, None, None))),
                    },
                };
                let translated = match prepared {
                    Ok(value) => value,
                    Err(result) => {
                        job.finish(result);
                        return;
                    }
                };
                let candidate = job.begin_apply(InstallCandidate {
                    plugin_id: translated.package.manifest().id.clone(),
                    digest: translated.package.digest().into(),
                    version: translated.package.manifest().version.clone(),
                });
                match candidate {
                    Ok(true) => {}
                    Ok(false) => {
                        job.finish(cancelled(&bundle));
                        return;
                    }
                    Err(error) => {
                        job.finish(failure(error, false, &bundle, None, None));
                        return;
                    }
                }
                // 提交开始后取消只返回 too-late；即便等待方退出，daemon 仍结算此任务。
                let result = operations
                    .apply_bundle(&owner, &input, translated.package, translated.source)
                    .await;
                job.finish(result);
            });
        }
        let _waiter = InstallationWaiter::new(job.clone());
        job.wait().await
    }

    async fn prepare_bundle(
        &self,
        input: &PluginBundleInstallRequest,
    ) -> Result<upstream::Translated, InstallResult> {
        let bundle = bundle_label(input);
        if input.github.is_some() {
            let request = PluginUpstreamRequest {
                ecosystem: input.ecosystem.clone(),
                package: input.package.clone(),
                version: input.version.clone(),
                expected_digest: None,
                github: input.github.clone(),
            };
            return upstream::prepare(self, &request, None)
                .await
                .map_err(|e| failure(e, false, &bundle, None, None));
        }
        let work = async {
            let toolchain = self
                .plugin_toolchain
                .as_ref()
                .ok_or_else(|| ApiError::Conflict("在线安装工具链不可用".into()))?;
            let output = upstream::run_toolchain(
                self,
                toolchain,
                &[
                    "resolve".into(),
                    input.package.clone().unwrap(),
                    input.version.clone().unwrap_or_else(|| "latest".into()),
                ],
            )
            .await?;
            let resolved: Value = serde_json::from_str(&output)
                .map_err(|e| ApiError::internal_assembly("读取 npm 版本解析结果失败", e))?;
            Ok::<_, ApiError>(resolved)
        }
        .await
        .map_err(|e| failure(e, false, &bundle, None, None))?;
        if let Some(error) = work.get("error") {
            let error: InstallError = serde_json::from_value(error.clone()).map_err(|e| {
                failure(
                    ApiError::InvalidInput(e.to_string()),
                    false,
                    &bundle,
                    None,
                    None,
                )
            })?;
            let mut result = cancelled(&bundle);
            result.application = Application::Failed;
            result.error = Some(error);
            return Err(result);
        }
        let document: upstream::VersionDocument = serde_json::from_value(work["document"].clone())
            .map_err(|e| {
                failure(
                    ApiError::internal_assembly("读取 npm 元数据失败", e),
                    false,
                    &bundle,
                    None,
                    None,
                )
            })?;
        let request = PluginUpstreamRequest {
            ecosystem: input.ecosystem.clone(),
            package: input.package.clone(),
            version: Some(document.version.clone()),
            expected_digest: None,
            github: None,
        };
        upstream::prepare(self, &request, Some(document))
            .await
            .map_err(|e| failure(e, false, &bundle, None, None))
    }

    async fn apply_bundle(
        &self,
        owner: &InstallOwner,
        input: &PluginBundleInstallRequest,
        package: PluginPackage,
        source: PluginSource,
    ) -> InstallResult {
        let bundle = match &source {
            PluginSource::Upstream { package, .. } => package.clone(),
            _ => {
                return failure(
                    ApiError::InvalidInput("安装来源不是上游原包".into()),
                    false,
                    &bundle_label(input),
                    None,
                    None,
                );
            }
        };
        let id = package.manifest().id.clone();
        let version = package.manifest().version.clone();
        if id == owner.plugin {
            return self.apply_self_update(owner, input, package, source).await;
        }
        let apply = async {
            let scope = input
                .change_id
                .as_ref()
                .map(|id| Self::change_scope(owner.caller(), id.clone()))
                .transpose()?;
            let changed = {
                let manager = self.locked("预览在线安装")?;
                if manager
                    .removed_for_change(&id, scope.as_ref())
                    .map_err(plugin_error)?
                {
                    true
                } else {
                    !matches!(
                        manager
                            .preview(&package, &source)
                            .map_err(plugin_error)?
                            .operation,
                        magi_app_server_protocol::generated::PluginCandidateOperation::Unchanged
                    )
                }
            };
            self.install_or_upgrade_change(&package, source, Some(owner.caller()), scope.as_ref())
                .await?;
            Ok::<_, ApiError>(changed)
        }
        .await;
        let changed = match apply {
            Ok(changed) => changed,
            Err(e) => return failure(e, false, &bundle, Some(version), None),
        };
        let enable = async {
            let enabled = input.enabled.unwrap_or(true);
            let (active, currently_enabled) = {
                let manager = self.locked("读取安装状态")?;
                (
                    manager.is_active(&id).map_err(plugin_error)?,
                    manager.state().plugins[&id].enabled,
                )
            };
            if !enabled && !active && !currently_enabled {
                return Ok(());
            }
            if enabled && !active {
                let delegated = self
                    .locked("检查安装授权委托")?
                    .permission_allowed(
                        &owner.plugin,
                        magi_plugin_system::PluginPermissionKind::PluginManagement,
                        "authorize",
                    )
                    .map_err(plugin_error)?;
                if delegated {
                    self.authorize(
                        &id,
                        package.manifest().permissions.clone(),
                        Some(owner.caller()),
                        Some(package.digest()),
                    )
                    .await?;
                }
            }
            self.manage_target(
                &id,
                enabled,
                false,
                Some(owner.caller()),
                LifecycleChange {
                    expected_digest: Some(package.digest()),
                    ..Default::default()
                },
            )
            .await
        }
        .await;
        match enable {
            Ok(()) => InstallResult {
                application: Application::Applied,
                bundle: Some(bundle),
                version: Some(version),
                changed: Some(changed),
                installed: Some(true),
                stage: None,
                error: None,
            },
            Err(e) => failure(e, true, &bundle, Some(version), Some(changed)),
        }
    }

    /// 自更新的提交由 daemon 安装任务拥有。排空旧宿主会结束发起 RPC，
    /// 不能再依赖该宿主接收回执或继续使用旧租约；唯一结果是安装事实、
    /// 持久任务回执和新宿主运行状态。新授权不得超出提交前的已批范围。
    async fn apply_self_update(
        &self,
        owner: &InstallOwner,
        input: &PluginBundleInstallRequest,
        package: PluginPackage,
        source: PluginSource,
    ) -> InstallResult {
        use magi_plugin_system::PluginPermissionKind;
        let id = &owner.plugin;
        let version = package.manifest().version.clone();
        let bundle = bundle_label(input);
        let updated = async {
            let previous = {
                let manager = self.locked("核对自更新授权")?;
                for target in ["install", "lifecycle", "authorize"] {
                    Self::check_caller(&manager, Some(owner.caller()), target)?;
                }
                if input.enabled == Some(false) || input.change_id.is_some() {
                    return Err(ApiError::InvalidInput(
                        "自更新保持原启用意愿，不接受自身移除或停用".into(),
                    ));
                }
                self.check_upstream_compatibility(&manager, &package, &source)?;
                let candidate = manager.preview(&package, &source).map_err(plugin_error)?;
                if candidate.data_reset {
                    return Err(ApiError::Forbidden(
                        "自更新需要重置插件数据，请在 Magi 在线升级入口确认".into(),
                    ));
                }
                for permission in &package.manifest().permissions {
                    for target in &permission.targets {
                        if !manager
                            .permission_allowed(id, permission.kind.clone(), target)
                            .map_err(plugin_error)?
                        {
                            return Err(ApiError::Forbidden(
                                "自更新包含尚未授权的能力，请在 Magi 在线升级入口确认新权限".into(),
                            ));
                        }
                    }
                }
                if !manager
                    .permission_allowed(id, PluginPermissionKind::Extension, "full-trust")
                    .map_err(plugin_error)?
                {
                    return Err(ApiError::Forbidden("自更新需要已批准的完全信任授权".into()));
                }
                if matches!(
                    candidate.operation,
                    magi_app_server_protocol::generated::PluginCandidateOperation::Unchanged
                ) {
                    return Ok(false);
                }
                manager.state().plugins[id].clone()
            };
            let drain =
                PluginManager::begin_runtime_upgrade_drain(self.manager.clone(), id, &owner.lease)
                    .map_err(plugin_error)?;
            self.quiesce(id).await?;
            let commit = (|| {
                let mut manager = self.locked("提交自更新")?;
                // 用户停用／撤权或其他安装若已改变事实，旧请求不能重启它。
                if manager.state().plugins.get(id) != Some(&previous) {
                    return Err(ApiError::Conflict(
                        "自更新期间插件状态已变化，请刷新后重试".into(),
                    ));
                }
                self.check_upstream_compatibility(&manager, &package, &source)?;
                manager
                    .upgrade(&package, source, &drain)
                    .map_err(plugin_error)?;
                drain.commit(&mut manager);
                manager
                    .authorize(id, package.manifest().permissions.clone())
                    .map_err(plugin_error)?;
                manager.activate(id).map_err(plugin_error)?;
                Ok(())
            })();
            self.changed();
            commit?;
            self.wait_running(id, None, Some(package.digest())).await?;
            Ok(true)
        }
        .await;
        match updated {
            Ok(changed) => InstallResult {
                application: Application::Applied,
                bundle: Some(bundle),
                version: Some(version),
                changed: Some(changed),
                installed: Some(true),
                stage: None,
                error: None,
            },
            Err(error) => {
                let installed = self.locked("核对自更新结果").is_ok_and(|manager| {
                    manager
                        .state()
                        .plugins
                        .get(id)
                        .is_some_and(|value| value.digest == package.digest())
                });
                failure(error, installed, &bundle, Some(version), None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    fn package(id: &str, permissions: Value) -> PluginPackage {
        versioned_package(id, permissions, "1.0.0")
    }
    fn versioned_package(id: &str, permissions: Value, version: &str) -> PluginPackage {
        let manifest = json!({"sdkVersion":2,"id":id,"version":version,"name":id,"description":"","dataSchemaVersion":1,
            "extension":{"hooks":[]},"permissions":permissions,"settingsSchema":{"type":"object"},
            "contributions":{"commands":[{"id":"status","title":"Status","description":""}]}});
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let mut files = vec![
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", b"export default () => {};".to_vec()),
        ];
        if id == "dsh.sample" {
            use sha2::{Digest, Sha256};
            let runtime = magi_core::hex_lower(Sha256::digest(b"{}"))[..24].to_owned();
            files.push(("extension/compat.json", serde_json::to_vec(&json!({
                "upstream":{"name":"sample","version":"1.0.0"},
                "compatibility":{"runtimeVersion":"0.2.2-rc.1","runtimeId":runtime,"requirements":null}
            })).unwrap()));
        }
        for (name, bytes) in files {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }
    fn fixture(grants: &[&str]) -> (ApiState, tempfile::TempDir, InstallOwner) {
        let root = tempfile::tempdir().unwrap();
        let compat = root.path().join("compat");
        std::fs::create_dir_all(compat.join("node_modules/@deepseek-ai/dsh-tools")).unwrap();
        std::fs::write(
            compat.join("node_modules/@deepseek-ai/dsh-tools/package.json"),
            br#"{"version":"0.2.2-rc.1"}"#,
        )
        .unwrap();
        std::fs::write(compat.join("package-lock.json"), b"{}").unwrap();
        let state = super::super::super::tests::bare_state(root.path()).with_plugin_toolchain(
            crate::PluginToolchain {
                node: root.path().join("node"),
                compat,
            },
        );
        let permissions = json!([{"kind":"extension","targets":["full-trust"]},{"kind":"pluginManagement","targets":grants}]);
        let caller = package("acme.manager", permissions);
        {
            let mut manager = state.plugin_manager.lock().unwrap();
            manager
                .install(
                    &caller,
                    PluginSource::Local {
                        name: "manager.zip".into(),
                    },
                )
                .unwrap();
            manager.enable("acme.manager").unwrap();
            manager
                .authorize("acme.manager", caller.manifest().permissions.clone())
                .unwrap();
            manager.activate("acme.manager").unwrap();
            manager
                .begin_runtime("acme.manager", caller.digest(), "current")
                .unwrap();
        }
        (
            state,
            root,
            owner("acme.manager", caller.digest(), Some("current"), false).unwrap(),
        )
    }
    fn input(enabled: bool) -> PluginBundleInstallRequest {
        PluginBundleInstallRequest {
            request_id: "job".into(),
            ecosystem: PluginEcosystem::Dsh,
            package: Some("sample".into()),
            github: None,
            version: Some("1.0.0".into()),
            enabled: Some(enabled),
            change_id: None,
        }
    }
    fn npm_source() -> PluginSource {
        PluginSource::Upstream {
            ecosystem: "dsh".into(),
            package: "sample".into(),
            version: "1.0.0".into(),
            github: None,
        }
    }
    fn target() -> PluginPackage {
        package(
            "dsh.sample",
            json!([{"kind":"extension","targets":["full-trust"]}]),
        )
    }

    #[tokio::test]
    async fn native_install_requires_explicit_authorize_delegation_and_reports_a_committed_inactive_package()
     {
        let (state, _root, who) = fixture(&["install", "lifecycle"]);
        let operations = PluginOperations::from_state(&state);
        let result = operations
            .apply_bundle(&who, &input(true), target(), npm_source())
            .await;
        assert_eq!(result.application, Application::Failed);
        assert_eq!(result.installed, Some(true), "{result:?}");
        assert_eq!(result.error.unwrap().code, "authorization-required");
        let manager = state.plugin_manager.lock().unwrap();
        assert!(manager.state().plugins["dsh.sample"].grants.is_empty());
        assert!(!manager.state().plugins["dsh.sample"].active);
    }

    #[tokio::test]
    async fn delegated_install_uses_the_committed_digest_and_disabled_install_does_not_require_extra_lifecycle_grants()
     {
        let (state, _root, who) = fixture(&["install", "lifecycle", "authorize"]);
        let operations = PluginOperations::from_state(&state);
        let result = operations
            .apply_bundle(&who, &input(true), target(), npm_source())
            .await;
        assert_eq!(result.application, Application::Applied, "{result:?}");
        assert!(state.plugin_manager.lock().unwrap().state().plugins["dsh.sample"].active);
        let (state, _root2, who) = fixture(&["install"]);
        let result = PluginOperations::from_state(&state)
            .apply_bundle(&who, &input(false), target(), npm_source())
            .await;
        assert_eq!(result.application, Application::Applied);
        let manager = state.plugin_manager.lock().unwrap();
        assert!(!manager.state().plugins["dsh.sample"].enabled);
        assert!(manager.state().plugins["dsh.sample"].grants.is_empty());
    }

    #[tokio::test]
    async fn background_admission_and_commit_both_require_the_current_authorized_owner() {
        let (state, _root, who) = fixture(&["install", "lifecycle", "authorize"]);
        let operations = PluginOperations::from_state(&state);
        let error = operations
            .call_background(
                &who.plugin,
                &who.digest,
                Some(&who.lease),
                "plugins.installBundle",
                serde_json::to_value(input(false)).unwrap(),
            )
            .await
            .unwrap_err();
        assert!(error.message().contains("background"));
        {
            let mut manager = state.plugin_manager.lock().unwrap();
            assert!(manager.end_runtime(&who.plugin, &who.lease));
            manager
                .begin_runtime(&who.plugin, &who.digest, "replacement")
                .unwrap();
        }
        let result = operations
            .apply_bundle(&who, &input(false), target(), npm_source())
            .await;
        assert_eq!(result.application, Application::Failed);
        assert_ne!(result.installed, Some(true));
        assert!(
            !state
                .plugin_manager
                .lock()
                .unwrap()
                .state()
                .plugins
                .contains_key("dsh.sample")
        );
    }

    struct SelfUpdateRuntime {
        manager: Arc<Mutex<PluginManager>>,
        stops: std::sync::atomic::AtomicUsize,
        revoke: bool,
        replacement: Option<PluginPackage>,
    }
    impl crate::ExtensionStatusSource for SelfUpdateRuntime {
        fn quiesce(
            &self,
            id: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        {
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut manager = self.manager.lock().unwrap();
            manager.end_runtime(&id, "current");
            if self.revoke {
                manager.deactivate(&id).unwrap();
            }
            Box::pin(async { Ok(()) })
        }
        fn status(
            &self,
            id: &str,
        ) -> Option<magi_app_server_protocol::generated::PluginRuntimeStatus> {
            if let Some(replacement) = &self.replacement {
                let drain = PluginManager::begin_upgrade_drain(self.manager.clone(), id).unwrap();
                let mut manager = self.manager.lock().unwrap();
                manager.upgrade(replacement, self_source(), &drain).unwrap();
                drain.commit(&mut manager);
                manager
                    .authorize(id, replacement.manifest().permissions.clone())
                    .unwrap();
                manager.activate(id).unwrap();
            }
            let mut manager = self.manager.lock().unwrap();
            let digest = manager.state().plugins[id].digest.clone();
            manager.begin_runtime(id, &digest, "new-instance").unwrap();
            manager.ready_runtime(id, "new-instance").unwrap();
            Some(magi_app_server_protocol::generated::PluginRuntimeStatus {
                state: magi_app_server_protocol::generated::PluginRuntimeState::Running,
                generation: 2,
                instance_id: None,
                reason: None,
                crashes: 0,
                blocking: vec![],
            })
        }
    }
    fn self_candidate(state: &ApiState, extra: bool) -> PluginPackage {
        let mut permissions = state
            .plugin_manager
            .lock()
            .unwrap()
            .package("acme.manager")
            .unwrap()
            .manifest()
            .permissions
            .clone();
        if extra {
            permissions.push(
                serde_json::from_value(json!({"kind":"sessionRead","targets":["directory"]}))
                    .unwrap(),
            );
        }
        versioned_package(
            "acme.manager",
            serde_json::to_value(permissions).unwrap(),
            "2.0.0",
        )
    }
    fn self_source() -> PluginSource {
        PluginSource::Local {
            name: "manager.zip".into(),
        }
    }

    #[tokio::test]
    async fn self_update_survives_old_instance_end_without_expanding_grants_and_rejects_its_old_lease()
     {
        let (state, _root, who) = fixture(&["install", "lifecycle", "authorize"]);
        let runtime = Arc::new(SelfUpdateRuntime {
            manager: state.plugin_manager.clone(),
            stops: 0.into(),
            revoke: false,
            replacement: None,
        });
        let candidate = self_candidate(&state, false);
        let state = state.with_extension_status(runtime.clone());
        let operations = PluginOperations::from_state(&state);
        let result = operations
            .apply_self_update(&who, &input(true), candidate.clone(), self_source())
            .await;
        assert_eq!(result.application, Application::Applied, "{result:?}");
        assert_eq!(runtime.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
        let manager = state.plugin_manager.lock().unwrap();
        let current = &manager.state().plugins[&who.plugin];
        assert_eq!(current.digest, candidate.digest());
        assert!(current.active && current.enabled);
        assert_eq!(current.grants, candidate.manifest().permissions);
        assert!(PluginOperations::check_caller(&manager, Some(who.caller()), "install").is_err());
        assert!(manager.runtime_ready(&who.plugin).unwrap());
    }

    #[tokio::test]
    async fn self_update_requires_authorize_delegation_and_rejects_new_permissions_before_stopping()
    {
        for delegated in [false, true] {
            let grants = if delegated {
                vec!["install", "lifecycle", "authorize"]
            } else {
                vec!["install", "lifecycle"]
            };
            let (state, _root, who) = fixture(&grants);
            let candidate = self_candidate(&state, delegated);
            let runtime = Arc::new(SelfUpdateRuntime {
                manager: state.plugin_manager.clone(),
                stops: 0.into(),
                revoke: false,
                replacement: None,
            });
            let state = state.with_extension_status(runtime.clone());
            let result = PluginOperations::from_state(&state)
                .apply_self_update(&who, &input(true), candidate, self_source())
                .await;
            assert_eq!(result.application, Application::Failed, "{result:?}");
            assert_eq!(runtime.stops.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(
                state.plugin_manager.lock().unwrap().state().plugins[&who.plugin].digest,
                who.digest
            );
        }
    }

    #[tokio::test]
    async fn self_update_rechecks_user_state_after_stop_and_preserves_old_installation_on_archive_failure()
     {
        for revoke in [false, true] {
            let (state, root, who) = fixture(&["install", "lifecycle", "authorize"]);
            let original =
                state.plugin_manager.lock().unwrap().state().plugins[&who.plugin].clone();
            let candidate = self_candidate(&state, false);
            if !revoke {
                std::fs::create_dir_all(
                    root.path()
                        .join("versions")
                        .join(&who.plugin)
                        .join(format!("{}.zip", candidate.digest())),
                )
                .unwrap();
            }
            let runtime = Arc::new(SelfUpdateRuntime {
                manager: state.plugin_manager.clone(),
                stops: 0.into(),
                revoke,
                replacement: None,
            });
            let state = state.with_extension_status(runtime.clone());
            let result = PluginOperations::from_state(&state)
                .apply_self_update(&who, &input(true), candidate, self_source())
                .await;
            assert_eq!(result.application, Application::Failed, "{result:?}");
            let manager = state.plugin_manager.lock().unwrap();
            let current = &manager.state().plugins[&who.plugin];
            assert_eq!(current.digest, original.digest);
            assert_eq!(current.grants, original.grants);
            assert_eq!(current.active, !revoke);
            assert!(!manager.is_draining(&who.plugin));
        }
    }

    #[tokio::test]
    async fn self_update_does_not_claim_another_versions_ready_instance_as_its_own_success() {
        let (state, _root, who) = fixture(&["install", "lifecycle", "authorize"]);
        let candidate = self_candidate(&state, false);
        let replacement = versioned_package(
            &who.plugin,
            serde_json::to_value(&candidate.manifest().permissions).unwrap(),
            "3.0.0",
        );
        let runtime = Arc::new(SelfUpdateRuntime {
            manager: state.plugin_manager.clone(),
            stops: 0.into(),
            revoke: false,
            replacement: Some(replacement.clone()),
        });
        let state = state.with_extension_status(runtime);
        let result = PluginOperations::from_state(&state)
            .apply_self_update(&who, &input(true), candidate, self_source())
            .await;
        assert_eq!(result.application, Application::Failed, "{result:?}");
        assert!(result.error.unwrap().message.contains("版本已变化"));
        let manager = state.plugin_manager.lock().unwrap();
        assert_eq!(
            manager.state().plugins[&who.plugin].digest,
            replacement.digest()
        );
        assert!(manager.runtime_ready(&who.plugin).unwrap());
    }
}
