//! OpenAI Tunnel 通道托管（最终开发基线 §8.3）。
//!
//! 职责与硬约束：
//! - 按需启动 `openai/tunnel-client`（**固定版本 + SHA-256 校验**），由它以 **stdio**
//!   拉起 `magi-daemon-app mcp-relay --stdio --slot`（Magi MCP 服务的槽位端点中继）；随应用退出停止，不常驻；
//! - 凭据是 OpenAI 平台 API 密钥（仅 **Tunnels Read + Use**），**按文件引用**，
//!   绝不进命令行参数 / 日志 / 诊断输出 —— 本模块只传递文件路径，永不读取内容；
//! - 缺失、校验失败、凭据缺失一律 **fail closed**：返回带「具体缺哪一项」的状态，
//!   由上层报告「无项目工具」，绝不假装可用；
//! - 只做进程托管，不含协议与鉴权（那是 Magi MCP 服务与连接器的事）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use magi_process::{std_command, tokio_command};

/// `runtimes connect` is a control-plane operation.  The client is expected to
/// return after the managed runtime is created, but a broken installation must
/// not leave a daemon request waiting forever (the tunnel's own command
/// deadline is about two minutes).
pub const TUNNEL_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// Local inventory probes are deliberately much shorter than the connect
/// deadline.  A blocked CLI must not block the daemon status route or the
/// monitor indefinitely.
pub const TUNNEL_STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Stopping is best effort during shutdown/reconfiguration, but still needs a
/// finite bound so a wedged vendor CLI cannot serialize every later setup.
pub const TUNNEL_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// A healthy `connect` response may precede the first local inventory entry by
/// a short interval.  Keep that startup window separate from a runtime that
/// was already ready and then disappeared.
const TUNNEL_STARTUP_GRACE: std::time::Duration = TUNNEL_CONNECT_TIMEOUT;

/// The tunnel-client command contract is versioned independently from Magi.
/// Keep this value next to the argument/status contract so a future client
/// upgrade cannot silently change the JSON shape while the old parser still
/// reports a ready tunnel.
pub const TUNNEL_CLIENT_VERSION: &str = "0.0.15";

/// tunnel-client 的启动参数模板占位符。
pub const PLACEHOLDER_TUNNEL_ID: &str = "{tunnel_id}";
pub const PLACEHOLDER_CREDENTIAL_FILE: &str = "{credential_file}";
pub const PLACEHOLDER_MCP_BINARY: &str = "{mcp_binary}";
pub const PLACEHOLDER_LOCAL_ENDPOINT: &str = "{local_endpoint}";
pub const PLACEHOLDER_PROFILE_NAME: &str = "{profile_name}";
pub const PLACEHOLDER_PROFILE_DIR: &str = "{profile_dir}";
pub const PLACEHOLDER_ALIAS: &str = "{alias}";
pub const PLACEHOLDER_CLIENT_BINARY: &str = "{client_binary}";
pub const PLACEHOLDER_MCP_COMMAND: &str = "{mcp_command}";

/// 中继由 daemon 可执行文件的子命令提供（`magi-daemon-app mcp-relay ...`）。
pub const MCP_RELAY_SUBCOMMAND: &str = "mcp-relay";

/// 通道托管配置。
#[derive(Clone, Debug)]
pub struct TunnelRuntimeConfig {
    /// `openai/tunnel-client` 可执行文件。
    pub client_binary: PathBuf,
    /// 期望的 SHA-256（hex）。缺省时从 `<client>.sha256` 读取；两者都缺失即拒绝启动。
    pub expected_client_sha256: Option<String>,
    /// 用户私有的 API 密钥文件：**只引用，不读取**。
    pub credential_file: PathBuf,
    /// 用户在自己的 OpenAI 账号创建的 Tunnel id。
    pub tunnel_id: String,
    /// 提供 `mcp-relay` 子命令的可执行文件（即 daemon 自己；stdio 中继，连接 daemon 的槽位端点）。
    pub mcp_binary: PathBuf,
    /// daemon 侧 GPT Web 槽位端点（本地 socket / 命名管道）。
    pub local_endpoint: PathBuf,
    /// tunnel-client 本地 runtime profile 目录。
    pub profile_dir: PathBuf,
    /// tunnel-client 本地 runtime profile 名称。
    pub profile_name: String,
    /// tunnel-client 本地 runtime 别名。
    pub alias: String,
    /// 传给 `tunnel-client` 的参数模板。
    pub launch_args: Vec<String>,
}

impl TunnelRuntimeConfig {
    /// 按参考项目形态给出默认参数：通过 `runtimes connect` 注册一个由
    /// tunnel-client 托管的 runtime，并让它以 MCP stdio 拉起 `magi-daemon-app mcp-relay --slot`。
    pub fn default_launch_args() -> Vec<String> {
        vec![
            "runtimes".to_string(),
            "connect".to_string(),
            "--alias".to_string(),
            PLACEHOLDER_ALIAS.to_string(),
            "--profile".to_string(),
            PLACEHOLDER_PROFILE_NAME.to_string(),
            "--profile-dir".to_string(),
            PLACEHOLDER_PROFILE_DIR.to_string(),
            "--tunnel-client-bin".to_string(),
            PLACEHOLDER_CLIENT_BINARY.to_string(),
            "--tunnel-id".to_string(),
            PLACEHOLDER_TUNNEL_ID.to_string(),
            "--runtime-api-key".to_string(),
            format!("file:{PLACEHOLDER_CREDENTIAL_FILE}"),
            "--mcp-command".to_string(),
            PLACEHOLDER_MCP_COMMAND.to_string(),
            "--json".to_string(),
        ]
    }

    /// 展开参数模板。产出里只有**文件路径**，没有凭据内容。
    pub fn expanded_args(&self) -> Vec<String> {
        self.launch_args
            .iter()
            .map(|arg| {
                arg.replace(PLACEHOLDER_TUNNEL_ID, &self.tunnel_id)
                    .replace(
                        PLACEHOLDER_CREDENTIAL_FILE,
                        &self.credential_file.to_string_lossy(),
                    )
                    .replace(PLACEHOLDER_MCP_BINARY, &self.mcp_binary.to_string_lossy())
                    .replace(
                        PLACEHOLDER_LOCAL_ENDPOINT,
                        &self.local_endpoint.to_string_lossy(),
                    )
                    .replace(PLACEHOLDER_PROFILE_NAME, &self.profile_name)
                    .replace(PLACEHOLDER_PROFILE_DIR, &self.profile_dir.to_string_lossy())
                    .replace(PLACEHOLDER_ALIAS, &self.alias)
                    .replace(
                        PLACEHOLDER_CLIENT_BINARY,
                        &self.client_binary.to_string_lossy(),
                    )
                    .replace(PLACEHOLDER_MCP_COMMAND, &self.mcp_command())
            })
            .collect()
    }

    /// 参数中的 MCP 命令是一个由 tunnel-client 解析的单字符串，而不是
    /// `Command` 的 argv。tunnel-client 的命令解析器在各平台都使用反斜杠
    /// 转义，因此不能在 Unix 上使用仅 shell 认识的单引号；路径只做
    /// parser-safe quoting，不读取或内联密钥。
    pub fn mcp_command(&self) -> String {
        [
            self.mcp_binary.to_string_lossy().into_owned(),
            MCP_RELAY_SUBCOMMAND.to_string(),
            "--stdio".to_string(),
            "--slot".to_string(),
            "--endpoint".to_string(),
            self.local_endpoint.to_string_lossy().into_owned(),
        ]
        .into_iter()
        .map(|value| command_quote(&value))
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// `tunnel-client` parses the command string itself on every platform.  Use
/// the same backslash-escaped double-quoted form on Unix and Windows; shell
/// single quotes are not part of the tunnel-client command grammar.
fn command_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LaunchStatus {
    pid: Option<u32>,
    /// The local runtime may become `ready` shortly after `connect` returns.
    /// `healthy` is enough to prove that the managed process was created;
    /// readiness is observed through the local inventory below.
    ready: bool,
}

fn parse_launch_result(output: &str) -> Result<LaunchStatus, String> {
    let value: serde_json::Value = serde_json::from_str(output.trim())
        .map_err(|_| "tunnel_client_non_json_connect_output".to_string())?;
    let running = value
        .get("running")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| {
            value
                .get("process_running")
                .and_then(serde_json::Value::as_bool)
        })
        == Some(true);
    let healthy = value.get("healthy").and_then(serde_json::Value::as_bool) == Some(true);
    let ready = value.get("ready").and_then(serde_json::Value::as_bool) == Some(true);
    if running && healthy {
        return Ok(LaunchStatus {
            pid: value
                .get("pid")
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            ready,
        });
    }
    Err(format!(
        "tunnel_runtime_not_healthy: running={running}, healthy={healthy}, ready={ready}",
    ))
}

/// 从 `ps -axo pid=,command=` 的输出里挑出本通道泄漏的 `tunnel-client run` 进程。
///
/// 必须同时满足：命令里有本通道的客户端二进制路径、`run` 子命令、同一个 `--profile-dir`
/// 与 `--profile` 名称；排除自身。任何一项对不上都不动，避免误杀别的隧道客户端。
fn leaked_client_pids(
    listing: &str,
    client_binary: &Path,
    profile_dir: &Path,
    profile_name: &str,
    own_pid: u32,
) -> Vec<u32> {
    let binary = client_binary.to_string_lossy();
    let dir = profile_dir.to_string_lossy();
    listing
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let words: Vec<&str> = command.split_whitespace().collect();
            let executable_matches = words.first().is_some_and(|first| *first == binary);
            let runs = words.get(1) == Some(&"run");
            let flag_value = |flag: &str| {
                words
                    .iter()
                    .position(|word| *word == flag)
                    .and_then(|index| words.get(index + 1))
                    .copied()
            };
            (pid != own_pid
                && executable_matches
                && runs
                && flag_value("--profile-dir") == Some(dir.as_ref())
                && flag_value("--profile") == Some(profile_name))
            .then_some(pid)
        })
        .collect()
}

fn parse_runtime_status(output: &str, alias: &str) -> TunnelClientStatus {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output.trim()) else {
        return TunnelClientStatus::Exited;
    };

    // Current tunnel-client releases expose the local runtime inventory as
    // `{ "entries": [{ "alias": ..., "runtime_state": ... }] }` from
    // `runtimes cleanup --json`.  Only the exact alias owned by this manager
    // may make the channel ready; missing or ambiguous inventory is fail
    // closed.  Runtime state is intentionally treated as an enum so vendor
    // diagnostics cannot inject secrets into the channel status.
    if let Some(entries) = value.get("entries").and_then(serde_json::Value::as_array) {
        let matches = entries
            .iter()
            .filter(|entry| entry.get("alias").and_then(serde_json::Value::as_str) == Some(alias))
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return TunnelClientStatus::Exited;
        }
        // An absent alias is a stopped/unknown runtime, not a ready one.  The
        // caller may preserve a short-lived NotReady startup state, but an
        // already-running runtime disappearing from inventory must close the
        // channel and revoke pending calls.
        let Some(entry) = matches.first() else {
            return TunnelClientStatus::Exited;
        };
        let runtime_state = entry
            .get("runtime_state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let live_runtime = entry.get("live_runtime");
        let live_system_pid = live_runtime
            .and_then(|runtime| runtime.get("system"))
            .and_then(|system| system.get("pid"))
            .and_then(serde_json::Value::as_u64);
        let live_status_pid = live_runtime
            .and_then(|runtime| runtime.get("status"))
            .and_then(|status| status.get("pid"))
            .and_then(serde_json::Value::as_u64);
        let pid = entry
            .get("pid")
            .or_else(|| entry.get("process_id"))
            .and_then(serde_json::Value::as_u64)
            .or(live_system_pid)
            .or(live_status_pid)
            .and_then(|pid| u32::try_from(pid).ok())
            .unwrap_or(0);
        return match runtime_state {
            "ready" => TunnelClientStatus::Running { pid },
            "starting" | "healthy" => TunnelClientStatus::NotReady {
                reason: runtime_state.to_string(),
            },
            "stopped" => TunnelClientStatus::Exited,
            _ => TunnelClientStatus::NotReady {
                // Keep vendor diagnostics out of the daemon status surface.
                // A tunnel-client response is external input and must not be
                // allowed to smuggle credentials or arbitrary log text into
                // UI/diagnostic output.
                reason: "runtime_unknown".to_string(),
            },
        };
    }

    // Keep accepting the launch/status shape used by older development
    // fixtures.  It is still alias-scoped and requires all health bits, so it
    // cannot turn an arbitrary JSON response into a ready channel.
    if value.get("alias").and_then(serde_json::Value::as_str) != Some(alias) {
        return TunnelClientStatus::Exited;
    }
    let process_running = value
        .get("process_running")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| value.get("running").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let healthy = value
        .get("healthy")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let ready = value
        .get("ready")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let runtime_state = value
        .get("runtime_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if process_running && healthy && ready && runtime_state == "ready" {
        return TunnelClientStatus::Running {
            pid: value
                .get("pid")
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .unwrap_or(0),
        };
    }
    if process_running {
        return TunnelClientStatus::NotReady {
            reason: if runtime_state.is_empty() {
                "runtime_not_ready".to_string()
            } else if matches!(runtime_state, "starting" | "healthy") {
                runtime_state.to_string()
            } else {
                "runtime_unknown".to_string()
            },
        };
    }
    TunnelClientStatus::Exited
}

/// 通道托管状态。`code()` 直接进入「具体缺哪一项」的展示与 `web_tunnel_unavailable`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunnelClientStatus {
    Stopped,
    Running {
        pid: u32,
    },
    /// Tunnel id 不符合 OpenAI Tunnel 的固定形状。
    InvalidTunnelId,
    /// 找不到 tunnel-client。
    MissingClient {
        path: String,
    },
    /// tunnel-client 校验失败（版本 / 摘要不符）。
    ChecksumMismatch,
    /// 没有提供固定版本对应的 SHA-256 校验值。
    ChecksumMissing,
    /// 客户端进程已经退出（不能继续把通道报告为 ready）。
    Exited,
    /// 客户端进程仍在，但 runtime 尚未达到可服务状态。
    NotReady {
        reason: String,
    },
    /// API 密钥文件缺失。
    MissingCredential {
        path: String,
    },
    Failed {
        reason: String,
    },
}

impl TunnelClientStatus {
    pub const fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// 降级展示用的稳定原因码（不包含任何凭据信息）。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Running { .. } => "running",
            Self::InvalidTunnelId => "tunnel_id_invalid",
            Self::MissingClient { .. } => "client_missing",
            Self::ChecksumMismatch => "client_checksum_mismatch",
            Self::ChecksumMissing => "client_checksum_missing",
            Self::Exited => "process_exited",
            Self::NotReady { .. } => "runtime_not_ready",
            Self::MissingCredential { .. } => "credential_missing",
            Self::Failed { .. } => "start_failed",
        }
    }

    /// 用户可读的「具体缺哪一项」。
    pub fn detail(&self) -> String {
        match self {
            Self::Stopped => "OpenAI Tunnel 未启动".to_string(),
            Self::Running { pid } => format!("OpenAI Tunnel 运行中（pid {pid}）"),
            Self::InvalidTunnelId => {
                "Tunnel id 无效：必须是 tunnel_ 加 32 位小写十六进制字符".to_string()
            }
            Self::MissingClient { path } => format!("缺少 tunnel-client：{path}"),
            Self::ChecksumMismatch => "tunnel-client 校验失败（版本或 SHA-256 不符）".to_string(),
            Self::ChecksumMissing => "缺少 tunnel-client 的固定 SHA-256 校验值".to_string(),
            Self::Exited => "tunnel-client 已退出".to_string(),
            Self::NotReady { reason } => format!("tunnel-client runtime 尚未就绪：{reason}"),
            Self::MissingCredential { path } => {
                format!("缺少 Tunnels Read + Use 的 API 密钥文件：{path}")
            }
            Self::Failed { reason } => format!("tunnel-client 启动失败：{reason}"),
        }
    }
}

struct TunnelState {
    status: TunnelClientStatus,
    /// `runtimes connect` may have created a managed runtime even when a later
    /// health probe reports it as not ready.  Keep enough lifecycle state to
    /// issue the matching stop command during shutdown/reconfigure.
    managed: bool,
    /// Only a `NotReady` runtime gets this grace period.  Once a runtime has
    /// been observed ready, an absent inventory entry is an actual exit and
    /// must revoke T3 immediately.
    startup_deadline: Option<std::time::Instant>,
}

/// tunnel-client 的进程托管。只做「按需启动 / 停止 / 报告状态」。
pub struct TunnelManager {
    config: TunnelRuntimeConfig,
    state: Mutex<TunnelState>,
    lifecycle: tokio::sync::Mutex<()>,
}

impl TunnelManager {
    pub fn new(config: TunnelRuntimeConfig) -> Self {
        Self {
            config,
            state: Mutex::new(TunnelState {
                status: TunnelClientStatus::Stopped,
                managed: false,
                startup_deadline: None,
            }),
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    pub fn status(&self) -> TunnelClientStatus {
        let (should_probe, startup_grace_active) = {
            let state = self.state.lock().expect("tunnel state lock");
            let active = matches!(
                state.status,
                TunnelClientStatus::Running { .. } | TunnelClientStatus::NotReady { .. }
            );
            let startup_grace_active = state
                .startup_deadline
                .is_some_and(|deadline| std::time::Instant::now() < deadline);
            (
                state.managed && (active || startup_grace_active),
                startup_grace_active,
            )
        };
        if !should_probe {
            return self.state.lock().expect("tunnel state lock").status.clone();
        }
        // `runtimes connect` is a control-plane command: it starts a managed
        // runtime and exits, so retaining its Child would incorrectly turn a
        // healthy tunnel into `Exited`.  The tunnel-client's local inventory
        // probe is `runtimes cleanup --json` without `--apply`; unlike the
        // optional `status` control-plane lookup it does not depend on a
        // remote API response and is the readiness source used by the
        // reference client.
        let output = self.run_status_probe();
        let probed = match output {
            Ok(output) if output.status.success() => {
                parse_runtime_status(&String::from_utf8_lossy(&output.stdout), &self.config.alias)
            }
            Ok(_) | Err(_) => {
                let previous = self.state.lock().expect("tunnel state lock").status.clone();
                if matches!(previous, TunnelClientStatus::NotReady { .. }) {
                    TunnelClientStatus::NotReady {
                        reason: "runtime_probe_unavailable".to_string(),
                    }
                } else {
                    TunnelClientStatus::Exited
                }
            }
        };
        // `connect` can report a healthy managed process before its local
        // inventory is populated.  Do not turn that one startup race into a
        // terminal exit; after the grace period, or after a previously ready
        // runtime disappears, keep the fail-closed `Exited` result.
        let next = if startup_grace_active && matches!(probed, TunnelClientStatus::Exited) {
            TunnelClientStatus::NotReady {
                reason: "starting".to_string(),
            }
        } else {
            probed
        };
        let mut state = self.state.lock().expect("tunnel state lock");
        if matches!(
            state.status,
            TunnelClientStatus::Running { .. } | TunnelClientStatus::NotReady { .. }
        ) {
            state.status = next;
            if state.status.is_running() || !startup_grace_active {
                state.startup_deadline = None;
            }
        }
        state.status.clone()
    }

    /// Run the local inventory probe with a hard deadline.  `status()` is a
    /// synchronous API because it is also used by the daemon's status
    /// projection, so relying on an async timeout here would still allow a
    /// wedged vendor CLI to block that route forever.
    fn run_status_probe(&self) -> std::io::Result<std::process::Output> {
        let mut command = std_command(&self.config.client_binary);
        command
            .args(["runtimes", "cleanup"])
            // tunnel-client 0.0.15 的 `cleanup` / `stop` 不再接受 profile 选择器，清单是整个
            // 本机 state root 的。身份边界因此只剩 Magi 独占的 runtime 别名：解析时只认
            // 与别名**完全相等**的条目，缺失或重名一律按未就绪处理（见 `parse_runtime_status`）。
            .arg("--json")
            .env_remove("OPENAI_API_KEY")
            .env_remove("CONTROL_PLANE_API_KEY")
            .env_remove("OPENAI_ADMIN_KEY")
            .env_remove("CONTROL_PLANE_ADMIN_KEY")
            .env_remove("MAGI_OPENAI_COMPAT_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let deadline = std::time::Instant::now() + TUNNEL_STATUS_TIMEOUT;
        loop {
            if child.try_wait()?.is_some() {
                return child.wait_with_output();
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "tunnel runtime status probe timed out",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// 按需启动。任何前置件不满足都 fail closed，并保留「具体缺哪一项」。
    pub async fn start(&self) -> TunnelClientStatus {
        let _lifecycle = self.lifecycle.lock().await;
        {
            let state = self.state.lock().expect("tunnel state lock");
            if matches!(
                state.status,
                TunnelClientStatus::Running { .. } | TunnelClientStatus::NotReady { .. }
            ) {
                drop(state);
                return self.status();
            }
        }
        if let Some(status) = self.preflight() {
            let mut state = self.state.lock().expect("tunnel state lock");
            state.status = status.clone();
            return status;
        }
        if let Err(error) = std::fs::create_dir_all(&self.config.profile_dir) {
            let status = TunnelClientStatus::Failed {
                reason: error.kind().to_string(),
            };
            self.state.lock().expect("tunnel state lock").status = status.clone();
            return status;
        }
        // The profile is tunnel-client-owned state and may contain runtime
        // metadata.  Keep it private before starting the managed process; do
        // not rely on the process umask to enforce the boundary.
        #[cfg(unix)]
        if let Err(error) = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &self.config.profile_dir,
                std::fs::Permissions::from_mode(0o700),
            )
        } {
            let status = TunnelClientStatus::Failed {
                reason: error.kind().to_string(),
            };
            self.state.lock().expect("tunnel state lock").status = status.clone();
            return status;
        }
        // 上一次 daemon 异常退出（崩溃、被强杀）时没来得及 `runtimes stop`，托管运行时会作为孤儿
        // 继续活着：它仍在向控制面轮询、抢收 ChatGPT 的工具调用，却把调用转给已经没有监听者的旧
        // 槽位 socket，而 `runtimes connect` 看不到它（清单里 live_runtime.found=false），
        // 于是新起的客户端和它争抢同一个隧道，工具调用就会随机失败。启动前先回收。
        self.reap_leaked_clients().await;
        let args = self.config.expanded_args();
        let mut command = tokio_command(&self.config.client_binary);
        command.args(&args);
        command.current_dir(&self.config.profile_dir);
        command.kill_on_drop(true);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        // The runtime key is passed only as a `file:` reference.  Do not let
        // common ambient API-key variables become an alternate credential
        // source for tunnel-client or leak through its child environment.
        for key in [
            "OPENAI_API_KEY",
            "CONTROL_PLANE_API_KEY",
            "OPENAI_ADMIN_KEY",
            "CONTROL_PLANE_ADMIN_KEY",
            "MAGI_OPENAI_COMPAT_API_KEY",
        ] {
            command.env_remove(key);
        }
        let output = match tokio::time::timeout(TUNNEL_CONNECT_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let status = TunnelClientStatus::Failed {
                    reason: error.kind().to_string(),
                };
                self.state.lock().expect("tunnel state lock").status = status.clone();
                return status;
            }
            Err(_) => {
                let mut state = self.state.lock().expect("tunnel state lock");
                state.managed = true;
                state.startup_deadline = None;
                let status = TunnelClientStatus::Failed {
                    reason: "connect_timeout".to_string(),
                };
                state.status = status.clone();
                return status;
            }
        };
        {
            let mut state = self.state.lock().expect("tunnel state lock");
            // `connect` may have created a managed runtime even when its
            // output reports an unhealthy launch, so retain ownership for the
            // matching stop operation.
            state.managed = true;
        }
        let launch = parse_launch_result(&String::from_utf8_lossy(&output.stdout));
        if !output.status.success() || launch.is_err() {
            let status = TunnelClientStatus::Failed {
                reason: launch
                    .err()
                    .unwrap_or_else(|| "runtimes_connect_failed".to_string()),
            };
            self.state.lock().expect("tunnel state lock").status = status.clone();
            return status;
        }
        let launch = launch.expect("checked above");
        let status = if launch.ready {
            TunnelClientStatus::Running {
                pid: launch.pid.unwrap_or(0),
            }
        } else {
            TunnelClientStatus::NotReady {
                reason: "starting".to_string(),
            }
        };
        let mut state = self.state.lock().expect("tunnel state lock");
        state.status = status.clone();
        state.startup_deadline =
            (!launch.ready).then(|| std::time::Instant::now() + TUNNEL_STARTUP_GRACE);
        status
    }

    /// 回收上一次运行泄漏下来的、属于本通道（同一客户端二进制、同一 profile 目录与名称）的
    /// `tunnel-client run` 进程。只匹配这一个精确身份，不碰别的 tunnel-client。
    async fn reap_leaked_clients(&self) {
        #[cfg(unix)]
        {
            let config = self.config.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let Ok(listing) = std_command("ps")
                    .args(["-axo", "pid=,command="])
                    .stdin(Stdio::null())
                    .output()
                else {
                    return;
                };
                let own_pid = std::process::id();
                let pids = leaked_client_pids(
                    &String::from_utf8_lossy(&listing.stdout),
                    &config.client_binary,
                    &config.profile_dir,
                    &config.profile_name,
                    own_pid,
                );
                for pid in &pids {
                    tracing::warn!(pid, "回收上次运行泄漏的 tunnel-client 进程");
                    let _ = std_command("kill")
                        .args(["-TERM", &pid.to_string()])
                        .status();
                }
                // 给它一个体面退出的窗口；仍然活着再强制结束。
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                for pid in pids {
                    while std::time::Instant::now() < deadline
                        && std_command("kill")
                            .args(["-0", &pid.to_string()])
                            .stderr(Stdio::null())
                            .status()
                            .is_ok_and(|status| status.success())
                    {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    let _ = std_command("kill")
                        .args(["-KILL", &pid.to_string()])
                        .stderr(Stdio::null())
                        .status();
                }
            })
            .await;
        }
    }

    /// 前置件校验的只读投影，供 daemon 在打开本地 stdio 入口前 fail closed。
    pub fn preflight_status(&self) -> Option<TunnelClientStatus> {
        let status = self.preflight();
        // `configure` calls this before starting the runtime so it can avoid
        // opening the local socket for an invalid setup.  Preserve the exact
        // failure in the manager too; otherwise a subsequent status read
        // would turn a useful `client_missing`/`credential_missing` result
        // back into the generic `stopped` state.
        if let Some(status) = status.as_ref() {
            let mut state = self.state.lock().expect("tunnel state lock");
            state.status = status.clone();
            state.managed = false;
            state.startup_deadline = None;
        }
        status
    }

    /// 前置件校验：返回 `Some(status)` 表示不可启动。
    fn preflight(&self) -> Option<TunnelClientStatus> {
        if !valid_tunnel_id(&self.config.tunnel_id) {
            return Some(TunnelClientStatus::InvalidTunnelId);
        }
        if !self.config.client_binary.is_file() {
            return Some(TunnelClientStatus::MissingClient {
                path: self.config.client_binary.to_string_lossy().to_string(),
            });
        }
        if let Some(expected) = self.config.expected_client_sha256.as_deref()
            && !expected.trim().is_empty()
        {
            if !verify_sha256(&self.config.client_binary, expected) {
                return Some(TunnelClientStatus::ChecksumMismatch);
            }
        } else {
            let sidecar = PathBuf::from(format!(
                "{}.sha256",
                self.config.client_binary.to_string_lossy()
            ));
            let Ok(expected) = std::fs::read_to_string(sidecar) else {
                return Some(TunnelClientStatus::ChecksumMissing);
            };
            let expected = expected.split_whitespace().next().unwrap_or_default();
            if expected.is_empty() || !verify_sha256(&self.config.client_binary, expected) {
                return Some(TunnelClientStatus::ChecksumMismatch);
            }
        }
        if !self.config.credential_file.is_file() {
            return Some(TunnelClientStatus::MissingCredential {
                path: self.config.credential_file.to_string_lossy().to_string(),
            });
        }
        None
    }

    /// 停止通道（应用退出、用户清除配置）。
    pub async fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let should_stop = {
            let state = self.state.lock().expect("tunnel state lock");
            state.managed
        };
        if should_stop {
            let mut command = tokio_command(&self.config.client_binary);
            command.args(["runtimes", "stop", &self.config.alias, "--json"]);
            command.stdin(Stdio::null());
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
            command.kill_on_drop(true);
            for key in [
                "OPENAI_API_KEY",
                "CONTROL_PLANE_API_KEY",
                "OPENAI_ADMIN_KEY",
                "CONTROL_PLANE_ADMIN_KEY",
                "MAGI_OPENAI_COMPAT_API_KEY",
            ] {
                command.env_remove(key);
            }
            let _ = tokio::time::timeout(TUNNEL_STOP_TIMEOUT, command.output()).await;
        }
        let mut state = self.state.lock().expect("tunnel state lock");
        state.status = TunnelClientStatus::Stopped;
        state.managed = false;
        state.startup_deadline = None;
    }
}

fn valid_tunnel_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("tunnel_") else {
        return false;
    };
    suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 校验文件 SHA-256（hex，大小写不敏感）。
pub fn verify_sha256(path: &Path, expected_hex: &str) -> bool {
    use sha2::{Digest, Sha256};
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual.eq_ignore_ascii_case(expected_hex.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_leaked_client_of_this_channel_is_matched() {
        let binary = Path::new("/Users/x/.magi/web-model/tunnel-client/v0.0.15/tunnel-client");
        let dir = Path::new("/Users/x/.magi/web-model/tunnel-profile");
        let listing = "\
49775 /Users/x/.magi/web-model/tunnel-client/v0.0.15/tunnel-client run --profile-dir /Users/x/.magi/web-model/tunnel-profile --profile magi-web-model --log.file /tmp/a.log
49776 /Users/x/code/Magi.app/daemon/magi-daemon-app mcp-relay --stdio --slot --endpoint /tmp/m.sock
50001 /Users/x/.magi/web-model/tunnel-client/v0.0.15/tunnel-client run --profile-dir /Users/x/.magi/web-model/other-profile --profile magi-web-model
50002 /Users/x/.magi/web-model/tunnel-client/v0.0.15/tunnel-client run --profile-dir /Users/x/.magi/web-model/tunnel-profile --profile someone-else
50003 /opt/other/tunnel-client run --profile-dir /Users/x/.magi/web-model/tunnel-profile --profile magi-web-model
50004 /Users/x/.magi/web-model/tunnel-client/v0.0.15/tunnel-client runtimes status magi-web-model --profile-dir /Users/x/.magi/web-model/tunnel-profile --profile magi-web-model
60000 /Users/x/.magi/web-model/tunnel-client/v0.0.15/tunnel-client run --profile-dir /Users/x/.magi/web-model/tunnel-profile --profile magi-web-model
";
        assert_eq!(
            leaked_client_pids(listing, binary, dir, "magi-web-model", 60000),
            vec![49775],
            "只回收同一二进制 + 同一 profile 目录/名称的 run 进程；子进程、别的隧道、别的子命令与自身都不动"
        );
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "magi-web-model-tunnel-{tag}-{}",
            sha256_hex(&format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        dir
    }

    fn config(dir: &Path) -> TunnelRuntimeConfig {
        TunnelRuntimeConfig {
            client_binary: dir.join("tunnel-client"),
            expected_client_sha256: None,
            credential_file: dir.join("api-key"),
            tunnel_id: "tunnel_0123456789abcdef0123456789abcdef".into(),
            mcp_binary: dir.join("magi-mcp"),
            local_endpoint: dir.join("slot.sock"),
            profile_dir: dir.join("profile"),
            profile_name: "magi-web-model".into(),
            alias: "magi-web-model".into(),
            launch_args: TunnelRuntimeConfig::default_launch_args(),
        }
    }

    fn sha256_of(path: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(std::fs::read(path).expect("read client"));
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn expanded_args_only_carry_file_paths_never_credentials() {
        let dir = temp_dir("args");
        let config = config(&dir);
        let args = config.expanded_args();
        assert!(args.contains(&"tunnel_0123456789abcdef0123456789abcdef".to_string()));
        assert!(
            args.iter()
                .any(|arg| { arg == &format!("file:{}", dir.join("api-key").to_string_lossy()) })
        );
        assert!(args.iter().any(|a| a.contains("magi-mcp")));
        assert!(args.iter().any(|a| a.contains("runtimes")));
        assert!(args.iter().any(|a| a.contains("slot.sock")));
        // 参数里只出现凭据文件路径，不出现任何秘密内容。
        assert!(!args.iter().any(|a| a.contains("sk-")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_command_uses_tunnel_client_quoting_for_paths_with_spaces() {
        let mut config = config(Path::new("/tmp/magi tunnel"));
        config.mcp_binary = PathBuf::from("/tmp/magi harness/bin");
        config.local_endpoint = PathBuf::from("/tmp/magi socket.sock");
        let command = config.mcp_command();
        assert_eq!(
            command,
            "\"/tmp/magi harness/bin\" \"mcp-relay\" \"--stdio\" \"--slot\" \"--endpoint\" \"/tmp/magi socket.sock\""
        );
        assert!(
            !command.contains("'"),
            "shell-only quoting is not tunnel-client syntax"
        );
    }

    #[tokio::test]
    async fn a_missing_client_fails_closed_with_the_specific_item() {
        let dir = temp_dir("missing");
        let manager = TunnelManager::new(config(&dir));
        let status = manager.start().await;
        assert!(matches!(status, TunnelClientStatus::MissingClient { .. }));
        assert_eq!(status.code(), "client_missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_checksum_mismatch_fails_closed_before_credentials_are_considered() {
        let dir = temp_dir("checksum");
        let mut config = config(&dir);
        std::fs::write(&config.client_binary, b"not the real client").expect("write");
        std::fs::write(&config.credential_file, b"secret").expect("write");
        config.expected_client_sha256 = Some("00".repeat(32));
        let manager = TunnelManager::new(config);
        assert_eq!(manager.start().await, TunnelClientStatus::ChecksumMismatch);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_credential_file_is_reported_as_such() {
        let dir = temp_dir("cred");
        let mut config = config(&dir);
        std::fs::write(&config.client_binary, b"client").expect("write");
        // 校验失败优先于凭据缺失：先确认摘要分支，再确认凭据分支。
        config.expected_client_sha256 = Some("00".repeat(32));
        let manager = TunnelManager::new(config.clone());
        assert_eq!(manager.start().await, TunnelClientStatus::ChecksumMismatch);
        config.expected_client_sha256 = Some(sha256_of(&config.client_binary));
        let manager = TunnelManager::new(config);
        let status = manager.start().await;
        assert!(matches!(
            status,
            TunnelClientStatus::MissingCredential { .. }
        ));
        assert_eq!(status.code(), "credential_missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_checksum_is_fail_closed_even_when_the_client_exists() {
        let dir = temp_dir("checksum-missing");
        let mut config = config(&dir);
        std::fs::write(&config.client_binary, b"client").expect("write");
        config.expected_client_sha256 = None;
        let manager = TunnelManager::new(config);
        assert_eq!(manager.start().await, TunnelClientStatus::ChecksumMissing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_sidecar_checksum_is_accepted_without_reading_the_credential_file() {
        let dir = temp_dir("checksum-sidecar");
        let config = config(&dir);
        std::fs::write(&config.client_binary, b"client").expect("write");
        std::fs::write(
            format!("{}.sha256", config.client_binary.display()),
            format!("{}  tunnel-client\n", sha256_of(&config.client_binary)),
        )
        .expect("write checksum");
        std::fs::write(&config.credential_file, b"not-read-by-magi").expect("write");
        let manager = TunnelManager::new(config);
        // The checksum passes; the fake file is not executable, so spawning it
        // fails without ever loading or logging the credential contents.
        assert!(matches!(
            manager.start().await,
            TunnelClientStatus::Failed { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_sha256_matches_known_digest() {
        let dir = temp_dir("sha");
        let file = dir.join("blob");
        std::fs::write(&file, b"abc").expect("write");
        // sha256("abc")
        assert!(verify_sha256(
            &file,
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        ));
        assert!(!verify_sha256(&file, "00"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_status_requires_process_health_and_ready_state() {
        let ready = serde_json::json!({
            "alias": "magi-web-model",
            "process_running": true,
            "healthy": true,
            "ready": true,
            "runtime_state": "ready",
            "pid": 42,
        });
        assert_eq!(
            parse_runtime_status(&ready.to_string(), "magi-web-model"),
            TunnelClientStatus::Running { pid: 42 }
        );

        let not_ready = serde_json::json!({
            "alias": "magi-web-model",
            "process_running": true,
            "healthy": true,
            "ready": false,
            "runtime_state": "starting",
        });
        assert_eq!(
            parse_runtime_status(&not_ready.to_string(), "magi-web-model"),
            TunnelClientStatus::NotReady {
                reason: "starting".to_string()
            }
        );

        let inventory_ready = serde_json::json!({
            "entries": [{
                "alias": "magi-web-model",
                "runtime_state": "ready",
                "live_runtime": {"system": {"pid": 77}},
            }],
        });
        assert_eq!(
            parse_runtime_status(&inventory_ready.to_string(), "magi-web-model"),
            TunnelClientStatus::Running { pid: 77 }
        );
        let inventory_starting = serde_json::json!({
            "entries": [{
                "alias": "magi-web-model",
                "runtime_state": "starting",
            }],
        });
        assert_eq!(
            parse_runtime_status(&inventory_starting.to_string(), "magi-web-model"),
            TunnelClientStatus::NotReady {
                reason: "starting".to_string()
            }
        );
        let duplicate = serde_json::json!({
            "entries": [
                {"alias": "magi-web-model", "runtime_state": "ready"},
                {"alias": "magi-web-model", "runtime_state": "ready"},
            ],
        });
        assert_eq!(
            parse_runtime_status(&duplicate.to_string(), "magi-web-model"),
            TunnelClientStatus::Exited
        );
    }

    #[test]
    fn runtime_status_does_not_echo_unknown_vendor_diagnostics() {
        let value = serde_json::json!({
            "entries": [{
                "alias": "magi-web-model",
                "runtime_state": "credential=sk-secret"
            }]
        });
        assert_eq!(
            parse_runtime_status(&value.to_string(), "magi-web-model"),
            TunnelClientStatus::NotReady {
                reason: "runtime_unknown".to_string()
            }
        );
    }

    #[test]
    fn tunnel_id_validation_matches_openai_runtime_shape() {
        assert!(valid_tunnel_id("tunnel_0123456789abcdef0123456789abcdef"));
        assert!(!valid_tunnel_id("tun-1"));
        assert!(!valid_tunnel_id("tunnel_0123456789ABCDEF0123456789abcdef"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_started_process_is_reported_running_and_stops_cleanly() {
        let dir = temp_dir("run");
        let mut config = config(&dir);
        config.client_binary = PathBuf::from("/bin/sh");
        config.launch_args = vec![
            "-c".into(),
            "printf '{\"running\":true,\"healthy\":true,\"ready\":true}'".into(),
        ];
        config.expected_client_sha256 = Some(sha256_of(&config.client_binary));
        std::fs::write(&config.credential_file, b"secret").expect("write");
        let manager = TunnelManager::new(config);
        let status = manager.start().await;
        assert!(status.is_running(), "{status:?}");
        manager.stop().await;
        assert_eq!(manager.status(), TunnelClientStatus::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_exited_tunnel_is_not_reported_as_ready() {
        let dir = temp_dir("exited");
        let mut config = config(&dir);
        config.client_binary = PathBuf::from("/bin/sh");
        config.expected_client_sha256 = Some(sha256_of(&config.client_binary));
        config.launch_args = vec![
            "-c".into(),
            "printf '{\"running\":true,\"healthy\":true,\"ready\":true}'".into(),
        ];
        std::fs::write(&config.credential_file, b"secret").expect("write");
        let manager = TunnelManager::new(config);
        assert!(manager.start().await.is_running());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(manager.status(), TunnelClientStatus::Exited);
        assert!(!manager.status().is_running());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn status_probes_the_local_inventory_not_the_remote_control_plane() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("status-command");
        let mut config = config(&dir);
        let script = r#"#!/bin/sh
        if [ "$1" = "runtimes" ] && [ "$2" = "cleanup" ] && [ "$3" = "--json" ]; then
  printf '{"entries":[{"alias":"magi-web-model","runtime_state":"ready","live_runtime":{"system":{"pid":99}}}]}'
  exit 0
fi
if [ "$1" = "runtimes" ] && [ "$2" = "connect" ]; then
  printf '{"running":true,"healthy":true,"ready":true,"pid":98}'
  exit 0
fi
if [ "$1" = "runtimes" ] && [ "$2" = "stop" ] && [ "$3" = "magi-web-model" ] && [ "$4" = "--json" ]; then
  exit 0
fi
exit 17
"#;
        std::fs::write(&config.client_binary, script).expect("write fake tunnel client");
        let mut permissions = std::fs::metadata(&config.client_binary)
            .expect("fake tunnel client metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&config.client_binary, permissions)
            .expect("make fake tunnel client executable");
        config.expected_client_sha256 = Some(sha256_of(&config.client_binary));
        std::fs::write(&config.credential_file, b"not-read-by-magi").expect("credential");

        let manager = TunnelManager::new(config);
        assert_eq!(
            manager.start().await,
            TunnelClientStatus::Running { pid: 98 }
        );
        assert_eq!(
            manager.status(),
            TunnelClientStatus::Running { pid: 99 },
            "readiness must come from the local cleanup inventory"
        );
        manager.stop().await;
        assert_eq!(manager.status(), TunnelClientStatus::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
