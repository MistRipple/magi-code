//! 未安装上游包的图标：只读取已核对完整性的原包，不安装依赖或执行插件。
use super::*;
use std::{collections::VecDeque, io::Cursor, path::Component, sync::LazyLock};

type IconCache = VecDeque<(String, Option<Vec<u8>>)>;
static CACHE: LazyLock<tokio::sync::Mutex<IconCache>> =
    LazyLock::new(|| tokio::sync::Mutex::new(VecDeque::new()));
static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);
const MAX_ICONS: usize = 96;
const MAX_ICON_BYTES: u64 = 256 * 1024;

pub(in crate::routes::plugins) async fn icon(
    Query(request): Query<PluginUpstreamRequest>,
) -> Result<Response, ApiError> {
    validate_package_name(&request.package)?;
    let version = request
        .version
        .as_deref()
        .ok_or_else(|| ApiError::InvalidInput("图标请求需要精确版本".into()))?;
    semver::Version::parse(version)
        .map_err(|_| ApiError::InvalidInput("图标请求需要精确版本".into()))?;
    let registry = registry()?;
    let key = format!("{registry}\0{}\0{version}", request.package);
    if let Some(bytes) = cached(&key).await {
        return Ok(response(bytes));
    }
    let _permit = DOWNLOADS
        .acquire()
        .await
        .map_err(|error| ApiError::internal_assembly("读取插件图标失败", error))?;
    if let Some(bytes) = cached(&key).await {
        return Ok(response(bytes));
    }
    let bytes = load(&registry, &request.package, version).await?;
    let mut cache = CACHE.lock().await;
    if cache.len() >= MAX_ICONS {
        cache.pop_front();
    }
    cache.push_back((key, bytes.clone()));
    Ok(response(bytes))
}

async fn cached(key: &str) -> Option<Option<Vec<u8>>> {
    CACHE
        .lock()
        .await
        .iter()
        .find(|(saved, _)| saved == key)
        .map(|(_, bytes)| bytes.clone())
}

fn response(bytes: Option<Vec<u8>>) -> Response {
    let empty = bytes.is_none();
    let mut response = super::super::icon_response("image/png", bytes.unwrap_or_default());
    if empty {
        *response.status_mut() = StatusCode::NO_CONTENT;
    }
    response
}

async fn load(registry: &str, package: &str, version: &str) -> Result<Option<Vec<u8>>, ApiError> {
    let url = format!("{registry}/{}/{version}", package_path(package));
    let bytes = download_bytes(&url, MAX_REGISTRY_DOCUMENT_BYTES, "上游插件图标元数据").await?;
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::InvalidInput("registry 元数据格式无效".into()))?;
    let document: VersionDocument = serde_json::from_value(raw.clone())
        .map_err(|_| ApiError::InvalidInput("registry 元数据格式无效".into()))?;
    if document.name != package || document.version != version {
        return Err(ApiError::InvalidInput("图标包身份与请求不一致".into()));
    }
    // 没有声明就展示通用图标，不猜测 README 图片，也不为此下载整个包。
    let Some(path) = raw.get("icon").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(None);
    };
    validate_path(&path)?;
    let integrity = document
        .dist
        .integrity
        .ok_or_else(|| ApiError::InvalidInput("图标包缺少完整性摘要".into()))?;
    let tarball = download_bytes(
        &document.dist.tarball,
        npm::MAX_TARBALL_BYTES,
        "上游插件图标包",
    )
    .await?;
    npm::verify_integrity(&tarball, &integrity)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    let expected_name = document.name;
    let expected_version = document.version;
    tokio::task::spawn_blocking(move || {
        let work = tempfile::tempdir()
            .map_err(|error| ApiError::internal_assembly("读取图标失败", error))?;
        npm::unpack_tarball(&tarball, work.path())
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(work.path().join("package.json"))
                .map_err(|error| ApiError::internal_assembly("读取图标清单失败", error))?,
        )
        .map_err(|_| ApiError::InvalidInput("图标清单无效".into()))?;
        if manifest["name"].as_str() != Some(expected_name.as_str())
            || manifest["version"].as_str() != Some(expected_version.as_str())
            || manifest["icon"].as_str() != Some(path.as_str())
        {
            return Err(ApiError::InvalidInput("图标清单与 registry 不一致".into()));
        }
        let icon = work.path().join(&path);
        let size = std::fs::metadata(&icon)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?
            .len();
        if size > MAX_ICON_BYTES {
            return Err(ApiError::InvalidInput("图标超过 256 KiB 上限".into()));
        }
        let bytes =
            std::fs::read(icon).map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        normalize(&bytes).map(Some)
    })
    .await
    .map_err(|error| ApiError::internal_assembly("读取图标失败", error))?
}

fn validate_path(path: &str) -> Result<(), ApiError> {
    if path.is_empty()
        || path.contains('\\')
        || !FsPath::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(ApiError::InvalidInput("图标必须是包内文件".into()));
    }
    Ok(())
}

fn normalize(bytes: &[u8]) -> Result<Vec<u8>, ApiError> {
    let format =
        image::guess_format(bytes).map_err(|_| ApiError::InvalidInput("图标不是位图".into()))?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err(ApiError::InvalidInput("只支持 PNG、JPEG、WebP 图标".into()));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| ApiError::InvalidInput("图标位图无效或尺寸超限".into()))?;
    let mut output = Cursor::new(Vec::new());
    image
        .thumbnail(128, 128)
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|error| ApiError::internal_assembly("生成图标失败", error))?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_bounded_bitmap_paths_and_decodable_images_are_accepted() {
        for path in ["../x.png", "/x.png", "ui/../../x.png", "C:\\x.png", ""] {
            assert!(validate_path(path).is_err());
        }
        assert!(validate_path("./icons/logo.png").is_ok());
        assert!(normalize(b"<svg onload='alert(1)'></svg>").is_err());
        assert!(normalize(b"\x89PNG\r\n\x1a\ntruncated").is_err());
        let mut source = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(256, 128)
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap();
        let output = normalize(source.get_ref()).unwrap();
        let image = image::load_from_memory(&output).unwrap();
        assert_eq!((image.width(), image.height()), (128, 64));
    }
}
