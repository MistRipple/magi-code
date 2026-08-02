use crate::assets::AppearanceAssetStore;
use crate::contract::ThemePack;
use crate::validator::{theme_content_hash, validate_user_theme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 24 * 1024 * 1024;
const MAX_ENTRIES: usize = 12;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThemeManifest {
    format_version: u32,
    theme_api_version: u32,
    theme_id: String,
    theme_content_hash: String,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    path: String,
    bytes: u64,
    sha256: String,
}

pub struct ImportedThemePackage {
    pub pack: ThemePack,
    pub assets: Vec<Vec<u8>>,
}

pub fn export_theme_package(
    pack: &ThemePack,
    assets: &AppearanceAssetStore,
) -> Result<Vec<u8>, String> {
    validate_user_theme(pack)?;
    let theme_bytes =
        serde_json::to_vec_pretty(pack).map_err(|error| format!("序列化主题失败: {error}"))?;
    let mut files = vec![("theme.json".to_string(), theme_bytes)];
    if let Some(wallpaper) = &pack.wallpaper {
        let (bytes, mime) = assets.read(&wallpaper.asset_id)?;
        let extension = match mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => return Err("主题背景图格式无效".to_string()),
        };
        files.push((
            format!("assets/{}.{}", wallpaper.asset_id, extension),
            bytes,
        ));
    }
    let manifest = ThemeManifest {
        format_version: 1,
        theme_api_version: pack.schema_version,
        theme_id: pack.id.clone(),
        theme_content_hash: theme_content_hash(pack)?,
        files: files
            .iter()
            .map(|(path, bytes)| ManifestFile {
                path: path.clone(),
                bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
            })
            .collect(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("序列化主题清单失败: {error}"))?;
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer
        .start_file("manifest.json", options)
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&manifest_bytes)
        .map_err(|error| error.to_string())?;
    for (path, bytes) in files {
        writer
            .start_file(path, options)
            .map_err(|error| error.to_string())?;
        writer
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
    }
    Ok(writer
        .finish()
        .map_err(|error| error.to_string())?
        .into_inner())
}

pub fn import_theme_package(bytes: &[u8]) -> Result<ImportedThemePackage, String> {
    if bytes.is_empty() || bytes.len() > MAX_PACKAGE_BYTES {
        return Err("主题包必须小于 16 MiB".to_string());
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "主题包不是有效 ZIP".to_string())?;
    if archive.len() < 2 || archive.len() > MAX_ENTRIES {
        return Err("主题包文件数量无效".to_string());
    }
    let mut expanded = 0_u64;
    let mut payloads = HashMap::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|error| error.to_string())?;
        if file.is_dir() {
            continue;
        }
        let path = file
            .enclosed_name()
            .ok_or_else(|| "主题包包含不安全路径".to_string())?;
        let path = path.to_string_lossy().replace('\\', "/");
        if path != "manifest.json" && path != "theme.json" && !path.starts_with("assets/") {
            return Err(format!("主题包包含不支持的文件: {path}"));
        }
        expanded = expanded.saturating_add(file.size());
        if expanded > MAX_EXPANDED_BYTES {
            return Err("主题包解压后内容过大".to_string());
        }
        let mut content = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut content)
            .map_err(|error| error.to_string())?;
        if payloads.insert(path, content).is_some() {
            return Err("主题包包含重复文件".to_string());
        }
    }
    let manifest: ThemeManifest = serde_json::from_slice(
        payloads
            .get("manifest.json")
            .ok_or_else(|| "主题包缺少 manifest.json".to_string())?,
    )
    .map_err(|_| "主题清单格式无效".to_string())?;
    if manifest.format_version != 1 || manifest.theme_api_version != 1 {
        return Err("主题包协议版本不受支持".to_string());
    }
    let declared: std::collections::HashSet<_> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let actual: std::collections::HashSet<_> = payloads
        .keys()
        .filter(|path| path.as_str() != "manifest.json")
        .map(String::as_str)
        .collect();
    if declared != actual {
        return Err("主题包内容与清单不一致".to_string());
    }
    for file in &manifest.files {
        let content = payloads
            .get(&file.path)
            .ok_or_else(|| "主题包文件缺失".to_string())?;
        if content.len() as u64 != file.bytes
            || format!("{:x}", Sha256::digest(content)) != file.sha256
        {
            return Err(format!("主题包文件校验失败: {}", file.path));
        }
    }
    let pack: ThemePack = serde_json::from_slice(
        payloads
            .get("theme.json")
            .ok_or_else(|| "主题包缺少 theme.json".to_string())?,
    )
    .map_err(|_| "主题定义格式无效".to_string())?;
    validate_user_theme(&pack)?;
    if manifest.theme_id != pack.id || manifest.theme_content_hash != theme_content_hash(&pack)? {
        return Err("主题包身份校验失败".to_string());
    }
    let asset_paths = payloads
        .keys()
        .filter(|path| path.starts_with("assets/"))
        .cloned()
        .collect::<Vec<_>>();
    match &pack.wallpaper {
        Some(wallpaper) => {
            if asset_paths.len() != 1 {
                return Err("带背景图的主题包必须且只能包含一个背景资源".to_string());
            }
            let asset_path = &asset_paths[0];
            let expected_prefix = format!("assets/{}.", wallpaper.asset_id);
            if !asset_path.starts_with(&expected_prefix)
                || !asset_path
                    .rsplit_once('.')
                    .is_some_and(|(_, extension)| matches!(extension, "png" | "jpg" | "webp"))
            {
                return Err("主题包背景资源与主题定义不一致".to_string());
            }
            let asset = payloads
                .get(asset_path)
                .ok_or_else(|| "主题包背景资源缺失".to_string())?;
            if format!("{:x}", Sha256::digest(asset)) != wallpaper.asset_id {
                return Err("主题包背景资源身份校验失败".to_string());
            }
        }
        None if !asset_paths.is_empty() => {
            return Err("无背景图主题不能携带额外资源".to_string());
        }
        None => {}
    }
    let assets = payloads
        .into_iter()
        .filter_map(|(path, bytes)| path.starts_with("assets/").then_some(bytes))
        .collect();
    Ok(ImportedThemePackage { pack, assets })
}
