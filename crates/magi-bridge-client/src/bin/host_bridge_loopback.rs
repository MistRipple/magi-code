fn main() {
    if let Err(error) = magi_bridge_client::run_host_bridge_loopback_server() {
        eprintln!("host bridge loopback server failed: {error}");
        std::process::exit(1);
    }
}
