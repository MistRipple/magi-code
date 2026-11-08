use super::*;
use crate::plugin_installations::{InstallCandidate, InstallOwner, InstallationWaiter};
use magi_app_server_protocol::generated::{
    PluginBundleInstallApplication as Application, PluginBundleInstallError as InstallError,
    PluginBundleInstallRequest, PluginBundleInstallResult as InstallResult, PluginEcosystem,
    PluginUpstreamRequest,
};

pub(super) fn owner(
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
    }

    pub(super) async fn install_bundle(
        &self,
        owner: InstallOwner,
        input: PluginBundleInstallRequest,
    ) -> Result<InstallResult, ApiError> {
        Self::check_caller(
            &*self.locked("检查安装任务准入")?,
            Some(owner.caller()),
            "install",
        )?;
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
                let prepared = tokio::select! {
                    biased;
                    _ = job.cancelled() => { job.finish(cancelled(&bundle)); return; }
                    result = tokio::time::timeout(Duration::from_secs(840), operations.prepare_bundle(&input)) => result
                        .unwrap_or_else(|_| Err(failure(ApiError::InvalidInput("安装准备超时，尚未提交版本".into()), false, &bundle, None, None))),
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
        let apply = async {
            if id == owner.plugin {
                return Err(ApiError::Conflict(
                    "插件不能在自己的调用中替换自身，请使用 Magi 在线升级入口".into(),
                ));
            }
            let changed = !matches!(
                self.locked("预览在线安装")?
                    .preview(&package, &source)
                    .map_err(plugin_error)?
                    .operation,
                magi_app_server_protocol::generated::PluginCandidateOperation::Unchanged
            );
            self.install_or_upgrade(&package, source, Some(owner.caller()))
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
                Some(package.digest()),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    fn package(id: &str, permissions: Value) -> PluginPackage {
        let manifest = json!({"sdkVersion":2,"id":id,"version":"1.0.0","name":id,"description":"","dataSchemaVersion":1,
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
}
