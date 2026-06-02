use crate::blob_store::BlobStore;
use crate::error::{SnapshotError, SnapshotResult};
use crate::types::{
    BINARY_BLOB_LIMIT, ContentKind, FileMeta, LARGE_TEXT_SUMMARY_BYTES, SymlinkInfo,
    TEXT_BLOB_LIMIT,
};
use ignore::{WalkBuilder, gitignore::Gitignore};
use std::fs;
use std::path::{Path, PathBuf};

/// 默认黑名单：非 git workspace 时强制排除的目录。
const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".magi",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    ".next",
    ".turbo",
    ".idea",
    ".vscode",
];

const BINARY_PROBE_BYTES: usize = 8192;

/// 探测一段字节是否为二进制。规则：包含 NUL 字节，或非可打印字节比例 > 30%。
pub fn looks_binary(probe: &[u8]) -> bool {
    if probe.is_empty() {
        return false;
    }
    if probe.contains(&0) {
        return true;
    }
    let non_printable = probe
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20))
        .count();
    non_printable * 100 / probe.len() > 30
}

/// 由文件后缀粗略判断 mime；只覆盖最常见情况，未识别返回 None。
pub fn guess_mime(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let m = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => return None,
    };
    Some(m.into())
}

/// 单文件分类 + 装入 blob，返回 FileMeta。
///
/// - 软链接：仅记录 target，不读取内容。
/// - 设备/socket/管道：记 special，无 blob。
/// - 文本 ≤ 5 MB：blob_hash = Some(zstd 压缩 blob)。
/// - 文本 > 5 MB：blob_hash = None，但 mtime/size 仍记。
/// - 二进制 ≤ 50 MB：blob_hash = Some(原样 blob)。
/// - 二进制 > 50 MB：blob_hash = None。
/// - IO/权限错误：error = Some(...)，blob_hash = None，size = 0。
pub fn read_file_meta(
    workspace_root: &Path,
    abs_path: &Path,
    blobs: &BlobStore,
) -> SnapshotResult<FileMeta> {
    let rel = relative_path(workspace_root, abs_path)?;

    let lstat = match fs::symlink_metadata(abs_path) {
        Ok(m) => m,
        Err(e) => {
            return Ok(FileMeta {
                path: rel,
                content_kind: ContentKind::Special,
                size: 0,
                mime: None,
                blob_hash: None,
                mtime_ms: None,
                symlink: None,
                error: Some(format!("symlink_metadata failed: {e}")),
            });
        }
    };

    if lstat.file_type().is_symlink() {
        let target = fs::read_link(abs_path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(FileMeta {
            path: rel,
            content_kind: ContentKind::Symlink,
            size: lstat.len(),
            mime: None,
            blob_hash: None,
            mtime_ms: mtime_ms(&lstat),
            symlink: Some(SymlinkInfo { target }),
            error: None,
        });
    }

    if !lstat.is_file() {
        return Ok(FileMeta {
            path: rel,
            content_kind: ContentKind::Special,
            size: lstat.len(),
            mime: None,
            blob_hash: None,
            mtime_ms: mtime_ms(&lstat),
            symlink: None,
            error: None,
        });
    }

    let size = lstat.len();
    let probe = read_probe(abs_path, size)?;
    let is_binary = looks_binary(&probe);

    if is_binary {
        if size > BINARY_BLOB_LIMIT {
            return Ok(FileMeta {
                path: rel,
                content_kind: ContentKind::Binary,
                size,
                mime: guess_mime(abs_path),
                blob_hash: None,
                mtime_ms: mtime_ms(&lstat),
                symlink: None,
                error: None,
            });
        }
        let bytes = match fs::read(abs_path) {
            Ok(b) => b,
            Err(e) => {
                return Ok(FileMeta {
                    path: rel,
                    content_kind: ContentKind::Binary,
                    size,
                    mime: guess_mime(abs_path),
                    blob_hash: None,
                    mtime_ms: mtime_ms(&lstat),
                    symlink: None,
                    error: Some(format!("read failed: {e}")),
                });
            }
        };
        let hash = blobs.put(&bytes, false)?;
        return Ok(FileMeta {
            path: rel,
            content_kind: ContentKind::Binary,
            size,
            mime: guess_mime(abs_path),
            blob_hash: Some(hash),
            mtime_ms: mtime_ms(&lstat),
            symlink: None,
            error: None,
        });
    }

    // 文本路径
    if size > TEXT_BLOB_LIMIT {
        return Ok(FileMeta {
            path: rel,
            content_kind: ContentKind::LargeText,
            size,
            mime: guess_mime(abs_path),
            blob_hash: None,
            mtime_ms: mtime_ms(&lstat),
            symlink: None,
            error: None,
        });
    }
    let bytes = match fs::read(abs_path) {
        Ok(b) => b,
        Err(e) => {
            return Ok(FileMeta {
                path: rel,
                content_kind: ContentKind::Text,
                size,
                mime: guess_mime(abs_path),
                blob_hash: None,
                mtime_ms: mtime_ms(&lstat),
                symlink: None,
                error: Some(format!("read failed: {e}")),
            });
        }
    };
    let hash = blobs.put(&bytes, true)?;
    Ok(FileMeta {
        path: rel,
        content_kind: ContentKind::Text,
        size,
        mime: guess_mime(abs_path),
        blob_hash: Some(hash),
        mtime_ms: mtime_ms(&lstat),
        symlink: None,
        error: None,
    })
}

/// 读取大文件头/尾摘要（每端 64 KB），用于 LargeText 前端展示。
pub fn read_large_text_summary(abs_path: &Path) -> (Option<String>, Option<String>) {
    let mut f = match std::fs::File::open(abs_path) {
        Ok(f) => f,
        Err(_) => return (None, None),
    };
    use std::io::{Read, Seek, SeekFrom};
    let mut head = vec![0u8; LARGE_TEXT_SUMMARY_BYTES];
    let head_n = f.read(&mut head).unwrap_or(0);
    head.truncate(head_n);

    let total = f.metadata().map(|m| m.len()).unwrap_or(0) as i64;
    let tail_off = (total - LARGE_TEXT_SUMMARY_BYTES as i64).max(head_n as i64);
    let mut tail = Vec::new();
    if tail_off > head_n as i64 {
        if f.seek(SeekFrom::Start(tail_off as u64)).is_ok() {
            f.read_to_end(&mut tail).ok();
        }
    }
    (
        Some(String::from_utf8_lossy(&head).into_owned()),
        if tail.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&tail).into_owned())
        },
    )
}

/// 全树扫描入口：返回 workspace_root 下所有合规文件相对路径（已排除黑名单）。
///
/// `respect_gitignore` = true 时启用 .gitignore；否则只走 DEFAULT_EXCLUDES。
pub fn walk_workspace(
    workspace_root: &Path,
    respect_gitignore: bool,
) -> SnapshotResult<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(workspace_root);
    builder
        .follow_links(false)
        .git_ignore(respect_gitignore)
        .git_exclude(respect_gitignore)
        .git_global(respect_gitignore)
        .ignore(respect_gitignore)
        .hidden(false)
        .require_git(false);
    for ex in DEFAULT_EXCLUDES {
        builder.add_custom_ignore_filename(ex);
    }
    let walker = builder.build();

    let mut paths = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, "walk error skipped");
                continue;
            }
        };
        let path = entry.path();
        if path == workspace_root {
            continue;
        }
        let depth = path.strip_prefix(workspace_root).ok();
        if let Some(rel) = depth {
            if has_default_excluded_component(rel) {
                continue;
            }
        }
        match entry.file_type() {
            Some(ft) if ft.is_file() || ft.is_symlink() => {
                paths.push(path.to_path_buf());
            }
            _ => {}
        }
    }
    Ok(paths)
}

#[derive(Clone)]
pub(crate) struct SnapshotPathFilter {
    workspace_root: PathBuf,
    gitignore: Option<Gitignore>,
}

impl SnapshotPathFilter {
    pub(crate) fn new(workspace_root: &Path, respect_gitignore: bool) -> Self {
        let gitignore = if respect_gitignore {
            build_gitignore(workspace_root)
        } else {
            None
        };
        Self {
            workspace_root: workspace_root.to_path_buf(),
            gitignore,
        }
    }

    pub(crate) fn excluded_prefixes(&self) -> Vec<PathBuf> {
        DEFAULT_EXCLUDES
            .iter()
            .map(|exclude| self.workspace_root.join(exclude))
            .collect()
    }

    pub(crate) fn excludes_abs_path(&self, abs: &Path) -> bool {
        let Ok(rel) = abs.strip_prefix(&self.workspace_root) else {
            return true;
        };
        if self.excludes_relative_path(rel) {
            return true;
        }
        self.gitignore.as_ref().is_some_and(|gitignore| {
            gitignore
                .matched_path_or_any_parents(abs, abs.is_dir())
                .is_ignore()
        })
    }

    pub(crate) fn excludes_relative_str(&self, rel: &str) -> bool {
        let rel = Path::new(rel);
        if self.excludes_relative_path(rel) {
            return true;
        }
        let abs = self.workspace_root.join(rel);
        self.gitignore.as_ref().is_some_and(|gitignore| {
            gitignore
                .matched_path_or_any_parents(&abs, abs.is_dir())
                .is_ignore()
        })
    }

    fn excludes_relative_path(&self, rel: &Path) -> bool {
        has_default_excluded_component(rel)
    }
}

fn build_gitignore(workspace_root: &Path) -> Option<Gitignore> {
    let gitignore_path = workspace_root.join(".gitignore");
    if !gitignore_path.is_file() {
        return None;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(workspace_root);
    if let Some(error) = builder.add(&gitignore_path) {
        tracing::warn!(
            path = %gitignore_path.display(),
            error = %error,
            "snapshot gitignore load reported partial error"
        );
    }
    match builder.build() {
        Ok(gitignore) => Some(gitignore),
        Err(error) => {
            tracing::warn!(
                path = %gitignore_path.display(),
                error = %error,
                "snapshot gitignore build failed"
            );
            None
        }
    }
}

fn has_default_excluded_component(rel: &Path) -> bool {
    rel.components().any(|component| {
        matches!(
            component,
            std::path::Component::Normal(name)
                if name.to_str().is_some_and(|s| DEFAULT_EXCLUDES.contains(&s))
        )
    })
}

fn relative_path(root: &Path, abs: &Path) -> SnapshotResult<String> {
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| SnapshotError::PathOutsideRoot(abs.display().to_string()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

fn read_probe(path: &Path, size: u64) -> SnapshotResult<Vec<u8>> {
    let read_n = (size as usize).min(BINARY_PROBE_BYTES);
    if read_n == 0 {
        return Ok(Vec::new());
    }
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let mut buf = vec![0u8; read_n];
    let n = f.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    Ok(buf)
}

fn mtime_ms(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detects_text_vs_binary() {
        assert!(!looks_binary(b"hello world\n"));
        assert!(looks_binary(b"hello\0world"));
    }

    #[test]
    fn relative_path_strips_root() {
        let root = Path::new("/a/b");
        assert_eq!(
            relative_path(root, Path::new("/a/b/c/d.txt")).unwrap(),
            "c/d.txt"
        );
    }

    #[test]
    fn walk_skips_default_excludes() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/foo")).unwrap();
        std::fs::write(dir.path().join("node_modules/foo/x.js"), "x").unwrap();
        std::fs::write(dir.path().join("real.txt"), "hi").unwrap();
        let paths = walk_workspace(dir.path(), false).unwrap();
        assert!(paths.iter().any(|p| p.ends_with("real.txt")));
        assert!(
            paths
                .iter()
                .all(|p| !p.to_string_lossy().contains("node_modules"))
        );
    }
}
