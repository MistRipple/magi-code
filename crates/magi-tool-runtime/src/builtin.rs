use crate::{
    BuiltinTool, BuiltinToolAccessMode, BuiltinToolName, BuiltinToolSpec, ToolExecutionContext,
    ToolExecutionContextQuery, ToolRuntimeResources,
};
use magi_core::{ApprovalRequirement, ExecutionResultStatus, RiskLevel, UtcMillis};
use serde_json::Value;
use std::{
    collections::HashMap,
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
            BuiltinToolName::FileWrite => execute_file_write(input, context),
            BuiltinToolName::FilePatch => execute_file_patch(input, context),
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
            BuiltinToolName::KnowledgeQuery => execute_knowledge_query(input),
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
        &["path", "file_path"],
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

    let (matches, scanned_files, truncated) =
        match search_text_matches(&root, &query, case_sensitive, include_hidden, limit) {
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

fn execute_shell_exec(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
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
    let cwd_input = request
        .as_ref()
        .and_then(|object| field_string(object, &["cwd", "working_directory", "workdir"]));
    let cwd = match cwd_input {
        Some(value) => match resolve_path_with_context(&value, context) {
            Ok(path) => path,
            Err(error) => return builtin_error("shell_exec", error),
        },
        None => match context_working_directory(context) {
            Ok(path) => path,
            Err(error) => return builtin_error("shell_exec", error),
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
            Err(error) => return builtin_error("shell_exec", format!("命令执行失败: {error}")),
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
            Err(error) => return builtin_error(surface_tool, error),
        },
        None => match context_working_directory(context) {
            Ok(path) => path,
            Err(error) => return builtin_error(surface_tool, error),
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
        Err(error) => return builtin_error(surface_tool, format!("命令启动失败: {error}")),
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
    let request = match parse_json_object(input) {
        Some(request) => request,
        None => return builtin_error("process_read", "输入必须为 JSON 对象，包含 terminal_id"),
    };
    let Some(terminal_id) = field_usize(&request, &["terminal_id", "terminalId", "id"]) else {
        return builtin_error("process_read", "缺少 terminal_id");
    };
    if let Some(error) = require_process_context("process_read", context) {
        return error;
    }
    let max_bytes = field_usize(&request, &["max_bytes", "preview_bytes", "limit"])
        .unwrap_or(12_000)
        .clamp(512, 200_000);

    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return builtin_error("process_read", format!("进程不存在: {terminal_id}"));
    };
    if !process_belongs_to_context(process, context) {
        return builtin_error("process_read", "进程不属于当前 session/workspace");
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

    serde_json::json!({
        "tool": "process_read",
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
    })
    .to_string()
}

fn execute_process_write(input: &str, context: &ToolExecutionContext) -> String {
    let request = match parse_json_object(input) {
        Some(request) => request,
        None => return builtin_error("process_write", "输入必须为 JSON 对象，包含 terminal_id"),
    };
    let Some(terminal_id) = field_usize(&request, &["terminal_id", "terminalId", "id"]) else {
        return builtin_error("process_write", "缺少 terminal_id");
    };
    if let Some(error) = require_process_context("process_write", context) {
        return error;
    }
    let content = field_string(&request, &["input", "content", "text"]).unwrap_or_default();
    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return builtin_error("process_write", format!("进程不存在: {terminal_id}"));
    };
    if !process_belongs_to_context(process, context) {
        return builtin_error("process_write", "进程不属于当前 session/workspace");
    }
    let Some(stdin) = process.child.stdin.as_mut() else {
        return builtin_error("process_write", format!("进程 #{terminal_id} 不接受输入"));
    };
    if let Err(error) = stdin.write_all(content.as_bytes()) {
        return builtin_error("process_write", format!("写入进程失败: {error}"));
    }
    let _ = stdin.flush();
    serde_json::json!({
        "tool": "process_write",
        "status": "succeeded",
        "terminal_id": terminal_id,
        "written_bytes": content.len(),
        "summary": format!("已写入进程 #{terminal_id}")
    })
    .to_string()
}

fn execute_process_kill(input: &str, context: &ToolExecutionContext) -> String {
    let request = match parse_json_object(input) {
        Some(request) => request,
        None => return builtin_error("process_kill", "输入必须为 JSON 对象，包含 terminal_id"),
    };
    let Some(terminal_id) = field_usize(&request, &["terminal_id", "terminalId", "id"]) else {
        return builtin_error("process_kill", "缺少 terminal_id");
    };
    if let Some(error) = require_process_context("process_kill", context) {
        return error;
    }
    let mut table = PROCESS_TABLE.lock().expect("process table lock poisoned");
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return builtin_error("process_kill", format!("进程不存在: {terminal_id}"));
    };
    if !process_belongs_to_context(process, context) {
        return builtin_error("process_kill", "进程不属于当前 session/workspace");
    }
    let _ = process.child.kill();
    let _ = process.child.wait();
    table.remove(&(terminal_id as u64));
    serde_json::json!({
        "tool": "process_kill",
        "status": "succeeded",
        "terminal_id": terminal_id,
        "summary": format!("已停止进程 #{terminal_id}")
    })
    .to_string()
}

fn execute_process_list(context: &ToolExecutionContext) -> String {
    if let Some(error) = require_process_context("process_list", context) {
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
    serde_json::json!({
        "tool": "process_list",
        "status": "succeeded",
        "processes": processes,
        "summary": "已列出当前上下文后台进程"
    })
    .to_string()
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
                return builtin_error("process_inspect", format!("进程检查失败: {error}"));
            }
        }
    } else {
        match Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid=,ppid=,state=,comm="])
            .output()
        {
            Ok(output) => output.stdout,
            Err(error) => {
                return builtin_error("process_inspect", format!("进程检查失败: {error}"));
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

/// 协调器 / 长任务工具（agent_spawn 等）落到 BuiltinTool::execute 时
/// 必然是误调用——它们的语义需要 orchestration 层访问 task_store + spawn_graph +
/// conversation registry，远超 BuiltinTool trait 暴露的 ToolExecutionContext。
/// 真正的拦截点在 `crates/magi-conversation-runtime/src/tool_batch.rs::execute_task_tool_call`
/// （conversation runtime）。这里返回一个明确的 `orchestration_required` 状态，
/// 避免悄无声息地把它当成无副作用工具执行。
fn execute_orchestration_only(name: BuiltinToolName, input: &str) -> String {
    serde_json::json!({
        "tool": name.as_str(),
        "status": "failed",
        "error": "coordinator tool reached the builtin executor; this must be intercepted at the orchestration layer (see execute_task_tool_call)",
        "input_echo": input,
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
    if let Some(inline) = inline {
        if !inline.is_empty() {
            return Ok(inline);
        }
    }
    if let Some(path) = path {
        let resolved = resolve_path(&path)?;
        return fs::read_to_string(&resolved)
            .map_err(|error| format!("读取 diff 源失败 {}: {error}", resolved.display()));
    }
    Ok(String::new())
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

    let path_input = match field_string(&request, &["path", "file_path"]) {
        Some(p) => p,
        None => return builtin_error("file_write", "缺少 path 字段"),
    };
    let content = match field_string(&request, &["content", "text", "data"]) {
        Some(c) => c,
        None => return builtin_error("file_write", "缺少 content 字段"),
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_write", e),
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(true);
    let create_dirs = field_bool(&request, &["create_dirs", "mkdir"]).unwrap_or(true);
    let existed_before = path.exists();

    if existed_before && !overwrite {
        return builtin_error(
            "file_write",
            format!("文件已存在且 overwrite=false: {}", path.display()),
        );
    }

    if create_dirs {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return builtin_error("file_write", format!("创建父目录失败: {e}"));
                }
            }
        }
    }

    let bytes = content.len();
    if let Err(e) = fs::write(&path, &content) {
        return builtin_error("file_write", format!("写入文件失败: {e}"));
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

    let path_input = match field_string(&request, &["path", "file_path"]) {
        Some(p) => p,
        None => return builtin_error("file_patch", "缺少 path 字段"),
    };
    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_patch", e),
    };

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return builtin_error("file_patch", format!("读取文件失败: {e}")),
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
        return serde_json::json!({
            "tool": "file_patch",
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
        return builtin_error("file_patch", format!("写回文件失败: {e}"));
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
        &["path", "file_path"],
        "file_remove",
        "缺少文件路径",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_remove", e),
    };

    let recursive = request
        .as_ref()
        .and_then(|obj| field_bool(obj, &["recursive", "force"]))
        .unwrap_or(false);

    if !path.exists() {
        return builtin_error("file_remove", format!("路径不存在: {}", path.display()));
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
        return builtin_error("file_remove", format!("删除失败: {e}"));
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

fn execute_file_mkdir(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let path_input = match required_string_or_raw(
        input,
        request.as_ref(),
        &["path", "dir_path"],
        "file_mkdir",
        "缺少目录路径",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let path = match resolve_path_with_context(&path_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_mkdir", e),
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
        return builtin_error(
            "file_mkdir",
            format!("路径已存在且不是目录: {}", path.display()),
        );
    }

    if let Err(e) = fs::create_dir_all(&path) {
        return builtin_error("file_mkdir", format!("创建目录失败: {e}"));
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

    let src_input = match field_string(&request, &["source", "src", "from"]) {
        Some(p) => p,
        None => return builtin_error("file_copy", "缺少 source 字段"),
    };
    let dst_input = match field_string(&request, &["destination", "dst", "dest", "to"]) {
        Some(p) => p,
        None => return builtin_error("file_copy", "缺少 destination 字段"),
    };

    let src = match resolve_path_with_context(&src_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_copy", e),
    };
    let dst = match resolve_path_with_context(&dst_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_copy", e),
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(false);

    if !src.exists() {
        return builtin_error("file_copy", format!("源路径不存在: {}", src.display()));
    }

    if dst.exists() && !overwrite {
        return builtin_error(
            "file_copy",
            format!("目标路径已存在且 overwrite=false: {}", dst.display()),
        );
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return builtin_error("file_copy", format!("创建目标父目录失败: {e}"));
            }
        }
    }

    if src.is_dir() {
        if let Err(e) = copy_dir_recursive(&src, &dst) {
            return builtin_error("file_copy", format!("复制目录失败: {e}"));
        }
    } else {
        if let Err(e) = fs::copy(&src, &dst) {
            return builtin_error("file_copy", format!("复制文件失败: {e}"));
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

    let src_input = match field_string(&request, &["source", "src", "from"]) {
        Some(p) => p,
        None => return builtin_error("file_move", "缺少 source 字段"),
    };
    let dst_input = match field_string(&request, &["destination", "dst", "dest", "to"]) {
        Some(p) => p,
        None => return builtin_error("file_move", "缺少 destination 字段"),
    };

    let src = match resolve_path_with_context(&src_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_move", e),
    };
    let dst = match resolve_path_with_context(&dst_input, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("file_move", e),
    };

    let overwrite = field_bool(&request, &["overwrite", "force"]).unwrap_or(false);

    if !src.exists() {
        return builtin_error("file_move", format!("源路径不存在: {}", src.display()));
    }

    if dst.exists() && !overwrite {
        return builtin_error(
            "file_move",
            format!("目标路径已存在且 overwrite=false: {}", dst.display()),
        );
    }

    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return builtin_error("file_move", format!("创建目标父目录失败: {e}"));
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
                return builtin_error("file_move", format!("跨设备移动目录失败: {e}"));
            }
            if let Err(e) = fs::remove_dir_all(&src) {
                return builtin_error("file_move", format!("删除源目录失败: {e}"));
            }
        } else {
            if let Err(e) = fs::copy(&src, &dst) {
                return builtin_error("file_move", format!("跨设备移动文件失败: {e}"));
            }
            if let Err(e) = fs::remove_file(&src) {
                return builtin_error("file_move", format!("删除源文件失败: {e}"));
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

/// 从自然语言查询中抽取检索关键词：小写化、按非标识符字符切分、过滤停用词。
/// 引擎融合路径与关键词遍历兜底路径共用，避免两处各写一份停用词表。
fn extract_query_keywords(query: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "can", "shall",
        "must", "need", "dare", "to", "of", "in", "for", "on", "with", "at", "by", "from", "as",
        "into", "through", "during", "before", "after", "above", "below", "between", "out", "off",
        "over", "under", "again", "further", "then", "once", "here", "there", "when", "where",
        "why", "how", "all", "each", "every", "both", "few", "more", "most", "other", "some",
        "such", "no", "not", "only", "own", "same", "so", "than", "too", "very", "just", "and",
        "but", "or", "nor", "if", "that", "this", "what", "which", "who", "whom", "whose", "it",
        "its", "i", "me", "my", "we", "our", "you", "your", "he", "she", "they", "them", "his",
        "her", "their", "find", "search", "look", "show", "get", "code", "function", "file",
    ];
    let stop: std::collections::HashSet<&str> = STOP_WORDS.iter().cloned().collect();
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2 && !stop.contains(w))
        .map(|w| w.to_string())
        .collect()
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

    let root = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["root", "dir", "directory"]))
        .unwrap_or_else(|| {
            context_working_directory(context)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });
    let limit = request
        .as_ref()
        .and_then(|obj| field_usize(obj, &["limit", "max_results"]))
        .unwrap_or(10)
        .clamp(1, 50);

    // 优先走本地代码检索引擎（LocalSearchEngine：TF-IDF + 符号索引 + 依赖图
    // 两路融合检索（对齐原版 CodebaseRetrievalService 的分层思路）：
    // - 引擎路：LocalSearchEngine（TF-IDF + 符号索引 + 依赖图 + 多信号排序），
    //   等价于原版 L1（语义）+ L3（符号），Rust 版已融为一路；
    // - grep 路：search_text 精确正则匹配，等价于原版 L2。
    // 引擎结果优先，grep 结果按文件去重后补充未覆盖文件。引擎不可用时整体回落
    // 到下方关键词遍历（过渡兜底）。
    if let (Some(store), Some(workspace_id)) = (
        resources.knowledge_store.as_ref(),
        context.workspace_id.as_ref(),
    ) && let Some(engine_results) = store.search_workspace_code(
        workspace_id,
        &query,
        magi_knowledge_store::local_search_engine::SearchOptions {
            max_results: Some(limit),
            ..Default::default()
        },
    ) && !engine_results.is_empty()
    {
        let mut covered_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for r in &engine_results {
            covered_files.insert(r.file_path.clone());
            let snippet = r
                .snippets
                .first()
                .map(|s| s.content.clone())
                .unwrap_or_default();
            let matched: Vec<String> = r
                .snippets
                .first()
                .map(|s| s.matched_tokens.clone())
                .unwrap_or_default();
            results.push(serde_json::json!({
                "path": r.file_path,
                "score": format!("{:.2}", r.score),
                "source": "engine",
                "matched_keywords": matched,
                "snippet": snippet,
            }));
        }

        // grep 补充：对查询里的关键词做精确匹配，补进引擎未覆盖的文件。
        let mut grep_hits = 0usize;
        if let Ok(root_path) = resolve_path_with_context(&root, context) {
            let keywords = extract_query_keywords(&query);
            if let Some(primary) = keywords.first() {
                if let Ok((matches, _scanned, _truncated)) =
                    search_text_matches(&root_path, primary, false, false, limit)
                {
                    for m in matches {
                        let path = m
                            .get("path")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                            .unwrap_or_default();
                        // 引擎返回相对路径、grep 返回绝对路径，用后缀包含判定去重。
                        if path.is_empty()
                            || covered_files.iter().any(|c| path.ends_with(c.as_str()))
                        {
                            continue;
                        }
                        covered_files.insert(path.clone());
                        let line_text = m
                            .get("line")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        results.push(serde_json::json!({
                            "path": path,
                            "source": "grep",
                            "matched_keywords": [primary],
                            "snippet": line_text,
                        }));
                        grep_hits += 1;
                        if results.len() >= limit + limit / 2 {
                            break;
                        }
                    }
                }
            }
        }

        return serde_json::json!({
            "tool": "search_semantic",
            "status": "succeeded",
            "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
            "query": query,
            "engine": "local_search_engine+grep",
            "returned_matches": results.len(),
            "results": results,
            "summary": format!(
                "本地代码检索 \"{}\" 返回 {} 个匹配（引擎 {} + grep 补充 {}）",
                query,
                results.len(),
                engine_results.len(),
                grep_hits
            )
        })
        .to_string();
    }

    let root_path = match resolve_path_with_context(&root, context) {
        Ok(p) => p,
        Err(e) => return builtin_error("search_semantic", e),
    };

    // 将查询拆分为关键词（小写化），过滤停用词
    let keywords: Vec<String> = extract_query_keywords(&query);

    if keywords.is_empty() {
        return builtin_error("search_semantic", "查询关键词过于宽泛，请提供更具体的描述");
    }

    // 代码文件扩展名
    let code_extensions: std::collections::HashSet<&str> = [
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "cpp", "h", "hpp", "cs", "rb",
        "swift", "kt", "scala", "lua", "sh", "bash", "zsh", "sql", "toml", "yaml", "yml", "json",
        "xml", "html", "css", "scss", "svelte", "vue", "md", "txt",
    ]
    .iter()
    .cloned()
    .collect();

    let mut scored_results: Vec<(f64, String, String, Vec<String>)> = Vec::new();
    let mut scanned = 0usize;
    semantic_walk_dir(
        &root_path,
        &code_extensions,
        &keywords,
        &mut scored_results,
        &mut scanned,
        5000,
    );

    // 按分数降序排序
    scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored_results.truncate(limit);

    let results: Vec<Value> = scored_results
        .iter()
        .map(|(score, path, snippet, matched_kw)| {
            serde_json::json!({
                "path": path,
                "score": format!("{:.2}", score),
                "matched_keywords": matched_kw,
                "snippet": snippet,
            })
        })
        .collect();

    serde_json::json!({
        "tool": "search_semantic",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "query": query,
        "keywords": keywords,
        "scanned_files": scanned,
        "returned_matches": results.len(),
        "results": results,
        "summary": format!("语义搜索 \"{}\" 扫描了 {} 个文件，返回 {} 个匹配", query, scanned, results.len())
    })
    .to_string()
}

fn semantic_walk_dir(
    dir: &Path,
    code_extensions: &std::collections::HashSet<&str>,
    keywords: &[String],
    results: &mut Vec<(f64, String, String, Vec<String>)>,
    scanned: &mut usize,
    max_files: usize,
) {
    if *scanned >= max_files {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut dirs = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        if *scanned >= max_files {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 跳过隐藏目录和常见忽略目录
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
            || name == "__pycache__"
            || name == "vendor"
            || name == ".git"
        {
            continue;
        }
        if path.is_dir() {
            dirs.push(path);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if code_extensions.contains(ext) {
                *scanned += 1;
                if let Ok(content) = fs::read_to_string(&path) {
                    let (score, matched, snippet) = score_file_content(&content, &name, keywords);
                    if score > 0.0 {
                        results.push((score, path.display().to_string(), snippet, matched));
                    }
                }
            }
        }
    }
    for subdir in dirs {
        semantic_walk_dir(
            &subdir,
            code_extensions,
            keywords,
            results,
            scanned,
            max_files,
        );
    }
}

fn score_file_content(
    content: &str,
    filename: &str,
    keywords: &[String],
) -> (f64, Vec<String>, String) {
    let content_lower = content.to_lowercase();
    let filename_lower = filename.to_lowercase();
    let mut score = 0.0f64;
    let mut matched_keywords = Vec::new();
    let mut best_line_score = 0.0f64;
    let mut best_line_idx = 0usize;

    for kw in keywords {
        // 文件名匹配权重更高
        if filename_lower.contains(kw) {
            score += 5.0;
            if !matched_keywords.contains(kw) {
                matched_keywords.push(kw.clone());
            }
        }
        // 内容匹配
        let count = content_lower.matches(kw.as_str()).count();
        if count > 0 {
            score += (count as f64).min(10.0);
            if !matched_keywords.contains(kw) {
                matched_keywords.push(kw.clone());
            }
        }
    }

    // 匹配关键词覆盖率加权
    let coverage = matched_keywords.len() as f64 / keywords.len() as f64;
    score *= coverage;

    // 找到匹配度最高的行区域用于 snippet
    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let line_lower = line.to_lowercase();
        let line_score: f64 = keywords
            .iter()
            .filter(|kw| line_lower.contains(kw.as_str()))
            .count() as f64;
        if line_score > best_line_score {
            best_line_score = line_score;
            best_line_idx = i;
        }
    }

    // 提取 snippet: 最佳匹配行 ±2 行
    let start = best_line_idx.saturating_sub(2);
    let end = (best_line_idx + 3).min(lines.len());
    let snippet = lines[start..end].join("\n");
    // 截断过长 snippet
    let snippet = if snippet.len() > 500 {
        let mut end = 500;
        while !snippet.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &snippet[..end])
    } else {
        snippet
    };

    (score, matched_keywords, snippet)
}

// ══════════════════════════════════════════════════════════════════════════════
// knowledge.query — 项目文档知识检索
// ══════════════════════════════════════════════════════════════════════════════

fn execute_knowledge_query(input: &str) -> String {
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
    let category = request
        .as_ref()
        .and_then(|obj| field_string(obj, &["category"]))
        .unwrap_or_else(|| "all".to_string());

    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 收集项目文档文件
    let mut doc_files: Vec<PathBuf> = Vec::new();
    let doc_patterns: &[&str] = match category.as_str() {
        "readme" => &["README.md", "README", "README.txt", "README.rst"],
        "docs" => &[], // 只扫描 docs/ 目录
        "code" => &[], // 只扫描代码注释
        _ => &[
            "README.md",
            "README",
            "README.txt",
            "CLAUDE.md",
            "CONTRIBUTING.md",
            "CHANGELOG.md",
            "LICENSE",
            "Cargo.toml",
            "package.json",
        ],
    };

    // 添加根目录下的文档文件
    for pattern in doc_patterns {
        let path = root.join(pattern);
        if path.exists() && path.is_file() {
            doc_files.push(path);
        }
    }

    // 添加 .claude/ 目录下的文档
    if category == "all" || category == "docs" {
        let claude_dir = root.join(".claude");
        if claude_dir.exists() {
            if let Ok(entries) = fs::read_dir(&claude_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("md") {
                        doc_files.push(path);
                    }
                }
            }
        }
    }

    // 扫描 docs/ 目录
    if category == "all" || category == "docs" {
        let docs_dir = root.join("docs");
        if docs_dir.exists() {
            collect_doc_files(&docs_dir, &mut doc_files, 3);
        }
        let doc_dir = root.join("doc");
        if doc_dir.exists() {
            collect_doc_files(&doc_dir, &mut doc_files, 3);
        }
    }

    // 关键词拆分
    let keywords: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_string())
        .collect();

    if keywords.is_empty() {
        return builtin_error("knowledge_query", "查询关键词为空");
    }

    // 搜索文档并提取匹配段落
    let mut results: Vec<Value> = Vec::new();
    for doc_path in &doc_files {
        if let Ok(content) = fs::read_to_string(doc_path) {
            let sections = extract_matching_sections(&content, &keywords);
            if !sections.is_empty() {
                let rel_path = doc_path
                    .strip_prefix(&root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| doc_path.display().to_string());
                for section in sections {
                    results.push(serde_json::json!({
                        "source": rel_path,
                        "heading": section.0,
                        "content": section.1,
                        "relevance": section.2,
                    }));
                }
            }
        }
    }

    // 按相关度排序
    results.sort_by(|a, b| {
        let ra = a["relevance"].as_f64().unwrap_or(0.0);
        let rb = b["relevance"].as_f64().unwrap_or(0.0);
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(10);

    serde_json::json!({
        "tool": "knowledge_query",
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "query": query,
        "category": category,
        "scanned_docs": doc_files.len(),
        "returned_sections": results.len(),
        "results": results,
        "summary": format!("在 {} 个文档中搜索 \"{}\"，返回 {} 个匹配段落", doc_files.len(), query, results.len())
    })
    .to_string()
}

fn collect_doc_files(dir: &Path, files: &mut Vec<PathBuf>, max_depth: usize) {
    if max_depth == 0 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_doc_files(&path, files, max_depth - 1);
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "md" | "txt" | "rst" | "adoc") {
                files.push(path);
            }
        }
    }
}

/// 从文档内容中提取与关键词匹配的段落
fn extract_matching_sections(content: &str, keywords: &[String]) -> Vec<(String, String, f64)> {
    let mut sections = Vec::new();
    let mut current_heading = String::new();
    let mut current_body = String::new();

    for line in content.lines() {
        if line.starts_with('#') {
            // 保存上一个段落
            if !current_body.is_empty() {
                let score = section_relevance(&current_heading, &current_body, keywords);
                if score > 0.0 {
                    let body = truncate_section(&current_body, 600);
                    sections.push((current_heading.clone(), body, score));
                }
            }
            current_heading = line.trim_start_matches('#').trim().to_string();
            current_body.clear();
        } else {
            if !current_body.is_empty() || !line.trim().is_empty() {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }
    }
    // 最后一个段落
    if !current_body.is_empty() {
        let score = section_relevance(&current_heading, &current_body, keywords);
        if score > 0.0 {
            let body = truncate_section(&current_body, 600);
            sections.push((current_heading, body, score));
        }
    }
    // 如果没有 markdown 标题，整体匹配
    if sections.is_empty() && !content.is_empty() {
        let score = section_relevance("", content, keywords);
        if score > 0.0 {
            let body = truncate_section(content, 600);
            sections.push(("(document)".to_string(), body, score));
        }
    }
    sections
}

fn section_relevance(heading: &str, body: &str, keywords: &[String]) -> f64 {
    let combined = format!("{}\n{}", heading, body).to_lowercase();
    let mut matched = 0usize;
    let mut score = 0.0f64;
    for kw in keywords {
        let count = combined.matches(kw.as_str()).count();
        if count > 0 {
            matched += 1;
            score += (count as f64).min(5.0);
            // 标题匹配加权
            if heading.to_lowercase().contains(kw.as_str()) {
                score += 3.0;
            }
        }
    }
    if matched == 0 {
        return 0.0;
    }
    score * (matched as f64 / keywords.len() as f64)
}

fn truncate_section(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.trim().to_string();
    }
    let mut end = max_chars;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end].trim())
}
