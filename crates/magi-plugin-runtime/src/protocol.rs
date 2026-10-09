use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 仅为私有 Worker IPC，不能作为客户端自报业务身份的 DTO。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationIdentity {
    pub plugin_id: String,
    pub package_digest: String,
    pub instance_id: String,
    pub invocation_id: String,
    pub workspace_id: Option<String>,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLimits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub timeout_ms: u64,
    pub max_frame_bytes: usize,
    pub max_capability_calls: u32,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 32 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            timeout_ms: 5_000,
            max_frame_bytes: 1024 * 1024,
            max_capability_calls: 64,
        }
    }
}

impl RuntimeLimits {
    pub(crate) fn validate(&self) -> Result<(), ExecutionError> {
        if !(2 * 1024 * 1024..=128 * 1024 * 1024).contains(&self.memory_bytes)
            || !(64 * 1024..=1024 * 1024).contains(&self.stack_bytes)
            || !(1..=60_000).contains(&self.timeout_ms)
            || !(1024..=4 * 1024 * 1024).contains(&self.max_frame_bytes)
            || !(1..=256).contains(&self.max_capability_calls)
        {
            return Err(ExecutionError::new(
                ExecutionErrorCode::InvalidRequest,
                "插件运行限制超出宿主允许范围",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub identity: InvocationIdentity,
    /// 唯一后台产物：一个无外部 import 的 UTF-8 ESM bundle。
    pub source: String,
    pub input: Value,
    pub limits: RuntimeLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    InvalidRequest,
    WorkerUnavailable,
    ProtocolViolation,
    ScriptFailed,
    ResourceLimit,
    TimedOut,
    Cancelled,
    CapabilityRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(deny_unknown_fields)]
#[error("{code:?}: {message}")]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
}

impl ExecutionError {
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkerFrame {
    Capability {
        sequence: u32,
        operation: String,
        arguments: Value,
    },
    Complete {
        result: Value,
    },
    Failed {
        error: ExecutionError,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityReply {
    pub sequence: u32,
    pub result: Result<Value, ExecutionError>,
}
