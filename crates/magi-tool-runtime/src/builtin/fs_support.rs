//! 文件工具共用的文件系统原语：原子写入、路径包含关系。

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 在 `path` 同目录里生成不会与现有文件冲突的临时路径。
pub(crate) fn sibling_temp_path(path: &Path, purpose: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("file"));
    let unique = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".magi-{purpose}-{}-{nanos}-{unique}",
        process::id()
    ));
    path.with_file_name(temp_name)
}

/// 规范化路径：已存在的最长前缀走 canonicalize（解开符号链接），其余部分原样接回。
/// 用于判断「目标是否落在源目录内部」这类对尚不存在的路径也要成立的关系。
fn canonicalize_lenient(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in tail.iter().rev() {
                    resolved.push(component);
                }
                return resolved;
            }
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    tail.push(name.to_os_string());
                    existing = parent;
                }
                _ => return path.to_path_buf(),
            },
        }
    }
}

/// `candidate` 是否与 `ancestor` 相同或位于其内部（按真实路径比较）。
pub(crate) fn is_same_or_inside(candidate: &Path, ancestor: &Path) -> bool {
    canonicalize_lenient(candidate).starts_with(canonicalize_lenient(ancestor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "magi-fs-support-{label}-{}-{}",
            process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn inside_check_handles_missing_destinations_and_siblings() {
        let dir = temp_dir("inside");
        let source = dir.join("src");
        fs::create_dir_all(source.join("nested")).expect("tree");

        assert!(is_same_or_inside(&source.join("new/deep"), &source));
        assert!(is_same_or_inside(&source, &source));
        assert!(!is_same_or_inside(&dir.join("src-other"), &source));
        assert!(!is_same_or_inside(&dir, &source));
        fs::remove_dir_all(&dir).ok();
    }
}
