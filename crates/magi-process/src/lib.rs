use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
    sync::{
        Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(unix)]
use std::{
    io::Read,
    os::unix::ffi::OsStringExt,
    os::unix::process::CommandExt,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[cfg(unix)]
const LOGIN_ENVIRONMENT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const LOGIN_ENVIRONMENT_MAX_BYTES: u64 = 1024 * 1024;

static PROCESS_ENVIRONMENT: OnceLock<RwLock<ProcessEnvironment>> = OnceLock::new();
static NEXT_MANAGED_PROCESS_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_MANAGED_PROCESSES: OnceLock<Mutex<BTreeMap<u64, ManagedProcessTerminator>>> =
    OnceLock::new();

pub struct ManagedChild {
    child: Child,
    registration_id: u64,
    #[cfg(windows)]
    job_handle: Arc<WindowsJobHandle>,
}

pub struct AsyncManagedChild {
    child: tokio::process::Child,
    registration_id: u64,
    #[cfg(windows)]
    job_handle: Arc<WindowsJobHandle>,
}

#[cfg(windows)]
struct WindowsJobHandle(isize);

#[derive(Clone)]
struct ManagedProcessTerminator {
    #[cfg(unix)]
    process_group_id: u32,
    #[cfg(windows)]
    job_handle: Arc<WindowsJobHandle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEnvironmentSummary {
    pub source: &'static str,
    pub path: Option<OsString>,
}

#[cfg(unix)]
pub fn user_shell() -> OsString {
    std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default_user_shell)
}

#[cfg(windows)]
pub fn user_shell() -> OsString {
    default_user_shell()
}

pub fn resolve_executable(program: impl AsRef<OsStr>) -> Option<PathBuf> {
    let program = Path::new(program.as_ref());
    if program.components().count() > 1 {
        return executable_file(program).then(|| program.to_path_buf());
    }

    let environment = process_environment_snapshot();
    let path = environment
        .overrides
        .get(OsStr::new("PATH"))
        .cloned()
        .or_else(|| std::env::var_os("PATH"))?;
    for directory in std::env::split_paths(&path) {
        #[cfg(windows)]
        for candidate in windows_executable_candidates(&directory, program) {
            if executable_file(&candidate) {
                return Some(candidate);
            }
        }
        #[cfg(not(windows))]
        {
            let candidate = directory.join(program);
            if executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[derive(Clone, Debug)]
struct ProcessEnvironment {
    overrides: BTreeMap<OsString, OsString>,
    source: &'static str,
}

/// 初始化 Magi 子进程统一使用的用户执行环境。
///
/// Desktop 从 Finder、开始菜单或桌面环境启动时，进程通常拿不到用户终端中的 PATH。
/// Unix 平台在产品入口启动阶段读取一次交互式登录 Shell 环境并缓存，确保 `.zshrc`、
/// `.bashrc` 等用户终端配置中的 PATH 也能进入 Desktop；Windows 使用系统传入的
/// 用户/机器环境。所有后续同步和异步子进程都复用同一份快照。
pub fn initialize_user_process_environment() -> ProcessEnvironmentSummary {
    process_environment_summary(false)
}

/// 重新读取登录 Shell 环境并刷新共享命令环境快照。
///
/// 该操作只更新 Magi 后续子进程使用的环境，不会执行任何用户命令，也不会安装或升级
/// 外部程序。用户在 Magi 运行期间安装命令、修改 Shell 配置或调整 PATH 后，应通过此
/// 入口让后续会话立即看到新环境。
pub fn refresh_user_process_environment() -> ProcessEnvironmentSummary {
    process_environment_summary(true)
}

/// 返回当前平台常见的开发命令名称，用于诊断面板展示；实际执行前仍会按命令逐项预检。
pub fn common_command_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "powershell",
            "pwsh",
            "git",
            "cargo",
            "rustc",
            "node",
            "npm",
            "pnpm",
            "python",
            "python3",
            "rg",
            "grep",
        ]
    }
    #[cfg(not(windows))]
    {
        &[
            "sh", "bash", "zsh", "fish", "git", "cargo", "rustc", "node", "npm", "pnpm", "python",
            "python3", "rg", "grep",
        ]
    }
}

/// 创建继承 Magi 平台进程策略的同步子进程命令。
pub fn std_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    apply_process_environment(&mut command);
    apply_platform_policy(&mut command);
    command
}

/// 创建继承 Magi 平台进程策略的异步子进程命令。
pub fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    apply_process_environment(command.as_std_mut());
    apply_platform_policy(command.as_std_mut());
    command
}

/// 启动一个可整体终止子进程树的同步进程。
///
/// Unix 使用独立进程组，Windows 使用 Job Object。调用方停止会话、关闭服务或释放
/// 进程句柄时，都能终止该命令派生的完整进程树，而不是只终止最外层 Shell。
pub fn spawn_managed(command: &mut Command) -> io::Result<ManagedChild> {
    prepare_managed_std_command(command);
    let child = command.spawn()?;
    #[cfg(windows)]
    let (child, job_handle) = {
        let mut child = child;
        let job_handle = match WindowsJobHandle::assign_std_child(&child).map(Arc::new) {
            Ok(job_handle) => job_handle,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Err(error) = resume_windows_process(child.id()) {
            let _ = job_handle.terminate();
            let _ = child.wait();
            return Err(error);
        }
        (child, job_handle)
    };
    #[cfg(unix)]
    let registration_id = register_managed_process(ManagedProcessTerminator {
        process_group_id: child.id(),
    });
    #[cfg(windows)]
    let registration_id = register_managed_process(ManagedProcessTerminator {
        job_handle: job_handle.clone(),
    });
    Ok(ManagedChild {
        child,
        registration_id,
        #[cfg(windows)]
        job_handle,
    })
}

/// 启动一个可整体终止子进程树的异步进程。
pub fn spawn_managed_tokio(command: &mut tokio::process::Command) -> io::Result<AsyncManagedChild> {
    prepare_managed_std_command(command.as_std_mut());
    let child = command.spawn()?;
    #[cfg(windows)]
    let (child, job_handle) = {
        let mut child = child;
        let job_handle = match WindowsJobHandle::assign_tokio_child(&child).map(Arc::new) {
            Ok(job_handle) => job_handle,
            Err(error) => {
                let _ = child.start_kill();
                return Err(error);
            }
        };
        let process_id = child.id().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "异步子进程在恢复执行前已退出")
        })?;
        if let Err(error) = resume_windows_process(process_id) {
            let _ = job_handle.terminate();
            let _ = child.start_kill();
            return Err(error);
        }
        (child, job_handle)
    };
    #[cfg(unix)]
    let registration_id = register_managed_process(ManagedProcessTerminator {
        process_group_id: child.id().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "异步子进程在注册受管进程前已退出",
            )
        })?,
    });
    #[cfg(windows)]
    let registration_id = register_managed_process(ManagedProcessTerminator {
        job_handle: job_handle.clone(),
    });
    Ok(AsyncManagedChild {
        child,
        registration_id,
        #[cfg(windows)]
        job_handle,
    })
}

impl ManagedChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn stdin_mut(&mut self) -> Option<&mut ChildStdin> {
        self.child.stdin.as_mut()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.unregister();
        }
        Ok(status)
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.unregister();
        Ok(status)
    }

    pub fn terminate(&mut self) -> io::Result<ExitStatus> {
        let termination = terminate_std_process_tree(self);
        let wait = self.child.wait();
        if wait.is_ok() {
            self.unregister();
        }
        termination.and(wait)
    }

    fn unregister(&mut self) {
        unregister_managed_process(self.registration_id);
        self.registration_id = 0;
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = terminate_std_process_tree(self);
            let _ = self.child.wait();
        } else {
            let _ = terminate_std_process_tree(self);
        }
        self.unregister();
    }
}

impl AsyncManagedChild {
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.unregister();
        }
        Ok(status)
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait().await?;
        self.unregister();
        Ok(status)
    }

    pub fn start_terminate(&mut self) -> io::Result<()> {
        terminate_async_process_tree(self)
    }

    pub async fn terminate(&mut self) -> io::Result<ExitStatus> {
        let termination = terminate_async_process_tree(self);
        let wait = self.child.wait().await;
        if wait.is_ok() {
            self.unregister();
        }
        termination.and(wait)
    }

    fn unregister(&mut self) {
        unregister_managed_process(self.registration_id);
        self.registration_id = 0;
    }
}

impl Drop for AsyncManagedChild {
    fn drop(&mut self) {
        let _ = terminate_async_process_tree(self);
        self.unregister();
    }
}

fn active_managed_processes() -> &'static Mutex<BTreeMap<u64, ManagedProcessTerminator>> {
    ACTIVE_MANAGED_PROCESSES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn register_managed_process(terminator: ManagedProcessTerminator) -> u64 {
    let registration_id = NEXT_MANAGED_PROCESS_ID.fetch_add(1, Ordering::Relaxed);
    active_managed_processes()
        .lock()
        .expect("managed process registry lock poisoned")
        .insert(registration_id, terminator);
    registration_id
}

fn unregister_managed_process(registration_id: u64) {
    if registration_id == 0 {
        return;
    }
    active_managed_processes()
        .lock()
        .expect("managed process registry lock poisoned")
        .remove(&registration_id);
}

/// 终止当前 Magi 进程创建的全部受管子进程树。
///
/// daemon 优雅退出和异常收口都调用此入口，覆盖工具 Shell、后台进程、MCP、Tunnel、
/// Vite 以及一次性本地执行器，避免某条调用栈阻塞时留下孤儿进程。
pub fn terminate_all_managed_processes() -> usize {
    let processes = {
        let mut registry = active_managed_processes()
            .lock()
            .expect("managed process registry lock poisoned");
        std::mem::take(&mut *registry)
            .into_values()
            .collect::<Vec<_>>()
    };
    let count = processes.len();

    #[cfg(unix)]
    {
        for process in &processes {
            let _ = send_unix_process_group_signal(process.process_group_id, libc::SIGTERM);
        }
        if !processes.is_empty() {
            thread::sleep(Duration::from_millis(50));
        }
        for process in &processes {
            let _ = send_unix_process_group_signal(process.process_group_id, libc::SIGKILL);
        }
    }

    #[cfg(windows)]
    for process in &processes {
        let _ = process.job_handle.terminate();
    }

    count
}

fn prepare_managed_std_command(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
}

#[cfg(unix)]
fn terminate_std_process_tree(process: &mut ManagedChild) -> io::Result<()> {
    let tree_termination = terminate_unix_process_group(process.child.id());
    let parent_termination = match process.child.kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(error),
    };
    let _ = parent_termination;
    tree_termination
}

#[cfg(windows)]
fn terminate_std_process_tree(process: &mut ManagedChild) -> io::Result<()> {
    let tree_termination = process.job_handle.terminate();
    let parent_termination = match process.child.kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(error),
    };
    let _ = parent_termination;
    tree_termination
}

#[cfg(unix)]
fn terminate_async_process_tree(process: &mut AsyncManagedChild) -> io::Result<()> {
    let tree_termination = process
        .child
        .id()
        .map(terminate_unix_process_group)
        .unwrap_or(Ok(()));
    let parent_termination = match process.child.start_kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(error),
    };
    let _ = parent_termination;
    tree_termination
}

#[cfg(windows)]
fn terminate_async_process_tree(process: &mut AsyncManagedChild) -> io::Result<()> {
    let tree_termination = process.job_handle.terminate();
    let parent_termination = match process.child.start_kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(error),
    };
    let _ = parent_termination;
    tree_termination
}

#[cfg(unix)]
fn terminate_unix_process_group(pid: u32) -> io::Result<()> {
    send_unix_process_group_signal(pid, libc::SIGTERM)?;
    thread::sleep(Duration::from_millis(50));
    match send_unix_process_group_signal(pid, libc::SIGKILL) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn send_unix_process_group_signal(pid: u32, signal: libc::c_int) -> io::Result<()> {
    let process_group = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "进程 ID 超出系统范围"))?;
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(windows)]
impl WindowsJobHandle {
    fn assign_std_child(child: &Child) -> io::Result<Self> {
        Self::assign_raw_process_handle(child.as_raw_handle() as isize)
    }

    fn assign_tokio_child(child: &tokio::process::Child) -> io::Result<Self> {
        let process_handle = child.raw_handle().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "异步子进程在绑定 Windows Job Object 前已退出",
            )
        })?;
        Self::assign_raw_process_handle(process_handle as isize)
    }

    fn assign_raw_process_handle(process_handle: isize) -> io::Result<Self> {
        use std::{mem::size_of, ptr};
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
        };

        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const information).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(job);
            }
            return Err(error);
        }
        let assigned = unsafe { AssignProcessToJobObject(job, process_handle as HANDLE) };
        if assigned == 0 {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(job);
            }
            return Err(error);
        }
        Ok(Self(job as isize))
    }

    fn terminate(&self) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        let terminated = unsafe { TerminateJobObject(self.0 as _, 1) };
        if terminated == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
fn resume_windows_process(process_id: u32) -> io::Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut found = false;
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while has_entry {
        if entry.th32OwnerProcessID == process_id {
            found = true;
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                let error = io::Error::last_os_error();
                unsafe { CloseHandle(snapshot) };
                return Err(error);
            }
            let resume_result = unsafe { ResumeThread(thread) };
            unsafe { CloseHandle(thread) };
            if resume_result == u32::MAX {
                let error = io::Error::last_os_error();
                unsafe { CloseHandle(snapshot) };
                return Err(error);
            }
        }
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    if found {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "未找到 Windows 子进程的初始线程",
        ))
    }
}

#[cfg(windows)]
impl Drop for WindowsJobHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        unsafe {
            CloseHandle(self.0 as _);
        }
    }
}

fn process_environment_lock() -> &'static RwLock<ProcessEnvironment> {
    PROCESS_ENVIRONMENT.get_or_init(|| RwLock::new(resolve_process_environment()))
}

fn process_environment_snapshot() -> ProcessEnvironment {
    process_environment_lock()
        .read()
        .expect("Magi process environment lock poisoned")
        .clone()
}

fn process_environment_summary(refresh: bool) -> ProcessEnvironmentSummary {
    let lock = process_environment_lock();
    if refresh {
        let mut environment = lock
            .write()
            .expect("Magi process environment lock poisoned");
        *environment = resolve_process_environment();
    }
    let environment = lock.read().expect("Magi process environment lock poisoned");
    ProcessEnvironmentSummary {
        source: environment.source,
        path: environment
            .overrides
            .get(OsStr::new("PATH"))
            .cloned()
            .or_else(|| std::env::var_os("PATH")),
    }
}

fn resolve_process_environment() -> ProcessEnvironment {
    #[cfg(unix)]
    {
        let inherited = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let (mut runtime_environment, source) = match capture_login_shell_environment() {
            Some(environment) => (environment, "login_shell"),
            None => (inherited.clone(), "inherited"),
        };
        augment_standard_unix_path(&mut runtime_environment, &inherited);
        ProcessEnvironment {
            overrides: environment_overrides(&inherited, runtime_environment),
            source,
        }
    }

    #[cfg(not(unix))]
    ProcessEnvironment {
        overrides: BTreeMap::new(),
        source: "inherited",
    }
}

#[cfg(unix)]
fn augment_standard_unix_path(
    environment: &mut BTreeMap<OsString, OsString>,
    inherited: &BTreeMap<OsString, OsString>,
) {
    let Some(current_path) = environment
        .get(OsStr::new("PATH"))
        .cloned()
        .or_else(|| inherited.get(OsStr::new("PATH")).cloned())
    else {
        return;
    };

    let mut paths = std::env::split_paths(&current_path).collect::<Vec<_>>();
    let mut standard_paths = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/sbin"),
    ];
    if let Some(home) = environment
        .get(OsStr::new("HOME"))
        .cloned()
        .or_else(|| inherited.get(OsStr::new("HOME")).cloned())
    {
        let home = PathBuf::from(home);
        standard_paths.extend([
            home.join(".cargo/bin"),
            home.join(".local/bin"),
            home.join(".volta/bin"),
            home.join("Library/pnpm"),
            home.join(".npm-global/bin"),
        ]);
    }
    for path in standard_paths {
        if !paths.iter().any(|existing| existing == &path) {
            paths.push(path);
        }
    }
    if let Ok(updated_path) = std::env::join_paths(paths) {
        environment.insert(OsString::from("PATH"), updated_path);
    }
}

#[cfg(unix)]
fn environment_overrides(
    inherited: &BTreeMap<OsString, OsString>,
    login_environment: BTreeMap<OsString, OsString>,
) -> BTreeMap<OsString, OsString> {
    login_environment
        .into_iter()
        .filter(|(key, value)| {
            login_environment_variable_is_stable(key) && inherited.get(key) != Some(value)
        })
        .collect()
}

#[cfg(unix)]
fn login_environment_variable_is_stable(key: &OsStr) -> bool {
    !matches!(
        key.to_str(),
        Some(
            "PWD" | "OLDPWD" | "SHLVL" | "_" | "RANDOM" | "SECONDS" | "LINENO" | "ZSH_EVAL_CONTEXT"
        )
    )
}

fn apply_process_environment(command: &mut Command) {
    let environment = process_environment_snapshot();
    command.envs(environment.overrides.iter());
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(windows)]
fn windows_executable_candidates(directory: &Path, program: &Path) -> Vec<PathBuf> {
    if program.extension().is_some() {
        return vec![directory.join(program)];
    }
    let extensions =
        std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
    extensions
        .to_string_lossy()
        .split(';')
        .filter(|extension| !extension.trim().is_empty())
        .map(|extension| directory.join(format!("{}{}", program.to_string_lossy(), extension)))
        .collect()
}

#[cfg(unix)]
fn capture_login_shell_environment() -> Option<BTreeMap<OsString, OsString>> {
    let shell = user_shell();
    let mut child = Command::new(shell)
        .args(["-ilc", "printf '\\0'; env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let output_reader = thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout
            .take(LOGIN_ENVIRONMENT_MAX_BYTES)
            .read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + LOGIN_ENVIRONMENT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => {
                let _ = output_reader.join();
                return None;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_reader.join();
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
        }
    }

    let output = output_reader.join().ok()?;
    let environment = parse_null_delimited_environment(&output);
    environment
        .contains_key(OsStr::new("PATH"))
        .then_some(environment)
}

#[cfg(unix)]
fn default_user_shell() -> OsString {
    if cfg!(target_os = "macos") {
        OsString::from("/bin/zsh")
    } else {
        OsString::from("/bin/sh")
    }
}

#[cfg(windows)]
fn default_user_shell() -> OsString {
    OsString::from("powershell.exe")
}

#[cfg(unix)]
fn parse_null_delimited_environment(output: &[u8]) -> BTreeMap<OsString, OsString> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let separator = entry.iter().position(|byte| *byte == b'=')?;
            if separator == 0 {
                return None;
            }
            Some((
                OsString::from_vec(entry[..separator].to_vec()),
                OsString::from_vec(entry[separator + 1..].to_vec()),
            ))
        })
        .collect()
}

fn apply_platform_policy(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, path::Path};

    #[cfg(unix)]
    use std::{collections::BTreeMap, os::unix::ffi::OsStringExt};

    #[cfg(unix)]
    use super::environment_overrides;
    use super::{
        initialize_user_process_environment, spawn_managed, spawn_managed_tokio, std_command,
        tokio_command,
    };

    #[cfg(unix)]
    fn configure_long_running_std_command(command: &mut std::process::Command) {
        command.args(["-c", "sleep 5"]);
    }

    #[cfg(windows)]
    fn configure_long_running_std_command(command: &mut std::process::Command) {
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 5",
        ]);
    }

    #[cfg(unix)]
    fn configure_long_running_tokio_command(command: &mut tokio::process::Command) {
        command.args(["-c", "sleep 5"]);
    }

    #[cfg(windows)]
    fn configure_long_running_tokio_command(command: &mut tokio::process::Command) {
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 5",
        ]);
    }

    #[test]
    fn factories_preserve_the_requested_program() {
        assert_eq!(std_command("magi-test").get_program(), "magi-test");
        assert_eq!(
            tokio_command("magi-async-test").as_std().get_program(),
            "magi-async-test"
        );
    }

    #[test]
    fn process_environment_initialization_exposes_path() {
        let summary = initialize_user_process_environment();
        assert!(matches!(summary.source, "login_shell" | "inherited"));
        assert!(summary.path.is_some());
    }

    #[cfg(windows)]
    #[test]
    fn windows_user_shell_is_powershell() {
        assert_eq!(super::user_shell(), OsString::from("powershell.exe"));
    }

    #[test]
    fn common_command_catalog_uses_platform_native_shell_names() {
        let commands = super::common_command_names();

        assert!(commands.contains(&"git"));
        assert!(commands.contains(&"rg"));
        if cfg!(windows) {
            assert!(commands.contains(&"powershell"));
            assert!(!commands.contains(&"cmd"));
            assert!(!commands.contains(&"zsh"));
        } else {
            assert!(commands.contains(&"sh"));
            assert!(!commands.contains(&"cmd"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn child_shell_receives_the_initialized_path_without_login_reset() {
        let summary = initialize_user_process_environment();
        let output = std_command(super::user_shell())
            .args(["-c", "printf '%s' \"$PATH\""])
            .output()
            .expect("user shell should start");

        assert!(output.status.success());
        assert_eq!(
            OsString::from_vec(output.stdout),
            summary.path.expect("initialized PATH")
        );
    }

    #[test]
    fn executable_resolution_uses_the_initialized_environment() {
        assert!(super::resolve_executable(super::user_shell()).is_some());
        assert!(super::resolve_executable("magi-command-that-does-not-exist").is_none());
    }

    #[test]
    fn managed_process_termination_does_not_wait_for_natural_exit() {
        let mut command = std_command(super::user_shell());
        configure_long_running_std_command(&mut command);
        let mut child = spawn_managed(&mut command).expect("managed child should start");
        let started = std::time::Instant::now();

        child.terminate().expect("managed child should terminate");

        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "managed process termination should be prompt"
        );
    }

    #[tokio::test]
    async fn async_managed_process_termination_does_not_wait_for_natural_exit() {
        let mut command = tokio_command(super::user_shell());
        configure_long_running_tokio_command(&mut command);
        let mut child =
            spawn_managed_tokio(&mut command).expect("async managed child should start");
        let started = std::time::Instant::now();

        child
            .terminate()
            .await
            .expect("async managed child should terminate");

        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "async managed process termination should be prompt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_environment_only_overrides_changed_stable_variables() {
        let inherited = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (
                OsString::from("MAGI_STATE_ROOT"),
                OsString::from("/tmp/magi"),
            ),
            (OsString::from("PWD"), OsString::from("/workspace")),
        ]);
        let login = BTreeMap::from([
            (
                OsString::from("PATH"),
                OsString::from("/opt/homebrew/bin:/usr/bin"),
            ),
            (
                OsString::from("MAGI_STATE_ROOT"),
                OsString::from("/tmp/magi"),
            ),
            (OsString::from("PWD"), OsString::from("/Users/test")),
            (
                OsString::from("VOLTA_HOME"),
                OsString::from("/Users/test/.volta"),
            ),
        ]);

        let overrides = environment_overrides(&inherited, login);

        assert_eq!(
            overrides.get(&OsString::from("PATH")),
            Some(&OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
        assert!(!overrides.contains_key(&OsString::from("MAGI_STATE_ROOT")));
        assert!(!overrides.contains_key(&OsString::from("PWD")));
        assert_eq!(
            overrides.get(&OsString::from("VOLTA_HOME")),
            Some(&OsString::from("/Users/test/.volta"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn standard_unix_tool_paths_are_added_without_replacing_user_path() {
        let inherited = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("HOME"), OsString::from("/Users/test")),
        ]);
        let mut environment = inherited.clone();

        super::augment_standard_unix_path(&mut environment, &inherited);

        let path = environment
            .get(&OsString::from("PATH"))
            .expect("PATH should remain available")
            .to_string_lossy();
        assert!(path.contains("/usr/bin"));
        assert!(path.contains("/opt/homebrew/bin"));
        assert!(path.contains("/Users/test/.cargo/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn null_delimited_environment_ignores_profile_output_noise() {
        let parsed = super::parse_null_delimited_environment(
            b"profile banner\n\0PATH=/opt/homebrew/bin:/usr/bin\0HOME=/Users/test\0",
        );

        assert_eq!(
            parsed.get(&OsString::from("PATH")),
            Some(&OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
        assert_eq!(
            parsed.get(&OsString::from("HOME")),
            Some(&OsString::from("/Users/test"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn std_factory_runs_windows_powershell_commands() {
        let status = std_command("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "exit 0",
            ])
            .status()
            .expect("PowerShell should start through the shared process factory");
        assert!(status.success());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn tokio_factory_runs_windows_powershell_commands() {
        let status = tokio_command("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "exit 0",
            ])
            .status()
            .await
            .expect("PowerShell should start through the shared async process factory");
        assert!(status.success());
    }

    #[test]
    fn production_code_uses_the_shared_process_factory() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("magi-process should live under <workspace>/crates");
        let mut violations = Vec::new();

        for source_root in [workspace_root.join("apps"), workspace_root.join("crates")] {
            collect_direct_command_constructors(&source_root, &mut violations);
        }

        assert!(
            violations.is_empty(),
            "生产代码必须通过 magi-process 创建子进程，仍存在直接构造：\n{}",
            violations.join("\n")
        );
    }

    fn collect_direct_command_constructors(root: &Path, violations: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.ends_with("magi-process") || path.ends_with("tests") {
                    continue;
                }
                collect_direct_command_constructors(&path, violations);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("rs")
                || path.file_name().and_then(|value| value.to_str()) == Some("tests.rs")
            {
                continue;
            }
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            for (line_index, line) in source.lines().enumerate() {
                if [
                    "Command::new(",
                    "StdCommand::new(",
                    "std::process::Command::new(",
                    "tokio::process::Command::new(",
                ]
                .iter()
                .any(|pattern| line.contains(pattern))
                {
                    violations.push(format!(
                        "{}:{}: {}",
                        path.strip_prefix(workspace_root(root))
                            .unwrap_or(&path)
                            .display(),
                        line_index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    fn workspace_root(path: &Path) -> &Path {
        path.ancestors()
            .find(|candidate| {
                candidate.join("Cargo.toml").is_file() && candidate.join("crates").is_dir()
            })
            .unwrap_or(path)
    }
}
