//! 上游原包的进程内派生缓存。图标、预览和安装共用已核对的发布字节，
//! 不缓存翻译产物、权限或安装事实；相同 URL／integrity 的并发获取合并。
use super::*;
use magi_plugin_system::npm::NativePackage;
use std::{
    collections::VecDeque,
    sync::{Arc, LazyLock},
};
use tokio::sync::{Mutex, OnceCell, Semaphore};

type Bytes = Arc<Vec<u8>>;
type Artifact = Arc<OnceCell<Bytes>>;
type ArtifactCache = VecDeque<(String, Artifact, usize)>;
const MAX_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 32;
static CACHE: LazyLock<Mutex<ArtifactCache>> = LazyLock::new(|| Mutex::new(VecDeque::new()));
static DOWNLOADS: Semaphore = Semaphore::const_new(3);

/// Verified registry bytes and their exact package identity. The caller owns
/// the temporary project and the later installation commit.
#[derive(Clone)]
pub struct NpmArchive {
    name: String,
    version: String,
    pub(super) integrity: String,
    pub(super) bytes: Bytes,
}

impl NpmArchive {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn integrity(&self) -> &str {
        &self.integrity
    }

    pub fn sha256(&self) -> String {
        use sha2::{Digest as _, Sha256};
        magi_core::hex_lower(Sha256::digest(self.bytes.as_slice()))
    }

    /// Validate an already-retained or local tarball archive and capture its exact identity.
    pub fn from_tarball_bytes(bytes: Vec<u8>) -> Result<Self, ApiError> {
        use base64::Engine as _;
        use sha2::{Digest as _, Sha512};
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&bytes))
        );
        let staging = tempfile::tempdir()
            .map_err(|error| ApiError::internal_assembly("创建原包校验目录失败", error))?;
        let prepared = NativePackage::prepare_tarball(&bytes, &integrity, staging.path())
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let identity = prepared.identity();
        Ok(Self {
            name: identity.name.clone(),
            version: identity.version.clone(),
            integrity,
            bytes: Arc::new(bytes),
        })
    }

    /// Preserve the checked registry bytes for a package manager's file input.
    pub fn write_archive(&self, path: &FsPath) -> std::io::Result<()> {
        use std::io::Write;
        std::fs::File::create_new(path)?.write_all(&self.bytes)
    }

    /// Extract the original package into an empty, private staging directory.
    pub fn stage(&self, directory: &FsPath) -> Result<NativePackage, ApiError> {
        NativePackage::prepare(
            &self.bytes,
            &self.integrity,
            &self.name,
            &self.version,
            directory,
        )
        .map_err(|error| ApiError::InvalidInput(error.to_string()))
    }
}

/// Resolve and fetch an exact npm release through the same validated registry
/// and artifact cache used by plugin discovery and preview.
pub async fn fetch_npm_archive(name: &str, version: &str) -> Result<NpmArchive, ApiError> {
    let (archive, _) = registry_release(name, Some(version), None).await?;
    Ok(archive)
}

/// Resolve and fetch either an exact version or `latest` from a validated registry URL.
pub async fn fetch_npm_release_archive(
    registry_override: Option<&str>,
    name: &str,
    exact_version: Option<&str>,
) -> Result<NpmArchive, ApiError> {
    let resolved_registry = match registry_override {
        Some(custom) => validated_remote_url(custom.trim())?
            .trim_end_matches('/')
            .to_string(),
        None => registry()?,
    };
    let document = registry_document(&resolved_registry, name, exact_version).await?;
    let (archive, _) = registry_release(name, exact_version, Some(document)).await?;
    Ok(archive)
}

/// Fetch a registry packument for read-only version inspection and listing.
pub async fn fetch_npm_packument(
    registry_override: Option<&str>,
    name: &str,
) -> Result<(String, serde_json::Value), ApiError> {
    validate_package_name(name)?;
    let resolved_registry = match registry_override {
        Some(custom) => validated_remote_url(custom.trim())?
            .trim_end_matches('/')
            .to_string(),
        None => registry()?,
    };
    let bytes = download_bytes(
        &format!("{resolved_registry}/{}", package_path(name)),
        MAX_REGISTRY_DOCUMENT_BYTES,
        "上游插件版本列表",
    )
    .await?;
    let packument: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::InvalidInput("registry 包文档格式无效".into()))?;
    if packument["name"].as_str() != Some(name) {
        return Err(ApiError::InvalidInput(
            "registry 返回的包名与请求不一致".into(),
        ));
    }
    Ok((resolved_registry, packument))
}

fn validate_release_request(name: &str, exact_version: Option<&str>) -> Result<(), ApiError> {
    validate_package_name(name)?;
    if let Some(version) = exact_version {
        semver::Version::parse(version)
            .map_err(|_| ApiError::InvalidInput("版本必须是精确的语义版本".into()))?;
    }
    Ok(())
}

fn check_document_identity(
    document: &VersionDocument,
    name: &str,
    exact_version: Option<&str>,
) -> Result<(), ApiError> {
    if document.name != name
        || exact_version.is_some_and(|version| document.version != version)
        || semver::Version::parse(&document.version).is_err()
    {
        return Err(ApiError::InvalidInput(
            "registry 返回的包名或版本与请求不一致".into(),
        ));
    }
    Ok(())
}

pub(super) async fn registry_document(
    registry: &str,
    name: &str,
    exact_version: Option<&str>,
) -> Result<VersionDocument, ApiError> {
    validate_release_request(name, exact_version)?;
    let version = exact_version.unwrap_or("latest");
    let bytes = download_bytes(
        &format!("{registry}/{}/{version}", package_path(name)),
        MAX_REGISTRY_DOCUMENT_BYTES,
        "上游插件元数据",
    )
    .await?;
    let document = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::InvalidInput("registry 元数据格式无效".into()))?;
    check_document_identity(&document, name, exact_version)?;
    Ok(document)
}

pub(super) async fn registry_release(
    name: &str,
    exact_version: Option<&str>,
    document: Option<VersionDocument>,
) -> Result<(NpmArchive, VersionDocument), ApiError> {
    let document = if let Some(document) = document {
        validate_release_request(name, exact_version)?;
        check_document_identity(&document, name, exact_version)?;
        document
    } else {
        registry_document(&registry()?, name, exact_version).await?
    };
    let integrity =
        document.dist.integrity.clone().ok_or_else(|| {
            ApiError::InvalidInput("该版本没有 sha512 完整性摘要，拒绝安装".into())
        })?;
    let bytes = tarball(&document.dist.tarball, &integrity, "上游插件包").await?;
    Ok((
        NpmArchive {
            name: document.name.clone(),
            version: document.version.clone(),
            integrity,
            bytes,
        },
        document,
    ))
}

pub(super) async fn tarball(url: &str, integrity: &str, what: &str) -> Result<Bytes, ApiError> {
    // key 同时绑定来源和内容身份；registry 的新版本声明仍每次读取。
    let key = format!("{url}\0{integrity}");
    let cell = {
        let mut cache = CACHE.lock().await;
        if let Some(index) = cache.iter().position(|(saved, _, _)| saved == &key) {
            let entry = cache.remove(index).unwrap();
            let cell = entry.1.clone();
            cache.push_back(entry);
            cell
        } else {
            let cell = Arc::new(OnceCell::new());
            if cache.len() >= MAX_CACHE_ENTRIES {
                cache.pop_front();
            }
            cache.push_back((key, cell.clone(), 0));
            cell
        }
    };
    let bytes = cell
        .get_or_try_init(|| async {
            let _permit = DOWNLOADS
                .acquire()
                .await
                .map_err(|error| ApiError::internal_assembly("获取上游原包失败", error))?;
            let bytes = download_bytes(url, npm::MAX_TARBALL_BYTES, what).await?;
            npm::verify_integrity(&bytes, integrity)
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
            let bytes = Arc::new(bytes);
            // 在 OnceCell 发布前，持有缓存锁登记真实体积并执行逐出；
            // 并发完成或调用取消不能留下超过字节预算的缓存。
            let mut cache = CACHE.lock().await;
            if let Some((_, _, size)) = cache
                .iter_mut()
                .find(|(_, saved, _)| Arc::ptr_eq(saved, &cell))
            {
                *size = bytes.len();
            }
            prune(&mut cache, MAX_CACHE_BYTES);
            Ok::<_, ApiError>(bytes)
        })
        .await?
        .clone();
    Ok(bytes)
}

fn prune(cache: &mut ArtifactCache, limit: usize) {
    let mut total: usize = cache.iter().map(|(_, _, size)| *size).sum();
    while total > limit {
        let Some((_, _, size)) = cache.pop_front() else {
            break;
        };
        total = total.saturating_sub(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use sha2::{Digest, Sha512};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn integrity(bytes: &[u8]) -> String {
        use base64::Engine as _;
        format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
        )
    }

    #[tokio::test]
    async fn resolved_document_cannot_substitute_another_release() {
        for (name, version) in [
            ("different-package", "1.0.0"),
            ("expected-package", "2.0.0"),
        ] {
            let document = serde_json::from_value(serde_json::json!({
                "name": name, "version": version,
                "dist": {"tarball":"invalid-url", "integrity":integrity(b"payload")},
            }))
            .unwrap();
            let result = registry_release("expected-package", Some("1.0.0"), Some(document)).await;
            assert!(
                matches!(result, Err(ApiError::InvalidInput(message)) if message.contains("包名或版本"))
            );
        }
    }

    #[tokio::test]
    async fn concurrent_consumers_share_verified_bytes_and_new_identity_refetches() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/source.tgz", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let router = Router::new().route(
            "/source.tgz",
            get(move || {
                let counted = counted.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    match counted.fetch_add(1, Ordering::SeqCst) {
                        0 => "first",
                        _ => "second",
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let hash = integrity(b"first");
        let (icon, preview, install) = tokio::join!(
            tarball(&url, &hash, "图标包"),
            tarball(&url, &hash, "预览包"),
            tarball(&url, &hash, "安装包")
        );
        let (icon, preview, install) = (icon.unwrap(), preview.unwrap(), install.unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&icon, &preview) && Arc::ptr_eq(&preview, &install));
        assert!(Arc::ptr_eq(
            &install,
            &tarball(&url, &hash, "安装包").await.unwrap()
        ));
        assert_eq!(
            &**tarball(&url, &integrity(b"second"), "升级包")
                .await
                .unwrap(),
            b"second"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn integrity_failure_is_not_cached_as_a_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/source.tgz", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let router = Router::new().route(
            "/source.tgz",
            get(move || {
                let counted = counted.clone();
                async move {
                    if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                        "corrupt"
                    } else {
                        "valid"
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let hash = integrity(b"valid");
        assert!(
            tarball(&url, &hash, "插件包")
                .await
                .unwrap_err()
                .message()
                .contains("摘要")
        );
        assert_eq!(&**tarball(&url, &hash, "插件包").await.unwrap(), b"valid");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[test]
    fn byte_eviction_releases_cache_ownership_without_invalidating_active_readers() {
        let original = Arc::new(vec![1; 4]);
        let make = |bytes: Bytes| {
            let cell = OnceCell::new();
            cell.set(bytes).unwrap();
            Arc::new(cell)
        };
        let mut cache = VecDeque::from([
            ("old".into(), make(original.clone()), 4),
            ("pending".into(), Arc::new(OnceCell::new()), 0),
            ("new".into(), make(Arc::new(vec![2; 3])), 3),
        ]);
        prune(&mut cache, 5);
        assert_eq!(
            cache
                .iter()
                .map(|(key, _, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["pending", "new"]
        );
        assert_eq!(Arc::strong_count(&original), 1);
        assert_eq!(&**original, &[1; 4]);
        cache[0].2 = 4; // 在发布另一个正在下载的原包之前登记真实体积。
        prune(&mut cache, 5);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].0, "new");
    }
}
