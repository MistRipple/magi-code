#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{env, path::PathBuf, process};

use magi_daemon::{Daemon, DaemonConfig};
use magi_runtime_state::{RuntimeStateManager, default_state_root};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 38123;
const DEFAULT_SERVICE_NAME: &str = "magi-rust-backend";

fn read_env(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_port() -> Result<u16, Box<dyn std::error::Error>> {
    let Some(raw_port) = read_env("MAGI_PORT") else {
        return Ok(DEFAULT_PORT);
    };
    raw_port
        .parse::<u16>()
        .map_err(|error| format!("invalid MAGI_PORT `{raw_port}`: {error}").into())
}

fn read_env_flag(name: &str) -> Option<bool> {
    let raw = read_env(name)?;
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn should_open_browser() -> bool {
    read_env_flag("MAGI_OPEN_BROWSER").unwrap_or_else(is_product_entry_executable)
}

fn is_product_entry_executable() -> bool {
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .is_some_and(|stem| stem.eq_ignore_ascii_case("magi"))
}

/// `magi-daemon-app mcp-relay ...`：GPT Web 的 OpenAI Tunnel 以 stdio 拉起的 MCP 中继。
/// 复用 daemon 自己的可执行文件，避免再单独分发一个 `magi-mcp`。必须在 tokio 运行时之前分流，
/// 中继自己持有运行时。
const MCP_RELAY_SUBCOMMAND: &str = "mcp-relay";

fn main() -> Result<process::ExitCode, Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some(MCP_RELAY_SUBCOMMAND) {
        return Ok(magi_mcp_server::relay_cli::run_relay_cli(&args[1..]));
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_daemon())?;
    Ok(process::ExitCode::SUCCESS)
}

async fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .compact()
        .init();
    magi_process::initialize_user_process_environment();

    let host = read_env("MAGI_HOST").unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = read_port()?;
    let service_name =
        read_env("MAGI_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
    let state_root = read_env("MAGI_STATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(default_state_root);

    let runtime_state_manager = RuntimeStateManager::new(state_root.join("runtime"));
    let pid = process::id();
    let config = DaemonConfig::new(host.clone(), port, service_name, state_root)
        .with_open_browser(should_open_browser());
    let daemon = Daemon::new(config);
    let handle = daemon.start().await?;
    runtime_state_manager.write_runtime_state(pid, Some(&host), handle.bound_addr().port());
    runtime_state_manager.write_pid(pid);
    let result = handle.wait_for_shutdown_signal().await;

    runtime_state_manager.remove_runtime_state();
    runtime_state_manager.remove_pid();

    result?;
    Ok(())
}
