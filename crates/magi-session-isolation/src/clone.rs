//! 建立隔离副本。
//!
//! 优先使用文件系统的写时复制（macOS APFS 的 `clonefile`、Linux 的 reflink）：整个目录
//! 几乎不占空间、秒级完成，`node_modules` 之类的大目录也包含在内且互相独立。文件系统不支持
//! （跨卷、ext4、Windows 等）时退回普通复制，并把依赖/构建产物这类体积巨大的目录改为指回
//! 主工作区的符号链接，避免复制几个 GB 的内容。
use crate::error::{IsolationError, IsolationResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Magi 自己的运行态目录（快照账本、会话索引等），属于主工作区本身，不进入副本。
const STATE_DIR_NAME: &str = ".magi";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloneStrategy {
    /// 写时复制，副本完整且互相独立。
    Clone,
    /// 普通复制；体积巨大的依赖目录以符号链接指回主工作区。
    Copy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneOutcome {
    pub strategy: CloneStrategy,
    /// 普通复制时被替换为符号链接的目录（相对路径）。
    pub linked_dirs: Vec<String>,
    /// 副本里是否有可用的独立 Git 仓库。主工作区是 linked worktree / submodule 时
    /// `.git` 只是指向外部 gitdir 的文件，原样复制会让副本与主工作区共用同一份索引，
    /// 因此不复制，副本里也就没有 Git。
    pub git_available: bool,
}

pub fn create_isolated_copy(source: &Path, dest: &Path) -> IsolationResult<CloneOutcome> {
    create_isolated_copy_with(source, dest, true)
}

pub(crate) fn create_isolated_copy_with(
    source: &Path,
    dest: &Path,
    allow_clone: bool,
) -> IsolationResult<CloneOutcome> {
    if !source.is_dir() {
        return Err(IsolationError::InvalidSource(source.to_path_buf()));
    }
    if dest.exists() {
        return Err(IsolationError::DestinationExists(dest.to_path_buf()));
    }
    fs::create_dir_all(dest)
        .map_err(|error| IsolationError::io("创建隔离副本目录", dest, error))?;

    let dot_git = source.join(".git");
    let skip_git = dot_git.is_file();
    let git_available = dot_git.is_dir();
    let entries = fs::read_dir(source)
        .map_err(|error| IsolationError::io("读取源目录", source, error))?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            name != STATE_DIR_NAME && !(skip_git && name == ".git")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    if allow_clone && entries.iter().all(|entry| clone_entry(entry, dest)) {
        return Ok(CloneOutcome {
            strategy: CloneStrategy::Clone,
            linked_dirs: Vec::new(),
            git_available,
        });
    }
    // 写时复制不可用或中途失败：清掉半成品，改用普通复制。
    if allow_clone {
        let _ = fs::remove_dir_all(dest);
        fs::create_dir_all(dest)
            .map_err(|error| IsolationError::io("创建隔离副本目录", dest, error))?;
    }
    let mut linked_dirs = Vec::new();
    for entry in &entries {
        let name = entry.file_name().expect("directory entry has a name");
        copy_entry(
            entry,
            &dest.join(name),
            true,
            Path::new(name),
            &mut linked_dirs,
        )?;
    }
    linked_dirs.sort();
    Ok(CloneOutcome {
        strategy: CloneStrategy::Copy,
        linked_dirs,
        git_available,
    })
}

/// 用文件系统的写时复制复制一个顶层条目到 `dest_dir` 下。不支持时返回 `false`。
fn clone_entry(entry: &Path, dest_dir: &Path) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = magi_process::std_command("cp");
        command.args(["-c", "-R", "-P", "-p"]);
        command
    } else if cfg!(target_os = "linux") {
        let mut command = magi_process::std_command("cp");
        command.args(["-a", "--reflink=always"]);
        command
    } else {
        return false;
    };
    command
        .arg(entry)
        .arg(dest_dir)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 普通复制时指回主工作区、不复制内容的目录。
fn is_linked_dir(name: &str, at_root: bool) -> bool {
    matches!(name, "node_modules" | ".venv" | "venv") || (at_root && name == "target")
}

fn copy_entry(
    source: &Path,
    dest: &Path,
    at_root: bool,
    relative: &Path,
    linked_dirs: &mut Vec<String>,
) -> IsolationResult<()> {
    let file_type = fs::symlink_metadata(source)
        .map_err(|error| IsolationError::io("读取文件信息", source, error))?
        .file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| IsolationError::io("读取符号链接", source, error))?;
        return create_symlink(&target, dest, source.is_dir())
            .map_err(|error| IsolationError::io("创建符号链接", dest, error));
    }
    if file_type.is_dir() {
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_linked_dir(&name, at_root) {
            let absolute = fs::canonicalize(source)
                .map_err(|error| IsolationError::io("解析目录路径", source, error))?;
            if create_symlink(&absolute, dest, true).is_ok() {
                linked_dirs.push(relative.to_string_lossy().replace('\\', "/"));
                return Ok(());
            }
        }
        fs::create_dir(dest).map_err(|error| IsolationError::io("创建目录", dest, error))?;
        for child in fs::read_dir(source)
            .map_err(|error| IsolationError::io("读取目录", source, error))?
            .filter_map(Result::ok)
        {
            let child_name = child.file_name();
            copy_entry(
                &child.path(),
                &dest.join(&child_name),
                false,
                &relative.join(&child_name),
                linked_dirs,
            )?;
        }
        if let Ok(metadata) = fs::metadata(source) {
            let _ = fs::set_permissions(dest, metadata.permissions());
        }
        return Ok(());
    }
    if file_type.is_file() {
        fs::copy(source, dest).map_err(|error| IsolationError::io("复制文件", source, error))?;
    }
    // 套接字、设备文件等特殊文件不复制。
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path, _is_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// 把相对路径解析到根目录下；绝对路径、`..` 等会逃出根目录的写法一律拒绝。
pub(crate) fn join_relative(root: &Path, relative: &str) -> IsolationResult<PathBuf> {
    use std::path::Component;
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(IsolationError::UnsafePath(relative.to_string()));
    }
    Ok(root.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("README.md"), "# demo\n").unwrap();
        fs::create_dir_all(root.join(".magi/snapshots")).unwrap();
        fs::write(root.join(".magi/snapshots/blob"), "state").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(
            root.join("node_modules/pkg/index.js"),
            "module.exports = 1;\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/out"), "artifact").unwrap();
        dir
    }

    #[test]
    fn copy_fallback_replicates_files_and_links_heavy_directories() {
        let source = fixture();
        let dest_parent = tempfile::tempdir().unwrap();
        let dest = dest_parent.path().join("copy");
        let outcome = create_isolated_copy_with(source.path(), &dest, false).unwrap();

        assert_eq!(outcome.strategy, CloneStrategy::Copy);
        assert_eq!(
            fs::read_to_string(dest.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert_eq!(
            fs::read_to_string(dest.join("README.md")).unwrap(),
            "# demo\n"
        );
        assert!(!dest.join(".magi").exists(), "Magi 运行态不属于副本");
        assert_eq!(outcome.linked_dirs, vec!["node_modules", "target"]);
        assert!(
            fs::symlink_metadata(dest.join("node_modules"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // 链接指回主工作区：副本里能读到依赖，但没有复制它们。
        assert_eq!(
            fs::read_to_string(dest.join("node_modules/pkg/index.js")).unwrap(),
            "module.exports = 1;\n"
        );
        // 改副本不影响主工作区。
        fs::write(dest.join("src/main.rs"), "changed").unwrap();
        assert_eq!(
            fs::read_to_string(source.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_executable_bit_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let source = fixture();
        let script = source.path().join("run.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("README.md", source.path().join("link")).unwrap();

        let dest_parent = tempfile::tempdir().unwrap();
        let dest = dest_parent.path().join("copy");
        create_isolated_copy_with(source.path(), &dest, false).unwrap();

        let mode = fs::metadata(dest.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        assert_eq!(
            fs::read_link(dest.join("link")).unwrap(),
            PathBuf::from("README.md")
        );
    }

    #[test]
    fn default_strategy_produces_an_equivalent_independent_copy() {
        let source = fixture();
        let dest_parent = tempfile::tempdir().unwrap();
        let dest = dest_parent.path().join("copy");
        let outcome = create_isolated_copy(source.path(), &dest).unwrap();

        // 无论走写时复制还是回退到普通复制，源码都必须完整且互相独立。
        assert_eq!(
            fs::read_to_string(dest.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(!dest.join(".magi").exists());
        fs::write(dest.join("README.md"), "changed").unwrap();
        assert_eq!(
            fs::read_to_string(source.path().join("README.md")).unwrap(),
            "# demo\n"
        );
        if outcome.strategy == CloneStrategy::Clone {
            assert!(outcome.linked_dirs.is_empty());
            assert!(dest.join("node_modules/pkg/index.js").is_file());
        }
    }

    #[test]
    fn linked_worktree_git_marker_is_not_copied() {
        let source = fixture();
        fs::write(
            source.path().join(".git"),
            "gitdir: /elsewhere/.git/worktrees/x\n",
        )
        .unwrap();
        let dest_parent = tempfile::tempdir().unwrap();
        let dest = dest_parent.path().join("copy");
        let outcome = create_isolated_copy(source.path(), &dest).unwrap();
        assert!(!outcome.git_available);
        assert!(
            !dest.join(".git").exists(),
            "指向外部 gitdir 的 .git 文件会让副本共用主工作区的索引"
        );
    }

    #[test]
    fn standalone_git_directory_is_copied_so_git_works_inside_the_copy() {
        let source = fixture();
        fs::create_dir_all(source.path().join(".git/objects")).unwrap();
        fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let dest_parent = tempfile::tempdir().unwrap();
        let dest = dest_parent.path().join("copy");
        let outcome = create_isolated_copy(source.path(), &dest).unwrap();
        assert!(outcome.git_available);
        assert!(dest.join(".git/HEAD").is_file());
    }

    #[test]
    fn existing_destination_is_rejected() {
        let source = fixture();
        let dest = tempfile::tempdir().unwrap();
        assert!(matches!(
            create_isolated_copy(source.path(), dest.path()),
            Err(IsolationError::DestinationExists(_))
        ));
    }

    #[test]
    fn relative_paths_cannot_escape_the_root() {
        let root = Path::new("/root");
        assert!(join_relative(root, "src/a.rs").is_ok());
        assert!(join_relative(root, "../etc/passwd").is_err());
        assert!(join_relative(root, "/etc/passwd").is_err());
        assert!(join_relative(root, "").is_err());
    }
}
