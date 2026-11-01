use std::path::PathBuf;
use std::process::ExitCode;

use crate::local_socket::{StdioRelay, local_endpoint_for};

/// 以进程退出码运行中继。`args` 不含程序名。
pub fn run_relay_cli(args: &[String]) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 只报错误类别，不回显令牌或其它敏感内容。
            eprintln!("magi-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == "--stdio") {
        return Err("缺少 --stdio：本入口只支持 stdio 中继".to_string());
    }
    if args
        .iter()
        .any(|arg| arg == "--token" || arg.starts_with("--token="))
    {
        return Err(
            "不接受命令行令牌参数；请使用 --token-file 或环境变量 MAGI_MCP_TOKEN".to_string(),
        );
    }
    let endpoint = match (value_of(args, "--endpoint"), value_of(args, "--state-root")) {
        (Some(path), _) => PathBuf::from(path),
        (None, Some(root)) => local_endpoint_for(&PathBuf::from(root)),
        (None, None) => match std::env::var("MAGI_STATE_ROOT") {
            Ok(root) if !root.trim().is_empty() => local_endpoint_for(&PathBuf::from(root)),
            _ => return Err("需要 --endpoint、--state-root 或环境变量 MAGI_STATE_ROOT".to_string()),
        },
    };

    let token = match value_of(args, "--token-file") {
        Some(path) => {
            std::fs::read_to_string(&path).map_err(|error| format!("无法读取令牌文件：{error}"))?
        }
        None => std::env::var("MAGI_MCP_TOKEN")
            .map_err(|_| "需要 --token-file 或环境变量 MAGI_MCP_TOKEN".to_string())?,
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("令牌为空".to_string());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("运行时初始化失败：{error}"))?;
    runtime
        .block_on(StdioRelay::run(&endpoint, &token))
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
