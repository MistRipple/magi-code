//! 上游原包的进程内派生缓存。图标、预览和安装共用已核对的发布字节，
//! 不缓存翻译产物、权限或安装事实；相同 URL／integrity 的并发获取合并。
use super::*;
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

    fn integrity(bytes: &[u8]) -> String {
        use base64::Engine as _;
        format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
        )
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
