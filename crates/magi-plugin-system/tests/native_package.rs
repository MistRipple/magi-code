use base64::Engine as _;
use flate2::{Compression, write::GzEncoder};
use magi_plugin_system::npm::NativePackage;
use serde_json::{Value, json};
use sha2::{Digest, Sha512};

fn artifact(manifest: Value, files: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut append = |name: &str, bytes: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("package/{name}"), bytes)
            .unwrap();
    };
    append("package.json", &serde_json::to_vec(&manifest).unwrap());
    for (name, bytes) in files {
        append(name, bytes);
    }
    let bytes = archive.into_inner().unwrap().finish().unwrap();
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&bytes))
    );
    (bytes, integrity)
}

#[test]
fn original_bytes_and_conditional_exports_survive_preparation_without_executing_code() {
    let manifest = json!({"name":"@example/business", "version":"1.2.3", "type":"module",
        "exports":{".":{"types":"./missing.d.ts", "node":{"import":"./index.js", "require":"./platform.cjs"}},"./client":"./client.js"},
        "peerDependencies":{"@deepseek-ai/cordis":"^4.1.0"}, "customMetadata":{"retained":true}});
    let source = b"throw new Error('must not execute during preparation');";
    let (bytes, integrity) = artifact(
        manifest.clone(),
        &[
            ("index.js", source),
            ("client.js", b"window.example = true;"),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let package =
        NativePackage::prepare(&bytes, &integrity, "@example/business", "1.2.3", dir.path())
            .unwrap();
    assert_eq!(package.manifest(), manifest.as_object().unwrap());
    assert_eq!(package.identity().integrity, integrity);
    assert_eq!(package.identity().archive_sha256.len(), 64);
    assert_eq!(package.manifest_sha256().len(), 64);
    assert_eq!(
        std::fs::read(package.directory().join("index.js")).unwrap(),
        source
    );
    assert!(!package.directory().join("manifest.json").exists());
    assert!(!package.directory().join("extension").exists());
}

#[test]
fn bad_integrity_does_not_extract_and_identity_mismatch_cannot_be_accepted() {
    let (bytes, integrity) = artifact(
        json!({"name":"original", "version":"1.0.0", "main":"index.js"}),
        &[],
    );
    let dir = tempfile::tempdir().unwrap();
    let invalid_integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode([0u8; 64])
    );
    assert!(
        NativePackage::prepare(&bytes, &invalid_integrity, "original", "1.0.0", dir.path())
            .is_err()
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(NativePackage::prepare(&bytes, &integrity, "different", "1.0.0", dir.path()).is_err());
}

#[test]
fn malformed_entry_paths_dependency_shapes_and_nonempty_staging_are_rejected() {
    for change in [
        json!({"exports":{".":"../outside.js"}}),
        json!({"exports":{".":"./../outside.js"}}),
        json!({"exports":{".":"./node_modules/another/index.js"}}),
        json!({"dependencies":[]}),
        json!({"peerDependencies":{"other":null}}),
        json!({"exports":{".":"./index.js","default":"./index.js"}}),
    ] {
        let mut manifest = json!({"name":"original","version":"1.0.0","main":"index.js"});
        manifest
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let (bytes, integrity) = artifact(manifest, &[("index.js", b"export function apply() {}")]);
        let dir = tempfile::tempdir().unwrap();
        assert!(
            NativePackage::prepare(&bytes, &integrity, "original", "1.0.0", dir.path()).is_err(),
            "{change}"
        );
    }
    let (bytes, integrity) = artifact(
        json!({"name":"original","version":"1.0.0","main":"index.js"}),
        &[],
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("existing"), "keep").unwrap();
    assert!(NativePackage::prepare(&bytes, &integrity, "original", "1.0.0", dir.path()).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("existing")).unwrap(),
        "keep"
    );
}
