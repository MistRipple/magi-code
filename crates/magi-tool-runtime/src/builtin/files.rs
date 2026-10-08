//! 文件读取、写入、局部修改与差异预览。复制、移动、删除见 `file_transfer`。

use super::{
    failure::{filesystem_failure, invalid_input, path_resolution_failure},
    field_bool, field_string, field_usize, parse_json_object, resolve_path_with_context,
};
use crate::{BuiltinToolAccessMode, ToolExecutionContext};
use magi_core::{ToolFailure, fs_atomic::write_atomic_preserving_target};
use serde_json::Value;
use std::{fs, io::Read, path::Path};

const DEFAULT_FILE_READ_MAX_BYTES: usize = 64 * 1024;
const FILE_READ_MAX_BYTES: usize = 1024 * 1024;

fn non_empty_path_field(
    request: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<String, String> {
    match field_string(request, key) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(invalid_input(tool, format!("缺少 {key} 字段"))),
    }
}

fn content_hash_failure(tool: &str, path: &Path, error: impl std::fmt::Display) -> String {
    tracing::warn!(tool, path = %path.display(), error = %error, "content hash failed");
    ToolFailure::new(tool, "hash_failed", "计算内容版本失败")
        .instruction("原因已记录到日志；不要用相同参数重复调用，换一种方式或告知用户。")
        .into_payload()
}

pub(super) fn execute_file_read(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input("file_read", "输入必须为 JSON 对象，包含 path 字段");
    };
    let path_input = match non_empty_path_field(&request, "path", "file_read") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let max_bytes = field_usize(&request, "max_bytes")
        .unwrap_or(DEFAULT_FILE_READ_MAX_BYTES)
        .clamp(1, FILE_READ_MAX_BYTES);

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(path) => path,
        Err(error) => return path_resolution_failure("file_read", &path_input, &error),
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return filesystem_failure("file_read", "读取文件信息", &path, &error).into_payload();
        }
    };

    if metadata.is_dir() {
        let names: std::io::Result<Vec<std::ffi::OsString>> =
            fs::read_dir(&path).and_then(|entries| {
                entries
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect()
            });
        let entries = match names {
            Ok(names) => names,
            Err(error) => {
                return filesystem_failure("file_read", "读取目录", &path, &error).into_payload();
            }
        };
        let mut entries: Vec<String> = entries
            .into_iter()
            .map(|name| name.to_string_lossy().to_string())
            .collect();
        entries.sort();
        let content_hash = match magi_snapshot::path_content_hash(&path) {
            Ok(content_hash) => content_hash,
            Err(error) => return content_hash_failure("file_read", &path, error),
        };
        return serde_json::json!({
            "tool": "file_read",
            "status": "succeeded",
            "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
            "mode": "directory",
            "path": path.display().to_string(),
            "content_hash": content_hash,
            "entries": entries,
            "entry_count": entries.len(),
            "summary": format!("目录 {} 包含 {} 项", path.display(), entries.len())
        })
        .to_string();
    }

    let file_size_bytes = metadata.len();
    let mut bytes = Vec::with_capacity(max_bytes.saturating_add(1));
    let read_result = fs::File::open(&path).and_then(|file| {
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| ())
    });
    if let Err(error) = read_result {
        return filesystem_failure("file_read", "读取文件", &path, &error).into_payload();
    }
    let truncated = file_size_bytes > max_bytes as u64 || bytes.len() > max_bytes;
    bytes.truncate(max_bytes);

    let content = match decode_text_preview(bytes, truncated) {
        Ok(content) => content,
        Err(()) => {
            return ToolFailure::new(
                "file_read",
                "not_utf8_text",
                "文件不是 UTF-8 文本（可能是二进制文件或其他编码）",
            )
            .instruction(
                "不要再用 file_read 读取它。图片请用 view_image；其他二进制文件需要时用 shell_exec 调用对应工具（如 file、xxd）。",
            )
            .with("file_size_bytes", file_size_bytes)
            .into_payload();
        }
    };
    let content_hash = match magi_snapshot::path_content_hash(&path) {
        Ok(content_hash) => content_hash,
        Err(error) => return content_hash_failure("file_read", &path, error),
    };

    serde_json::json!({
        "tool": "file_read",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "mode": "file",
        "path": path.display().to_string(),
        "content_hash": content_hash,
        "file_size_bytes": file_size_bytes,
        "max_bytes": max_bytes,
        "bytes_read": content.len(),
        "truncated": truncated,
        "encoding": "utf-8",
        "content": content,
        "summary": if truncated {
            format!("已预览文件 {} 的前 {} 字节", path.display(), content.len())
        } else {
            format!("已读取文件 {}", path.display())
        }
    })
    .to_string()
}

/// 把读到的字节解码成文本。含 NUL 或不是 UTF-8 的内容不是文本，不能用替换字符伪装成文本；
/// 预览被截断时，末尾被切开的多字节字符不算错误，丢弃即可。
fn decode_text_preview(mut bytes: Vec<u8>, truncated: bool) -> Result<String, ()> {
    if bytes.contains(&0) {
        return Err(());
    }
    match std::str::from_utf8(&bytes) {
        Ok(_) => {}
        Err(error) if truncated && error.error_len().is_none() => {
            bytes.truncate(error.valid_up_to());
        }
        Err(_) => return Err(()),
    }
    String::from_utf8(bytes).map_err(|_| ())
}

pub(super) fn execute_file_write(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input(
            "file_write",
            "输入必须为 JSON 对象，包含 path 和 content 字段",
        );
    };
    let path_input = match non_empty_path_field(&request, "path", "file_write") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let Some(content) = field_string(&request, "content") else {
        return invalid_input("file_write", "缺少 content 字段");
    };
    let overwrite = field_bool(&request, "overwrite").unwrap_or(true);
    let create_dirs = field_bool(&request, "create_dirs").unwrap_or(true);

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(path) => path,
        Err(error) => return path_resolution_failure("file_write", &path_input, &error),
    };

    let existed_before = path.exists();
    if existed_before && !overwrite {
        return ToolFailure::new(
            "file_write",
            "already_exists",
            "目标已存在，且 overwrite=false",
        )
        .instruction("换一个目标路径；确认要覆盖时设置 overwrite=true。")
        .into_payload();
    }

    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        if !create_dirs {
            return ToolFailure::new(
                "file_write",
                "parent_not_found",
                "父目录不存在，且 create_dirs=false",
            )
            .instruction("设置 create_dirs=true，或先用 file_mkdir 创建父目录。")
            .into_payload();
        }
        if let Err(error) = fs::create_dir_all(parent) {
            return filesystem_failure("file_write", "创建父目录", parent, &error).into_payload();
        }
    }

    if let Err(error) = write_atomic_preserving_target(&path, content.as_bytes()) {
        return filesystem_failure("file_write", "写入文件", &path, &error).into_payload();
    }

    serde_json::json!({
        "tool": "file_write",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "bytes_written": content.len(),
        "created": !existed_before,
        "overwritten": existed_before,
        "summary": format!("已写入 {} ({} 字节)", path.display(), content.len())
    })
    .to_string()
}

pub(super) fn execute_file_patch(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input("file_patch", "输入必须为 JSON 对象");
    };
    let path_input = match non_empty_path_field(&request, "path", "file_patch") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let patches = match parse_patches(&request) {
        Ok(patches) => patches,
        Err(failure) => return failure,
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(path) => path,
        Err(error) => return path_resolution_failure("file_patch", &path_input, &error),
    };
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return filesystem_failure("file_patch", "读取待修改文件", &path, &error)
                .into_payload();
        }
    };

    // 多个 patch 依次应用：后一个在前一个的结果上匹配。
    let mut result = content;
    let mut matched = 0usize;
    let mut missing_matches = 0usize;
    let mut ambiguous_matches = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for (index, (old, new)) in patches.iter().enumerate() {
        let count = result.matches(old.as_str()).count();
        if count == 0 {
            missing_matches += 1;
            errors.push(format!("patch[{index}]: old_string 未在文件中找到"));
            continue;
        }
        if count > 1 {
            ambiguous_matches += 1;
            errors.push(format!(
                "patch[{index}]: old_string 匹配了 {count} 处（需要唯一匹配）"
            ));
            continue;
        }
        result = result.replacen(old, new, 1);
        matched += 1;
    }

    if !errors.is_empty() {
        let (kind, error, instruction) = match (missing_matches, ambiguous_matches) {
            (missing, 0) if matched == 0 && missing == patches.len() => (
                "no_match",
                "目标内容与当前文件不匹配",
                "先用 file_read 重新读取文件（内容可能已变化），再以文件里的原文作为 old_string 重新生成修改。",
            ),
            (0, ambiguous) if matched == 0 && ambiguous == patches.len() => (
                "ambiguous_match",
                "目标内容在当前文件中不是唯一匹配",
                "给 old_string 增加前后文，使它在文件中唯一，然后重新调用。",
            ),
            _ => (
                "not_applicable",
                "patch 与当前文件内容不匹配",
                "先用 file_read 重新读取文件，再逐个检查 old_string 是否存在且唯一；整批修改不会部分生效。",
            ),
        };
        tracing::warn!(
            tool = "file_patch",
            path = %path.display(),
            total = patches.len(),
            matched,
            kind,
            errors = ?errors,
            "file_patch batch is not applicable"
        );
        return ToolFailure::new("file_patch", kind, error)
            .instruction(instruction)
            .with("access_mode", BuiltinToolAccessMode::ExplicitWrite.as_str())
            .with("applied", 0)
            .with("total", patches.len())
            .with("errors", errors)
            .into_payload();
    }

    if let Err(error) = write_atomic_preserving_target(&path, result.as_bytes()) {
        return filesystem_failure("file_patch", "写回文件", &path, &error).into_payload();
    }

    serde_json::json!({
        "tool": "file_patch",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "applied": matched,
        "total": patches.len(),
        "summary": format!("已应用 {}/{} 个 patch 到 {}", matched, patches.len(), path.display())
    })
    .to_string()
}

/// `patches` 数组或单组 `old_string`/`new_string`，二选一。
fn parse_patches(
    request: &serde_json::Map<String, Value>,
) -> Result<Vec<(String, String)>, String> {
    let invalid = |message: String| Err(invalid_input("file_patch", message));
    if request.contains_key("patches")
        && (request.contains_key("old_string") || request.contains_key("new_string"))
    {
        return invalid("patches 与 old_string/new_string 不能同时提供".to_string());
    }
    match request.get("patches") {
        Some(Value::Array(items)) if items.is_empty() => invalid("patches 不能为空".to_string()),
        Some(Value::Array(items)) => {
            let mut patches = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let Some(old) = item.get("old_string").and_then(Value::as_str) else {
                    return invalid(format!("patches[{index}] 缺少 old_string 字段"));
                };
                if old.is_empty() {
                    return invalid(format!("patches[{index}].old_string 不能为空"));
                }
                let Some(new) = item.get("new_string").and_then(Value::as_str) else {
                    return invalid(format!("patches[{index}] 缺少 new_string 字段"));
                };
                patches.push((old.to_string(), new.to_string()));
            }
            Ok(patches)
        }
        Some(_) => invalid("patches 必须为数组".to_string()),
        None => {
            let (Some(old), Some(new)) = (
                field_string(request, "old_string"),
                field_string(request, "new_string"),
            ) else {
                return invalid("缺少 patches 数组或 old_string/new_string 字段".to_string());
            };
            if old.is_empty() {
                return invalid("old_string 不能为空".to_string());
            }
            Ok(vec![(old, new)])
        }
    }
}

pub(super) fn execute_file_mkdir(input: &str, context: &ToolExecutionContext) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input("file_mkdir", "输入必须为 JSON 对象，包含 path 字段");
    };
    let path_input = match non_empty_path_field(&request, "path", "file_mkdir") {
        Ok(value) => value,
        Err(failure) => return failure,
    };
    let path = match resolve_path_with_context(&path_input, context) {
        Ok(path) => path,
        Err(error) => return path_resolution_failure("file_mkdir", &path_input, &error),
    };

    if path.exists() {
        if path.is_dir() {
            return serde_json::json!({
                "tool": "file_mkdir",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
                "path": path.display().to_string(),
                "already_existed": true,
                "summary": format!("目录已存在: {}", path.display())
            })
            .to_string();
        }
        return ToolFailure::new("file_mkdir", "already_exists", "目标已存在，且不是目录")
            .instruction("换一个目录路径，或先确认该路径上的文件是否可以处理。")
            .into_payload();
    }

    if let Err(error) = fs::create_dir_all(&path) {
        return filesystem_failure("file_mkdir", "创建目录", &path, &error).into_payload();
    }

    serde_json::json!({
        "tool": "file_mkdir",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "already_existed": false,
        "summary": format!("已创建目录 {}", path.display())
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// diff_preview
// ══════════════════════════════════════════════════════════════════════════════

pub(super) fn execute_diff_preview(input: &str, context: &ToolExecutionContext) -> String {
    let Some(object) = parse_json_object(input) else {
        return invalid_input("diff_preview", "输入必须为 JSON 对象");
    };
    let before_path = field_string(&object, "before_path");
    let after_path = field_string(&object, "after_path");
    let before = field_string(&object, "before");
    let after = field_string(&object, "after");
    if before_path.is_none() && after_path.is_none() && before.is_none() && after.is_none() {
        return invalid_input("diff_preview", "差异预览需要 before/after 文本或路径");
    }
    let before_label = field_string(&object, "before_label")
        .unwrap_or_else(|| before_path.clone().unwrap_or_else(|| "before".to_string()));
    let after_label = field_string(&object, "after_label")
        .unwrap_or_else(|| after_path.clone().unwrap_or_else(|| "after".to_string()));

    let before_text = match read_diff_source(before_path, before, context) {
        Ok(text) => text,
        Err(failure) => return failure,
    };
    let after_text = match read_diff_source(after_path, after, context) {
        Ok(text) => text,
        Err(failure) => return failure,
    };

    let diff = build_diff_preview(&before_text, &after_text);
    serde_json::json!({
        "tool": "diff_preview",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "before_label": before_label,
        "after_label": after_label,
        "before_lines": before_text.lines().count(),
        "after_lines": after_text.lines().count(),
        "changed": diff.changed,
        "common_prefix_lines": diff.common_prefix_lines,
        "common_suffix_lines": diff.common_suffix_lines,
        "changed_before_lines": diff.changed_before_lines,
        "changed_after_lines": diff.changed_after_lines,
        "preview": diff.preview,
        "summary": if diff.changed {
            format!("生成差异预览: {} -> {}", before_label, after_label)
        } else {
            format!("{} 与 {} 没有差异", before_label, after_label)
        }
    })
    .to_string()
}

/// 行内文本优先；没有行内文本时读取路径；两者都没有按空文本处理（新增/删除整份内容的预览）。
fn read_diff_source(
    path: Option<String>,
    inline: Option<String>,
    context: &ToolExecutionContext,
) -> Result<String, String> {
    if let Some(inline) = inline
        && !inline.is_empty()
    {
        return Ok(inline);
    }
    let Some(path) = path else {
        return Ok(String::new());
    };
    let resolved = resolve_path_with_context(&path, context)
        .map_err(|error| path_resolution_failure("diff_preview", &path, &error))?;
    fs::read_to_string(&resolved).map_err(|error| {
        filesystem_failure("diff_preview", "读取差异预览源文件", &resolved, &error).into_payload()
    })
}

struct DiffPreviewResult {
    changed: bool,
    common_prefix_lines: usize,
    common_suffix_lines: usize,
    changed_before_lines: usize,
    changed_after_lines: usize,
    preview: String,
}

fn build_diff_preview(before: &str, after: &str) -> DiffPreviewResult {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let common_prefix_lines = common_prefix_len(&before_lines, &after_lines);
    let common_suffix_lines = common_suffix_len(&before_lines, &after_lines, common_prefix_lines);

    let before_change_end = before_lines.len().saturating_sub(common_suffix_lines);
    let after_change_end = after_lines.len().saturating_sub(common_suffix_lines);
    let changed_before = &before_lines[common_prefix_lines..before_change_end];
    let changed_after = &after_lines[common_prefix_lines..after_change_end];
    let changed = before_lines != after_lines;

    let mut preview_lines = Vec::new();
    if changed {
        preview_lines.push(format!(
            "@@ -{},{} +{},{} @@",
            common_prefix_lines + 1,
            changed_before.len(),
            common_prefix_lines + 1,
            changed_after.len()
        ));
        for line in changed_before {
            preview_lines.push(format!("-{}", line));
        }
        for line in changed_after {
            preview_lines.push(format!("+{}", line));
        }
    } else {
        preview_lines.push("no changes".to_string());
    }

    DiffPreviewResult {
        changed,
        common_prefix_lines,
        common_suffix_lines,
        changed_before_lines: changed_before.len(),
        changed_after_lines: changed_after.len(),
        preview: preview_lines.join("\n"),
    }
}

fn common_prefix_len(before: &[&str], after: &[&str]) -> usize {
    before
        .iter()
        .zip(after.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

fn common_suffix_len(before: &[&str], after: &[&str], prefix_len: usize) -> usize {
    let mut count = 0usize;
    let mut before_index = before.len();
    let mut after_index = after.len();
    while before_index > prefix_len && after_index > prefix_len {
        if before[before_index - 1] != after[after_index - 1] {
            break;
        }
        before_index -= 1;
        after_index -= 1;
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_preview_rejects_binary_and_non_utf8_content() {
        assert!(decode_text_preview(b"plain text".to_vec(), false).is_ok());
        assert!(decode_text_preview(vec![0x50, 0x4b, 0x00, 0x04], false).is_err());
        assert!(decode_text_preview(vec![0xff, 0xfe, 0x41], false).is_err());
    }

    #[test]
    fn truncated_preview_drops_a_split_multibyte_character_but_not_invalid_bytes() {
        let text = "中文".as_bytes();
        // 截在“文”的中间：被切开的字符丢弃，不算二进制。
        let preview = decode_text_preview(text[..4].to_vec(), true).expect("split character");
        assert_eq!(preview, "中");
        // 同样的字节没有被截断时说明文件本身不是合法 UTF-8。
        assert!(decode_text_preview(text[..4].to_vec(), false).is_err());
        // 截断位置之前就出现的非法字节仍然是二进制。
        assert!(decode_text_preview(vec![0x41, 0xff, 0x42], true).is_err());
    }
}
