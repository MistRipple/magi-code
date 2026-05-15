//! Task System v2 — M04a：本文件已下沉到 `magi-conversation-runtime::prompt_utils`。
//!
//! 本壳子仅做 `pub use` 重导出，让迁移期间 magi-api 内部 caller
//! （task_llm_loop / task_execution / session_turn_execution）继续按
//! `crate::prompt_utils::xxx` 引用。后续切完 import 后删壳。

pub(crate) use magi_conversation_runtime::prompt_utils::prepend_session_instructions;
