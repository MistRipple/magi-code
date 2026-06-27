//! 崩溃安全的原子文件写入。
//!
//! 各 Tier-4 mission store（Charter / Plan / KG / Validation / Checkpoint /
//! HumanCheckpoint / ProjectMemory）把整份状态序列化后整体覆盖写盘。这些文件正是
//! 进程重启后崩溃恢复要读取的对象，**绝不能**出现“写到一半被中断”的截断态。
//!
//! 朴素的 `fs::write(path, bytes)` 会先把目标文件截断为 0 再逐步写入，进程在写入
//! 过程中被杀就会留下截断/半截 JSON，后续 `load` 解析直接失败、整段历史丢失。
//!
//! 本函数遵循仓库既有约定（settings_store / api state 同款）：先写到同目录下的临时
//! 文件，再用 `rename` 原子替换目标。POSIX 下同目录 `rename` 是原子操作，读者要么看到
//! 旧内容、要么看到新内容，不会看到中间态。
//!
//! 在原子性之外还做了持久化：`rename` 前 `fsync` 临时文件，`rename` 后 `fsync` 父目录。
//! 否则内容或目录项可能只停留在 OS page cache，OS 崩溃/掉电时即便 `rename` 语义成立，
//! 落盘的也可能是旧内容甚至空洞。这里不做成可配置开关——这些 store 只在 mission 状态
//! 跃迁时写一次，且每次写都排在昂贵的模型调用之后，`fsync` 的相对成本可忽略，没有理由
//! 留一个除“关掉持久化”外无人使用的配置面。

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 为 `path` 生成同目录下的临时写入路径，复用目标文件名以保证落在同一文件系统，
/// 从而让后续 `rename` 保持原子。
fn temp_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// 原子地把 `contents` 写入 `path`：先写同目录临时文件，再 `rename` 覆盖目标。
///
/// 不负责创建父目录；调用方应在写前确保父目录存在（与既有 store 行为一致）。
/// 写入失败时尽量清理残留的临时文件，避免目录里堆积 `.tmp`。
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let temp_path = temp_path_for(path);
    // 写临时文件并 fsync，确保内容真正落盘后再 rename。
    let flush_result = (|| -> io::Result<()> {
        let mut file = File::create(&temp_path)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()
    })();
    if let Err(error) = flush_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    // 尽力 fsync 父目录，让 rename 产生的新目录项本身也持久化；
    // 不支持对目录 fsync 的平台上失败可忽略，不影响已完成的原子替换。
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_file_with_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        write_atomic(&path, b"hello").expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "hello");
    }

    #[test]
    fn write_atomic_overwrites_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        fs::write(&path, b"old").expect("seed");
        write_atomic(&path, b"new").expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "new");
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        write_atomic(&path, b"payload").expect("write");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp residue expected");
    }
}
