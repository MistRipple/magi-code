use crate::{
    BuiltinTool, BuiltinToolAccessMode, BuiltinToolName, BuiltinToolSpec, ToolExecutionContext,
    ToolExecutionContextQuery, ToolRuntimeResources, apply_patch::execute_apply_patch,
    tool_catalog::execute_tool_catalog, view_image::execute_view_image,
};
use magi_core::{ApprovalRequirement, ExecutionResultStatus, RiskLevel, UtcMillis};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt::Display,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_SHELL_TIMEOUT_MS: u64 = 30_000;
const MAX_SHELL_TIMEOUT_MS: u64 = 120_000;
const SHELL_TIMEOUT_POLL_MS: u64 = 20;
const FILE_READ_PUBLIC_ERROR: &str = "文件暂不可读取，请检查路径或权限";
const FILE_WRITE_PUBLIC_ERROR: &str = "文件暂不可写入，请检查路径或权限";
const FILE_DELETE_PUBLIC_ERROR: &str = "文件暂不可删除，请检查路径或权限";
const FILE_COPY_PUBLIC_ERROR: &str = "文件暂不可复制，请检查路径或权限";
const FILE_MOVE_PUBLIC_ERROR: &str = "文件暂不可移动，请检查路径或权限";
const DIRECTORY_CREATE_PUBLIC_ERROR: &str = "目录暂不可创建，请检查路径或权限";
const SEARCH_TEXT_PUBLIC_ERROR: &str = "文本搜索暂不可用，请检查路径或权限";
const DIFF_PREVIEW_PUBLIC_ERROR: &str = "差异预览源暂不可读取，请检查路径或权限";
const SHELL_EXEC_PUBLIC_ERROR: &str = "shell 命令暂不可执行，请检查运行环境";
const PROCESS_LAUNCH_PUBLIC_ERROR: &str = "后台进程暂不可启动，请检查运行环境";
const PROCESS_WRITE_PUBLIC_ERROR: &str = "后台进程暂不可写入，请稍后重试";
const PROCESS_INSPECT_PUBLIC_ERROR: &str = "进程信息暂不可读取，请稍后重试";
const WEB_SEARCH_PUBLIC_ERROR: &str = "网络搜索暂不可用，请稍后重试";
const WEB_FETCH_PUBLIC_ERROR: &str = "网页内容暂不可获取，请稍后重试";
const PATH_RESOLUTION_PUBLIC_ERROR: &str = "路径暂不可解析，请检查工作区或路径";
const PATH_NOT_FOUND_PUBLIC_ERROR: &str = "目标路径不存在，请检查路径";
const PATH_ALREADY_EXISTS_PUBLIC_ERROR: &str = "目标路径已存在，请确认是否允许覆盖";
const PROTECTED_DELETE_PUBLIC_ERROR: &str = "该路径受保护，不能删除";

#[derive(Clone)]
struct ActiveShellExec {
    execution_id: u64,
    session_id: Option<String>,
    workspace_id: Option<String>,
    task_id: Option<String>,
    child: Arc<Mutex<Child>>,
}

struct ShellExecOutput {
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    cancelled: bool,
}

static SHELL_EXECUTION_COUNTER: AtomicU64 = AtomicU64::new(1);
static ACTIVE_SHELL_EXECUTIONS: LazyLock<Mutex<HashMap<u64, ActiveShellExec>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct ManagedProcess {
    terminal_id: u64,
    command: String,
    cwd: String,
    session_id: Option<String>,
    workspace_id: Option<String>,
    child: Child,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    started_at_ms: u64,
}

static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);
static PROCESS_TABLE: LazyLock<Mutex<HashMap<u64, ManagedProcess>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

    fn execute(
        &self,
        input: &str,
        context: &ToolExecutionContext,
        resources: &ToolRuntimeResources,
    ) -> String {
        match self.name {
            BuiltinToolName::FileRead => execute_file_read(input, context),
            BuiltinToolName::ViewImage => execute_view_image(input, context),
            BuiltinToolName::FileWrite => execute_file_write(input, context),
            BuiltinToolName::FilePatch => execute_file_patch(input, context),
            BuiltinToolName::ApplyPatch => execute_apply_patch(input, context),
            BuiltinToolName::FileRemove => execute_file_remove(input, context),
            BuiltinToolName::FileMkdir => execute_file_mkdir(input, context),
            BuiltinToolName::FileCopy => execute_file_copy(input, context),
            BuiltinToolName::FileMove => execute_file_move(input, context),
            BuiltinToolName::SearchText => execute_search_text(input, context),
            BuiltinToolName::SearchSemantic => execute_search_semantic(input, context, resources),
            BuiltinToolName::ShellExec => execute_shell_exec(input, context),
            BuiltinToolName::ProcessLaunch => execute_process_launch(input, context),
            BuiltinToolName::ProcessRead => execute_process_read(input, context),
            BuiltinToolName::ProcessWrite => execute_process_write(input, context),
            BuiltinToolName::ProcessKill => execute_process_kill(input, context),
            BuiltinToolName::ProcessList => execute_process_list(context),
            BuiltinToolName::ProcessInspect => execute_process_inspect(input),
            BuiltinToolName::DiffPreview => execute_diff_preview(input),
            BuiltinToolName::WebSearch => execute_web_search(input),
            BuiltinToolName::WebFetch => execute_web_fetch(input),
            BuiltinToolName::DiagramRender => execute_diagram_render(input),
            BuiltinToolName::KnowledgeQuery => execute_knowledge_query(input, context, resources),
            BuiltinToolName::CodeSymbols => execute_code_symbols(input, context, resources),
            BuiltinToolName::ToolCatalog => execute_tool_catalog(input, context, resources),
            BuiltinToolName::AgentSpawn
            | BuiltinToolName::AgentWait
            | BuiltinToolName::TodoWrite
            | BuiltinToolName::MemoryWrite
            | BuiltinToolName::MissionCharterWrite
            | BuiltinToolName::PlanWrite
            | BuiltinToolName::KgWrite
            | BuiltinToolName::ValidationRecord
            | BuiltinToolName::Checkpoint
            | BuiltinToolName::HumanCheckpointRequest => {
                execute_orchestration_only(self.name, input)
            }
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

fn required_string_or_raw(
    input: &str,
    request: Option<&serde_json::Map<String, Value>>,
    keys: &[&str],
    tool: &str,
    missing_message: &str,
) -> Result<String, String> {
    let value = match request {
        Some(object) => field_string(object, keys).unwrap_or_default(),
        None => input.trim().to_string(),
    }
    .trim()
    .to_string();
    if value.is_empty() {
        return Err(builtin_error(tool, missing_message));
    }
    Ok(value)
}

fn field_usize(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value
                .as_u64()
                .map(|value| value as usize)
                .or_else(|| value.as_str().and_then(|value| value.parse::<usize>().ok()))
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

fn context_working_directory(context: &ToolExecutionContext) -> Result<PathBuf, String> {
    context
        .working_directory
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| "无法解析当前工作目录".to_string())
}

pub(crate) fn resolve_path_with_context(
    input: &str,
    context: &ToolExecutionContext,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(input);
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd = context_working_directory(context)?;
    if path
        .components()
        .all(|component| matches!(component, std::path::Component::CurDir))
    {
        return Ok(cwd);
    }
    Ok(cwd.join(path))
}

fn execute_file_read(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let path_input = match required_string_or_raw(
        input,
        request.as_ref(),
        &["path", "file_path", "filePath"],
        "file_read",
        "缺少文件路径",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let max_bytes = request
        .as_ref()
        .and_then(|object| field_usize(object, &["max_bytes", "preview_bytes"]))
        .unwrap_or(64 * 1024)
        .max(1);

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(path) => path,
        Err(error) => {
            return builtin_path_resolution_error("file_read", &path_input, error);
        }
    };

    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return builtin_filesystem_error(
                "file_read",
                FILE_READ_PUBLIC_ERROR,
                "读取文件元数据失败",
                &path,
                error,
            );
        }
    };

    if metadata.is_dir() {
        let mut entries = match fs::read_dir(&path) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            Err(error) => {
                return builtin_filesystem_error(
                    "file_read",
                    FILE_READ_PUBLIC_ERROR,
                    "读取目录失败",
                    &path,
                    error,
                );
            }
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
        Err(error) => {
            return builtin_filesystem_error(
                "file_read",
                FILE_READ_PUBLIC_ERROR,
                "读取文件失败",
                &path,
                error,
            );
        }
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

fn execute_search_text(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let query = match required_string_or_raw(
        input,
        request.as_ref(),
        &["query", "text", "needle"],
        "search_text",
        "缺少搜索关键词",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let root_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["root", "path", "workspace"]))
        .unwrap_or_else(|| ".".to_string());
    let root = match resolve_path_with_context(&root_input, context) {
        Ok(path) => path,
        Err(error) => {
            return builtin_path_resolution_error("search_text", &root_input, error);
        }
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

    let (matches, scanned_files, truncated) =
        match search_text_matches(&root, &query, case_sensitive, include_hidden, limit) {
            Ok(result) => result,
            Err(error) => {
                return builtin_filesystem_error(
                    "search_text",
                    SEARCH_TEXT_PUBLIC_ERROR,
                    error.action,
                    &error.path,
                    error.source,
                );
            }
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

fn execute_shell_exec(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    if let Some(payload) = execute_shell_exec_background_action(input, request.as_ref(), context) {
        return payload;
    }
    let command = match required_string_or_raw(
        input,
        request.as_ref(),
        &["command", "script", "line"],
        "shell_exec",
        "缺少 shell 命令",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if request
        .as_ref()
        .and_then(|object| field_bool(object, &["background", "long_running", "longRunning"]))
        .unwrap_or(false)
    {
        return execute_process_launch_with_surface(
            input,
            context,
            "shell_exec",
            Some("background"),
        );
    }
    let access_mode = request
        .as_ref()
        .and_then(|object| {
            field_string(object, &["access_mode", "write_mode", "intent"])
                .and_then(|value| BuiltinToolAccessMode::from_str(&value))
        })
        .unwrap_or(BuiltinToolAccessMode::MaybeWrite);
    if access_mode == BuiltinToolAccessMode::ReadOnly
        && !magi_permissions::PermissionEngine::shell_arguments_request_read_only(input)
    {
        return builtin_rejected(
            "shell_exec",
            "shell_exec 声明 access_mode=read_only 时，命令不能包含写入迹象",
        );
    }
    let cwd_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["cwd", "working_directory", "workdir"]));
    let cwd = match cwd_input {
        Some(value) => match resolve_path_with_context(&value, context) {
            Ok(path) => path,
            Err(error) => {
                return builtin_path_resolution_error("shell_exec", &value, error);
            }
        },
        None => match context_working_directory(context) {
            Ok(path) => path,
            Err(error) => {
                return builtin_path_resolution_error("shell_exec", "<context>", error);
            }
        },
    };
    let shell = request
        .as_ref()
        .and_then(|object| field_string(object, &["shell"]))
        .unwrap_or_else(default_shell_binary);
    let timeout_ms = request
        .as_ref()
        .and_then(|object| field_usize(object, &["timeout_ms", "timeoutMs", "timeout"]))
        .map(|value| value as u64)
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_MS)
        .clamp(SHELL_TIMEOUT_POLL_MS, MAX_SHELL_TIMEOUT_MS);
    if let Some(payload) = non_git_read_only_probe_payload(&command, &cwd, access_mode, timeout_ms)
    {
        return payload;
    }

    let output =
        match execute_shell_command_with_timeout(&shell, &command, &cwd, timeout_ms, context) {
            Ok(output) => output,
            Err(error) => {
                return builtin_runtime_error(
                    "shell_exec",
                    SHELL_EXEC_PUBLIC_ERROR,
                    "启动 shell 命令失败",
                    error,
                );
            }
        };

    let succeeded = output
        .status
        .as_ref()
        .map(ExitStatus::success)
        .unwrap_or(false)
        && !output.timed_out;
    let status = if output.cancelled {
        "cancelled"
    } else if succeeded {
        "succeeded"
    } else {
        "failed"
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.as_ref().and_then(ExitStatus::code);

    serde_json::json!({
        "tool": "shell_exec",
        "status": status,
        "command": command,
        "cwd": cwd.display().to_string(),
        "access_mode": access_mode.as_str(),
        "timeout_ms": timeout_ms,
        "timed_out": output.timed_out,
        "cancelled": output.cancelled,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "summary": if output.timed_out {
            format!("命令执行超时({timeout_ms}ms): {command}")
        } else if output.cancelled {
            format!("命令已取消: {command}")
        } else if succeeded {
            format!("命令执行成功: {}", command)
        } else {
            format!("命令执行失败(退出码 {:?}): {}", exit_code, command)
        }
    })
    .to_string()
}

fn execute_shell_exec_background_action(
    input: &str,
    request: Option<&serde_json::Map<String, Value>>,
    context: &ToolExecutionContext,
) -> Option<String> {
    let request = request?;
    let action = field_string(request, &["action", "operation", "op"])
        .map(|value| value.trim().to_ascii_lowercase());
    let has_terminal_id = field_usize(request, &["terminal_id", "terminalId", "id"]).is_some();
    let has_command = field_string(request, &["command", "script", "line"])
        .is_some_and(|value| !value.trim().is_empty());

    let mode = match action.as_deref() {
        None if has_terminal_id && !has_command => "read",
        None => return None,
        Some("run" | "exec" | "command") => return None,
        Some("read" | "poll" | "status") => "read",
        Some("write" | "stdin" | "send") => "write",
        Some("kill" | "stop" | "terminate" | "cancel") => "kill",
        Some("list" | "ls") => "list",
        Some(other) => {
            return Some(builtin_error(
                "shell_exec",
                format!("未知后台进程动作: {other}"),
            ));
        }
    };

    Some(match mode {
        "read" => {
            execute_process_read_with_surface(input, context, "shell_exec", Some("background_read"))
        }
        "write" => execute_process_write_with_surface(
            input,
            context,
            "shell_exec",
            Some("background_write"),
        ),
        "kill" => {
            execute_process_kill_with_surface(input, context, "shell_exec", Some("background_kill"))
        }
        "list" => execute_process_list_with_surface(context, "shell_exec", Some("background_list")),
        _ => unreachable!("validated shell_exec background action"),
    })
}

fn non_git_read_only_probe_payload(
    command: &str,
    cwd: &Path,
    access_mode: BuiltinToolAccessMode,
    timeout_ms: u64,
) -> Option<String> {
    if access_mode != BuiltinToolAccessMode::ReadOnly {
        return None;
    }
    let target = git_probe_target(command, cwd)?;
    if is_git_worktree(&target) {
        return None;
    }
    Some(
        serde_json::json!({
            "tool": "shell_exec",
            "status": "succeeded",
            "command": command,
            "cwd": cwd.display().to_string(),
            "access_mode": access_mode.as_str(),
            "timeout_ms": timeout_ms,
            "timed_out": false,
            "cancelled": false,
            "exit_code": 0,
            "stdout": "NOT_GIT_WORKTREE\n",
            "stderr": "",
            "git_worktree": false,
            "skipped": true,
            "summary": format!("工作区不是 Git worktree，已跳过 Git 状态探测: {command}")
        })
        .to_string(),
    )
}

fn git_probe_target(command: &str, cwd: &Path) -> Option<PathBuf> {
    simple_git_probe_target(command, cwd).or_else(|| compound_git_probe_target(command, cwd))
}

fn simple_git_probe_target(command: &str, cwd: &Path) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed
        .chars()
        .any(|ch| matches!(ch, '&' | '|' | ';' | '`' | '$' | '<' | '>' | '\n'))
    {
        return None;
    }
    let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
    if tokens.first().copied() != Some("git") {
        return None;
    }
    let mut index = 1usize;
    let mut target = cwd.to_path_buf();
    if tokens.get(index).copied() == Some("-C") {
        let path = tokens.get(index + 1)?;
        target = resolve_git_probe_path(path, cwd);
        index += 2;
    }
    match tokens.get(index).copied() {
        Some("status") | Some("diff") => Some(target),
        _ => None,
    }
}

fn compound_git_probe_target(command: &str, cwd: &Path) -> Option<PathBuf> {
    let tokens = command
        .split_whitespace()
        .map(clean_shell_token)
        .collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if token != "git" {
            continue;
        }
        let mut cursor = index + 1;
        let mut target = cwd.to_path_buf();
        if tokens.get(cursor).map(String::as_str) == Some("-C") {
            let path = tokens.get(cursor + 1)?;
            target = resolve_git_probe_path(path, cwd);
            cursor += 2;
        }
        if matches!(
            tokens.get(cursor).map(String::as_str),
            Some("status") | Some("diff")
        ) {
            return Some(target);
        }
    }
    None
}

fn clean_shell_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '\'' | '"' | '(' | ')' | '{' | '}' | '[' | ']' | ';' | '&' | '|'
            )
        })
        .to_string()
}

fn resolve_git_probe_path(path: &str, cwd: &Path) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

fn is_git_worktree(path: &Path) -> bool {
    let path_arg = path.to_string_lossy().to_string();
    Command::new("git")
        .args([
            "-C",
            path_arg.as_str(),
            "rev-parse",
            "--is-inside-work-tree",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn execute_shell_command_with_timeout(
    shell: &str,
    command: &str,
    cwd: &Path,
    timeout_ms: u64,
    context: &ToolExecutionContext,
) -> Result<ShellExecOutput, String> {
    let mut command_builder = Command::new(shell);
    command_builder
        .arg(shell_arg())
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        command_builder.process_group(0);
    }
    let mut child = command_builder.spawn().map_err(|error| error.to_string())?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let execution_id = register_active_shell_exec(context, &child);
    let stdout_reader = thread::spawn(move || read_child_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_child_pipe(stderr));
    let started_at = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        let wait_state = {
            child
                .lock()
                .expect("shell child lock poisoned")
                .try_wait()
                .map_err(|error| error.to_string())?
        };
        match wait_state {
            Some(status) => {
                if !active_shell_exec_is_registered(execution_id) {
                    cancelled = true;
                }
                break Some(status);
            }
            None if started_at.elapsed() >= timeout => {
                timed_out = true;
                break terminate_shell_child(&child);
            }
            None if !active_shell_exec_is_registered(execution_id) => {
                cancelled = true;
                break child.lock().expect("shell child lock poisoned").wait().ok();
            }
            None => thread::sleep(Duration::from_millis(SHELL_TIMEOUT_POLL_MS)),
        }
    };
    unregister_active_shell_exec(execution_id);

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(ShellExecOutput {
        status,
        stdout,
        stderr,
        timed_out,
        cancelled,
    })
}

fn register_active_shell_exec(context: &ToolExecutionContext, child: &Arc<Mutex<Child>>) -> u64 {
    let execution_id = SHELL_EXECUTION_COUNTER.fetch_add(1, Ordering::SeqCst);
    let process = ActiveShellExec {
        execution_id,
        session_id: context
            .session_id
            .as_ref()
            .map(|id| id.as_str().to_string()),
        workspace_id: context
            .workspace_id
            .as_ref()
            .map(|id| id.as_str().to_string()),
        task_id: context.task_id.as_ref().map(|id| id.as_str().to_string()),
        child: Arc::clone(child),
    };
    ACTIVE_SHELL_EXECUTIONS
        .lock()
        .expect("active shell execution lock poisoned")
        .insert(execution_id, process);
    execution_id
}

fn unregister_active_shell_exec(execution_id: u64) {
    ACTIVE_SHELL_EXECUTIONS
        .lock()
        .expect("active shell execution lock poisoned")
        .remove(&execution_id);
}

fn active_shell_exec_is_registered(execution_id: u64) -> bool {
    ACTIVE_SHELL_EXECUTIONS
        .lock()
        .expect("active shell execution lock poisoned")
        .contains_key(&execution_id)
}

fn active_shell_matches_query(
    process: &ActiveShellExec,
    query: &ToolExecutionContextQuery,
) -> bool {
    let has_scope =
        query.session_id.is_some() || query.workspace_id.is_some() || query.task_id.is_some();
    if !has_scope {
        return false;
    }
    if let Some(session_id) = query.session_id.as_ref()
        && process.session_id.as_deref() != Some(session_id.as_str())
    {
        return false;
    }
    if let Some(workspace_id) = query.workspace_id.as_ref()
        && process.workspace_id.as_deref() != Some(workspace_id.as_str())
    {
        return false;
    }
    if let Some(task_id) = query.task_id.as_ref()
        && process.task_id.as_deref() != Some(task_id.as_str())
    {
        return false;
    }
    true
}

pub(crate) fn cancel_active_shell_execs(query: &ToolExecutionContextQuery) -> usize {
    let processes = {
        let mut table = ACTIVE_SHELL_EXECUTIONS
            .lock()
            .expect("active shell execution lock poisoned");
        let execution_ids = table
            .values()
            .filter(|process| active_shell_matches_query(process, query))
            .map(|process| process.execution_id)
            .collect::<Vec<_>>();
        execution_ids
            .into_iter()
            .filter_map(|execution_id| table.remove(&execution_id))
            .collect::<Vec<_>>()
    };
    for process in &processes {
        let _ = terminate_shell_child(&process.child);
    }
    processes.len()
}

fn terminate_shell_child(child: &Arc<Mutex<Child>>) -> Option<ExitStatus> {
    let mut child = child.lock().expect("shell child lock poisoned");
    terminate_process_group(&mut child);
    child.wait().ok()
}

fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(&process_group)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        thread::sleep(Duration::from_millis(50));
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(&process_group)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

fn read_child_pipe<T: Read>(pipe: Option<T>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer);
    buffer
}

fn execute_process_launch(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_launch_with_surface(input, context, "process_launch", None)
}

fn execute_process_launch_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let request = parse_json_object(input);
    let command = match required_string_or_raw(
        input,
        request.as_ref(),
        &["command", "script", "line"],
        surface_tool,
        "缺少 shell 命令",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let cwd_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["cwd", "working_directory", "workdir"]));
    let cwd = match cwd_input {
        Some(value) => match resolve_path_with_context(&value, context) {
            Ok(path) => path,
            Err(error) => {
                return builtin_path_resolution_error(surface_tool, &value, error);
            }
        },
        None => match context_working_directory(context) {
            Ok(path) => path,
            Err(error) => {
                return builtin_path_resolution_error(surface_tool, "<context>", error);
            }
        },
    };
    let shell = request
        .as_ref()
        .and_then(|object| field_string(object, &["shell"]))
        .unwrap_or_else(default_shell_binary);

    let mut child = match Command::new(&shell)
        .arg(shell_arg())
        .arg(&command)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let public_message = if surface_tool == "shell_exec" {
                SHELL_EXEC_PUBLIC_ERROR
            } else {
                PROCESS_LAUNCH_PUBLIC_ERROR
            };
            return builtin_runtime_error(surface_tool, public_message, "启动后台进程失败", error);
        }
    };

    let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
    let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
    spawn_managed_process_reader(child.stdout.take(), Arc::clone(&stdout_buffer));
    spawn_managed_process_reader(child.stderr.take(), Arc::clone(&stderr_buffer));
    let terminal_id = NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed);
    let process = ManagedProcess {
        terminal_id,
        command: command.clone(),
        cwd: cwd.display().to_string(),
        session_id: context.session_id.as_ref().map(ToString::to_string),
        workspace_id: context.workspace_id.as_ref().map(ToString::to_string),
        child,
        stdout: stdout_buffer,
        stderr: stderr_buffer,
        started_at_ms: UtcMillis::now().0,
    };
    PROCESS_TABLE
        .lock()
        .expect("process table lock poisoned")
        .insert(terminal_id, process);

    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "command": command,
        "cwd": cwd.display().to_string(),
        "session_id": context.session_id.as_ref().map(ToString::to_string),
        "workspace_id": context.workspace_id.as_ref().map(ToString::to_string),
        "startup_status": "running",
        "summary": if surface_tool == "shell_exec" {
            format!("已在后台启动 shell 终端 #{terminal_id}: {command}")
        } else {
            format!("已在后台启动进程 #{terminal_id}: {command}")
        }
    });
    if let Some(mode) = mode {
        payload["mode"] = serde_json::Value::String(mode.to_string());
    }
    payload.to_string()
}

fn execute_process_read(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_read_with_surface(input, context, "process_read", None)
}

fn execute_process_read_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let request = match parse_json_object(input) {
        Some(request) => request,
        None => return builtin_error(surface_tool, "输入必须为 JSON 对象，包含 terminal_id"),
    };
    let Some(terminal_id) = field_usize(&request, &["terminal_id", "terminalId", "id"]) else {
        return builtin_error(surface_tool, "缺少 terminal_id");
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let max_bytes = field_usize(&request, &["max_bytes", "preview_bytes", "limit"])
        .unwrap_or(12_000)
        .clamp(512, 200_000);

    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return builtin_error(surface_tool, format!("进程不存在: {terminal_id}"));
    };
    if !process_belongs_to_context(process, context) {
        return builtin_error(surface_tool, "进程不属于当前 session/workspace");
    }
    let running = process
        .child
        .try_wait()
        .ok()
        .flatten()
        .map(|_| false)
        .unwrap_or(true);
    let stdout = tail_utf8(
        &process.stdout.lock().expect("stdout lock poisoned"),
        max_bytes,
    );
    let stderr = tail_utf8(
        &process.stderr.lock().expect("stderr lock poisoned"),
        max_bytes,
    );

    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "running": running,
        "stdout": stdout,
        "stderr": stderr,
        "summary": if running {
            format!("进程 #{terminal_id} 正在运行")
        } else {
            format!("进程 #{terminal_id} 已结束")
        }
    });
    if let Some(mode) = mode {
        payload["mode"] = serde_json::Value::String(mode.to_string());
    }
    payload.to_string()
}

fn execute_process_write(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_write_with_surface(input, context, "process_write", None)
}

fn execute_process_write_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let request = match parse_json_object(input) {
        Some(request) => request,
        None => return builtin_error(surface_tool, "输入必须为 JSON 对象，包含 terminal_id"),
    };
    let Some(terminal_id) = field_usize(&request, &["terminal_id", "terminalId", "id"]) else {
        return builtin_error(surface_tool, "缺少 terminal_id");
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let content = field_string(&request, &["input", "content", "text"]).unwrap_or_default();
    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return builtin_error(surface_tool, format!("进程不存在: {terminal_id}"));
    };
    if !process_belongs_to_context(process, context) {
        return builtin_error(surface_tool, "进程不属于当前 session/workspace");
    }
    let Some(stdin) = process.child.stdin.as_mut() else {
        return builtin_error(surface_tool, format!("进程 #{terminal_id} 不接受输入"));
    };
    if let Err(error) = stdin.write_all(content.as_bytes()) {
        return builtin_runtime_error(
            surface_tool,
            PROCESS_WRITE_PUBLIC_ERROR,
            "写入后台进程失败",
            error,
        );
    }
    let _ = stdin.flush();
    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "written_bytes": content.len(),
        "summary": format!("已写入进程 #{terminal_id}")
    });
    if let Some(mode) = mode {
        payload["mode"] = serde_json::Value::String(mode.to_string());
    }
    payload.to_string()
}

fn execute_process_kill(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_kill_with_surface(input, context, "process_kill", None)
}

fn execute_process_kill_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let request = match parse_json_object(input) {
        Some(request) => request,
        None => return builtin_error(surface_tool, "输入必须为 JSON 对象，包含 terminal_id"),
    };
    let Some(terminal_id) = field_usize(&request, &["terminal_id", "terminalId", "id"]) else {
        return builtin_error(surface_tool, "缺少 terminal_id");
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return builtin_error(surface_tool, format!("进程不存在: {terminal_id}"));
    };
    if !process_belongs_to_context(process, context) {
        return builtin_error(surface_tool, "进程不属于当前 session/workspace");
    }
    let _ = process.child.kill();
    let _ = process.child.wait();
    table.remove(&(terminal_id as u64));
    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "summary": format!("已停止进程 #{terminal_id}")
    });
    if let Some(mode) = mode {
        payload["mode"] = serde_json::Value::String(mode.to_string());
    }
    payload.to_string()
}

fn execute_process_list(context: &ToolExecutionContext) -> String {
    execute_process_list_with_surface(context, "process_list", None)
}

fn execute_process_list_with_surface(
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let mut processes = Vec::new();
    for process in table.values_mut() {
        if !process_belongs_to_context(process, context) {
            continue;
        }
        let running = process
            .child
            .try_wait()
            .ok()
            .flatten()
            .map(|_| false)
            .unwrap_or(true);
        processes.push(serde_json::json!({
            "terminal_id": process.terminal_id,
            "command": process.command,
            "cwd": process.cwd,
            "running": running,
            "session_id": process.session_id,
            "workspace_id": process.workspace_id,
            "started_at": process.started_at_ms,
        }));
    }
    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "processes": processes,
        "summary": "已列出当前上下文后台进程"
    });
    if let Some(mode) = mode {
        payload["mode"] = serde_json::Value::String(mode.to_string());
    }
    payload.to_string()
}

fn spawn_managed_process_reader<T: Read + Send + 'static>(
    pipe: Option<T>,
    target: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut chunk = [0_u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    let mut buffer = target.lock().expect("process output lock poisoned");
                    buffer.extend_from_slice(&chunk[..size]);
                    const MAX_BUFFER: usize = 1_000_000;
                    if buffer.len() > MAX_BUFFER {
                        let drain = buffer.len() - MAX_BUFFER;
                        buffer.drain(0..drain);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn process_belongs_to_context(process: &ManagedProcess, context: &ToolExecutionContext) -> bool {
    if let Some(session_id) = process.session_id.as_deref() {
        if context.session_id.as_ref().map(|id| id.as_str()) != Some(session_id) {
            return false;
        }
    }
    if let Some(workspace_id) = process.workspace_id.as_deref() {
        if context.workspace_id.as_ref().map(|id| id.as_str()) != Some(workspace_id) {
            return false;
        }
    }
    process.session_id.is_some() || process.workspace_id.is_some()
}

fn require_process_context(tool: &str, context: &ToolExecutionContext) -> Option<String> {
    if context.session_id.is_some() || context.workspace_id.is_some() {
        return None;
    }
    Some(builtin_error(
        tool,
        "后台进程工具需要 session 或 workspace 上下文",
    ))
}

fn tail_utf8(bytes: &[u8], max_bytes: usize) -> String {
    let start = bytes.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&bytes[start..]).to_string()
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
            Err(error) => {
                return builtin_runtime_error(
                    "process_inspect",
                    PROCESS_INSPECT_PUBLIC_ERROR,
                    "查询进程列表失败",
                    error,
                );
            }
        }
    } else {
        match Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid=,ppid=,state=,comm="])
            .output()
        {
            Ok(output) => output.stdout,
            Err(error) => {
                return builtin_runtime_error(
                    "process_inspect",
                    PROCESS_INSPECT_PUBLIC_ERROR,
                    "查询进程信息失败",
                    error,
                );
            }
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
        (
            before_path,
            after_path,
            before,
            after,
            before_label,
            after_label,
        )
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
        Err(error) => return diff_preview_source_error(error),
    };
    let after_text = match read_diff_source(after_path, after) {
        Ok(text) => text,
        Err(error) => return diff_preview_source_error(error),
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
        "error_code": builtin_error_code(tool, "failed"),
        "error": message.into(),
    })
    .to_string()
}

fn builtin_filesystem_error(
    tool: &str,
    public_message: &'static str,
    action: &'static str,
    path: &Path,
    error: impl Display,
) -> String {
    tracing::warn!(
        tool,
        action,
        path = %path.display(),
        error = %error,
        "builtin filesystem operation failed"
    );
    builtin_error(tool, public_message)
}

fn builtin_runtime_error(
    tool: &str,
    public_message: &'static str,
    action: &'static str,
    error: impl Display,
) -> String {
    tracing::warn!(
        tool,
        action,
        error = %error,
        "builtin runtime operation failed"
    );
    builtin_error(tool, public_message)
}

fn builtin_path_resolution_error(tool: &str, requested_path: &str, error: impl Display) -> String {
    tracing::warn!(
        tool,
        requested_path,
        error = %error,
        "builtin path resolution failed"
    );
    builtin_error(tool, PATH_RESOLUTION_PUBLIC_ERROR)
}

fn builtin_rejected(tool: &str, message: impl Into<String>) -> String {
    serde_json::json!({
        "tool": tool,
        "status": "rejected",
        "error_code": builtin_error_code(tool, "rejected"),
        "error": message.into(),
    })
    .to_string()
}

fn builtin_error_code(tool: &str, suffix: &str) -> String {
    let tool = tool
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    format!("{tool}_{suffix}")
}

/// 协调器 / 长任务工具（agent_spawn 等）落到 BuiltinTool::execute 时
/// 必然是误调用——它们的语义需要 orchestration 层访问 task_store + spawn_graph +
/// conversation registry，远超 BuiltinTool trait 暴露的 ToolExecutionContext。
/// 真正的拦截点在 `crates/magi-conversation-runtime/src/tool_batch.rs::execute_task_tool_call`
/// （conversation runtime）。这里返回稳定错误码，避免把内部调用链或原始输入暴露到产品表面。
fn execute_orchestration_only(name: BuiltinToolName, _input: &str) -> String {
    serde_json::json!({
        "tool": name.as_str(),
        "status": "failed",
        "error_code": "orchestration_required",
        "error": "该协调工具需要由任务运行时处理，当前执行入口不可用",
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
    if cfg!(windows) { "/C" } else { "-lc" }
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

struct SearchTextFilesystemError {
    action: &'static str,
    path: PathBuf,
    source: std::io::Error,
}

fn search_text_matches(
    root: &Path,
    query: &str,
    case_sensitive: bool,
    include_hidden: bool,
    limit: usize,
) -> Result<(Vec<Value>, usize, bool), SearchTextFilesystemError> {
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
            Err(error) => {
                return Err(SearchTextFilesystemError {
                    action: "读取搜索路径元数据失败",
                    path,
                    source: error,
                });
            }
        };
        if metadata.is_dir() {
            let mut entries = match fs::read_dir(&path) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .collect::<Vec<_>>(),
                Err(error) => {
                    return Err(SearchTextFilesystemError {
                        action: "读取搜索目录失败",
                        path,
                        source: error,
                    });
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

enum DiffSourceReadError {
    Resolve(String),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn read_diff_source(
    path: Option<String>,
    inline: Option<String>,
) -> Result<String, DiffSourceReadError> {
    if let Some(inline) = inline {
        if !inline.is_empty() {
            return Ok(inline);
        }
    }
    if let Some(path) = path {
        let resolved = resolve_path(&path).map_err(DiffSourceReadError::Resolve)?;
        return fs::read_to_string(&resolved).map_err(|source| DiffSourceReadError::Read {
            path: resolved,
            source,
        });
    }
    Ok(String::new())
}

fn diff_preview_source_error(error: DiffSourceReadError) -> String {
    match error {
        DiffSourceReadError::Resolve(error) => builtin_runtime_error(
            "diff_preview",
            DIFF_PREVIEW_PUBLIC_ERROR,
            "解析差异预览源路径失败",
            error,
        ),
        DiffSourceReadError::Read { path, source } => builtin_filesystem_error(
            "diff_preview",
            DIFF_PREVIEW_PUBLIC_ERROR,
            "读取差异预览源文件失败",
            &path,
            source,
        ),
    }
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
    if query.is_some() { "query" } else { "pid" }
}

fn execute_file_write(input: &str, context: &ToolExecutionContext) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => {
            return builtin_error(
                "file_write",
                "输入必须为 JSON 对象，包含 path 和 content 字段",
            );
        }
    };

    let path_input = match field_string(&request, &["path", "file_path", "filePath"]) {
        Some(p) => p,
        None => return builtin_error("file_write", "缺少 path 字段"),
    };
    let content = match field_string(&request, &["content", "text", "data"]) {
        Some(c) => c,
        None => return builtin_error("file_write", "缺少 content 字段"),
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_write", &path_input, error);
        }
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(true);
    let create_dirs = field_bool(&request, &["create_dirs", "mkdir"]).unwrap_or(true);
    let existed_before = path.exists();

    if existed_before && !overwrite {
        tracing::warn!(
            tool = "file_write",
            path = %path.display(),
            "file_write target already exists and overwrite is disabled"
        );
        return builtin_error("file_write", PATH_ALREADY_EXISTS_PUBLIC_ERROR);
    }

    if create_dirs {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return builtin_filesystem_error(
                        "file_write",
                        DIRECTORY_CREATE_PUBLIC_ERROR,
                        "创建文件父目录失败",
                        parent,
                        e,
                    );
                }
            }
        }
    }

    let bytes = content.len();
    if let Err(e) = fs::write(&path, &content) {
        return builtin_filesystem_error(
            "file_write",
            FILE_WRITE_PUBLIC_ERROR,
            "写入文件失败",
            &path,
            e,
        );
    }

    serde_json::json!({
        "tool": "file_write",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": path.display().to_string(),
        "bytes_written": bytes,
        "created": !existed_before,
        "overwritten": existed_before,
        "summary": format!("已写入 {} ({} 字节)", path.display(), bytes)
    })
    .to_string()
}

fn execute_file_patch(input: &str, context: &ToolExecutionContext) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("file_patch", "输入必须为 JSON 对象"),
    };

    let path_input = match field_string(&request, &["path", "file_path", "filePath"]) {
        Some(p) => p,
        None => return builtin_error("file_patch", "缺少 path 字段"),
    };
    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_patch", &path_input, error);
        }
    };

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            return builtin_filesystem_error(
                "file_patch",
                FILE_READ_PUBLIC_ERROR,
                "读取待修改文件失败",
                &path,
                e,
            );
        }
    };

    let patches_from_array: Vec<(String, String)> = request
        .get("patches")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let old = p
                        .get("old_string")
                        .or_else(|| p.get("old"))
                        .and_then(Value::as_str)?;
                    let new = p
                        .get("new_string")
                        .or_else(|| p.get("new"))
                        .and_then(Value::as_str)?;
                    Some((old.to_string(), new.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let patches: Vec<(String, String)> = if !patches_from_array.is_empty() {
        patches_from_array
    } else if let (Some(old), Some(new)) = (
        field_string(&request, &["old_string", "old"]),
        field_string(&request, &["new_string", "new"]),
    ) {
        vec![(old, new)]
    } else {
        return builtin_error(
            "file_patch",
            "缺少 patches 数组或 old_string/new_string 字段",
        );
    };

    if patches.is_empty() {
        return builtin_error("file_patch", "patches 为空");
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
            errors.push(format!(
                "patch[{}]: old_string 匹配了 {} 处（需要唯一匹配）",
                i, count
            ));
            continue;
        }
        result = result.replacen(old, new, 1);
        applied += 1;
    }

    if applied == 0 {
        tracing::warn!(
            tool = "file_patch",
            path = %path.display(),
            total = patches.len(),
            errors = ?errors,
            "file_patch failed to apply any patch"
        );
        return serde_json::json!({
            "tool": "file_patch",
            "status": "failed",
            "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
            "error_code": builtin_error_code("file_patch", "failed"),
            "error": "所有 patch 均未能应用",
            "applied": 0,
            "total": patches.len(),
            "errors": errors,
        })
        .to_string();
    }

    if let Err(e) = fs::write(&path, &result) {
        return builtin_filesystem_error(
            "file_patch",
            FILE_WRITE_PUBLIC_ERROR,
            "写回修改文件失败",
            &path,
            e,
        );
    }

    serde_json::json!({
        "tool": "file_patch",
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

fn execute_file_remove(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let path_input = match required_string_or_raw(
        input,
        request.as_ref(),
        &["path", "file_path", "filePath"],
        "file_remove",
        "缺少文件路径",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_remove", &path_input, error);
        }
    };

    let recursive = request
        .as_ref()
        .and_then(|obj| field_bool(obj, &["recursive", "force"]))
        .unwrap_or(false);

    if !path.exists() {
        tracing::warn!(
            tool = "file_remove",
            path = %path.display(),
            "file_remove target path does not exist"
        );
        return builtin_error("file_remove", PATH_NOT_FOUND_PUBLIC_ERROR);
    }
    if let Some(reason) = protected_remove_target_reason(&path_input, &path, context) {
        return builtin_rejected("file_remove", reason);
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
        return builtin_filesystem_error(
            "file_remove",
            FILE_DELETE_PUBLIC_ERROR,
            "删除文件或目录失败",
            &path,
            e,
        );
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
) -> Option<String> {
    let trimmed = raw_input.trim();
    if matches!(trimmed, "/" | "." | ".." | "~") {
        tracing::warn!(
            tool = "file_remove",
            requested_path = if trimmed.is_empty() {
                "<empty>"
            } else {
                trimmed
            },
            "file_remove rejected protected raw path"
        );
        return Some(PROTECTED_DELETE_PUBLIC_ERROR.to_string());
    }

    let canonical_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if is_filesystem_root(&canonical_target) {
        tracing::warn!(
            tool = "file_remove",
            target = %canonical_target.display(),
            "file_remove rejected filesystem root"
        );
        return Some(PROTECTED_DELETE_PUBLIC_ERROR.to_string());
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
        return Some(PROTECTED_DELETE_PUBLIC_ERROR.to_string());
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
        return Some(PROTECTED_DELETE_PUBLIC_ERROR.to_string());
    }

    None
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn execute_file_mkdir(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let path_input = match required_string_or_raw(
        input,
        request.as_ref(),
        &["path", "file_path", "filePath", "dir_path", "dirPath"],
        "file_mkdir",
        "缺少目录路径",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_mkdir", &path_input, error);
        }
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
        tracing::warn!(
            tool = "file_mkdir",
            path = %path.display(),
            "file_mkdir target exists and is not a directory"
        );
        return builtin_error("file_mkdir", DIRECTORY_CREATE_PUBLIC_ERROR);
    }

    if let Err(e) = fs::create_dir_all(&path) {
        return builtin_filesystem_error(
            "file_mkdir",
            DIRECTORY_CREATE_PUBLIC_ERROR,
            "创建目录失败",
            &path,
            e,
        );
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

fn execute_file_copy(input: &str, context: &ToolExecutionContext) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => {
            return builtin_error(
                "file_copy",
                "输入必须为 JSON 对象，包含 source 和 destination 字段",
            );
        }
    };

    let src_input = match field_string(
        &request,
        &["source", "src", "from", "source_path", "sourcePath"],
    ) {
        Some(p) => p,
        None => return builtin_error("file_copy", "缺少 source 字段"),
    };
    let dst_input = match field_string(
        &request,
        &[
            "destination",
            "dst",
            "dest",
            "to",
            "destination_path",
            "destinationPath",
            "target_path",
            "targetPath",
        ],
    ) {
        Some(p) => p,
        None => return builtin_error("file_copy", "缺少 destination 字段"),
    };

    let src = match resolve_path_with_context(&src_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_copy", &src_input, error);
        }
    };
    let dst = match resolve_path_with_context(&dst_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_copy", &dst_input, error);
        }
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(false);

    if !src.exists() {
        tracing::warn!(
            tool = "file_copy",
            source = %src.display(),
            "file_copy source path does not exist"
        );
        return builtin_error("file_copy", FILE_COPY_PUBLIC_ERROR);
    }

    if dst.exists() && !overwrite {
        tracing::warn!(
            tool = "file_copy",
            destination = %dst.display(),
            "file_copy destination exists and overwrite is disabled"
        );
        return builtin_error("file_copy", PATH_ALREADY_EXISTS_PUBLIC_ERROR);
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return builtin_filesystem_error(
                    "file_copy",
                    DIRECTORY_CREATE_PUBLIC_ERROR,
                    "创建复制目标父目录失败",
                    parent,
                    e,
                );
            }
        }
    }

    if src.is_dir() {
        if let Err(e) = copy_dir_recursive(&src, &dst) {
            return builtin_filesystem_error(
                "file_copy",
                FILE_COPY_PUBLIC_ERROR,
                "复制目录失败",
                &src,
                e,
            );
        }
    } else {
        if let Err(e) = fs::copy(&src, &dst) {
            return builtin_filesystem_error(
                "file_copy",
                FILE_COPY_PUBLIC_ERROR,
                "复制文件失败",
                &src,
                e,
            );
        }
    }

    serde_json::json!({
        "tool": "file_copy",
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

fn execute_file_move(input: &str, context: &ToolExecutionContext) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => {
            return builtin_error(
                "file_move",
                "输入必须为 JSON 对象，包含 source 和 destination 字段",
            );
        }
    };

    let src_input = match field_string(
        &request,
        &["source", "src", "from", "source_path", "sourcePath"],
    ) {
        Some(p) => p,
        None => return builtin_error("file_move", "缺少 source 字段"),
    };
    let dst_input = match field_string(
        &request,
        &[
            "destination",
            "dst",
            "dest",
            "to",
            "destination_path",
            "destinationPath",
            "target_path",
            "targetPath",
        ],
    ) {
        Some(p) => p,
        None => return builtin_error("file_move", "缺少 destination 字段"),
    };

    let src = match resolve_path_with_context(&src_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_move", &src_input, error);
        }
    };
    let dst = match resolve_path_with_context(&dst_input, context) {
        Ok(p) => p,
        Err(error) => {
            return builtin_path_resolution_error("file_move", &dst_input, error);
        }
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(false);

    if !src.exists() {
        tracing::warn!(
            tool = "file_move",
            source = %src.display(),
            "file_move source path does not exist"
        );
        return builtin_error("file_move", FILE_MOVE_PUBLIC_ERROR);
    }

    if dst.exists() && !overwrite {
        tracing::warn!(
            tool = "file_move",
            destination = %dst.display(),
            "file_move destination exists and overwrite is disabled"
        );
        return builtin_error("file_move", PATH_ALREADY_EXISTS_PUBLIC_ERROR);
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return builtin_filesystem_error(
                    "file_move",
                    DIRECTORY_CREATE_PUBLIC_ERROR,
                    "创建移动目标父目录失败",
                    parent,
                    e,
                );
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
                return builtin_filesystem_error(
                    "file_move",
                    FILE_MOVE_PUBLIC_ERROR,
                    "移动目录失败",
                    &src,
                    e,
                );
            }
            if let Err(e) = fs::remove_dir_all(&src) {
                return builtin_filesystem_error(
                    "file_move",
                    FILE_DELETE_PUBLIC_ERROR,
                    "删除移动源目录失败",
                    &src,
                    e,
                );
            }
        } else {
            if let Err(e) = fs::copy(&src, &dst) {
                return builtin_filesystem_error(
                    "file_move",
                    FILE_MOVE_PUBLIC_ERROR,
                    "移动文件失败",
                    &src,
                    e,
                );
            }
            if let Err(e) = fs::remove_file(&src) {
                return builtin_filesystem_error(
                    "file_move",
                    FILE_DELETE_PUBLIC_ERROR,
                    "删除移动源文件失败",
                    &src,
                    e,
                );
            }
        }
    }

    serde_json::json!({
        "tool": "file_move",
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
    let query = match required_string_or_raw(
        input,
        request.as_ref(),
        &["query", "q", "search"],
        "web_search",
        "缺少搜索关键词 query",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let encoded = urlencoding::encode(&query);
    let search_url = format!("https://html.duckduckgo.com/html/?q={encoded}");

    let client = match reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return builtin_runtime_error(
                "web_search",
                WEB_SEARCH_PUBLIC_ERROR,
                "初始化 HTTP 客户端失败",
                e,
            );
        }
    };

    let response = match client.get(&search_url).send() {
        Ok(r) => r,
        Err(e) => {
            return builtin_runtime_error(
                "web_search",
                WEB_SEARCH_PUBLIC_ERROR,
                "发送搜索请求失败",
                e,
            );
        }
    };

    if !response.status().is_success() {
        return builtin_error(
            "web_search",
            format!("搜索服务返回 HTTP {}", response.status().as_u16()),
        );
    }

    let html = match response.text() {
        Ok(t) => t,
        Err(e) => {
            return builtin_runtime_error(
                "web_search",
                WEB_SEARCH_PUBLIC_ERROR,
                "读取搜索响应失败",
                e,
            );
        }
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
    let url = match required_string_or_raw(
        input,
        request.as_ref(),
        &["url", "href", "link"],
        "web_fetch",
        "缺少 URL",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let prompt = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["prompt"]));

    let client = match reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return builtin_runtime_error(
                "web_fetch",
                WEB_FETCH_PUBLIC_ERROR,
                "初始化 HTTP 客户端失败",
                e,
            );
        }
    };

    let response = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            return builtin_runtime_error(
                "web_fetch",
                WEB_FETCH_PUBLIC_ERROR,
                "发送网页请求失败",
                e,
            );
        }
    };

    if !response.status().is_success() {
        return builtin_error(
            "web_fetch",
            format!("网页返回 HTTP {}", response.status().as_u16()),
        );
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let body = match response.text() {
        Ok(t) => t,
        Err(e) => {
            return builtin_runtime_error(
                "web_fetch",
                WEB_FETCH_PUBLIC_ERROR,
                "读取网页响应体失败",
                e,
            );
        }
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
        (
            format!(
                "{}\n\n---\n*[内容已截断至 50,000 字符]*",
                &content[..max_len]
            ),
            true,
        )
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
    let mut cleaned = extract_main_content(html);
    for pattern in [
        r"(?si)<script[\s\S]*?</script>",
        r"(?si)<style[\s\S]*?</style>",
        r"(?si)<noscript[\s\S]*?</noscript>",
    ] {
        cleaned = regex::Regex::new(pattern)
            .unwrap()
            .replace_all(&cleaned, "")
            .to_string();
    }
    for tag in ["nav", "footer", "header", "aside", "iframe"] {
        let pattern = format!(r"(?si)<{tag}[^>]*>[\s\S]*?</{tag}>");
        cleaned = regex::Regex::new(&pattern)
            .unwrap()
            .replace_all(&cleaned, "")
            .to_string();
    }

    let mut md = cleaned;
    for level in 1..=6 {
        let pattern = format!(r"(?si)<h{level}[^>]*>([\s\S]*?)</h{level}>");
        md = regex::Regex::new(&pattern)
            .unwrap()
            .replace_all(&md, |caps: &regex::Captures| {
                let text = strip_html_tags(&decode_html_entities(&caps[1]));
                format!("\n{} {}\n", "#".repeat(level), text.trim())
            })
            .to_string();
    }
    let md = regex::Regex::new(r"(?si)<pre[^>]*>\s*<code[^>]*>([\s\S]*?)</code>\s*</pre>")
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("\n```\n{}\n```\n", decode_html_entities(&caps[1]).trim())
        });
    let md = regex::Regex::new(r"(?si)<code[^>]*>([\s\S]*?)</code>")
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!("`{}`", decode_html_entities(&caps[1]).trim())
        });
    let md = regex::Regex::new(r#"(?si)<a[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#)
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            let text = strip_html_tags(&decode_html_entities(&caps[2]));
            format!("[{}]({})", text.trim(), &caps[1])
        });
    let md = regex::Regex::new(r"(?si)<li[^>]*>([\s\S]*?)</li>")
        .unwrap()
        .replace_all(&md, |caps: &regex::Captures| {
            format!(
                "\n- {}",
                strip_html_tags(&decode_html_entities(&caps[1])).trim()
            )
        });
    let md = md
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let md = md.replace("</p>", "\n\n");
    let md = regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&md, "");
    let md = regex::Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&md, "\n\n");

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
// diagram.render — 统一图表渲染数据
// ══════════════════════════════════════════════════════════════════════════════

fn execute_diagram_render(input: &str) -> String {
    let request = match parse_json_object(input) {
        Some(obj) => obj,
        None => return builtin_error("diagram_render", "输入必须为 JSON 对象"),
    };

    let kind = match field_string(&request, &["kind"]) {
        Some(value) => normalize_diagram_kind(&value),
        None => None,
    };
    let kind = match kind {
        Some(value) => value,
        None => {
            return builtin_error(
                "diagram_render",
                "缺少或不支持的图表 kind，支持 mermaid、dot、graph、flow",
            );
        }
    };

    let title = field_string(&request, &["title"]);
    let theme = field_string(&request, &["theme"]).unwrap_or_else(|| "default".to_string());
    let layout = field_string(&request, &["layout"])
        .and_then(|value| normalize_diagram_layout(&value))
        .unwrap_or_else(|| "auto".to_string());
    let interactive =
        field_bool(&request, &["interactive"]).unwrap_or(matches!(kind, "graph" | "flow"));

    let mut payload = serde_json::json!({
        "tool": "diagram_render",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "type": "diagram_render",
        "kind": kind,
        "title": title,
        "theme": theme,
        "layout": layout,
        "interactive": interactive,
    });

    match kind {
        "mermaid" => {
            let source = match field_string(&request, &["source", "code"]) {
                Some(value) if !value.trim().is_empty() => value.trim().to_string(),
                _ => return builtin_error("diagram_render", "该图表源码格式需要 source 字段"),
            };
            let diagram_type = match detect_mermaid_kind_type(&source) {
                Some(value) => value,
                None => {
                    return builtin_error(
                        "diagram_render",
                        "无法识别的图表类型。source 须包含有效声明（graph, flowchart, sequenceDiagram, classDiagram 等）",
                    );
                }
            };
            if diagram_type == "mindmap" {
                return builtin_error(
                    "diagram_render",
                    "思维导图必须使用 kind=flow 或 kind=graph 的 graph.nodes/edges 结构化输入；mindmap 不是当前产品展示面",
                );
            }
            payload["source"] = serde_json::json!(source);
            payload["diagram_type"] = serde_json::json!(diagram_type);
            payload["summary"] = serde_json::json!(format!("已生成图表数据（{}）", diagram_type));
        }
        "dot" => {
            let source = match field_string(&request, &["source", "code"]) {
                Some(value) if !value.trim().is_empty() => value.trim().to_string(),
                _ => return builtin_error("diagram_render", "该图表源码格式需要 source 字段"),
            };
            if !is_dot_source(&source) {
                return builtin_error(
                    "diagram_render",
                    "DOT 源码须以 graph、digraph、strict graph 或 strict digraph 开头",
                );
            }
            payload["source"] = serde_json::json!(source);
            payload["diagram_type"] = serde_json::json!("dot");
            payload["summary"] = serde_json::json!("已生成图表数据");
        }
        "graph" | "flow" => {
            let graph = match request.get("graph") {
                Some(value) if validate_graph_payload(value) => value.clone(),
                _ => {
                    return builtin_error(
                        "diagram_render",
                        format!("kind={} 需要 graph.nodes 和 graph.edges 数组", kind),
                    );
                }
            };
            let node_count = graph
                .get("nodes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let edge_count = graph
                .get("edges")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            payload["graph"] = graph;
            payload["diagram_type"] = serde_json::json!(kind);
            payload["summary"] = serde_json::json!(format!(
                "已生成图表数据（{} 个节点，{} 条边）",
                node_count, edge_count
            ));
        }
        _ => unreachable!("normalize_diagram_kind only returns supported kinds"),
    }

    payload.to_string()
}

fn normalize_diagram_kind(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "mermaid" => Some("mermaid"),
        "dot" => Some("dot"),
        "graph" => Some("graph"),
        "flow" => Some("flow"),
        _ => None,
    }
}

fn normalize_diagram_layout(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" | "dagre" | "elk" | "tidy-tree" | "cose" | "force" | "fcose" | "cose-bilkent"
        | "grid" | "circle" | "preset" => Some(value.trim().to_ascii_lowercase()),
        _ => None,
    }
}

fn detect_mermaid_kind_type(source: &str) -> Option<&'static str> {
    let trimmed = strip_mermaid_frontmatter(source).trim_start();
    let diagram_types: &[(&str, &str)] = &[
        ("graph ", "flowchart"),
        ("flowchart ", "flowchart"),
        ("sequenceDiagram", "sequence"),
        ("classDiagram", "class"),
        ("stateDiagram", "state"),
        ("erDiagram", "er"),
        ("gantt", "gantt"),
        ("pie", "pie"),
        ("journey", "journey"),
        ("gitGraph", "git"),
        ("mindmap", "mindmap"),
        ("timeline", "timeline"),
        ("quadrantChart", "quadrant"),
        ("requirementDiagram", "requirement"),
        ("C4Context", "c4"),
        ("sankey", "sankey"),
        ("xychart", "xychart"),
        ("block-beta", "block"),
    ];

    diagram_types
        .iter()
        .find(|(prefix, _)| trimmed.to_lowercase().starts_with(&prefix.to_lowercase()))
        .map(|(_, diagram_type)| *diagram_type)
}

fn strip_mermaid_frontmatter(source: &str) -> &str {
    let trimmed = source.trim_start();
    let Some(after_open) = trimmed.strip_prefix("---") else {
        return source;
    };
    let after_open = after_open.trim_start_matches(['\r', '\n']);
    if let Some(close_index) = after_open.find("\n---") {
        let after_close = &after_open[close_index + "\n---".len()..];
        return after_close.trim_start_matches(['\r', '\n']);
    }
    source
}

fn is_dot_source(source: &str) -> bool {
    let lower = source.trim_start().to_ascii_lowercase();
    lower.starts_with("graph ")
        || lower.starts_with("graph{")
        || lower.starts_with("digraph ")
        || lower.starts_with("digraph{")
        || lower.starts_with("strict graph ")
        || lower.starts_with("strict graph{")
        || lower.starts_with("strict digraph ")
        || lower.starts_with("strict digraph{")
}

fn validate_graph_payload(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("nodes").and_then(Value::as_array).is_some()
        && object.get("edges").and_then(Value::as_array).is_some()
}

// ══════════════════════════════════════════════════════════════════════════════
// search.semantic — 基于关键词拆分的语义代码检索
// ══════════════════════════════════════════════════════════════════════════════

/// 代码符号导航：按符号名查定义 / 列出文件符号。基于本地索引引擎的符号表。
fn execute_code_symbols(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let action = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["action"]))
        .unwrap_or_default();

    let Some(store) = resources.knowledge_store.as_ref() else {
        return builtin_error("code_symbols", "代码索引引擎不可用");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return builtin_error("code_symbols", "缺少 workspace 上下文，无法查询符号");
    };

    let symbol_to_json = |s: &magi_knowledge_store::symbol_index::SymbolEntry| {
        serde_json::json!({
            "name": s.name,
            "kind": format!("{:?}", s.kind),
            "path": s.file_path,
            "line": s.line,
            "endLine": s.end_line,
            "exported": s.is_exported,
            "container": s.container,
            "signature": s.signature,
        })
    };

    match action.as_str() {
        "definition" | "goto_definition" => {
            let Some(name) = request
                .as_ref()
                .and_then(|obj| field_string(obj, &["name", "symbol", "query"]))
            else {
                return builtin_error("code_symbols", "action=definition 需要 name 字段");
            };
            let limit = request
                .as_ref()
                .and_then(|obj| field_usize(obj, &["limit", "max_results"]))
                .unwrap_or(20)
                .clamp(1, 100);
            let Some(symbols) = store.find_symbol_definitions(workspace_id, &name, limit) else {
                return builtin_error("code_symbols", "代码索引引擎未就绪");
            };
            let results: Vec<Value> = symbols.iter().map(symbol_to_json).collect();
            serde_json::json!({
                "tool": "code_symbols",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
                "action": "definition",
                "name": name,
                "returned_matches": results.len(),
                "results": results,
                "summary": format!("符号 \"{}\" 找到 {} 处定义", name, results.len())
            })
            .to_string()
        }
        "file_symbols" | "list_file_symbols" => {
            let Some(path) = request.as_ref().and_then(|obj| {
                field_string(obj, &["path", "file", "file_path", "filePath", "filepath"])
            }) else {
                return builtin_error("code_symbols", "action=file_symbols 需要 path 字段");
            };
            let path = normalize_code_symbols_file_path(&path, context);
            let Some(symbols) = store.list_file_symbols(workspace_id, &path) else {
                return builtin_error("code_symbols", "代码索引引擎未就绪");
            };
            let results: Vec<Value> = symbols.iter().map(symbol_to_json).collect();
            serde_json::json!({
                "tool": "code_symbols",
                "status": "succeeded",
                "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
                "action": "file_symbols",
                "path": path,
                "returned_matches": results.len(),
                "results": results,
                "summary": format!("文件 \"{}\" 含 {} 个符号", path, results.len())
            })
            .to_string()
        }
        other => builtin_error(
            "code_symbols",
            format!("未知 action：{other}（支持 definition / file_symbols）"),
        ),
    }
}

fn normalize_code_symbols_file_path(path: &str, context: &ToolExecutionContext) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let input_path = Path::new(trimmed);
    let relative = if input_path.is_absolute() {
        context
            .working_directory
            .as_ref()
            .and_then(|workspace_root| {
                input_path
                    .strip_prefix(workspace_root)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .or_else(|| {
                let canonical_input = input_path.canonicalize().ok()?;
                let canonical_root = context.working_directory.as_ref()?.canonicalize().ok()?;
                canonical_input
                    .strip_prefix(canonical_root)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .unwrap_or_else(|| input_path.to_path_buf())
    } else {
        input_path.to_path_buf()
    };

    relative.to_string_lossy().replace('\\', "/")
}

fn execute_search_semantic(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let query = match required_string_or_raw(
        input,
        request.as_ref(),
        &["query"],
        "search_semantic",
        "缺少 query 字段",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let limit = request
        .as_ref()
        .and_then(|obj| field_usize(obj, &["limit", "max_results"]))
        .unwrap_or(10)
        .clamp(1, 50);

    let Some(store) = resources.knowledge_store.as_ref() else {
        return builtin_error("search_semantic", "代码索引引擎不可用");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return builtin_error("search_semantic", "缺少 workspace 上下文，无法查询代码索引");
    };
    let Some(engine_results) = store.search_workspace_code(
        workspace_id,
        &query,
        magi_knowledge_store::local_search_engine::SearchOptions {
            max_results: Some(limit),
            ..Default::default()
        },
    ) else {
        return builtin_error("search_semantic", "代码索引引擎未就绪");
    };

    let results: Vec<Value> = engine_results
        .iter()
        .map(|result| {
            let primary_snippet = result.snippets.first();
            let matched_keywords = primary_snippet
                .map(|snippet| snippet.matched_tokens.clone())
                .unwrap_or_default();
            serde_json::json!({
                "path": &result.file_path,
                "score": format!("{:.2}", result.score),
                "source": "engine",
                "matched_keywords": matched_keywords,
                "snippet": primary_snippet.map(|snippet| snippet.content.as_str()).unwrap_or_default(),
                "snippets": &result.snippets,
                "score_breakdown": &result.score_breakdown,
            })
        })
        .collect();

    serde_json::json!({
        "tool": "search_semantic",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "query": query,
        "engine": "local_search_engine",
        "workspace_id": workspace_id.as_str(),
        "returned_matches": results.len(),
        "results": results,
        "summary": format!("本地代码索引检索 \"{}\" 返回 {} 个匹配", query, results.len())
    })
    .to_string()
}

// ══════════════════════════════════════════════════════════════════════════════
// knowledge.query — 当前工作区知识库检索
// ══════════════════════════════════════════════════════════════════════════════

fn execute_knowledge_query(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = parse_json_object(input);
    let query = match required_string_or_raw(
        input,
        request.as_ref(),
        &["query", "q"],
        "knowledge_query",
        "缺少 query 字段",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let Some(store) = resources.knowledge_store.as_ref() else {
        return builtin_error("knowledge_query", "知识库不可用");
    };
    let Some(workspace_id) = context.workspace_id.as_ref() else {
        return builtin_error(
            "knowledge_query",
            "缺少 workspace 上下文，无法查询项目知识库",
        );
    };

    let kind = match parse_knowledge_kind(request.as_ref()) {
        Ok(kind) => kind,
        Err(error) => return error,
    };
    let tags = parse_knowledge_tags(request.as_ref());
    let limit = request
        .as_ref()
        .and_then(|obj| field_usize(obj, &["limit", "max_results"]))
        .unwrap_or(10)
        .clamp(1, 50);

    let knowledge_query = magi_knowledge_store::KnowledgeQuery {
        kind,
        text: Some(query.clone()),
        tags: tags.clone(),
        workspace_id: Some(workspace_id.clone()),
        limit,
    };
    let governed_query = store.governed_query(&knowledge_query);
    let results: Vec<Value> = governed_query
        .results
        .iter()
        .map(|item| {
            serde_json::json!({
                "knowledge_id": &item.knowledge_id,
                "title": &item.title,
                "kind": knowledge_kind_label(item.kind),
                "excerpt": &item.excerpt,
                "updated_at": item.updated_at,
                "score": item.score,
                "matched_terms": &item.matched_terms,
                "source_ref": item.source_ref.as_deref(),
                "code_source": item.code_source.as_ref(),
                "audit_link": item.audit_link.as_ref(),
                "governance_link": item.governance_link.as_ref(),
            })
        })
        .collect();

    serde_json::json!({
        "tool": "knowledge_query",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "workspace_id": workspace_id.as_str(),
        "query": query,
        "kind": kind.map(knowledge_kind_label).unwrap_or("all"),
        "tags": tags,
        "limit": limit,
        "total_matches": governed_query.total_matches,
        "returned_matches": results.len(),
        "truncated": governed_query.truncated,
        "results": results,
        "summary": format!("在当前工作区知识库中搜索 \"{}\"，返回 {} 个匹配项", query, results.len())
    })
    .to_string()
}

fn parse_knowledge_kind(
    request: Option<&serde_json::Map<String, Value>>,
) -> Result<Option<magi_knowledge_store::KnowledgeKind>, String> {
    let Some(raw_kind) = request.and_then(|obj| field_string(obj, &["kind"])) else {
        return Ok(None);
    };
    let normalized = raw_kind
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_");
    match normalized.as_str() {
        "" | "all" => Ok(None),
        "adr" | "architecture_decision" | "architecture_decision_record" => {
            Ok(Some(magi_knowledge_store::KnowledgeKind::Adr))
        }
        "faq" => Ok(Some(magi_knowledge_store::KnowledgeKind::Faq)),
        "learning" => Ok(Some(magi_knowledge_store::KnowledgeKind::Learning)),
        "code_index" | "codeindex" => Ok(Some(magi_knowledge_store::KnowledgeKind::CodeIndex)),
        other => Err(builtin_error(
            "knowledge_query",
            format!("未知 kind：{other}（支持 all / adr / faq / learning / code_index）"),
        )),
    }
}

fn parse_knowledge_tags(request: Option<&serde_json::Map<String, Value>>) -> Vec<String> {
    let Some(value) = request.and_then(|obj| obj.get("tags")) else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(value) => value
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn knowledge_kind_label(kind: magi_knowledge_store::KnowledgeKind) -> &'static str {
    match kind {
        magi_knowledge_store::KnowledgeKind::Adr => "adr",
        magi_knowledge_store::KnowledgeKind::Faq => "faq",
        magi_knowledge_store::KnowledgeKind::Learning => "learning",
        magi_knowledge_store::KnowledgeKind::CodeIndex => "code_index",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_resolution_failure_uses_public_message() {
        let output = builtin_path_resolution_error(
            "file_read",
            "/private/workspace/secret.txt",
            "无法解析当前目录: No such file or directory (os error 2)",
        );
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error_code"], "file_read_failed");
        assert_eq!(payload["error"], PATH_RESOLUTION_PUBLIC_ERROR);
        assert!(!output.contains("/private/workspace/secret.txt"));
        assert!(!output.contains("No such file"));
        assert!(!output.contains("os error"));
    }
}
