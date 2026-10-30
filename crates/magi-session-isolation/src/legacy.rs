//! 将旧版复制降级产生的依赖链接转为独立内容。原副本、成果及源目录均保留。
use crate::{IsolationError, IsolationResult, SessionIsolation, clone::join_relative};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyJournal {
    relative: String,
    ready: bool,
}

fn unsafe_path(message: &str) -> IsolationError {
    IsolationError::UnsafePath(message.into())
}

fn real_directory(path: &Path) -> IsolationResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| IsolationError::io("读取迁移目录", path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(unsafe_path("旧隔离副本迁移不能跟随目录链接"));
    }
    Ok(())
}

fn journal_directory(parent: &Path, name: &str) -> IsolationResult<std::path::PathBuf> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(IsolationError::io("创建迁移目录", &path, error)),
    }
    real_directory(&path)?;
    Ok(path)
}

/// 只在恢复阶段、开始执行前调用。全部目录处理成功后调用方才可清除 linked_dirs 并持久化。
/// ready 清单先落盘，再移除链接和改名；进程中断后继续同一份已完成内容，不新建空副本。
pub fn materialize_legacy_shared_directories(record: &SessionIsolation) -> IsolationResult<()> {
    if record.linked_dirs.is_empty() {
        return Ok(());
    }
    real_directory(&record.root)?;
    let root = record
        .root
        .canonicalize()
        .map_err(|error| IsolationError::io("解析隔离副本", &record.root, error))?;
    let source = record
        .source_root
        .canonicalize()
        .map_err(|error| IsolationError::io("解析旧依赖来源", &record.source_root, error))?;
    if root.starts_with(&source) || source.starts_with(&root) {
        return Err(unsafe_path("旧隔离副本与来源不能重叠"));
    }
    let state = journal_directory(&root, ".magi")?;
    let journals = journal_directory(&state, "legacy-shared-directories")?;
    for relative in &record.linked_dirs {
        let destination = join_relative(&root, relative)?;
        let relative_path = Path::new(relative);
        let name = relative_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !matches!(name, "node_modules" | ".venv" | "venv")
            && !(name == "target" && relative_path.components().count() == 1)
        {
            return Err(unsafe_path("迁移记录包含非旧版共享目录"));
        }
        let parent = destination
            .parent()
            .expect("relative destination has parent");
        if parent.canonicalize().ok().as_deref() != Some(parent) {
            return Err(unsafe_path("旧共享目录的父目录已变化"));
        }
        let key = Sha256::digest(relative.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let journal_path = journals.join(format!("{key}.json"));
        let staged = journals.join(format!("{key}.copy"));
        if fs::symlink_metadata(&journal_path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(unsafe_path("旧共享目录迁移清单必须是普通文件"));
        }
        let mut journal: CopyJournal = match fs::read(&journal_path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| unsafe_path("旧共享目录迁移清单损坏"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => CopyJournal {
                relative: relative.clone(),
                ready: false,
            },
            Err(error) => return Err(IsolationError::io("读取迁移清单", &journal_path, error)),
        };
        if journal.relative != *relative {
            return Err(unsafe_path("旧共享目录迁移清单身份不匹配"));
        }
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                // 包括改名后、登记持久化前中断；不能覆盖已独立的目录及其新成果。
                continue;
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let expected = source.join(relative);
                if destination.canonicalize().ok().as_deref() != Some(expected.as_path()) {
                    return Err(unsafe_path("旧共享链接已改变或来源不可用"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && journal.ready => {}
            Err(error) => return Err(IsolationError::io("读取旧共享链接", &destination, error)),
            Ok(_) => return Err(unsafe_path("旧共享目录被替换为普通文件")),
        }
        let save = |journal: &CopyJournal| -> IsolationResult<()> {
            let bytes = serde_json::to_vec(journal).map_err(|_| unsafe_path("无法编码迁移清单"))?;
            magi_core::fs_atomic::write_atomic(&journal_path, &bytes)
                .map_err(|error| IsolationError::io("写入迁移清单", &journal_path, error))
        };
        if !journal.ready {
            if staged.exists() {
                // 没有清单就不拥有已有目录，不能清理未知成果。
                if !journal_path.is_file() {
                    return Err(IsolationError::DestinationExists(staged));
                }
                real_directory(&staged)?;
                fs::remove_dir_all(&staged)
                    .map_err(|error| IsolationError::io("清理未完成迁移", &staged, error))?;
            }
            save(&journal)?;
            let expected = source.join(relative);
            real_directory(&expected)?;
            if expected.canonicalize().ok().as_deref() != Some(expected.as_path()) {
                return Err(unsafe_path("旧共享目录来源已改变"));
            }
            crate::clone::copy_entry(&expected, &staged)?;
            journal.ready = true;
            save(&journal)?;
        }
        real_directory(&staged)?;
        if let Ok(metadata) = fs::symlink_metadata(&destination) {
            if !metadata.file_type().is_symlink()
                || destination.canonicalize().ok().as_deref()
                    != Some(source.join(relative).as_path())
            {
                return Err(unsafe_path("复制过程中旧共享链接被替换，保留现有内容"));
            }
            // 仅删除链接本身；Windows 的目录符号链接使用 remove_dir。
            #[cfg(windows)]
            let unlink = fs::remove_dir(&destination);
            #[cfg(not(windows))]
            let unlink = fs::remove_file(&destination);
            unlink.map_err(|error| IsolationError::io("移除旧共享链接", &destination, error))?;
        }
        fs::rename(&staged, &destination)
            .map_err(|error| IsolationError::io("替换独立依赖目录", &destination, error))?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{CloneStrategy, IsolationOrigin};
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, SessionIsolation) {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let source = parent.join("source");
        let root = parent.join("copy");
        fs::create_dir_all(source.join("packages/ui/node_modules/pkg")).unwrap();
        fs::write(
            source.join("packages/ui/node_modules/pkg/code.js"),
            "original dependency",
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/ui")).unwrap();
        fs::write(root.join("result.txt"), "unmerged result").unwrap();
        symlink(
            source.join("packages/ui/node_modules"),
            root.join("packages/ui/node_modules"),
        )
        .unwrap();
        (
            temp,
            SessionIsolation {
                session_id: "session".into(),
                workspace_id: "project".into(),
                source_root: source,
                root,
                origin: IsolationOrigin::Manual,
                strategy: CloneStrategy::Copy,
                linked_dirs: vec!["packages/ui/node_modules".into()],
                git_available: false,
                created_at_ms: 1,
            },
        )
    }

    #[test]
    fn legacy_dependencies_become_independent_without_replacing_unmerged_results() {
        let (_temp, record) = fixture();
        materialize_legacy_shared_directories(&record).unwrap();
        let path = record.root.join("packages/ui/node_modules/pkg/code.js");
        fs::write(&path, "copy change").unwrap();
        assert_eq!(
            fs::read_to_string(
                record
                    .source_root
                    .join("packages/ui/node_modules/pkg/code.js")
            )
            .unwrap(),
            "original dependency"
        );
        assert_eq!(
            fs::read_to_string(record.root.join("result.txt")).unwrap(),
            "unmerged result"
        );
        // 模拟目录已换好、登记清空 linked_dirs 之前进程中断；不能重新覆盖副本修改。
        materialize_legacy_shared_directories(&record).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "copy change");
    }

    #[test]
    fn legacy_ready_copy_recovers_after_link_removal_without_recopied_source_changes() {
        let (_temp, record) = fixture();
        let relative = &record.linked_dirs[0];
        let journals = record.root.join(".magi/legacy-shared-directories");
        fs::create_dir_all(&journals).unwrap();
        let key = Sha256::digest(relative.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let staged = journals.join(format!("{key}.copy"));
        crate::clone::copy_entry(&record.source_root.join(relative), &staged).unwrap();
        fs::write(
            journals.join(format!("{key}.json")),
            serde_json::to_vec(&CopyJournal {
                relative: relative.clone(),
                ready: true,
            })
            .unwrap(),
        )
        .unwrap();
        fs::remove_file(record.root.join(relative)).unwrap();
        fs::write(
            record.source_root.join(relative).join("pkg/code.js"),
            "new source content",
        )
        .unwrap();
        materialize_legacy_shared_directories(&record).unwrap();
        assert_eq!(
            fs::read_to_string(record.root.join(relative).join("pkg/code.js")).unwrap(),
            "original dependency"
        );
        assert!(!staged.exists());
    }

    #[test]
    fn legacy_external_links_and_unowned_staging_are_preserved_and_rejected() {
        let (_temp, record) = fixture();
        let relative = &record.linked_dirs[0];
        let journals = record.root.join(".magi/legacy-shared-directories");
        fs::create_dir_all(&journals).unwrap();
        let key = Sha256::digest(relative.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let staged = journals.join(format!("{key}.copy"));
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("user.txt"), "keep").unwrap();
        assert!(materialize_legacy_shared_directories(&record).is_err());
        assert_eq!(fs::read_to_string(staged.join("user.txt")).unwrap(), "keep");
        let external = record.source_root.parent().unwrap().join("external");
        fs::create_dir(&external).unwrap();
        fs::remove_file(record.root.join(relative)).unwrap();
        symlink(&external, record.root.join(relative)).unwrap();
        assert!(materialize_legacy_shared_directories(&record).is_err());
        assert_eq!(fs::read_link(record.root.join(relative)).unwrap(), external);
    }
}
