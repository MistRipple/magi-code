//! 未安装上游包的图标：只读取已核对完整性的原包，不安装依赖或执行插件。
use super::*;
use std::{collections::VecDeque, path::Component, sync::LazyLock};

type Icon = (&'static str, Vec<u8>);
type IconCache = VecDeque<(String, Option<Icon>)>;
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

async fn cached(key: &str) -> Option<Option<Icon>> {
    CACHE
        .lock()
        .await
        .iter()
        .find(|(saved, _)| saved == key)
        .map(|(_, bytes)| bytes.clone())
}

fn response(bytes: Option<Icon>) -> Response {
    let empty = bytes.is_none();
    let (mime, bytes) = bytes.unwrap_or(("image/png", Vec::new()));
    let mut response = super::super::icon_response(mime, bytes);
    if empty {
        *response.status_mut() = StatusCode::NO_CONTENT;
    }
    response
}

async fn load(registry: &str, package: &str, version: &str) -> Result<Option<Icon>, ApiError> {
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
        normalize(&path, bytes).map(Some)
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

fn normalize(path: &str, bytes: Vec<u8>) -> Result<Icon, ApiError> {
    magi_plugin_system::validate_icon(path, &bytes)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    let mime = magi_plugin_system::icon_content_type(path)
        .ok_or_else(|| ApiError::InvalidInput("图标类型无效".into()))?;
    Ok((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn online_icons_keep_the_original_format_and_share_package_validation() {
        for path in ["../x.png", "/x.png", "ui/../../x.png", "C:\\x.png", ""] {
            assert!(validate_path(path).is_err());
        }
        assert!(validate_path("./icons/logo.png").is_ok());
        assert!(normalize("icon.svg", b"<svg onload='alert(1)'></svg>".to_vec()).is_err());
        assert!(normalize("icon.png", b"not png".to_vec()).is_err());
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><path d="M0 0L32 32"/></svg>"##.to_vec();
        assert_eq!(
            normalize("icon.svg", svg.clone()).unwrap(),
            ("image/svg+xml", svg)
        );
    }
}
