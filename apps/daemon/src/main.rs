#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{env, path::PathBuf, process};

use magi_agent::runtime_state::RuntimeStateManager;
use magi_daemon::{Daemon, DaemonConfig};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 38123;
const DEFAULT_SERVICE_NAME: &str = "magi-rust-backend";

fn default_state_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".magi")
}

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .compact()
        .init();

    let host = read_env("MAGI_HOST").unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = read_port()?;
    let service_name =
        read_env("MAGI_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
    let state_root = read_env("MAGI_STATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(default_state_root);

    let runtime_state_manager = RuntimeStateManager::new(state_root.join("agent"));
    let pid = process::id();
    runtime_state_manager.write_runtime_state(pid, Some(&host), port);
    runtime_state_manager.write_pid(pid);

    let config = DaemonConfig::new(host, port, service_name, state_root)
        .with_open_browser(should_open_browser());
    let daemon = Daemon::new(config);
    let result = daemon.run().await;

    runtime_state_manager.remove_runtime_state();
    runtime_state_manager.remove_pid();

    result?;
    Ok(())
}
