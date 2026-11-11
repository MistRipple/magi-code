use magi_plugin_system::PluginPackage;
use serde_json::{Value, json};
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

fn manifest() -> Value {
    json!({
        "sdkVersion":2,"id":"acme.dashboard","version":"1.0.0","name":"Dashboard","description":"", "permissions":[],"dataSchemaVersion":1,
        "settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{"resources":[{"id":"open","title":"Open","description":""}],"views":[{"id":"dashboard","title":"Dashboard","entry":"ui/index.html","placements":["rightPane","main"]}]}
    })
}

fn archive(manifest: &Value, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(manifest).unwrap()),
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
    assert!(package.files().contains_key("ui/index.html"));
    assert_eq!(
        PluginPackage::from_archive(&bytes).unwrap().digest(),
        package.digest()
    );
}

#[test]
fn sdk_one_packages_are_rejected_with_an_explicit_reason() {
    for (field, value) in [("sdkVersion", json!(1)), ("backend", json!("plugin.mjs"))] {
        let mut m = manifest();
        m[field] = value;
        let error = PluginPackage::from_archive(&archive(&m, &[("plugin.mjs", b"x")]))
            .expect_err("SDK 1 必须被拒绝");
        assert!(error.to_string().contains("SDK 1"), "{field}: {error}");
    }
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
        ("sdkVersion", json!(3)),
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
    m["permissions"] = json!([{"kind":"browser","targets":[]}]);
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_ok());
    m["settingsSchema"] =
        json!({"type":"object","properties":{"key":{"$ref":"file:///etc/passwd"}}});
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    m["settingsSchema"] = json!({"type":"object","properties":"invalid"});
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
}

#[test]
fn oversized_manifest_and_resources_are_rejected_before_execution() {
    let mut m = manifest();
    m["description"] = json!("x".repeat(4097));
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    let payload = vec![b'x'; 4 * 1024 * 1024 + 1];
    assert!(
        PluginPackage::from_archive(&archive(&manifest(), &[("ui/large.css", &payload)])).is_err()
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
    value["permissions"] = json!([{"kind":"browser","targets":["login"]}]);
    value["contributions"]["browserSurfaces"] = json!([{
        "id":"login","title":"Login","description":"","persistence":"persistent","keepMounted":true,"closeAction":"hide","backgroundInference":true
    }]);
    mutate(&mut value);
    value
}

#[test]
fn browser_surface_contributions_are_declarations_backed_by_browser_permission() {
    let valid = surface_manifest(|_| {});
    let package = PluginPackage::from_archive(&archive(&valid, &[])).unwrap();
    assert_eq!(package.manifest().contributions.browser_surfaces.len(), 1);

    // 没有同目标的 browser 权限：声明不能代替授权。
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

    empty["permissions"] = json!([{"kind":"storage","targets":["__settings"]}]);
    assert!(PluginPackage::from_archive(&archive(&empty, &[])).is_ok());
}

#[test]
fn permissions_without_an_enforcement_point_are_rejected() {
    // 文件与网络权限没有任何强制点：受信后台直接以用户权限运行，清单不能声明装饰性 OS 权限。
    for kind in ["filesRead", "filesWrite", "network"] {
        let mut value = manifest();
        value["permissions"] = json!([{"kind": kind, "targets": ["x"]}]);
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
        "sdkVersion":2,"id":"acme.guard","version":"1.0.0","name":"Guard","description":"",
        "extension":{"hooks":[{"name":"tool.call","mode":"guard","deadlineMs":3000}]},
        "permissions":[{"kind":"extension","targets":["full-trust"]}],
        "dataSchemaVersion":1,
        "settingsSchema":{"type":"object","additionalProperties":false},
        "contributions":{}
    })
}

const ENTRY: &[(&str, &[u8])] = &[("extension/index.mjs", b"export default async () => {}")];

#[test]
fn upstream_binary_assets_require_a_trusted_node_extension_and_preserve_original_paths() {
    let mut translated = extension_manifest();
    translated["extension"]["compat"] = json!({"kind":"dsh"});
    for (path, payload) in [
        ("extension/upstream/vendor/auth.wasm", b"\0asm\x01\0\0\0".as_slice()),
        ("extension/upstream/assets/runtime.000.part", b"MZpayload"),
    ] {
        let mut files = ENTRY.to_vec(); files.push((path, payload));
        let package = PluginPackage::from_archive(&archive(&translated, &files)).unwrap();
        assert_eq!(package.files().get(path).unwrap(), payload);
        // 同一公共资源合同适用于原生 Node 插件，不依赖 DSH compat 身份。
        assert!(PluginPackage::from_archive(&archive(&extension_manifest(), &files)).is_ok());
        assert!(PluginPackage::from_archive(&archive(&manifest(), &[(path, payload)])).is_err());
        let mut untrusted = translated.clone(); untrusted["permissions"] = json!([]);
        assert!(PluginPackage::from_archive(&archive(&untrusted, &files)).is_err());
    }
    for path in ["ui/hidden.js", "extension/hidden.js", "extension/upstream/../hidden.wasm"] {
        let mut files = ENTRY.to_vec(); files.push((path, b"\0asm\x01\0\0\0"));
        assert!(PluginPackage::from_archive(&archive(&translated, &files)).is_err(), "{path}");
    }
}

#[test]
fn resource_bundles_keep_an_aggregate_limit_above_the_old_package_capacity() {
    let data = vec![0u8; 4 * 1024 * 1024];
    let names: Vec<_> = (0..17).map(|index| format!("extension/upstream/assets/{index}.bin")).collect();
    let bytes = |count: usize| {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (path, contents) in [("manifest.json", serde_json::to_vec(&extension_manifest()).unwrap()), ("extension/index.mjs", b"export default () => {}".to_vec())] {
            writer.start_file(path, options).unwrap(); writer.write_all(&contents).unwrap();
        }
        for name in names.iter().take(count) {
            writer.start_file(name, options).unwrap(); writer.write_all(&data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    };
    assert!(PluginPackage::from_archive(&bytes(9)).is_ok());
    assert!(PluginPackage::from_archive(&bytes(17)).unwrap_err().to_string().contains("展开大小超限"));
}

#[test]
fn session_messages_require_explicit_scopes_and_a_trusted_extension() {
    for targets in [
        json!(["context"]),
        json!(["wake"]),
        json!(["context", "wake"]),
    ] {
        let mut value = extension_manifest();
        value["permissions"]
            .as_array_mut()
            .unwrap()
            .push(json!({"kind":"sessionWrite","targets":targets}));
        assert!(PluginPackage::from_archive(&extension_archive(&value, ENTRY)).is_ok());
    }
    for targets in [json!([]), json!(["*"]), json!(["other-session"])] {
        let mut value = extension_manifest();
        value["permissions"]
            .as_array_mut()
            .unwrap()
            .push(json!({"kind":"sessionWrite","targets":targets}));
        assert!(PluginPackage::from_archive(&extension_archive(&value, ENTRY)).is_err());
    }
    let mut page = manifest();
    page["permissions"] = json!([{"kind":"sessionWrite","targets":["context"]}]);
    assert!(PluginPackage::from_archive(&archive(&page, &[])).is_err());
}

#[test]
fn session_directory_permission_requires_an_extension_and_an_exact_read_scope() {
    let mut manifest = extension_manifest();
    manifest["permissions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kind":"sessionRead","targets":["directory"]}));
    assert!(PluginPackage::from_archive(&extension_archive(&manifest, ENTRY)).is_ok());
    for targets in [
        json!([]),
        json!(["*"]),
        json!(["messages"]),
        json!(["directory", "write"]),
    ] {
        manifest["permissions"][1]["targets"] = targets;
        assert!(PluginPackage::from_archive(&extension_archive(&manifest, ENTRY)).is_err());
    }
    let mut without_extension = self::manifest();
    without_extension["permissions"] = json!([{"kind":"sessionRead","targets":["directory"]}]);
    assert!(PluginPackage::from_archive(&archive(&without_extension, &[])).is_err());
}

#[test]
fn node_extension_packages_carry_hooks_trust_and_a_single_bundled_entry() {
    let package =
        PluginPackage::from_archive(&extension_archive(&extension_manifest(), ENTRY)).unwrap();
    assert!(package.manifest().extension.is_some());
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
        "不得携带 SDK 1 后台入口",
    );
    reject(
        |m| m["backend"] = "plugin.mjs".into(),
        ENTRY,
        "不得声明 SDK 1 后台",
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
    qjs["permissions"] = json!([{"kind":"extension","targets":["full-trust"]}]);
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
    let trust = json!({"kind":"extension","targets":["full-trust"]});
    let rewrite = json!({"kind":"contextRewrite","targets":["history"]});
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
    let wrong_target = json!({"kind":"contextRewrite","targets":["all"]});
    assert!(build(&rewrite_hook, vec![&trust, &wrong_target]).is_err());
    let mut qjs = manifest();
    qjs["permissions"] = json!([rewrite]);
    assert!(PluginPackage::from_archive(&archive(&qjs, &[])).is_err());
}

fn tool_manifest() -> Value {
    let mut m = extension_manifest();
    m["extension"]["hooks"] = json!([]);
    m["permissions"] = json!([
        {"kind":"extension","targets":["full-trust"]},
        {"kind":"tools","targets":["echo"]}
    ]);
    m["contributions"] = json!({"tools":[{
        "id":"echo","title":"Echo","description":"","readOnly":true,
        "inputSchema":{"type":"object"}
    }]});
    m
}

#[test]
fn tool_descriptions_allow_long_guidance_but_stay_bounded() {
    let mut m = tool_manifest();
    m["contributions"]["tools"][0]["description"] = json!("工".repeat(10_000));
    let ok = PluginPackage::from_archive(&extension_archive(&m, ENTRY));
    assert!(ok.is_ok(), "{:?}", ok.err());
    m["contributions"]["tools"][0]["description"] = json!("x".repeat(32 * 1024 + 1));
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
    // 其他贡献的说明上限不变。
    let mut m = tool_manifest();
    m["contributions"]["tools"][0]["description"] = json!("");
    m["contributions"]["commands"] =
        json!([{"id":"go","title":"Go","description":"x".repeat(4097)}]);
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
}

#[test]
fn extension_tools_need_a_tools_permission_and_an_extension() {
    let ok = PluginPackage::from_archive(&extension_archive(&tool_manifest(), ENTRY));
    assert!(ok.is_ok(), "{:?}", ok.err());
    // 缺少覆盖该工具的 tools 权限。
    let mut m = tool_manifest();
    m["permissions"] = json!([{"kind":"extension","targets":["full-trust"]}]);
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
    // 没有 extension 的包不能声明工具。
    let mut m = tool_manifest();
    m.as_object_mut().unwrap().remove("extension");
    m["permissions"] = json!([{"kind":"tools","targets":["echo"]}]);
    assert!(PluginPackage::from_archive(&extension_archive(&m, &[])).is_err());
    // 工具参数 schema 必须是有界的 object schema。
    let mut m = tool_manifest();
    m["contributions"]["tools"][0]["inputSchema"] = json!({"type":"string"});
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
}

#[test]
fn tool_timeout_is_bounded_and_optional() {
    let mut m = tool_manifest();
    m["contributions"]["tools"][0]["timeoutMs"] = json!(570_000);
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_ok());
    for bad in [json!(999), json!(600_001), json!(0), json!(-5)] {
        m["contributions"]["tools"][0]["timeoutMs"] = bad;
        assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
    }
    m["contributions"]["tools"][0]["timeoutMs"] = Value::Null;
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_ok());
}

fn model_source_manifest() -> Value {
    let mut m = extension_manifest();
    m["extension"]["hooks"] = json!([]);
    m["permissions"] = json!([
        {"kind":"extension","targets":["full-trust"]},
        {"kind":"modelSources","targets":["proxy"]}
    ]);
    m["contributions"] = json!({"modelSources":[{"id":"proxy","title":"Proxy","description":""}]});
    m
}

#[test]
fn model_sources_need_their_own_authorization_and_an_extension() {
    // 只提供模型数据源的扩展没有钩子、工具或命令也成立。
    let ok = PluginPackage::from_archive(&extension_archive(&model_source_manifest(), ENTRY));
    assert!(ok.is_ok(), "{:?}", ok.err());
    // 缺少覆盖该数据源的 modelSources 权限（models 权限不能替代：它是插件使用 Magi 的模型）。
    let mut m = model_source_manifest();
    m["permissions"] = json!([
        {"kind":"extension","targets":["full-trust"]},
        {"kind":"models","targets":["default"]}
    ]);
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
    // 没有 extension 的包既不能声明数据源，也不能声明这项权限。
    let mut m = model_source_manifest();
    m.as_object_mut().unwrap().remove("extension");
    m["permissions"] = json!([{"kind":"modelSources","targets":["proxy"]}]);
    assert!(PluginPackage::from_archive(&extension_archive(&m, &[])).is_err());
    // 数据源 ID 与其他贡献共用命名空间，且有数量上限。
    let mut m = model_source_manifest();
    m["contributions"]["modelSources"] = json!([
        {"id":"proxy","title":"A","description":""},
        {"id":"proxy","title":"B","description":""}
    ]);
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
    let mut m = model_source_manifest();
    m["contributions"]["modelSources"] = json!(
        (0..9)
            .map(|i| json!({"id":format!("s{i}"),"title":"T","description":""}))
            .collect::<Vec<_>>()
    );
    assert!(PluginPackage::from_archive(&extension_archive(&m, ENTRY)).is_err());
}

#[test]
fn browser_and_node_scripts_share_a_budget_with_bounded_package_resources() {
    fn compressed_archive(manifest: &Value, entry: &[u8], other: (&str, &[u8])) -> Vec<u8> {
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(manifest).unwrap()),
            ("ui/index.html", b"<!doctype html><p>dashboard</p>".to_vec()),
            ("extension/index.mjs", entry.to_vec()),
            (other.0, other.1.to_vec()),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    let mut padded = b"export default async () => {};\n".to_vec();
    let manifest = extension_manifest();
    // 所有浏览器与 Node 脚本采用同一合同，不限于一个特殊的扩展入口。
    padded.extend(std::iter::repeat_n(b'/', 5 * 1024 * 1024));
    assert!(
        PluginPackage::from_archive(&compressed_archive(
            &manifest,
            &padded,
            ("ui/extra.js", b"x")
        ))
        .is_ok()
    );
    let other = vec![b'/'; 5 * 1024 * 1024];
    for name in [
        "ui/runtime.js",
        "ui/module.mjs",
        "extension/upstream/lib/index.cjs",
    ] {
        assert!(
            PluginPackage::from_archive(&compressed_archive(
                &manifest,
                b"export default () => {}",
                (name, &other)
            ))
            .is_ok(),
            "{name}"
        );
    }
    // 超限脚本在任何位置均被拒绝，错误提供具体资源及实际容量。
    let mut too_big = b"export default async () => {};\n".to_vec();
    too_big.extend(std::iter::repeat_n(
        b'/',
        magi_plugin_system::MAX_SCRIPT_BYTES,
    ));
    for name in ["ui/runtime.js", "extension/upstream/lib/index.mjs"] {
        let error = PluginPackage::from_archive(&compressed_archive(
            &manifest,
            b"export default () => {}",
            (name, &too_big),
        ))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(name) && error.contains("16777216"),
            "{error}"
        );
    }
    // 资源与展开总量仍有界，脚本容量不扩大 CSS 等其他资源的上限。
    assert!(
        PluginPackage::from_archive(&compressed_archive(
            &manifest,
            b"export default async () => {}",
            ("ui/extra.css", &other)
        ))
        .is_err()
    );
    let full = vec![b'/'; magi_plugin_system::MAX_SCRIPT_BYTES];
    // 两个 16 MiB 脚本现在处于 64 MiB 总量内；上面的单文件拒绝仍有效，
    // 聚合超限由 resource_bundles_keep_an_aggregate_limit_above_the_old_package_capacity 覆盖。
    assert!(
        PluginPackage::from_archive(&compressed_archive(
            &manifest,
            &full,
            ("ui/runtime.js", &full)
        ))
        .is_ok()
    );
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

#[test]
fn icons_preserve_svg_and_common_image_formats_inside_ui() {
    let mut m = manifest();
    m["icon"] = json!("ui/icon.png");
    let package = PluginPackage::from_archive(&archive(&m, &[("ui/icon.png", PNG)])).unwrap();
    let (content_type, bytes) = package.icon().expect("声明的图标可读");
    assert_eq!(content_type, "image/png");
    assert_eq!(bytes, PNG);
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36"><defs><linearGradient id="g"><stop stop-color="red"/></linearGradient></defs><rect width="36" height="36" fill="url(#g)"/></svg>"##;
    m["icon"] = json!("ui/icon.svg");
    let vector = PluginPackage::from_archive(&archive(&m, &[("ui/icon.svg", svg)])).unwrap();
    assert_eq!(vector.icon(), Some(("image/svg+xml", svg.as_slice())));

    // 没有声明图标的包没有图标。
    let plain = PluginPackage::from_archive(&archive(&manifest(), &[])).unwrap();
    assert!(plain.icon().is_none());

    // SVG 图片可用；主动内容、外部引用、类型不符和越界仍拒绝。
    let mut oversized = PNG.to_vec();
    oversized.resize(256 * 1024 + 1, 0);
    for (declared, name, bytes) in [
        ("ui/icon.png", "ui/other.png", PNG.to_vec()),
        (
            "ui/icon.png",
            "ui/icon.png",
            b"<script>alert(1)</script>".to_vec(),
        ),
        ("ui/icon.jpg", "ui/icon.jpg", PNG.to_vec()),
        ("ui/icon.png", "ui/icon.png", oversized),
        ("icon.png", "icon.png", PNG.to_vec()),
        (
            "ui/icon.svg",
            "ui/icon.svg",
            b"<svg><script>alert(1)</script></svg>".to_vec(),
        ),
        (
            "ui/icon.svg",
            "ui/icon.svg",
            b"<svg onload='alert(1)'/>".to_vec(),
        ),
        (
            "ui/icon.svg",
            "ui/icon.svg",
            b"<svg><image href='https://example.test/a.png'/></svg>".to_vec(),
        ),
        (
            "ui/icon.svg",
            "ui/icon.svg",
            b"<!DOCTYPE svg><svg/>".to_vec(),
        ),
    ] {
        let mut m = manifest();
        m["icon"] = json!(declared);
        assert!(
            PluginPackage::from_archive(&archive(&m, &[(name, bytes.as_slice())])).is_err(),
            "{declared} 应被拒绝"
        );
    }
}

#[test]
fn composer_text_and_images_are_distinct_explicit_view_permissions() {
    let mut manifest = self::manifest();
    for targets in [
        json!(["text"]),
        json!(["images"]),
        json!(["images", "text"]),
    ] {
        manifest["permissions"] = json!([{"kind":"composer","targets":targets}]);
        assert!(PluginPackage::from_archive(&archive(&manifest, &[])).is_ok());
    }
    for targets in [
        json!([]),
        json!(["*"]),
        json!(["submit"]),
        json!(["text", "unknown"]),
    ] {
        manifest["permissions"] = json!([{"kind":"composer","targets":targets}]);
        assert!(PluginPackage::from_archive(&archive(&manifest, &[])).is_err());
    }
    manifest["permissions"] = json!([{"kind":"composer","targets":["text"]}]);
    manifest["contributions"]["views"] = json!([]);
    assert!(PluginPackage::from_archive(&archive(&manifest, &[])).is_err());
}

#[test]
fn persistent_clients_require_an_explicit_grant_and_a_declared_view_entry() {
    let mut m = manifest();
    m["contributions"]["client"] = json!({"entry":"ui/index.html"});
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    m["permissions"] = json!([{"kind":"client","targets":["persistent"]}]);
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_ok());
    m["contributions"]["client"]["entry"] = json!("ui/other.html");
    assert!(
        PluginPackage::from_archive(&archive(&m, &[("ui/other.html", b"<p>other</p>")])).is_err()
    );
    m["contributions"].as_object_mut().unwrap().remove("client");
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
    m["contributions"]["client"] = json!({"entry":"ui/index.html"});
    m["permissions"] = json!([{"kind":"client","targets":["*"]}]);
    assert!(PluginPackage::from_archive(&archive(&m, &[])).is_err());
}
