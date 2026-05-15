//! Task System v2 — P7：本文件已下沉到 `magi-conversation-runtime::session_writeback`。
//!
//! 本壳子仅做 `pub use` 重导出，让迁移期间所有内部 caller（task_llm_loop /
//! session_turn_execution / task_execution / dispatch_flow / routes::*）继续按
//! `crate::session_turn_writeback::xxx` 引用。下一刀 M07 会切完所有 import 后
//! 删除本文件。

pub use magi_conversation_runtime::session_writeback::{
    append_session_tool_call_items_batch, append_session_turn_error_item, append_session_turn_item,
    append_session_turn_item_with_task_store, publish_current_session_turn_item_event,
    publish_session_turn_item_event, session_turn_item, upsert_session_turn_item,
    upsert_session_turn_item_with_task_store,
};
