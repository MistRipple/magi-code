use magi_plugin_system::PluginResourceStore;
use serde_json::json;
use tempfile::tempdir;

#[test]
fn resource_updates_are_versioned_and_conflicts_are_explicit() {
    let dir = tempdir().unwrap();
    let mut store = PluginResourceStore::open(dir.path().join("resources.json")).unwrap();
    let first = store
        .write("acme.dashboard", "board", 0, json!({"items":[]}))
        .unwrap();
    assert_eq!(first.version, 1);
    let error = store
        .write("acme.dashboard", "board", 0, json!({"items":[1]}))
        .unwrap_err();
    assert!(error.to_string().contains("expected=0"));
    let second = store
        .write("acme.dashboard", "board", 1, json!({"items":[1]}))
        .unwrap();
    assert_eq!(store.read("acme.dashboard", "board").unwrap(), second);
}
