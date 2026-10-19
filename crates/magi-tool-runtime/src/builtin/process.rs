//! 受管后台进程：启动、读取、写入、停止、列出，以及进程查询。
//!
//! 对模型公开的入口是 `shell_exec` 的 `background` / `action` 参数（见 `shell`），
//! 这里的 `*_with_surface` 函数由它委托，失败码前缀因此是 `shell_exec_*`。

use super::{
    failure::invalid_input,
    field_string, field_usize, parse_json_object, required_string_field,
    shell::{
        ACTIVE_SHELL_EXECUTIONS, SHELL_OUTPUT_MAX_BYTES, ShellInvocation,
        missing_executables_failure, requested_access_mode, resolve_shell_invocation,
        terminate_shell_child,
    },
};
use crate::background_job::{
    BackgroundJob, BackgroundJobJournal, BackgroundJobStatus, DurableProcessJob,
};
use crate::{BuiltinToolAccessMode, ToolExecutionContext, ToolExecutionContextQuery};
use magi_core::ToolFailure;
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

enum ProcessOutputReaders {
    Reading([JoinHandle<std::io::Result<()>>; 2]),
    Settled(bool),
}

impl ProcessOutputReaders {
    fn settle(&mut self) -> bool {
        let readers = match self {
            Self::Settled(success) => return *success,
            Self::Reading(readers) => readers,
        };
        let deadline =
            Instant::now() + Duration::from_millis(super::shell::SHELL_PIPE_DRAIN_GRACE_MS);
        while readers.iter().any(|reader| !reader.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if readers.iter().any(|reader| !reader.is_finished()) {
            return false;
        }
        let Self::Reading(readers) = std::mem::replace(self, Self::Settled(false)) else {
            unreachable!()
        };
        let mut success = true;
        for reader in readers {
            success &= matches!(reader.join(), Ok(Ok(())));
        }
        *self = Self::Settled(success);
        success
    }
}

pub(super) struct ManagedProcess {
    terminal_id: u64,
    command: String,
    cwd: String,
    scope: ProcessExecutionScope,
    child: Option<ManagedChild>,
    /// 子进程 stdin 单独加锁：写入可能因子进程不读而阻塞，绝不能在持有全局进程表锁时进行。
    stdin: Option<Arc<Mutex<std::process::ChildStdin>>>,
    stdout: SharedOutput,
    stderr: SharedOutput,
    started_at_ms: u64,
    job: Option<Arc<DurableProcessJob>>,
    observed_exit: Arc<Mutex<Option<ExitStatus>>>,
    /// 与进程一起移交结算；取消不能丢下仍在写产物的读取线程。
    output_readers: Option<ProcessOutputReaders>,
}

impl ManagedProcess {
    fn job_settlement_pending(&self) -> bool {
        self.job.as_ref().is_some_and(|job| {
            job.is_unsettled()
                || self.child.is_some()
                || matches!(self.output_readers, Some(ProcessOutputReaders::Reading(_)))
        })
    }

    fn poll_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let exit = match self.child.as_mut() {
            Some(child) => child.try_wait()?,
            None => *self.observed_exit.lock().expect("process exit lock"),
        };
        if let Some(status) = &exit {
            *self.observed_exit.lock().expect("process exit lock") = Some(*status);
        }
        Ok(exit)
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            let status = child.terminate()?;
            *self.observed_exit.lock().expect("process exit lock") = Some(status);
            // 只保留退出和 Job 结算记录。写盘重试不能再按已回收的 PID 发送信号。
            self.child = None;
        }
        Ok(())
    }
}

pub(super) static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);

pub(super) static PROCESS_TABLE: LazyLock<Mutex<HashMap<u64, ManagedProcess>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 启动与停止都先登记在途操作，再做进程/journal IO；查询不能将暂时离开进程表的操作当作已结算。
struct ProcessOperationGuard {
    id: u64,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
type PendingProcessOperation = (ProcessExecutionScope, Arc<std::sync::atomic::AtomicBool>);
static PROCESS_OPERATIONS: LazyLock<Mutex<HashMap<u64, PendingProcessOperation>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
impl ProcessOperationGuard {
    fn begin(context: &ToolExecutionContext) -> Self {
        Self::from_scope(ProcessExecutionScope::from_context(context))
    }
    fn from_scope(scope: ProcessExecutionScope) -> Self {
        let id = NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        PROCESS_OPERATIONS
            .lock()
            .expect("process operation lock")
            .insert(id, (scope, cancelled.clone()));
        Self { id, cancelled }
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}
impl Drop for ProcessOperationGuard {
    fn drop(&mut self) {
        PROCESS_OPERATIONS
            .lock()
            .expect("process operation lock")
            .remove(&self.id);
    }
}

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
        if query.taskless_only && self.task_id.is_some() {
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
    cancel_matching_processes(|scope| scope.matches_query(query))
}

fn cancel_matching_processes(matches_scope: impl Fn(&ProcessExecutionScope) -> bool) -> usize {
    for (scope, cancelled) in PROCESS_OPERATIONS
        .lock()
        .expect("process operation lock")
        .values()
    {
        if matches_scope(scope) {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
    let shell_processes = {
        let mut table = ACTIVE_SHELL_EXECUTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let execution_ids = table
            .values()
            .filter(|process| matches_scope(&process.scope))
            .map(|process| process.execution_id)
            .collect::<Vec<_>>();
        execution_ids
            .into_iter()
            .filter_map(|execution_id| {
                let operation =
                    ProcessOperationGuard::from_scope(table.get(&execution_id)?.scope.clone());
                let process = table.remove(&execution_id)?;
                process.cancelled.store(true, Ordering::SeqCst);
                Some((process, operation))
            })
            .collect::<Vec<_>>()
    };
    let managed_processes = take_managed_processes(|process| matches_scope(&process.scope));
    let cancelled_count = shell_processes.len() + managed_processes.len();
    for (process, _settlement_guard) in &shell_processes {
        let _ = terminate_shell_child(&process.child);
    }
    for (process, _settlement_guard) in managed_processes {
        if let Err(failure) = stop_process(process, BackgroundJobStatus::Cancelled, "shell_exec") {
            tracing::error!(failure = %failure.into_payload(), "后台进程取消尚未结算，保留句柄等待重试");
        }
    }
    cancelled_count
}

/// 取消回执不是结算证据；终止或 Job 终态提交失败都继续阻止新工作抢占资源。
pub(crate) fn has_active_processes(query: &ToolExecutionContextQuery) -> bool {
    if ACTIVE_SHELL_EXECUTIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .any(|process| process.scope.matches_query(query))
    {
        return true;
    }
    // 同时持有进程表与在途操作的读取边界；停止操作在移走句柄前登记，失败重入表后
    // 才释放 guard，不能在两份瞬时集合的交接窗口返回“已结算”。
    let mut table = PROCESS_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if table.values_mut().any(|process| {
        process.scope.matches_query(query)
            && (process.job_settlement_pending() || !matches!(process.poll_exit(), Ok(Some(_))))
    }) {
        return true;
    }
    PROCESS_OPERATIONS
        .lock()
        .expect("process operation lock")
        .values()
        .any(|(scope, _)| scope.matches_query(query))
}

pub(crate) fn cancel_all_active_processes() -> usize {
    cancel_matching_processes(|_| true)
}

/// 已退出的后台进程只为查看输出保留最近一批，避免进程表随长期使用无限增长。
pub(super) fn prune_exited_processes(table: &mut HashMap<u64, ManagedProcess>) {
    let mut exited = table
        .values_mut()
        .filter_map(|process| {
            (!process.job_settlement_pending() && matches!(process.poll_exit(), Ok(Some(_))))
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

fn take_managed_processes(
    predicate: impl Fn(&ManagedProcess) -> bool,
) -> Vec<(ManagedProcess, ProcessOperationGuard)> {
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
        .filter_map(|terminal_id| {
            let operation =
                ProcessOperationGuard::from_scope(table.get(&terminal_id)?.scope.clone());
            let process = table.remove(&terminal_id)?;
            if let Some(job) = &process.job {
                job.request_cancel();
            }
            Some((process, operation))
        })
        .collect()
}

pub(super) fn process_belongs_to_context(
    process: &ManagedProcess,
    context: &ToolExecutionContext,
) -> bool {
    // 委派权只允许管理子任务，不授予同会话其他任务的进程控制权。
    if context.caller_scope != crate::ToolCallerScope::SessionMainline
        && (context.task_id.is_none()
            || process.scope.task_id.as_deref()
                != context.task_id.as_ref().map(|task| task.as_str()))
    {
        return false;
    }
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
        format!("进程 #{terminal_id} 不属于当前任务、会话或工作区的授权范围"),
    )
    .instruction("用 action=list 查看当前上下文可管理的进程；不要重试同一个 terminal_id。")
    .into_payload()
}

pub(super) fn execute_process_launch(input: &str, context: &ToolExecutionContext) -> String {
    execute_process_launch_with_surface(input, context, "process_launch", None, None, None)
}

pub(super) fn execute_process_launch_with_surface(
    input: &str,
    context: &ToolExecutionContext,
    surface_tool: &str,
    mode: Option<&str>,
    journal: Option<(&str, &Arc<dyn BackgroundJobJournal>)>,
    cancellation: Option<&Arc<std::sync::atomic::AtomicBool>>,
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
    let launch_guard = ProcessOperationGuard::begin(context);

    let job = match journal {
        Some((call, journal)) => {
            match journal.begin(context, call, &command, &cwd.to_string_lossy()) {
                Ok(record) => Some(Arc::new(DurableProcessJob::new(record, journal.clone()))),
                Err(error) => {
                    return process_failure(surface_tool, "job_persist_failed", error)
                        .with("execution_started", false)
                        .into_payload();
                }
            }
        }
        None => None,
    };
    let terminal_id = launch_guard.id;
    let stdout_buffer = SharedOutput::default();
    let stderr_buffer = SharedOutput::default();
    let observed_exit = Arc::new(Mutex::new(None));
    // Starting 提交后即保留结算所需的记录；失败时可以只补交终态，不再启动命令。
    let mut process = ManagedProcess {
        terminal_id,
        command: command.clone(),
        cwd: cwd.display().to_string(),
        scope: ProcessExecutionScope::from_context(context),
        child: None,
        stdin: None,
        stdout: Arc::clone(&stdout_buffer),
        stderr: Arc::clone(&stderr_buffer),
        started_at_ms: UtcMillis::now().0,
        job: job.clone(),
        observed_exit: observed_exit.clone(),
        output_readers: None,
    };
    let output_files = match job
        .as_ref()
        .map(|job| {
            let record = job.snapshot();
            std::fs::create_dir_all(record.stdout_path.parent().expect("job output directory"))?;
            Ok::<_, std::io::Error>((
                std::fs::File::create(&record.stdout_path)?,
                std::fs::File::create(&record.stderr_path)?,
            ))
        })
        .transpose()
    {
        Ok(files) => files,
        Err(error) => {
            return fail_process_start(
                process,
                BackgroundJobStatus::Failed,
                surface_tool,
                process_failure(surface_tool, "job_output_failed", error.to_string()),
                false,
            );
        }
    };

    let mut command_builder = std_command(&shell.program);
    command_builder
        .args(&shell.arguments)
        .arg(&command)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if launch_guard.is_cancelled()
        || cancellation.is_some_and(|cancel| cancel.load(Ordering::SeqCst))
    {
        if let Err(failure) = stop_process(process, BackgroundJobStatus::Cancelled, surface_tool) {
            return failure.with("execution_started", false).into_payload();
        }
        return serde_json::json!({"tool":surface_tool,"status":"cancelled","execution_started":false,"error_code":"job_cancelled_before_start"}).to_string();
    }
    process.child = Some(match spawn_managed(&mut command_builder) {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(tool = surface_tool, %error, "后台进程启动失败");
            return fail_process_start(
                process,
                BackgroundJobStatus::Failed,
                surface_tool,
                process_failure(surface_tool, "spawn_failed", "进程启动失败").instruction(
                    "原因已记录到日志；检查 shell 与 cwd 是否可用，不要用相同参数重复调用。",
                ),
                false,
            );
        }
    });

    if let Some(job) = &job
        && let Err(error) = job.transition(BackgroundJobStatus::Running, None)
    {
        return fail_process_start(
            process,
            BackgroundJobStatus::OutcomeUnknown,
            surface_tool,
            process_failure(surface_tool, "job_persist_failed", error),
            true,
        );
    }

    let (stdout_file, stderr_file) = output_files.map_or((None, None), |(stdout, stderr)| {
        (Some(stdout), Some(stderr))
    });
    let child = process.child.as_mut().expect("spawned process");
    let stdout_reader =
        spawn_process_reader(child.take_stdout(), Arc::clone(&stdout_buffer), stdout_file);
    let stderr_reader =
        spawn_process_reader(child.take_stderr(), Arc::clone(&stderr_buffer), stderr_file);
    process.stdin = child.take_stdin().map(|stdin| Arc::new(Mutex::new(stdin)));
    if job.is_some() {
        process.output_readers = Some(ProcessOutputReaders::Reading([
            stdout_reader,
            stderr_reader,
        ]));
    }

    // 观察一小段时间：命令不存在、端口被占用这类立刻退出的启动失败，不能报告成“已启动”。
    let deadline = Instant::now() + STARTUP_OBSERVATION;
    let early_exit = loop {
        if let Ok(Some(status)) = process.poll_exit() {
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

    if let Some(job) = job.as_ref() {
        if let Some(status) = &early_exit {
            let output_settled = settle_exited_process(terminal_id);
            let state = if !output_settled {
                BackgroundJobStatus::OutcomeUnknown
            } else if status.success() {
                BackgroundJobStatus::Completed
            } else {
                BackgroundJobStatus::Failed
            };
            if let Err(error) = job.transition(state, status.code()) {
                return process_failure(surface_tool, "job_persist_failed", error)
                    .with("job_id", job.snapshot().job_id)
                    .with("terminal_id", terminal_id)
                    .with("execution_started", true)
                    .with("resources_settled", false)
                    .instruction(
                        "原进程的结算尚未提交；用相同 job_id 核对或重试 kill，不要重新启动命令。",
                    )
                    .into_payload();
            }
        } else {
            watch_job(terminal_id, job.clone(), observed_exit);
        }
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
    if let Some(job) = job.as_ref() {
        let record = job.snapshot();
        payload["job_id"] = record.job_id.into();
        payload["job_status"] = serde_json::to_value(record.status).expect("job status serializes");
        if record.status == BackgroundJobStatus::OutcomeUnknown {
            payload["status"] = "indeterminate".into();
            payload["error_code"] = "job_output_unconfirmed".into();
            payload["error"] =
                "后台进程或输出结果尚未确认；保留 Job 引用核对，不要自动重放命令".into();
        }
    }
    if launch_guard.is_cancelled()
        || cancellation.is_some_and(|cancel| cancel.load(Ordering::SeqCst))
    {
        let stopped = execute_process_kill_with_surface(
            &serde_json::json!({"terminal_id":terminal_id}).to_string(),
            context,
            surface_tool,
            mode,
        );
        let stopped: Value = serde_json::from_str(&stopped).expect("process kill JSON");
        if stopped["status"] != "succeeded" {
            if stopped["error_code"] == format!("{surface_tool}_not_found") {
                return process_failure(
                    surface_tool,
                    "job_settlement_unconfirmed",
                    "原进程正在由另一个取消操作结算",
                )
                .with("job_id", payload["job_id"].clone())
                .with("terminal_id", terminal_id)
                .with("resources_settled", false)
                .instruction(
                    "先核对原 Job 与取消回执；此结果不证明资源已释放，不能重新启动原命令。",
                )
                .into_payload();
            }
            return stopped.to_string();
        }
        if let Some(job) = job.as_ref() {
            payload["job_status"] =
                serde_json::to_value(job.snapshot().status).expect("job status serializes");
        }
        payload["status"] = "cancelled".into();
        payload["running"] = false.into();
        payload["resources_settled"] = true.into();
        payload["error_code"] = "job_cancelled_during_start".into();
        return payload.to_string();
    }
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
    if payload["status"] == "indeterminate" {
        payload["summary"] =
            "后台 Job 的输出或执行结果未确认，请按 job_id 核对，不能视为已完成".into();
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
    let exit_status = process.poll_exit().ok().flatten();
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
    let (process, _settlement_guard) = {
        let mut table = PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(process) = table.get(&(terminal_id as u64)) else {
            return process_not_found(surface_tool, terminal_id);
        };
        if !process_belongs_to_context(process, context) {
            return process_not_owned(surface_tool, terminal_id);
        }
        if let Some(job) = &process.job {
            job.request_cancel();
        }
        let operation = ProcessOperationGuard::from_scope(process.scope.clone());
        (
            table
                .remove(&(terminal_id as u64))
                .expect("checked process should remain in table"),
            operation,
        )
    };
    if let Err(failure) = stop_process(process, BackgroundJobStatus::Cancelled, surface_tool) {
        return failure.into_payload();
    }
    let mut payload = serde_json::json!({
        "tool": surface_tool,
        "status": "succeeded",
        "terminal_id": terminal_id,
        "resources_settled": true,
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
            .poll_exit()
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

fn spawn_process_reader<T: Read + Send + 'static>(
    pipe: Option<T>,
    target: SharedOutput,
    mut file: Option<std::fs::File>,
) -> JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return Ok(());
        };
        let mut chunk = [0_u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Err(error) => return Err(error),
                Ok(size) => {
                    lock_output(&target).push(&chunk[..size]);
                    if let Some(file) = file.as_mut()
                        && let Err(error) = file.write_all(&chunk[..size])
                    {
                        return Err(error);
                    }
                }
            }
        }
        if let Some(file) = file {
            file.sync_data()?;
        }
        Ok(())
    })
}

/// 调用方持有原操作 guard；启动失败、显式 kill 和作用域取消共用此结算入口。
#[allow(clippy::result_large_err)]
fn stop_process(
    mut process: ManagedProcess,
    status: BackgroundJobStatus,
    surface_tool: &str,
) -> Result<(), ToolFailure> {
    if let Some(job) = &process.job {
        job.request_cancel();
    }
    let result = match process.terminate() {
        Err(error) => {
            tracing::warn!(terminal_id = process.terminal_id, %error, "停止后台进程树失败");
            Err(
                process_failure(surface_tool, "terminate_failed", "没能停止后台进程")
                    .with("process_stopped", false),
            )
        }
        Ok(()) => {
            let output_settled = process
                .output_readers
                .as_mut()
                .is_none_or(ProcessOutputReaders::settle);
            let status = if output_settled {
                status
            } else {
                BackgroundJobStatus::OutcomeUnknown
            };
            process
                .job
                .as_ref()
                .map_or(Ok(()), |job| {
                    job.transition(status, None).map_err(|error| {
                        process_failure(surface_tool, "job_persist_failed", error)
                            .with("process_stopped", true)
                    })
                })
                .and_then(|()| {
                    if output_settled {
                        Ok(())
                    } else {
                        Err(process_failure(
                            surface_tool,
                            "job_output_unsettled",
                            "进程已停止但输出读取尚未结算",
                        )
                        .with("process_stopped", true))
                    }
                })
        }
    };
    result.map_err(|failure| {
        let mut failure = failure
            .with("terminal_id", process.terminal_id)
            .with("resources_settled", false)
            .instruction("结算尚未完成；稍后重试相同的 kill 核对原 Job，不能重新启动原命令。");
        if let Some(job) = &process.job {
            failure = failure.with("job_id", job.snapshot().job_id);
        }
        PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(process.terminal_id, process);
        failure
    })
}

fn fail_process_start(
    process: ManagedProcess,
    status: BackgroundJobStatus,
    surface_tool: &str,
    mut failure: ToolFailure,
    execution_started: bool,
) -> String {
    if let Some(job) = &process.job {
        failure = failure.with("job_id", job.snapshot().job_id);
    }
    match stop_process(process, status, surface_tool) {
        Ok(()) => failure.with("resources_settled", true),
        Err(settlement) => settlement,
    }
    .with("execution_started", execution_started)
    .into_payload()
}

/// 根进程退出不代表其后代已经释放。先移交在途 guard，再于表锁外回收整棵树。
fn settle_exited_process(terminal_id: u64) -> bool {
    let (mut process, _settlement_guard) = {
        let mut table = PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(process) = table.get(&terminal_id) else {
            return false; // 已被另一个停止操作领取，不能据此声称结算完成。
        };
        let guard = ProcessOperationGuard::from_scope(process.scope.clone());
        (table.remove(&terminal_id).expect("checked process"), guard)
    };
    let result = process.terminate();
    if let Err(error) = &result {
        tracing::error!(terminal_id, %error, "后台 Job 派生进程尚未结算");
    }
    let output_settled = result.is_ok()
        && process
            .output_readers
            .as_mut()
            .is_none_or(ProcessOutputReaders::settle);
    PROCESS_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(terminal_id, process);
    output_settled
}

fn watch_job(
    terminal_id: u64,
    job: Arc<DurableProcessJob>,
    observed_exit: Arc<Mutex<Option<ExitStatus>>>,
) {
    thread::spawn(move || {
        let exit = loop {
            let observed = {
                let mut table = PROCESS_TABLE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match table.get_mut(&terminal_id) {
                    Some(process) => process.poll_exit(),
                    None => break *observed_exit.lock().expect("process exit lock"),
                }
            };
            match observed {
                Ok(Some(status)) => break Some(status),
                Err(error) => {
                    tracing::error!(%error, "后台 Job 退出状态查询失败");
                    break None;
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
            }
        };
        let output_settled = settle_exited_process(terminal_id);
        let (status, code) = match exit {
            Some(exit) if output_settled && exit.success() => {
                (BackgroundJobStatus::Completed, exit.code())
            }
            Some(exit) if output_settled => (BackgroundJobStatus::Failed, exit.code()),
            _ => (BackgroundJobStatus::OutcomeUnknown, None),
        };
        if job.is_cancelling() {
            return;
        }
        if let Err(error) = job.transition(status, code) {
            tracing::error!(%error, "后台 Job 终态未提交，保留原句柄供结算重试；重启后结果未知");
        }
    });
}

pub(super) fn execute_durable_job_action(
    input: &str,
    context: &ToolExecutionContext,
    resources: &crate::ToolRuntimeResources,
) -> Option<String> {
    let request = parse_json_object(input)?;
    let action = field_string(&request, "action")?;
    // 先按动作分派；run 的可选 job_id 不能把新命令变成历史 Job 查询，
    // 未知动作继续由 shell 的统一参数检查拒绝。
    if !matches!(action.as_str(), "read" | "wait" | "write" | "kill" | "list") {
        return None;
    }
    let job_id = (action != "list")
        .then(|| field_string(&request, "job_id"))
        .flatten();
    if job_id.is_none() && action != "list" {
        return None;
    }
    let journal = match resources.background_job_journal.as_ref() {
        Some(journal) => journal,
        None if job_id.is_none() => return None,
        None => {
            return Some(
                process_failure(
                    "shell_exec",
                    "job_unavailable",
                    "当前运行时未提供持久 Job 查询",
                )
                .into_payload(),
            );
        }
    };
    let jobs = match journal.list(context) {
        Ok(jobs) => jobs,
        Err(error) => {
            return Some(process_failure("shell_exec", "job_query_failed", error).into_payload());
        }
    };
    if action == "list" && job_id.is_none() {
        let mut payload: Value = serde_json::from_str(&execute_process_list_with_surface(
            context,
            "shell_exec",
            Some("background_list"),
        ))
        .expect("process list json");
        payload["jobs"] = serde_json::json!(jobs);
        return Some(payload.to_string());
    }
    let job_id = job_id?;
    let Some(mut job) = jobs.into_iter().find(|job| job.job_id == job_id) else {
        return Some(
            process_failure(
                "shell_exec",
                "job_not_found",
                "Job 不存在或不在当前授权范围",
            )
            .into_payload(),
        );
    };
    if action == "wait" {
        let cancellation = super::cancellation::OperationCancellation::new(context, resources);
        let timeout = field_usize(&request, "timeout_ms").unwrap_or(60_000);
        if !(1_000..=60_000).contains(&timeout) {
            return Some(invalid_input(
                "shell_exec",
                "action=wait 的 timeout_ms 必须在 1000–60000 之间",
            ));
        }
        job = match journal.wait_for_terminal(context, &job_id, Duration::from_millis(timeout as u64), Some(cancellation.flag())) {
            Ok(Some(job)) => job,
            Ok(None) => return Some(serde_json::json!({
                "tool":"shell_exec", "status":"cancelled", "job_id":job_id,
                "error_code":"shell_exec_wait_cancelled", "execution_started":false,
                "instruction":"等待已取消；此回执不证明进程已经停止，进程结算由原 Task 取消入口负责。",
            }).to_string()),
            Err(error) => return Some(process_failure("shell_exec", "job_wait_failed", error).into_payload()),
        };
    }
    if action == "read" || action == "wait" {
        use std::io::{Seek, SeekFrom};
        let mut payload = serde_json::json!({"tool":"shell_exec","status":"succeeded","mode":"background_read","job":job,"job_id":job.job_id,"running":!job.status.is_terminal(),"exit_code":job.exit_code});
        if action == "wait" {
            payload["mode"] = "background_wait".into();
            payload["wait_timed_out"] = (!job.status.is_terminal()).into();
            match job.status {
                BackgroundJobStatus::Failed => {
                    payload["status"] = "failed".into();
                    payload["error_code"] = "shell_exec_job_failed".into();
                    payload["error"] = "后台命令以非零退出码结束".into();
                    payload["instruction"] =
                        "根据退出码和输出处理失败，不能把等待结束当成命令成功。".into();
                }
                BackgroundJobStatus::Cancelled => {
                    payload["status"] = "cancelled".into();
                    payload["error_code"] = "shell_exec_job_cancelled".into();
                    payload["instruction"] = "后台命令已取消，保留输出不代表命令已完成。".into();
                }
                BackgroundJobStatus::OutcomeUnknown => {
                    payload["status"] = "indeterminate".into();
                    payload["error_code"] = "shell_exec_job_outcome_unknown".into();
                }
                BackgroundJobStatus::Starting | BackgroundJobStatus::Running => {
                    payload["instruction"] = "等待超时，命令仍未结算；可处理其他工作，依赖结果前须继续等待，不要重新启动命令。".into();
                }
                BackgroundJobStatus::Completed => {}
            }
        }
        let max = field_usize(&request, "max_bytes")
            .unwrap_or(12_000)
            .clamp(512, 200_000);
        for (stream, path) in [("stdout", &job.stdout_path), ("stderr", &job.stderr_path)] {
            let offset = field_usize(&request, &format!("{stream}_offset")).unwrap_or(0) as u64;
            let data = (|| {
                let mut file = std::fs::File::open(path)?;
                let length = file.metadata()?.len();
                let offset = offset.min(length);
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                file.take(max as u64).read_to_end(&mut bytes)?;
                let next = offset + bytes.len() as u64;
                Ok::<_, std::io::Error>((bytes, offset, next, length))
            })();
            match data {
                Ok((bytes, offset, next, length)) => {
                    payload[stream] = String::from_utf8_lossy(&bytes).into_owned().into();
                    payload[format!("{stream}_start_offset")] = offset.into();
                    payload[format!("{stream}_next_offset")] = next.into();
                    payload[format!("{stream}_has_more")] = (next < length).into();
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && job.status == BackgroundJobStatus::Starting =>
                {
                    payload[stream] = "".into();
                    payload["output_pending"] = true.into();
                }
                Err(error) => {
                    return Some(
                        process_failure("shell_exec", "job_output_failed", error.to_string())
                            .into_payload(),
                    );
                }
            }
        }
        if job.status == BackgroundJobStatus::OutcomeUnknown {
            payload["instruction"] =
                "进程连接已丢失，保留的输出不能证明任务完成；先核对副作用，不要自动重放原命令。"
                    .into();
        }
        return Some(payload.to_string());
    }
    if action == "kill"
        && let Some(receipt) = settled_job_stop_receipt(&job)
    {
        return Some(receipt);
    }
    let candidates = {
        let table = PROCESS_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        table
            .values()
            .filter(|process| process_belongs_to_context(process, context))
            .filter_map(|process| {
                process
                    .job
                    .as_ref()
                    .map(|job| (process.terminal_id, job.clone()))
            })
            .collect::<Vec<_>>()
    };
    // snapshot 可能等待正在写盘的 journal，不能在持有全局进程表锁时读取。
    let terminal = candidates.into_iter().find_map(|(terminal, candidate)| {
        (candidate.snapshot().job_id == job_id).then_some(terminal)
    });
    let result = if let Some(terminal) = terminal {
        let mut arguments = request;
        arguments.insert("terminal_id".into(), terminal.into());
        let arguments = Value::Object(arguments).to_string();
        if action == "kill" {
            execute_process_kill_with_surface(
                &arguments,
                context,
                "shell_exec",
                Some("background_kill"),
            )
        } else {
            execute_process_write_with_surface(
                &arguments,
                context,
                "shell_exec",
                Some("background_write"),
            )
        }
    } else {
        process_failure(
            "shell_exec",
            "job_not_attached",
            "Job 没有活动进程连接；可读取历史输出，不能按旧 PID 操作或重放",
        )
        .into_payload()
    };
    // Running 快照之后或找到句柄之后都可能被另一个停止操作抢先结算。
    // 句柄缺失时重读原持久事实；尚未提交或 OutcomeUnknown 仍保留失败回执。
    if action == "kill" {
        let payload: Value = serde_json::from_str(&result).expect("process kill JSON");
        if matches!(
            payload["error_code"].as_str(),
            Some("shell_exec_not_found" | "shell_exec_job_not_attached")
        ) {
            let current_jobs = match journal.list(context) {
                Ok(jobs) => jobs,
                Err(error) => {
                    return Some(
                        process_failure("shell_exec", "job_query_failed", error).into_payload(),
                    );
                }
            };
            if let Some(receipt) = current_jobs
                .iter()
                .find(|current| current.job_id == job_id)
                .and_then(settled_job_stop_receipt)
            {
                return Some(receipt);
            }
        }
    }
    Some(result)
}

fn settled_job_stop_receipt(job: &BackgroundJob) -> Option<String> {
    // 已确认终态只在资源回收后提交；未知结果不能用旧 PID 或短期句柄推断已释放。
    matches!(
        job.status,
        BackgroundJobStatus::Completed | BackgroundJobStatus::Failed | BackgroundJobStatus::Cancelled
    )
    .then(|| serde_json::json!({
        "tool":"shell_exec", "status":"succeeded", "mode":"background_kill",
        "job_id":job.job_id, "job":job, "running":false, "resources_settled":true,
        "summary":"后台 Job 已结算，读取原结算回执",
        "instruction":"停止操作已经完成；命令是否成功仍以原 job.status 和 exitCode 为准，不要重新启动原命令。",
    }).to_string())
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

    fn process_with_output(stdout: JoinHandle<std::io::Result<()>>) -> ManagedProcess {
        let stderr = thread::spawn(|| Ok(()));
        ManagedProcess {
            terminal_id: NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed),
            command: "settlement fixture".into(),
            cwd: String::new(),
            scope: ProcessExecutionScope::default(),
            child: None,
            stdin: None,
            stdout: Arc::new(Mutex::new(OutputBuffer::default())),
            stderr: Arc::new(Mutex::new(OutputBuffer::default())),
            started_at_ms: UtcMillis::now().0,
            job: None,
            observed_exit: Arc::new(Mutex::new(None)),
            output_readers: Some(ProcessOutputReaders::Reading([stdout, stderr])),
        }
    }

    #[test]
    fn stopping_process_waits_for_output_before_reporting_settled() {
        let (release, pending) = std::sync::mpsc::channel();
        let process = process_with_output(thread::spawn(move || {
            pending.recv().unwrap();
            Ok(())
        }));
        let (stopped, result) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            stopped
                .send(stop_process(
                    process,
                    BackgroundJobStatus::Cancelled,
                    "shell_exec",
                ))
                .unwrap();
        });
        let premature = result.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            matches!(premature, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "输出尚未回收时不能返回资源已结算"
        );
        assert!(result.recv_timeout(Duration::from_secs(2)).unwrap().is_ok());
    }

    #[test]
    fn stop_retains_unfinished_output_and_does_not_hide_read_failures_on_retry() {
        for read_error in [false, true] {
            let (release, pending) = std::sync::mpsc::channel();
            let process = process_with_output(thread::spawn(move || {
                pending.recv().unwrap();
                if read_error {
                    Err(std::io::Error::other("output write failed"))
                } else {
                    Ok(())
                }
            }));
            let id = process.terminal_id;
            let error =
                stop_process(process, BackgroundJobStatus::Cancelled, "shell_exec").unwrap_err();
            let receipt: Value = serde_json::from_str(&error.into_payload()).unwrap();
            assert_eq!(receipt["resources_settled"], false);
            release.send(()).unwrap();
            let retained = PROCESS_TABLE
                .lock()
                .unwrap()
                .remove(&id)
                .expect("未完成输出保留原句柄供重试");
            let retry = stop_process(retained, BackgroundJobStatus::Cancelled, "shell_exec");
            assert_eq!(
                retry.is_err(),
                read_error,
                "输出失败不能在读取线程已被 join 后变成成功"
            );
            if read_error {
                let retained = PROCESS_TABLE.lock().unwrap().remove(&id).unwrap();
                assert!(
                    stop_process(retained, BackgroundJobStatus::Cancelled, "shell_exec").is_err()
                );
                PROCESS_TABLE.lock().unwrap().remove(&id);
            }
        }
    }

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
