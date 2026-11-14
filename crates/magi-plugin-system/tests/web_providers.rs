use magi_app_server_protocol::generated::{
    PluginRuntimeCatalog, PluginWebConfigureRequest, PluginWebKind,
};
use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
use serde_json::json;
use std::io::{Cursor, Write};
use tempfile::tempdir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn package() -> PluginPackage {
    let manifest = json!({
        "sdkVersion":2,"id":"example.web","version":"1.0.0","name":"Web","description":"",
        "dataSchemaVersion":1,"settingsSchema":{"type":"object"},
        "permissions":[{"kind":"extension","targets":["full-trust"]},{"kind":"tools","targets":["*"]},{"kind":"webProviders","targets":["search","fetch"]}],
        "extension":{"hooks":[],"runtimeContributions":["tools"]},"contributions":{}
    });
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", b"export default () => {}".to_vec()),
    ] {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&data).unwrap();
    }
    PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
}

fn change(revision: i64, tool: &str) -> PluginWebConfigureRequest {
    serde_json::from_value(json!({"expectedRevision":revision,"changes":[
        {"kind":"search","providerId":"search-engine","contributionId":tool},
        {"kind":"fetch","providerId":"fetch-engine","contributionId":tool}
    ]}))
    .unwrap()
}

#[test]
fn web_selection_is_atomic_authorized_persistent_and_bound_to_current_runtime() {
    let root = tempdir().unwrap();
    let package = package();
    let mut manager = PluginManager::open(root.path()).unwrap();
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "web.zip".into(),
            },
        )
        .unwrap();
    manager
        .begin_runtime("example.web", package.digest(), "first")
        .unwrap();
    let catalog: PluginRuntimeCatalog = serde_json::from_value(json!({"tools":[
        {"id":"read","title":"Read","description":"","readOnly":true,"inputSchema":{"type":"object"}},
        {"id":"write","title":"Write","description":"","readOnly":false,"inputSchema":{"type":"object"}}
    ]})).unwrap();
    manager
        .replace_runtime_catalog("example.web", "first", 0, catalog)
        .unwrap();
    assert!(
        manager
            .configure_web("example.web", "stale", change(0, "read"))
            .is_err()
    );
    assert!(
        manager
            .configure_web("example.web", "first", change(0, "write"))
            .is_err()
    );
    let mut mixed = change(0, "read");
    mixed.changes[1].contribution_id = Some("missing".into());
    assert!(
        manager
            .configure_web("example.web", "first", mixed)
            .is_err()
    );
    assert_eq!(manager.web_configuration().revision, 0);
    let configured = manager
        .configure_web("example.web", "first", change(0, "read"))
        .unwrap();
    assert_eq!(configured.revision, 1);
    assert!(
        manager
            .configure_web("example.web", "first", change(0, "read"))
            .is_err()
    );
    assert_eq!(
        manager
            .configure_web("example.web", "first", change(1, "read"))
            .unwrap(),
        configured
    );
    manager.ready_runtime("example.web", "first").unwrap();
    assert!(
        manager
            .selected_web_tool(&PluginWebKind::Search)
            .unwrap()
            .is_some()
    );
    manager.disable("example.web").unwrap();
    assert!(
        manager
            .selected_web_tool(&PluginWebKind::Search)
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .configure_web("example.web", "first", change(1, "read"))
            .is_err()
    );
    drop(manager);
    let mut restored = PluginManager::open(root.path()).unwrap();
    assert_eq!(restored.web_configuration(), configured);
    restored.enable("example.web").unwrap();
    assert!(
        restored
            .selected_web_tool(&PluginWebKind::Fetch)
            .unwrap()
            .is_some(),
        "启动期保留实际选择，不静默改用原生业务"
    );
    assert!(
        restored
            .prepare_tool_call(
                &restored
                    .selected_web_tool(&PluginWebKind::Fetch)
                    .unwrap()
                    .unwrap(),
                "{}"
            )
            .is_err()
    );
}
