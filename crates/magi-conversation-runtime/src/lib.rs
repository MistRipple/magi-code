//! Task System v2 - Conversation Runtime
//!
//! 提供 Mailbox 作为 user 信号进入任务系统的**单一通道**，
//! Conversation 绑定 SessionId、Mailbox 与当前 Turn 槽位，
//! Turn 状态机刻画"一轮 user → assistant"的推进契约。
//!
//! 已交付 slice：
//! - S1：Mailbox + Conversation 骨架（user 信号入栈姿势）
//! - S2：Turn 状态机 + 单 Conversation 不并发不变式（v2 拥有 Turn lifecycle，
//!   v1 `run_task_llm_loop` 暂作"一轮 IO 引擎"被 v2 调度）

mod conversation;
mod mailbox;
mod registry;
mod turn;

pub use conversation::{BeginTurnError, Conversation, TurnAdvanceError};
pub use mailbox::{MailboxItem, UserSignal};
pub use registry::ConversationRegistry;
pub use turn::{Turn, TurnState, TurnTransitionError};
