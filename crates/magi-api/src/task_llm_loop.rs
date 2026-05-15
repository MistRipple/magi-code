//! Task System v2 — M09：本文件已下沉到 `magi-conversation-runtime::task_llm_loop`。
//!
//! v2 版本的 `run_task_llm_loop` 仍返回 `(TaskOutcome, Option<ExecutionContextSummary>)`，
//! 无 ApiError 错误通道，magi-api 调用点 (`task_execution.rs`) 无需额外桥接。

pub(crate) use magi_conversation_runtime::task_llm_loop::{
    TaskLlmLoopRequest, run_task_llm_loop,
};
