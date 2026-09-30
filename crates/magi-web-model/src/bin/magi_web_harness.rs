//! `magi-web-harness`：T3 的 stdio 中继（设计基线 §5.7.3）。
//!
//! 用法（由 `openai/tunnel-client` 以子进程方式拉起）：
//!
//! ```text
//! magi-web-harness --stdio [--local-socket <path> | --state-root <path>]
//! ```
//!
//! 它**不是** MCP server 本体：只把 stdin/stdout 与 daemon 的本地 socket 互转，
//! 令牌校验、挂起调用与去重账本都在 daemon 侧完成。本进程不监听 TCP、不写端口
//! 文件、不读凭据。

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 只报错误类别与参数名，不回显任何可能含路径之外的敏感内容。
            eprintln!("magi-web-harness: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == "--stdio") {
        return Err("缺少 --stdio：本入口只支持 stdio 中继".to_string());
    }
    let endpoint = match value_of(args, "--local-socket") {
        Some(path) => PathBuf::from(path),
        None => match value_of(args, "--state-root") {
            Some(root) => {
                magi_web_model::harness::stdio::local_endpoint_for(PathBuf::from(root).as_path())
            }
            None => {
                return Err("需要 --local-socket 或 --state-root".to_string());
            }
        },
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("运行时初始化失败：{error}"))?;
    runtime
        .block_on(magi_web_model::harness::StdioRelay::run(&endpoint))
        .map_err(|error| format!("中继结束：{error}"))
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
    }
    None
}
