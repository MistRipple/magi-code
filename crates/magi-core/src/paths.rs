//! 跨 Tier 4 store + project-memory 共享的文件系统布局工具。
//!
//! 八个 store（Charter / Plan / KG / Validation / Workspace / Checkpoint /
//! HumanCheckpoint / ProjectMemory）共享 `<magi_home>/projects/<slug>/` 前缀，
//! 必须使用同一个 slug 函数——否则同一 workspace 在不同 store 会落到不同目录。
//! 本模块把规则收敛到唯一来源。
//!
//! 物理布局：
//! - mission stores：`<magi_home>/projects/<slug>/missions/<mission_id>/<artifact>`
//! - project memory：`<magi_home>/projects/<slug>/memory/<entry>`

use crate::{MissionId, WorkspaceRootPath};
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

/// `<magi_home>/projects/<slug>/missions` —— 单个 workspace 下所有 mission 的根。
pub fn missions_root(magi_home: &Path, workspace_root: &WorkspaceRootPath) -> PathBuf {
    project_root(magi_home, workspace_root).join("missions")
}

/// 指定 mission 的目录：`<magi_home>/projects/<slug>/missions/<mission_id>`。
pub fn mission_dir(
    magi_home: &Path,
    workspace_root: &WorkspaceRootPath,
    mission_id: &MissionId,
) -> PathBuf {
    missions_root(magi_home, workspace_root).join(mission_id.as_str())
}

/// `<magi_home>/projects/<slug>/memory` —— project memory 根目录。
pub fn project_memory_root(magi_home: &Path, workspace_root: &WorkspaceRootPath) -> PathBuf {
    project_root(magi_home, workspace_root).join("memory")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn missions_root_composes_projects_slug_missions() {
        let home = PathBuf::from("/tmp/.magi");
        let root = WorkspaceRootPath::from("/Users/x/proj");
        assert_eq!(
            missions_root(&home, &root),
            PathBuf::from("/tmp/.magi/projects/-Users-x-proj/missions")
        );
    }

    #[test]
    fn mission_dir_appends_mission_id() {
        let home = PathBuf::from("/tmp/.magi");
        let root = WorkspaceRootPath::from("/Users/x/proj");
        let mission = MissionId::new("M-001");
        assert_eq!(
            mission_dir(&home, &root, &mission),
            PathBuf::from("/tmp/.magi/projects/-Users-x-proj/missions/M-001")
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
