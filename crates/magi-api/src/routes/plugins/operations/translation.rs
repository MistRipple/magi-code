//! 运行组件的版本来自发行构建和已安装归档；不建立独立升级目录或持久状态。
use super::*;
use magi_app_server_protocol::generated::{
    PluginTranslationRuntimeState as RuntimeState, PluginTranslationRuntimeStatus,
};

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl PluginOperations {
    pub(in crate::routes::plugins) fn translation_runtime_id(&self) -> Result<String, String> {
        let toolchain = self
            .plugin_toolchain
            .as_ref()
            .ok_or("本机缺少插件运行组件，请更新 Magi 后重试")?;
        let bytes = std::fs::read(toolchain.compat.join("dist/translation-runtime.json"))
            .map_err(|_| "插件运行组件缺失，请更新 Magi 后重试")?;
        let manifest: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "插件运行组件版本无效，请更新 Magi 后重试")?;
        let id = manifest["id"].as_str().filter(|id| valid_id(id));
        if manifest["format"] != 1 || id.is_none() {
            return Err("插件运行组件版本无效，请更新 Magi 后重试".into());
        }
        Ok(id.unwrap().to_owned())
    }

    pub(in crate::routes::plugins) fn translation_runtime_status(
        &self,
        package: &PluginPackage,
        current: Result<&str, &String>,
    ) -> Option<PluginTranslationRuntimeStatus> {
        package.manifest().extension.as_ref()?.compat.as_ref()?;
        let report = package
            .files()
            .get("extension/compat.json")
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
        let installed = report
            .as_ref()
            .and_then(|value| value["translationRuntimeId"].as_str())
            .filter(|id| valid_id(id))
            .map(str::to_owned);
        Some(match current {
            Ok(id) => PluginTranslationRuntimeStatus {
                state: if installed.as_deref() == Some(id) {
                    RuntimeState::Current
                } else {
                    RuntimeState::UpdateAvailable
                },
                installed_id: installed,
                current_id: Some(id.into()),
                reason: None,
            },
            Err(reason) => PluginTranslationRuntimeStatus {
                state: RuntimeState::Unavailable,
                installed_id: installed,
                current_id: None,
                reason: Some(reason.clone()),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    fn package(id: Option<&str>) -> PluginPackage {
        let manifest = json!({
            "sdkVersion":2,"id":"dsh.fixture","version":"1.0.0","name":"Fixture","description":"",
            "dataSchemaVersion":1,"settingsSchema":{"type":"object"},
            "extension":{"hooks":[],"compat":{"kind":"dsh"}},
            "permissions":[{"kind":"extension","targets":["full-trust"]}],
            "contributions":{"commands":[{"id":"status","title":"Status","description":""}]}
        });
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("extension/index.mjs", b"export default () => {};".to_vec()),
            (
                "extension/compat.json",
                serde_json::to_vec(&json!({"translationRuntimeId":id})).unwrap(),
            ),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        PluginPackage::from_archive(&writer.finish().unwrap().into_inner()).unwrap()
    }

    #[test]
    fn translation_runtime_compares_the_installed_archive_with_the_shipped_components() {
        let root = tempfile::tempdir().unwrap();
        let state = super::super::super::tests::bare_state(root.path());
        let mut operations = PluginOperations::from_state(&state);
        let current = "a".repeat(64);
        let old = "b".repeat(64);
        let compat = root.path().join("compat");
        std::fs::create_dir_all(compat.join("dist")).unwrap();
        operations.plugin_toolchain = Some(crate::PluginToolchain {
            node: root.path().join("node"),
            compat: compat.clone(),
        });
        assert!(operations.translation_runtime_id().is_err());
        std::fs::write(
            compat.join("dist/translation-runtime.json"),
            json!({"format":1,"id":current}).to_string(),
        )
        .unwrap();
        assert_eq!(operations.translation_runtime_id().unwrap(), current);
        for (installed, expected) in [
            (Some(current.as_str()), RuntimeState::Current),
            (Some(old.as_str()), RuntimeState::UpdateAvailable),
            (None, RuntimeState::UpdateAvailable),
        ] {
            let status = operations
                .translation_runtime_status(&package(installed), Ok(&current))
                .unwrap();
            assert_eq!(status.state, expected);
            assert_eq!(status.installed_id.as_deref(), installed);
            assert_eq!(status.current_id.as_deref(), Some(current.as_str()));
        }
        let unavailable = operations
            .translation_runtime_status(&package(Some(&old)), Err(&"组件缺失".into()))
            .unwrap();
        assert_eq!(unavailable.state, RuntimeState::Unavailable);
        assert_eq!(unavailable.installed_id, Some(old));
        assert!(unavailable.current_id.is_none());
        assert_eq!(unavailable.reason.as_deref(), Some("组件缺失"));
        for invalid in [
            json!({"format":2,"id":current}),
            json!({"format":1,"id":"invalid"}),
        ] {
            std::fs::write(
                compat.join("dist/translation-runtime.json"),
                invalid.to_string(),
            )
            .unwrap();
            assert!(operations.translation_runtime_id().is_err());
        }
    }
}
