//! Worker 进程级限制：在读取任何调用报文之前由 Worker 对自身施加，失败即拒绝执行。
//!
//! 限制目标：Worker 只允许在已打开的 stdio 上收发报文和分配内存；禁止新建文件访问、
//! 网络、派生或替换进程。QuickJS 未暴露这些 API 只是第一层，此处是引擎逃逸时的
//! 操作系统级边界。没有对应平台实现的目标直接编译失败，不静默降级。

use std::io;

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::{CString, c_char, c_int},
        io, ptr,
    };

    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    // 默认拒绝；仅放行引擎运行所需的只读系统信息查询。已打开的 stdio 描述符不受路径规则约束。
    const PROFILE: &str =
        "(version 1)(deny default)(allow sysctl-read)(allow process-info-pidinfo (target self))";

    pub fn apply() -> io::Result<()> {
        let profile = CString::new(PROFILE).map_err(io::Error::other)?;
        let mut error: *mut c_char = ptr::null_mut();
        // 0 表示 profile 参数是 SBPL 文本；该接口被 Chromium 等产品用于同一用途。
        let status = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
        if status == 0 {
            return Ok(());
        }
        if !error.is_null() {
            unsafe { sandbox_free_error(error) };
        }
        Err(io::Error::other("无法建立 Worker 操作系统沙箱"))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::io;

    // 拒绝：进程创建/替换、网络、文件系统修改与新路径访问之外的提权相关调用。
    // 以 seccomp 白名单实现：未列出的系统调用返回 EPERM。需要的只有 stdio 读写、内存、
    // 时钟、futex 与退出。
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xC000_00B7;
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xC000_003E;

    #[cfg(target_arch = "aarch64")]
    const ALLOWED: &[libc::c_long] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_lseek,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_gettimeofday,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_set_tid_address,
        libc::SYS_ioctl,
        libc::SYS_fcntl,
        libc::SYS_prlimit64,
        libc::SYS_sched_yield,
        libc::SYS_getrlimit,
        libc::SYS_rseq,
        libc::SYS_set_robust_list,
        libc::SYS_ppoll,
        libc::SYS_pselect6,
        libc::SYS_tgkill,
        libc::SYS_membarrier,
    ];
    #[cfg(target_arch = "x86_64")]
    const ALLOWED: &[libc::c_long] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_lseek,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_gettimeofday,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_set_tid_address,
        libc::SYS_ioctl,
        libc::SYS_fcntl,
        libc::SYS_prlimit64,
        libc::SYS_sched_yield,
        libc::SYS_getrlimit,
        libc::SYS_rseq,
        libc::SYS_set_robust_list,
        libc::SYS_poll,
        libc::SYS_pselect6,
        libc::SYS_tgkill,
        libc::SYS_membarrier,
    ];

    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO_EPERM: u32 = 0x0005_0000 | libc::EPERM as u32;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

    fn ins(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }

    fn filter() -> Vec<libc::sock_filter> {
        let mut program = vec![
            // 架构不符直接杀死进程，避免 32 位 ABI 调用号混淆。
            ins(BPF_LD_W_ABS, 0, 0, 4),
            ins(BPF_JEQ_K, 1, 0, AUDIT_ARCH),
            ins(BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS),
            ins(BPF_LD_W_ABS, 0, 0, 0),
        ];
        for number in ALLOWED {
            program.push(ins(BPF_JEQ_K, 0, 1, *number as u32));
            program.push(ins(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW));
        }
        program.push(ins(BPF_RET_K, 0, 0, SECCOMP_RET_ERRNO_EPERM));
        program
    }

    pub fn apply() -> io::Result<()> {
        let program = filter();
        let prog = libc::sock_fprog {
            len: program.len() as u16,
            filter: program.as_ptr() as *mut libc::sock_filter,
        };
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &prog) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
compile_error!("插件 Worker 需要该平台的操作系统级沙箱实现");

#[cfg(windows)]
mod platform {
    use std::io;

    // Worker 自身的 AppContainer/受限令牌尚未实现：`ENFORCED` 为 false，发行校验会如实
    // 报告，阶段 A 门禁在 Windows 上保持未关闭，见方案 §5.1。
    pub fn apply() -> io::Result<()> {
        Ok(())
    }
}

/// 对当前进程施加不可撤销的限制。必须在读取调用报文之前调用。
pub fn apply() -> io::Result<()> {
    platform::apply()
}

/// 当前平台的进程级限制是否由 Worker 自身强制执行。
pub const ENFORCED: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// 施加限制后逐项尝试被禁止的操作，返回 `(操作, 是否被拒绝)`。
fn probe() -> Vec<(&'static str, bool)> {
    use std::{fs, net::TcpStream};
    let path = std::env::temp_dir().join(format!("magi-sandbox-{}", std::process::id()));
    vec![
        ("readFile", fs::read("/etc/hosts").is_err()),
        ("writeFile", fs::write(&path, b"x").is_err()),
        ("createDir", fs::create_dir(&path).is_err()),
        ("network", TcpStream::connect(("127.0.0.1", 9)).is_err()),
        (
            "spawn",
            magi_process::std_command("/bin/echo")
                .arg("x")
                .output()
                .is_err(),
        ),
    ]
}

/// 发行校验入口：在本进程施加限制并证明被禁止的操作确实失败。
pub fn selftest() -> io::Result<serde_json::Value> {
    apply()?;
    let report = probe();
    if ENFORCED && let Some((name, _)) = report.iter().find(|(_, denied)| !denied) {
        return Err(io::Error::other(format!("沙箱未拒绝 {name}")));
    }
    Ok(serde_json::json!({
        "enforced": ENFORCED,
        "denied": report.into_iter().filter(|(_, d)| *d).map(|(n, _)| n).collect::<Vec<_>>(),
    }))
}
