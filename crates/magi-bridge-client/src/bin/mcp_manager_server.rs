fn main() {
    if let Err(error) = magi_bridge_client::run_mcp_manager_server() {
        eprintln!("mcp manager server failed: {error}");
        std::process::exit(1);
    }
}
