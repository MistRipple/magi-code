//! Task System v2 - Conversation Runtime（Slice S1 起手）
//!
//! 提供 Mailbox 作为 user 信号进入任务系统的**单一通道**，
//! 以及 Conversation 骨架将 SessionId 与其 Mailbox 绑定。
//!
//! S1 仅覆盖"user 信号入栈姿势"。Turn 状态机、模型 IO、工具 IO
//! 等下游执行仍由 v1 task_llm_loop / dispatcher 承担，由 S2 起后续 slice 替换。

mod conversation;
mod mailbox;
mod registry;

pub use conversation::Conversation;
pub use mailbox::{MailboxItem, UserSignal};
pub use registry::ConversationRegistry;
