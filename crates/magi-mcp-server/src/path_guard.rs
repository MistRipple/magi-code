//! 路径与范围限制：令牌只对绑定的工作区有效。
//!
//! 所有路径参数先规范化再校验：必须落在工作区根内，拒绝 `..` 逃逸与符号链接逃逸，
//! 写入目标的父目录链也要校验；Magi 状态目录、凭据目录等敏感位置永远拒绝。
//! 黑名单集中在本文件，不散落到调用方。

use std::path::{Component, Path, PathBuf};

/// 无论读写都拒绝的路径组件（Magi 运行态、凭据目录）。
const DENIED_COMPONENTS: &[&str] = &[".magi", ".ssh", ".gnupg"];

/// 只拒绝写入的路径组件（读取仓库元数据可以，改写它不行）。
const DENIED_WRITE_COMPONENTS: &[&str] = &[".git"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathAccess {
    Read,
    Write,
}

/// 调用方声明的一次路径访问。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRequest {
    pub raw: String,
    pub access: PathAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathViolation {
    #[error("路径不能为空")]
    Empty,
    #[error("路径包含非法字符")]
    InvalidCharacter,
    #[error("路径超出工作区范围")]
    OutsideWorkspace,
    #[error("路径经过符号链接后超出工作区范围")]
    SymlinkEscape,
    #[error("该位置不允许访问")]
    Denied,
    #[error("工作区根目录不可用: {0}")]
    WorkspaceUnavailable(String),
}

#[derive(Clone, Debug)]
pub struct PathGuard {
    root: PathBuf,
}

impl PathGuard {
    /// `workspace_root` 必须是已存在的目录；内部保存其规范化后的真实路径。
    pub fn new(workspace_root: &Path) -> Result<Self, PathViolation> {
        let root = workspace_root
            .canonicalize()
            .map_err(|error| PathViolation::WorkspaceUnavailable(error.to_string()))?;
        if !root.is_dir() {
            return Err(PathViolation::WorkspaceUnavailable("不是目录".to_string()));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 校验并返回规范化后的绝对路径（目标不存在时返回其在工作区内的词法规范路径）。
    pub fn check(&self, raw: &str, access: PathAccess) -> Result<PathBuf, PathViolation> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(PathViolation::Empty);
        }
        if trimmed.contains('\0') {
            return Err(PathViolation::InvalidCharacter);
        }

        let joined = {
            let candidate = Path::new(trimmed);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                self.root.join(candidate)
            }
        };
        let lexical = lexically_normalize(&joined).ok_or(PathViolation::OutsideWorkspace)?;
        if !lexical.starts_with(&self.root) {
            // 绝对路径可能经由符号链接才落进工作区（如 /tmp → /private/tmp），
            // 这里只信任规范化后的真实路径，因此继续用真实路径判断。
            let resolved = resolve_existing_prefix(&lexical);
            if !resolved.starts_with(&self.root) {
                return Err(PathViolation::OutsideWorkspace);
            }
        }

        // 最长的已存在前缀必须真实落在工作区内（符号链接逃逸）。
        let resolved = resolve_existing_prefix(&lexical);
        if !resolved.starts_with(&self.root) {
            return Err(PathViolation::SymlinkEscape);
        }

        let relative = resolved
            .strip_prefix(&self.root)
            .map_err(|_| PathViolation::OutsideWorkspace)?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let name = name.to_string_lossy();
            if DENIED_COMPONENTS.iter().any(|denied| name == *denied) {
                return Err(PathViolation::Denied);
            }
            if access == PathAccess::Write
                && DENIED_WRITE_COMPONENTS.iter().any(|denied| name == *denied)
            {
                return Err(PathViolation::Denied);
            }
        }
        Ok(resolved)
    }

    /// 校验一组访问，任何一项违规就整体拒绝，并返回全部规范化路径。
    pub fn check_all(&self, requests: &[PathRequest]) -> Result<Vec<PathBuf>, PathViolation> {
        requests
            .iter()
            .map(|request| self.check(&request.raw, request.access))
            .collect()
    }
}

/// 不访问文件系统的词法规范化：消解 `.` 与 `..`；`..` 越过根返回 `None`。
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    Some(out)
}

/// 把最长的已存在前缀替换成其真实路径，再拼回不存在的尾部。
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(real) => {
                let mut result = real;
                for part in tail.into_iter().rev() {
                    result.push(part);
                }
                return result;
            }
            Err(_) => {
                let Some(name) = existing.file_name().map(|name| name.to_os_string()) else {
                    return path.to_path_buf();
                };
                tail.push(name);
                if !existing.pop() {
                    return path.to_path_buf();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> (tempfile::TempDir, PathGuard) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let guard = PathGuard::new(dir.path()).unwrap();
        (dir, guard)
    }

    #[test]
    fn relative_and_absolute_paths_inside_the_workspace_pass() {
        let (_dir, guard) = guard();
        let read = guard.check("src/main.rs", PathAccess::Read).unwrap();
        assert!(read.starts_with(guard.root()));
        let absolute = guard.root().join("src/main.rs");
        assert!(
            guard
                .check(absolute.to_str().unwrap(), PathAccess::Read)
                .is_ok()
        );
        // 尚不存在的写入目标（父目录存在）通过。
        assert!(guard.check("src/new_file.rs", PathAccess::Write).is_ok());
        // 尚不存在的多级目录也通过（创建目录的场景）。
        assert!(guard.check("a/b/c.txt", PathAccess::Write).is_ok());
    }

    #[test]
    fn parent_escape_and_outside_absolute_paths_are_rejected() {
        let (_dir, guard) = guard();
        assert_eq!(
            guard.check("../outside.txt", PathAccess::Read),
            Err(PathViolation::OutsideWorkspace)
        );
        assert_eq!(
            guard.check("src/../../outside.txt", PathAccess::Write),
            Err(PathViolation::OutsideWorkspace)
        );
        assert_eq!(
            guard.check("/etc/passwd", PathAccess::Read),
            Err(PathViolation::OutsideWorkspace)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_for_existing_and_new_targets() {
        let (dir, guard) = guard();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        assert_eq!(
            guard.check("link/secret.txt", PathAccess::Read),
            Err(PathViolation::SymlinkEscape)
        );
        // 通过符号链接写入一个尚不存在的文件同样被拒绝。
        assert_eq!(
            guard.check("link/new.txt", PathAccess::Write),
            Err(PathViolation::SymlinkEscape)
        );
    }

    #[test]
    fn sensitive_locations_are_denied_and_git_is_write_protected() {
        let (dir, guard) = guard();
        std::fs::create_dir_all(dir.path().join(".magi")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        assert_eq!(
            guard.check(".magi/session-projections/x.json", PathAccess::Read),
            Err(PathViolation::Denied)
        );
        assert_eq!(
            guard.check(".ssh/id_rsa", PathAccess::Read),
            Err(PathViolation::Denied)
        );
        assert!(guard.check(".git/HEAD", PathAccess::Read).is_ok());
        assert_eq!(
            guard.check(".git/config", PathAccess::Write),
            Err(PathViolation::Denied)
        );
    }

    #[test]
    fn empty_and_nul_paths_are_rejected_and_batches_fail_as_a_whole() {
        let (_dir, guard) = guard();
        assert_eq!(
            guard.check("  ", PathAccess::Read),
            Err(PathViolation::Empty)
        );
        assert_eq!(
            guard.check("src/a\0b", PathAccess::Read),
            Err(PathViolation::InvalidCharacter)
        );
        let batch = [
            PathRequest {
                raw: "src/main.rs".to_string(),
                access: PathAccess::Read,
            },
            PathRequest {
                raw: "../x".to_string(),
                access: PathAccess::Write,
            },
        ];
        assert!(guard.check_all(&batch).is_err());
    }

    #[test]
    fn a_missing_workspace_root_is_unavailable() {
        assert!(matches!(
            PathGuard::new(Path::new("/definitely/not/here")),
            Err(PathViolation::WorkspaceUnavailable(_))
        ));
    }
}
