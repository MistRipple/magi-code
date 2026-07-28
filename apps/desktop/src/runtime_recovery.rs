use magi_runtime_state::RuntimeStateManager;
use netstat2::{
    AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpSocketInfo, TcpState,
    get_sockets_info,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process, thread,
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessesToUpdate, Signal, System};

const PROCESS_STOP_TIMEOUT: Duration = Duration::from_secs(3);
const PROCESS_STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortOccupant {
    pub pid: u32,
    pub process_name: String,
    pub executable_path: Option<String>,
    pub started_at: u64,
    pub is_magi: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortDiagnosis {
    pub port: u16,
    pub listener_detected: bool,
    pub occupants: Vec<PortOccupant>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedPortOccupant {
    pub pid: u32,
    pub started_at: u64,
}

pub fn diagnose_port(port: u16, state_root: &Path) -> Result<PortDiagnosis, String> {
    let socket_info = get_sockets_info(AddressFamilyFlags::all(), ProtocolFlags::TCP)
        .map_err(|error| format!("读取端口 {port} 的监听信息失败: {error}"))?;
    let mut listener_detected = false;
    let mut pids = BTreeSet::new();
    for socket in socket_info {
        if matches!(
            socket.protocol_socket_info,
            ProtocolSocketInfo::Tcp(TcpSocketInfo {
                local_port,
                state: TcpState::Listen,
                ..
            }) if local_port == port
        ) {
            listener_detected = true;
            pids.extend(socket.associated_pids);
        }
    }

    let runtime_state = RuntimeStateManager::new(state_root.join("runtime")).read_runtime_state();
    let runtime_pid = runtime_state
        .filter(|state| state.port == port)
        .map(|state| state.pid);
    let mut system = System::new();
    let sysinfo_pids = pids.iter().copied().map(Pid::from_u32).collect::<Vec<_>>();
    if !sysinfo_pids.is_empty() {
        system.refresh_processes(ProcessesToUpdate::Some(&sysinfo_pids), true);
    }

    let occupants = pids
        .into_iter()
        .map(|pid| {
            let process = system.process(Pid::from_u32(pid));
            let process_name = process
                .map(|process| process.name().to_string_lossy().into_owned())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("PID {pid}"));
            let executable_path = process
                .and_then(|process| process.exe())
                .map(|path| path.to_string_lossy().into_owned());
            let started_at = process
                .map(|process| process.start_time())
                .unwrap_or_default();
            let is_magi = runtime_pid == Some(pid)
                && process_identity_is_magi(&process_name, executable_path.as_deref());
            PortOccupant {
                pid,
                process_name,
                executable_path,
                started_at,
                is_magi,
            }
        })
        .collect();

    Ok(PortDiagnosis {
        port,
        listener_detected,
        occupants,
    })
}

pub fn magi_health_available(port: u16) -> bool {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, HEALTH_PROBE_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(HEALTH_PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HEALTH_PROBE_TIMEOUT));
    use std::io::{Read, Write};
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = Vec::with_capacity(1024);
    if stream.take(16 * 1024).read_to_end(&mut response).is_err() {
        return false;
    }
    let response = String::from_utf8_lossy(&response);
    response.starts_with("HTTP/1.1 200")
        && response.contains("\"serviceName\":\"magi-rust-backend\"")
}

pub fn terminate_port_occupants(
    port: u16,
    state_root: &Path,
    expected: &[ExpectedPortOccupant],
    allow_external: bool,
) -> Result<(), String> {
    let diagnosis = diagnose_port(port, state_root)?;
    if !diagnosis.listener_detected {
        return Ok(());
    }
    if diagnosis.occupants.is_empty() {
        return Err(format!(
            "端口 {port} 正在被占用，但系统未允许读取监听进程，无法安全终止"
        ));
    }

    let actual_identity = diagnosis
        .occupants
        .iter()
        .map(|occupant| (occupant.pid, occupant.started_at))
        .collect::<BTreeSet<_>>();
    let expected_identity = expected
        .iter()
        .map(|occupant| (occupant.pid, occupant.started_at))
        .collect::<BTreeSet<_>>();
    if actual_identity != expected_identity {
        return Err(format!(
            "端口 {port} 的占用进程已经变化，为避免结束错误进程，请重新确认"
        ));
    }
    if diagnosis.occupants.iter().any(|occupant| !occupant.is_magi) && !allow_external {
        return Err(format!(
            "端口 {port} 被其他程序占用，需要用户确认后才能结束"
        ));
    }
    if diagnosis
        .occupants
        .iter()
        .any(|occupant| occupant.pid == process::id())
    {
        return Err("不能通过端口恢复操作终止当前 Magi 桌面进程".to_string());
    }

    for occupant in &diagnosis.occupants {
        let current = diagnose_port(port, state_root)?;
        if !current.listener_detected {
            break;
        }
        let Some(current_occupant) = current
            .occupants
            .iter()
            .find(|current| current.pid == occupant.pid)
        else {
            continue;
        };
        if current_occupant.started_at != occupant.started_at {
            return Err(format!(
                "PID {} 的进程身份已经变化，为避免结束错误进程，请重新确认",
                occupant.pid
            ));
        }
        if current_occupant.is_magi != occupant.is_magi {
            return Err(format!(
                "PID {} 的 Magi 进程身份已经变化，为避免结束错误进程，请重新确认",
                occupant.pid
            ));
        }
        stop_process(current_occupant)?;
    }
    wait_for_port_release(port, state_root, PROCESS_STOP_TIMEOUT)
}

pub fn wait_for_port_release(
    port: u16,
    state_root: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let diagnosis = diagnose_port(port, state_root)?;
        if !diagnosis.listener_detected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("等待端口 {port} 释放超时"));
        }
        thread::sleep(PROCESS_STOP_POLL_INTERVAL);
    }
}

fn stop_process(expected: &PortOccupant) -> Result<(), String> {
    let pid = Pid::from_u32(expected.pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let Some(target) = system.process(pid) else {
        return Ok(());
    };
    if target.start_time() != expected.started_at {
        return Err(format!(
            "PID {} 已被新的进程复用，为避免误操作已停止恢复",
            expected.pid
        ));
    }

    let term_sent = target.kill_with(Signal::Term).unwrap_or(false);
    if term_sent && wait_for_process_exit(expected.pid, PROCESS_STOP_TIMEOUT) {
        return Ok(());
    }

    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let Some(target) = system.process(pid) else {
        return Ok(());
    };
    if target.start_time() != expected.started_at {
        return Err(format!("PID {} 在强制终止前发生变化", expected.pid));
    }
    if !target.kill() {
        return Err(format!(
            "无法结束进程 {}（PID {}），请检查当前用户权限",
            expected.process_name, expected.pid
        ));
    }
    if wait_for_process_exit(expected.pid, PROCESS_STOP_TIMEOUT) {
        Ok(())
    } else {
        Err(format!(
            "进程 {}（PID {}）在强制终止后仍未退出",
            expected.process_name, expected.pid
        ))
    }
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        if system.process(pid).is_none() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(PROCESS_STOP_POLL_INTERVAL);
    }
}

fn process_identity_is_magi(process_name: &str, executable_path: Option<&str>) -> bool {
    let mut candidates = vec![process_name.to_string()];
    if let Some(path) = executable_path {
        candidates.push(
            PathBuf::from(path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    candidates.into_iter().any(|candidate| {
        matches!(
            candidate.trim().to_ascii_lowercase().as_str(),
            "magi" | "magi-desktop" | "magi-daemon-app"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn temp_state_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("magi-desktop-recovery-{name}-{}", process::id()))
    }

    #[test]
    fn diagnosis_finds_the_exact_listening_process() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should resolve")
            .port();
        let state_root = temp_state_root("listener");
        let runtime = RuntimeStateManager::new(state_root.join("runtime"));
        runtime.write_runtime_state(process::id(), Some("127.0.0.1"), port);

        let diagnosis = diagnose_port(port, &state_root).expect("port diagnosis should succeed");
        assert!(diagnosis.listener_detected);
        let current = diagnosis
            .occupants
            .iter()
            .find(|occupant| occupant.pid == process::id())
            .expect("current process should own the listener");
        assert!(!current.process_name.is_empty());

        let _ = std::fs::remove_dir_all(state_root);
    }

    #[test]
    fn unrelated_process_identity_is_not_classified_as_magi() {
        assert!(!process_identity_is_magi("node", Some("/usr/bin/node")));
        assert!(process_identity_is_magi("Magi", None));
        assert!(process_identity_is_magi(
            "unknown",
            Some("/Applications/Magi.app/Contents/MacOS/Magi")
        ));
    }

    #[test]
    fn cleanup_never_terminates_the_current_desktop_process() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should resolve")
            .port();
        let state_root = temp_state_root("self-protection");
        let diagnosis = diagnose_port(port, &state_root).expect("port diagnosis should succeed");
        let current = diagnosis
            .occupants
            .iter()
            .find(|occupant| occupant.pid == process::id())
            .expect("current process should own the listener");
        let confirmation_error = terminate_port_occupants(
            port,
            &state_root,
            &[ExpectedPortOccupant {
                pid: current.pid,
                started_at: current.started_at,
            }],
            false,
        )
        .expect_err("a non-Magi listener must require explicit confirmation");
        assert!(confirmation_error.contains("用户确认"));

        let error = terminate_port_occupants(
            port,
            &state_root,
            &[ExpectedPortOccupant {
                pid: current.pid,
                started_at: current.started_at,
            }],
            true,
        )
        .expect_err("runtime recovery must not terminate its own desktop process");
        assert!(error.contains("当前 Magi 桌面进程"));
        assert!(listener.local_addr().is_ok());

        let _ = std::fs::remove_dir_all(state_root);
    }

    #[test]
    fn cleanup_rejects_a_stale_process_identity_snapshot() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should resolve")
            .port();
        let state_root = temp_state_root("stale-identity");
        let diagnosis = diagnose_port(port, &state_root).expect("port diagnosis should succeed");
        let current = diagnosis
            .occupants
            .iter()
            .find(|occupant| occupant.pid == process::id())
            .expect("current process should own the listener");

        let error = terminate_port_occupants(
            port,
            &state_root,
            &[ExpectedPortOccupant {
                pid: current.pid,
                started_at: current.started_at.saturating_add(1),
            }],
            true,
        )
        .expect_err("a changed process identity must stop cleanup");
        assert!(error.contains("占用进程已经变化"));
        assert!(listener.local_addr().is_ok());

        let _ = std::fs::remove_dir_all(state_root);
    }
}
