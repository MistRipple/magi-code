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
        "sdkVersion":2,"id":"acme.lifecycle","version":"1.0.0","name":"Lifecycle","description":"", "permissions": if with_permission { json!([{"kind":"storage","scope":"application","targets":["cache"]}]) } else { json!([]) },
        "dataSchemaVersion":1,"settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"resources":[{"id":"open","title":"Open","description":""}]}
    })
}

fn package(with_permission: bool) -> PluginPackage {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    {
        let (name, bytes) = (
            "manifest.json",
            serde_json::to_vec(&manifest(with_permission)).unwrap(),
        );
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
                scope: magi_plugin_system::PluginScopeKind::Application,
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
        kind: magi_plugin_system::PluginPermissionKind::Storage,
        scope: magi_plugin_system::PluginScopeKind::Application,
        targets: vec!["undeclared-resource".into()],
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
    let result =
        manager
            .lock()
            .unwrap()
            .upgrade(package, PluginSource::Local { name: name.into() }, &drain);
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
        scope: PluginScopeKind::Application,
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
                &single
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
}

#[test]
fn data_schema_change_clears_stored_data_and_is_announced_by_the_preview() {
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

    // 有持久数据时升级成功，但该插件的数据被清空；预览阶段已标记 data_reset。
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
    assert!(
        manager
            .lock()
            .unwrap()
            .preview(
                &next,
                &PluginSource::Local {
                    name: "next.zip".into()
                }
            )
            .unwrap()
            .data_reset
    );
    upgrade_with_drain(&manager, &next, "next.zip").unwrap();
    {
        let manager = manager.lock().unwrap();
        assert_eq!(manager.state().plugins["acme.lifecycle"].version, "2.0.0");
        assert!(!manager.has_resource_data("acme.lifecycle"));
    }

    // 没有任何数据时直接切换到新的数据格式版本。
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
                scope: magi_plugin_system::PluginScopeKind::Application,
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
        "sdkVersion":2,"id":"acme.shared","version":"1.0.0","name":"Shared","description":"", "permissions":[
            {"kind":"storage","scope":"application","targets":["session"]},
            {"kind":"storage","scope":"application","targets":["cache"]}
        ],
        "dataSchemaVersion":1,"settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"resources":[{"id":"open","title":"Open","description":""}]}
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
        scope: PluginScopeKind::Application,
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
    {
        let (name, bytes) = ("manifest.json", serde_json::to_vec(&manifest).unwrap());
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
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
        "sdkVersion":2,"id":"acme.surface","version":"1.0.0","name":"Surface","description":"", "permissions":[{"kind":"browser","scope":"application","targets":["login"]}],
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
    widened["permissions"] = json!([{"kind":"storage","scope":"application","targets":["cache"]}]);
    let widened = package_from_manifest(widened);
    let preview = manager.preview(&widened, &local("v2.zip")).unwrap();
    assert_eq!(
        preview.operation,
        magi_app_server_protocol::generated::PluginCandidateOperation::Upgrade
    );
    assert_eq!(preview.installed_version.as_deref(), Some("1.0.0"));
    assert_eq!(preview.added_permissions, vec![grant()]);
    assert!(preview.removed_permissions.is_empty());
    assert!(!preview.data_reset);

    // 目标范围变化被视为新增加移除，而不是静默沿用；目标顺序不影响判断。
    let mut base = manifest(true);
    base["permissions"] = json!([{"kind":"storage","scope":"application","targets":["b","a"]}]);
    let root2 = tempdir().unwrap();
    let mut second = PluginManager::open(root2.path()).unwrap();
    second
        .install(&package_from_manifest(base), local("v1.zip"))
        .unwrap();
    let mut reordered = manifest(true);
    reordered["version"] = json!("1.0.1");
    reordered["permissions"] =
        json!([{"kind":"storage","scope":"application","targets":["a","b"]}]);
    let preview = second
        .preview(&package_from_manifest(reordered), &local("v2.zip"))
        .unwrap();
    assert!(preview.added_permissions.is_empty() && preview.removed_permissions.is_empty());
    let mut expanded = manifest(true);
    expanded["version"] = json!("1.0.2");
    expanded["permissions"] =
        json!([{"kind":"storage","scope":"application","targets":["a","b","c"]}]);
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
    assert!(!preview.data_reset);
    third.enable("acme.lifecycle", "application").unwrap();
    third
        .authorize("acme.lifecycle", "application", vec![grant()])
        .unwrap();
    third.activate("acme.lifecycle", "application").unwrap();
    third
        .write_resource("acme.lifecycle", "application", "cache", 0, json!({"k": 1}))
        .unwrap();
    assert!(third.preview(&schema, &local("v2.zip")).unwrap().data_reset);
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

#[test]
fn retired_builtin_is_removed_on_reopen_without_erasing_resources() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(false);
    manager.install(&package, local("retired.zip")).unwrap();
    manager.enable("acme.lifecycle", "application").unwrap();
    manager.activate("acme.lifecycle", "application").unwrap();
    let state_path = root.path().join("state.json");
    // 模拟旧宿主曾经发布的内置包；新宿主目录不再声明该身份。
    let mut state = serde_json::to_value(manager.state()).unwrap();
    state["plugins"]["acme.lifecycle"]["source"] = json!({"builtin":{"name":"acme.lifecycle"}});
    drop(manager);
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let mut resources = PluginResourceStore::open(root.path().join("resources.json")).unwrap();
    resources
        .write(
            "acme.lifecycle",
            "application",
            "cache",
            0,
            json!({"saved":true}),
        )
        .unwrap();
    let retained = std::fs::read(root.path().join("resources.json")).unwrap();
    let reopened = PluginManager::open(root.path()).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("resources.json")).unwrap(),
        retained
    );
    assert!(!reopened.state().plugins.contains_key("acme.lifecycle"));
    assert_eq!(
        reopened.retired_builtins(),
        [magi_plugin_system::RetiredBuiltin {
            id: "acme.lifecycle".into(),
            was_active: true
        }]
    );
    assert!(root.path().join("resources.json").exists());
    assert!(std::fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("retired-builtins")
    }));
    let mut reopened = reopened;
    assert!(
        reopened
            .install(
                &package,
                PluginSource::Builtin {
                    name: "acme.lifecycle".into()
                }
            )
            .is_err()
    );
}
