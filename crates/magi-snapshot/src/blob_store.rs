use crate::error::{SnapshotError, SnapshotResult};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

const ZSTD_LEVEL: i32 = 3;
const TEMP_PREFIX: &str = ".magi-snapshot-tmp";
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 内容寻址 blob 存储，按 sha2-256 前 16 字节（32 hex）命名，
/// 目录拆成两级（前两位作为子目录，避免 inode 爆炸）。
///
/// text blob 走 zstd 压缩；binary blob 直接存原始字节，免得双重压缩浪费 CPU。
/// 落盘走 tmp + sync_all + rename 原子写。
pub struct BlobStore {
    root: PathBuf,
    refcount: Mutex<HashMap<String, u64>>,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> SnapshotResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| SnapshotError::io(&root, e))?;
        Ok(Self {
            root,
            refcount: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 计算 sha2-256 的前 16 字节作为内容寻址键。
    pub fn hash_bytes(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let digest = hasher.finalize();
        let mut out = String::with_capacity(32);
        for byte in &digest[..16] {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        let prefix = if hash.len() >= 2 { &hash[..2] } else { "00" };
        self.root.join(prefix).join(hash)
    }

    /// 写入 blob：text 走 zstd，binary 直接存。返回内容 hash。
    /// 已存在则跳过写入，引用计数 +1。
    pub fn put(&self, content: &[u8], compress: bool) -> SnapshotResult<String> {
        let hash = Self::hash_bytes(content);
        let target = self.blob_path(&hash);

        {
            let mut counts = self.refcount.lock().expect("refcount poisoned");
            *counts.entry(hash.clone()).or_insert(0) += 1;
        }

        if target.exists() {
            return Ok(hash);
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| SnapshotError::io(parent, e))?;
        }

        let payload: Vec<u8> = if compress {
            zstd::stream::encode_all(content, ZSTD_LEVEL)
                .map_err(|e| SnapshotError::Internal(format!("zstd encode failed: {e}")))?
        } else {
            content.to_vec()
        };

        let tmp = temp_path_for(&target);

        {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| SnapshotError::io(&tmp, e))?;
            f.write_all(&payload)
                .map_err(|e| SnapshotError::io(&tmp, e))?;
            f.sync_all().map_err(|e| SnapshotError::io(&tmp, e))?;
        }

        if let Err(error) = fs::rename(&tmp, &target) {
            if target.exists() {
                let _ = fs::remove_file(&tmp);
                return Ok(hash);
            }
            return Err(SnapshotError::io(&target, error));
        }
        Ok(hash)
    }

    /// 读取 blob 内容；text blob 走 zstd 解压。
    pub fn get(&self, hash: &str, compressed: bool) -> SnapshotResult<Vec<u8>> {
        let path = self.blob_path(hash);
        let mut f = File::open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SnapshotError::BlobMissing(hash.to_string()),
            _ => SnapshotError::io(&path, e),
        })?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| SnapshotError::io(&path, e))?;
        if compressed {
            zstd::stream::decode_all(buf.as_slice())
                .map_err(|e| SnapshotError::Internal(format!("zstd decode failed: {e}")))
        } else {
            Ok(buf)
        }
    }

    /// blob 是否已存在。
    pub fn contains(&self, hash: &str) -> bool {
        self.blob_path(hash).exists()
    }

    /// 引用计数 +N。session 启动重放 baseline+events 时使用。
    pub fn retain(&self, hash: &str, n: u64) {
        let mut counts = self.refcount.lock().expect("refcount poisoned");
        *counts.entry(hash.to_string()).or_insert(0) += n;
    }

    /// 释放一个引用，引用计数归零时从磁盘删除。
    pub fn release(&self, hash: &str) -> SnapshotResult<()> {
        // 判定最后一个拥有方与删除必须和 put/retain 串行，否则释放旧引用
        // 可能删除另一线程刚获得的新引用。
        let mut counts = self.refcount.lock().expect("refcount poisoned");
        let drop_now = match counts.get_mut(hash) {
            Some(c) if *c > 1 => {
                *c -= 1;
                false
            }
            Some(_) => {
                counts.remove(hash);
                true
            }
            None => true,
        };
        if drop_now {
            // 未按需加载或已归档会话仍是持久拥有方。只看内存计数会在
            // 重启后删除其它会话尚未加载的 baseline，破坏恢复与回退。
            if self.has_durable_reference(hash)? {
                return Ok(());
            }
            let path = self.blob_path(hash);
            if path.exists() {
                fs::remove_file(&path).map_err(|e| SnapshotError::io(&path, e))?;
            }
        }
        Ok(())
    }

    fn has_durable_reference(&self, hash: &str) -> SnapshotResult<bool> {
        use crate::baseline_index::{BaselineIndex, RefsIndex, baseline_path, refs_path};
        use crate::change_log::ChangeLog;
        let Some(root) = self.root.parent() else {
            return Ok(false);
        };
        let indices = root.join("index");
        let entries = match fs::read_dir(&indices) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(SnapshotError::io(&indices, error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| SnapshotError::io(&indices, error))?;
            let dir = entry.path();
            if !entry
                .file_type()
                .map_err(|error| SnapshotError::io(&dir, error))?
                .is_dir()
            {
                continue;
            }
            if BaselineIndex::load(&baseline_path(&dir))?
                .entries
                .values()
                .chain(RefsIndex::load(&refs_path(&dir))?.entries.values())
                .any(|meta| meta.blob_hash.as_deref() == Some(hash))
            {
                return Ok(true);
            }
            let events = dir.join("events.log");
            if events.exists()
                && ChangeLog::read_path(&events)?.iter().any(|event| {
                    [event.before.as_ref(), event.after.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|meta| meta.blob_hash.as_deref() == Some(hash))
                })
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 全量扫描 refcount，返回当前 0 引用的 blob hash 列表，不主动删除。
    /// 调用方在归档/删除 session 时显式批量 release。
    pub fn dangling_blobs(&self) -> Vec<String> {
        let counts = self.refcount.lock().expect("refcount poisoned");
        counts
            .iter()
            .filter_map(|(h, c)| if *c == 0 { Some(h.clone()) } else { None })
            .collect()
    }
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn temp_path_for(target: &Path) -> PathBuf {
    target.with_file_name(format!(
        "{TEMP_PREFIX}-{}-{}-{}",
        std::process::id(),
        now_ns(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 原子写一段 JSON 文本。
pub(crate) fn write_atomic(path: &Path, payload: &[u8]) -> SnapshotResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SnapshotError::io(parent, e))?;
    }
    let tmp = temp_path_for(path);
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| SnapshotError::io(&tmp, e))?;
        f.write_all(payload)
            .map_err(|e| SnapshotError::io(&tmp, e))?;
        f.sync_all().map_err(|e| SnapshotError::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| SnapshotError::io(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn put_get_round_trip_text() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"hello world".repeat(100);
        let hash = store.put(&payload, true).unwrap();
        let back = store.get(&hash, true).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn reads_legacy_compressed_blob_with_the_same_content_hash() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"legacy snapshot\n".repeat(100);
        let hash = "b705b923516f3c5101d456c45c47197f";
        // Produced by zstd 0.13.3 at the existing level 3; do not regenerate with the current encoder.
        let legacy = [
            40, 181, 47, 253, 0, 88, 197, 0, 0, 128, 108, 101, 103, 97, 99, 121, 32, 115, 110, 97,
            112, 115, 104, 111, 116, 10, 1, 0, 90, 156, 127, 46, 1,
        ];
        let path = store.blob_path(hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, legacy).unwrap();
        assert_eq!(BlobStore::hash_bytes(&payload), hash);
        assert_eq!(store.get(hash, true).unwrap(), payload);
        assert_eq!(store.put(&payload, true).unwrap(), hash);
        assert_eq!(fs::read(path).unwrap(), legacy);
    }

    #[test]
    fn put_get_round_trip_binary() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload: Vec<u8> = (0..=255).cycle().take(4096).collect();
        let hash = store.put(&payload, false).unwrap();
        let back = store.get(&hash, false).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn duplicate_content_only_one_blob() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"deterministic";
        let h1 = store.put(payload, false).unwrap();
        let h2 = store.put(payload, false).unwrap();
        assert_eq!(h1, h2);
        let counts = store.refcount.lock().unwrap();
        assert_eq!(counts.get(&h1).copied(), Some(2));
    }

    #[test]
    fn concurrent_put_uses_distinct_temp_files() {
        let dir = tempdir().unwrap();
        let store = std::sync::Arc::new(BlobStore::new(dir.path()).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.put(b"concurrent snapshot payload", true).unwrap()
            }));
        }

        let hashes = handles
            .into_iter()
            .map(|handle| handle.join().expect("put thread"))
            .collect::<Vec<_>>();
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
        let counts = store.refcount.lock().unwrap();
        assert_eq!(counts.get(&hashes[0]).copied(), Some(8));
    }

    #[test]
    fn release_removes_blob_at_zero() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let h = store.put(b"x", false).unwrap();
        assert!(store.contains(&h));
        store.release(&h).unwrap();
        assert!(!store.contains(&h));
    }
}
