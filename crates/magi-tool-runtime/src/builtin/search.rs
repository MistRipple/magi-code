use crate::{BuiltinToolAccessMode, ToolExecutionContext};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{
    failure::{filesystem_failure, invalid_input, path_resolution_failure},
    field_bool, field_string, field_usize, parse_json_object, required_string_field,
    resolve_path_with_context,
};
use magi_core::ToolFailure;

pub(super) fn execute_search_text(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let query =
        match required_string_field(request.as_ref(), "query", "search_text", "缺少搜索关键词")
        {
            Ok(value) => value,
            Err(error) => return error,
        };
    let root_input = request
        .as_ref()
        .and_then(|object| field_string(object, "root"))
        .unwrap_or_else(|| ".".to_string());
    let root = match resolve_path_with_context(&root_input, context) {
        Ok(path) => path,
        Err(error) => {
            return path_resolution_failure("search_text", &root_input, &error);
        }
    };
    let limit = request
        .as_ref()
        .and_then(|object| field_usize(object, "limit"))
        .unwrap_or(20)
        .clamp(1, 500);
    let case_sensitive = request
        .as_ref()
        .and_then(|object| field_bool(object, "case_sensitive"))
        .unwrap_or(true);
    let include_hidden = request
        .as_ref()
        .and_then(|object| field_bool(object, "include_hidden"))
        .unwrap_or(false);
    let query_mode = request
        .as_ref()
        .and_then(|object| field_string(object, "query_mode"))
        .unwrap_or_else(|| "literal".to_string());
    let matcher = match SearchTextMatcher::new(&query, &query_mode, case_sensitive) {
        Ok(matcher) => matcher,
        Err(SearchTextMatcherError::UnsupportedMode) => {
            return invalid_input("search_text", "query_mode 只支持 literal 或 regex");
        }
        Err(SearchTextMatcherError::InvalidRegex) => {
            return ToolFailure::new("search_text", "invalid_regex", "正则表达式无效")
                .instruction("修正 query 的正则语法，或把 query_mode 改成 literal 做字面量搜索。")
                .into_payload();
        }
    };

    let outcome = match search_text_matches(&root, &matcher, include_hidden, limit) {
        Ok(outcome) => outcome,
        Err(error) => return search_text_root_failure(error),
    };

    serde_json::json!({
        "tool": "search_text",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "root": root.display().to_string(),
        "query": query,
        "query_mode": query_mode,
        "case_sensitive": case_sensitive,
        "limit": limit,
        "scanned_files": outcome.scanned_files,
        "returned_matches": outcome.matches.len(),
        "truncated": outcome.truncated,
        "skipped": {
            "unreadable": outcome.skipped_unreadable,
            "too_large": outcome.skipped_too_large,
            "non_text": outcome.skipped_non_text,
        },
        "matches": outcome.matches,
        "summary": format!(
            "在 {} 中扫描了 {} 个文件，找到 {} 个匹配{}",
            root.display(),
            outcome.scanned_files,
            outcome.matches.len(),
            search_text_skip_note(&outcome),
        )
    })
    .to_string()
}

/// 有文件没能搜索时在摘要里点明，避免把「没搜到」读成「没有」。
pub(super) fn search_text_skip_note(outcome: &SearchTextOutcome) -> String {
    let skipped = outcome.skipped_unreadable + outcome.skipped_too_large + outcome.skipped_non_text;
    if skipped == 0 {
        return String::new();
    }
    format!(
        "（另有 {skipped} 个文件或目录未搜索：{} 个无法读取、{} 个超过 2MB、{} 个不是文本）",
        outcome.skipped_unreadable, outcome.skipped_too_large, outcome.skipped_non_text
    )
}

/// 搜索根本身读不了：由统一的文件系统失败分类给出「不存在」「没有权限」等类别，
/// 模型才知道该改路径还是换方式。
pub(super) fn search_text_root_failure(error: SearchTextFilesystemError) -> String {
    filesystem_failure("search_text", error.action, &error.path, &error.source).into_payload()
}

pub(super) fn should_skip_directory(path: &Path, include_hidden: bool) -> bool {
    let name = match path.file_name().and_then(|value| value.to_str()) {
        Some(value) => value,
        None => return false,
    };
    if !include_hidden && name.starts_with('.') {
        return true;
    }
    matches!(name, "target" | "node_modules" | "dist" | "coverage")
}

pub(super) struct SearchTextFilesystemError {
    action: &'static str,
    path: PathBuf,
    source: std::io::Error,
}

pub(super) enum SearchTextMatcher {
    Literal { query: String, case_sensitive: bool },
    Regex(regex::Regex),
}

pub(super) enum SearchTextMatcherError {
    UnsupportedMode,
    InvalidRegex,
}

impl SearchTextMatcher {
    fn new(
        query: &str,
        query_mode: &str,
        case_sensitive: bool,
    ) -> Result<Self, SearchTextMatcherError> {
        match query_mode {
            "literal" => Ok(Self::Literal {
                query: if case_sensitive {
                    query.to_string()
                } else {
                    query.to_lowercase()
                },
                case_sensitive,
            }),
            "regex" => regex::RegexBuilder::new(query)
                .case_insensitive(!case_sensitive)
                .build()
                .map(Self::Regex)
                .map_err(|_| SearchTextMatcherError::InvalidRegex),
            _ => Err(SearchTextMatcherError::UnsupportedMode),
        }
    }

    fn find(&self, line: &str) -> Option<usize> {
        match self {
            Self::Literal {
                query,
                case_sensitive,
            } => {
                if *case_sensitive {
                    line.find(query)
                } else {
                    line.to_lowercase().find(query)
                }
            }
            Self::Regex(regex) => regex.find(line).map(|matched| matched.start()),
        }
    }
}

/// 一次文本搜索的结果：命中之外还要说明「哪些地方没搜到」，模型才不会把没搜到当成没有。
pub(super) struct SearchTextOutcome {
    matches: Vec<Value>,
    scanned_files: usize,
    truncated: bool,
    /// 读不了的子目录或文件（权限、搜索期间被删除、悬空链接……）。
    skipped_unreadable: usize,
    /// 超过体积上限的文件。
    skipped_too_large: usize,
    /// 不是 UTF-8 文本的文件（二进制等）。
    skipped_non_text: usize,
}

pub(super) const SEARCH_TEXT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 遍历搜索根目录。只有**搜索根本身**读不了才算工具失败；
/// 子目录或文件读不了只跳过并计数——一个无权限的目录不应让整次搜索作废。
pub(super) fn search_text_matches(
    root: &Path,
    matcher: &SearchTextMatcher,
    include_hidden: bool,
    limit: usize,
) -> Result<SearchTextOutcome, SearchTextFilesystemError> {
    let mut stack = vec![root.to_path_buf()];
    let mut outcome = SearchTextOutcome {
        matches: Vec::new(),
        scanned_files: 0,
        truncated: false,
        skipped_unreadable: 0,
        skipped_too_large: 0,
        skipped_non_text: 0,
    };

    while let Some(path) = stack.pop() {
        if outcome.matches.len() >= limit {
            break;
        }
        let is_root = path == root;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if is_root => {
                return Err(SearchTextFilesystemError {
                    action: "读取搜索路径信息",
                    path,
                    source: error,
                });
            }
            Err(_) => {
                outcome.skipped_unreadable += 1;
                continue;
            }
        };
        if metadata.is_dir() {
            let mut entries = match fs::read_dir(&path) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .collect::<Vec<_>>(),
                Err(error) if is_root => {
                    return Err(SearchTextFilesystemError {
                        action: "读取搜索目录",
                        path,
                        source: error,
                    });
                }
                Err(_) => {
                    outcome.skipped_unreadable += 1;
                    continue;
                }
            };
            entries.sort();
            for entry in entries.into_iter().rev() {
                if should_skip_directory(&entry, include_hidden) {
                    continue;
                }
                stack.push(entry);
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        outcome.scanned_files += 1;
        if metadata.len() > SEARCH_TEXT_MAX_FILE_BYTES {
            outcome.skipped_too_large += 1;
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                outcome.skipped_non_text += 1;
                continue;
            }
            Err(_) => {
                outcome.skipped_unreadable += 1;
                continue;
            }
        };

        for (line_number, line) in content.lines().enumerate() {
            if outcome.matches.len() >= limit {
                break;
            }
            if let Some(column) = matcher.find(line) {
                outcome.matches.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "line": line_number + 1,
                    "column": column + 1,
                    "excerpt": line.trim().to_string(),
                }));
            }
        }
    }

    outcome.truncated = outcome.matches.len() >= limit;
    Ok(outcome)
}
