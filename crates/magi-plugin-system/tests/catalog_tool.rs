//! 发布工具 `scripts/plugin-center.mjs` 产出的目录必须能被 daemon 的验签实现接受：
//! 两端各写一份格式处理，靠这条回归保证它们不会悄悄分叉。

use magi_plugin_system::{CatalogError, PluginCatalog, PluginPackage, TrustedKey};
use std::{path::PathBuf, process::Command};

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn node(args: &[&str]) -> Option<std::process::Output> {
    Command::new("node").args(args).output().ok()
}

#[test]
fn catalog_signed_by_the_publish_tool_is_accepted_by_the_daemon_verifier() {
    if node(&["--version"]).is_none() {
        eprintln!("未安装 Node.js，跳过发布工具互操作回归");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let work = directory.path();
    let root = repository();
    let script = |name: &str| {
        root.join("scripts")
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    let path = |name: &str| work.join(name).to_string_lossy().into_owned();

    let archive = path("board.zip");
    let packed = node(&[
        &script("pack-plugin.mjs"),
        &root.join("examples/plugins/board").to_string_lossy(),
        &archive,
    ])
    .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );

    let key_file = path("key.pem");
    let generated = node(&[
        &script("plugin-center.mjs"),
        "keygen",
        &key_file,
        "--key-id",
        "ci-key",
    ])
    .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let output = String::from_utf8(generated.stdout).unwrap();
    let public_key = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("publicKey:"))
        .expect("keygen 必须打印 publicKey")
        .trim()
        .to_owned();

    let catalog_file = path("catalog.json");
    let published = node(&[
        &script("plugin-center.mjs"),
        "publish",
        "--key",
        &key_file,
        "--key-id",
        "ci-key",
        "--base-url",
        "https://example.test/plugins",
        "--out",
        &catalog_file,
        &archive,
    ])
    .unwrap();
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );

    let bytes = std::fs::read(&catalog_file).unwrap();
    let trusted = [TrustedKey {
        key_id: "ci-key".into(),
        public_key,
    }];
    let catalog = PluginCatalog::verify(&bytes, &trusted).unwrap();
    let entry = catalog.latest("example.board").expect("目录包含示例插件");
    let package = PluginPackage::from_archive(&std::fs::read(&archive).unwrap()).unwrap();
    assert_eq!(
        entry.sha256,
        package.digest(),
        "目录摘要与安装包摘要是同一定义"
    );
    assert_eq!(entry.url, "https://example.test/plugins/board.zip");

    // 被篡改的目录或不受信的公钥都不能通过。
    let mut tampered = bytes.clone();
    let position = tampered.iter().position(|byte| *byte == b'A').unwrap();
    tampered[position] = b'B';
    assert!(PluginCatalog::verify(&tampered, &trusted).is_err());
    let stranger = [TrustedKey {
        key_id: "ci-key".into(),
        public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=".into(),
    }];
    assert_eq!(
        PluginCatalog::verify(&bytes, &stranger).unwrap_err(),
        CatalogError::Untrusted
    );

    // 无密钥发布：普通目录，包地址是相对路径，daemon 按未签名目录解析。
    let plain_file = path("plain.json");
    let plain = node(&[
        &script("plugin-center.mjs"),
        "publish",
        "--out",
        &plain_file,
        &archive,
    ])
    .unwrap();
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let parsed = PluginCatalog::parse(&std::fs::read(&plain_file).unwrap(), &[]).unwrap();
    assert!(!parsed.signed);
    assert_eq!(parsed.catalog.plugins[0].url, "board.zip");
    assert_eq!(parsed.catalog.plugins[0].sha256, package.digest());
    // 配置了公钥的用户不会接受这份未签名目录。
    assert_eq!(
        PluginCatalog::parse(&std::fs::read(&plain_file).unwrap(), &trusted).unwrap_err(),
        CatalogError::Unsigned
    );
}
