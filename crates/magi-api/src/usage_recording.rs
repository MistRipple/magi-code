//! Task System v2 — M04b：本文件已下沉到 `magi-conversation-runtime::usage_recording`。

pub(crate) use magi_conversation_runtime::usage_recording::{
    ModelUsageBinding, model_usage_binding_for_worker, publish_model_usage_record,
};

// 仅 task_llm_loop 测试用例引用，需 #[cfg(test)] 避免非测试 build 报 unused_imports。
#[cfg(test)]
pub(crate) use magi_conversation_runtime::usage_recording::session_turn_model_usage_binding;
