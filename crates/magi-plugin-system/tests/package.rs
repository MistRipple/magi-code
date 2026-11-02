use magi_plugin_system::PluginPackage;
use serde_json::{Value, json};
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

fn manifest() -> Value {
    json!({
        "sdkVersion":1,"id":"acme.dashboard","version":"1.0.0","name":"Dashboard","description":"",
        "backend":"plugin.mjs","permissions":[],"dataSchemaVersion":1,
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
fn bundled_manifest_uses_the_same_archive_validation_path() {
    let mut value = manifest();
    value["contributions"]["views"] = json!([]);
    let typed = serde_json::from_value(value).unwrap();
    let package = PluginPackage::from_manifest_and_source(typed, "export default input").unwrap();
    assert_eq!(package.manifest().id, "acme.dashboard");
    assert_eq!(package.source(), "export default input");
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
fn application_permissions_are_accepted_and_schema_cannot_fetch() {
    let mut m = manifest();
    m["permissions"] = json!([{"kind":"browser","scope":"application","targets":[]}]);
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

#[test]
fn home_entry_requires_a_main_page_to_open() {
    let mut orphan = manifest();
    orphan["contributions"]["views"][0]["placements"] = json!(["home"]);
    assert!(PluginPackage::from_archive(&archive(&orphan, &[])).is_err());

    let mut paired = manifest();
    paired["contributions"]["views"][0]["placements"] = json!(["home", "main"]);
    assert!(PluginPackage::from_archive(&archive(&paired, &[])).is_ok());
}

#[test]
fn home_cards_are_declared_views_with_a_per_plugin_limit() {
    let mut card = manifest();
    card["contributions"]["views"][0]["placements"] = json!(["homeCard"]);
    assert!(
        PluginPackage::from_archive(&archive(&card, &[])).is_ok(),
        "卡片不要求主内容区页面"
    );

    let mut crowded = manifest();
    let view = crowded["contributions"]["views"][0].clone();
    let with = |id: &str| {
        let mut copy = view.clone();
        copy["id"] = json!(id);
        copy["placements"] = json!(["homeCard"]);
        copy
    };
    crowded["contributions"]["views"] = json!([with("card-a"), with("card-b"), with("card-c")]);
    let error = PluginPackage::from_archive(&archive(&crowded, &[])).unwrap_err();
    assert!(error.to_string().contains("主页卡片"), "{error}");
}

fn surface_manifest(mutate: impl FnOnce(&mut Value)) -> Value {
    let mut value = manifest();
    value["permissions"] = json!([{"kind":"browser","scope":"application","targets":["login"]}]);
    value["contributions"]["browserSurfaces"] = json!([{
        "id":"login","title":"Login","description":"","scope":"application",
        "persistence":"persistent","keepMounted":true,"closeAction":"hide","backgroundInference":true
    }]);
    mutate(&mut value);
    value
}

#[test]
fn browser_surface_contributions_are_declarations_backed_by_browser_permission() {
    let valid = surface_manifest(|_| {});
    let package = PluginPackage::from_archive(&archive(&valid, &[])).unwrap();
    assert_eq!(package.manifest().contributions.browser_surfaces.len(), 1);

    // 没有同作用域、同目标的 browser 权限：声明不能代替授权。
    let unauthorized = surface_manifest(|value| value["permissions"] = json!([]));
    assert!(PluginPackage::from_archive(&archive(&unauthorized, &[])).is_err());
    let wrong_target = surface_manifest(|value| {
        value["permissions"][0]["targets"] = json!(["other"]);
    });
    assert!(PluginPackage::from_archive(&archive(&wrong_target, &[])).is_err());

    // 后台推理要求 guest 保持挂载。
    let unmounted = surface_manifest(|value| {
        value["contributions"]["browserSurfaces"][0]["keepMounted"] = json!(false);
    });
    assert!(PluginPackage::from_archive(&archive(&unmounted, &[])).is_err());

    // 贡献 ID 与其他贡献共用命名空间。
    let duplicate = surface_manifest(|value| {
        value["contributions"]["browserSurfaces"][0]["id"] = json!("open");
        value["permissions"][0]["targets"] = json!(["open"]);
    });
    assert!(PluginPackage::from_archive(&archive(&duplicate, &[])).is_err());
}

#[test]
fn settings_form_keeps_the_field_order_the_author_wrote() {
    // 在内存里构造的 JSON 对象会按键排序，所以直接写入作者的原始文本。
    let base = serde_json::to_string(&manifest()).unwrap();
    let plain = r#""settingsSchema":{"additionalProperties":false,"type":"object"}"#;
    assert!(base.contains(plain));
    let raw = base.replace(
        plain,
        r#""settingsSchema":{"type":"object","additionalProperties":false,"properties":{"zeta":{"type":"string"},"alpha":{"type":"string"},"mid":{"type":"integer"}}}"#,
    );
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", raw.as_bytes()),
        ("plugin.mjs", b"export default (input) => input".as_slice()),
        (
            "ui/index.html",
            b"<!doctype html><p>dashboard</p>".as_slice(),
        ),
    ] {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    let package = PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap();
    assert_eq!(
        package.manifest().settings_schema["x-property-order"],
        json!(["zeta", "alpha", "mid"]),
        "对象在内存中按键排序，作者顺序必须在解包时恢复"
    );
}

#[test]
fn a_plugin_without_contributions_must_still_declare_the_permissions_it_switches_on() {
    let mut empty = manifest();
    empty["contributions"] = json!({});
    assert!(PluginPackage::from_archive(&archive(&empty, &[])).is_err());

    empty["permissions"] =
        json!([{"kind":"storage","scope":"application","targets":["__settings"]}]);
    assert!(PluginPackage::from_archive(&archive(&empty, &[])).is_ok());
}

#[test]
fn permissions_without_an_enforcement_point_are_rejected() {
    // 文件与网络权限没有任何强制点：受信后台直接以用户权限运行，清单不能声明装饰性 OS 权限。
    for kind in ["filesRead", "filesWrite", "network"] {
        let mut value = manifest();
        value["permissions"] = json!([{"kind": kind, "scope":"application", "targets": ["x"]}]);
        assert!(
            PluginPackage::from_archive(&archive(&value, &[])).is_err(),
            "{kind}"
        );
    }
}

fn extension_archive(manifest: &Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("manifest.json", SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(&serde_json::to_vec(manifest).unwrap())
        .unwrap();
    for (name, bytes) in files {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn extension_manifest() -> Value {
    json!({
        "sdkVersion":1,"id":"acme.guard","version":"1.0.0","name":"Guard","description":"",
        "extension":{"hooks":[{"name":"tool.call","mode":"guard","deadlineMs":3000}]},
        "permissions":[{"kind":"extension","scope":"application","targets":["full-trust"]}],
        "dataSchemaVersion":1,
        "settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{}
    })
}

const ENTRY: &[(&str, &[u8])] = &[("extension/index.mjs", b"export default async () => {}")];

#[test]
fn node_extension_packages_carry_hooks_trust_and_a_single_bundled_entry() {
    let package =
        PluginPackage::from_archive(&extension_archive(&extension_manifest(), ENTRY)).unwrap();
    assert!(package.manifest().backend.is_none());
    assert_eq!(
        package.extension_entry(),
        Some(&b"export default async () => {}"[..])
    );
}

#[test]
fn node_extension_packages_reject_every_ambiguous_or_unsupported_shape() {
    let reject = |mutate: fn(&mut Value), files: &[(&str, &[u8])], why: &str| {
        let mut value = extension_manifest();
        mutate(&mut value);
        assert!(
            PluginPackage::from_archive(&extension_archive(&value, files)).is_err(),
            "{why}"
        );
    };
    reject(|_| {}, &[], "缺少入口");
    reject(
        |_| {},
        &[ENTRY[0], ("plugin.mjs", b"x")],
        "不得同时携带 QuickJS 后台",
    );
    reject(
        |m| m["backend"] = "plugin.mjs".into(),
        &[ENTRY[0], ("plugin.mjs", b"x")],
        "不得同时声明两种后台",
    );
    reject(
        |m| m["permissions"] = json!([]),
        ENTRY,
        "必须声明完全信任权限",
    );
    reject(
        |m| m["extension"]["hooks"] = json!([{"name":"turn.end","mode":"guard"}]),
        ENTRY,
        "观察钩子不能是 guard",
    );
    reject(
        |m| m["extension"]["hooks"] = json!([{"name":"nope","mode":"observe"}]),
        ENTRY,
        "目录外钩子",
    );
    reject(
        |m| {
            m["extension"]["hooks"] = json!([
                {"name":"tool.call","mode":"guard"},{"name":"tool.call","mode":"advise"}
            ])
        },
        ENTRY,
        "钩子重复",
    );
    reject(
        |m| m["contributions"] = json!({"tools":[{"id":"t","title":"T","description":"","inputSchema":{"type":"object"}}]}),
        ENTRY,
        "Node 暂不支持工具贡献",
    );
    reject(
        |m| m["extension"]["hooks"] = json!([{"name":"model.request","mode":"advise"}]),
        ENTRY,
        "未接线的钩子不得在清单里声明",
    );
    reject(
        |m| m["extension"]["hooks"] = json!([{"name":"input","mode":"advise"}]),
        ENTRY,
        "未接线的模式不得声明",
    );
    reject(
        |m| m["extension"]["hooks"] = json!([]),
        ENTRY,
        "至少一个钩子",
    );
    reject(|m| m["extension"]["extra"] = json!(1), ENTRY, "未知字段");
    reject(
        |m| m["extension"]["hooks"][0]["deadlineMs"] = json!(60000),
        ENTRY,
        "deadline 超出协议上限",
    );
    // 声明 extension 权限的包若没有 extension 声明同样无效。
    let mut qjs = manifest();
    qjs["permissions"] =
        json!([{"kind":"extension","scope":"application","targets":["full-trust"]}]);
    assert!(PluginPackage::from_archive(&archive(&qjs, &[])).is_err());
    // 同时携带两种入口文件。
    assert!(
        PluginPackage::from_archive(&archive(&manifest(), &[("extension/index.mjs", b"x")]))
            .is_err()
    );
}

#[test]
fn history_rewrite_hooks_require_the_separate_context_rewrite_authorization() {
    let rewrite_hook = json!([{"name":"context.assemble","mode":"advise"}]);
    let trust = json!({"kind":"extension","scope":"application","targets":["full-trust"]});
    let rewrite = json!({"kind":"contextRewrite","scope":"application","targets":["history"]});
    let build = |hooks: &Value, permissions: Vec<&Value>| {
        let mut value = extension_manifest();
        value["extension"]["hooks"] = hooks.clone();
        value["permissions"] = json!(permissions);
        PluginPackage::from_archive(&extension_archive(&value, ENTRY))
    };
    assert!(build(&rewrite_hook, vec![&trust, &rewrite]).is_ok());
    assert!(
        build(&rewrite_hook, vec![&trust]).is_err(),
        "改写钩子必须声明 contextRewrite"
    );
    assert!(
        build(
            &json!([{"name":"tool.call","mode":"guard"}]),
            vec![&trust, &rewrite]
        )
        .is_err(),
        "没有改写钩子就不能申请 contextRewrite"
    );
    let wrong_target = json!({"kind":"contextRewrite","scope":"application","targets":["all"]});
    assert!(build(&rewrite_hook, vec![&trust, &wrong_target]).is_err());
    let mut qjs = manifest();
    qjs["permissions"] = json!([rewrite]);
    assert!(PluginPackage::from_archive(&archive(&qjs, &[])).is_err());
}
