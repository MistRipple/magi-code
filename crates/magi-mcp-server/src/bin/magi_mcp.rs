//! `magi-mcp`：Magi MCP 服务的 stdio 中继（命令行见 `magi_mcp_server::relay_cli`）。

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    magi_mcp_server::relay_cli::run_relay_cli(&args)
}
