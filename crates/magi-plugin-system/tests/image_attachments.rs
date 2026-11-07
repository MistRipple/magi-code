use base64::Engine as _;
use magi_app_server_protocol::generated::{PluginImageAttachmentBegin, PluginImageAttachmentRead};
use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
use serde_json::json;
use std::io::Write;

fn package(id: &str, version: &str, schema: u64) -> PluginPackage {
    let manifest = json!({"sdkVersion":2,"id":id,"version":version,"name":"Images","description":"","extension":{"hooks":[]},"permissions":[{"kind":"extension","targets":["full-trust"]},{"kind":"attachments","targets":["images"]}],"dataSchemaVersion":schema,"settingsSchema":{"type":"object"},"contributions":{"commands":[{"id":"capture","title":"Capture","description":""}]}});
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("extension/index.mjs", b"export default () => {};".to_vec()),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    PluginPackage::from_archive(&zip.finish().unwrap().into_inner()).unwrap()
}
fn install(manager: &mut PluginManager, id: &str) {
    let package = package(id, "1.0.0", 1);
    manager
        .install(
            &package,
            PluginSource::Local {
                name: "images.zip".into(),
            },
        )
        .unwrap();
    manager.enable(id).unwrap();
    manager
        .authorize(id, package.manifest().permissions.clone())
        .unwrap();
    manager.activate(id).unwrap();
}
fn image(bytes: &[u8]) -> PluginImageAttachmentBegin {
    PluginImageAttachmentBegin {
        name: "窗口.svg".into(),
        media_type: "image/svg+xml".into(),
        bytes: bytes.len() as u64,
    }
}
fn read(manager: &PluginManager, plugin: &str, id: &str) -> Vec<u8> {
    let result = manager
        .read_image_attachment(
            plugin,
            PluginImageAttachmentRead {
                attachment_id: id.into(),
                offset: 0,
                length: 786432,
            },
        )
        .unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(result.data_base64)
        .unwrap()
}
const SVG: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg' width='2' height='2'><rect width='2' height='2'/></svg>";

#[test]
fn images_survive_restart_deduplicate_payloads_and_release_without_cross_plugin_access() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(dir.path()).unwrap();
    install(&mut manager, "acme.first");
    install(&mut manager, "acme.second");
    let first = manager
        .save_image_attachment("acme.first", image(SVG), SVG)
        .unwrap();
    let second = manager
        .save_image_attachment("acme.second", image(SVG), SVG)
        .unwrap();
    assert_ne!(first.attachment_id, second.attachment_id);
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(
        std::fs::read_dir(dir.path().join("images"))
            .unwrap()
            .count(),
        1
    );
    assert!(
        manager
            .image_attachment("acme.second", &first.attachment_id)
            .is_err()
    );
    assert!(
        manager
            .read_resource("acme.first", &format!("__image:{}", first.attachment_id))
            .is_err()
    );
    drop(manager);
    let mut manager = PluginManager::open(dir.path()).unwrap();
    assert_eq!(
        manager
            .image_attachment("acme.first", &first.attachment_id)
            .unwrap(),
        first
    );
    assert_eq!(read(&manager, "acme.first", &first.attachment_id), SVG);
    manager
        .release_image_attachment("acme.first", &first.attachment_id)
        .unwrap();
    assert!(
        manager
            .image_attachment("acme.first", &first.attachment_id)
            .is_err()
    );
    assert_eq!(read(&manager, "acme.second", &second.attachment_id), SVG);
    assert!(
        !manager
            .release_image_attachment("acme.first", &first.attachment_id)
            .unwrap()
    );
    manager
        .release_image_attachment("acme.second", &second.attachment_id)
        .unwrap();
    assert_eq!(
        std::fs::read_dir(dir.path().join("images"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn failed_commits_corrupt_payloads_revocation_and_upgrade_resets_do_not_forge_images() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(dir.path()).unwrap();
    install(&mut manager, "acme.images");
    assert!(
        manager
            .save_image_attachment("acme.images", image(b"not svg"), b"not svg")
            .is_err()
    );
    let record = manager
        .save_image_attachment("acme.images", image(SVG), SVG)
        .unwrap();
    std::fs::write(dir.path().join("images").join(&record.sha256), b"corrupt").unwrap();
    assert!(
        manager
            .read_image_attachment(
                "acme.images",
                PluginImageAttachmentRead {
                    attachment_id: record.attachment_id.clone(),
                    offset: 0,
                    length: 10
                }
            )
            .is_err()
    );
    manager
        .save_image_attachment("acme.images", image(SVG), SVG)
        .unwrap();
    assert_eq!(read(&manager, "acme.images", &record.attachment_id), SVG);
    manager.deactivate("acme.images").unwrap();
    assert!(
        manager
            .image_attachment("acme.images", &record.attachment_id)
            .is_err()
    );
    let candidate = package("acme.images", "2.0.0", 2);
    assert!(
        manager
            .preview(
                &candidate,
                &PluginSource::Local {
                    name: "images.zip".into()
                }
            )
            .unwrap()
            .data_reset
    );
    let shared = std::sync::Arc::new(std::sync::Mutex::new(manager));
    let drain = PluginManager::begin_upgrade_drain(shared.clone(), "acme.images").unwrap();
    shared
        .lock()
        .unwrap()
        .upgrade(
            &candidate,
            PluginSource::Local {
                name: "images.zip".into(),
            },
            &drain,
        )
        .unwrap();
    drop(drain);
    drop(shared);
    let mut manager = PluginManager::open(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_dir(dir.path().join("images"))
            .unwrap()
            .count(),
        0,
        "数据重置后的孤立载荷应回收"
    );
    manager
        .authorize("acme.images", candidate.manifest().permissions.clone())
        .unwrap();
    manager.activate("acme.images").unwrap();
    assert!(
        manager
            .image_attachment("acme.images", &record.attachment_id)
            .is_err()
    );
}

#[test]
fn image_metadata_failure_leaves_no_readable_attachment_and_orphans_are_collected() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = PluginManager::open(dir.path()).unwrap();
    install(&mut manager, "acme.images");
    let path = dir.path().join("resources.json");
    std::fs::create_dir(&path).unwrap();
    assert!(
        manager
            .save_image_attachment("acme.images", image(SVG), SVG)
            .is_err()
    );
    assert!(!manager.has_resource_data("acme.images"));
    std::fs::remove_dir(path).unwrap();
    drop(manager);
    PluginManager::open(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_dir(dir.path().join("images"))
            .unwrap()
            .count(),
        0
    );
}
