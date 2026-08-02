use image::ImageReader;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 32_000_000;

#[derive(Clone, Debug)]
pub struct AppearanceAssetStore {
    root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct StoredAsset {
    pub asset_id: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
}

impl AppearanceAssetStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| format!("创建主题资源目录失败: {error}"))?;
        Ok(Self { root })
    }

    pub fn put_image(&self, bytes: &[u8]) -> Result<StoredAsset, String> {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err("背景图必须小于 10 MiB".to_string());
        }
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| format!("无法识别背景图格式: {error}"))?;
        let format = reader
            .format()
            .ok_or_else(|| "无法识别背景图格式".to_string())?;
        let (extension, mime_type) = match format {
            image::ImageFormat::Png => ("png", "image/png"),
            image::ImageFormat::Jpeg => ("jpg", "image/jpeg"),
            image::ImageFormat::WebP => ("webp", "image/webp"),
            _ => return Err("仅支持 PNG、JPEG 和 WebP 背景图".to_string()),
        };
        let (width, height) = reader
            .into_dimensions()
            .map_err(|error| format!("读取背景图尺寸失败: {error}"))?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return Err("背景图像素尺寸超出 3200 万像素限制".to_string());
        }
        let asset_id = format!("{:x}", Sha256::digest(bytes));
        let path = self.root.join(format!("{asset_id}.{extension}"));
        if !path.exists() {
            magi_core::fs_atomic::write_atomic(&path, bytes)
                .map_err(|error| format!("保存背景图失败: {error}"))?;
        }
        Ok(StoredAsset {
            asset_id,
            mime_type: mime_type.to_string(),
            width,
            height,
        })
    }

    pub fn read(&self, asset_id: &str) -> Result<(Vec<u8>, String), String> {
        let (path, mime) = self
            .resolve(asset_id)
            .ok_or_else(|| "主题资源不存在".to_string())?;
        let bytes = fs::read(path).map_err(|error| format!("读取主题资源失败: {error}"))?;
        Ok((bytes, mime.to_string()))
    }

    pub fn resolve(&self, asset_id: &str) -> Option<(PathBuf, &'static str)> {
        if asset_id.len() != 64 || !asset_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        [
            ("png", "image/png"),
            ("jpg", "image/jpeg"),
            ("webp", "image/webp"),
        ]
        .into_iter()
        .find_map(|(extension, mime)| {
            let path = self.root.join(format!("{asset_id}.{extension}"));
            path.is_file().then_some((path, mime))
        })
    }

    pub fn cleanup_unreferenced(
        &self,
        referenced: &std::collections::HashSet<String>,
    ) -> Result<(), String> {
        for entry in
            fs::read_dir(&self.root).map_err(|error| format!("读取主题资源目录失败: {error}"))?
        {
            let entry = entry.map_err(|error| format!("读取主题资源失败: {error}"))?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                continue;
            }
            let file_name = entry.file_name();
            let Some(stem) = Path::new(&file_name)
                .file_stem()
                .and_then(|value| value.to_str())
            else {
                continue;
            };
            if !referenced.contains(stem) {
                fs::remove_file(entry.path())
                    .map_err(|error| format!("清理未使用主题资源失败: {error}"))?;
            }
        }
        Ok(())
    }
}
