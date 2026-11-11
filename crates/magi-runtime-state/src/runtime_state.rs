use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessesToUpdate, System};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 38123;
/// 返回 Desktop 与独立 daemon 共用的唯一应用状态根。
///
/// `~/.magi` 是当前产品已经使用的全局状态事实源，开发 daemon、正式
/// Electron 必须解析到同一个目录；项目数据由该目录下的 projects 管理，
/// 源文件夹中的旧 `.magi` 仅作为单向迁移输入。
pub fn default_state_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".magi")
}

/// 判断 daemon 全局状态根与 workspace 自身状态根是否发生物理重叠。
///
/// 两类状态由不同所有者管理。只要一个根是另一个根的祖先，就会让状态扫描
/// 把 personal 与 workspace 数据混在一起，因此不能只判断路径是否完全相等。
pub fn state_roots_overlap(state_root: &Path, workspace_root: &Path) -> io::Result<bool> {
    let normalized_state_root = normalize_path(state_root)?;
    let normalized_workspace_state_root = normalize_path(&workspace_root.join(".magi"))?;
    Ok(normalized_state_root == normalized_workspace_state_root
        || normalized_state_root.starts_with(&normalized_workspace_state_root)
        || normalized_workspace_state_root.starts_with(&normalized_state_root))
}

fn normalize_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    match std::fs::canonicalize(&absolute) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(absolute),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub started_at: u64,
    pub updated_at: u64,
}

pub struct RuntimeStateManager {
    state_dir: PathBuf,
}

impl RuntimeStateManager {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
        }
    }

    pub fn runtime_file(&self) -> PathBuf {
        self.state_dir.join("runtime.json")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.state_dir.join("runtime.pid")
    }

    pub fn read_runtime_state(&self) -> Option<RuntimeState> {
        let path = self.runtime_file();
        let content = std::fs::read_to_string(&path).ok()?;
        let state: RuntimeState = serde_json::from_str(&content).ok()?;
        if state.pid == 0 || state.port == 0 || state.started_at == 0 {
            return None;
        }
        if !is_process_alive(state.pid) {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Some(state)
    }

    pub fn write_runtime_state(&self, pid: u32, host: Option<&str>, port: u16) -> RuntimeState {
        let _ = std::fs::create_dir_all(&self.state_dir);
        let host = host
            .filter(|h| !h.trim().is_empty())
            .unwrap_or(DEFAULT_HOST)
            .to_string();
        let port = if port == 0 { DEFAULT_PORT } else { port };
        let now = now_millis();
        let state = RuntimeState {
            pid,
            host: host.clone(),
            port,
            base_url: format!("http://{host}:{port}"),
            started_at: now,
            updated_at: now,
        };
        let _ = std::fs::write(
            self.runtime_file(),
            serde_json::to_string_pretty(&state).unwrap_or_default(),
        );
        state
    }

    pub fn remove_runtime_state(&self) {
        let _ = std::fs::remove_file(self.runtime_file());
    }

    pub fn write_pid(&self, pid: u32) {
        let _ = std::fs::create_dir_all(&self.state_dir);
        let _ = std::fs::write(self.pid_file(), pid.to_string());
    }

    pub fn remove_pid(&self) {
        let _ = std::fs::remove_file(self.pid_file());
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_state_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("magi-runtime-state-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn runtime_state_round_trips_current_process() {
        let dir = temp_state_dir("round-trip");
        let manager = RuntimeStateManager::new(&dir);
        let pid = std::process::id();

        let written = manager.write_runtime_state(pid, Some("127.0.0.1"), 38123);
        assert_eq!(written.pid, pid);
        assert_eq!(written.base_url, "http://127.0.0.1:38123");

        let read = manager
            .read_runtime_state()
            .expect("runtime state should read");
        assert_eq!(read.pid, pid);
        assert_eq!(read.port, 38123);

        manager.write_pid(pid);
        assert_eq!(
            std::fs::read_to_string(manager.pid_file()).unwrap(),
            pid.to_string()
        );
        manager.remove_runtime_state();
        manager.remove_pid();
        assert!(manager.read_runtime_state().is_none());
        assert!(!manager.pid_file().exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_state_root_is_the_shared_user_state_root() {
        let root = default_state_root();
        assert_eq!(
            root.file_name().and_then(|value| value.to_str()),
            Some(".magi")
        );
    }

    #[test]
    fn runtime_state_uses_defaults_for_blank_host_or_zero_port() {
        let dir = temp_state_dir("defaults");
        let manager = RuntimeStateManager::new(&dir);
        let state = manager.write_runtime_state(std::process::id(), Some("  "), 0);

        assert_eq!(state.host, DEFAULT_HOST);
        assert_eq!(state.port, DEFAULT_PORT);
        assert_eq!(state.base_url, "http://127.0.0.1:38123");
        let _ = std::fs::remove_dir_all(dir);
    }
}
