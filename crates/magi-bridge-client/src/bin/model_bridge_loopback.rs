fn main() {
    if let Err(error) = magi_bridge_client::run_model_bridge_loopback_server() {
        eprintln!("model bridge loopback server failed: {error}");
        std::process::exit(1);
    }
}
