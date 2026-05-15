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

mod builtin_tool_schema;
mod conversation;
mod driver;
mod mailbox;
mod registry;
mod skill_apply_tool;
mod stream;
pub mod tool_batch;
mod turn;

pub use builtin_tool_schema::{internal_builtin_tool_rejection_payload, public_builtin_tool_definitions};
pub use conversation::{AdvanceTurnError, BeginTurnError, Conversation, TurnAdvanceError};
pub use driver::{RoundOutcome, TurnDriver};
pub use mailbox::{MailboxItem, UserSignal};
pub use registry::ConversationRegistry;
pub use skill_apply_tool::{
    SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime, skill_apply_tool_definition,
};
pub use stream::{StreamEvent, StreamFanOut, SubscriptionId, ToolPhase};
pub use tool_batch::execute_task_tool_call_batch;
pub use turn::{Turn, TurnState, TurnTransitionError};
