//! Task System v2 — M04c：本文件已下沉到 `magi-conversation-runtime::session_turn_execution`。
//!
//! v2 版本错误返回值改为 `Result<_, String>`，magi-api 调用方需要用
//! `.map_err(|msg| ApiError::model_invocation_failed("执行 session turn 失败", msg))`
//! 桥接到 `ApiError` 枚举。

pub use magi_conversation_runtime::session_turn_execution::{
    BUSINESS_MODEL_PROVIDER, SessionTurnExecutionOutput, SessionTurnExecutionRequest,
    SessionTurnExecutionRuntime, run_session_turn_execution,
};
