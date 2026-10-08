//! 受管后台进程：启动、读取、写入、停止、列出，以及进程查询。
//!
//! 对模型公开的入口是 `shell_exec` 的 `background` / `action` 参数（见 `shell`），
//! 这里的 `*_with_surface` 函数由它委托，失败码前缀因此是 `shell_exec_*`。

use super::{
    failure::{ToolFailure, invalid_input},
    field_string, field_usize, parse_json_object, required_string_field,
    shell::{
        ACTIVE_SHELL_EXECUTIONS, SHELL_OUTPUT_MAX_BYTES, ShellInvocation,
        missing_executables_failure, requested_access_mode, resolve_shell_invocation,
        terminate_shell_child,
    },
};
use crate::{BuiltinToolAccessMode, ToolExecutionContext, ToolExecutionContextQuery};
use magi_core::UtcMillis;
use magi_process::{ManagedChild, spawn_managed, std_command};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{Read, Write},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// process_write 等待子进程读取输入的上限。
pub(super) const PROCESS_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// 进程表中保留的已退出后台进程数量上限。
pub(super) const MAX_RETAINED_EXITED_PROCESSES: usize = 32;

#[derive(Clone, Debug, Default)]
pub(super) struct ProcessExecutionScope {
    worker_id: Option<String>,
    task_id: Option<String>,
    session_id: Option<String>,
    workspace_id: Option<String>,
}

pub(super) struct ManagedProcess {
    terminal_id: u64,
    command: String,
    cwd: String,
    scope: ProcessExecutionScope,
    child: ManagedChild,
    /// 子进程 stdin 单独加锁：写入可能因子进程不读而阻塞，绝不能在持有全局进程表锁时进行。
    stdin: Option<Arc<Mutex<std::process::ChildStdin>>>,
    stdout: SharedOutput,
    stderr: SharedOutput,
    started_at_ms: u64,
}

pub(super) static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);

pub(super) static PROCESS_TABLE: LazyLock<Mutex<HashMap<u64, ManagedProcess>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl ProcessExecutionScope {
    pub(super) fn from_context(context: &ToolExecutionContext) -> Self {
        Self {
            worker_id: context.worker_id.as_ref().map(ToString::to_string),
            task_id: context.task_id.as_ref().map(ToString::to_string),
            session_id: context.session_id.as_ref().map(ToString::to_string),
            workspace_id: context.workspace_id.as_ref().map(ToString::to_string),
        }
    }

    pub(super) fn matches_query(&self, query: &ToolExecutionContextQuery) -> bool {
        let has_scope = query.worker_id.is_some()
            || query.task_id.is_some()
            || query.session_id.is_some()
            || query.workspace_id.is_some();
        if !has_scope {
            return false;
        }
        if let Some(worker_id) = query.worker_id.as_ref()
            && self.worker_id.as_deref() != Some(worker_id.as_str())
        {
            return false;
        }
        if let Some(task_id) = query.task_id.as_ref()
            && self.task_id.as_deref() != Some(task_id.as_str())
        {
            return false;
        }
        if let Some(session_id) = query.session_id.as_ref()
            && self.session_id.as_deref() != Some(session_id.as_str())
        {
            return false;
        }
        if let Some(workspace_id) = query.workspace_id.as_ref()
            && self.workspace_id.as_deref() != Some(workspace_id.as_str())
        {
            return false;
        }
        true
    }
}

pub(crate) fn cancel_active_processes(query: &ToolExecutionContextQuery) -> usize {
    let shell_processes = {
        let mut table = ACTIVE_SHELL_EXECUTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let execution_ids = table
            .values()
            .filter(|process| process.scope.matches_query(query))
            .map(|process| process.execution_id)
            .collect::<Vec<_>>();
        execution_ids
            .into_iter()
            .filter_map(|execution_id| {
                let process = table.remove(&execution_id)?;
                process.cancelled.store(true, Ordering::SeqCst);
                Some(process)
            })
            .collect::<Vec<_>>()
    };
    let managed_processes = take_managed_processes(|process| process.scope.matches_query(query));
    let cancelled_count = shell_processes.len() + managed_processes.len();
    for process in &shell_processes {
        let _ = terminate_shell_child(&process.child);
    }
    for mut process in managed_processes {
        if let Err(error) = process.child.terminate() {
            tracing::warn!(
                terminal_id = process.terminal_id,
                %error,
                "取消工具执行时终止后台进程树失败"
            );
        }
    }
    cancelled_count
}

pub(crate) fn cancel_all_active_processes() -> usize {
    let shell_processes = {
        let mut table = ACTIVE_SHELL_EXECUTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        table
            .drain()
            .map(|(_, process)| {
                process.cancelled.store(true, Ordering::SeqCst);
                process
            })
            .collect::<Vec<_>>()
    };
    let managed_processes = take_managed_processes(|_| true);
    let cancelled_count = shell_processes.len() + managed_processes.len();
    for process in &shell_processes {
        let _ = terminate_shell_child(&process.child);
    }
    for mut process in managed_processes {
        if let Err(error) = process.child.terminate() {
            tracing::warn!(
                terminal_id = process.terminal_id,
                %error,
                "服务关闭时终止后台进程树失败"
            );
        }
    }
    cancelled_count
}

/// 已退出的后台进程只为查看输出保留最近一批，避免进程表随长期使用无限增长。
pub(super) fn prune_exited_processes(table: &mut HashMap<u64, ManagedProcess>) {
    let mut exited = table
        .values_mut()
        .filter_map(|process| {
            matches!(process.child.try_wait(), Ok(Some(_)))
                .then_some((process.started_at_ms, process.terminal_id))
        })
        .collect::<Vec<_>>();
    if exited.len() < MAX_RETAINED_EXITED_PROCESSES {
        return;
    }
    exited.sort_unstable();
    let excess = exited.len() + 1 - MAX_RETAINED_EXITED_PROCESSES;
    for (_, terminal_id) in exited.into_iter().take(excess) {
        table.remove(&terminal_id);
    }
}

pub(super) fn take_managed_processes(
    predicate: impl Fn(&ManagedProcess) -> bool,
) -> Vec<ManagedProcess> {
    let mut table = PROCESS_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let terminal_ids = table
        .values()
        .filter(|process| predicate(process))
        .map(|process| process.terminal_id)
        .collect::<Vec<_>>();
    terminal_ids
        .into_iter()
        .filter_map(|terminal_id| table.remove(&terminal_id))
        .collect()
}

pub(super) fn process_belongs_to_context(
    process: &ManagedProcess,
    context: &ToolExecutionContext,
) -> bool {
    if let Some(session_id) = process.scope.session_id.as_deref()
        && context.session_id.as_ref().map(|id| id.as_str()) != Some(session_id)
    {
        return false;
    }
    if let Some(workspace_id) = process.scope.workspace_id.as_deref()
        && context.workspace_id.as_ref().map(|id| id.as_str()) != Some(workspace_id)
    {
        return false;
    }
    process.scope.session_id.is_some() || process.scope.workspace_id.is_some()
}

pub(super) fn execute_process_inspect(input: &str) -> String {
    let request = parse_json_object(input);
    if request.as_ref().is_some_and(|object| {
        ["process_id", "name", "pattern", "max_results"]
            .iter()
            .any(|key| object.contains_key(*key))
    }) {
        return invalid_input(
            "process_inspect",
            "process_inspect 只接受 pid/query/limit 字段",
        );
    }
    if request.is_none() {
        return invalid_input("process_inspect", "输入必须为 JSON 对象");
    }
    let query = request
        .as_ref()
        .and_then(|object| field_string(object, "query"));
    let pid = request
        .as_ref()
        .and_then(|object| field_usize(object, "pid"))
        .map(|pid| pid as u32)
        .unwrap_or_else(std::process::id);
    let limit = request
        .as_ref()
        .and_then(|object| field_usize(object, "limit"))
        .unwrap_or(20)
        .clamp(1, 100);

    let output = match platform_process_listing(query.is_some(), pid) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(tool = "process_inspect", %error, "查询进程信息失败");
            return ToolFailure::new("process_inspect", "listing_failed", "无法读取系统进程列表")
                .instruction("原因已记录到日志；不要用相同参数重复调用，改用 shell_exec 执行系统自带的进程命令。")
                .into_payload();
        }
    };

    let raw_output = String::from_utf8_lossy(&output);
    let mut matches = Vec::new();
    let query_lower = query.as_ref().map(|value| value.to_lowercase());

    for line in raw_output.lines() {
        if let Some(query_lower) = &query_lower
            && !line.to_lowercase().contains(query_lower)
        {
            continue;
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

#[cfg(not(windows))]
pub(super) fn platform_process_listing(list_all: bool, pid: u32) -> std::io::Result<Vec<u8>> {
    let mut command = std_command("ps");
    if list_all {
        command.args(["-ax", "-o", "pid=,ppid=,state=,comm="]);
    } else {
        command.args(["-p", &pid.to_string(), "-o", "pid=,ppid=,state=,comm="]);
    }
    command.output().map(|output| output.stdout)
}

#[cfg(windows)]
pub(super) fn platform_process_listing(list_all: bool, pid: u32) -> std::io::Result<Vec<u8>> {
    let mut command = std_command("tasklist");
    command.args(["/FO", "CSV", "/NH"]);
    if !list_all {
        command.args(["/FI", &format!("PID eq {pid}")]);
    }
    let output = command.output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut normalized = String::new();
    for line in text.lines() {
        let fields = parse_windows_csv_line(line);
        let Some(process_pid) = fields.get(1).and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let command = fields.first().cloned().unwrap_or_default();
        normalized.push_str(&format!("{process_pid} 0 R {command}\n"));
    }
    Ok(normalized.into_bytes())
}

#[cfg(windows)]
pub(super) fn parse_windows_csv_line(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('"')
        .split("\",\"")
        .map(str::to_string)
        .collect()
}

pub(super) fn parse_ps_line(line: &str) -> Option<Value> {
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

pub(super) fn infer_process_mode(query: &Option<String>) -> &'static str {
    if query.is_some() { "query" } else { "pid" }
}

/// 后台进程输出的滚动缓冲：只保留最近 `SHELL_OUTPUT_MAX_BYTES`，同时记录被淘汰的字节数，
/// 让读取方能用绝对偏移增量读取，并且知道有输出已经被淘汰。
#[derive(Default)]
pub(super) struct OutputBuffer {
    bytes: Vec<u8>,
    /// `bytes[0]` 在整个输出流里的绝对偏移。
    start_offset: u64,
}

/// 一次读取的结果：`start_offset..next_offset` 是返回的文本在输出流里的位置。
pub(super) struct OutputSlice {
    text: String,
    start_offset: u64,
    next_offset: u64,
    /// 请求范围里，因已被淘汰或超出读取窗口而没有返回的字节数。
    omitted_bytes: u64,
    has_more: bool,
}

impl OutputBuffer {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > SHELL_OUTPUT_MAX_BYTES {
            let drain = self.bytes.len() - SHELL_OUTPUT_MAX_BYTES;
            self.bytes.drain(0..drain);
            self.start_offset += drain as u64;
        }
    }

    fn end_offset(&self) -> u64 {
        self.start_offset + self.bytes.len() as u64
    }

    /// `after` 为 `None` 时返回最近的 `max_bytes`；给出偏移时从该偏移起按顺序返回至多 `max_bytes`，
    /// 可以用返回的 `next_offset` 继续读取。切分位置不会落在多字节字符中间。
    fn read(&self, after: Option<u64>, max_bytes: usize) -> OutputSlice {
        let end = self.end_offset();
        let wanted_from = match after {
            Some(after) => after.min(end),
            None => end.saturating_sub(max_bytes as u64),
        };
        let from = wanted_from.max(self.start_offset);
        let mut begin = (from - self.start_offset) as usize;
        // 尾部读取可能从字符中间开始：跳过开头的续字节。
        if after.is_none() {
            while begin < self.bytes.len() && (self.bytes[begin] & 0b1100_0000) == 0b1000_0000 {
                begin += 1;
            }
        }
        let mut stop = (begin + max_bytes).min(self.bytes.len());
        if stop < self.bytes.len() {
            // 没读完时不要把后面的多字节字符拦腰切开。
            while stop > begin && (self.bytes[stop] & 0b1100_0000) == 0b1000_0000 {
                stop -= 1;
            }
        }
        let start_offset = self.start_offset + begin as u64;
        let next_offset = self.start_offset + stop as u64;
        OutputSlice {
            text: String::from_utf8_lossy(&self.bytes[begin..stop]).to_string(),
            start_offset,
            next_offset,
            omitted_bytes: start_offset.saturating_sub(after.unwrap_or(0)),
            has_more: next_offset < end,
        }
    }
}

type SharedOutput = Arc<Mutex<OutputBuffer>>;

pub(super) fn lock_output(output: &SharedOutput) -> std::sync::MutexGuard<'_, OutputBuffer> {
    output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn output_fields(payload: &mut Value, stream: &str, slice: OutputSlice) {
    payload[stream] = Value::String(slice.text);
    payload[format!("{stream}_start_offset")] = slice.start_offset.into();
    payload[format!("{stream}_next_offset")] = slice.next_offset.into();
    payload[format!("{stream}_omitted_bytes")] = slice.omitted_bytes.into();
    payload[format!("{stream}_has_more")] = slice.has_more.into();
}

/// 后台启动后观察多久来确认进程没有立刻退出。
const STARTUP_OBSERVATION: Duration = Duration::from_millis(300);
const STARTUP_POLL: Duration = Duration::from_millis(20);
/// 进程退出后，等输出线程把管道里的剩余内容读进缓冲的时间。
const OUTPUT_SETTLE: Duration = Duration::from_millis(60);

fn process_failure(surface_tool: &str, kind: &str, error: impl Into<String>) -> ToolFailure {
    ToolFailure::new(surface_tool, kind, error)
}

fn process_not_found(surface_tool: &str, terminal_id: usize) -> String {
    process_failure(surface_tool, "not_found", format!("进程 #{terminal_id} 不存在"))
        .instruction(
            "用 action=list 查看当前可用的 terminal_id。已结束并被清理的进程无法再读取，需要时重新启动。",
        )
        .into_payload()
}

fn process_not_owned(surface_tool: &str, terminal_id: usize) -> String {
    process_failure(
        surface_tool,
        "not_owned",
        format!("进程 #{terminal_id} 不属于当前会话或工作区"),
    )
    .instruction("用 action=list 查看当前上下文可管理的进程；不要重试同一个 terminal_id。")
    .into_payload()
}

pub(super) fn execute_process_launch(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_launch_with_surface(input, context, "process_launch", None)
}

pub(super) fn execute_process_launch_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let request = parse_json_object(input);
    let command =
        match required_string_field(request.as_ref(), "command", surface_tool, "缺少 shell 命令")
        {
            Ok(value) => value,
            Err(error) => return error,
        };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let invocation = match resolve_shell_invocation(surface_tool, request.as_ref(), context) {
        Ok(invocation) => invocation,
        Err(failure) => return failure,
    };
    let access_mode = requested_access_mode(request.as_ref());
    if let Some(failure) =
        missing_executables_failure(surface_tool, &command, &invocation, access_mode)
    {
        return failure;
    }
    let ShellInvocation { cwd, shell } = invocation;

    let mut command_builder = std_command(&shell.program);
    command_builder
        .args(&shell.arguments)
        .arg(&command)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match spawn_managed(&mut command_builder) {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(tool = surface_tool, %error, "后台进程启动失败");
            return process_failure(surface_tool, "spawn_failed", "进程启动失败")
                .instruction(
                    "原因已记录到日志；检查 shell 与 cwd 是否可用，不要用相同参数重复调用。",
                )
                .into_payload();
        }
    };

    let stdout_buffer = SharedOutput::default();
    let stderr_buffer = SharedOutput::default();
    spawn_managed_process_reader(child.take_stdout(), Arc::clone(&stdout_buffer));
    spawn_managed_process_reader(child.take_stderr(), Arc::clone(&stderr_buffer));
    let stdin = child.take_stdin().map(|stdin| Arc::new(Mutex::new(stdin)));
    let terminal_id = NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed);
    let mut process = ManagedProcess {
        terminal_id,
        command: command.clone(),
        cwd: cwd.display().to_string(),
        scope: ProcessExecutionScope::from_context(context),
        child,
        stdin,
        stdout: Arc::clone(&stdout_buffer),
        stderr: Arc::clone(&stderr_buffer),
        started_at_ms: UtcMillis::now().0,
    };

    // 观察一小段时间：命令不存在、端口被占用这类立刻退出的启动失败，不能报告成“已启动”。
    let deadline = Instant::now() + STARTUP_OBSERVATION;
    let early_exit = loop {
        if let Ok(Some(status)) = process.child.try_wait() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(STARTUP_POLL);
    };
    if early_exit.is_some() {
        thread::sleep(OUTPUT_SETTLE);
    }

    {
        let mut table = PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_exited_processes(&mut table);
        table.insert(terminal_id, process);
    }

    let exit_code = early_exit.as_ref().and_then(ExitStatus::code);
    let exited_with_failure = early_exit.as_ref().is_some_and(|status| !status.success());
    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": if exited_with_failure { "failed" } else { "succeeded" },
        "terminal_id": terminal_id,
        "command": command,
        "cwd": cwd.display().to_string(),
        "session_id": context.session_id.as_ref().map(ToString::to_string),
        "workspace_id": context.workspace_id.as_ref().map(ToString::to_string),
        "running": early_exit.is_none(),
        "startup_status": if exited_with_failure { "failed" } else { "confirmed" },
    });
    if early_exit.is_some() {
        payload["exit_code"] = exit_code.into();
        output_fields(
            &mut payload,
            "stdout",
            lock_output(&stdout_buffer).read(None, 12_000),
        );
        output_fields(
            &mut payload,
            "stderr",
            lock_output(&stderr_buffer).read(None, 12_000),
        );
    }
    if exited_with_failure {
        payload["error_code"] = format!("{surface_tool}_exited_early").into();
        payload["error"] = "后台命令启动后立刻退出，没有保持运行".into();
        payload["instruction"] =
            "查看 stdout / stderr 里的报错，修正命令或环境后再启动；相同命令不会得到不同结果。"
                .into();
        payload["summary"] =
            format!("后台命令 #{terminal_id} 启动后立刻退出（退出码 {exit_code:?}）: {command}")
                .into();
    } else if early_exit.is_some() {
        payload["summary"] = format!("后台命令 #{terminal_id} 已执行完毕: {command}").into();
    } else {
        payload["summary"] = if surface_tool == "shell_exec" {
            format!("已在后台启动 shell 终端 #{terminal_id}: {command}")
        } else {
            format!("已在后台启动进程 #{terminal_id}: {command}")
        }
        .into();
    }
    if let Some(mode) = mode {
        payload["mode"] = Value::String(mode.to_string());
    }
    payload.to_string()
}

pub(super) fn execute_process_read(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_read_with_surface(input, context, "process_read", None)
}

pub(super) fn execute_process_read_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input(surface_tool, "输入必须为 JSON 对象，包含 terminal_id");
    };
    let Some(terminal_id) = field_usize(&request, "terminal_id") else {
        return invalid_input(surface_tool, "缺少 terminal_id");
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let max_bytes = field_usize(&request, "max_bytes")
        .unwrap_or(12_000)
        .clamp(512, 200_000);
    let stdout_offset = field_usize(&request, "stdout_offset").map(|offset| offset as u64);
    let stderr_offset = field_usize(&request, "stderr_offset").map(|offset| offset as u64);

    let mut table = PROCESS_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(process) = table.get_mut(&(terminal_id as u64)) else {
        return process_not_found(surface_tool, terminal_id);
    };
    if !process_belongs_to_context(process, context) {
        return process_not_owned(surface_tool, terminal_id);
    }
    let exit_status = process.child.try_wait().ok().flatten();
    let running = exit_status.is_none();
    let exit_code = exit_status.as_ref().and_then(ExitStatus::code);

    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "running": running,
        "exit_code": exit_code,
        "summary": if running {
            format!("进程 #{terminal_id} 正在运行")
        } else {
            format!("进程 #{terminal_id} 已结束")
        }
    });
    output_fields(
        &mut payload,
        "stdout",
        lock_output(&process.stdout).read(stdout_offset, max_bytes),
    );
    output_fields(
        &mut payload,
        "stderr",
        lock_output(&process.stderr).read(stderr_offset, max_bytes),
    );
    if let Some(mode) = mode {
        payload["mode"] = Value::String(mode.to_string());
    }
    payload.to_string()
}

pub(super) fn execute_process_write(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_write_with_surface(input, context, "process_write", None)
}

pub(super) fn execute_process_write_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input(surface_tool, "输入必须为 JSON 对象，包含 terminal_id");
    };
    let Some(terminal_id) = field_usize(&request, "terminal_id") else {
        return invalid_input(surface_tool, "缺少 terminal_id");
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let content = field_string(&request, "input").unwrap_or_default();
    let stdin = {
        let table = PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(process) = table.get(&(terminal_id as u64)) else {
            return process_not_found(surface_tool, terminal_id);
        };
        if !process_belongs_to_context(process, context) {
            return process_not_owned(surface_tool, terminal_id);
        }
        let Some(stdin) = process.stdin.clone() else {
            return process_stdin_closed(surface_tool, terminal_id);
        };
        stdin
    };
    // 进程表锁已释放；写入在独立线程里进行并设上限：子进程不读输入时本次调用按超时
    // 返回，不会一直占着工具执行，其他进程工具、中断与关机都不受影响。辅助线程在进程
    // 退出、写端收到断管错误后自行结束。
    let bytes = content.into_bytes();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = stdin.write_all(&bytes).and_then(|()| stdin.flush());
        let _ = result_tx.send(result);
    });
    match result_rx.recv_timeout(PROCESS_WRITE_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            return process_stdin_closed(surface_tool, terminal_id);
        }
        Ok(Err(error)) => {
            tracing::warn!(tool = surface_tool, terminal_id, %error, "写入后台进程失败");
            return process_failure(surface_tool, "io_failed", "写入后台进程失败")
                .instruction(
                    "原因已记录到日志；先用 action=read 确认进程状态，不要用相同参数重复写入。",
                )
                .into_payload();
        }
        Err(_) => {
            return process_failure(surface_tool, "timeout", "进程没有在限定时间内读取输入")
                .instruction(
                    "进程当前没有在读 stdin；先用 action=read 查看它在等什么，不要重复写入。",
                )
                .into_payload();
        }
    }

    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "summary": format!("已写入进程 #{terminal_id}")
    });
    if let Some(mode) = mode {
        payload["mode"] = Value::String(mode.to_string());
    }
    payload.to_string()
}

fn process_stdin_closed(surface_tool: &str, terminal_id: usize) -> String {
    process_failure(
        surface_tool,
        "stdin_closed",
        format!("进程 #{terminal_id} 不再接受输入"),
    )
    .instruction("进程可能已退出或关闭了 stdin；用 action=read 查看状态，需要时重新启动。")
    .into_payload()
}

pub(super) fn execute_process_kill(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_kill_with_surface(input, context, "process_kill", None)
}

pub(super) fn execute_process_kill_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    let Some(request) = parse_json_object(input) else {
        return invalid_input(surface_tool, "输入必须为 JSON 对象，包含 terminal_id");
    };
    let Some(terminal_id) = field_usize(&request, "terminal_id") else {
        return invalid_input(surface_tool, "缺少 terminal_id");
    };
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let mut process = {
        let mut table = PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(process) = table.get(&(terminal_id as u64)) else {
            return process_not_found(surface_tool, terminal_id);
        };
        if !process_belongs_to_context(process, context) {
            return process_not_owned(surface_tool, terminal_id);
        }
        table
            .remove(&(terminal_id as u64))
            .expect("checked process should remain in table")
    };
    if let Err(error) = process.child.terminate() {
        tracing::warn!(tool = surface_tool, terminal_id, %error, "停止后台进程树失败");
        // 没停掉就必须仍然可以管理，否则它会变成再也找不到的孤儿进程。
        PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(terminal_id as u64, process);
        return process_failure(surface_tool, "terminate_failed", "没能停止后台进程")
            .instruction("进程仍在进程表中；用 action=read 确认状态，必要时告知用户手动结束它。")
            .into_payload();
    }
    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "summary": format!("已停止进程 #{terminal_id}")
    });
    if let Some(mode) = mode {
        payload["mode"] = Value::String(mode.to_string());
    }
    payload.to_string()
}

pub(super) fn execute_process_list(context: &ToolExecutionContext) -> String {
    execute_process_list_with_surface(context, "process_list", None)
}

pub(super) fn execute_process_list_with_surface(
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
) -> String {
    if let Some(error) = require_process_context(surface_tool, context) {
        return error;
    }
    let mut table = PROCESS_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            "session_id": process.scope.session_id.as_deref(),
            "workspace_id": process.scope.workspace_id.as_deref(),
            "task_id": process.scope.task_id.as_deref(),
            "worker_id": process.scope.worker_id.as_deref(),
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
        payload["mode"] = Value::String(mode.to_string());
    }
    payload.to_string()
}

pub(super) fn spawn_managed_process_reader<T: Read + Send + 'static>(
    pipe: Option<T>,
    target: SharedOutput,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut chunk = [0_u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(size) => lock_output(&target).push(&chunk[..size]),
            }
        }
    })
}

fn require_process_context(tool: &str, context: &ToolExecutionContext) -> Option<String> {
    if context.session_id.is_some() || context.workspace_id.is_some() {
        return None;
    }
    Some(
        process_failure(
            tool,
            "context_required",
            "后台进程工具需要 session 或 workspace 上下文",
        )
        .instruction("该工具只能在会话或工作区内使用；不要重试。")
        .into_payload(),
    )
}

#[cfg(test)]
mod output_buffer_tests {
    use super::*;

    fn filled(text: &str) -> OutputBuffer {
        let mut buffer = OutputBuffer::default();
        buffer.push(text.as_bytes());
        buffer
    }

    #[test]
    fn tail_read_returns_recent_output_with_offsets() {
        let buffer = filled("abcdef");

        let slice = buffer.read(None, 4);

        assert_eq!(slice.text, "cdef");
        assert_eq!((slice.start_offset, slice.next_offset), (2, 6));
        assert_eq!(slice.omitted_bytes, 2);
        assert!(!slice.has_more);
    }

    #[test]
    fn incremental_read_continues_from_the_cursor_and_pages_through_large_output() {
        let buffer = filled("abcdef");

        let first = buffer.read(Some(0), 4);
        assert_eq!(
            (first.text.as_str(), first.next_offset, first.has_more),
            ("abcd", 4, true)
        );
        let second = buffer.read(Some(first.next_offset), 4);
        assert_eq!(
            (second.text.as_str(), second.next_offset, second.has_more),
            ("ef", 6, false)
        );
        let drained = buffer.read(Some(second.next_offset), 4);
        assert_eq!(drained.text, "");
        assert_eq!(drained.next_offset, 6);
        let future = buffer.read(Some(100), 4);
        assert_eq!((future.text.as_str(), future.next_offset), ("", 6));
    }

    #[test]
    fn evicted_output_is_reported_instead_of_silently_skipped() {
        let mut buffer = OutputBuffer::default();
        buffer.push(&vec![b'a'; SHELL_OUTPUT_MAX_BYTES]);
        buffer.push(b"tail");

        let slice = buffer.read(Some(0), 8);

        assert_eq!(
            slice.start_offset, 4,
            "缓冲只保留最近的输出，起点随淘汰前移"
        );
        assert_eq!(slice.omitted_bytes, 4, "必须告诉调用方有 4 字节已被淘汰");
        assert_eq!(buffer.end_offset(), (SHELL_OUTPUT_MAX_BYTES + 4) as u64);
    }

    #[test]
    fn slices_never_split_a_multibyte_character() {
        let buffer = filled("中文字符");

        let first = buffer.read(Some(0), 4);
        assert_eq!(first.text, "中");
        assert_eq!(first.next_offset, 3, "游标停在字符边界，下一页从“文”开始");
        let second = buffer.read(Some(first.next_offset), 12);
        assert_eq!(second.text, "文字符");

        // 尾部读取从字符中间开始时，跳过被切开的续字节而不是产生乱码。
        let tail = buffer.read(None, 4);
        assert!(!tail.text.contains('\u{fffd}'), "{:?}", tail.text);
    }
}
