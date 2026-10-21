use magi_app_server_protocol::generated::PluginDrainPhase;
use magi_plugin_system::{
    PluginError, PluginManager, PluginPackage, PluginPermission, PluginPermissionKind,
    PluginResourceStore, PluginScopeKind, PluginSource,
};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Write},
    sync::{Arc, Mutex},
};
use tempfile::tempdir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn manifest(with_permission: bool) -> Value {
    json!({
        "sdkVersion":1,"id":"acme.lifecycle","version":"1.0.0","name":"Lifecycle","description":"",
        "backend":"plugin.mjs",        "permissions": if with_permission { json!([{"kind":"storage","scope":"workspace","targets":["cache"]}]) } else { json!([]) },
        "dataSchemaVersion":1,"settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"commands":[{"id":"open","title":"Open","description":""}]}
    })
}

fn package(with_permission: bool) -> PluginPackage {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        (
            "manifest.json",
            serde_json::to_vec(&manifest(with_permission)).unwrap(),
        ),
        ("plugin.mjs", b"export default input".to_vec()),
    ] {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

#[test]
fn install_enable_authorize_activate_and_reopen_use_one_immutable_package() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(true);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "dev.zip".into(),
            },
        )
        .unwrap();
    assert!(manager.activate("acme.lifecycle", "application").is_err());
    manager.enable("acme.lifecycle", "application").unwrap();
    assert!(manager.activate("acme.lifecycle", "application").is_err());
    manager
        .authorize(
            "acme.lifecycle",
            "application",
            vec![magi_plugin_system::PluginPermission {
                kind: magi_plugin_system::PluginPermissionKind::Storage,
                scope: magi_plugin_system::PluginScopeKind::Workspace,
                targets: vec!["cache".into()],
            }],
        )
        .unwrap();
    let active = manager.activate("acme.lifecycle", "application").unwrap();
    assert_eq!(active.digest, package.digest());
    assert_eq!(manager.active("application").len(), 1);
    assert!(manager.disable("acme.lifecycle", "application").is_err());
    manager.deactivate("acme.lifecycle", "application").unwrap();
    manager.disable("acme.lifecycle", "application").unwrap();
    let reopened = PluginManager::open(root.path()).unwrap();
    assert_eq!(
        reopened.package("acme.lifecycle").unwrap().digest(),
        package.digest()
    );
    reopened.state();
}

#[test]
fn reserved_first_party_identity_cannot_be_claimed_by_external_packages() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let mut manifest = manifest(false);
    manifest["id"] = json!("openai.chatgpt-web");
    let package = package_from_manifest(manifest);
    assert!(
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "spoof.zip".into(),
                },
            )
            .is_err()
    );
    manager
        .install(
            &package,
            PluginSource::Builtin {
                name: "openai.chatgpt-web".into(),
            },
        )
        .unwrap();
}

#[test]
fn lifecycle_rejects_out_of_manifest_grants_and_uninstall_with_enabled_scope() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Address {
                url: "https://example.invalid/p.zip".into(),
            },
        )
        .unwrap();
    let grant = magi_plugin_system::PluginPermission {
        kind: magi_plugin_system::PluginPermissionKind::Network,
        scope: magi_plugin_system::PluginScopeKind::Workspace,
        targets: vec!["example.invalid".into()],
    };
    assert!(
        manager
            .authorize("acme.lifecycle", "application", vec![grant])
            .is_err()
    );
    manager.enable("acme.lifecycle", "application").unwrap();
    assert!(manager.uninstall("acme.lifecycle").is_err());
    manager.disable("acme.lifecycle", "application").unwrap();
    manager.uninstall("acme.lifecycle").unwrap();
    assert!(manager.package("acme.lifecycle").is_err());
}

fn upgrade_with_drain(
    manager: &Arc<Mutex<PluginManager>>,
    package: &PluginPackage,
    name: &str,
) -> Result<(), PluginError> {
    let drain =
        PluginManager::begin_upgrade_drain(Arc::clone(manager), package.manifest().id.as_str())?;
    let result = manager.lock().unwrap().upgrade(
        package,
        PluginSource::Local { name: name.into() },
        &drain,
        None,
    );
    drop(drain);
    result
}

fn version_two() -> PluginPackage {
    let mut second_manifest = manifest(true);
    second_manifest["version"] = json!("1.1.0");
    package_from_manifest(second_manifest)
}

fn grant() -> PluginPermission {
    PluginPermission {
        kind: PluginPermissionKind::Storage,
        scope: PluginScopeKind::Workspace,
        targets: vec!["cache".into()],
    }
}

#[test]
fn upgrade_drains_the_plugin_then_keeps_enablement_but_not_activation_or_grants() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let first = package(true);
    manager
        .install(
            &first,
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let second = version_two();

    // 停用排空不能充当整插件升级的准入关闭证明。
    let single =
        PluginManager::begin_deactivation(Arc::clone(&manager), "acme.lifecycle", "application")
            .unwrap();
    assert!(
        manager
            .lock()
            .unwrap()
            .upgrade(
                &second,
                PluginSource::Local {
                    name: "second.zip".into()
                },
                &single,
                None
            )
            .is_err()
    );
    drop(single);

    upgrade_with_drain(&manager, &second, "second.zip").unwrap();
    let manager = manager.lock().unwrap();
    assert_eq!(manager.state().plugins.len(), 1);
    assert_eq!(
        manager
            .package("acme.lifecycle")
            .unwrap()
            .manifest()
            .version,
        "1.1.0"
    );
    let installed = &manager.state().plugins["acme.lifecycle"];
    assert_eq!(installed.enabled_scopes.len(), 1, "启用意愿跨升级保留");
    assert!(installed.active_scopes.is_empty(), "新包必须重新激活");
    assert!(installed.grants.is_empty(), "授权绑定摘要，不跨版本继承");
    assert!(
        !manager
            .is_active_for_scope("acme.lifecycle", "application")
            .unwrap()
    );
    assert!(
        manager.projection().unwrap().plugins[0].draining.is_empty(),
        "升级提交后不能残留排空标记"
    );
}

#[test]
fn upgrade_is_refused_while_runs_are_in_flight_and_changes_nothing() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let first = package(true);
    manager
        .install(
            &first,
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.lifecycle",
        first.digest(),
        "application",
        &magi_plugin_system::RunCancellation::default(),
    )
    .unwrap();

    let error = upgrade_with_drain(&manager, &version_two(), "second.zip").unwrap_err();
    assert!(error.to_string().contains("在途运行"), "{error}");
    {
        let guard = manager.lock().unwrap();
        let installed = &guard.state().plugins["acme.lifecycle"];
        assert_eq!(installed.version, "1.0.0");
        assert!(installed.active_scopes.contains("application"));
    }
    drop(lease);
    upgrade_with_drain(&manager, &version_two(), "second.zip").unwrap();
}

#[test]
fn data_schema_change_is_refused_only_when_stored_data_would_need_conversion() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let with_resource = |mut value: Value| {
        value["contributions"]["resources"] =
            json!([{"id":"cache","title":"Cache","description":""}]);
        value
    };
    manager
        .install(
            &package_from_manifest(with_resource(manifest(true))),
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    let mut changed = with_resource(manifest(true));
    changed["version"] = json!("2.0.0");
    changed["dataSchemaVersion"] = json!(2);
    let next = package_from_manifest(changed);

    // 有持久数据（这里是设置）时，需要转换器，升级被拒绝且版本不变。
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    manager
        .write_resource("acme.lifecycle", "application", "cache", 0, json!({"k": 1}))
        .unwrap();
    manager.deactivate("acme.lifecycle", "application").unwrap();
    manager.disable("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let error = upgrade_with_drain(&manager, &next, "next.zip").unwrap_err();
    assert!(error.to_string().contains("数据"), "{error}");
    assert_eq!(
        manager.lock().unwrap().state().plugins["acme.lifecycle"].version,
        "1.0.0"
    );

    // 没有任何数据时无需转换，可以直接切换到新的数据格式版本。
    let fresh_root = tempdir().unwrap();
    let mut fresh = PluginManager::open(fresh_root.path()).unwrap();
    fresh
        .install(
            &package_from_manifest(with_resource(manifest(true))),
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    let fresh = Arc::new(Mutex::new(fresh));
    upgrade_with_drain(&fresh, &next, "next.zip").unwrap();
    assert_eq!(
        fresh
            .lock()
            .unwrap()
            .package("acme.lifecycle")
            .unwrap()
            .manifest()
            .data_schema_version,
        2
    );
}

#[test]
fn upgrade_drain_blocks_activation_until_it_is_released() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let drain = PluginManager::begin_upgrade_drain(Arc::clone(&manager), "acme.lifecycle").unwrap();
    {
        let mut guard = manager.lock().unwrap();
        assert!(guard.activate("acme.lifecycle", "application").is_err());
        assert!(guard.enable("acme.lifecycle", "application").is_err());
        let projected = &guard.projection().unwrap().plugins[0].draining;
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].phase, PluginDrainPhase::Draining);
        assert_eq!(
            projected[0].deadline_ms - projected[0].started_at_ms,
            magi_plugin_system::PLUGIN_DRAIN_TIMEOUT.as_millis() as u64
        );
    }
    assert!(
        PluginManager::begin_upgrade_drain(Arc::clone(&manager), "acme.lifecycle").is_err(),
        "同一插件不能同时存在两个排空操作"
    );
    drop(drain);
    manager
        .lock()
        .unwrap()
        .enable("acme.lifecycle", "application")
        .unwrap();
}

#[tokio::test]
async fn settle_cancels_in_flight_runs_after_the_drain_period_and_waits_for_settlement() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(false);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "settle.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let cancellation = magi_plugin_system::RunCancellation::default();
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.lifecycle",
        package.digest(),
        "application",
        &cancellation,
    )
    .unwrap();
    let drain =
        PluginManager::begin_deactivation(Arc::clone(&manager), "acme.lifecycle", "application")
            .unwrap();

    // 调用在取消后仍不结算：返回冲突，租约和激活状态保持原样。
    let error = PluginManager::settle_runtime_leases_within(
        &manager,
        &drain,
        std::time::Duration::from_millis(30),
        std::time::Duration::from_millis(30),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("尚未结算"), "{error}");
    assert!(cancellation.is_cancelled(), "排空到期必须取消在途调用");

    // 取消后调用自行结算，则排空成功。
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        drop(lease);
    });
    PluginManager::settle_runtime_leases_within(
        &manager,
        &drain,
        std::time::Duration::from_millis(10),
        std::time::Duration::from_secs(2),
    )
    .await
    .unwrap();
    releaser.join().unwrap();
}

#[test]
fn installed_packages_are_parsed_once_and_replaced_by_digest() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let first = package(false);
    manager
        .install(
            &first,
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    let a = manager.package("acme.lifecycle").unwrap();
    let b = manager.package("acme.lifecycle").unwrap();
    assert!(Arc::ptr_eq(&a, &b), "同一摘要必须复用已解析的包");
    let projection = manager.projection().unwrap();
    assert_eq!(
        projection.plugins[0].source.kind,
        magi_app_server_protocol::generated::PluginSourceKind::Local
    );
    assert_eq!(projection.plugins[0].source.location, "first.zip");

    let manager = Arc::new(Mutex::new(manager));
    let mut second_manifest = manifest(false);
    second_manifest["version"] = json!("1.1.0");
    upgrade_with_drain(
        &manager,
        &package_from_manifest(second_manifest),
        "second.zip",
    )
    .unwrap();
    let c = manager.lock().unwrap().package("acme.lifecycle").unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(c.manifest().version, "1.1.0");
}

#[test]
fn reopening_cleans_unreferenced_package_versions_after_lifecycle_commit() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let first = package(false);
    manager
        .install(
            &first,
            PluginSource::Local {
                name: "first.zip".into(),
            },
        )
        .unwrap();
    let old_path = root
        .path()
        .join("versions/acme.lifecycle")
        .join(format!("{}.zip", first.digest()));
    let mut second_manifest = manifest(false);
    second_manifest["version"] = json!("1.1.0");
    let manager = Arc::new(Mutex::new(manager));
    upgrade_with_drain(
        &manager,
        &package_from_manifest(second_manifest),
        "second.zip",
    )
    .unwrap();
    assert!(!old_path.exists());
    let reopened = PluginManager::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .package("acme.lifecycle")
            .unwrap()
            .manifest()
            .version,
        "1.1.0",
        "升级和启动清理都必须保留当前安装包"
    );
}

#[test]
fn active_manifests_follow_the_global_activation_state() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Local {
                name: "global.zip".into(),
            },
        )
        .unwrap();
    assert!(
        manager
            .manifests_for_scope("application")
            .unwrap()
            .is_empty()
    );
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    assert_eq!(manager.manifests_for_scope("application").unwrap().len(), 1);
    manager.deactivate("acme.lifecycle", "application").unwrap();
    assert!(
        manager
            .manifests_for_scope("application")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn plugins_are_global_and_reject_workspace_scope_keys() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Local {
                name: "global.zip".into(),
            },
        )
        .unwrap();
    assert!(manager.enable("acme.lifecycle", "workspace:one").is_err());
    assert!(manager.manifests_for_scope("workspace:one").is_err());
    assert!(
        manager
            .is_active_for_scope("acme.lifecycle", "workspace:one")
            .is_err()
    );
}

#[test]
fn active_commands_follow_the_global_activation_state() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Local {
                name: "commands.zip".into(),
            },
        )
        .unwrap();
    assert!(
        manager
            .command_contributions_for_scope("application")
            .unwrap()
            .is_empty()
    );
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let commands = manager
        .command_contributions_for_scope("application")
        .unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].0.id, "acme.lifecycle");
    assert_eq!(commands[0].1.id, "open");
}

#[test]
fn runtime_identity_rejects_stale_package_and_foreign_scope() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(true);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "runtime-identity.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();

    assert!(
        manager
            .validate_runtime_identity(
                "acme.lifecycle",
                package.digest(),
                "application",
                "application",
            )
            .is_ok()
    );
    assert!(
        manager
            .validate_runtime_identity(
                "acme.lifecycle",
                "stale-digest",
                "application",
                "application",
            )
            .is_err()
    );
    assert!(
        manager
            .validate_runtime_identity(
                "acme.lifecycle",
                package.digest(),
                "application",
                "workspace:other",
            )
            .is_err()
    );
}

#[test]
fn runtime_lease_blocks_deactivation_until_worker_is_settled() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(false);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "runtime-lease.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();

    let manager = Arc::new(Mutex::new(manager));
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.lifecycle",
        package.digest(),
        "application",
        &magi_plugin_system::RunCancellation::default(),
    )
    .unwrap();
    assert!(
        manager
            .lock()
            .unwrap()
            .deactivate("acme.lifecycle", "application")
            .is_err()
    );
    drop(lease);
    manager
        .lock()
        .unwrap()
        .deactivate("acme.lifecycle", "application")
        .unwrap();
}

#[test]
fn deactivation_drain_rejects_new_runtime_leases_and_cleans_up_on_abort() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(false);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "drain.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));

    let guard =
        PluginManager::begin_deactivation(Arc::clone(&manager), "acme.lifecycle", "application")
            .unwrap();
    assert!(
        magi_plugin_system::PluginRuntimeLease::acquire(
            Arc::clone(&manager),
            "acme.lifecycle",
            package.digest(),
            "application",
            &magi_plugin_system::RunCancellation::default(),
        )
        .is_err()
    );
    drop(guard);
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.lifecycle",
        package.digest(),
        "application",
        &magi_plugin_system::RunCancellation::default(),
    )
    .unwrap();
    drop(lease);
}

#[test]
fn deactivation_drain_fences_admission_and_counts_leases() {
    let root = tempdir().unwrap();
    let package = package_from_manifest(json!({
        "sdkVersion":1,"id":"acme.application","version":"1.0.0","name":"Application","description":"",
        "backend":"plugin.mjs","permissions":[],
        "dataSchemaVersion":1,"settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"commands":[{"id":"open","title":"Open","description":""}]}
    }));
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "application.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.application", "application").unwrap();
    manager.activate("acme.application", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let guard =
        PluginManager::begin_deactivation(Arc::clone(&manager), "acme.application", "application")
            .unwrap();
    assert!(
        manager
            .lock()
            .unwrap()
            .enable("acme.application", "application")
            .is_err()
    );
    assert!(
        magi_plugin_system::PluginRuntimeLease::acquire(
            Arc::clone(&manager),
            "acme.application",
            package.digest(),
            "application",
            &magi_plugin_system::RunCancellation::default(),
        )
        .is_err()
    );
    drop(guard);
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.application",
        package.digest(),
        "application",
        &magi_plugin_system::RunCancellation::default(),
    )
    .unwrap();
    assert_eq!(
        manager
            .lock()
            .unwrap()
            .runtime_lease_count_for_deactivation("acme.application", "application"),
        1
    );
    drop(lease);
}

#[test]
fn drain_expiry_cancels_in_flight_runs_then_deactivation_waits_for_settlement() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(false);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "cancel.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let run = magi_plugin_system::RunCancellation::default();
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.lifecycle",
        package.digest(),
        "application",
        &run,
    )
    .unwrap();

    let notified = manager
        .lock()
        .unwrap()
        .cancel_runtime_leases_for_deactivation("acme.lifecycle", "application");
    assert_eq!(notified, 1);
    assert!(run.is_cancelled());
    // 取消只是通知；调用结算释放租约前，停用仍被拒绝。
    assert!(
        manager
            .lock()
            .unwrap()
            .deactivate("acme.lifecycle", "application")
            .is_err()
    );
    drop(lease);
    manager
        .lock()
        .unwrap()
        .deactivate("acme.lifecycle", "application")
        .unwrap();
}

#[test]
fn resource_permission_is_required_for_each_declared_resource() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(true);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "resource-permission.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize(
            "acme.lifecycle",
            "application",
            vec![magi_plugin_system::PluginPermission {
                kind: magi_plugin_system::PluginPermissionKind::Storage,
                scope: magi_plugin_system::PluginScopeKind::Workspace,
                targets: vec!["cache".into()],
            }],
        )
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    assert!(
        manager
            .permission_allowed(
                "acme.lifecycle",
                "application",
                magi_plugin_system::PluginPermissionKind::Storage,
                "cache",
            )
            .unwrap()
    );
    assert!(
        !manager
            .permission_allowed(
                "acme.lifecycle",
                "application",
                magi_plugin_system::PluginPermissionKind::Storage,
                "other",
            )
            .unwrap()
    );
}

#[test]
fn activation_requires_every_declared_permission_to_be_granted() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package_from_manifest(json!({
        "sdkVersion":1,"id":"acme.shared","version":"1.0.0","name":"Shared","description":"",
        "backend":"plugin.mjs","permissions":[
            {"kind":"storage","scope":"application","targets":["session"]},
            {"kind":"storage","scope":"workspace","targets":["cache"]}
        ],
        "dataSchemaVersion":1,"settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"commands":[{"id":"open","title":"Open","description":""}]}
    }));
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "shared.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.shared", "application").unwrap();
    let workspace_grant = PluginPermission {
        kind: PluginPermissionKind::Storage,
        scope: PluginScopeKind::Workspace,
        targets: vec!["cache".into()],
    };
    let application_grant = PluginPermission {
        kind: PluginPermissionKind::Storage,
        scope: PluginScopeKind::Application,
        targets: vec!["session".into()],
    };
    manager
        .authorize("acme.shared", "application", vec![workspace_grant.clone()])
        .unwrap();
    assert!(manager.activate("acme.shared", "application").is_err());
    manager
        .authorize(
            "acme.shared",
            "application",
            vec![workspace_grant, application_grant],
        )
        .unwrap();
    manager.activate("acme.shared", "application").unwrap();
}

fn package_from_manifest(manifest: Value) -> PluginPackage {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("plugin.mjs", b"export default input".to_vec()),
    ] {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

#[test]
fn revoking_authorization_cancels_runs_but_uninstall_waits_for_settlement() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(true);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "revoke.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let run = magi_plugin_system::RunCancellation::default();
    let lease = magi_plugin_system::PluginRuntimeLease::acquire(
        Arc::clone(&manager),
        "acme.lifecycle",
        package.digest(),
        "application",
        &run,
    )
    .unwrap();

    manager
        .lock()
        .unwrap()
        .authorize("acme.lifecycle", "application", Vec::new())
        .unwrap();
    assert!(run.is_cancelled(), "撤权优先于排空：在途调用立即取消");
    {
        let mut manager = manager.lock().unwrap();
        manager.disable("acme.lifecycle", "application").unwrap();
        let before = manager.state().clone();
        let package_path = root
            .path()
            .join(&before.plugins["acme.lifecycle"].package_path);
        assert!(
            manager.uninstall("acme.lifecycle").is_err(),
            "撤权与禁用不能把取消信号当成资源已经结算"
        );
        assert_eq!(manager.state(), &before);
        assert!(package_path.is_file(), "在途调用仍引用的包必须保留");
    }
    drop(lease);
    manager.lock().unwrap().uninstall("acme.lifecycle").unwrap();
    assert!(
        !manager
            .lock()
            .unwrap()
            .state()
            .plugins
            .contains_key("acme.lifecycle")
    );
}

#[test]
fn draining_closes_independent_editing_admission() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Local {
                name: "edit.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let guard = manager.lock().unwrap();
    assert!(
        guard
            .ensure_editing_admitted("acme.lifecycle", "application")
            .is_ok()
    );
    drop(guard);
    let drain =
        PluginManager::begin_deactivation(Arc::clone(&manager), "acme.lifecycle", "application")
            .unwrap();
    assert!(
        manager
            .lock()
            .unwrap()
            .ensure_editing_admitted("acme.lifecycle", "application")
            .is_err()
    );
    drop(drain);
}

#[test]
fn activation_rejects_browser_surface_policies_the_host_cannot_mount() {
    let mut manifest = json!({
        "sdkVersion":1,"id":"acme.surface","version":"1.0.0","name":"Surface","description":"",
        "backend":"plugin.mjs",        "permissions":[{"kind":"browser","scope":"application","targets":["login"]}],
        "dataSchemaVersion":1,"settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"browserSurfaces":[{
            "id":"login","title":"Login","description":"","scope":"application",
            "persistence":"persistent","keepMounted":true,"closeAction":"hide","backgroundInference":true
        }]}
    });
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package_from_manifest(manifest.clone()),
            PluginSource::Local {
                name: "s.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.surface", "application").unwrap();
    manager
        .authorize(
            "acme.surface",
            "application",
            vec![PluginPermission {
                kind: PluginPermissionKind::Browser,
                scope: PluginScopeKind::Application,
                targets: vec!["login".into()],
            }],
        )
        .unwrap();
    manager.activate("acme.surface", "application").unwrap();

    manifest["version"] = json!("1.1.0");
    manifest["contributions"]["browserSurfaces"][0]["closeAction"] = json!("destroy");
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package_from_manifest(manifest),
            PluginSource::Local {
                name: "s.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.surface", "application").unwrap();
    manager
        .authorize(
            "acme.surface",
            "application",
            vec![PluginPermission {
                kind: PluginPermissionKind::Browser,
                scope: PluginScopeKind::Application,
                targets: vec!["login".into()],
            }],
        )
        .unwrap();
    let error = manager.activate("acme.surface", "application").unwrap_err();
    assert!(error.to_string().contains("宿主尚不支持"), "{error}");
}

fn local(name: &str) -> PluginSource {
    PluginSource::Local { name: name.into() }
}

#[test]
fn preview_of_a_new_plugin_lists_every_requested_permission_and_changes_nothing() {
    let root = tempdir().unwrap();
    let manager = PluginManager::open(root.path()).unwrap();
    let candidate = package(true);
    let preview = manager.preview(&candidate, &local("a.zip")).unwrap();
    assert_eq!(
        preview.operation,
        magi_app_server_protocol::generated::PluginCandidateOperation::Install
    );
    assert_eq!(preview.digest, candidate.digest());
    assert_eq!(preview.added_permissions, vec![grant()]);
    assert!(preview.removed_permissions.is_empty());
    assert!(preview.installed_version.is_none());
    assert!(manager.state().plugins.is_empty(), "预览不得写入安装状态");
}

#[test]
fn preview_of_an_upgrade_reports_permission_diff_and_blocked_data_migration() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager.install(&package(false), local("v1.zip")).unwrap();

    let mut widened = manifest(false);
    widened["version"] = json!("1.2.0");
    widened["permissions"] = json!([{"kind":"storage","scope":"workspace","targets":["cache"]}]);
    let widened = package_from_manifest(widened);
    let preview = manager.preview(&widened, &local("v2.zip")).unwrap();
    assert_eq!(
        preview.operation,
        magi_app_server_protocol::generated::PluginCandidateOperation::Upgrade
    );
    assert_eq!(preview.installed_version.as_deref(), Some("1.0.0"));
    assert_eq!(preview.added_permissions, vec![grant()]);
    assert!(preview.removed_permissions.is_empty());
    assert!(!preview.data_schema_blocked);

    // 目标范围变化被视为新增加移除，而不是静默沿用；目标顺序不影响判断。
    let mut base = manifest(true);
    base["permissions"] = json!([{"kind":"storage","scope":"workspace","targets":["b","a"]}]);
    let root2 = tempdir().unwrap();
    let mut second = PluginManager::open(root2.path()).unwrap();
    second
        .install(&package_from_manifest(base), local("v1.zip"))
        .unwrap();
    let mut reordered = manifest(true);
    reordered["version"] = json!("1.0.1");
    reordered["permissions"] = json!([{"kind":"storage","scope":"workspace","targets":["a","b"]}]);
    let preview = second
        .preview(&package_from_manifest(reordered), &local("v2.zip"))
        .unwrap();
    assert!(preview.added_permissions.is_empty() && preview.removed_permissions.is_empty());
    let mut expanded = manifest(true);
    expanded["version"] = json!("1.0.2");
    expanded["permissions"] =
        json!([{"kind":"storage","scope":"workspace","targets":["a","b","c"]}]);
    let preview = second
        .preview(&package_from_manifest(expanded), &local("v3.zip"))
        .unwrap();
    assert_eq!(preview.added_permissions.len(), 1);
    assert_eq!(preview.removed_permissions.len(), 1);

    // 数据格式变化：无数据时可切换，有数据时在预览阶段就标记为阻塞。
    let with_resource = |mut value: Value| {
        value["contributions"]["resources"] =
            json!([{"id":"cache","title":"Cache","description":""}]);
        value
    };
    let mut schema = with_resource(manifest(true));
    schema["version"] = json!("2.0.0");
    schema["dataSchemaVersion"] = json!(2);
    let schema = package_from_manifest(schema);
    let root3 = tempdir().unwrap();
    let mut third = PluginManager::open(root3.path()).unwrap();
    third
        .install(
            &package_from_manifest(with_resource(manifest(true))),
            local("v1.zip"),
        )
        .unwrap();
    let preview = third.preview(&schema, &local("v2.zip")).unwrap();
    assert_eq!(preview.data_schema_from, Some(1));
    assert!(!preview.data_schema_blocked);
    third.enable("acme.lifecycle", "application").unwrap();
    third
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    third.activate("acme.lifecycle", "application").unwrap();
    third
        .write_resource("acme.lifecycle", "application", "cache", 0, json!({"k": 1}))
        .unwrap();
    assert!(
        third
            .preview(&schema, &local("v2.zip"))
            .unwrap()
            .data_schema_blocked
    );
}

#[test]
fn preview_of_the_installed_package_is_unchanged() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let installed = package(true);
    manager.install(&installed, local("v1.zip")).unwrap();
    let preview = manager.preview(&installed, &local("v1.zip")).unwrap();
    assert_eq!(
        preview.operation,
        magi_app_server_protocol::generated::PluginCandidateOperation::Unchanged
    );
    assert!(preview.added_permissions.is_empty());
}

mod data_migration {
    use super::*;
    use magi_plugin_system::{
        ConvertedData, DataMigrationPlan, MigrationEntry, PluginDataMigrator,
    };

    /// 以闭包代替 Worker：真实 Worker 路径由 Electron 验收覆盖。
    type Convert = Box<dyn Fn(&MigrationEntry) -> Option<Value> + Send + Sync>;
    struct Fake(Convert);

    impl PluginDataMigrator for Fake {
        fn convert(
            &self,
            _package: &PluginPackage,
            plan: &DataMigrationPlan,
        ) -> Result<ConvertedData, PluginError> {
            Ok(ConvertedData {
                plan_fingerprint: plan.fingerprint(),
                entries: plan
                    .entries
                    .iter()
                    .map(|entry| (entry.clone(), (self.0)(entry)))
                    .collect(),
            })
        }
    }

    fn full_grant() -> PluginPermission {
        PluginPermission {
            kind: PluginPermissionKind::Storage,
            scope: PluginScopeKind::Workspace,
            targets: vec!["cache".into(), "extra".into()],
        }
    }

    fn with_resources(mut value: Value) -> Value {
        value["permissions"] = json!([
            {"kind":"storage","scope":"workspace","targets":["cache","extra"]}
        ]);
        value["contributions"]["resources"] = json!([
            {"id":"cache","title":"Cache","description":""},
            {"id":"extra","title":"Extra","description":""}
        ]);
        value
    }

    fn v1() -> PluginPackage {
        package_from_manifest(with_resources(manifest(true)))
    }

    fn v2(declare: bool) -> PluginPackage {
        let mut value = with_resources(manifest(true));
        value["version"] = json!("2.0.0");
        value["dataSchemaVersion"] = json!(2);
        if declare {
            value["dataMigration"] = json!({"fromVersions": [1]});
        }
        package_from_manifest(value)
    }

    fn seeded(root: &std::path::Path) -> Arc<Mutex<PluginManager>> {
        let mut manager = PluginManager::open(root).unwrap();
        manager.install(&v1(), local("v1.zip")).unwrap();
        manager.enable("acme.lifecycle", "application").unwrap();
        manager
            .authorize("acme.lifecycle", "application", vec![full_grant()])
            .unwrap();
        manager.activate("acme.lifecycle", "application").unwrap();
        manager
            .write_resource("acme.lifecycle", "application", "cache", 0, json!({"k": 1}))
            .unwrap();
        manager
            .write_resource("acme.lifecycle", "application", "extra", 0, json!("old"))
            .unwrap();
        manager.deactivate("acme.lifecycle", "application").unwrap();
        Arc::new(Mutex::new(manager))
    }

    fn upgrade(
        manager: &Arc<Mutex<PluginManager>>,
        package: &PluginPackage,
        migrator: &Fake,
    ) -> Result<(), PluginError> {
        let drain = PluginManager::begin_upgrade_drain(Arc::clone(manager), "acme.lifecycle")?;
        let plan = manager.lock().unwrap().plan_data_migration(package)?;
        let converted = plan
            .map(|plan| migrator.convert(package, &plan))
            .transpose()?;
        let result = manager
            .lock()
            .unwrap()
            .upgrade(package, local("v2.zip"), &drain, converted);
        drop(drain);
        result
    }

    fn read(manager: &Arc<Mutex<PluginManager>>, resource: &str) -> Option<(u64, Value)> {
        let mut manager = manager.lock().unwrap();
        manager.enable("acme.lifecycle", "application").ok();
        manager
            .authorize("acme.lifecycle", "application", vec![full_grant()])
            .unwrap();
        manager.activate("acme.lifecycle", "application").unwrap();
        let value = manager
            .read_resource("acme.lifecycle", "application", resource)
            .ok()
            .filter(|resource| resource.version > 0)
            .map(|resource| (resource.version, resource.value));
        manager.deactivate("acme.lifecycle", "application").unwrap();
        value
    }

    #[test]
    fn undeclared_conversion_is_refused_with_the_reason() {
        let root = tempdir().unwrap();
        let manager = seeded(root.path());
        let error = upgrade(&manager, &v2(false), &Fake(Box::new(|_| None))).unwrap_err();
        assert!(error.to_string().contains("dataMigration"), "{error}");
        assert_eq!(read(&manager, "cache"), Some((1, json!({"k": 1}))));
    }

    #[test]
    fn preview_distinguishes_convertible_from_blocked_data_changes() {
        let root = tempdir().unwrap();
        let manager = seeded(root.path());
        let manager = manager.lock().unwrap();
        let blocked = manager.preview(&v2(false), &local("v2.zip")).unwrap();
        assert!(blocked.data_schema_blocked && !blocked.data_conversion);
        let convertible = manager.preview(&v2(true), &local("v2.zip")).unwrap();
        assert!(!convertible.data_schema_blocked && convertible.data_conversion);
        assert_eq!(convertible.data_schema_from, Some(1));
    }

    #[test]
    fn declared_conversion_replaces_data_bumps_versions_and_drops_entries() {
        let root = tempdir().unwrap();
        let manager = seeded(root.path());
        let migrator = Fake(Box::new(|entry| match entry.resource_id.as_str() {
            "cache" => Some(json!({"k": 1, "migrated": true})),
            _ => None,
        }));
        upgrade(&manager, &v2(true), &migrator).unwrap();
        assert_eq!(
            read(&manager, "cache"),
            Some((2, json!({"k": 1, "migrated": true}))),
            "版本在旧版本之上递增，乐观并发不回退"
        );
        assert_eq!(read(&manager, "extra"), None, "返回 drop 的条目被丢弃");
        assert!(!root.path().join("upgrade-operation.json").exists());
        let reopened = PluginManager::open(root.path()).unwrap();
        assert_eq!(reopened.state().plugins["acme.lifecycle"].version, "2.0.0");
    }

    #[test]
    fn conversion_results_that_break_the_new_contract_are_refused_and_change_nothing() {
        let root = tempdir().unwrap();
        let manager = seeded(root.path());
        // 保留了新版没有声明的资源。
        let mut undeclared = with_resources(manifest(true));
        undeclared["version"] = json!("2.0.0");
        undeclared["dataSchemaVersion"] = json!(2);
        undeclared["dataMigration"] = json!({"fromVersions": [1]});
        undeclared["contributions"]["resources"] =
            json!([{"id":"cache","title":"Cache","description":""}]);
        let package = package_from_manifest(undeclared);
        let error = upgrade(
            &manager,
            &package,
            &Fake(Box::new(|entry| Some(entry.resource.value.clone()))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("extra"), "{error}");
        assert_eq!(
            manager.lock().unwrap().state().plugins["acme.lifecycle"].version,
            "1.0.0"
        );
        assert!(!root.path().join("upgrade-operation.json").exists());
        assert_eq!(read(&manager, "extra"), Some((1, json!("old"))));
    }

    #[test]
    fn stale_conversion_is_rejected_when_data_changed_during_conversion() {
        let root = tempdir().unwrap();
        let manager = seeded(root.path());
        let package = v2(true);
        let drain =
            PluginManager::begin_upgrade_drain(Arc::clone(&manager), "acme.lifecycle").unwrap();
        let plan = manager
            .lock()
            .unwrap()
            .plan_data_migration(&package)
            .unwrap()
            .unwrap();
        let mut converted = Fake(Box::new(|entry| Some(entry.resource.value.clone())))
            .convert(&package, &plan)
            .unwrap();
        converted.plan_fingerprint[0].2 += 1;
        let error = manager
            .lock()
            .unwrap()
            .upgrade(&package, local("v2.zip"), &drain, Some(converted))
            .unwrap_err();
        assert!(error.to_string().contains("发生变化"), "{error}");
    }

    fn crafted_operation(root: &std::path::Path, to_digest: &str, from_digest: &str) {
        std::fs::create_dir_all(root.join("staging/acme.lifecycle")).unwrap();
        std::fs::write(
            root.join("staging/acme.lifecycle/staged.json"),
            serde_json::to_vec(&json!([{
                "scope": "application",
                "resource_id": "cache",
                "resource": {"version": 2, "value": {"k": "converted"}}
            }]))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("upgrade-operation.json"),
            serde_json::to_vec(&json!({
                "plugin_id": "acme.lifecycle",
                "from_digest": from_digest,
                "to_digest": to_digest,
                "staged_path": "staging/acme.lifecycle/staged.json"
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn restart_before_the_package_switch_discards_staged_data_and_keeps_the_old_version() {
        let root = tempdir().unwrap();
        let old_digest = {
            let manager = seeded(root.path());
            manager.lock().unwrap().state().plugins["acme.lifecycle"]
                .digest
                .clone()
        };
        crafted_operation(root.path(), v2(true).digest(), &old_digest);
        let manager = Arc::new(Mutex::new(PluginManager::open(root.path()).unwrap()));
        assert_eq!(
            manager.lock().unwrap().state().plugins["acme.lifecycle"].digest,
            old_digest
        );
        assert!(!root.path().join("upgrade-operation.json").exists());
        assert!(
            !root
                .path()
                .join("staging/acme.lifecycle/staged.json")
                .exists()
        );
        assert_eq!(
            read(&manager, "cache"),
            Some((1, json!({"k": 1}))),
            "旧数据完整"
        );
    }

    #[test]
    fn restart_after_the_package_switch_replays_staged_data() {
        let root = tempdir().unwrap();
        let manager = seeded(root.path());
        let old_digest = manager.lock().unwrap().state().plugins["acme.lifecycle"]
            .digest
            .clone();
        let migrator = Fake(Box::new(|entry| Some(entry.resource.value.clone())));
        let package = v2(true);
        upgrade(&manager, &package, &migrator).unwrap();
        drop(manager);
        // 模拟“包切换已提交、暂存数据尚未应用”的崩溃：资源文件回到旧内容，记录与暂存仍在。
        let resources = root.path().join("resources.json");
        let after = std::fs::read(&resources).unwrap();
        let before = String::from_utf8(after)
            .unwrap()
            .replace("\"version\": 2", "\"version\": 1");
        std::fs::write(&resources, before).unwrap();
        crafted_operation(root.path(), package.digest(), &old_digest);
        let manager = Arc::new(Mutex::new(PluginManager::open(root.path()).unwrap()));
        assert_eq!(
            read(&manager, "cache"),
            Some((2, json!({"k": "converted"})))
        );
        assert!(!root.path().join("upgrade-operation.json").exists());
    }
}

#[test]
fn missing_package_is_isolated_on_startup_without_losing_other_plugins_or_resources() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(true);
    let mut healthy_manifest = manifest(false);
    healthy_manifest["id"] = json!("acme.healthy");
    let healthy = package_from_manifest(healthy_manifest);
    for package in [&package, &healthy] {
        manager
            .install(
                package,
                PluginSource::Local {
                    name: "test.zip".into(),
                },
            )
            .unwrap();
    }
    manager.enable("acme.lifecycle", "application").unwrap();
    manager
        .authorize(
            "acme.lifecycle",
            "application",
            package.manifest().permissions.clone(),
        )
        .unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let missing_path = manager.state().plugins["acme.lifecycle"]
        .package_path
        .clone();
    let original = std::fs::read(root.path().join("state.json")).unwrap();
    // 模拟旧 Windows 版本清理了仍被 state.json 引用的 ZIP。
    std::fs::remove_file(root.path().join(missing_path)).unwrap();
    drop(manager);
    let mut resources = PluginResourceStore::open(root.path().join("resources.json")).unwrap();
    let retained = resources
        .write(
            "acme.lifecycle",
            "application",
            "cache",
            0,
            json!({"saved":true}),
        )
        .unwrap();
    let resource_bytes = std::fs::read(root.path().join("resources.json")).unwrap();

    let mut recovered = PluginManager::open(root.path()).unwrap();
    assert!(!recovered.state().plugins.contains_key("acme.lifecycle"));
    assert_eq!(recovered.projection().unwrap().plugins.len(), 1);
    assert_eq!(
        recovered.package("acme.healthy").unwrap().digest(),
        healthy.digest()
    );
    let backup = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state.json.missing-packages-")
        })
        .unwrap();
    assert_eq!(std::fs::read(backup).unwrap(), original);
    assert_eq!(
        std::fs::read(root.path().join("resources.json")).unwrap(),
        resource_bytes
    );
    let mut different = manifest(true);
    different["version"] = json!("2.0.0");
    assert!(matches!(
        recovered.install(
            &package_from_manifest(different),
            PluginSource::Local {
                name: "new.zip".into()
            }
        ),
        Err(PluginError::Conflict(_))
    ));
    assert_eq!(
        resources.read("acme.lifecycle", "application", "cache"),
        Some(retained)
    );
    recovered
        .install(
            &package,
            PluginSource::Local {
                name: "reinstall.zip".into(),
            },
        )
        .unwrap();
    let reinstalled = &recovered.state().plugins["acme.lifecycle"];
    assert!(reinstalled.enabled_scopes.is_empty());
    assert!(reinstalled.active_scopes.is_empty());
    assert!(reinstalled.grants.is_empty());
    drop(recovered);
    assert_eq!(
        PluginManager::open(root.path())
            .unwrap()
            .projection()
            .unwrap()
            .plugins
            .len(),
        2
    );
}

#[test]
fn startup_recovery_does_not_accept_illegal_package_paths_or_overwrite_state() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package(false),
            PluginSource::Local {
                name: "test.zip".into(),
            },
        )
        .unwrap();
    let state_path = root.path().join("state.json");
    let mut state = serde_json::to_value(manager.state()).unwrap();
    state["plugins"]["acme.lifecycle"]["package_path"] = json!("../outside.zip");
    let original = serde_json::to_vec(&state).unwrap();
    std::fs::write(&state_path, &original).unwrap();
    drop(manager);
    assert!(matches!(
        PluginManager::open(root.path()),
        Err(PluginError::CorruptState(_))
    ));
    assert_eq!(std::fs::read(state_path).unwrap(), original);
}

#[test]
fn repeated_reopen_and_uninstall_preserve_all_referenced_version_packages() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    for id in ["acme.first", "acme.second"] {
        let mut manifest = manifest(false);
        manifest["id"] = json!(id);
        let package = package_from_manifest(manifest);
        manager
            .install(
                &package,
                PluginSource::Local {
                    name: "test.zip".into(),
                },
            )
            .unwrap();
    }
    // Windows 的 read_dir 使用反斜线，持久化记录使用斜线；本测试在 Windows CI 同样运行。
    for _ in 0..3 {
        drop(manager);
        manager = PluginManager::open(root.path()).unwrap();
        assert_eq!(manager.projection().unwrap().plugins.len(), 2);
        assert!(manager.package("acme.first").is_ok());
        assert!(manager.package("acme.second").is_ok());
    }
    manager.uninstall("acme.first").unwrap();
    drop(manager);
    let reopened = PluginManager::open(root.path()).unwrap();
    assert_eq!(reopened.projection().unwrap().plugins.len(), 1);
    assert!(reopened.package("acme.second").is_ok());
}
