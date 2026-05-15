//! Task System v2 — P7：本文件已下沉到 `magi-conversation-runtime::tool_result_utils`。
//!
//! 本壳子仅做 `pub use` 重导出，保留迁移期间 task_llm_loop 与 session_turn_writeback
//! 旧 `use crate::tool_result_utils::*` 调用点的兼容性。M07 切完 import 后删除。

pub(crate) use magi_conversation_runtime::tool_result_utils::{
    infer_tool_call_status, summarize_tool_result, tool_execution_status_label,
    turn_item_status_for_tool_result,
};
