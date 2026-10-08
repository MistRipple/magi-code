//! 文件工具共用的文件系统原语：原子写入、路径包含关系。

use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
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

/// 先写同目录临时文件再 rename：写入中途失败或进程崩溃都不会留下半截的目标文件。
/// 目标是符号链接时写入链接指向的真实文件，已有文件的权限位保持不变。
pub(crate) fn write_file_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    let permissions = fs::metadata(&target)
        .ok()
        .map(|metadata| metadata.permissions());
    let temp = sibling_temp_path(&target, "write");

    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
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
    fn atomic_write_creates_and_replaces_without_leaving_temp_files() {
        let dir = temp_dir("replace");
        let path = dir.join("a.txt");

        write_file_atomically(&path, b"one").expect("create");
        write_file_atomically(&path, b"two").expect("replace");

        assert_eq!(fs::read_to_string(&path).expect("read"), "two");
        let leftovers = fs::read_dir(&dir).expect("list").count();
        assert_eq!(leftovers, 1, "only the target file remains");
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_permissions_and_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = temp_dir("unix");
        let real = dir.join("real.sh");
        fs::write(&real, "old").expect("seed");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o750)).expect("chmod");
        let link = dir.join("link.sh");
        symlink(&real, &link).expect("symlink");

        write_file_atomically(&link, b"new").expect("write through link");

        assert!(fs::symlink_metadata(&link).expect("link meta").is_symlink());
        assert_eq!(fs::read_to_string(&real).expect("read"), "new");
        assert_eq!(
            fs::metadata(&real).expect("meta").permissions().mode() & 0o777,
            0o750
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_failure_leaves_original_and_no_temp_file() {
        let dir = temp_dir("failure");
        let path = dir.join("keep.txt");
        fs::write(&path, "original").expect("seed");
        // 目标位置是目录：rename 会失败。
        let directory_target = dir.join("subdir");
        fs::create_dir_all(&directory_target).expect("subdir");

        let error = write_file_atomically(&directory_target, b"x").expect_err("must fail");

        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::IsADirectory
                    | io::ErrorKind::Other
                    | io::ErrorKind::PermissionDenied
            ) || error.raw_os_error().is_some(),
            "{error:?}"
        );
        assert_eq!(fs::read_to_string(&path).expect("read"), "original");
        let names = fs::read_dir(&dir)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(
            names.iter().all(|name| !name.contains(".magi-")),
            "{names:?}"
        );
        fs::remove_dir_all(&dir).ok();
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
