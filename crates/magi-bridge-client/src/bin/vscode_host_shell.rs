fn main() {
    if let Err(error) = magi_bridge_client::run_vscode_host_shell_server() {
        eprintln!("vscode host shell server failed: {error}");
        std::process::exit(1);
    }
}
