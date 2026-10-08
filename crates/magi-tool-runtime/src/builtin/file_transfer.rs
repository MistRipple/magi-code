//! 文件与目录的删除、复制、移动。
//!
//! 共同约定：
//! - 先检查再动手：源不存在、目标已存在、类型不匹配、目标落在源目录内部，都在改动任何东西之前失败。
//! - 覆盖不先删目标：文件覆盖靠 rename 原子替换；覆盖已存在目录必须额外显式确认，
//!   并且先把旧目录挪到备份位置，新内容就位后才删除，失败则恢复。
//! - 中途失败要说清楚目标处于什么状态，不让模型凭空猜测。

use super::{
    context_working_directory,
    failure::{
        FsFailureKind, ToolFailure, filesystem_failure, invalid_input, path_resolution_failure,
    },
    field_bool, field_string,
    fs_support::{is_same_or_inside, sibling_temp_path},
    parse_json_object, resolve_path_with_context,
};
use crate::{BuiltinToolAccessMode, ToolExecutionContext};
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

fn required_path(
    request: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    tool: &str,
) -> Result<String, String> {
    match field_string(request, key) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(invalid_input(tool, format!("缺少 {key} 字段"))),
    }
}

fn is_dir_nofollow(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_dir()
}

// ══════════════════════════════════════════════════════════════════════════════
// file_remove
// ══════════════════════════════════════════════════════════════════════════════

pub(super) fn execute_file_remove(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input("file_remove", "输入必须为 JSON 对象，包含 path 字段");
    };
    let path_input = match required_path(&request, "path", "file_remove") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let recursive = field_bool(&request, "recursive").unwrap_or(false);
    let path = match resolve_path_with_context(&path_input, context) {
        Ok(path) => path,
        Err(error) => return path_resolution_failure("file_remove", &path_input, &error),
    };

    // 不跟随符号链接：悬空链接、指向目录的链接都按链接本身删除。
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return filesystem_failure("file_remove", "读取待删除路径信息", &path, &error)
                .into_payload();
        }
    };
    if let Some(reason) = protected_remove_target_reason(&path_input, &path, context) {
        return ToolFailure::new("file_remove", "protected_path", reason)
            .rejected()
            .instruction("该路径受保护，不能删除；不要重试，改为删除其中具体的文件或子目录。")
            .into_payload();
    }

    let is_dir = is_dir_nofollow(&metadata);
    let result = if is_dir {
        if recursive {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_dir(&path)
        }
    } else {
        fs::remove_file(&path)
    };
    if let Err(error) = result {
        let mut failure = filesystem_failure("file_remove", "删除文件或目录", &path, &error);
        if FsFailureKind::classify(&error) == FsFailureKind::DirectoryNotEmpty {
            failure = failure.instruction(
                "目录非空：确认要连同内容一起删除时设置 recursive=true；否则先处理目录里的内容。",
            );
        }
        return failure.into_payload();
    }

    serde_json::json!({
        "tool": "file_remove",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "was_directory": is_dir,
        "recursive": recursive,
        "summary": format!("已删除 {}", path.display())
    })
    .to_string()
}

fn protected_remove_target_reason(
    raw_input: &str,
    path: &Path,
    context: &ToolExecutionContext,
) -> Option<&'static str> {
    const PROTECTED: &str = "该路径受保护，不能删除";
    let trimmed = raw_input.trim();
    if matches!(trimmed, "/" | "." | ".." | "~") {
        tracing::warn!(
            tool = "file_remove",
            requested_path = trimmed,
            "file_remove rejected protected raw path"
        );
        return Some(PROTECTED);
    }

    let canonical_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical_target.parent().is_none() {
        tracing::warn!(
            tool = "file_remove",
            target = %canonical_target.display(),
            "file_remove rejected filesystem root"
        );
        return Some(PROTECTED);
    }

    if let Ok(cwd) = context_working_directory(context)
        && let Ok(canonical_cwd) = cwd.canonicalize()
        && canonical_target == canonical_cwd
    {
        tracing::warn!(
            tool = "file_remove",
            target = %canonical_target.display(),
            "file_remove rejected current working directory"
        );
        return Some(PROTECTED);
    }

    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|path| path.canonicalize().ok())
        && canonical_target == home
    {
        tracing::warn!(
            tool = "file_remove",
            target = %canonical_target.display(),
            "file_remove rejected home directory"
        );
        return Some(PROTECTED);
    }
    None
}

// ══════════════════════════════════════════════════════════════════════════════
// 复制原语
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Default)]
struct CopyStats {
    files: usize,
    symlinks: usize,
    skipped_symlinks: usize,
}

/// 先复制到目标同目录的临时文件，再 rename 就位：覆盖已有文件时不会出现写了一半的目标。
fn copy_file_into_place(src: &Path, dst: &Path) -> io::Result<()> {
    let temp = sibling_temp_path(dst, "copy");
    let result = fs::copy(src, &temp).and_then(|_| fs::rename(&temp, dst));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path, stats: &mut CopyStats) -> io::Result<()> {
    let target = fs::read_link(src)?;
    if fs::symlink_metadata(dst).is_ok() {
        fs::remove_file(dst)?;
    }
    std::os::unix::fs::symlink(target, dst)?;
    stats.symlinks += 1;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(_src: &Path, _dst: &Path, stats: &mut CopyStats) -> io::Result<()> {
    stats.skipped_symlinks += 1;
    Ok(())
}

/// 复制目录树。符号链接按链接复制（不跟随），调用方负责保证目标不在源内部。
fn copy_tree(src: &Path, dst: &Path, stats: &mut CopyStats) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&from, &to, stats)?;
        } else if file_type.is_symlink() {
            copy_symlink(&from, &to, stats)?;
        } else {
            copy_file_into_place(&from, &to)?;
            stats.files += 1;
        }
    }
    Ok(())
}

fn copy_summary(stats: &CopyStats) -> serde_json::Value {
    serde_json::json!({
        "files_copied": stats.files,
        "symlinks_copied": stats.symlinks,
        "symlinks_skipped": stats.skipped_symlinks,
    })
}

/// 复制与移动共同的前置检查结果。
struct TransferPlan {
    src: PathBuf,
    dst: PathBuf,
    src_is_dir: bool,
    /// 目标已存在时它是否是目录；不存在为 `None`。
    dst_existing_is_dir: Option<bool>,
}

enum TransferCheck {
    Ready(TransferPlan),
    Failed(String),
}

fn check_transfer(
    tool: &str,
    request: &serde_json::Map<String, serde_json::Value>,
    context: &ToolExecutionContext,
    overwrite: bool,
) -> TransferCheck {
    let src_input = match required_path(request, "source", tool) {
        Ok(value) => value,
        Err(failure) => return TransferCheck::Failed(failure),
    };
    let dst_input = match required_path(request, "destination", tool) {
        Ok(value) => value,
        Err(failure) => return TransferCheck::Failed(failure),
    };
    let src = match resolve_path_with_context(&src_input, context) {
        Ok(path) => path,
        Err(error) => {
            return TransferCheck::Failed(path_resolution_failure(tool, &src_input, &error));
        }
    };
    let dst = match resolve_path_with_context(&dst_input, context) {
        Ok(path) => path,
        Err(error) => {
            return TransferCheck::Failed(path_resolution_failure(tool, &dst_input, &error));
        }
    };

    let src_meta = match fs::symlink_metadata(&src) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return TransferCheck::Failed(
                ToolFailure::new(tool, "source_not_found", "源路径不存在")
                    .instruction(
                        "用 file_read 读取其父目录确认实际路径后，再用正确的 source 调用。",
                    )
                    .into_payload(),
            );
        }
        Err(error) => {
            return TransferCheck::Failed(
                filesystem_failure(tool, "读取源路径信息", &src, &error).into_payload(),
            );
        }
    };
    let src_is_dir = is_dir_nofollow(&src_meta);

    if is_same_or_inside(&dst, &src) && (src_is_dir || is_same_or_inside(&src, &dst)) {
        let (kind, error) = if is_same_or_inside(&src, &dst) {
            ("same_path", "源与目标是同一个路径")
        } else {
            ("destination_inside_source", "目标位于源目录内部")
        };
        return TransferCheck::Failed(
            ToolFailure::new(tool, kind, error)
                .instruction("换一个不在源目录内部、也不同于源的目标路径。")
                .into_payload(),
        );
    }

    let dst_existing_is_dir = match fs::symlink_metadata(&dst) {
        Ok(metadata) => Some(is_dir_nofollow(&metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return TransferCheck::Failed(
                filesystem_failure(tool, "读取目标路径信息", &dst, &error).into_payload(),
            );
        }
    };

    if let Some(dst_is_dir) = dst_existing_is_dir {
        if !overwrite {
            return TransferCheck::Failed(
                ToolFailure::new(tool, "already_exists", "目标已存在，且 overwrite=false")
                    .instruction("换一个目标路径；确认要覆盖时设置 overwrite=true。")
                    .into_payload(),
            );
        }
        if dst_is_dir != src_is_dir {
            return TransferCheck::Failed(
                ToolFailure::new(
                    tool,
                    "type_mismatch",
                    "源和目标一个是目录、一个是文件，不能互相覆盖",
                )
                .instruction("换一个目标路径，或先用 file_remove 处理掉目标后再操作。")
                .into_payload(),
            );
        }
    }

    TransferCheck::Ready(TransferPlan {
        src,
        dst,
        src_is_dir,
        dst_existing_is_dir,
    })
}

fn ensure_parent_dir(tool: &str, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent()
        && !parent.exists()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return Err(filesystem_failure(tool, "创建目标父目录", parent, &error).into_payload());
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// file_copy
// ══════════════════════════════════════════════════════════════════════════════

pub(super) fn execute_file_copy(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input(
            "file_copy",
            "输入必须为 JSON 对象，包含 source 和 destination 字段",
        );
    };
    let overwrite = field_bool(&request, "overwrite").unwrap_or(false);
    let plan = match check_transfer("file_copy", &request, context, overwrite) {
        TransferCheck::Ready(plan) => plan,
        TransferCheck::Failed(failure) => return failure,
    };
    if let Err(failure) = ensure_parent_dir("file_copy", &plan.dst) {
        return failure;
    }

    let mut stats = CopyStats::default();
    let result = if plan.src_is_dir {
        copy_tree(&plan.src, &plan.dst, &mut stats)
    } else {
        copy_file_into_place(&plan.src, &plan.dst).map(|()| stats.files += 1)
    };
    if let Err(error) = result {
        let mut failure = filesystem_failure("file_copy", "复制", &plan.src, &error);
        if plan.src_is_dir {
            failure = failure
                .instruction(
                    "目标目录可能已部分写入；用 file_read 查看目标目录，必要时先清理再重新复制。",
                )
                .with("destination_partially_written", true);
        }
        return failure.into_payload();
    }

    let mut payload = serde_json::json!({
        "tool": "file_copy",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "source": plan.src.display().to_string(),
        "destination": plan.dst.display().to_string(),
        "is_directory": plan.src_is_dir,
        "merged_into_existing_directory": plan.src_is_dir && plan.dst_existing_is_dir == Some(true),
        "summary": format!("已复制 {} → {}", plan.src.display(), plan.dst.display())
    });
    if let (Some(target), Some(extra)) = (payload.as_object_mut(), copy_summary(&stats).as_object())
    {
        target.extend(extra.clone());
    }
    payload.to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// file_move
// ══════════════════════════════════════════════════════════════════════════════

pub(super) fn execute_file_move(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input(
            "file_move",
            "输入必须为 JSON 对象，包含 source 和 destination 字段",
        );
    };
    let overwrite = field_bool(&request, "overwrite").unwrap_or(false);
    let confirm_replace_directory =
        field_bool(&request, "confirm_replace_directory").unwrap_or(false);
    let plan = match check_transfer("file_move", &request, context, overwrite) {
        TransferCheck::Ready(plan) => plan,
        TransferCheck::Failed(failure) => return failure,
    };

    let replaces_directory = plan.dst_existing_is_dir == Some(true);
    if replaces_directory && !confirm_replace_directory {
        return ToolFailure::new(
            "file_move",
            "directory_overwrite_requires_confirmation",
            "目标是已存在的目录，覆盖会整体替换它的全部内容",
        )
        .instruction(
            "确认要替换该目录时，同时设置 overwrite=true 和 confirm_replace_directory=true；否则换一个目标路径。",
        )
        .into_payload();
    }
    if let Err(failure) = ensure_parent_dir("file_move", &plan.dst) {
        return failure;
    }

    // 替换目录：旧目录先挪到备份位置，新内容就位后才删除，任何一步失败都能恢复。
    let backup = if replaces_directory {
        let backup = sibling_temp_path(&plan.dst, "replaced");
        if let Err(error) = fs::rename(&plan.dst, &backup) {
            return filesystem_failure("file_move", "备份将被替换的目录", &plan.dst, &error)
                .into_payload();
        }
        Some(backup)
    } else {
        None
    };
    let restore_backup = |backup: &Option<PathBuf>| {
        if let Some(backup) = backup
            && let Err(error) = fs::rename(backup, &plan.dst)
        {
            tracing::error!(
                backup = %backup.display(),
                destination = %plan.dst.display(),
                error = %error,
                "file_move failed to restore the replaced directory"
            );
        }
    };

    let mut stats = CopyStats::default();
    match fs::rename(&plan.src, &plan.dst) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {
            if let Err(error) = copy_across_devices(&plan, &mut stats) {
                restore_backup(&backup);
                return filesystem_failure("file_move", "跨磁盘移动", &plan.src, &error)
                    .instruction("移动未生效，源保持原样；换一个同磁盘的目标，或改用 file_copy 加 file_remove。")
                    .into_payload();
            }
            let removed = if plan.src_is_dir {
                fs::remove_dir_all(&plan.src)
            } else {
                fs::remove_file(&plan.src)
            };
            if let Err(error) = removed {
                finish_backup(&backup);
                return filesystem_failure("file_move", "删除移动后的源路径", &plan.src, &error)
                    .instruction("内容已经复制到目标，但源路径没能删除；确认目标无误后，用 file_remove 删除源。")
                    .with("destination_written", true)
                    .into_payload();
            }
        }
        Err(error) => {
            restore_backup(&backup);
            return filesystem_failure("file_move", "移动", &plan.src, &error).into_payload();
        }
    }

    let cleanup_warning = finish_backup(&backup);
    let mut payload = serde_json::json!({
        "tool": "file_move",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "source": plan.src.display().to_string(),
        "destination": plan.dst.display().to_string(),
        "replaced_existing": plan.dst_existing_is_dir.is_some(),
        "summary": format!("已移动 {} → {}", plan.src.display(), plan.dst.display())
    });
    if let Some(warning) = cleanup_warning {
        payload["cleanup_warning"] = serde_json::Value::String(warning);
    }
    payload.to_string()
}

fn copy_across_devices(plan: &TransferPlan, stats: &mut CopyStats) -> io::Result<()> {
    let source_is_symlink = fs::symlink_metadata(&plan.src)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    let result = if source_is_symlink {
        copy_symlink(&plan.src, &plan.dst, stats)
    } else if plan.src_is_dir {
        copy_tree(&plan.src, &plan.dst, stats)
    } else {
        copy_file_into_place(&plan.src, &plan.dst)
    };
    // 移动会删除源：任何没能原样复制的内容（当前平台不支持的符号链接）都不能让源被删。
    let result = result.and_then(|()| {
        if stats.skipped_symlinks > 0 {
            Err(io::Error::new(
                ErrorKind::Unsupported,
                "源中包含当前平台无法复制的符号链接",
            ))
        } else {
            Ok(())
        }
    });
    if result.is_err() && plan.dst_existing_is_dir != Some(false) {
        // 目标原本不存在，或旧目录已被挪走备份：残留的是这次写到一半的内容。
        // 覆盖已有文件走临时文件加 rename，失败时原文件本身没有被动过，不能清理。
        let _ = if plan.src_is_dir && !source_is_symlink {
            fs::remove_dir_all(&plan.dst)
        } else {
            fs::remove_file(&plan.dst)
        };
    }
    result
}

/// 新内容已就位：删除被替换目录的备份。删不掉不影响移动结果，只提示残留位置。
fn finish_backup(backup: &Option<PathBuf>) -> Option<String> {
    let backup = backup.as_ref()?;
    match fs::remove_dir_all(backup) {
        Ok(()) => None,
        Err(error) => {
            tracing::warn!(
                backup = %backup.display(),
                error = %error,
                "file_move could not remove the replaced directory backup"
            );
            Some(format!(
                "被替换的旧目录备份未能删除，仍在 {}，可用 file_remove 清理",
                backup.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Workspace {
        root: PathBuf,
        context: ToolExecutionContext,
    }

    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "magi-file-transfer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).expect("create workspace");
            let context = ToolExecutionContext {
                working_directory: Some(root.clone()),
                ..ToolExecutionContext::default()
            };
            Self { root, context }
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("parents");
            fs::write(path, content).expect("write");
        }

        fn read(&self, relative: &str) -> String {
            fs::read_to_string(self.root.join(relative)).expect("read")
        }

        fn exists(&self, relative: &str) -> bool {
            fs::symlink_metadata(self.root.join(relative)).is_ok()
        }

        fn run(&self, tool: fn(&str, &ToolExecutionContext) -> String, input: Value) -> Value {
            serde_json::from_str(&tool(&input.to_string(), &self.context)).expect("json payload")
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn move_renames_files_and_reports_missing_source_distinctly() {
        let ws = Workspace::new();
        ws.write("a.txt", "A");

        let moved = ws.run(
            execute_file_move,
            json!({"source": "a.txt", "destination": "sub/b.txt"}),
        );
        assert_eq!(moved["status"], "succeeded");
        assert!(!ws.exists("a.txt"));
        assert_eq!(ws.read("sub/b.txt"), "A");

        let missing = ws.run(
            execute_file_move,
            json!({"source": "nope.txt", "destination": "c.txt"}),
        );
        assert_eq!(missing["error_code"], "file_move_source_not_found");
        assert!(missing["instruction"].as_str().is_some());
    }

    #[test]
    fn move_without_overwrite_keeps_both_sides_untouched() {
        let ws = Workspace::new();
        ws.write("a.txt", "A");
        ws.write("b.txt", "B");

        let payload = ws.run(
            execute_file_move,
            json!({"source": "a.txt", "destination": "b.txt"}),
        );

        assert_eq!(payload["error_code"], "file_move_already_exists");
        assert_eq!(ws.read("a.txt"), "A");
        assert_eq!(ws.read("b.txt"), "B");
    }

    #[test]
    fn move_overwrite_replaces_an_existing_file_atomically() {
        let ws = Workspace::new();
        ws.write("a.txt", "A");
        ws.write("b.txt", "B");

        let payload = ws.run(
            execute_file_move,
            json!({"source": "a.txt", "destination": "b.txt", "overwrite": true}),
        );

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["replaced_existing"], true);
        assert!(!ws.exists("a.txt"));
        assert_eq!(ws.read("b.txt"), "A");
    }

    #[test]
    fn move_overwrite_onto_a_directory_requires_explicit_confirmation() {
        let ws = Workspace::new();
        ws.write("src/new.txt", "new");
        ws.write("dst/old.txt", "old");

        let refused = ws.run(
            execute_file_move,
            json!({"source": "src", "destination": "dst", "overwrite": true}),
        );
        assert_eq!(
            refused["error_code"],
            "file_move_directory_overwrite_requires_confirmation"
        );
        assert_eq!(ws.read("dst/old.txt"), "old", "目标目录不能被动过");
        assert_eq!(ws.read("src/new.txt"), "new");

        let confirmed = ws.run(
            execute_file_move,
            json!({"source": "src", "destination": "dst", "overwrite": true, "confirm_replace_directory": true}),
        );
        assert_eq!(confirmed["status"], "succeeded");
        assert_eq!(ws.read("dst/new.txt"), "new");
        assert!(!ws.exists("dst/old.txt"), "旧目录内容被整体替换");
        assert!(!ws.exists("src"));
        let leftovers = fs::read_dir(&ws.root)
            .expect("list")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".magi-"))
            .count();
        assert_eq!(leftovers, 0, "备份目录不能残留");
    }

    #[test]
    fn move_rejects_type_mismatch_and_destination_inside_source_before_changing_anything() {
        let ws = Workspace::new();
        ws.write("dir/file.txt", "x");
        ws.write("file.txt", "y");

        let mismatch = ws.run(
            execute_file_move,
            json!({"source": "file.txt", "destination": "dir", "overwrite": true, "confirm_replace_directory": true}),
        );
        assert_eq!(mismatch["error_code"], "file_move_type_mismatch");

        let inside = ws.run(
            execute_file_move,
            json!({"source": "dir", "destination": "dir/inner"}),
        );
        assert_eq!(inside["error_code"], "file_move_destination_inside_source");

        let same = ws.run(
            execute_file_move,
            json!({"source": "file.txt", "destination": "./file.txt"}),
        );
        assert_eq!(same["error_code"], "file_move_same_path");

        assert_eq!(ws.read("dir/file.txt"), "x");
        assert_eq!(ws.read("file.txt"), "y");
        assert!(!ws.exists("dir/inner"));
    }

    #[test]
    fn copy_copies_trees_and_refuses_to_recurse_into_itself() {
        let ws = Workspace::new();
        ws.write("tree/a.txt", "A");
        ws.write("tree/nested/b.txt", "B");

        let copied = ws.run(
            execute_file_copy,
            json!({"source": "tree", "destination": "copy"}),
        );
        assert_eq!(copied["status"], "succeeded");
        assert_eq!(copied["files_copied"], 2);
        assert_eq!(ws.read("copy/nested/b.txt"), "B");

        let inside = ws.run(
            execute_file_copy,
            json!({"source": "tree", "destination": "tree/sub"}),
        );
        assert_eq!(inside["error_code"], "file_copy_destination_inside_source");
        assert!(!ws.exists("tree/sub"));
    }

    #[test]
    fn copy_overwrite_merges_directories_and_reports_it() {
        let ws = Workspace::new();
        ws.write("src/a.txt", "new-a");
        ws.write("dst/a.txt", "old-a");
        ws.write("dst/keep.txt", "keep");

        let refused = ws.run(
            execute_file_copy,
            json!({"source": "src", "destination": "dst"}),
        );
        assert_eq!(refused["error_code"], "file_copy_already_exists");

        let merged = ws.run(
            execute_file_copy,
            json!({"source": "src", "destination": "dst", "overwrite": true}),
        );
        assert_eq!(merged["merged_into_existing_directory"], true);
        assert_eq!(ws.read("dst/a.txt"), "new-a");
        assert_eq!(ws.read("dst/keep.txt"), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_symlinks_instead_of_following_them() {
        let ws = Workspace::new();
        ws.write("src/real.txt", "real");
        std::os::unix::fs::symlink("real.txt", ws.root.join("src/link.txt")).expect("symlink");

        let payload = ws.run(
            execute_file_copy,
            json!({"source": "src", "destination": "out"}),
        );

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["symlinks_copied"], 1);
        let link = fs::symlink_metadata(ws.root.join("out/link.txt")).expect("link meta");
        assert!(link.file_type().is_symlink());
    }

    #[test]
    fn remove_reports_missing_target_and_non_empty_directory_with_distinct_codes() {
        let ws = Workspace::new();
        ws.write("dir/file.txt", "x");

        let missing = ws.run(execute_file_remove, json!({"path": "ghost"}));
        assert_eq!(missing["error_code"], "file_remove_not_found");

        let non_empty = ws.run(execute_file_remove, json!({"path": "dir"}));
        assert_eq!(non_empty["error_code"], "file_remove_directory_not_empty");
        assert!(
            non_empty["instruction"]
                .as_str()
                .is_some_and(|text| text.contains("recursive"))
        );
        assert!(ws.exists("dir/file.txt"));

        let recursive = ws.run(
            execute_file_remove,
            json!({"path": "dir", "recursive": true}),
        );
        assert_eq!(recursive["status"], "succeeded");
        assert!(!ws.exists("dir"));
    }

    #[test]
    fn remove_protects_the_workspace_root_and_reports_a_rejection() {
        let ws = Workspace::new();

        let payload = ws.run(execute_file_remove, json!({"path": "."}));

        assert_eq!(payload["status"], "rejected");
        assert_eq!(payload["error_code"], "file_remove_protected_path");
        assert!(ws.root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn remove_deletes_dangling_and_directory_symlinks_as_links() {
        let ws = Workspace::new();
        ws.write("target/keep.txt", "keep");
        std::os::unix::fs::symlink("target", ws.root.join("dirlink")).expect("dir link");
        std::os::unix::fs::symlink("missing", ws.root.join("dangling")).expect("dangling");

        assert_eq!(
            ws.run(execute_file_remove, json!({"path": "dirlink"}))["status"],
            "succeeded"
        );
        assert_eq!(
            ws.run(execute_file_remove, json!({"path": "dangling"}))["status"],
            "succeeded"
        );
        assert_eq!(ws.read("target/keep.txt"), "keep", "链接指向的内容不能被删");
    }
}
