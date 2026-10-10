use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
use serde_json::{Value, json};
use std::io::{Cursor, Write};
use tempfile::tempdir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn manifest(with_permission: bool) -> Value {
    json!({
        "sdkVersion":1,"id":"acme.lifecycle","version":"1.0.0","name":"Lifecycle","description":"",
        "backend":"plugin.mjs","applicationInstance":false,
        "permissions": if with_permission { json!([{"kind":"storage","scope":"workspace","targets":["cache"]}]) } else { json!([]) },
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
    assert!(manager.activate("acme.lifecycle", "workspace:one").is_err());
    manager.enable("acme.lifecycle", "workspace:one").unwrap();
    assert!(manager.activate("acme.lifecycle", "workspace:one").is_err());
    manager
        .authorize(
            "acme.lifecycle",
            "workspace:one",
            vec![magi_plugin_system::PluginPermission {
                kind: magi_plugin_system::PluginPermissionKind::Storage,
                scope: magi_plugin_system::PluginScopeKind::Workspace,
                targets: vec!["cache".into()],
            }],
        )
        .unwrap();
    let active = manager.activate("acme.lifecycle", "workspace:one").unwrap();
    assert_eq!(active.digest, package.digest());
    assert_eq!(manager.active("workspace:one").len(), 1);
    assert!(manager.disable("acme.lifecycle", "workspace:one").is_err());
    manager
        .deactivate("acme.lifecycle", "workspace:one")
        .unwrap();
    manager.disable("acme.lifecycle", "workspace:one").unwrap();
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
        kind: magi_plugin_system::PluginPermissionKind::Network,
        scope: magi_plugin_system::PluginScopeKind::Workspace,
        targets: vec!["example.invalid".into()],
    };
    assert!(
        manager
            .authorize("acme.lifecycle", "workspace:one", vec![grant])
            .is_err()
    );
    manager.enable("acme.lifecycle", "workspace:one").unwrap();
    assert!(manager.uninstall("acme.lifecycle").is_err());
    manager.disable("acme.lifecycle", "workspace:one").unwrap();
    manager.uninstall("acme.lifecycle").unwrap();
    assert!(manager.package("acme.lifecycle").is_err());
}

#[test]
fn upgrade_requires_a_drained_scope_and_keeps_one_installed_version() {
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
    manager.enable("acme.lifecycle", "workspace:one").unwrap();
    assert!(
        manager
            .upgrade(
                &package(false),
                PluginSource::Local {
                    name: "same.zip".into()
                }
            )
            .is_ok()
    );
    let mut second_manifest = manifest(false);
    second_manifest["version"] = json!("1.1.0");
    let second = package_from_manifest(second_manifest);
    assert!(
        manager
            .upgrade(
                &second,
                PluginSource::Local {
                    name: "second.zip".into()
                }
            )
            .is_err()
    );
    manager.disable("acme.lifecycle", "workspace:one").unwrap();
    manager
        .upgrade(
            &second,
            PluginSource::Local {
                name: "second.zip".into(),
            },
        )
        .unwrap();
    assert_eq!(manager.state().plugins.len(), 1);
    assert_eq!(
        manager
            .package("acme.lifecycle")
            .unwrap()
            .manifest()
            .version,
        "1.1.0"
    );
}

#[test]
fn active_manifests_are_projected_to_the_requested_scope() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let workspace = package(false);
    manager
        .install(
            &workspace,
            PluginSource::Local {
                name: "workspace.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "workspace:one").unwrap();
    manager.activate("acme.lifecycle", "workspace:one").unwrap();
    assert_eq!(
        manager.manifests_for_scope("workspace:one").unwrap().len(),
        1
    );
    assert!(
        manager
            .manifests_for_scope("workspace:two")
            .unwrap()
            .is_empty()
    );
    assert!(
        manager
            .is_active_for_scope("acme.lifecycle", "workspace:two")
            .is_ok_and(|active| !active)
    );
}

#[test]
fn active_commands_are_projected_to_the_requested_scope() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package(false);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "commands.zip".into(),
            },
        )
        .unwrap();
    manager.enable("acme.lifecycle", "workspace:one").unwrap();
    manager.activate("acme.lifecycle", "workspace:one").unwrap();
    let commands = manager
        .command_contributions_for_scope("workspace:one")
        .unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].0.id, "acme.lifecycle");
    assert_eq!(commands[0].1.id, "open");
    assert!(
        manager
            .command_contributions_for_scope("workspace:two")
            .unwrap()
            .is_empty()
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
    manager.enable("acme.lifecycle", "workspace:one").unwrap();
    manager
        .authorize(
            "acme.lifecycle",
            "workspace:one",
            vec![magi_plugin_system::PluginPermission {
                kind: magi_plugin_system::PluginPermissionKind::Storage,
                scope: magi_plugin_system::PluginScopeKind::Workspace,
                targets: vec!["cache".into()],
            }],
        )
        .unwrap();
    manager.activate("acme.lifecycle", "workspace:one").unwrap();
    assert!(manager
        .permission_allowed(
            "acme.lifecycle",
            "workspace:one",
            magi_plugin_system::PluginPermissionKind::Storage,
            "cache",
        )
        .unwrap());
    assert!(!manager
        .permission_allowed(
            "acme.lifecycle",
            "workspace:one",
            magi_plugin_system::PluginPermissionKind::Storage,
            "other",
        )
        .unwrap());
}

#[test]
fn workspace_activation_requires_application_scoped_grants_too() {
    let root = tempdir().unwrap();
    let mut manager = PluginManager::open(root.path()).unwrap();
    let package = package_from_manifest(json!({
        "sdkVersion":1,"id":"acme.shared","version":"1.0.0","name":"Shared","description":"",
        "backend":"plugin.mjs","applicationInstance":true,
        "permissions":[
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
    manager.enable("acme.shared", "workspace:one").unwrap();
    manager
        .authorize(
            "acme.shared",
            "workspace:one",
            vec![magi_plugin_system::PluginPermission {
                kind: magi_plugin_system::PluginPermissionKind::Storage,
                scope: magi_plugin_system::PluginScopeKind::Workspace,
                targets: vec!["cache".into()],
            }],
        )
        .unwrap();
    assert!(manager.activate("acme.shared", "workspace:one").is_err());
    manager
        .authorize(
            "acme.shared",
            "application",
            vec![magi_plugin_system::PluginPermission {
                kind: magi_plugin_system::PluginPermissionKind::Storage,
                scope: magi_plugin_system::PluginScopeKind::Application,
                targets: vec!["session".into()],
            }],
        )
        .unwrap();
    manager.activate("acme.shared", "workspace:one").unwrap();
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
