use magi_plugin_system::PluginPackage;
use serde_json::{Value, json};
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

fn manifest() -> Value {
    json!({
        "sdkVersion":1,"id":"acme.dashboard","version":"1.0.0","name":"Dashboard","description":"",
        "backend":"plugin.mjs","applicationInstance":false,"permissions":[],"dataSchemaVersion":1,
        "settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"commands":[{"id":"open","title":"Open","description":""}],"views":[{"id":"dashboard","title":"Dashboard","entry":"ui/index.html","placements":["rightPane","main"]}]}
    })
}

fn archive(manifest: &Value, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(manifest).unwrap()),
        ("plugin.mjs", b"export default (input) => input".to_vec()),
        ("ui/index.html", b"<!doctype html><p>dashboard</p>".to_vec()),
    ] {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    for (name, bytes) in extra {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn source_views_and_manifest_come_from_one_immutable_validated_package() {
    let bytes = archive(&manifest(), &[]);
    let package = PluginPackage::from_archive(&bytes).unwrap();
    assert_eq!(package.manifest().id, "acme.dashboard");
    assert_eq!(package.digest().len(), 64);
    assert_eq!(package.source(), "export default (input) => input");
    assert!(package.files().contains_key("ui/index.html"));
    assert_eq!(
        PluginPackage::from_archive(&bytes).unwrap().digest(),
        package.digest()
    );
}

#[test]
fn malicious_archive_paths_and_native_payloads_are_rejected() {
    for (name, payload) in [
        ("../escape", b"data".as_slice()),
        ("ui/CON.html", b"data"),
        ("ui/INDEX.HTML", b"data"),
        ("ui/sneaky.js", b"\x7fELFprogram"),
        ("ui/tool.exe", b"MZprogram"),
        ("backend.wasm", b"\0asmprogram"),
    ] {
        assert!(
            PluginPackage::from_archive(&archive(&manifest(), &[(name, payload)])).is_err(),
            "{name}"
        );
    }
}

#[test]
fn undeclared_schema_versions_fields_and_contribution_conflicts_are_rejected() {
    let original = manifest();
    for (field, value) in [
        ("sdkVersion", json!(2)),
        ("backend", json!("plugin.wasm")),
        ("unknownField", json!(true)),
        ("id", json!("../escape")),
        ("version", json!("latest")),
    ] {
        let mut m = original.clone();
        m[field] = value;
        assert!(
            PluginPackage::from_archive(&archive(&m, &[])).is_err(),
            "{field}"
        );
    }
    let mut m = original;
    m["contributions"]["views"][0]["id"] = json!("open");
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
}

#[test]
fn views_cannot_reference_unpacked_missing_or_external_resources() {
    for entry in [
        "https://example.com",
        "ui/missing.html",
        "plugin.mjs",
        "../index.html",
    ] {
        let mut m = manifest();
        m["contributions"]["views"][0]["entry"] = json!(entry);
        assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    }
}

#[test]
fn application_permissions_need_application_instance_and_schema_cannot_fetch() {
    let mut m = manifest();
    m["permissions"] = json!([{"kind":"browser","scope":"application","targets":[]}]);
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    m["applicationInstance"] = json!(true);
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_ok());
    m["settingsSchema"] =
        json!({"type":"object","properties":{"key":{"$ref":"file:///etc/passwd"}}});
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    m["settingsSchema"] = json!({"type":"object","properties":"invalid"});
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
}

#[test]
fn oversized_manifest_and_backend_are_rejected_before_execution() {
    let mut m = manifest();
    m["description"] = json!("x".repeat(4097));
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    let payload = vec![b'x'; 4 * 1024 * 1024 + 1];
    assert!(
        PluginPackage::from_archive(&archive(&manifest(), &[("ui/large.js", &payload)])).is_err()
    );
}
