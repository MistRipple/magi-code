use std::{env, path::PathBuf, process};

use magi_agent::runtime_state::RuntimeStateManager;
use magi_daemon::{Daemon, DaemonConfig};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 38123;
const DEFAULT_SERVICE_NAME: &str = "magi-shadow-rust-backend";
const DEFAULT_STATE_ROOT: &str = "/Users/xie/code/magi-rust-rewrite/tmp/state";

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
    let state_root = PathBuf::from(
        read_env("MAGI_STATE_ROOT").unwrap_or_else(|| DEFAULT_STATE_ROOT.to_string()),
    );

    let runtime_state_manager = RuntimeStateManager::new(state_root.join("agent"));
    let pid = process::id();
    runtime_state_manager.write_runtime_state(pid, Some(&host), port);
    runtime_state_manager.write_pid(pid);

    let config = DaemonConfig::new(
        host,
        port,
        service_name,
        state_root,
    );
    let daemon = Daemon::new(config);
    let result = daemon.run().await;

    runtime_state_manager.remove_runtime_state();
    runtime_state_manager.remove_pid();

    result?;
    Ok(())
}
