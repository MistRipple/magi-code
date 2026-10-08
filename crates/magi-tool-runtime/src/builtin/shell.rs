use crate::{BuiltinToolAccessMode, ToolExecutionContext, ToolExecutionProgress};

use magi_core::ToolCallId;

use magi_process::{ManagedChild, spawn_managed, std_command};

use serde_json::Value;

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self},
    time::{Duration, Instant},
};

use super::{
    context_working_directory,
    failure::{invalid_input, path_resolution_failure},
    field_bool, field_string, field_usize, parse_json_object,
    process::ProcessExecutionScope,
    process::{
        execute_process_kill_with_surface, execute_process_launch_with_surface,
        execute_process_list_with_surface, execute_process_read_with_surface,
        execute_process_write_with_surface,
    },
    required_string_field, resolve_path_with_context,
};
use magi_core::ToolFailure;

// Shell 命令可能包含构建、测试或用户明确要求的等待。默认超时必须覆盖常见
// 的长命令，不能把没有输出但仍在正常运行的命令误判成失败。
pub(super) const DEFAULT_SHELL_TIMEOUT_MS: u64 = 300_000;

pub(super) const MIN_SHELL_TIMEOUT_MS: u64 = 1_000;

pub(super) const MAX_SHELL_TIMEOUT_MS: u64 = 1_800_000;

pub(super) const SHELL_TIMEOUT_POLL_MS: u64 = 20;

/// 主进程结束后等待输出管道关闭的上限。超过它说明有后台进程继承了管道。
pub(super) const SHELL_PIPE_DRAIN_GRACE_MS: u64 = 1_500;

pub(super) const SHELL_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

pub(super) const SHELL_PROGRESS_INTERVAL_MS: u64 = 250;

#[derive(Clone)]
pub(super) struct ActiveShellExec {
    pub(super) execution_id: u64,
    pub(super) scope: ProcessExecutionScope,
    pub(super) child: Arc<Mutex<ManagedChild>>,
    pub(super) cancelled: Arc<AtomicBool>,
}

pub(super) struct ShellExecOutput {
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    timed_out: bool,
    cancelled: bool,
    /// 主进程退出后仍有后台进程占着输出管道，已被连同进程组一起终止。
    background_pipe_holders_terminated: bool,
}

#[derive(Clone, Default)]
pub(super) struct ShellProgressState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    revision: u64,
    finished: bool,
}

#[derive(Clone, Copy)]
pub(super) enum ShellProgressStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShellCommandSpec {
    pub(super) program: String,
    pub(super) arguments: Vec<String>,
}

#[cfg(test)]
pub(crate) struct ShellPipeOutput {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

pub(super) static SHELL_EXECUTION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) static ACTIVE_SHELL_EXECUTIONS: LazyLock<Mutex<HashMap<u64, ActiveShellExec>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 解析后的 Shell 调用环境：工作目录与启动 Shell。前台执行与后台启动共用同一份解析。
pub(super) struct ShellInvocation {
    pub(super) cwd: PathBuf,
    pub(super) shell: ShellCommandSpec,
}

pub(super) fn requested_access_mode(
    request: Option<&serde_json::Map<String, Value>>,
) -> BuiltinToolAccessMode {
    request
        .and_then(|object| {
            field_string(object, "access_mode")
                .and_then(|value| BuiltinToolAccessMode::from_str(&value))
        })
        .unwrap_or(BuiltinToolAccessMode::MaybeWrite)
}

/// 解析 `cwd` 与 `shell`。失败时返回可直接作为工具结果的失败载荷。
pub(super) fn resolve_shell_invocation(
    tool: &str,
    request: Option<&serde_json::Map<String, Value>>,
    context: &ToolExecutionContext,
) -> Result<ShellInvocation, String> {
    let cwd = match request.and_then(|object| field_string(object, "cwd")) {
        Some(value) => resolve_path_with_context(&value, context)
            .map_err(|error| path_resolution_failure(tool, &value, &error))?,
        None => context_working_directory(context)
            .map_err(|error| path_resolution_failure(tool, "<context>", &error))?,
    };
    if !cwd.is_dir() {
        return Err(ToolFailure::new(
            tool,
            "working_directory_unavailable",
            "工作目录不存在或不是目录",
        )
        .instruction(
            "确认 cwd 是否正确（可用 search_text(target=path) 查找文件）；默认工作区目录不可用时，告知用户重新选择工作区。",
        )
        .with("cwd", cwd.display().to_string())
        .into_payload());
    }
    let configured_shell = request
        .and_then(|object| field_string(object, "shell"))
        .filter(|value| !value.trim().is_empty());
    let shell = resolve_shell_command_spec(configured_shell).map_err(|error| {
        ToolFailure::new(tool, "invalid_shell", format!("Shell 参数无效：{error}"))
            .instruction("省略 shell 参数使用默认 Shell，或改成受支持的 Shell。")
            .into_payload()
    })?;
    Ok(ShellInvocation { cwd, shell })
}

/// 命令依赖的可执行文件在当前环境找不到时，不启动命令，直接说明缺什么。
pub(super) fn missing_executables_failure(
    tool: &str,
    command: &str,
    invocation: &ShellInvocation,
    access_mode: BuiltinToolAccessMode,
) -> Option<String> {
    let required_executables =
        crate::policy::shell_command_required_executables(command, &invocation.shell.program);
    let find_missing = || {
        required_executables
            .iter()
            .filter(|executable| magi_process::resolve_executable(executable).is_none())
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut missing_executables = find_missing();
    if !missing_executables.is_empty() {
        // 用户可能在 Magi 运行期间安装了命令或修改了 Shell PATH；刷新只重建环境快照，
        // 不会执行原命令，因此不会把一次失败变成隐式重复执行。
        magi_process::refresh_user_process_environment();
        missing_executables = find_missing();
    }
    if missing_executables.is_empty() {
        return None;
    }
    let guidance = missing_executable_guidance(&missing_executables);
    Some(
        ToolFailure::new(
            tool,
            "command_not_found",
            format!("当前执行环境找不到命令：{}", missing_executables.join(", ")),
        )
        .instruction(
            guidance.unwrap_or("安装缺失的命令，或改用其他方式完成；不要重复执行同一条命令。"),
        )
        .with("command", command)
        .with("cwd", invocation.cwd.display().to_string())
        .with("access_mode", access_mode.as_str())
        .with("missing_executables", missing_executables)
        .with("suggested_tool", guidance.map(|_| "search_text"))
        .with("summary", guidance.unwrap_or("命令依赖不可用"))
        .into_payload(),
    )
}

pub(super) fn execute_shell_exec(
    input: &str,
    context: &ToolExecutionContext,
    progress: Option<(&ToolCallId, &(dyn Fn(ToolExecutionProgress) + Sync))>,
) -> String {
    let request = parse_json_object(input);
    if let Some(payload) = execute_shell_exec_background_action(input, request.as_ref(), context) {
        return payload;
    }
    let command = match required_string_field(
        request.as_ref(),
        "command",
        "shell_exec",
        "缺少 shell 命令；管理后台进程时设置 action=read/write/kill/list 并提供 terminal_id",
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if request
        .as_ref()
        .and_then(|object| field_bool(object, "background"))
        .unwrap_or(false)
    {
        return execute_process_launch_with_surface(
            input,
            context,
            "shell_exec",
            Some("background"),
        );
    }
    let requested_access_mode = requested_access_mode(request.as_ref());
    let read_only_declaration_is_valid =
        magi_permissions::PermissionEngine::shell_arguments_request_read_only(input);
    if requested_access_mode == BuiltinToolAccessMode::ReadOnly
        && !read_only_declaration_is_valid
        && context.access_profile != magi_core::AccessProfile::FullAccess
    {
        return ToolFailure::new(
            "shell_exec",
            "read_only_violation",
            "声明 access_mode=read_only 的命令不能包含写入迹象",
        )
        .rejected()
        .instruction(
            "命令确实需要写入时，把 access_mode 改为 maybe_write 或 explicit_write；只是探查时，去掉重定向和写入类命令。",
        )
        .into_payload();
    }
    let access_mode = if requested_access_mode == BuiltinToolAccessMode::ReadOnly
        && !read_only_declaration_is_valid
    {
        BuiltinToolAccessMode::MaybeWrite
    } else {
        requested_access_mode
    };
    let invocation = match resolve_shell_invocation("shell_exec", request.as_ref(), context) {
        Ok(invocation) => invocation,
        Err(failure) => return failure,
    };
    let timeout_ms = request
        .as_ref()
        .and_then(|object| field_usize(object, "timeout_ms"))
        .map(|value| value as u64)
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_MS)
        .clamp(MIN_SHELL_TIMEOUT_MS, MAX_SHELL_TIMEOUT_MS);
    if let Some(payload) =
        non_git_read_only_probe_payload(&command, &invocation.cwd, access_mode, timeout_ms)
    {
        return payload;
    }
    if let Some(failure) =
        missing_executables_failure("shell_exec", &command, &invocation, access_mode)
    {
        return failure;
    }
    let ShellInvocation { cwd, shell } = invocation;

    let output = match execute_shell_command_with_timeout(
        &shell, &command, &cwd, timeout_ms, context, progress,
    ) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(tool = "shell_exec", %error, "启动 shell 命令失败");
            return ToolFailure::new("shell_exec", "spawn_failed", "shell 命令启动失败")
                .instruction(
                    "原因已记录到日志；检查 shell 与 cwd 是否可用，不要用相同参数重复调用。",
                )
                .into_payload();
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
    let missing_executable = shell_missing_executable(exit_code, &stderr);
    let summary = if output.timed_out {
        format!("命令执行超时({timeout_ms}ms): {command}")
    } else if output.cancelled {
        format!("命令已取消: {command}")
    } else if succeeded {
        format!("命令执行成功: {command}")
    } else if let Some(executable) = missing_executable.as_deref() {
        format!("命令依赖不可用（{executable}）: {command}")
    } else {
        format!("命令执行失败(退出码 {:?}): {command}", exit_code)
    };

    let mut payload = serde_json::json!({
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
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
        "summary": summary,
    });
    if output.background_pipe_holders_terminated {
        payload["background_processes_terminated"] = Value::Bool(true);
        payload["hint"] = Value::String(
            "命令结束后仍有后台进程占用输出，已连同进程组一起终止。需要长期运行的服务请使用 shell_exec(background=true) 启动。".to_string(),
        );
    }
    if let Some((error_code, error, instruction)) = shell_failure_description(
        &output,
        succeeded,
        exit_code,
        timeout_ms,
        missing_executable.as_deref(),
    ) {
        payload["error_code"] = Value::String(error_code.to_string());
        payload["error"] = Value::String(error);
        payload["instruction"] = Value::String(instruction.to_string());
    }
    if let Some(executable) = missing_executable {
        payload["missing_executable"] = Value::String(executable);
    }
    payload.to_string()
}

/// 命令没有成功时的失败说明：超时、缺命令、非零退出是三种不同的下一步。
/// 用户主动取消不是失败，不返回说明。
fn shell_failure_description(
    output: &ShellExecOutput,
    succeeded: bool,
    exit_code: Option<i32>,
    timeout_ms: u64,
    missing_executable: Option<&str>,
) -> Option<(&'static str, String, &'static str)> {
    if output.cancelled || succeeded {
        return None;
    }
    if output.timed_out {
        return Some((
            "shell_exec_timeout",
            format!("命令在 {timeout_ms}ms 内没有结束，已被终止"),
            "命令是长时间运行的服务或监听进程时，用 background=true 启动，再用 action=read 查看输出；否则调大 timeout_ms（最大 1800000）或把任务拆成更小的步骤。不要用相同参数重复执行。",
        ));
    }
    if let Some(executable) = missing_executable {
        return Some((
            "shell_exec_command_not_found",
            format!("当前执行环境找不到命令：{executable}"),
            "安装缺失的命令，或改用其他方式完成；不要重复执行同一条命令。",
        ));
    }
    let error = match exit_code {
        Some(code) => format!("命令以退出码 {code} 结束"),
        None => "命令被信号终止".to_string(),
    };
    Some((
        "shell_exec_nonzero_exit",
        error,
        "根据 stdout / stderr 里的报错修改命令或先满足前置条件；相同命令不会得到不同结果，不要原样重试。",
    ))
}

pub(super) fn missing_executable_guidance(missing_executables: &[String]) -> Option<&'static str> {
    if missing_executables.iter().any(|executable| {
        executable.eq_ignore_ascii_case("rg") || executable.eq_ignore_ascii_case("grep")
    }) {
        return Some(
            "文本搜索请改用 search_text 或 search_semantic；Magi 不假设用户系统已安装 rg/grep。若用户明确要求该命令，请先用 command -v 确认可用性。",
        );
    }
    None
}

pub(super) fn shell_missing_executable(exit_code: Option<i32>, stderr: &str) -> Option<String> {
    if !matches!(exit_code, Some(127 | 9009))
        && !stderr.contains("command not found")
        && !stderr.contains("not recognized as an internal or external command")
        && !stderr.contains("is not recognized as the name of a cmdlet")
    {
        return None;
    }
    stderr.lines().rev().find_map(executable_from_shell_error)
}

pub(super) fn executable_from_shell_error(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some((_, executable)) = trimmed.rsplit_once("command not found:") {
        return normalized_executable_name(executable);
    }
    if let Some((prefix, _)) = trimmed.rsplit_once(": command not found") {
        return normalized_executable_name(prefix.rsplit(':').next().unwrap_or(prefix));
    }
    if let Some((prefix, _)) = trimmed.rsplit_once(": not found") {
        return normalized_executable_name(prefix.rsplit(':').next().unwrap_or(prefix));
    }
    if trimmed.contains("not recognized as an internal or external command") {
        return trimmed
            .split('\'')
            .nth(1)
            .and_then(normalized_executable_name);
    }
    if trimmed.contains("is not recognized as the name of a cmdlet") {
        return trimmed
            .split('\'')
            .nth(1)
            .and_then(normalized_executable_name);
    }
    None
}

pub(super) fn normalized_executable_name(value: &str) -> Option<String> {
    let candidate = value.trim().trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '\'' | '"' | '`')
    });
    if candidate.is_empty()
        || !candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._+-/\\".contains(character))
    {
        return None;
    }
    Some(candidate.to_string())
}

pub(super) fn execute_shell_exec_background_action(
    input: &str,
    request: Option<&serde_json::Map<String, Value>>,
    context: &ToolExecutionContext,
) -> Option<String> {
    let request = request?;
    match field_string(request, "action")?.as_str() {
        "run" => None,
        "read" => Some(execute_process_read_with_surface(
            input,
            context,
            "shell_exec",
            Some("background_read"),
        )),
        "write" => Some(execute_process_write_with_surface(
            input,
            context,
            "shell_exec",
            Some("background_write"),
        )),
        "kill" => Some(execute_process_kill_with_surface(
            input,
            context,
            "shell_exec",
            Some("background_kill"),
        )),
        "list" => Some(execute_process_list_with_surface(
            context,
            "shell_exec",
            Some("background_list"),
        )),
        other => Some(invalid_input(
            "shell_exec",
            format!("未知后台进程动作: {other}（支持 run / read / write / kill / list）"),
        )),
    }
}

pub(super) fn non_git_read_only_probe_payload(
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

pub(super) fn git_probe_target(command: &str, cwd: &Path) -> Option<PathBuf> {
    simple_git_probe_target(command, cwd).or_else(|| compound_git_probe_target(command, cwd))
}

pub(super) fn simple_git_probe_target(command: &str, cwd: &Path) -> Option<PathBuf> {
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

pub(super) fn compound_git_probe_target(command: &str, cwd: &Path) -> Option<PathBuf> {
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

pub(super) fn clean_shell_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '\'' | '"' | '(' | ')' | '{' | '}' | '[' | ']' | ';' | '&' | '|'
            )
        })
        .to_string()
}

pub(super) fn resolve_git_probe_path(path: &str, cwd: &Path) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

pub(super) fn is_git_worktree(path: &Path) -> bool {
    let path_arg = path.to_string_lossy().to_string();
    std_command("git")
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

pub(super) fn execute_shell_command_with_timeout(
    shell: &ShellCommandSpec,
    command: &str,
    cwd: &Path,
    timeout_ms: u64,
    context: &ToolExecutionContext,
    progress: Option<(&ToolCallId, &(dyn Fn(ToolExecutionProgress) + Sync))>,
) -> Result<ShellExecOutput, String> {
    let mut command_builder = std_command(&shell.program);
    command_builder
        .args(&shell.arguments)
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_managed(&mut command_builder).map_err(|error| error.to_string())?;

    let stdout = child.take_stdout();
    let stderr = child.take_stderr();
    let child = Arc::new(Mutex::new(child));
    let (execution_id, cancellation_requested) = register_active_shell_exec(context, &child);
    let progress_state = Arc::new(Mutex::new(ShellProgressState::default()));
    let started_at = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let mut cancelled = false;
    // 读取线程不放进 scope：脱离进程组的后代可能永远不关闭管道，此时只能分离
    // 读取线程而不能无限等待它。
    let stdout_state = Arc::clone(&progress_state);
    let stdout_reader = thread::spawn(move || {
        read_child_pipe_with_progress(stdout, ShellProgressStream::Stdout, stdout_state)
    });
    let stderr_state = Arc::clone(&progress_state);
    let stderr_reader = thread::spawn(move || {
        read_child_pipe_with_progress(stderr, ShellProgressStream::Stderr, stderr_state)
    });
    let mut background_pipe_holders_terminated = false;
    let status = thread::scope(|scope| {
        let progress_publisher = progress.map(|(tool_call_id, on_progress)| {
            let publisher_state = Arc::clone(&progress_state);
            scope.spawn(move || publish_shell_progress(tool_call_id, on_progress, publisher_state))
        });
        let status = loop {
            let wait_state = {
                child
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .try_wait()
                    .map_err(|error| error.to_string())?
            };
            match wait_state {
                Some(status) => {
                    if cancellation_requested.load(Ordering::SeqCst)
                        || !active_shell_exec_is_registered(execution_id)
                    {
                        cancelled = true;
                    }
                    break Some(status);
                }
                None if started_at.elapsed() >= timeout => {
                    timed_out = true;
                    break terminate_shell_child(&child);
                }
                None if cancellation_requested.load(Ordering::SeqCst)
                    || !active_shell_exec_is_registered(execution_id) =>
                {
                    cancelled = true;
                    break terminate_shell_child(&child);
                }
                None => thread::sleep(Duration::from_millis(SHELL_TIMEOUT_POLL_MS)),
            }
        };
        let readers = [stdout_reader, stderr_reader];
        if !wait_for_shell_pipe_readers(&readers) {
            // 主进程已结束但管道仍被后台进程持有（例如 `npm run dev &`）：终止整个
            // 进程组让管道关闭，避免工具调用和整轮对话被无限挂住。
            background_pipe_holders_terminated = true;
            let _ = terminate_shell_child(&child);
            if !wait_for_shell_pipe_readers(&readers) {
                tracing::warn!(
                    "shell 后台进程脱离进程组后仍持有输出管道，读取线程将随其退出而结束"
                );
            }
        }
        for reader in readers {
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
        progress_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finished = true;
        if let Some(progress_publisher) = progress_publisher {
            let _ = progress_publisher.join();
        }
        Ok::<_, String>(status)
    })?;
    unregister_active_shell_exec(execution_id);
    let progress_state = progress_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    Ok(ShellExecOutput {
        status,
        stdout: progress_state.stdout,
        stderr: progress_state.stderr,
        stdout_truncated: progress_state.stdout_truncated,
        stderr_truncated: progress_state.stderr_truncated,
        timed_out,
        cancelled,
        background_pipe_holders_terminated,
    })
}

/// 在宽限期内等待输出读取线程结束；返回 false 表示管道仍被其他进程持有。
pub(super) fn wait_for_shell_pipe_readers(readers: &[thread::JoinHandle<()>]) -> bool {
    let deadline = Instant::now() + Duration::from_millis(SHELL_PIPE_DRAIN_GRACE_MS);
    loop {
        if readers.iter().all(thread::JoinHandle::is_finished) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(SHELL_TIMEOUT_POLL_MS));
    }
}

pub(super) fn read_child_pipe_with_progress<T: Read>(
    pipe: Option<T>,
    stream: ShellProgressStream,
    state: Arc<Mutex<ShellProgressState>>,
) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut chunk = [0_u8; 8192];
    loop {
        let size = match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(size) => size,
        };
        {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match stream {
                ShellProgressStream::Stdout => {
                    state.stdout.extend_from_slice(&chunk[..size]);
                    if state.stdout.len() > SHELL_OUTPUT_MAX_BYTES {
                        state.stdout_truncated = true;
                        let excess = state.stdout.len() - SHELL_OUTPUT_MAX_BYTES;
                        state.stdout.drain(..excess);
                    }
                }
                ShellProgressStream::Stderr => {
                    state.stderr.extend_from_slice(&chunk[..size]);
                    if state.stderr.len() > SHELL_OUTPUT_MAX_BYTES {
                        state.stderr_truncated = true;
                        let excess = state.stderr.len() - SHELL_OUTPUT_MAX_BYTES;
                        state.stderr.drain(..excess);
                    }
                }
            }
            state.revision = state.revision.saturating_add(1);
        }
    }
}

pub(super) fn publish_shell_progress(
    tool_call_id: &ToolCallId,
    on_progress: &(dyn Fn(ToolExecutionProgress) + Sync),
    state: Arc<Mutex<ShellProgressState>>,
) {
    let interval = Duration::from_millis(SHELL_PROGRESS_INTERVAL_MS);
    let mut published_revision = 0_u64;
    let mut last_published_at = Instant::now() - interval;
    loop {
        let snapshot = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let has_new_output = snapshot.revision > published_revision;
        if has_new_output
            && (published_revision == 0
                || last_published_at.elapsed() >= interval
                || snapshot.finished)
        {
            on_progress(ToolExecutionProgress {
                tool_call_id: tool_call_id.clone(),
                tool_name: "shell_exec".to_string(),
                payload: serde_json::json!({
                    "tool": "shell_exec",
                    "status": "running",
                    "stdout": String::from_utf8_lossy(&snapshot.stdout),
                    "stderr": String::from_utf8_lossy(&snapshot.stderr),
                    "stdout_truncated": snapshot.stdout_truncated,
                    "stderr_truncated": snapshot.stderr_truncated,
                    "summary": "命令正在执行",
                })
                .to_string(),
            });
            published_revision = snapshot.revision;
            last_published_at = Instant::now();
        }
        if snapshot.finished && published_revision >= snapshot.revision {
            break;
        }
        thread::sleep(Duration::from_millis(SHELL_TIMEOUT_POLL_MS));
    }
}

pub(super) fn register_active_shell_exec(
    context: &ToolExecutionContext,
    child: &Arc<Mutex<ManagedChild>>,
) -> (u64, Arc<AtomicBool>) {
    let execution_id = SHELL_EXECUTION_COUNTER.fetch_add(1, Ordering::SeqCst);
    let cancelled = Arc::new(AtomicBool::new(false));
    let process = ActiveShellExec {
        execution_id,
        scope: ProcessExecutionScope::from_context(context),
        child: Arc::clone(child),
        cancelled: Arc::clone(&cancelled),
    };
    ACTIVE_SHELL_EXECUTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(execution_id, process);
    (execution_id, cancelled)
}

pub(super) fn unregister_active_shell_exec(execution_id: u64) {
    ACTIVE_SHELL_EXECUTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&execution_id);
}

pub(super) fn active_shell_exec_is_registered(execution_id: u64) -> bool {
    ACTIVE_SHELL_EXECUTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(&execution_id)
}

pub(super) fn terminate_shell_child(child: &Arc<Mutex<ManagedChild>>) -> Option<ExitStatus> {
    let mut child = child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match child.terminate() {
        Ok(status) => Some(status),
        Err(error) => {
            tracing::warn!(%error, "终止同步 Shell 进程树失败");
            None
        }
    }
}

#[cfg(test)]
pub(crate) fn read_child_pipe<T: Read>(pipe: Option<T>) -> ShellPipeOutput {
    let Some(mut pipe) = pipe else {
        return ShellPipeOutput {
            bytes: Vec::new(),
            truncated: false,
        };
    };
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let size = match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(size) => size,
        };
        bytes.extend_from_slice(&chunk[..size]);
        if bytes.len() > SHELL_OUTPUT_MAX_BYTES {
            truncated = true;
            let excess = bytes.len() - SHELL_OUTPUT_MAX_BYTES;
            bytes.drain(..excess);
        }
    }
    ShellPipeOutput { bytes, truncated }
}

pub(super) fn resolve_shell_command_spec(
    configured_shell: Option<String>,
) -> Result<ShellCommandSpec, String> {
    match configured_shell {
        Some(shell) => {
            let spec = parse_shell_command_spec(&shell)?;
            if cfg!(windows) && !is_powershell_shell(&spec.program) {
                return Err("Windows 仅支持 PowerShell（powershell.exe 或 pwsh）".to_string());
            }
            Ok(spec)
        }
        None => {
            let program = magi_process::user_shell().to_string_lossy().to_string();
            Ok(default_shell_command_spec(program))
        }
    }
}

pub(super) fn default_shell_command_spec(program: String) -> ShellCommandSpec {
    let arguments = if cfg!(windows) && is_powershell_shell(&program) {
        vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
        ]
    } else {
        vec![shell_arg(&program).to_string()]
    };
    ShellCommandSpec { arguments, program }
}

pub(super) fn parse_shell_command_spec(shell: &str) -> Result<ShellCommandSpec, String> {
    let tokens = tokenize_shell_command_spec(shell)?;
    let Some(program) = tokens.first().cloned() else {
        return Err("Shell 程序不能为空".to_string());
    };
    let mut arguments = tokens.into_iter().skip(1).collect::<Vec<_>>();
    if !has_shell_command_argument(&program, &arguments) {
        arguments.push(shell_arg(&program).to_string());
    }
    Ok(ShellCommandSpec { program, arguments })
}

pub(super) fn has_shell_command_argument(program: &str, arguments: &[String]) -> bool {
    if is_powershell_shell(program) {
        return arguments.iter().any(|argument| {
            argument.eq_ignore_ascii_case("-command") || argument.eq_ignore_ascii_case("-c")
        });
    }
    arguments.iter().any(|argument| {
        argument == "-c"
            || argument
                .strip_prefix('-')
                .is_some_and(|flags| flags.contains('c'))
    })
}

pub(super) fn is_powershell_shell(program: &str) -> bool {
    let executable = shell_executable_name(program);
    executable.contains("powershell") || matches!(executable.as_str(), "pwsh" | "pwsh.exe")
}

pub(super) fn tokenize_shell_command_spec(shell: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;

    for character in shell.trim().chars() {
        match quote {
            Some(active_quote) if character == active_quote => quote = None,
            Some(_) => current.push(character),
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }

    if quote.is_some() {
        return Err("Shell 程序包含未闭合引号".to_string());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

pub(super) fn shell_arg(shell: &str) -> &'static str {
    if is_powershell_shell(shell) {
        "-Command"
    } else {
        "-c"
    }
}

pub(super) fn shell_executable_name(shell: &str) -> String {
    shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_argument_matches_selected_shell_dialect() {
        assert_eq!(
            shell_arg(r#"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"#),
            "-Command"
        );
        assert_eq!(shell_arg("pwsh"), "-Command");
        assert_eq!(shell_arg("/bin/zsh"), "-c");
    }

    #[test]
    fn missing_text_search_command_provides_native_tool_guidance() {
        let guidance = missing_executable_guidance(&["rg".to_string()]);

        assert!(guidance.is_some_and(|value| value.contains("search_text")));
    }

    #[test]
    fn shell_command_spec_accepts_program_and_argument_prefix() {
        assert_eq!(
            parse_shell_command_spec("sh -lc").expect("shell spec"),
            ShellCommandSpec {
                program: "sh".to_string(),
                arguments: vec!["-lc".to_string()],
            }
        );
        assert_eq!(
            parse_shell_command_spec(r#""C:\Program Files\PowerShell\7\pwsh.exe" -Command"#)
                .expect("quoted shell spec"),
            ShellCommandSpec {
                program: r#"C:\Program Files\PowerShell\7\pwsh.exe"#.to_string(),
                arguments: vec!["-Command".to_string()],
            }
        );
        assert_eq!(
            parse_shell_command_spec("pwsh -NoProfile").expect("PowerShell spec"),
            ShellCommandSpec {
                program: "pwsh".to_string(),
                arguments: vec!["-NoProfile".to_string(), "-Command".to_string()],
            }
        );
        assert_eq!(
            parse_shell_command_spec("bash -l").expect("POSIX shell spec"),
            ShellCommandSpec {
                program: "bash".to_string(),
                arguments: vec!["-l".to_string(), "-c".to_string()],
            }
        );
    }

    #[test]
    fn default_shell_command_spec_preserves_native_program_paths_with_spaces() {
        let arguments = if cfg!(windows) {
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
            ]
        } else {
            vec!["-Command".to_string()]
        };
        assert_eq!(
            default_shell_command_spec(r#"C:\Program Files\PowerShell\7\pwsh.exe"#.to_string()),
            ShellCommandSpec {
                program: r#"C:\Program Files\PowerShell\7\pwsh.exe"#.to_string(),
                arguments,
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_shell_uses_profile_free_powershell() {
        assert_eq!(
            default_shell_command_spec("powershell.exe".to_string()),
            ShellCommandSpec {
                program: "powershell.exe".to_string(),
                arguments: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                ],
            }
        );
        assert!(resolve_shell_command_spec(Some("sh.exe".to_string())).is_err());
    }

    #[test]
    fn shell_timeout_defaults_cover_silent_long_commands() {
        assert_eq!(DEFAULT_SHELL_TIMEOUT_MS, 300_000);
        assert_eq!(MIN_SHELL_TIMEOUT_MS, 1_000);
        assert_eq!(MAX_SHELL_TIMEOUT_MS, 1_800_000);
    }
}
