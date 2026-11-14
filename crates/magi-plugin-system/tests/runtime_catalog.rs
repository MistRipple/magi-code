use magi_app_server_protocol::generated::PluginRuntimeCatalog;
use magi_plugin_system::{PluginManager, PluginPackage, PluginSource};
use serde_json::json;
use std::io::{Cursor, Write};
use tempfile::{TempDir, tempdir};
use zip::{ZipWriter, write::SimpleFileOptions};

const ID: &str = "acme.runtime";

fn fixture() -> (TempDir, PluginManager, PluginPackage) {
    fixture_with(&["tools", "commands", "settings"])
}

fn fixture_with(kinds: &[&str]) -> (TempDir, PluginManager, PluginPackage) {
    let manifest = json!({
        "sdkVersion": 2, "id": ID, "version": "1.0.0", "name": "Runtime", "description": "",
        "dataSchemaVersion": 1, "settingsSchema": {"type": "object"},
        "permissions": [
            {"kind": "extension", "targets": ["full-trust"]},
            {"kind": "tools", "targets": ["*"]},
            {"kind": "storage", "targets": ["__settings"]}
        ],
        "extension": {"hooks": [], "runtimeContributions": kinds},
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
fn runtime_skills_require_a_declaration_and_share_ready_cas_and_revocation() {
    let (_dir, mut manager, package) = fixture();
    manager
        .begin_runtime(ID, package.digest(), "skills")
        .unwrap();
    let value: PluginRuntimeCatalog = serde_json::from_value(json!({
        "skills":[{"id":"runtime-skill","title":"Runtime skill","description":"Original description","instruction":"Original body"}]
    })).unwrap();
    assert!(
        manager
            .replace_runtime_catalog(ID, "skills", 0, value.clone())
            .is_err(),
        "未声明的动态技能不能进入目录"
    );
    let (_dir, mut manager, package) = fixture_with(&["tools", "skills"]);
    let id = ID;
    manager
        .begin_runtime(id, package.digest(), "skills")
        .unwrap();
    manager
        .replace_runtime_catalog(id, "skills", 0, value.clone())
        .unwrap();
    assert!(
        manager
            .manifests()
            .unwrap()
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .contributions
            .skills
            .is_empty()
    );
    manager.ready_runtime(id, "skills").unwrap();
    assert_eq!(
        manager
            .manifests()
            .unwrap()
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .contributions
            .skills[0]
            .instruction,
        "Original body"
    );
    let mut invalid = value.clone();
    invalid.skills[0].instruction.clear();
    assert!(
        manager
            .replace_runtime_catalog(id, "skills", 1, invalid)
            .is_err()
    );
    assert_eq!(
        manager
            .effective_manifest(id)
            .unwrap()
            .contributions
            .skills
            .len(),
        1
    );
    manager
        .replace_runtime_catalog(id, "skills", 1, PluginRuntimeCatalog::default())
        .unwrap();
    assert!(
        manager
            .effective_manifest(id)
            .unwrap()
            .contributions
            .skills
            .is_empty()
    );
    manager
        .replace_runtime_catalog(id, "skills", 2, value)
        .unwrap();
    manager.disable(id).unwrap();
    assert!(manager.manifests().unwrap().iter().all(|m| m.id != id));
    assert!(
        manager
            .replace_runtime_catalog(id, "skills", 3, PluginRuntimeCatalog::default())
            .is_err()
    );
}

#[test]
fn large_runtime_tool_catalogs_keep_byte_budget_and_count_limits() {
    let (_dir, mut manager, package) = fixture();
    manager
        .begin_runtime(ID, package.digest(), "large")
        .unwrap();
    let mut large = catalog();
    let template = large.tools[0].clone();
    large.tools = (0..300)
        .map(|n| {
            let mut tool = template.clone();
            tool.id = format!("tool-{n}");
            tool
        })
        .collect();
    manager
        .replace_runtime_catalog(ID, "large", 0, large.clone())
        .unwrap();
    let mut over_count = large.clone();
    over_count.tools = (0..1025)
        .map(|n| {
            let mut tool = template.clone();
            tool.id = format!("tool-{n}");
            tool
        })
        .collect();
    assert!(
        manager
            .replace_runtime_catalog(ID, "large", 1, over_count)
            .is_err()
    );
    for tool in &mut large.tools {
        tool.description = "x".repeat(8 * 1024);
    }
    assert!(
        manager
            .replace_runtime_catalog(ID, "large", 1, large)
            .is_err()
    );
}

#[test]
fn runtime_input_schemas_have_their_own_budget_and_keep_reference_validation() {
    let (_dir, mut manager, package) = fixture();
    manager
        .begin_runtime(ID, package.digest(), "schemas")
        .unwrap();
    let mut value = catalog();
    value.tools[0].input_schema["description"] = json!("s".repeat(160 * 1024));
    manager
        .replace_runtime_catalog(ID, "schemas", 0, value.clone())
        .unwrap();
    value.tools[0].input_schema["description"] = json!("s".repeat(256 * 1024));
    assert!(
        manager
            .replace_runtime_catalog(ID, "schemas", 1, value.clone())
            .is_err()
    );
    value.tools[0].input_schema = json!({"type":"object", "$ref":"https://example.test/schema"});
    assert!(
        manager
            .replace_runtime_catalog(ID, "schemas", 1, value)
            .is_err()
    );
    // 拒绝后 revision 不变，仍能以原版本提交有效快照。
    manager
        .replace_runtime_catalog(ID, "schemas", 1, catalog())
        .unwrap();
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
fn disabling_permanently_invalidates_the_previous_lease() {
    let (_dir, mut manager, package) = fixture();
    manager.begin_runtime(ID, package.digest(), "old").unwrap();
    manager
        .replace_runtime_catalog(ID, "old", 0, catalog())
        .unwrap();
    manager.ready_runtime(ID, "old").unwrap();
    assert!(manager.runtime_ready(ID).unwrap());
    manager.disable(ID).unwrap();
    assert!(manager.check_runtime(ID, "old").is_err());
    assert!(!manager.runtime_ready(ID).unwrap());
    assert!(
        manager
            .effective_manifest(ID)
            .unwrap()
            .contributions
            .tools
            .is_empty()
    );
    manager.enable(ID).unwrap();
    assert!(manager.check_runtime(ID, "old").is_err());
    assert!(!manager.runtime_ready(ID).unwrap());
    manager.begin_runtime(ID, package.digest(), "new").unwrap();
    manager.check_runtime(ID, "new").unwrap();
    assert!(!manager.runtime_ready(ID).unwrap());
    manager.ready_runtime(ID, "new").unwrap();
    assert!(manager.runtime_ready(ID).unwrap());
}

#[test]
fn tool_schemas_the_engine_cannot_compile_do_not_invalidate_the_catalog() {
    let (_dir, mut manager, package) = fixture();
    manager.begin_runtime(ID, package.digest(), "zod").unwrap();
    // Zod 生成的“拒绝孤立代理项”写法，Rust 正则无法编译；同一目录里的其他工具必须照常可用。
    let catalog: PluginRuntimeCatalog = serde_json::from_value(json!({"tools": [
        {"id": "zod", "title": "Zod", "description": "", "readOnly": true,
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string", "pattern": r"^[^\uD800-\uDFFF]*$"}}}},
        {"id": "plain", "title": "Plain", "description": "", "readOnly": true,
         "inputSchema": {"type": "object", "properties": {"count": {"type": "integer"}}, "required": ["count"]}}
    ]}))
    .unwrap();
    manager
        .replace_runtime_catalog(ID, "zod", 0, catalog)
        .unwrap();
    manager.ready_runtime(ID, "zod").unwrap();
    let call = |tool: &str, arguments: &str| {
        manager.prepare_tool_call(&magi_plugin_system::model_tool_name(ID, tool), arguments)
    };
    // 编译不了的 schema 不做预校验，交给工具实现方。
    assert!(call("zod", r#"{"text": "anything"}"#).is_ok());
    // 能编译的 schema 照常在 Magi 侧预校验。
    assert!(call("plain", r#"{"count": 1}"#).is_ok());
    assert!(call("plain", r#"{"count": "x"}"#).is_err());
}
