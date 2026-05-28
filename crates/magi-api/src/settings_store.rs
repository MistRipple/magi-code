//! 任务系统 — M04a：本文件已下沉到 `magi-conversation-runtime::settings_store`。
//!
//! 本壳子仅做 `pub use` 重导出，让迁移期间 magi-api 内部 caller + 外部
//! magi-daemon 依然能按 `magi_api::SettingsStore` 引用。后续可直接切到
//! `magi_conversation_runtime::settings_store::*` 后删壳。

pub use magi_conversation_runtime::settings_store::SettingsStore;
