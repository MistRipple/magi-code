use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};

/// 用户信号载荷。普通 Session Turn 由 TurnService 直接接纳；当前活跃 Turn 的
/// 引导输入由 SessionTurnCoordinator 的 turn-id 通道承载，不进入 Task mailbox。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSignal {
    pub text: Option<String>,
    /// 仅为当前进程的投递载荷；持久事实仍在 canonical 用户 item 的 images metadata。
    #[serde(default)]
    pub images: Vec<magi_bridge_client::llm_types::ImageSource>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
    pub accepted_at: UtcMillis,
}

/// Mailbox 信号作者。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MailboxAuthor {
    Agent(String),
    System,
    Parent(String),
    Child(String),
}

/// Mailbox 信号类型。运行时信号统一按这个枚举分类。
///
/// 注：代理初始任务、追问、系统 followup 等都进入同一任务信号通道；
/// 代理终态结果由 agent_wait 从 Task 终态及 canonical Step 证据读取，TaskStore
/// 仅保留调度状态与历史 output_refs。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailboxKind {
    Message,
    Decision,
    Interrupt,
    Followup,
}

/// 非用户输入类运行时信号。在任务循环的 Step 边界按 FIFO 消费；
/// 持久事实仍由原 Task、context package 与 canonical 日志拥有。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSignal {
    pub author: MailboxAuthor,
    pub kind: MailboxKind,
    pub trigger_turn: bool,
    pub payload: serde_json::Value,
    pub enqueued_at: UtcMillis,
}
