//! 任务系统 — M04b：本文件已下沉到 `magi-conversation-runtime::model_config`。
//!
//! 错误返回值改为 `Result<_, String>`，magi-api 调用方需要用
//! `.map_err(ApiError::InvalidInput)` 桥接。

pub(crate) use magi_conversation_runtime::model_config::{
    NormalizedModelConfig, merge_orchestrator_session_override,
    reject_deprecated_model_config_fields, strip_orchestrator_session_owned_fields,
};
