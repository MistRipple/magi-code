use crate::protocol::{CapabilityReply, WorkerFrame};
use crate::{ExecutionError, ExecutionErrorCode, Invocation, InvocationIdentity};
use magi_process::ManagedProcessGroup;
use serde_json::Value;
use std::{future::Future, path::PathBuf, pin::Pin, process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, watch};

/// 生命周期所有者持有的取消信号；不能由插件清除或替换。
#[derive(Clone, Debug)]
pub struct RunCancellation(watch::Sender<bool>);

impl Default for RunCancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl RunCancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

pub struct CapabilityRequest {
    /// 只来自 daemon 提交的 Invocation，而不是 Worker 的报文。
    pub identity: InvocationIdentity,
    pub operation: String,
    pub arguments: Value,
    pub cancellation: RunCancellation,
}

/// 实现方必须经原生授权入口执行，不得据此创建另一份业务状态。
///
/// 每个实现必须传播 cancellation，且 future 被丢弃时不得留下无归属后台执行。
/// 外部副作用的终态与未确认结果仍由对应原生服务持有。
pub trait CapabilityHandler: Send + Sync {
    fn call(
        &self,
        request: CapabilityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ExecutionError>> + Send + '_>>;
}

/// 执行宿主不持有用户授权，也不解释工作流。该对象及其进程组归 daemon 所有。
#[derive(Clone)]
pub struct PluginHost {
    worker_path: PathBuf,
    processes: ManagedProcessGroup,
    capacity: Arc<Semaphore>,
}

struct InvocationCancellationGuard(RunCancellation);

impl Drop for InvocationCancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl PluginHost {
    pub fn new(worker_path: PathBuf, processes: ManagedProcessGroup) -> Self {
        Self {
            worker_path,
            processes,
            capacity: Arc::new(Semaphore::new(4)),
        }
    }

    pub async fn invoke(
        &self,
        invocation: Invocation,
        handler: &dyn CapabilityHandler,
        cancellation: &RunCancellation,
    ) -> Result<Value, ExecutionError> {
        invocation.limits.validate()?;
        if !self.worker_path.is_absolute() || invocation.identity.invocation_id.is_empty() {
            return Err(ExecutionError::new(
                ExecutionErrorCode::InvalidRequest,
                "无效宿主执行身份或 Worker 路径",
            ));
        }
        let encoded = serde_json::to_vec(&invocation).map_err(protocol_error)?;
        if encoded.len() > invocation.limits.max_frame_bytes {
            return Err(ExecutionError::new(
                ExecutionErrorCode::ResourceLimit,
                "插件执行输入超过报文上限",
            ));
        }
        let deadline = tokio::time::sleep(Duration::from_millis(invocation.limits.timeout_ms));
        tokio::pin!(deadline);
        let permit = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(cancelled_error()),
            _ = &mut deadline => return Err(timeout_error()),
            permit = self.capacity.acquire() => permit.map_err(protocol_error)?,
        };
        let mut command = magi_process::tokio_command(&self.worker_path);
        command
            .env_clear()
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = self.processes.spawn_tokio(&mut command).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCode::WorkerUnavailable,
                "无法启动已打包的插件 Worker",
            )
        })?;
        let mut input = child
            .take_stdin()
            .ok_or_else(|| protocol_error("Worker stdin 缺失"))?;
        let output = child
            .take_stdout()
            .ok_or_else(|| protocol_error("Worker stdout 缺失"))?;
        let callback_cancellation = RunCancellation::default();
        let _cancellation_guard = InvocationCancellationGuard(callback_cancellation.clone());
        let exchange = async {
            input.write_all(&encoded).await.map_err(protocol_error)?;
            input.write_all(b"\n").await.map_err(protocol_error)?;
            input.flush().await.map_err(protocol_error)?;
            let mut output = BufReader::new(output);
            let mut sequence = 0;
            loop {
                let mut bytes = Vec::new();
                (&mut output)
                    .take(invocation.limits.max_frame_bytes as u64 + 1)
                    .read_until(b'\n', &mut bytes)
                    .await
                    .map_err(protocol_error)?;
                if bytes.is_empty()
                    || !bytes.ends_with(b"\n")
                    || bytes.len() > invocation.limits.max_frame_bytes
                {
                    return Err(protocol_error("Worker 报文缺失、被截断或超限"));
                }
                let frame: WorkerFrame = serde_json::from_slice(&bytes).map_err(protocol_error)?;
                match frame {
                    WorkerFrame::Complete { result } => return Ok(result),
                    WorkerFrame::Failed { error } => return Err(error),
                    WorkerFrame::Capability {
                        sequence: received,
                        operation,
                        arguments,
                    } => {
                        sequence += 1;
                        if received != sequence
                            || sequence > invocation.limits.max_capability_calls
                            || !valid_operation(&operation)
                        {
                            return Err(protocol_error("Worker 能力请求身份、顺序或数量无效"));
                        }
                        let result = handler
                            .call(CapabilityRequest {
                                identity: invocation.identity.clone(),
                                operation,
                                arguments,
                                cancellation: callback_cancellation.clone(),
                            })
                            .await;
                        let reply = serde_json::to_vec(&CapabilityReply { sequence, result })
                            .map_err(protocol_error)?;
                        if reply.len() + 1 > invocation.limits.max_frame_bytes {
                            return Err(ExecutionError::new(
                                ExecutionErrorCode::ResourceLimit,
                                "宿主能力结果超过报文上限",
                            ));
                        }
                        input.write_all(&reply).await.map_err(protocol_error)?;
                        input.write_all(b"\n").await.map_err(protocol_error)?;
                        input.flush().await.map_err(protocol_error)?;
                    }
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(cancelled_error()),
            _ = &mut deadline => Err(timeout_error()),
            result = exchange => result,
        };
        callback_cancellation.cancel();
        // 无论完成、异常、超时或取消，都结算同一受管进程；不会留下等待宿主响应的 Worker。
        drop(input);
        let stopped = match tokio::time::timeout(Duration::from_millis(100), child.wait()).await {
            Ok(status) => status,
            Err(_) => child.terminate().await,
        };
        stopped.map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCode::WorkerUnavailable,
                "插件 Worker 未完成进程释放",
            )
        })?;
        drop(permit);
        result
    }
}

pub(crate) fn valid_operation(operation: &str) -> bool {
    !operation.is_empty()
        && operation.len() <= 96
        && operation.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

fn protocol_error(error: impl std::fmt::Display) -> ExecutionError {
    let _ = error; // 不把 OS 路径、插件报文或宿主内部错误当作可信错误文本返回。
    ExecutionError::new(
        ExecutionErrorCode::ProtocolViolation,
        "插件 Worker 通信失败",
    )
}

fn timeout_error() -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::TimedOut, "插件执行超过宿主时限")
}

fn cancelled_error() -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::Cancelled, "插件执行已取消")
}
