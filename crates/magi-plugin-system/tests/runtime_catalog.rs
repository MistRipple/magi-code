use magi_app_server_protocol::generated::PluginRuntimeCatalog;
use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
use serde_json::json;
use std::io::{Cursor, Write};
use tempfile::{TempDir, tempdir};
use zip::{ZipWriter, write::SimpleFileOptions};

const ID: &str = "acme.runtime";

fn fixture() -> (TempDir, PluginManager, PluginPackage) {
    let manifest = json!({
        "sdkVersion": 2, "id": ID, "version": "1.0.0", "name": "Runtime", "description": "",
        "dataSchemaVersion": 1, "settingsSchema": {"type": "object"},
        "permissions": [
            {"kind": "extension", "targets": ["full-trust"]},
            {"kind": "tools", "targets": ["*"]},
            {"kind": "storage", "targets": ["__settings"]}
        ],
        "extension": {"hooks": [], "runtimeContributions": ["tools", "commands", "settings"]},
        "contributions": {}
    });
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", b"export default () => {}".to_vec()),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(&bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&zip.finish().unwrap().into_inner()).unwrap();
    let dir = tempdir().unwrap();
    let mut manager = PluginManager::open(dir.path()).unwrap();
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "fixture.zip".into(),
            },
        )
        .unwrap();
    manager.enable(ID).unwrap();
    manager
        .authorize(ID, package.manifest().permissions.clone())
        .unwrap();
    manager.activate(ID).unwrap();
    (dir, manager, package)
}

fn catalog() -> PluginRuntimeCatalog {
    serde_json::from_value(json!({
        "tools": [{"id": "dynamic", "title": "Dynamic", "description": "", "readOnly": false,
                   "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}],
        "commands": [{"id": "command", "title": "Command", "description": ""}],
        "settingsSchema": {"type": "object", "properties": {"enabled": {"type": "boolean"}}, "additionalProperties": false}
    })).unwrap()
}

#[test]
fn catalog_is_hidden_until_ready_and_never_persisted_as_an_installation_fact() {
    let (dir, mut manager, package) = fixture();
    manager
        .begin_runtime(ID, package.digest(), "first")
        .unwrap();
    assert_eq!(
        manager
            .replace_runtime_catalog(ID, "first", 0, catalog())
            .unwrap(),
        1
    );
    assert!(
        manager
            .effective_manifest(ID)
            .unwrap()
            .contributions
            .tools
            .is_empty()
    );
    assert!(
        manager
            .runtime_registration_allowed(ID, "first", "tool", "dynamic")
            .unwrap()
    );
    assert!(
        !manager
            .runtime_registration_allowed(ID, "first", "tool", "unregistered")
            .unwrap()
    );
    manager.ready_runtime(ID, "first").unwrap();
    assert_eq!(
        manager
            .effective_manifest(ID)
            .unwrap()
            .contributions
            .tools
            .len(),
        1
    );
    assert!(
        manager
            .package(ID)
            .unwrap()
            .manifest()
            .contributions
            .tools
            .is_empty()
    );
    assert!(
        manager
            .write_settings(ID, 0, json!({"enabled": "invalid"}))
            .is_err()
    );
    manager
        .write_settings(ID, 0, json!({"enabled": true}))
        .unwrap();
    let reopened = PluginManager::open(dir.path()).unwrap();
    assert!(
        reopened
            .effective_manifest(ID)
            .unwrap()
            .contributions
            .tools
            .is_empty()
    );
}

#[test]
fn catalog_replacement_is_atomic_versioned_and_bounded_by_manifest() {
    let (_dir, mut manager, package) = fixture();
    manager
        .begin_runtime(ID, package.digest(), "first")
        .unwrap();
    manager
        .replace_runtime_catalog(ID, "first", 0, catalog())
        .unwrap();
    manager.ready_runtime(ID, "first").unwrap();
    assert!(
        manager
            .replace_runtime_catalog(ID, "first", 0, PluginRuntimeCatalog::default())
            .is_err()
    );
    for value in [
        json!({"modelSources": [{"id": "undeclared", "title": "Model", "description": ""}]}),
        json!({"tools": vec![catalog().tools[0].clone(); 129]}),
        json!({"tools": vec![catalog().tools[0].clone(); 2]}),
        json!({"settingsSchema": {"type": "invalid"}}),
    ] {
        let invalid: PluginRuntimeCatalog = serde_json::from_value(value).unwrap();
        assert!(
            manager
                .replace_runtime_catalog(ID, "first", 1, invalid)
                .is_err()
        );
        assert_eq!(
            manager
                .effective_manifest(ID)
                .unwrap()
                .contributions
                .tools
                .len(),
            1
        );
    }
    assert_eq!(
        manager
            .replace_runtime_catalog(ID, "first", 1, PluginRuntimeCatalog::default())
            .unwrap(),
        2
    );
    assert!(
        !manager
            .runtime_registration_allowed(ID, "first", "tool", "dynamic")
            .unwrap()
    );
}

#[test]
fn old_instance_cannot_publish_bind_mark_ready_or_remove_the_new_catalog() {
    let (_dir, mut manager, package) = fixture();
    manager.begin_runtime(ID, package.digest(), "old").unwrap();
    manager
        .replace_runtime_catalog(ID, "old", 0, catalog())
        .unwrap();
    manager.begin_runtime(ID, package.digest(), "new").unwrap();
    manager
        .replace_runtime_catalog(ID, "new", 0, catalog())
        .unwrap();
    assert!(
        manager
            .replace_runtime_catalog(ID, "old", 1, PluginRuntimeCatalog::default())
            .is_err()
    );
    assert!(
        manager
            .runtime_registration_allowed(ID, "old", "tool", "dynamic")
            .is_err()
    );
    assert!(manager.ready_runtime(ID, "old").is_err());
    assert!(!manager.end_runtime(ID, "old"));
    manager.ready_runtime(ID, "new").unwrap();
    assert_eq!(
        manager
            .effective_manifest(ID)
            .unwrap()
            .contributions
            .tools
            .len(),
        1
    );
    assert!(manager.end_runtime(ID, "new"));
    assert!(
        manager
            .effective_manifest(ID)
            .unwrap()
            .contributions
            .tools
            .is_empty()
    );
}

#[test]
fn deactivation_and_revocation_permanently_invalidate_the_previous_lease() {
    for revoke in [false, true] {
        let (_dir, mut manager, package) = fixture();
        manager.begin_runtime(ID, package.digest(), "old").unwrap();
        manager
            .replace_runtime_catalog(ID, "old", 0, catalog())
            .unwrap();
        manager.ready_runtime(ID, "old").unwrap();
        if revoke {
            manager.authorize(ID, vec![]).unwrap();
        } else {
            manager.deactivate(ID).unwrap();
        }
        assert!(manager.check_runtime(ID, "old").is_err());
        manager
            .authorize(ID, package.manifest().permissions.clone())
            .unwrap();
        manager.activate(ID).unwrap();
        assert!(
            manager
                .effective_manifest(ID)
                .unwrap()
                .contributions
                .tools
                .is_empty()
        );
        assert!(manager.check_runtime(ID, "old").is_err());
        manager.begin_runtime(ID, package.digest(), "new").unwrap();
        manager.check_runtime(ID, "new").unwrap();
    }
}
