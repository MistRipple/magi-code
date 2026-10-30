//! 项目共享状态布局。新写入按稳定项目 ID 定位；旧路径 slug 只用于迁移读取。
//!
//! 项目历史和记忆共用 project_state_root，不能按默认 cwd 或隔离副本定位。
//!
//! 物理布局：
//! - project memory：`<state>/projects/<encodedWorkspaceId>/memory/<entry>`

use crate::{WorkspaceId, WorkspaceRootPath};
use std::path::{Path, PathBuf};

/// 把 workspace 绝对路径映射为目录名安全的 slug。
///
/// 规则（**所有 store 共用唯一实现**）：
/// 1. `/` 和 `\` 都视为路径分隔符，替换为 `-`；
/// 2. ASCII 字母数字、`-`、`_`、`.` 原样保留；
/// 3. 其它任意字符替换为 `_`（覆盖 `:`、空格、Unicode 等）；
/// 4. 输入空串返回 `_`。
///
/// 例：
/// - `/Users/x/proj` → `-Users-x-proj`
/// - `C:\proj` → `C_-proj`
/// - `""` → `_`
pub fn workspace_slug(absolute_path: &str) -> String {
    let mut slug = String::with_capacity(absolute_path.len());
    for ch in absolute_path.chars() {
        if ch == '/' || ch == '\\' {
            slug.push('-');
        } else if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            slug.push(ch);
        } else {
            slug.push('_');
        }
    }
    if slug.is_empty() {
        slug.push('_');
    }
    slug
}

/// `<magi_home>/projects/<slug>` —— 单个 workspace 在 magi home 下的根。
pub fn project_root(magi_home: &Path, workspace_root: &WorkspaceRootPath) -> PathBuf {
    let slug = workspace_slug(workspace_root.as_str());
    magi_home.join("projects").join(slug)
}

/// 旧 `<magi_home>/projects/<slug>/memory`，仅用于单向迁移读取。
pub fn project_memory_root(magi_home: &Path, workspace_root: &WorkspaceRootPath) -> PathBuf {
    project_root(magi_home, workspace_root).join("memory")
}

/// 按稳定项目身份定位共享状态；源目录和主要目录变化不改变此位置。
pub fn project_state_root(state_root: &Path, workspace_id: &WorkspaceId) -> PathBuf {
    state_root
        .join("projects")
        .join(crate::hex_lower(workspace_id.as_str().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids_keep_separators_case_and_unicode_distinct() {
        let root = Path::new("state");
        let roots = [
            "project/a",
            "project\\a",
            "project_a",
            "Project/a",
            "项目/a",
        ]
        .map(|id| project_state_root(root, &WorkspaceId::new(id)));
        let unique = roots.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), roots.len());
        for path in roots {
            let relative = path.strip_prefix(root.join("projects")).unwrap();
            assert_eq!(relative.components().count(), 1);
            assert!(
                relative
                    .to_string_lossy()
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            );
        }
    }

    #[test]
    fn workspace_slug_typical_absolute_path() {
        assert_eq!(workspace_slug("/Users/x/proj"), "-Users-x-proj");
    }

    #[test]
    fn workspace_slug_empty_string_returns_underscore() {
        assert_eq!(workspace_slug(""), "_");
    }

    #[test]
    fn workspace_slug_root_slash_returns_dash() {
        // `/` 只是一个分隔符，按规则替换为 `-`，不再特判为 `root`。
        assert_eq!(workspace_slug("/"), "-");
    }

    #[test]
    fn workspace_slug_windows_path_replaces_backslash_and_colon() {
        assert_eq!(workspace_slug("C:\\proj"), "C_-proj");
    }

    #[test]
    fn workspace_slug_special_chars_become_underscore() {
        assert_eq!(workspace_slug("/a b/c"), "-a_b-c");
    }

    #[test]
    fn workspace_slug_nested_path() {
        assert_eq!(
            workspace_slug("/Users/xie/code/magi-rust-rewrite"),
            "-Users-xie-code-magi-rust-rewrite"
        );
    }

    #[test]
    fn workspace_slug_preserves_dots_and_underscores() {
        assert_eq!(workspace_slug("/Users/x_y/a.b"), "-Users-x_y-a.b");
    }

    #[test]
    fn project_root_composes_projects_slug() {
        let home = PathBuf::from("/tmp/.magi");
        let root = WorkspaceRootPath::from("/Users/x/proj");
        assert_eq!(
            project_root(&home, &root),
            PathBuf::from("/tmp/.magi/projects/-Users-x-proj")
        );
    }

    #[test]
    fn project_memory_root_composes_projects_slug_memory() {
        let home = PathBuf::from("/tmp/.magi");
        let root = WorkspaceRootPath::from("/Users/x/proj");
        assert_eq!(
            project_memory_root(&home, &root),
            PathBuf::from("/tmp/.magi/projects/-Users-x-proj/memory")
        );
    }
}
