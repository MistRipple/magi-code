use crate::{PluginError, invalid};
use magi_app_server_protocol::{PluginManifest, PluginScopeKind};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Read},
    sync::OnceLock,
};

pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_UNPACKED_BYTES: usize = 32 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FILES: usize = 512;

/// 已校验的不可变包。安装与执行读取同一份内容，避免校验后再次读取外部源文件。
#[derive(Clone, Debug)]
pub struct PluginPackage {
    manifest: PluginManifest,
    digest: String,
    archive: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
}

impl PluginPackage {
    pub fn from_archive(bytes: &[u8]) -> Result<Self, PluginError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKAGE_BYTES {
            return Err(invalid("发布包大小超限"));
        }
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| invalid("发布包必须是 Magi ZIP 包"))?;
        if archive.len() > MAX_FILES {
            return Err(invalid("发布包文件数量超限"));
        }
        let mut files = BTreeMap::new();
        let mut names = HashSet::new();
        let mut total = 0usize;
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|_| invalid("无法读取发布包目录"))?;
            let name = file.name().trim_end_matches('/').to_string();
            if !valid_path(&name) || !names.insert(name.to_ascii_lowercase()) {
                return Err(invalid("发布包包含非法、重复或大小写冲突路径"));
            }
            let mode = file.unix_mode().unwrap_or(0);
            let kind = mode & 0o170000;
            if !matches!(kind, 0 | 0o100000 | 0o040000) || mode & 0o111 != 0 && !file.is_dir() {
                return Err(invalid("发布包不得包含符号链接、特殊文件或可执行权限文件"));
            }
            if file.is_dir() {
                continue;
            }
            if file.size() > MAX_FILE_BYTES as u64 {
                return Err(invalid("发布包文件大小超限"));
            }
            let mut content = Vec::new();
            file.by_ref()
                .take(MAX_FILE_BYTES as u64 + 1)
                .read_to_end(&mut content)?;
            total = total
                .checked_add(content.len())
                .ok_or_else(|| invalid("发布包大小超限"))?;
            if content.len() > MAX_FILE_BYTES || total > MAX_UNPACKED_BYTES {
                return Err(invalid("发布包展开大小超限"));
            }
            validate_file(&name, &content)?;
            files.insert(name, content);
        }
        let manifest: PluginManifest = serde_json::from_slice(
            files
                .get("manifest.json")
                .ok_or_else(|| invalid("缺少 manifest.json"))?,
        )
        .map_err(|_| invalid("manifest.json 不符合插件合同"))?;
        validate_manifest(&manifest)?;
        let source = files
            .get("plugin.mjs")
            .ok_or_else(|| invalid("缺少 plugin.mjs"))?;
        if source.len() > 512 * 1024 {
            return Err(invalid("后台 ESM bundle 超过 512KiB"));
        }
        for view in &manifest.contributions.views {
            if !valid_path(&view.entry)
                || !view.entry.starts_with("ui/")
                || !view.entry.ends_with(".html")
                || !files.contains_key(&view.entry)
            {
                return Err(invalid("视图入口必须引用包内 ui/ 下的 HTML 文件"));
            }
        }
        Ok(Self {
            manifest,
            digest: format!("{:x}", Sha256::digest(bytes)),
            archive: bytes.to_vec(),
            files,
        })
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
    pub(crate) fn archive_bytes(&self) -> &[u8] {
        &self.archive
    }
    pub fn source(&self) -> &str {
        // from_archive 已验证固定入口存在且为 UTF-8；之后内容不可变。
        std::str::from_utf8(&self.files["plugin.mjs"]).expect("validated plugin source")
    }
}

pub fn validate_manifest(manifest: &PluginManifest) -> Result<(), PluginError> {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    let validator = VALIDATOR.get_or_init(|| {
        let mut schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../contracts/app-server/app-server.schema.json"
        ))
        .expect("repository schema must be valid JSON");
        schema["$ref"] = serde_json::json!("#/$defs/PluginManifest");
        jsonschema::validator_for(&schema).expect("repository plugin schema must compile")
    });
    let serialized = serde_json::to_value(manifest).map_err(|_| invalid("插件清单序列化失败"))?;
    if !validator.is_valid(&serialized) {
        return Err(invalid("插件清单违反版本、字段或大小合同"));
    }
    if manifest.sdk_version != 1 || manifest.data_schema_version < 1 {
        return Err(invalid("仅支持 SDK 1，数据格式版本必须为正整数"));
    }
    if manifest.id.len() > 96
        || manifest.id.split('.').count() < 2
        || !manifest.id.split('.').all(valid_id)
    {
        return Err(invalid("插件 ID 必须包含有效的作者命名空间"));
    }
    if manifest.version.len() > 64
        || semver::Version::parse(&manifest.version).is_err()
        || manifest.name.trim().is_empty()
        || manifest.name.len() > 128
        || manifest.description.len() > 4096
    {
        return Err(invalid("插件版本、名称或说明无效"));
    }
    let mut permissions = HashSet::new();
    if manifest.permissions.len() > 64 {
        return Err(invalid("权限声明超限"));
    }
    for permission in &manifest.permissions {
        if permission.scope == PluginScopeKind::Application && !manifest.application_instance {
            return Err(invalid("应用级权限需要显式声明应用级实例"));
        }
        if permission.targets.len() > 64
            || permission
                .targets
                .iter()
                .any(|t| t.trim().is_empty() || t.len() > 1024)
        {
            return Err(invalid("权限目标无效"));
        }
        let key = serde_json::to_string(permission).map_err(|_| invalid("权限声明无效"))?;
        if !permissions.insert(key) {
            return Err(invalid("权限声明重复"));
        }
    }
    let c = &manifest.contributions;
    let mut ids = HashSet::new();
    let mut check = |id: &str, title: &str, description: &str| -> Result<(), PluginError> {
        if !valid_id(id)
            || id.len() > 64
            || title.trim().is_empty()
            || title.len() > 128
            || description.len() > 4096
            || !ids.insert(id.to_string())
        {
            return Err(invalid("贡献身份、名称或说明无效，或贡献 ID 重复"));
        }
        Ok(())
    };
    for tool in &c.tools {
        check(&tool.id, &tool.title, &tool.description)?;
        validate_object_schema(&tool.input_schema)?;
    }
    for view in &c.views {
        check(&view.id, &view.title, "")?;
        if view.placements.is_empty()
            || view.placements.len() > 4
            || view
                .placements
                .iter()
                .enumerate()
                .any(|(i, p)| view.placements[..i].contains(p))
        {
            return Err(invalid("视图展示位置无效或重复"));
        }
    }
    for item in c
        .commands
        .iter()
        .chain(&c.workflows)
        .chain(&c.engines)
        .chain(&c.resources)
    {
        check(&item.id, &item.title, &item.description)?;
    }
    if ids.is_empty() || ids.len() > 128 {
        return Err(invalid("贡献清单为空或超限"));
    }
    validate_object_schema(&manifest.settings_schema)?;
    Ok(())
}

fn validate_object_schema(schema: &serde_json::Value) -> Result<(), PluginError> {
    if !schema.is_object()
        || schema.get("type").and_then(|x| x.as_str()) != Some("object")
        || serde_json::to_vec(schema).map_or(true, |v| v.len() > 32 * 1024)
    {
        return Err(invalid("配置或工具输入必须声明有界的 object schema"));
    }
    reject_external_references(schema)?;
    jsonschema::validator_for(schema).map_err(|_| invalid("配置或工具输入 schema 无效"))?;
    Ok(())
}

fn reject_external_references(value: &serde_json::Value) -> Result<(), PluginError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                if matches!(key.as_str(), "$ref" | "$dynamicRef")
                    && !child.as_str().is_some_and(|s| s.starts_with('#'))
                {
                    return Err(invalid("插件 schema 不得引用外部文件或网络资源"));
                }
                reject_external_references(child)?;
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                reject_external_references(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment.ends_with('.')
                && segment
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
                && !matches!(
                    segment
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}

fn validate_file(name: &str, bytes: &[u8]) -> Result<(), PluginError> {
    let extension = name.rsplit('.').next().unwrap_or("");
    let text = matches!(extension, "html" | "css" | "js" | "mjs" | "json" | "svg");
    if name != "manifest.json"
        && name != "plugin.mjs"
        && (!name.starts_with("ui/")
            || !matches!(
                extension,
                "html"
                    | "css"
                    | "js"
                    | "mjs"
                    | "json"
                    | "svg"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "woff"
                    | "woff2"
            ))
    {
        return Err(invalid("发布包含未允许的文件类型或入口"));
    }
    if bytes.starts_with(b"MZ")
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"\0asm")
        || bytes.starts_with(b"\xfe\xed\xfa")
        || bytes.starts_with(b"\xcf\xfa\xed\xfe")
        || bytes.starts_with(b"\xca\xfe\xba\xbe")
    {
        return Err(invalid("发布包不得包含原生程序、字节码或 WASM 后台组件"));
    }
    if text && std::str::from_utf8(bytes).is_err() {
        return Err(invalid("插件文本资源必须使用 UTF-8"));
    }
    Ok(())
}
