use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const MAX_FILE_SIZE: u64 = 512 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffChangeType {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnFileDiff {
    pub path: String,
    pub change_type: DiffChangeType,
    pub unified_diff: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnDiff {
    pub turn_id: String,
    pub task_id: Option<String>,
    pub worker_id: Option<String>,
    pub file_diffs: Vec<TurnFileDiff>,
    pub timestamp: u64,
}

impl TurnDiff {
    pub fn changed_count(&self) -> usize {
        self.file_diffs.len()
    }

    pub fn added_count(&self) -> usize {
        self.file_diffs
            .iter()
            .filter(|d| d.change_type == DiffChangeType::Added)
            .count()
    }

    pub fn modified_count(&self) -> usize {
        self.file_diffs
            .iter()
            .filter(|d| d.change_type == DiffChangeType::Modified)
            .count()
    }

    pub fn deleted_count(&self) -> usize {
        self.file_diffs
            .iter()
            .filter(|d| d.change_type == DiffChangeType::Deleted)
            .count()
    }

    pub fn changed_paths(&self) -> Vec<&str> {
        self.file_diffs.iter().map(|d| d.path.as_str()).collect()
    }
}

struct FileBaseline {
    hash: u64,
    content: String,
}

pub struct TurnDiffTracker {
    baselines: HashMap<String, FileBaseline>,
    tracked_paths: Vec<String>,
    diffs: Vec<TurnDiff>,
}

impl TurnDiffTracker {
    pub fn new() -> Self {
        Self {
            baselines: HashMap::new(),
            tracked_paths: Vec::new(),
            diffs: Vec::new(),
        }
    }

    pub fn snapshot_baseline(&mut self, paths: &[String]) {
        self.tracked_paths = paths.to_vec();
        self.baselines.clear();

        for path in paths {
            if let Some(baseline) = read_file_baseline(path) {
                self.baselines.insert(path.clone(), baseline);
            }
        }
    }

    pub fn capture_turn(
        &mut self,
        turn_id: &str,
        task_id: Option<&str>,
        worker_id: Option<&str>,
    ) -> TurnDiff {
        let mut file_diffs = Vec::new();

        for path in &self.tracked_paths {
            let current = read_file_baseline(path);
            let previous = self.baselines.get(path);

            match (previous, &current) {
                (None, Some(_)) => {
                    file_diffs.push(TurnFileDiff {
                        path: path.clone(),
                        change_type: DiffChangeType::Added,
                        unified_diff: None,
                    });
                }
                (Some(prev), Some(cur)) if prev.hash != cur.hash => {
                    let diff = generate_unified_diff(path, &prev.content, &cur.content);
                    file_diffs.push(TurnFileDiff {
                        path: path.clone(),
                        change_type: DiffChangeType::Modified,
                        unified_diff: Some(diff),
                    });
                }
                (Some(_), None) => {
                    file_diffs.push(TurnFileDiff {
                        path: path.clone(),
                        change_type: DiffChangeType::Deleted,
                        unified_diff: None,
                    });
                }
                _ => {}
            }
        }

        // 检查未在 tracked_paths 中但当前目录可能新增的文件 — 暂不支持自动发现

        let turn_diff = TurnDiff {
            turn_id: turn_id.to_string(),
            task_id: task_id.map(|s| s.to_string()),
            worker_id: worker_id.map(|s| s.to_string()),
            file_diffs,
            timestamp: now_millis(),
        };

        self.diffs.push(turn_diff.clone());

        // 更新基线为当前状态
        for path in &self.tracked_paths {
            if let Some(baseline) = read_file_baseline(path) {
                self.baselines.insert(path.clone(), baseline);
            } else {
                self.baselines.remove(path);
            }
        }

        turn_diff
    }

    pub fn all_diffs(&self) -> &[TurnDiff] {
        &self.diffs
    }

    pub fn diff_count(&self) -> usize {
        self.diffs.len()
    }

    pub fn all_changed_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .diffs
            .iter()
            .flat_map(|d| d.file_diffs.iter().map(|f| f.path.clone()))
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn clear(&mut self) {
        self.diffs.clear();
        self.baselines.clear();
        self.tracked_paths.clear();
    }
}

impl Default for TurnDiffTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn read_file_baseline(path: &str) -> Option<FileBaseline> {
    let p = Path::new(path);
    if !p.exists() {
        return None;
    }
    let metadata = fs::metadata(p).ok()?;
    if metadata.len() > MAX_FILE_SIZE {
        return None;
    }
    if is_binary(p) {
        return None;
    }
    let content = fs::read_to_string(p).ok()?;
    let hash = simple_hash(&content);
    Some(FileBaseline { hash, content })
}

fn is_binary(path: &Path) -> bool {
    const BINARY_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "woff", "woff2", "ttf", "eot", "otf",
        "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "o",
        "obj", "pyc", "class", "wasm",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map_or(false, |ext| {
            BINARY_EXTENSIONS.contains(&ext.to_lowercase().as_str())
        })
}

fn simple_hash(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
            (Some(o), Some(n)) if o == n => output.push(format!(" {o}")),
            (Some(o), Some(n)) => {
                output.push(format!("-{o}"));
                output.push(format!("+{n}"));
            }
            (Some(o), None) => output.push(format!("-{o}")),
            (None, Some(n)) => output.push(format!("+{n}")),
            (None, None) => {}
        }
        i += 1;
    }

    output.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn make_temp_dir() -> std::path::PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "magi-turn-diff-test-{}-{}-{}",
            std::process::id(),
            id,
            now_millis()
        ));
        fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    fn write_file(dir: &Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).expect("创建文件");
        f.write_all(content.as_bytes()).expect("写入文件");
        path.to_string_lossy().to_string()
    }

    #[test]
    fn tracker_constructs_empty() {
        let tracker = TurnDiffTracker::new();
        assert_eq!(tracker.diff_count(), 0);
        assert!(tracker.all_changed_paths().is_empty());
    }

    #[test]
    fn snapshot_and_capture_no_changes() {
        let dir = make_temp_dir();
        let p1 = write_file(&dir, "a.txt", "hello");
        let p2 = write_file(&dir, "b.txt", "world");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p1, p2]);

        let diff = tracker.capture_turn("turn-1", None, None);
        assert_eq!(diff.changed_count(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_modification() {
        let dir = make_temp_dir();
        let p = write_file(&dir, "a.txt", "hello");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p.clone()]);

        fs::write(&p, "hello world").expect("修改文件");

        let diff = tracker.capture_turn("turn-1", Some("task-1"), Some("worker-1"));
        assert_eq!(diff.modified_count(), 1);
        assert_eq!(diff.file_diffs[0].change_type, DiffChangeType::Modified);
        assert!(diff.file_diffs[0].unified_diff.is_some());
        assert_eq!(diff.task_id.as_deref(), Some("task-1"));
        assert_eq!(diff.worker_id.as_deref(), Some("worker-1"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_deletion() {
        let dir = make_temp_dir();
        let p = write_file(&dir, "a.txt", "content");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p.clone()]);

        fs::remove_file(&p).expect("删除文件");

        let diff = tracker.capture_turn("turn-1", None, None);
        assert_eq!(diff.deleted_count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_addition() {
        let dir = make_temp_dir();
        let target = dir.join("new.txt");
        let p = target.to_string_lossy().to_string();

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p.clone()]);

        fs::write(&target, "new content").expect("新建文件");

        let diff = tracker.capture_turn("turn-1", None, None);
        assert_eq!(diff.added_count(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiple_turns_accumulate() {
        let dir = make_temp_dir();
        let p = write_file(&dir, "a.txt", "v1");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p.clone()]);

        fs::write(&p, "v2").expect("写 v2");
        tracker.capture_turn("turn-1", None, None);

        fs::write(&p, "v3").expect("写 v3");
        tracker.capture_turn("turn-2", None, None);

        assert_eq!(tracker.diff_count(), 2);
        assert_eq!(tracker.all_changed_paths(), vec![p]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_binary_files() {
        let dir = make_temp_dir();
        let p = write_file(&dir, "image.png", "fake png");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p.clone()]);

        let diff = tracker.capture_turn("turn-1", None, None);
        assert_eq!(diff.changed_count(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_resets_state() {
        let dir = make_temp_dir();
        let p = write_file(&dir, "a.txt", "v1");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_baseline(&[p.clone()]);

        fs::write(&p, "v2").expect("修改");
        tracker.capture_turn("turn-1", None, None);

        tracker.clear();
        assert_eq!(tracker.diff_count(), 0);
        assert!(tracker.all_changed_paths().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn change_type_serializes() {
        let ct = DiffChangeType::Modified;
        let json = serde_json::to_string(&ct).unwrap();
        assert_eq!(json, "\"modified\"");
    }

    #[test]
    fn turn_diff_serializes() {
        let diff = TurnDiff {
            turn_id: "t-1".to_string(),
            task_id: Some("task-1".to_string()),
            worker_id: None,
            file_diffs: vec![TurnFileDiff {
                path: "src/main.rs".to_string(),
                change_type: DiffChangeType::Added,
                unified_diff: None,
            }],
            timestamp: 1234567890,
        };
        let json = serde_json::to_string(&diff).unwrap();
        assert!(json.contains("\"added\""));
        assert!(json.contains("src/main.rs"));
    }
}
