use crate::{
    BuiltinTool, BuiltinToolAccessMode, BuiltinToolName, BuiltinToolSpec,
};
use magi_core::{ApprovalRequirement, ExecutionResultStatus, RiskLevel};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub(crate) struct NormalizedBuiltinTool {
    name: BuiltinToolName,
    risk_level: RiskLevel,
    approval_requirement: ApprovalRequirement,
}

impl NormalizedBuiltinTool {
    pub(crate) fn new(
        name: BuiltinToolName,
        risk_level: RiskLevel,
        approval_requirement: ApprovalRequirement,
    ) -> Self {
        Self {
            name,
            risk_level,
            approval_requirement,
        }
    }
}

impl BuiltinTool for NormalizedBuiltinTool {
    fn name(&self) -> &'static str {
        self.name.as_str()
    }

    fn execute(&self, input: &str) -> String {
        match self.name {
            BuiltinToolName::FileRead => execute_file_read(input),
            BuiltinToolName::FileWrite => execute_file_write(input),
            BuiltinToolName::FilePatch => execute_file_patch(input),
            BuiltinToolName::FileRemove => execute_file_remove(input),
            BuiltinToolName::FileMkdir => execute_file_mkdir(input),
            BuiltinToolName::FileCopy => execute_file_copy(input),
            BuiltinToolName::FileMove => execute_file_move(input),
            BuiltinToolName::SearchText => execute_search_text(input),
            BuiltinToolName::SearchSemantic => execute_search_semantic(input),
            BuiltinToolName::ShellExec => execute_shell_exec(input),
            BuiltinToolName::ProcessInspect => execute_process_inspect(input),
            BuiltinToolName::DiffPreview => execute_diff_preview(input),
            BuiltinToolName::WebSearch => execute_web_search(input),
            BuiltinToolName::WebFetch => execute_web_fetch(input),
            BuiltinToolName::MermaidDiagram => execute_mermaid_diagram(input),
            BuiltinToolName::KnowledgeQuery => execute_knowledge_query(input),
            BuiltinToolName::WorkerSendMessage => execute_orchestration_stub(self.name, input),
            BuiltinToolName::TaskSplit => execute_orchestration_stub(self.name, input),
            BuiltinToolName::TaskList => execute_orchestration_stub(self.name, input),
            BuiltinToolName::TaskUpdate => execute_orchestration_stub(self.name, input),
            BuiltinToolName::TaskClaimNext => execute_orchestration_stub(self.name, input),
            BuiltinToolName::ContextCompact => execute_orchestration_stub(self.name, input),
            BuiltinToolName::SkillApply => execute_skill_apply_stub(input),
        }
    }

    fn spec(&self) -> BuiltinToolSpec {
        BuiltinToolSpec {
            name: self.name.as_str().to_string(),
            risk_level: self.risk_level,
            approval_requirement: self.approval_requirement,
        }
    }
}

pub(crate) fn infer_execution_status(payload: &str) -> ExecutionResultStatus {
    let parsed = serde_json::from_str::<Value>(payload).ok();
    let status = parsed
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .map(|status| status.to_ascii_lowercase());

    match status.as_deref() {
        Some("succeeded") | Some("success") | Some("ok") => ExecutionResultStatus::Succeeded,
        Some("failed") | Some("error") => ExecutionResultStatus::Failed,
        Some("rejected") => ExecutionResultStatus::Rejected,
        Some("needs_approval") | Some("needsapproval") => ExecutionResultStatus::NeedsApproval,
        Some("cancelled") | Some("canceled") => ExecutionResultStatus::Cancelled,
        _ => parsed
            .as_ref()
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            .map(|ok| {
                if ok {
                    ExecutionResultStatus::Succeeded
                } else {
                    ExecutionResultStatus::Failed
                }
            })
            .unwrap_or(ExecutionResultStatus::Succeeded),
    }
}

pub(crate) fn parse_json_object(input: &str) -> Option<serde_json::Map<String, Value>> {
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
}

pub(crate) fn field_string(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(|value| value.to_string())
    })
}

fn field_usize(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value.as_u64().map(|value| value as usize).or_else(|| {
                value
                    .as_str()
                    .and_then(|value| value.parse::<usize>().ok())
            })
        })
    })
}

fn field_bool(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.as_str().and_then(|value| value.parse::<bool>().ok()))
        })
    })
}

pub(crate) fn resolve_path(input: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(input);
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("无法解析当前目录: {error}"))
    }
}

fn execute_file_read(input: &str) -> String {
    let request = parse_json_object(input);
    let path_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["path", "file_path"]))
        .unwrap_or_else(|| input.trim().to_string());
    if path_input.is_empty() {
        return builtin_error("file_read", "缺少文件路径");
    }

    let max_bytes = request
        .as_ref()
        .and_then(|object| field_usize(object, &["max_bytes", "preview_bytes"]))
        .unwrap_or(64 * 1024)
        .max(1);

    let path = match resolve_path(&path_input) {
        Ok(path) => path,
        Err(error) => return builtin_error("file_read", error),
    };

    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return builtin_error("file_read", format!("读取元数据失败: {error}")),
    };

    if metadata.is_dir() {
        let mut entries = match fs::read_dir(&path) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            Err(error) => return builtin_error("file_read", format!("读取目录失败: {error}")),
        };
        entries.sort();
        return serde_json::json!({
            "tool": "file_read",
            "status": "succeeded",
            "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
            "mode": "directory",
            "path": path.display().to_string(),
            "entries": entries,
            "entry_count": entries.len(),
            "summary": format!("目录 {} 包含 {} 项", path.display(), entries.len())
        })
        .to_string();
    }

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => return builtin_error("file_read", format!("读取文件失败: {error}")),
    };
    let truncated = bytes.len() > max_bytes;
    let preview_bytes = if truncated {
        &bytes[..max_bytes]
    } else {
        &bytes[..]
    };
    let content = String::from_utf8_lossy(preview_bytes).to_string();

    serde_json::json!({
        "tool": "file_read",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "mode": "file",
        "path": path.display().to_string(),
        "bytes_read": bytes.len(),
        "preview_bytes": preview_bytes.len(),
        "truncated": truncated,
        "encoding": "utf-8-lossy",
        "content": content,
        "summary": if truncated {
            format!("已预览文件 {} 的前 {} 字节", path.display(), max_bytes)
        } else {
            format!("已读取文件 {}", path.display())
        }
    })
    .to_string()
}

fn execute_search_text(input: &str) -> String {
    let request = parse_json_object(input);
    let query = request
        .as_ref()
        .and_then(|object| field_string(object, &["query", "text", "needle"]))
        .unwrap_or_else(|| input.trim().to_string());
    if query.is_empty() {
        return builtin_error("search_text", "缺少搜索关键词");
    }
    let root_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["root", "path", "workspace"]))
        .unwrap_or_else(|| ".".to_string());
    let root = match resolve_path(&root_input) {
        Ok(path) => path,
        Err(error) => return builtin_error("search_text", error),
    };
    let limit = request
        .as_ref()
        .and_then(|object| field_usize(object, &["limit", "max_results"]))
        .unwrap_or(20)
        .clamp(1, 500);
    let case_sensitive = request
        .as_ref()
        .and_then(|object| field_bool(object, &["case_sensitive"]))
        .unwrap_or(true);
    let include_hidden = request
        .as_ref()
        .and_then(|object| field_bool(object, &["include_hidden"]))
        .unwrap_or(false);

    let (matches, scanned_files, truncated) = match search_text_matches(
        &root,
        &query,
        case_sensitive,
        include_hidden,
        limit,
    ) {
        Ok(result) => result,
        Err(error) => return builtin_error("search_text", error),
    };

    serde_json::json!({
        "tool": "search_text",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "root": root.display().to_string(),
        "query": query,
        "case_sensitive": case_sensitive,
        "limit": limit,
        "scanned_files": scanned_files,
        "returned_matches": matches.len(),
        "truncated": truncated,
        "matches": matches,
        "summary": format!(
            "在 {} 中扫描了 {} 个文件，找到 {} 个匹配",
            root.display(),
            scanned_files,
            matches.len()
        )
    })
    .to_string()
}

fn execute_shell_exec(input: &str) -> String {
    let request = parse_json_object(input);
    let command = request
        .as_ref()
        .and_then(|object| field_string(object, &["command", "script", "line"]))
        .unwrap_or_else(|| input.trim().to_string());
    if command.is_empty() {
        return builtin_error("shell_exec", "缺少 shell 命令");
    }
    let access_mode = request
        .as_ref()
        .and_then(|object| {
            field_string(object, &["access_mode", "write_mode", "intent"])
                .and_then(|value| BuiltinToolAccessMode::from_str(&value))
        })
        .unwrap_or(BuiltinToolAccessMode::MaybeWrite);
    let cwd_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["cwd", "working_directory", "workdir"]));
    let cwd = match cwd_input {
        Some(value) => match resolve_path(&value) {
            Ok(path) => path,
            Err(error) => return builtin_error("shell_exec", error),
        },
        None => match std::env::current_dir() {
            Ok(path) => path,
            Err(error) => return builtin_error("shell_exec", format!("无法解析当前目录: {error}")),
        },
    };
    let shell = request
        .as_ref()
        .and_then(|object| field_string(object, &["shell"]))
        .unwrap_or_else(default_shell_binary);

    let output = match Command::new(&shell)
        .arg(shell_arg())
        .arg(&command)
        .current_dir(&cwd)
        .output()
    {
        Ok(output) => output,
        Err(error) => return builtin_error("shell_exec", format!("命令执行失败: {error}")),
    };

    let status = if output.status.success() {
        "succeeded"
    } else {
        "failed"
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    serde_json::json!({
        "tool": "shell_exec",
        "status": status,
        "command": command,
        "cwd": cwd.display().to_string(),
        "access_mode": access_mode.as_str(),
        "exit_code": output.status.code(),
        "stdout": stdout,
        "stderr": stderr,
        "summary": if output.status.success() {
            format!("命令执行成功: {}", command)
        } else {
            format!("命令执行失败(退出码 {:?}): {}", output.status.code(), command)
        }
    })
    .to_string()
}

fn execute_process_inspect(input: &str) -> String {
    let request = parse_json_object(input);
    let raw_trimmed = input.trim();
    let query = request
        .as_ref()
        .and_then(|object| field_string(object, &["query", "name", "pattern"]))
        .or_else(|| {
            if request.is_none() && !raw_trimmed.is_empty() && raw_trimmed.parse::<u32>().is_err() {
                Some(raw_trimmed.to_string())
            } else {
                None
            }
        });
    let pid = request
        .as_ref()
        .and_then(|object| field_usize(object, &["pid", "process_id"]))
        .map(|pid| pid as u32)
        .or_else(|| {
            if query.is_some() {
                None
            } else {
                raw_trimmed.parse::<u32>().ok()
            }
        })
        .unwrap_or_else(std::process::id);
    let limit = request
        .as_ref()
        .and_then(|object| field_usize(object, &["limit", "max_results"]))
        .unwrap_or(20)
        .clamp(1, 100);

    let output = if query.is_some() {
        match Command::new("ps")
            .args(["-ax", "-o", "pid=,ppid=,state=,comm="])
            .output()
        {
            Ok(output) => output.stdout,
            Err(error) => return builtin_error("process_inspect", format!("进程检查失败: {error}")),
        }
    } else {
        match Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid=,ppid=,state=,comm="])
            .output()
        {
            Ok(output) => output.stdout,
            Err(error) => return builtin_error("process_inspect", format!("进程检查失败: {error}")),
        }
    };

    let raw_output = String::from_utf8_lossy(&output);
    let mut matches = Vec::new();
    let query_lower = query.as_ref().map(|value| value.to_lowercase());

    for line in raw_output.lines() {
        if let Some(query_lower) = &query_lower {
            if !line.to_lowercase().contains(query_lower) {
                continue;
            }
        }

        if let Some(record) = parse_ps_line(line) {
            matches.push(record);
        }

        if matches.len() >= limit {
            break;
        }
    }

    serde_json::json!({
        "tool": "process_inspect",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "mode": infer_process_mode(&query),
        "requested_pid": pid,
        "query": query,
        "limit": limit,
        "returned_matches": matches.len(),
        "matches": matches,
        "summary": if let Some(query) = query {
            format!("进程查询 {} 返回 {} 条记录", query, matches.len())
        } else {
            format!("进程 {} 返回 {} 条记录", pid, matches.len())
        }
    })
    .to_string()
}

fn execute_diff_preview(input: &str) -> String {
    let request = parse_json_object(input);
    let parsed = if let Some(object) = request {
        let before_path = field_string(&object, &["before_path", "left_path"]);
        let after_path = field_string(&object, &["after_path", "right_path"]);
        let before = field_string(&object, &["before", "left"]);
        let after = field_string(&object, &["after", "right"]);
        let before_label = field_string(&object, &["before_label", "left_label"])
            .unwrap_or_else(|| before_path.clone().unwrap_or_else(|| "before".to_string()));
        let after_label = field_string(&object, &["after_label", "right_label"])
            .unwrap_or_else(|| after_path.clone().unwrap_or_else(|| "after".to_string()));
        (before_path, after_path, before, after, before_label, after_label)
    } else if let Some((before, after)) = input.split_once("\n---\n") {
        (
            None,
            None,
            Some(before.to_string()),
            Some(after.to_string()),
            "before".to_string(),
            "after".to_string(),
        )
    } else if let Some((before, after)) = input.split_once("|||") {
        (
            None,
            None,
            Some(before.to_string()),
            Some(after.to_string()),
            "before".to_string(),
            "after".to_string(),
        )
    } else {
        (
            None,
            None,
            Some(String::new()),
            Some(input.to_string()),
            "before".to_string(),
            "after".to_string(),
        )
    };

    let (before_path, after_path, before, after, before_label, after_label) = parsed;
    let before_text = match read_diff_source(before_path, before) {
        Ok(text) => text,
        Err(error) => return builtin_error("diff_preview", error),
    };
    let after_text = match read_diff_source(after_path, after) {
        Ok(text) => text,
        Err(error) => return builtin_error("diff_preview", error),
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

fn builtin_error(tool: &str, message: impl Into<String>) -> String {
    serde_json::json!({
        "tool": tool,
        "status": "failed",
        "error": message.into(),
    })
    .to_string()
}

fn default_shell_binary() -> String {
    if cfg!(windows) {
        "cmd".to_string()
    } else {
        "sh".to_string()
    }
}

fn shell_arg() -> &'static str {
    if cfg!(windows) {
        "/C"
    } else {
        "-lc"
    }
}

fn should_skip_directory(path: &Path, include_hidden: bool) -> bool {
    let name = match path.file_name().and_then(|value| value.to_str()) {
        Some(value) => value,
        None => return false,
    };
    if !include_hidden && name.starts_with('.') {
        return true;
    }
    matches!(name, "target" | "node_modules" | "dist" | "coverage")
}

fn search_text_matches(
    root: &Path,
    query: &str,
    case_sensitive: bool,
    include_hidden: bool,
    limit: usize,
) -> Result<(Vec<Value>, usize, bool), String> {
    let mut stack = vec![root.to_path_buf()];
    let mut matches = Vec::new();
    let mut scanned_files = 0usize;
    let normalized_query = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };

    while let Some(path) = stack.pop() {
        if matches.len() >= limit {
            break;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => return Err(format!("读取路径失败 {}: {error}", path.display())),
        };
        if metadata.is_dir() {
            let mut entries = match fs::read_dir(&path) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .collect::<Vec<_>>(),
                Err(error) => return Err(format!("读取目录失败 {}: {error}", path.display())),
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

        scanned_files += 1;
        if metadata.len() > 2 * 1024 * 1024 {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };

        for (line_number, line) in content.lines().enumerate() {
            if matches.len() >= limit {
                break;
            }
            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            if let Some(column) = haystack.find(&normalized_query) {
                matches.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "line": line_number + 1,
                    "column": column + 1,
                    "excerpt": line.trim().to_string(),
                }));
            }
        }
    }

    let truncated = matches.len() >= limit;
    Ok((matches, scanned_files, truncated))
}

fn parse_ps_line(line: &str) -> Option<Value> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let ppid = parts.next()?.parse::<u32>().ok()?;
    let state = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    Some(serde_json::json!({
        "pid": pid,
        "ppid": ppid,
        "state": state,
        "command": command,
    }))
}

fn read_diff_source(path: Option<String>, inline: Option<String>) -> Result<String, String> {
    if let Some(path) = path {
        let resolved = resolve_path(&path)?;
        return fs::read_to_string(&resolved)
            .map_err(|error| format!("读取 diff 源失败 {}: {error}", resolved.display()));
    }
    Ok(inline.unwrap_or_default())
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

fn infer_process_mode(query: &Option<String>) -> &'static str {
    if query.is_some() {
        "query"
    } else {
        "pid"
    }
}

fn execute_file_write(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("file.write", "输入必须为 JSON 对象，包含 path 和 content 字段"),
    };

    let path_input = match field_string(&request, &["path", "file_path"]) {
        Some(p) => p,
        None => return builtin_error("file.write", "缺少 path 字段"),
    };
    let content = match field_string(&request, &["content", "text", "data"]) {
        Some(c) => c,
        None => return builtin_error("file.write", "缺少 content 字段"),
    };

    let path = match resolve_path(&path_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.write", e),
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(true);
    let create_dirs = field_bool(&request, &["create_dirs", "mkdir"]).unwrap_or(true);

    if path.exists() && !overwrite {
        return builtin_error("file.write", format!("文件已存在且 overwrite=false: {}", path.display()));
    }

    if create_dirs {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return builtin_error("file.write", format!("创建父目录失败: {e}"));
                }
            }
        }
    }

    let bytes = content.len();
    if let Err(e) = fs::write(&path, &content) {
        return builtin_error("file.write", format!("写入文件失败: {e}"));
    }

    serde_json::json!({
        "tool": "file.write",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "bytes_written": bytes,
        "created": !path.exists() || overwrite,
        "summary": format!("已写入 {} ({} 字节)", path.display(), bytes)
    })
    .to_string()
}

fn execute_file_patch(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("file.patch", "输入必须为 JSON 对象"),
    };

    let path_input = match field_string(&request, &["path", "file_path"]) {
        Some(p) => p,
        None => return builtin_error("file.patch", "缺少 path 字段"),
    };
    let path = match resolve_path(&path_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.patch", e),
    };

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return builtin_error("file.patch", format!("读取文件失败: {e}")),
    };

    let patches: Vec<(String, String)> = if let Some(arr) = request.get("patches").and_then(Value::as_array) {
        arr.iter()
            .filter_map(|p| {
                let old = p.get("old_string").or_else(|| p.get("old")).and_then(Value::as_str)?;
                let new = p.get("new_string").or_else(|| p.get("new")).and_then(Value::as_str)?;
                Some((old.to_string(), new.to_string()))
            })
            .collect()
    } else if let (Some(old), Some(new)) = (
        field_string(&request, &["old_string", "old"]),
        field_string(&request, &["new_string", "new"]),
    ) {
        vec![(old, new)]
    } else {
        return builtin_error("file.patch", "缺少 patches 数组或 old_string/new_string 字段");
    };

    if patches.is_empty() {
        return builtin_error("file.patch", "patches 为空");
    }

    let mut result = content.clone();
    let mut applied = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for (i, (old, new)) in patches.iter().enumerate() {
        let count = result.matches(old.as_str()).count();
        if count == 0 {
            errors.push(format!("patch[{}]: old_string 未在文件中找到", i));
            continue;
        }
        if count > 1 {
            errors.push(format!("patch[{}]: old_string 匹配了 {} 处（需要唯一匹配）", i, count));
            continue;
        }
        result = result.replacen(old, new, 1);
        applied += 1;
    }

    if applied == 0 {
        return serde_json::json!({
            "tool": "file.patch",
            "status": "failed",
            "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
            "path": path.display().to_string(),
            "applied": 0,
            "total": patches.len(),
            "errors": errors,
            "error": "所有 patch 均未能应用"
        })
        .to_string();
    }

    if let Err(e) = fs::write(&path, &result) {
        return builtin_error("file.patch", format!("写回文件失败: {e}"));
    }

    serde_json::json!({
        "tool": "file.patch",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "applied": applied,
        "total": patches.len(),
        "errors": errors,
        "summary": format!("已应用 {}/{} 个 patch 到 {}", applied, patches.len(), path.display())
    })
    .to_string()
}

fn execute_file_remove(input: &str) -> String {
    let request = parse_json_object(input);
    let path_input = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["path", "file_path"]))
        .unwrap_or_else(|| input.trim().to_string());
    if path_input.is_empty() {
        return builtin_error("file.remove", "缺少文件路径");
    }

    let path = match resolve_path(&path_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.remove", e),
    };

    let recursive = request
        .as_ref()
        .and_then(|obj| field_bool(obj, &["recursive", "force"]))
        .unwrap_or(false);

    if !path.exists() {
        return builtin_error("file.remove", format!("路径不存在: {}", path.display()));
    }

    let is_dir = path.is_dir();
    let result = if is_dir {
        if recursive {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_dir(&path)
        }
    } else {
        fs::remove_file(&path)
    };

    if let Err(e) = result {
        return builtin_error("file.remove", format!("删除失败: {e}"));
    }

    serde_json::json!({
        "tool": "file.remove",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "was_directory": is_dir,
        "recursive": recursive,
        "summary": format!("已删除 {}", path.display())
    })
    .to_string()
}

fn execute_file_mkdir(input: &str) -> String {
    let request = parse_json_object(input);
    let path_input = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["path", "dir_path"]))
        .unwrap_or_else(|| input.trim().to_string());
    if path_input.is_empty() {
        return builtin_error("file.mkdir", "缺少目录路径");
    }

    let path = match resolve_path(&path_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.mkdir", e),
    };

    if path.exists() {
        if path.is_dir() {
            return serde_json::json!({
                "tool": "file.mkdir",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
                "path": path.display().to_string(),
                "already_existed": true,
                "summary": format!("目录已存在: {}", path.display())
            })
            .to_string();
        }
        return builtin_error("file.mkdir", format!("路径已存在且不是目录: {}", path.display()));
    }

    if let Err(e) = fs::create_dir_all(&path) {
        return builtin_error("file.mkdir", format!("创建目录失败: {e}"));
    }

    serde_json::json!({
        "tool": "file.mkdir",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "already_existed": false,
        "summary": format!("已创建目录 {}", path.display())
    })
    .to_string()
}

fn execute_file_copy(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("file.copy", "输入必须为 JSON 对象，包含 source 和 destination 字段"),
    };

    let src_input = match field_string(&request, &["source", "src", "from"]) {
        Some(p) => p,
        None => return builtin_error("file.copy", "缺少 source 字段"),
    };
    let dst_input = match field_string(&request, &["destination", "dst", "dest", "to"]) {
        Some(p) => p,
        None => return builtin_error("file.copy", "缺少 destination 字段"),
    };

    let src = match resolve_path(&src_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.copy", e),
    };
    let dst = match resolve_path(&dst_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.copy", e),
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(false);

    if !src.exists() {
        return builtin_error("file.copy", format!("源路径不存在: {}", src.display()));
    }

    if dst.exists() && !overwrite {
        return builtin_error("file.copy", format!("目标路径已存在且 overwrite=false: {}", dst.display()));
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return builtin_error("file.copy", format!("创建目标父目录失败: {e}"));
            }
        }
    }

    if src.is_dir() {
        if let Err(e) = copy_dir_recursive(&src, &dst) {
            return builtin_error("file.copy", format!("复制目录失败: {e}"));
        }
    } else {
        if let Err(e) = fs::copy(&src, &dst) {
            return builtin_error("file.copy", format!("复制文件失败: {e}"));
        }
    }

    serde_json::json!({
        "tool": "file.copy",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "source": src.display().to_string(),
        "destination": dst.display().to_string(),
        "is_directory": src.is_dir(),
        "summary": format!("已复制 {} → {}", src.display(), dst.display())
    })
    .to_string()
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let entry_dst = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &entry_dst)?;
        } else {
            fs::copy(entry.path(), &entry_dst)?;
        }
    }
    Ok(())
}

fn execute_file_move(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("file.move", "输入必须为 JSON 对象，包含 source 和 destination 字段"),
    };

    let src_input = match field_string(&request, &["source", "src", "from"]) {
        Some(p) => p,
        None => return builtin_error("file.move", "缺少 source 字段"),
    };
    let dst_input = match field_string(&request, &["destination", "dst", "dest", "to"]) {
        Some(p) => p,
        None => return builtin_error("file.move", "缺少 destination 字段"),
    };

    let src = match resolve_path(&src_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.move", e),
    };
    let dst = match resolve_path(&dst_input) {
        Ok(p) => p,
        Err(e) => return builtin_error("file.move", e),
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(false);

    if !src.exists() {
        return builtin_error("file.move", format!("源路径不存在: {}", src.display()));
    }

    if dst.exists() && !overwrite {
        return builtin_error("file.move", format!("目标路径已存在且 overwrite=false: {}", dst.display()));
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return builtin_error("file.move", format!("创建目标父目录失败: {e}"));
            }
        }
    }

    if dst.exists() && overwrite {
        let _ = if dst.is_dir() {
            fs::remove_dir_all(&dst)
        } else {
            fs::remove_file(&dst)
        };
    }

    if let Err(_) = fs::rename(&src, &dst) {
        if src.is_dir() {
            if let Err(e) = copy_dir_recursive(&src, &dst) {
                return builtin_error("file.move", format!("跨设备移动目录失败: {e}"));
            }
            if let Err(e) = fs::remove_dir_all(&src) {
                return builtin_error("file.move", format!("删除源目录失败: {e}"));
            }
        } else {
            if let Err(e) = fs::copy(&src, &dst) {
                return builtin_error("file.move", format!("跨设备移动文件失败: {e}"));
            }
            if let Err(e) = fs::remove_file(&src) {
                return builtin_error("file.move", format!("删除源文件失败: {e}"));
            }
        }
    }

    serde_json::json!({
        "tool": "file.move",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "source": src.display().to_string(),
        "destination": dst.display().to_string(),
        "summary": format!("已移动 {} → {}", src.display(), dst.display())
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// web.search — DuckDuckGo HTML 搜索
// ══════════════════════════════════════════════════════════════════════════════

fn execute_web_search(input: &str) -> String {
    let request = parse_json_object(input);
    let query = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["query", "q", "search"]))
        .unwrap_or_else(|| input.trim().to_string());
    if query.is_empty() {
        return builtin_error("web_search", "缺少搜索关键词 query");
    }

    let encoded = urlencoding::encode(&query);
    let search_url = format!("https://html.duckduckgo.com/html/?q={encoded}");

    let client = match reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return builtin_error("web_search", format!("HTTP 客户端初始化失败: {e}")),
    };

    let response = match client.get(&search_url).send() {
        Ok(r) => r,
        Err(e) => return builtin_error("web_search", format!("搜索请求失败: {e}")),
    };

    if !response.status().is_success() {
        return builtin_error("web_search", format!("搜索返回 HTTP {}", response.status()));
    }

    let html = match response.text() {
        Ok(t) => t,
        Err(e) => return builtin_error("web_search", format!("读取响应失败: {e}")),
    };

    let results = parse_duckduckgo_results(&html);

    serde_json::json!({
        "tool": "web_search",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "query": query,
        "result_count": results.len(),
        "results": results,
        "summary": format!("搜索 \"{}\" 返回 {} 条结果", query, results.len())
    })
    .to_string()
}

fn parse_duckduckgo_results(html: &str) -> Vec<Value> {
    let mut results = Vec::new();
    let link_re = regex::Regex::new(
        r#"<a[^>]+class="result__a"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>[\s\S]*?<a[^>]+class="result__snippet"[^>]*>([\s\S]*?)</a>"#
    ).unwrap();

    for cap in link_re.captures_iter(html) {
        let raw_url = &cap[1];
        let title = strip_html_tags(&decode_html_entities(&cap[2]));
        let snippet = strip_html_tags(&decode_html_entities(&cap[3]));
        let url = decode_duckduckgo_url(raw_url);
        if !title.trim().is_empty() {
            results.push(serde_json::json!({
                "title": title.trim(),
                "url": url,
                "snippet": snippet.trim()
            }));
        }
        if results.len() >= 10 {
            break;
        }
    }
    results
}

fn decode_duckduckgo_url(raw: &str) -> String {
    if raw.contains("duckduckgo.com/l/?") {
        if let Some(pos) = raw.find("uddg=") {
            let after = &raw[pos + 5..];
            let end = after.find('&').unwrap_or(after.len());
            if let Ok(decoded) = urlencoding::decode(&after[..end]) {
                return decoded.to_string();
            }
        }
    }
    if raw.starts_with("//") {
        return format!("https:{raw}");
    }
    raw.to_string()
}

fn strip_html_tags(text: &str) -> String {
    let tag_re = regex::Regex::new(r"<[^>]+>").unwrap();
    tag_re.replace_all(text, "").to_string()
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
}

// ══════════════════════════════════════════════════════════════════════════════
// web.fetch — URL 内容获取 + HTML→Markdown
// ══════════════════════════════════════════════════════════════════════════════

fn execute_web_fetch(input: &str) -> String {
    let request = parse_json_object(input);
    let url = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["url", "href", "link"]))
        .unwrap_or_else(|| input.trim().to_string());
    if url.is_empty() {
        return builtin_error("web_fetch", "缺少 URL");
    }

    let prompt = request.as_ref().and_then(|obj| field_string(obj, &["prompt"]));

    let client = match reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return builtin_error("web_fetch", format!("HTTP 客户端初始化失败: {e}")),
    };

    let response = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => return builtin_error("web_fetch", format!("请求失败: {e}")),
    };

    if !response.status().is_success() {
        return builtin_error("web_fetch", format!("HTTP {}", response.status()));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let body = match response.text() {
        Ok(t) => t,
        Err(e) => return builtin_error("web_fetch", format!("读取响应体失败: {e}")),
    };

    let content = if content_type.contains("application/json") {
        format!("```json\n{}\n```", body)
    } else if content_type.contains("text/plain") {
        body.clone()
    } else {
        html_to_markdown(&body)
    };

    let max_len = 50_000;
    let (content, truncated) = if content.len() > max_len {
        (format!("{}\n\n---\n*[内容已截断至 50,000 字符]*", &content[..max_len]), true)
    } else {
        (content, false)
    };

    serde_json::json!({
        "tool": "web_fetch",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "url": url,
        "prompt": prompt,
        "content_type": content_type,
        "content_length": content.len(),
        "truncated": truncated,
        "content": content,
        "summary": format!("已获取 {} ({} 字符)", url, content.len())
    })
    .to_string()
}

fn html_to_markdown(html: &str) -> String {
    let main = extract_main_content(html);
    let cleaned = regex::Regex::new(r"(?si)<script[\s\S]*?</script>").unwrap().replace_all(&main, "");
    let cleaned = regex::Regex::new(r"(?si)<style[\s\S]*?</style>").unwrap().replace_all(&cleaned, "");
    let cleaned = regex::Regex::new(r"(?si)<noscript[\s\S]*?</noscript>").unwrap().replace_all(&cleaned, "");
    let cleaned = regex::Regex::new(r"(?si)<(nav|footer|header|aside|iframe)[^>]*>[\s\S]*?</\1>").unwrap().replace_all(&cleaned, "");

    let md = regex::Regex::new(r"(?si)<h([1-6])[^>]*>([\s\S]*?)</h\1>").unwrap()
        .replace_all(&cleaned, |caps: &regex::Captures| {
            let level: usize = caps[1].parse().unwrap_or(1);
            let text = strip_html_tags(&decode_html_entities(&caps[2]));
            format!("\n{} {}\n", "#".repeat(level), text.trim())
        });
    let md = regex::Regex::new(r"(?si)<pre[^>]*>\s*<code[^>]*>([\s\S]*?)</code>\s*</pre>").unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("\n```\n{}\n```\n", decode_html_entities(&caps[1]).trim())
        });
    let md = regex::Regex::new(r"(?si)<code[^>]*>([\s\S]*?)</code>").unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("`{}`", decode_html_entities(&caps[1]).trim())
        });
    let md = regex::Regex::new(r#"(?si)<a[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#).unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            let text = strip_html_tags(&decode_html_entities(&caps[2]));
            format!("[{}]({})", text.trim(), &caps[1])
        });
    let md = regex::Regex::new(r"(?si)<li[^>]*>([\s\S]*?)</li>").unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("\n- {}", strip_html_tags(&decode_html_entities(&caps[1])).trim())
        });
    let md = md.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
    let md = md.replace("</p>", "\n\n");
    let md = regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&md, "");
    let md = regex::Regex::new(r"\n{3,}").unwrap().replace_all(&md, "\n\n");

    decode_html_entities(&md).trim().to_string()
}

fn extract_main_content(html: &str) -> String {
    let patterns = [
        r"(?si)<main[^>]*>([\s\S]*?)</main>",
        r"(?si)<article[^>]*>([\s\S]*?)</article>",
        r#"(?si)<div[^>]+role="main"[^>]*>([\s\S]*?)</div>"#,
        r"(?si)<body[^>]*>([\s\S]*?)</body>",
    ];
    for pat in &patterns {
        if let Some(caps) = regex::Regex::new(pat).unwrap().captures(html) {
            return caps[1].to_string();
        }
    }
    html.to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// mermaid.diagram — Mermaid 语法验证 + 渲染数据
// ══════════════════════════════════════════════════════════════════════════════

fn execute_mermaid_diagram(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("mermaid_diagram", "输入必须为 JSON 对象，包含 code 字段"),
    };

    let code = match field_string(&request, &["code"]) {
        Some(c) => c,
        None => return builtin_error("mermaid_diagram", "缺少 code 字段"),
    };
    let title = field_string(&request, &["title"]);
    let theme = field_string(&request, &["theme"]).unwrap_or_else(|| "default".to_string());

    let trimmed = code.trim();
    if trimmed.is_empty() {
        return builtin_error("mermaid_diagram", "Mermaid 代码为空");
    }

    let diagram_types: &[(&str, &str)] = &[
        ("graph ", "flowchart"), ("flowchart ", "flowchart"),
        ("sequenceDiagram", "sequence"), ("classDiagram", "class"),
        ("stateDiagram", "state"), ("erDiagram", "er"),
        ("gantt", "gantt"), ("pie", "pie"), ("journey", "journey"),
        ("gitGraph", "git"), ("mindmap", "mindmap"), ("timeline", "timeline"),
        ("quadrantChart", "quadrant"), ("requirementDiagram", "requirement"),
        ("C4Context", "c4"), ("sankey", "sankey"), ("xychart", "xychart"),
        ("block-beta", "block"),
    ];

    let diagram_type = diagram_types.iter()
        .find(|(prefix, _)| trimmed.to_lowercase().starts_with(&prefix.to_lowercase()))
        .map(|(_, t)| *t);

    let diagram_type = match diagram_type {
        Some(t) => t,
        None => return builtin_error(
            "mermaid_diagram",
            "无法识别的 Mermaid 图表类型。代码须以有效声明开头（graph, flowchart, sequenceDiagram, classDiagram 等）"
        ),
    };

    serde_json::json!({
        "tool": "mermaid_diagram",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "type": "mermaid_diagram",
        "code": trimmed,
        "title": title,
        "theme": theme,
        "diagram_type": diagram_type,
        "summary": format!("已生成 {} 类型 Mermaid 图表数据", diagram_type)
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// search.semantic — 语义代码检索（需要 CodebaseRetrievalService 注入）
// ══════════════════════════════════════════════════════════════════════════════

fn execute_search_semantic(input: &str) -> String {
    let request = parse_json_object(input);
    let query = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["query"]))
        .unwrap_or_else(|| input.trim().to_string());
    if query.is_empty() {
        return builtin_error("search.semantic", "缺少 query 字段");
    }

    serde_json::json!({
        "tool": "search.semantic",
        "status": "failed",
        "error": "代码语义检索服务暂未初始化。请使用 search.text 进行正则/文本搜索作为替代。",
        "query": query,
        "hint": "此工具需要 CodebaseRetrievalService 就绪后方可使用"
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// knowledge.query — 项目知识库查询（需要 ProjectKnowledgeBase 注入）
// ══════════════════════════════════════════════════════════════════════════════

fn execute_knowledge_query(input: &str) -> String {
    let request = parse_json_object(input);
    let category = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["category"]))
        .unwrap_or_else(|| "all".to_string());

    serde_json::json!({
        "tool": "knowledge_query",
        "status": "failed",
        "error": "项目知识库服务暂未初始化。",
        "category": category,
        "hint": "此工具需要 ProjectKnowledgeBase 就绪后方可使用"
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// orchestration.* — 编排工具（需要 OrchestratorService 注入）
// ══════════════════════════════════════════════════════════════════════════════

fn execute_orchestration_stub(name: BuiltinToolName, _input: &str) -> String {
    serde_json::json!({
        "tool": name.as_str(),
        "status": "failed",
        "error": format!("编排工具 {} 需要 Orchestrator 运行时上下文，当前未在编排循环中执行。", name.as_str()),
        "hint": "此工具仅在 Orchestrator/Worker LLM 交互循环中由运行时注入后可用"
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// skill.apply — Skill 加载（需要 SkillRuntime 注入）
// ══════════════════════════════════════════════════════════════════════════════

fn execute_skill_apply_stub(input: &str) -> String {
    let request = parse_json_object(input);
    let skill_name = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["skill_name", "name"]));

    serde_json::json!({
        "tool": "skill_apply",
        "status": "failed",
        "error": "Skill 运行时服务暂未初始化。",
        "skill_name": skill_name,
        "hint": "此工具需要 SkillRuntime 就绪后方可使用"
    })
    .to_string()
}
