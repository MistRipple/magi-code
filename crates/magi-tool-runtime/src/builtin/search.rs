use super::{
    failure::invalid_input,
    field_bool, field_string, parse_json_object,
    read_support::{
        DEFAULT_MAX_BYTES, MAX_BYTES, MAX_FILE_BYTES, ReadOperation, WorkspaceFiles, bounded_usize,
        decode_text,
    },
    required_string_field,
};
use crate::{BuiltinToolAccessMode, ToolExecutionContext, ToolRuntimeResources};
use magi_core::ToolFailure;
use serde_json::{Value, json};
use std::{fs, path::Path};

pub(super) fn execute_search_text(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    search(input, context, resources).unwrap_or_else(|failure| failure)
}

fn search(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> Result<String, String> {
    let request = parse_json_object(input);
    let query = required_string_field(request.as_ref(), "query", "search_text", "缺少搜索关键词")?;
    if query.chars().count() > 4096 {
        return Err(invalid_input("search_text", "query 最多 4096 个字符"));
    }
    let request = request.unwrap();
    let op = ReadOperation::new("search_text", &request, context, resources)?;
    let root = op.resolve(
        &field_string(&request, "root").unwrap_or_else(|| ".".into()),
        context,
    )?;
    let root = crate::canonicalize_tool_permission_path(&root);
    let metadata = fs::metadata(&root).map_err(|e| op.io_failure(&root, &e))?;
    if metadata.is_dir() {
        fs::read_dir(&root).map_err(|e| op.io_failure(&root, &e))?;
    } else if !metadata.is_file() {
        return Err(invalid_input("search_text", "搜索根必须是目录或普通文件"));
    }
    let limit = bounded_usize(&request, "limit", 20, 500, "search_text")?;
    let max_bytes = bounded_usize(
        &request,
        "max_bytes",
        DEFAULT_MAX_BYTES,
        MAX_BYTES,
        "search_text",
    )?;
    let case_sensitive = field_bool(&request, "case_sensitive").unwrap_or(true);
    let include_hidden = field_bool(&request, "include_hidden").unwrap_or(false);
    let query_mode = field_string(&request, "query_mode").unwrap_or_else(|| "literal".into());
    let target = field_string(&request, "target").unwrap_or_else(|| "content".into());
    if !matches!(target.as_str(), "content" | "path") {
        return Err(invalid_input(
            "search_text",
            "target 只支持 content 或 path",
        ));
    }
    let output_mode = field_string(&request, "output_mode")
        .unwrap_or_else(|| if target == "path" { "files" } else { "matches" }.into());
    if !matches!(output_mode.as_str(), "matches" | "files")
        || (target == "path" && output_mode != "files")
    {
        return Err(invalid_input(
            "search_text",
            "output_mode 只支持 matches 或 files；target=path 时使用 files",
        ));
    }
    // 字面量只做转义，所有匹配均使用同一个 Rust regex 引擎，位置来自原始文本。
    let pattern = match query_mode.as_str() {
        "literal" => regex::escape(&query),
        "regex" => query.clone(),
        _ => {
            return Err(invalid_input(
                "search_text",
                "query_mode 只支持 literal 或 regex",
            ));
        }
    };
    let matcher = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|_| {
            ToolFailure::new("search_text", "invalid_regex", "正则表达式无效")
                .instruction("修正 query 的正则语法，或把 query_mode 改成 literal 做字面量搜索。")
                .into_payload()
        })?;
    op.check()?;
    let mut items = Vec::<Value>::new();
    let mut output_bytes = 0;
    let mut stop_reason = None;
    let mut scanned_files = 0;
    let mut unreadable = 0;
    let mut too_large = 0;
    let mut non_text = 0;
    let mut walker = WorkspaceFiles::new(op.clone(), &root, context, include_hidden)?;
    while let Some(path) = walker.next_file()? {
        let path = path.as_path();
        op.authorize(path)?;
        scanned_files += 1;
        let path_text = path.to_string_lossy().to_string();
        let relative_path = path
            .strip_prefix(&root)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(path.file_name().unwrap()));
        if target == "path" {
            if matcher.is_match(&relative_path.to_string_lossy().replace('\\', "/")) {
                stop_reason = push_item(
                    &mut items,
                    json!(path_text),
                    &mut output_bytes,
                    limit,
                    max_bytes,
                );
            }
        } else {
            let metadata = match fs::metadata(path) {
                Ok(value) => value,
                Err(_) => {
                    unreadable += 1;
                    continue;
                }
            };
            if metadata.len() > MAX_FILE_BYTES as u64 {
                too_large += 1;
                continue;
            }
            let mut file = match op.open(path) {
                Ok(file) => file,
                Err(failure) if path == root => return Err(failure),
                Err(_) => {
                    op.check()?;
                    unreadable += 1;
                    continue;
                }
            };
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 16 * 1024];
            let mut failed = false;
            while bytes.len() <= MAX_FILE_BYTES {
                let cap = buffer.len().min(MAX_FILE_BYTES + 1 - bytes.len());
                match op.read_chunk(&mut file, &mut buffer[..cap], path) {
                    Ok(0) => break,
                    Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                    Err(failure) if path == root => return Err(failure),
                    Err(_) => {
                        op.check()?;
                        unreadable += 1;
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                continue;
            }
            if bytes.len() > MAX_FILE_BYTES {
                too_large += 1;
                continue;
            }
            let content = match decode_text(bytes, false) {
                Ok(content) => content,
                Err(()) => {
                    non_text += 1;
                    continue;
                }
            };
            for (index, line) in content.lines().enumerate() {
                op.check()?;
                let Some(matched) = matcher.find(line) else {
                    continue;
                };
                let item = if output_mode == "files" {
                    json!(path_text)
                } else {
                    let (excerpt, start) = excerpt(line, matched.start());
                    json!({
                        "path": path_text,
                        "line": index + 1,
                        "column": line[..matched.start()].chars().count() + 1,
                        "excerpt": excerpt,
                        "excerpt_start_column": line[..start].chars().count() + 1,
                        "excerpt_truncated": excerpt.len() < line.len(),
                    })
                };
                stop_reason = push_item(&mut items, item, &mut output_bytes, limit, max_bytes);
                if stop_reason.is_some() || output_mode == "files" {
                    break;
                }
            }
        }
        if stop_reason.is_some() {
            break;
        }
    }
    op.check()?;
    unreadable += walker.unreadable;
    let skip_note = if unreadable + too_large + non_text == 0 {
        String::new()
    } else {
        format!(
            "（另有 {} 个文件或目录未搜索：{unreadable} 个无法读取、{too_large} 个超过 2MB、{non_text} 个不是文本）",
            unreadable + too_large + non_text
        )
    };
    let mut result = json!({
        "tool": "search_text", "status": "succeeded", "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "root": root.display().to_string(), "query": query, "query_mode": query_mode, "target": target, "output_mode": output_mode,
        "case_sensitive": case_sensitive, "limit": limit, "max_bytes": max_bytes,
        "scanned_files": scanned_files, "truncated": stop_reason.is_some(), "stop_reason": stop_reason,
        "skipped": { "unreadable": unreadable, "too_large": too_large, "non_text": non_text },
        "summary": format!("在 {} 中扫描了 {scanned_files} 个文件，返回 {} 项{skip_note}", root.display(), items.len())
    });
    if output_mode == "files" {
        result["returned_files"] = items.len().into();
        result["files"] = items.into();
    } else {
        result["returned_matches"] = items.len().into();
        result["matches"] = items.into();
    }
    Ok(result.to_string())
}

fn push_item(
    items: &mut Vec<Value>,
    item: Value,
    bytes: &mut usize,
    limit: usize,
    max_bytes: usize,
) -> Option<&'static str> {
    if items.len() == limit {
        return Some("limit");
    }
    let size = item.to_string().len() + 1;
    if *bytes + size > max_bytes {
        return Some("max_bytes");
    }
    *bytes += size;
    items.push(item);
    None
}

fn excerpt(line: &str, matched: usize) -> (&str, usize) {
    let mut start = matched.saturating_sub(256);
    while !line.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + 2048).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    (&line[start..end], start)
}
