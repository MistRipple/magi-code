use std::collections::HashMap;
use std::fs;
use std::io::{Read as IoRead, Write as IoWrite};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dependency_graph::DependencyGraphSnapshot;
use crate::inverted_index::InvertedIndexSnapshot;
use crate::query_expander::ExpansionCacheSnapshot;
use crate::symbol_index::SymbolIndexSnapshot;

const PERSISTENCE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileManifestEntry {
    pub mtime: u64,
    pub size: u64,
    pub file_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistenceSnapshot {
    pub version: u32,
    pub project_root: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub file_manifest: Vec<(String, FileManifestEntry)>,
    pub inverted_index: InvertedIndexSnapshot,
    pub symbol_index: SymbolIndexSnapshot,
    pub dependency_graph: DependencyGraphSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_cache: Option<ExpansionCacheSnapshot>,
}

#[derive(Clone, Debug, Default)]
pub struct FreshnessResult {
    pub unchanged: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub added: Vec<String>,
}

impl FreshnessResult {
    pub fn change_count(&self) -> usize {
        self.modified.len() + self.deleted.len() + self.added.len()
    }
}

pub struct IndexPersistence {
    cache_file_path: PathBuf,
    legacy_cache_file_path: PathBuf,
}

impl IndexPersistence {
    pub fn new(project_root: &str) -> Self {
        let base = Path::new(project_root).join(".magi").join("cache");
        Self {
            cache_file_path: base.join("search-index.json.gz"),
            legacy_cache_file_path: base.join("search-index.json"),
        }
    }

    pub fn save(&self, snapshot: &PersistenceSnapshot) -> Result<(), String> {
        let dir = self
            .cache_file_path
            .parent()
            .ok_or("invalid cache path")?;
        fs::create_dir_all(dir).map_err(|e| format!("mkdir failed: {}", e))?;

        let json_str =
            serde_json::to_string(snapshot).map_err(|e| format!("serialize failed: {}", e))?;

        let compressed = compress_gzip(json_str.as_bytes())?;

        fs::write(&self.cache_file_path, &compressed)
            .map_err(|e| format!("write failed: {}", e))?;

        if self.legacy_cache_file_path.exists() {
            let _ = fs::remove_file(&self.legacy_cache_file_path);
        }

        Ok(())
    }

    pub fn load(&self) -> Option<PersistenceSnapshot> {
        let raw = if self.cache_file_path.exists() {
            let compressed = fs::read(&self.cache_file_path).ok()?;
            decompress_gzip(&compressed).ok()?
        } else if self.legacy_cache_file_path.exists() {
            fs::read(&self.legacy_cache_file_path).ok()?
        } else {
            return None;
        };

        let snapshot: PersistenceSnapshot = serde_json::from_slice(&raw).ok()?;

        if snapshot.version != PERSISTENCE_VERSION {
            return None;
        }

        Some(snapshot)
    }

    pub fn validate_freshness(
        &self,
        project_root: &str,
        snapshot: &PersistenceSnapshot,
        current_files: &[(String, String)],
    ) -> FreshnessResult {
        let mut result = FreshnessResult::default();

        let manifest: HashMap<&str, &FileManifestEntry> = snapshot
            .file_manifest
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();

        let current_set: std::collections::HashSet<&str> =
            current_files.iter().map(|(p, _)| p.as_str()).collect();

        for (file_path, entry) in &manifest {
            if !current_set.contains(file_path) {
                result.deleted.push(file_path.to_string());
                continue;
            }

            let full_path = Path::new(project_root).join(file_path);
            match fs::metadata(&full_path) {
                Ok(meta) => {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);

                    if (mtime as i64 - entry.mtime as i64).unsigned_abs() > 1 {
                        result.modified.push(file_path.to_string());
                    } else {
                        result.unchanged.push(file_path.to_string());
                    }
                }
                Err(_) => {
                    result.deleted.push(file_path.to_string());
                }
            }
        }

        for (file_path, _) in current_files {
            if !manifest.contains_key(file_path.as_str()) {
                result.added.push(file_path.clone());
            }
        }

        result
    }

    pub fn should_full_rebuild(freshness: &FreshnessResult, total_files: usize) -> bool {
        if total_files == 0 {
            return true;
        }
        let change_count = freshness.change_count();
        change_count as f64 / total_files as f64 > 0.3
    }

    pub fn build_file_manifest(
        project_root: &str,
        files: &[(String, String)],
    ) -> Vec<(String, FileManifestEntry)> {
        let mut manifest = Vec::with_capacity(files.len());
        for (path, file_type) in files {
            let full_path = Path::new(project_root).join(path);
            if let Ok(meta) = fs::metadata(&full_path) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                manifest.push((
                    path.clone(),
                    FileManifestEntry {
                        mtime,
                        size: meta.len(),
                        file_type: file_type.clone(),
                    },
                ));
            }
        }
        manifest
    }

    pub fn invalidate(&self) {
        for path in [&self.cache_file_path, &self.legacy_cache_file_path] {
            if path.exists() {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn compress_gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| format!("gzip compress: {}", e))?;
    encoder
        .finish()
        .map_err(|e| format!("gzip finish: {}", e))
}

fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;

    let mut decoder = GzDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|e| format!("gzip decompress: {}", e))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gzip_roundtrip() {
        let original = b"hello world, this is test data for gzip compression";
        let compressed = compress_gzip(original).unwrap();
        assert!(compressed.len() > 0);
        let decompressed = decompress_gzip(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_freshness_result() {
        let mut f = FreshnessResult::default();
        f.modified.push("a.rs".into());
        f.deleted.push("b.rs".into());
        f.added.push("c.rs".into());
        assert_eq!(f.change_count(), 3);
    }

    #[test]
    fn test_should_full_rebuild() {
        let mut f = FreshnessResult::default();
        for i in 0..4 {
            f.modified.push(format!("{}.rs", i));
        }
        assert!(IndexPersistence::should_full_rebuild(&f, 10));
        f.modified.clear();
        f.modified.push("x.rs".into());
        assert!(!IndexPersistence::should_full_rebuild(&f, 10));
    }

    #[test]
    fn test_load_nonexistent() {
        let persistence = IndexPersistence::new("/tmp/nonexistent_project_12345");
        assert!(persistence.load().is_none());
    }
}
