fn main() {
    if let Err(error) = magi_worker_runtime::run_local_worker_executor_stdio() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
