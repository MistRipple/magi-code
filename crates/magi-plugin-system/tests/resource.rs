use magi_plugin_system::{PluginManager, PluginPackage, PluginResourceStore, PluginSource};
use serde_json::json;
use std::{
    io::{Cursor, Write},
    path::Path,
};
use tempfile::tempdir;
use zip::ZipWriter;

#[test]
fn resource_updates_are_versioned_and_conflicts_are_explicit() {
    let dir = tempdir().unwrap();
    let mut store = PluginResourceStore::open(dir.path().join("resources.json")).unwrap();
    let first = store
        .write(
            "acme.dashboard",
            "workspace:one",
            "board",
            0,
            json!({"items":[]}),
        )
        .unwrap();
    assert_eq!(first.version, 1);
    let error = store
        .write(
            "acme.dashboard",
            "workspace:one",
            "board",
            0,
            json!({"items":[1]}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("expected=0"));
    let second = store
        .write(
            "acme.dashboard",
            "workspace:one",
            "board",
            1,
            json!({"items":[1]}),
        )
        .unwrap();
    assert_eq!(
        store
            .read("acme.dashboard", "workspace:one", "board")
            .unwrap(),
        second
    );
    assert!(
        store
            .read("acme.dashboard", "workspace:two", "board")
            .is_none()
    );
}

#[test]
fn resource_capabilities_require_a_manifest_resource_identity() {
    let dir = tempdir().unwrap();
    let manifest = serde_json::json!({
        "sdkVersion": 1,
        "id": "acme.dashboard",
        "version": "1.0.0",
        "name": "Dashboard",
        "description": "",
        "backend": "plugin.mjs",
        "applicationInstance": false,
        "permissions": [],
        "dataSchemaVersion": 1,
        "settingsSchema": {"type":"object","additionalProperties":false},
        "contributions": {"resources":[{"id":"board","title":"Board","description":""}]}
    });
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("plugin.mjs", b"export default () => ({})".to_vec()),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
    let mut manager = PluginManager::open(dir.path()).unwrap();
    manager
        .install(
            &package,
            PluginSource::Local {
                name: Path::new("dashboard.zip").display().to_string(),
            },
        )
        .unwrap();
    assert!(
        manager
            .resource_declared("acme.dashboard", "board")
            .unwrap()
    );
    assert!(
        !manager
            .resource_declared("acme.dashboard", "forged")
            .unwrap()
    );
}
