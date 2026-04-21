use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSnapshot {
    pub path: String,
    pub content: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMetadata {
    pub snapshot_id: String,
    pub session_id: String,
    pub created_at: u64,
    pub file_count: usize,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub metadata: SnapshotMetadata,
    pub files: Vec<FileSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub diff_type: DiffType,
    pub unified_diff: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffType {
    Added,
    Modified,
    Deleted,
    Unchanged,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn simple_hash(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

const MAX_FILE_SIZE: usize = 512 * 1024;
const MAX_CACHE_SIZE: usize = 100;

pub struct SnapshotManager {
    storage_path: PathBuf,
    snapshots: HashMap<String, Snapshot>,
    content_cache: HashMap<String, String>,
    cache_order: Vec<String>,
}

impl SnapshotManager {
    pub fn new(storage_path: impl Into<PathBuf>) -> Self {
        Self {
            storage_path: storage_path.into(),
            snapshots: HashMap::new(),
            content_cache: HashMap::new(),
            cache_order: Vec::new(),
        }
    }

    pub fn create_snapshot(
        &mut self,
        session_id: &str,
        files: &[String],
        description: Option<&str>,
    ) -> Result<String, String> {
        let snapshot_id = format!("snap-{}", now_millis());
        let mut file_snapshots = Vec::new();

        for file_path in files {
            let path = Path::new(file_path);
            if !path.exists() {
                continue;
            }
            let metadata = fs::metadata(path).map_err(|e| format!("stat 失败 {file_path}: {e}"))?;
            if metadata.len() as usize > MAX_FILE_SIZE {
                continue;
            }
            if is_binary_file(path) {
                continue;
            }

            let content = fs::read_to_string(path)
                .map_err(|e| format!("读取文件失败 {file_path}: {e}"))?;
            let hash = simple_hash(&content);

            self.cache_content(file_path, &content);

            file_snapshots.push(FileSnapshot {
                path: file_path.clone(),
                content,
                hash,
            });
        }

        let snapshot = Snapshot {
            metadata: SnapshotMetadata {
                snapshot_id: snapshot_id.clone(),
                session_id: session_id.to_string(),
                created_at: now_millis(),
                file_count: file_snapshots.len(),
                description: description.map(|s| s.to_string()),
            },
            files: file_snapshots,
        };

        self.persist_snapshot(&snapshot)?;
        self.snapshots.insert(snapshot_id.clone(), snapshot);

        Ok(snapshot_id)
    }

    pub fn restore_snapshot(&self, snapshot_id: &str) -> Result<usize, String> {
        let snapshot = self
            .snapshots
            .get(snapshot_id)
            .ok_or_else(|| format!("快照 {snapshot_id} 不存在"))?;

        let mut restored = 0;
        for file in &snapshot.files {
            let path = Path::new(&file.path);
            if let Some(dir) = path.parent() {
                if !dir.exists() {
                    fs::create_dir_all(dir)
                        .map_err(|e| format!("创建目录失败: {e}"))?;
                }
            }
            fs::write(path, &file.content)
                .map_err(|e| format!("还原文件 {} 失败: {e}", file.path))?;
            restored += 1;
        }

        Ok(restored)
    }

    pub fn diff_with_current(&self, snapshot_id: &str) -> Result<Vec<FileDiff>, String> {
        let snapshot = self
            .snapshots
            .get(snapshot_id)
            .ok_or_else(|| format!("快照 {snapshot_id} 不存在"))?;

        let mut diffs = Vec::new();

        for file in &snapshot.files {
            let path = Path::new(&file.path);
            if !path.exists() {
                diffs.push(FileDiff {
                    path: file.path.clone(),
                    diff_type: DiffType::Deleted,
                    unified_diff: None,
                });
                continue;
            }

            let current = fs::read_to_string(path).unwrap_or_default();
            let current_hash = simple_hash(&current);

            if current_hash == file.hash {
                diffs.push(FileDiff {
                    path: file.path.clone(),
                    diff_type: DiffType::Unchanged,
                    unified_diff: None,
                });
            } else {
                let diff = generate_unified_diff(&file.path, &file.content, &current);
                diffs.push(FileDiff {
                    path: file.path.clone(),
                    diff_type: DiffType::Modified,
                    unified_diff: Some(diff),
                });
            }
        }

        Ok(diffs)
    }

    pub fn get_snapshot(&self, snapshot_id: &str) -> Option<&Snapshot> {
        self.snapshots.get(snapshot_id)
    }

    pub fn list_snapshots(&self) -> Vec<&SnapshotMetadata> {
        let mut metas: Vec<&SnapshotMetadata> = self
            .snapshots
            .values()
            .map(|s| &s.metadata)
            .collect();
        metas.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        metas
    }

    pub fn remove_snapshot(&mut self, snapshot_id: &str) -> bool {
        if self.snapshots.remove(snapshot_id).is_some() {
            let file_path = self.storage_path.join(format!("{snapshot_id}.json"));
            let _ = fs::remove_file(file_path);
            return true;
        }
        false
    }

    fn persist_snapshot(&self, snapshot: &Snapshot) -> Result<(), String> {
        if !self.storage_path.exists() {
            fs::create_dir_all(&self.storage_path)
                .map_err(|e| format!("创建快照目录失败: {e}"))?;
        }
        let file_path = self
            .storage_path
            .join(format!("{}.json", snapshot.metadata.snapshot_id));
        let payload = serde_json::to_string_pretty(snapshot)
            .map_err(|e| format!("序列化快照失败: {e}"))?;
        fs::write(&file_path, payload).map_err(|e| format!("写入快照失败: {e}"))?;
        Ok(())
    }

    fn cache_content(&mut self, path: &str, content: &str) {
        if self.content_cache.len() >= MAX_CACHE_SIZE {
            if let Some(oldest) = self.cache_order.first().cloned() {
                self.content_cache.remove(&oldest);
                self.cache_order.remove(0);
            }
        }
        self.content_cache
            .insert(path.to_string(), content.to_string());
        self.cache_order.push(path.to_string());
    }
}

fn is_binary_file(path: &Path) -> bool {
    const BINARY_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "webp", "woff", "woff2", "ttf", "eot",
        "otf", "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib",
        "o", "obj", "pyc", "class", "wasm",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map_or(false, |ext| BINARY_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn generate_unified_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut output = Vec::new();
    output.push(format!("--- a/{path}"));
    output.push(format!("+++ b/{path}"));

    let max_len = old_lines.len().max(new_lines.len());
    let mut i = 0;
    while i < max_len {
        let old_line = old_lines.get(i).copied();
        let new_line = new_lines.get(i).copied();

        match (old_line, new_line) {
            (Some(o), Some(n)) if o == n => {
                output.push(format!(" {o}"));
            }
            (Some(o), Some(n)) => {
                output.push(format!("-{o}"));
                output.push(format!("+{n}"));
            }
            (Some(o), None) => {
                output.push(format!("-{o}"));
            }
            (None, Some(n)) => {
                output.push(format!("+{n}"));
            }
            (None, None) => {}
        }
        i += 1;
    }

    output.join("\n")
}
