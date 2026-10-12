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
            "application",
            "board",
            0,
            json!({"items":[]}),
        )
        .unwrap();
    assert_eq!(first.version, 1);
    let error = store
        .write(
            "acme.dashboard",
            "application",
            "board",
            0,
            json!({"items":[1]}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("expected=0"));
    let second = store
        .write(
            "acme.dashboard",
            "application",
            "board",
            1,
            json!({"items":[1]}),
        )
        .unwrap();
    assert_eq!(
        store
            .read("acme.dashboard", "application", "board")
            .unwrap(),
        second
    );
    assert!(
        store
            .read("acme.dashboard", "application", "other")
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

#[test]
fn failed_persistence_does_not_advance_the_in_memory_resource_version() {
    let dir = tempdir().unwrap();
    let directory = dir.path().join("state");
    let mut store = PluginResourceStore::open(directory.join("resources.json")).unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    let error = store
        .write("acme.dashboard", "application", "board", 0, json!({"n": 1}))
        .unwrap_err();
    assert!(error.to_string().contains("资源存储失败"), "{error}");
    assert!(
        store
            .read("acme.dashboard", "application", "board")
            .is_none(),
        "落盘失败的写入不能留在内存快照里"
    );

    std::fs::create_dir_all(&directory).unwrap();
    let recovered = store
        .write("acme.dashboard", "application", "board", 0, json!({"n": 2}))
        .unwrap();
    assert_eq!(recovered.version, 1, "失败写入不得消耗版本号");
}

fn dashboard_package() -> PluginPackage {
    let manifest = serde_json::json!({
        "sdkVersion": 1,
        "id": "acme.dashboard",
        "version": "1.0.0",
        "name": "Dashboard",
        "description": "",
        "backend": "plugin.mjs",
        "permissions": [{"kind":"storage","scope":"workspace","targets":["board"]}],
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
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

#[test]
fn manager_resource_access_is_declared_authorized_and_scoped_in_one_place() {
    use magi_plugin_system::{
        PluginError, PluginPermission, PluginPermissionKind, PluginScopeKind,
    };

    let dir = tempdir().unwrap();
    let mut manager = PluginManager::open(dir.path()).unwrap();
    manager
        .install(
            &dashboard_package(),
            PluginSource::Local {
                name: "dashboard.zip".into(),
            },
        )
        .unwrap();
    let scope = "application";
    // 未激活：隐藏入口之外，直接调用也被拒绝。
    assert!(matches!(
        manager.read_resource("acme.dashboard", scope, "board"),
        Err(PluginError::NotAuthorized(_))
    ));
    manager.enable("acme.dashboard", scope).unwrap();
    manager
        .authorize(
            "acme.dashboard",
            scope,
            vec![PluginPermission {
                kind: PluginPermissionKind::Storage,
                scope: PluginScopeKind::Workspace,
                targets: vec!["board".into()],
            }],
        )
        .unwrap();
    manager.activate("acme.dashboard", scope).unwrap();

    assert!(matches!(
        manager.read_resource("acme.dashboard", scope, "forged"),
        Err(PluginError::NotFound(_))
    ));
    // 尚未写入的资源读到版本 0，调用方据此完成首次提交。
    let empty = manager
        .read_resource("acme.dashboard", scope, "board")
        .unwrap();
    assert_eq!((empty.version, empty.value), (0, serde_json::Value::Null));
    let first = manager
        .write_resource(
            "acme.dashboard",
            scope,
            "board",
            empty.version,
            json!({"n": 1}),
        )
        .unwrap();
    assert_eq!(first.version, 1);
    assert!(
        manager
            .write_resource("acme.dashboard", scope, "board", 0, json!({"n": 2}))
            .is_err(),
        "过期版本必须明确冲突"
    );
    // 插件全局生效：工作区作用域键不是合法的资源作用域。
    assert!(
        manager
            .read_resource("acme.dashboard", "workspace:two", "board")
            .is_err()
    );
}
