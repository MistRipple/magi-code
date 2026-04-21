fn main() {
    if let Err(error) = magi_bridge_client::run_mcp_bridge_loopback_server() {
        eprintln!("mcp bridge loopback server failed: {error}");
        std::process::exit(1);
    }
}
