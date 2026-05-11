use crate::error::{SnapshotError, SnapshotResult};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const ZSTD_LEVEL: i32 = 3;
const TEMP_PREFIX: &str = ".magi-snapshot-tmp";

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

        let tmp = target.with_file_name(format!(
            "{TEMP_PREFIX}-{}-{}",
            std::process::id(),
            now_ns()
        ));

        {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| SnapshotError::io(&tmp, e))?;
            f.write_all(&payload).map_err(|e| SnapshotError::io(&tmp, e))?;
            f.sync_all().map_err(|e| SnapshotError::io(&tmp, e))?;
        }

        fs::rename(&tmp, &target).map_err(|e| SnapshotError::io(&target, e))?;
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
        f.read_to_end(&mut buf).map_err(|e| SnapshotError::io(&path, e))?;
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
        let drop_now = {
            let mut counts = self.refcount.lock().expect("refcount poisoned");
            match counts.get_mut(hash) {
                Some(c) if *c > 1 => {
                    *c -= 1;
                    false
                }
                Some(_) => {
                    counts.remove(hash);
                    true
                }
                None => true,
            }
        };
        if drop_now {
            let path = self.blob_path(hash);
            if path.exists() {
                fs::remove_file(&path).map_err(|e| SnapshotError::io(&path, e))?;
            }
        }
        Ok(())
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

/// 原子写一段 JSON 文本。
pub(crate) fn write_atomic(path: &Path, payload: &[u8]) -> SnapshotResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SnapshotError::io(parent, e))?;
    }
    let tmp = path.with_file_name(format!(
        "{TEMP_PREFIX}-{}-{}",
        std::process::id(),
        now_ns()
    ));
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| SnapshotError::io(&tmp, e))?;
        f.write_all(payload).map_err(|e| SnapshotError::io(&tmp, e))?;
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
    fn release_removes_blob_at_zero() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let h = store.put(b"x", false).unwrap();
        assert!(store.contains(&h));
        store.release(&h).unwrap();
        assert!(!store.contains(&h));
    }
}
