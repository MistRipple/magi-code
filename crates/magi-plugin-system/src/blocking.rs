use crate::PluginRuntimeLease;
use magi_plugin_runtime::{
    CapabilityHandler, ExecutionError, ExecutionErrorCode, Invocation, PluginHost, RunCancellation,
};
use serde_json::Value;
use std::sync::Arc;

/// 在同步调用方（工具批、命令执行）中运行一次 Worker 调用并等待结果。
///
/// 调用方可能已处于异步运行时内，不能在当前线程再 `block_on`，因此使用独立线程
/// 与单线程运行时。运行租约随该线程结束释放，保证排空等待看到的是调用真正结算后的状态。
pub fn run_invocation_blocking(
    host: PluginHost,
    handler: Arc<dyn CapabilityHandler>,
    invocation: Invocation,
    cancellation: RunCancellation,
    lease: PluginRuntimeLease,
) -> Result<Value, ExecutionError> {
    std::thread::spawn(move || {
        let _lease = lease;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                ExecutionError::new(ExecutionErrorCode::WorkerUnavailable, "插件执行器不可用")
            })
            .and_then(|runtime| {
                runtime.block_on(host.invoke(invocation, handler.as_ref(), &cancellation))
            })
    })
    .join()
    .map_err(|_| {
        ExecutionError::new(
            ExecutionErrorCode::WorkerUnavailable,
            "插件执行线程异常退出",
        )
    })?
}
