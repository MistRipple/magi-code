use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 38123;
const CLIENT_LEASE_STALE_MS: u64 = 60_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeState {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub started_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentClientLease {
    pub client_id: String,
    pub pid: u32,
    pub workspace_roots: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AgentClientLease {
    pub fn is_stale(&self, now: u64) -> bool {
        now.saturating_sub(self.updated_at) > CLIENT_LEASE_STALE_MS
    }
}

pub struct RuntimeStateManager {
    state_dir: PathBuf,
    clients_dir: PathBuf,
}

impl RuntimeStateManager {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        let clients_dir = state_dir.join("clients");
        Self {
            state_dir,
            clients_dir,
        }
    }

    pub fn runtime_file(&self) -> PathBuf {
        self.state_dir.join("runtime.json")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.state_dir.join("agent.pid")
    }

    fn client_lease_file(&self, client_id: &str) -> Option<PathBuf> {
        let normalized = normalize_client_id(client_id)?;
        Some(self.clients_dir.join(format!("{normalized}.json")))
    }

    pub fn read_runtime_state(&self) -> Option<AgentRuntimeState> {
        let path = self.runtime_file();
        let content = std::fs::read_to_string(&path).ok()?;
        let state: AgentRuntimeState = serde_json::from_str(&content).ok()?;
        if state.pid == 0 || state.port == 0 || state.started_at == 0 {
            return None;
        }
        if !is_process_alive(state.pid) {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Some(state)
    }

    pub fn write_runtime_state(&self, pid: u32, host: Option<&str>, port: u16) -> AgentRuntimeState {
        let _ = std::fs::create_dir_all(&self.state_dir);
        let host = host
            .filter(|h| !h.trim().is_empty())
            .unwrap_or(DEFAULT_HOST)
            .to_string();
        let port = if port == 0 { DEFAULT_PORT } else { port };
        let now = now_millis();
        let state = AgentRuntimeState {
            pid,
            host: host.clone(),
            port,
            base_url: format!("http://{host}:{port}"),
            started_at: now,
            updated_at: now,
        };
        let _ = std::fs::write(self.runtime_file(), serde_json::to_string_pretty(&state).unwrap_or_default());
        state
    }

    pub fn remove_runtime_state(&self) {
        let _ = std::fs::remove_file(self.runtime_file());
    }

    pub fn read_pid(&self) -> Option<u32> {
        let content = std::fs::read_to_string(self.pid_file()).ok()?;
        let pid: u32 = content.trim().parse().ok()?;
        if pid == 0 {
            return None;
        }
        if !is_process_alive(pid) {
            let _ = std::fs::remove_file(self.pid_file());
            return None;
        }
        Some(pid)
    }

    pub fn write_pid(&self, pid: u32) {
        let _ = std::fs::create_dir_all(&self.state_dir);
        let _ = std::fs::write(self.pid_file(), pid.to_string());
    }

    pub fn remove_pid(&self) {
        let _ = std::fs::remove_file(self.pid_file());
    }

    pub fn write_client_lease(&self, client_id: &str, pid: u32, workspace_roots: Vec<String>) -> Option<AgentClientLease> {
        let normalized = normalize_client_id(client_id)?;
        let _ = std::fs::create_dir_all(&self.clients_dir);
        let now = now_millis();
        let existing = self.read_client_lease(&normalized);
        let created_at = existing.as_ref().map(|l| l.created_at).unwrap_or(now);
        let lease = AgentClientLease {
            client_id: normalized.clone(),
            pid,
            workspace_roots,
            created_at,
            updated_at: now,
        };
        let path = self.client_lease_file(&normalized)?;
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&lease).unwrap_or_default());
        Some(lease)
    }

    pub fn read_client_lease(&self, client_id: &str) -> Option<AgentClientLease> {
        let path = self.client_lease_file(client_id)?;
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn remove_client_lease(&self, client_id: &str) {
        if let Some(path) = self.client_lease_file(client_id) {
            let _ = std::fs::remove_file(path);
        }
    }

    pub fn list_client_leases(&self) -> Vec<AgentClientLease> {
        let entries = match std::fs::read_dir(&self.clients_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        let now = now_millis();
        let mut leases = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let Ok(lease) = serde_json::from_str::<AgentClientLease>(&content) else {
                let _ = std::fs::remove_file(&path);
                continue;
            };
            if lease.is_stale(now) || !is_process_alive(lease.pid) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            leases.push(lease);
        }
        leases.sort_by_key(|l| l.created_at);
        leases
    }

    pub fn resolve_base_url(&self) -> String {
        self.read_runtime_state()
            .map(|s| s.base_url)
            .unwrap_or_else(default_base_url)
    }

    pub fn resolve_port(&self) -> u16 {
        self.read_runtime_state()
            .map(|s| s.port)
            .unwrap_or(DEFAULT_PORT)
    }
}

fn normalize_client_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized: String = trimmed
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
        .collect();
    Some(normalized)
}

fn default_base_url() -> String {
    format!("http://{DEFAULT_HOST}:{DEFAULT_PORT}")
}

pub fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::process::Command;
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
