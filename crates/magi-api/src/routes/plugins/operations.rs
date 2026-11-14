//! HTTP 与扩展宿主共用的插件管理入口。只持有原有事实源与停止端口，
//! 不捕获完整 ApiState；停止端口使用弱引用，避免 daemon/runtime 引用环。
use super::*;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

mod installation;
mod translation;
pub(super) use installation::owner as installation_owner;

#[derive(Clone)]
pub struct PluginOperations {
    manager: Arc<Mutex<PluginManager>>,
    events: Arc<InMemoryEventBus>,
    installations: Arc<crate::plugin_installations::PluginInstallations>,
    extension_status: Option<Weak<dyn crate::ExtensionStatusSource>>,
    pub(super) plugin_toolchain: Option<crate::PluginToolchain>,
    pub(super) processes: magi_process::ManagedProcessGroup,
}

pub(super) type OperationCaller<'a> = (&'a str, &'a str, Option<&'a str>, bool);

#[derive(Clone, Copy, Default)]
pub(super) struct LifecycleChange<'a> {
    expected_digest: Option<&'a str>,
    expected_state: Option<&'a magi_app_server_protocol::generated::PluginLifecycleState>,
    reason: Option<&'a str>,
}

impl<'a> LifecycleChange<'a> {
    pub(super) fn for_digest(expected_digest: Option<&'a str>) -> Self {
        Self {
            expected_digest,
            ..Default::default()
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
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

    fn install(state: &ApiState, package: &PluginPackage, enabled: bool) {
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
        if enabled {
            manager
                .begin_runtime(id, package.digest(), "current")
                .unwrap();
        } else {
            manager.disable(id).unwrap();
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
    async fn management_uses_real_lifecycle_and_rejects_unknown_grant_fields() {
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
                .is_enabled("acme.target")
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
                .is_enabled("acme.target")
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
    async fn management_change_restores_only_its_own_removed_installation_and_rejects_stale_writers()
     {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package_permissions("acme.manager", &["install", "lifecycle", "background"]);
        let readonly = package_permissions("acme.observer", &["background"]);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &readonly, true);
        install(&state, &target, true);
        let original = state.plugin_manager.lock().unwrap().state().plugins["acme.target"].clone();
        let operations = PluginOperations::from_state(&state);
        let request = json!({"pluginId":"acme.target","expectedDigest":target.digest(),"changeId":"request-1"});
        for (id, digest, lease) in [
            ("acme.observer", readonly.digest(), "current"),
            ("acme.manager", caller.digest(), "expired"),
        ] {
            assert!(
                operations
                    .call_background(id, digest, Some(lease), "plugins.remove", request.clone())
                    .await
                    .is_err()
            );
        }
        operations
            .call_background(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.remove",
                request.clone(),
            )
            .await
            .unwrap();
        assert!(
            !state
                .plugin_manager
                .lock()
                .unwrap()
                .state()
                .plugins
                .contains_key("acme.target")
        );
        assert!(
            operations
                .install_or_upgrade(&target, original.source.clone(), None)
                .await
                .is_err()
        );
        assert!(
            operations
                .call_background(
                    "acme.manager",
                    caller.digest(),
                    Some("expired"),
                    "plugins.finishChange",
                    json!({"changeId":"request-1","commit":true})
                )
                .await
                .is_err()
        );
        operations
            .call_background(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.finishChange",
                json!({"changeId":"request-1","commit":false}),
            )
            .await
            .unwrap();
        assert_eq!(
            state.plugin_manager.lock().unwrap().state().plugins["acme.target"],
            original
        );
        operations
            .call_background(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.remove",
                request,
            )
            .await
            .unwrap();
        operations.installation_instance_ended("acme.manager", "current");
        assert_eq!(
            state.plugin_manager.lock().unwrap().state().plugins["acme.target"],
            original,
            "instance end restores pending removals without downloading or changing authorization"
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
            .disable("acme.background")
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
                    manager.lock().unwrap().disable("acme.background").unwrap();
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
            .disable("acme.manager")
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
    async fn reconciliation_uses_the_supervisor_and_preserves_intent_and_instance_identity() {
        use magi_app_server_protocol::generated::{PluginRuntimeState, PluginRuntimeStatus};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        struct Runtime {
            manager: Arc<Mutex<PluginManager>>,
            calls: AtomicUsize,
            failed: AtomicBool,
            revoke: AtomicBool,
        }
        impl crate::ExtensionStatusSource for Runtime {
            fn reconcile(
                &self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
            {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let mut manager = self.manager.lock().unwrap();
                manager.ready_runtime("acme.target", "current").unwrap();
                if self.revoke.load(Ordering::SeqCst) {
                    manager.disable("acme.manager").unwrap();
                }
                Box::pin(async { Ok(()) })
            }
            fn status(&self, _: &str) -> Option<PluginRuntimeStatus> {
                Some(PluginRuntimeStatus {
                    state: if self.failed.load(Ordering::SeqCst) {
                        PluginRuntimeState::Failed
                    } else {
                        PluginRuntimeState::Running
                    },
                    generation: 7,
                    instance_id: None,
                    reason: Some("real startup failure".into()),
                    crashes: 0,
                    blocking: vec![],
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let runtime = Arc::new(Runtime {
            manager: state.plugin_manager.clone(),
            calls: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            revoke: AtomicBool::new(false),
        });
        let state = state.with_extension_status(runtime.clone());
        let caller = package("acme.manager", true);
        let target = package("acme.target", false);
        let disabled = package("acme.disabled", false);
        let unprivileged = package("acme.unprivileged", false);
        for package in [&caller, &target, &unprivileged] {
            install(&state, package, true);
        }
        install(&state, &disabled, false);
        let operations = PluginOperations::from_state(&state);
        let params = json!({"pluginIds":["acme.target", "acme.disabled"]});
        for (id, digest, lease, input) in [
            (
                "acme.unprivileged",
                unprivileged.digest(),
                Some("current"),
                params.clone(),
            ),
            ("acme.manager", caller.digest(), Some("old"), params.clone()),
            ("acme.manager", "old", Some("current"), params.clone()),
            (
                "acme.manager",
                caller.digest(),
                Some("current"),
                json!({"pluginIds":["acme.target", "acme.target"]}),
            ),
            (
                "acme.manager",
                caller.digest(),
                Some("current"),
                json!({"pluginIds":["missing"]}),
            ),
            (
                "acme.manager",
                caller.digest(),
                Some("current"),
                json!({"pluginIds":[], "unknown":true}),
            ),
        ] {
            assert!(
                operations
                    .call(id, digest, lease, "plugins.reconcile", input)
                    .await
                    .is_err()
            );
        }
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        let before = state.plugin_manager.lock().unwrap().state().clone();
        let result = operations
            .call(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.reconcile",
                params.clone(),
            )
            .await
            .unwrap();
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.plugin_manager.lock().unwrap().state(), &before);
        state
            .plugin_manager
            .lock()
            .unwrap()
            .check_runtime("acme.target", "current")
            .unwrap();
        assert_eq!(
            result["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["manifest"]["id"] == "acme.target")
                .unwrap()["runtime"]["generation"],
            7
        );
        runtime.failed.store(true, Ordering::SeqCst);
        assert!(
            operations
                .call(
                    "acme.manager",
                    caller.digest(),
                    Some("current"),
                    "plugins.reconcile",
                    params.clone()
                )
                .await
                .unwrap_err()
                .message()
                .contains("real startup failure")
        );
        runtime.failed.store(false, Ordering::SeqCst);
        runtime.revoke.store(true, Ordering::SeqCst);
        assert!(
            operations
                .call(
                    "acme.manager",
                    caller.digest(),
                    Some("current"),
                    "plugins.reconcile",
                    params
                )
                .await
                .is_err(),
            "收敛期间撤权不能返回成功"
        );
    }

    #[tokio::test]
    async fn unchanged_install_preserves_the_running_instance_and_checks_admission_first() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct RuntimeProbe(Arc<AtomicUsize>);
        impl crate::ExtensionStatusSource for RuntimeProbe {
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
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let stops = Arc::new(AtomicUsize::new(0));
        let state = super::super::tests::bare_state(root.path())
            .with_extension_status(Arc::new(RuntimeProbe(stops.clone())));
        let target = package("acme.target", false);
        let unprivileged = package("acme.unprivileged", false);
        install(&state, &target, true);
        install(&state, &unprivileged, true);
        let operations = PluginOperations::from_state(&state);
        let source = PluginSource::Local {
            name: "acme.target.zip".into(),
        };
        let before = state.plugin_manager.lock().unwrap().state().clone();
        operations
            .install_or_upgrade(&target, source.clone(), None)
            .await
            .unwrap();
        assert_eq!(
            stops.load(Ordering::SeqCst),
            0,
            "同一摘要不能停止正在工作的宿主"
        );
        assert_eq!(state.plugin_manager.lock().unwrap().state(), &before);
        state
            .plugin_manager
            .lock()
            .unwrap()
            .check_runtime("acme.target", "current")
            .unwrap();

        assert!(
            operations
                .install_or_upgrade(
                    &target,
                    source.clone(),
                    Some((
                        "acme.unprivileged",
                        unprivileged.digest(),
                        Some("current"),
                        false
                    ))
                )
                .await
                .is_err()
        );
        let foreign = PluginSource::Upstream {
            ecosystem: "pi".into(),
            package: "foreign".into(),
            version: "1.0.0".into(),
            github: None,
        };
        assert!(
            operations
                .install_or_upgrade(&target, foreign, None)
                .await
                .is_err()
        );
        let drain =
            PluginManager::begin_drain(state.plugin_manager.clone(), "acme.target").unwrap();
        assert!(
            operations
                .install_or_upgrade(&target, source.clone(), None)
                .await
                .is_err()
        );
        drop(drain);
        assert_eq!(stops.load(Ordering::SeqCst), 0, "拒绝的确认也不能打断实例");
        assert_eq!(state.plugin_manager.lock().unwrap().state(), &before);
    }

    #[tokio::test]
    async fn disable_source_survives_reload_and_stale_restore_cannot_override_user_intent() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package("acme.manager", true);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &target, true);
        let operations = PluginOperations::from_state(&state);
        let input = json!({"pluginId":"acme.target","enabled":false,"expectedDigest":target.digest(),
            "expectedState":{"enabled":true},"reason":"paused for diagnosis"});
        let result = operations
            .call(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                input.clone(),
            )
            .await
            .unwrap();
        let disabled = result["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["manifest"]["id"] == "acme.target")
            .unwrap();
        assert_eq!(disabled["enabled"], false);
        assert_eq!(
            disabled["disabledBy"],
            json!({"pluginId":"acme.manager","name":"acme.manager","reason":"paused for diagnosis"})
        );
        let reopened = PluginManager::open(state.plugin_manager.lock().unwrap().root()).unwrap();
        assert_eq!(
            reopened.state().plugins["acme.target"]
                .disabled_by
                .as_ref()
                .unwrap()
                .reason,
            "paused for diagnosis"
        );
        let restore = json!({"pluginId":"acme.target","enabled":true,"expectedDigest":target.digest(),
            "expectedState":{"enabled":false,"disabledBy":disabled["disabledBy"]}});
        operations
            .call(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                restore.clone(),
            )
            .await
            .unwrap();
        assert!(
            state.plugin_manager.lock().unwrap().state().plugins["acme.target"]
                .disabled_by
                .is_none()
        );
        operations
            .call(
                "acme.manager",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                input,
            )
            .await
            .unwrap();
        operations
            .lifecycle("disable", "acme.target", None, Default::default())
            .await
            .unwrap();
        assert!(
            operations
                .call(
                    "acme.manager",
                    caller.digest(),
                    Some("current"),
                    "plugins.setEnabled",
                    restore
                )
                .await
                .unwrap_err()
                .message()
                .contains("来源已变化")
        );
        let manager = state.plugin_manager.lock().unwrap();
        assert!(!manager.state().plugins["acme.target"].enabled);
        assert!(manager.state().plugins["acme.target"].disabled_by.is_none());
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
                    instance_id: None,
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
    async fn delegated_lifecycle_requires_its_own_grant_and_exact_target_version() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::tests::bare_state(root.path());
        let caller = package_permissions("acme.delegate", &["install", "lifecycle"]);
        let installer = package_permissions("acme.installer", &["install"]);
        let target = package("acme.target", false);
        install(&state, &caller, true);
        install(&state, &installer, true);
        install(&state, &target, false);
        let operations = PluginOperations::from_state(&state);
        assert!(
            operations
                .call(
                    "acme.installer",
                    installer.digest(),
                    Some("current"),
                    "plugins.setEnabled",
                    json!({"pluginId":"acme.target", "enabled":true})
                )
                .await
                .unwrap_err()
                .message()
                .contains("lifecycle")
        );
        let error = operations
            .call(
                "acme.delegate",
                caller.digest(),
                Some("current"),
                "plugins.setEnabled",
                json!({"pluginId":"acme.target", "enabled":true, "expectedDigest":"old"}),
            )
            .await
            .unwrap_err();
        assert!(error.message().contains("版本"));
        assert!(
            !state
                .plugin_manager
                .lock()
                .unwrap()
                .is_enabled("acme.target")
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
                .is_enabled("acme.target")
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
                .is_enabled("acme.target")
                .unwrap(),
            "过期 profile 提案不能停止新版实例"
        );
    }
}

impl PluginOperations {
    pub fn from_state(state: &ApiState) -> Self {
        Self {
            manager: Arc::clone(&state.plugin_manager),
            events: Arc::clone(&state.event_bus),
            installations: Arc::clone(&state.plugin_installations),
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

    fn change_scope(
        caller: OperationCaller<'_>,
        change_id: String,
    ) -> Result<magi_plugin_system::PluginChangeScope, ApiError> {
        let scope = magi_plugin_system::PluginChangeScope {
            plugin: caller.0.into(),
            digest: caller.1.into(),
            instance: caller
                .2
                .ok_or_else(|| ApiError::Forbidden("管理恢复需要活动宿主实例".into()))?
                .into(),
            change_id,
        };
        scope.validate().map_err(plugin_error)?;
        Ok(scope)
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
        self.install_or_upgrade_change(package, source, caller, None)
            .await
    }

    async fn install_or_upgrade_change(
        &self,
        package: &PluginPackage,
        source: PluginSource,
        caller: Option<OperationCaller<'_>>,
        scope: Option<&magi_plugin_system::PluginChangeScope>,
    ) -> Result<(), ApiError> {
        {
            let mut manager = self.locked("检查待恢复移除")?;
            if manager
                .removed_for_change(&package.manifest().id, scope)
                .map_err(plugin_error)?
            {
                Self::check_caller(&manager, caller, "install")?;
                self.check_upstream_compatibility(&manager, package, &source)?;
                manager
                    .replace_removed(package, source, scope.unwrap())
                    .map_err(plugin_error)?;
                drop(manager);
                self.changed();
                return Ok(());
            }
        }
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
        {
            let manager = self.locked("检查插件更新")?;
            Self::check_caller(&manager, caller, "install")?;
            manager
                .ensure_editing_admitted(&package.manifest().id)
                .map_err(plugin_error)?;
            self.check_upstream_compatibility(&manager, package, &source)?;
            // 复用预览的来源及摘要判断，在关闭宿主准入前结束无需变更的确认。
            // 运行中的工具、后台进程和客户端不应因重复安装同一包而被回收。
            if matches!(
                manager
                    .preview(package, &source)
                    .map_err(plugin_error)?
                    .operation,
                magi_app_server_protocol::generated::PluginCandidateOperation::Unchanged
            ) {
                return Ok(());
            }
        }
        let drain = PluginManager::begin_drain(Arc::clone(&self.manager), &package.manifest().id)
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
        match source {
            PluginSource::Upstream { ecosystem, .. } if ecosystem == "dsh" => {
                let (version, runtime) = self.dsh_runtime()?;
                manager
                    .ensure_dsh_candidate_compatible(package, source, &version, &runtime)
                    .map_err(plugin_error)
            }
            // 没有准入校验器的上游生态不能安装；新增生态必须同时提供自己的校验。
            PluginSource::Upstream { ecosystem, .. } => Err(ApiError::InvalidInput(format!(
                "上游生态 {ecosystem} 没有可用的兼容准入校验"
            ))),
            _ => Ok(()),
        }
    }

    /// 写操作只能由获授权的已启用宿主调用。
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
            || !manager.is_enabled(caller).map_err(plugin_error)?
        {
            return Err(ApiError::Forbidden(
                "插件管理调用的版本或启用状态已失效".into(),
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
        } else if matches!(
            method,
            "plugins.preview"
                | "plugins.install"
                | "plugins.installBundle"
                | "plugins.cancelInstall"
        ) {
            "install"
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
            "plugins.installBundle" => {
                let input = serde_json::from_value(params)
                    .map_err(|e| ApiError::InvalidInput(format!("安装参数无效：{e}")))?;
                let owner = installation::owner(caller, digest, lease, background)?;
                serde_json::to_value(self.install_bundle(owner, input).await?)
                    .map_err(|e| ApiError::internal_assembly("编码安装结果失败", e))
            }
            "plugins.cancelInstall" => {
                let input: magi_app_server_protocol::generated::PluginBundleInstallCancelRequest =
                    serde_json::from_value(params)
                        .map_err(|e| ApiError::InvalidInput(format!("取消参数无效：{e}")))?;
                let owner = installation::owner(caller, digest, lease, background)?;
                serde_json::to_value(self.installations.cancel(&owner, &input.request_id).await?)
                    .map_err(|e| ApiError::internal_assembly("编码安装取消结果失败", e))
            }
            "plugins.finishChange" => {
                let input: magi_app_server_protocol::generated::PluginManagementChangeRequest =
                    serde_json::from_value(params).map_err(|e| {
                        ApiError::InvalidInput(format!("管理恢复结算参数无效：{e}"))
                    })?;
                let identity = (caller, digest, lease, background);
                let scope = Self::change_scope(identity, input.change_id)?;
                let restored = {
                    let mut manager = self.locked("结算管理移除恢复")?;
                    Self::check_caller(&manager, Some(identity), "lifecycle")?;
                    manager
                        .finish_change(&scope, input.commit)
                        .map_err(plugin_error)?
                };
                self.changed();
                for id in &restored {
                    if self
                        .locked("读取恢复启用意愿")?
                        .is_enabled(id)
                        .map_err(plugin_error)?
                    {
                        self.wait_running(id, Some(identity), None).await?;
                    }
                }
                serde_json::to_value(
                    magi_app_server_protocol::generated::PluginManagementChangeReceipt { restored },
                )
                .map_err(|error| ApiError::internal_assembly("编码管理恢复回执失败", error))
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
            "plugins.reconcile" => {
                let input: magi_app_server_protocol::generated::PluginReconcileRequest =
                    serde_json::from_value(params)
                        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
                let ids = &input.plugin_ids;
                if ids.len() > 512
                    || ids.iter().any(|id| id.is_empty() || id.len() > 128)
                    || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
                {
                    return Err(ApiError::InvalidInput("收敛插件身份无效或重复".into()));
                }
                let identity = Some((caller, digest, lease, background));
                let expected = {
                    let manager = self.locked("读取运行目录收敛目标")?;
                    ids.iter()
                        .map(|id| {
                            manager.ensure_editing_admitted(id).map_err(plugin_error)?;
                            Ok((
                                id.clone(),
                                manager
                                    .package(id)
                                    .map_err(plugin_error)?
                                    .digest()
                                    .to_owned(),
                                manager.is_enabled(id).map_err(plugin_error)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, ApiError>>()?
                };
                let source = self
                    .extension_status
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .ok_or_else(|| ApiError::Conflict("扩展监管器不可用".into()))?;
                tokio::time::timeout(Duration::from_secs(110), async {
                    source.reconcile().await.map_err(|reason| {
                        ApiError::Conflict(format!("运行目录收敛失败：{reason}"))
                    })?;
                    // 所有目标共用同一启动预算，复用启用入口的就绪判定和调用者准入。
                    futures_util::future::try_join_all(
                        expected
                            .iter()
                            .filter(|(_, _, active)| *active)
                            .map(|(id, digest, _)| self.wait_running(id, identity, Some(digest))),
                    )
                    .await?;
                    Ok::<(), ApiError>(())
                })
                .await
                .map_err(|_| {
                    ApiError::Conflict(
                        "运行目录收敛尚未确认，请在 Magi 插件管理查看运行状态".into(),
                    )
                })??;
                {
                    let manager = self.locked("核对运行目录收敛结果")?;
                    Self::check_caller(&manager, identity, "lifecycle")?;
                    for (id, digest, active) in &expected {
                        manager.ensure_editing_admitted(id).map_err(plugin_error)?;
                        Self::check_target(&manager, id, Some(digest))?;
                        if manager.is_enabled(id).map_err(plugin_error)? != *active {
                            return Err(ApiError::Conflict(
                                "收敛期间目标启用状态已变化，请刷新后重试".into(),
                            ));
                        }
                    }
                }
                self.projection()
            }
            "plugins.setEnabled" | "plugins.remove" => {
                use magi_app_server_protocol::generated::PluginSetEnabledRequest;
                let remove = method == "plugins.remove";
                let mut change_scope = None;
                let input = if remove {
                    let target: magi_app_server_protocol::generated::PluginRemoveRequest =
                        serde_json::from_value(params)
                            .map_err(|e| ApiError::InvalidInput(format!("移除参数无效：{e}")))?;
                    if let Some(change_id) = target.change_id {
                        change_scope = Some(Self::change_scope(
                            (caller, digest, lease, background),
                            change_id,
                        )?);
                    }
                    PluginSetEnabledRequest {
                        plugin_id: target.plugin_id,
                        expected_digest: target.expected_digest,
                        enabled: false,
                        expected_state: None,
                        reason: None,
                    }
                } else {
                    serde_json::from_value::<PluginSetEnabledRequest>(params)
                        .map_err(|e| ApiError::InvalidInput(format!("启停参数无效：{e}")))?
                };
                if input.plugin_id == caller {
                    return Err(ApiError::Conflict(
                        "插件不能在自己的调用中停止、移除或重启自身，请使用 Magi 插件管理入口"
                            .into(),
                    ));
                }
                if let Some(reason) = &input.reason
                    && (method != "plugins.setEnabled"
                        || input.enabled
                        || reason.trim().is_empty()
                        || reason.len() > 4096)
                {
                    return Err(ApiError::InvalidInput(
                        "停用原因仅可用于停用操作，须为 1–4096 字节".into(),
                    ));
                }
                if let Some(scope) = change_scope {
                    let identity = Some((caller, digest, lease, background));
                    {
                        let manager = self.locked("检查可恢复移除")?;
                        Self::check_caller(&manager, identity, "lifecycle")?;
                        Self::check_target(
                            &manager,
                            &input.plugin_id,
                            input.expected_digest.as_deref(),
                        )?;
                    }
                    let drain =
                        PluginManager::begin_drain(Arc::clone(&self.manager), &input.plugin_id)
                            .map_err(plugin_error)?;
                    self.quiesce(&input.plugin_id).await?;
                    let result = {
                        let mut manager = self.locked("提交可恢复移除")?;
                        Self::check_caller(&manager, identity, "lifecycle")?;
                        Self::check_target(
                            &manager,
                            &input.plugin_id,
                            input.expected_digest.as_deref(),
                        )?;
                        let result = manager
                            .remove_for_change(&input.plugin_id, scope, &drain)
                            .map_err(plugin_error);
                        if result.is_ok() {
                            drain.commit(&mut manager);
                        }
                        result
                    };
                    self.changed();
                    result?;
                    return self.projection();
                }
                self.manage_target(
                    &input.plugin_id,
                    input.enabled,
                    remove,
                    Some((caller, digest, lease, background)),
                    LifecycleChange {
                        expected_digest: input.expected_digest.as_deref(),
                        expected_state: input.expected_state.as_ref(),
                        reason: input.reason.as_deref(),
                    },
                )
                .await?;
                self.projection()
            }
            _ => Err(ApiError::InvalidInput("未知插件管理操作".into())),
        }
    }

    async fn manage_target(
        &self,
        id: &str,
        enabled: bool,
        remove: bool,
        caller: Option<OperationCaller<'_>>,
        change: LifecycleChange<'_>,
    ) -> Result<(), ApiError> {
        if remove {
            self.lifecycle("uninstall", id, caller, change).await?;
        } else if !enabled {
            self.lifecycle("disable", id, caller, change).await?;
        } else {
            self.lifecycle("enable", id, caller, change).await?;
            self.wait_running(id, caller, change.expected_digest)
                .await?;
        }
        Ok(())
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
        change: LifecycleChange<'_>,
    ) -> Result<(), ApiError> {
        {
            let manager = self.locked("检查插件管理准入")?;
            Self::check_caller(&manager, caller, "lifecycle")?;
            Self::check_lifecycle_target(&manager, id, change)?;
        }
        if action == "disable" || action == "uninstall" {
            let drain =
                PluginManager::begin_drain(Arc::clone(&self.manager), id).map_err(plugin_error)?;
            self.quiesce(id).await?;
            let result = {
                let mut manager = self.locked("停止插件")?;
                Self::check_lifecycle_target(&manager, id, change)?;
                Self::check_caller(&manager, caller, "lifecycle")?;
                let result = if action == "disable" {
                    let origin =
                        if let (Some(reason), Some((owner, _, _, _))) = (change.reason, caller) {
                            Some(magi_app_server_protocol::generated::PluginDisableInfo {
                                plugin_id: owner.into(),
                                name: manager
                                    .package(owner)
                                    .map_err(plugin_error)?
                                    .manifest()
                                    .name
                                    .clone(),
                                reason: reason.into(),
                            })
                        } else {
                            None
                        };
                    manager.disable_drained(id, origin, &drain)
                } else {
                    manager.uninstall(id, &drain)
                }
                .map_err(plugin_error);
                if result.is_ok() {
                    drain.commit(&mut manager);
                }
                result
            };
            self.changed();
            return result;
        }
        let result = {
            let mut manager = self.locked("启用插件")?;
            Self::check_lifecycle_target(&manager, id, change)?;
            Self::check_caller(&manager, caller, "lifecycle")?;
            match action {
                "enable" => manager.enable(id),
                _ => return Err(ApiError::InvalidInput("未知生命周期操作".into())),
            }
            .map_err(plugin_error)
        };
        self.changed();
        result
    }

    fn check_target(
        manager: &PluginManager,
        id: &str,
        expected_digest: Option<&str>,
    ) -> Result<(), ApiError> {
        if let Some(expected) = expected_digest
            && manager.package(id).map_err(plugin_error)?.digest() != expected
        {
            return Err(ApiError::Conflict(
                "目标插件版本已变化，请刷新后重试".into(),
            ));
        }
        Ok(())
    }

    fn check_lifecycle_target(
        manager: &PluginManager,
        id: &str,
        change: LifecycleChange<'_>,
    ) -> Result<(), ApiError> {
        Self::check_target(manager, id, change.expected_digest)?;
        if let Some(expected) = change.expected_state {
            if change.expected_digest.is_none() {
                return Err(ApiError::InvalidInput(
                    "启停状态确认必须绑定插件摘要".into(),
                ));
            }
            let installed = manager
                .state()
                .plugins
                .get(id)
                .ok_or_else(|| ApiError::Conflict("目标插件未安装".into()))?;
            if installed.enabled != expected.enabled
                || installed.disabled_by != expected.disabled_by
            {
                return Err(ApiError::Conflict(
                    "插件启停状态或停用来源已变化，请刷新后重试".into(),
                ));
            }
        }
        Ok(())
    }

    /// HTTP 与宿主目录共用安装归档、当前运行组件和监管器的只读投影。
    pub fn decorate_runtime(
        &self,
        manager: &PluginManager,
        list: &mut magi_app_server_protocol::PluginList,
    ) -> Result<(), ApiError> {
        let current = self.translation_runtime_id();
        for plugin in &mut list.plugins {
            if plugin.source.kind == magi_app_server_protocol::generated::PluginSourceKind::Upstream
            {
                let package = manager.package(&plugin.manifest.id).map_err(plugin_error)?;
                plugin.translation_runtime =
                    self.translation_runtime_status(&package, current.as_deref());
            }
        }
        if let Some(source) = self.extension_status.as_ref().and_then(Weak::upgrade) {
            for plugin in &mut list.plugins {
                plugin.runtime = source.status(&plugin.manifest.id);
            }
        }
        Ok(())
    }

    fn projection(&self) -> Result<Value, ApiError> {
        let manager = self.locked("读取插件结果")?;
        let mut list = manager.projection().map_err(plugin_error)?;
        self.decorate_runtime(&manager, &mut list)?;
        serde_json::to_value(list).map_err(|e| ApiError::internal_assembly("编码插件目录失败", e))
    }

    async fn wait_running(
        &self,
        id: &str,
        caller: Option<OperationCaller<'_>>,
        expected_digest: Option<&str>,
    ) -> Result<(), ApiError> {
        use magi_app_server_protocol::generated::PluginRuntimeState;
        let package = self
            .locked("读取插件入口")?
            .package(id)
            .map_err(plugin_error)?;
        let expected_digest = expected_digest.unwrap_or(package.digest());
        if package.digest() != expected_digest {
            return Err(ApiError::Conflict("等待启动的插件版本已变化".into()));
        }
        if package.manifest().extension.is_none() {
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
                let status = source.status(id);
                {
                    let manager = self.locked("检查启用结果")?;
                    Self::check_caller(&manager, caller, "lifecycle")?;
                    if manager.package(id).map_err(plugin_error)?.digest() != expected_digest {
                        return Err(ApiError::Conflict("等待启动的插件版本已变化".into()));
                    }
                    if !manager.is_enabled(id).map_err(plugin_error)? {
                        return Err(ApiError::Conflict("目标插件已被停用或撤权".into()));
                    }
                    if let Some(status) = status {
                        match status.state {
                            PluginRuntimeState::Running => {
                                if manager.runtime_ready(id).map_err(plugin_error)? {
                                    return Ok(());
                                }
                            }
                            PluginRuntimeState::Failed => {
                                return Err(ApiError::Conflict(format!(
                                    "插件启用失败：{}",
                                    status.reason.unwrap_or_default()
                                )));
                            }
                            _ => {}
                        }
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
