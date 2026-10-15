//! 仓库内的示例插件必须始终是合法的 Magi 发布包：作者照着示例写的东西不能在安装时才发现过期。

use magi_plugin_system::PluginPackage;
use std::{
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};
use zip::{ZipWriter, write::SimpleFileOptions};

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, root, out);
        } else {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, path));
        }
    }
}

fn pack(directory: &Path) -> Vec<u8> {
    let mut files = Vec::new();
    collect(directory, directory, &mut files);
    files.sort();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, path) in files {
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&fs::read(path).unwrap()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn board_example_is_a_valid_package_using_every_author_facing_contribution() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/board");
    let package = PluginPackage::from_archive(&pack(&directory)).expect("示例插件必须通过安装校验");
    let manifest = package.manifest();
    assert_eq!(manifest.id, "example.board");
    let contributions = &manifest.contributions;
    assert_eq!(contributions.tools.len(), 2);
    assert_eq!(contributions.commands.len(), 1);
    assert_eq!(contributions.resources.len(), 1);
    assert_eq!(contributions.views.len(), 1);
    assert!(package.files().contains_key("ui/index.html"));
    // 示例的后台入口按贡献分派：每个声明的工具和命令都必须在入口里有处理分支。
    let source = package.source();
    for id in contributions
        .tools
        .iter()
        .map(|tool| tool.id.as_str())
        .chain(
            contributions
                .commands
                .iter()
                .map(|command| command.id.as_str()),
        )
    {
        assert!(source.contains(&format!("'{id}'")), "入口没有处理 {id}");
    }
}

#[test]
fn author_guide_examples_are_installable_at_each_tutorial_stage() {
    let guide = include_str!("../../../docs/plugin-authoring.md");
    let section = |number| {
        guide
            .split_once(&format!("\n## {number}. "))
            .expect("指南章节存在")
            .1
            .split("\n## ")
            .next()
            .unwrap()
    };
    let code = |text: &str, language| {
        let marker = format!("```{language}\n");
        text.split_once(&marker)
            .and_then(|(_, tail)| tail.split_once("\n```"))
            .expect("指南须内嵌完整示例文件")
            .0
            .to_owned()
    };
    for (chapter, id) in [(2, "example.echo"), (4, "example.note")] {
        let directory = tempfile::tempdir().unwrap();
        let manifest = code(section(chapter), "json");
        fs::write(directory.path().join("manifest.json"), &manifest).unwrap();
        fs::write(
            directory.path().join("plugin.mjs"),
            code(section(chapter), "js"),
        )
        .unwrap();
        let package = PluginPackage::from_archive(&pack(directory.path()))
            .expect("仅复制指南中的文件就必须能通过安装校验");
        assert_eq!(package.manifest().id, id);
        assert_eq!(package.files().len(), 2);
        if chapter == 4 {
            let mut manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
            manifest["version"] = "1.1.0".into();
            manifest["permissions"][0]["targets"]
                .as_array_mut()
                .unwrap()
                .push("__settings".into());
            manifest["settingsSchema"] = serde_json::from_str(&code(section(5), "json")).unwrap();
            fs::write(directory.path().join("manifest.json"), manifest.to_string()).unwrap();
            PluginPackage::from_archive(&pack(directory.path())).expect("设置教程必须可安装");
            manifest["version"] = "1.2.0".into();
            manifest["contributions"]["views"] = serde_json::from_str::<serde_json::Value>(&code(
                section(6),
                "json",
            ))
            .unwrap()["views"]
                .clone();
            fs::write(directory.path().join("manifest.json"), manifest.to_string()).unwrap();
            fs::create_dir(directory.path().join("ui")).unwrap();
            fs::write(
                directory.path().join("ui/index.html"),
                code(section(6), "html"),
            )
            .unwrap();
            let package = PluginPackage::from_archive(&pack(directory.path()))
                .expect("按页面教程添加视图后必须可安装");
            assert_eq!(package.files().len(), 3);
        }
    }
}
