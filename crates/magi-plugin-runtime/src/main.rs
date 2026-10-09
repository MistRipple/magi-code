fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.is_empty() {
        magi_plugin_runtime::run_worker_stdio()
    } else if args.len() == 1 && args[0] == "--preflight" {
        preflight()
    } else {
        Err(std::io::Error::other("不支持的 Worker 参数"))
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

/// 发行校验也经过真实 Host → stdio Worker → QuickJS → SDK 链路。
/// 不另设引擎或模拟返回值；安装包缺少运行依赖时直接阻止交付。
fn preflight() -> std::io::Result<()> {
    use magi_plugin_runtime::{
        CapabilityHandler, CapabilityRequest, ExecutionError, ExecutionErrorCode, Invocation,
        InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
    };
    use std::{future::Future, pin::Pin};

    struct Probe;
    impl CapabilityHandler for Probe {
        fn call(
            &self,
            request: CapabilityRequest,
        ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, ExecutionError>> + Send + '_>>
        {
            Box::pin(async move {
                if request.operation != "preflight.echo"
                    || request.identity.invocation_id != "release-preflight"
                    || request.cancellation.is_cancelled()
                {
                    return Err(ExecutionError::new(
                        ExecutionErrorCode::CapabilityRejected,
                        "发行探测请求无效",
                    ));
                }
                Ok(request.arguments)
            })
        }
    }

    let host = PluginHost::new(
        std::env::current_exe()?,
        magi_process::ManagedProcessGroup::new(),
    );
    let invocation = Invocation {
        identity: InvocationIdentity {
            plugin_id: "magi.release-probe".into(),
            package_digest: "release-preflight".into(),
            instance_id: "release-preflight".into(),
            invocation_id: "release-preflight".into(),
            workspace_id: None,
            run_id: None,
            attempt_id: None,
        },
        source: r#"export default async (input, sdk) => ({
            echo: await sdk.call('preflight.echo', input),
            isolated: typeof process === 'undefined' && typeof require === 'undefined'
                && typeof fetch === 'undefined' && typeof Deno === 'undefined'
        })"#
        .into(),
        input: serde_json::json!({"version": env!("CARGO_PKG_VERSION")}),
        limits: RuntimeLimits::default(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime
        .block_on(host.invoke(invocation, &Probe, &RunCancellation::default()))
        .map_err(std::io::Error::other)?;
    if result
        != serde_json::json!({"echo": {"version": env!("CARGO_PKG_VERSION")}, "isolated": true})
    {
        return Err(std::io::Error::other("插件 Worker 发行探测结果无效"));
    }
    println!(
        "{}",
        serde_json::json!({"workerVersion": env!("CARGO_PKG_VERSION"), "ok": true})
    );
    Ok(())
}
