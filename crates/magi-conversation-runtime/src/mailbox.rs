use std::collections::VecDeque;

use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};

/// 用户信号载荷。普通 Session Turn 由 TurnService 直接接纳；当前活跃 Turn 的
/// 引导输入由 SessionTurnCoordinator 的 turn-id 通道承载，不进入 Task mailbox。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSignal {
    pub text: Option<String>,
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
/// 注：代理初始任务、追问、系统 followup 等都统一进入 Conversation
/// mailbox；代理终态结果由 agent_wait 从对应 thread transcript 读取，TaskStore
/// 仅保留调度状态与历史 output_refs。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailboxKind {
    Message,
    Decision,
    Interrupt,
    Followup,
}

/// 非用户输入类运行时信号。所有跨任务/代理/系统调度输入都以该结构进入
/// Conversation Mailbox，在下一次 Turn 边界被一次性 drain。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSignal {
    pub author: MailboxAuthor,
    pub kind: MailboxKind,
    pub trigger_turn: bool,
    pub payload: serde_json::Value,
    pub enqueued_at: UtcMillis,
}

/// Task Conversation mailbox 内的运行时信号。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxItem {
    Runtime(RuntimeSignal),
}

impl MailboxItem {
    pub fn runtime(signal: RuntimeSignal) -> Self {
        Self::Runtime(signal)
    }
}

/// 按 FIFO 顺序累积的信号缓冲。Conversation 在 Turn 边界 drain，不向外暴露
/// 任意位置的 peek/pop，Conversation 不主动 pull。
#[derive(Debug, Default)]
pub struct Mailbox {
    items: VecDeque<MailboxItem>,
}

impl Mailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: MailboxItem) {
        self.items.push_back(item);
    }

    /// Turn 边界唯一消费入口：按 FIFO 取出全部待处理信号。
    pub fn drain_all(&mut self) -> Vec<MailboxItem> {
        self.items.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_signal(text: &str, at: u64) -> RuntimeSignal {
        RuntimeSignal {
            author: MailboxAuthor::Parent("task-parent".to_string()),
            kind: MailboxKind::Message,
            trigger_turn: true,
            payload: serde_json::json!({"text": text}),
            enqueued_at: UtcMillis(at),
        }
    }

    #[test]
    fn push_and_drain_preserves_order() {
        let mut mailbox = Mailbox::new();
        mailbox.push(MailboxItem::runtime(runtime_signal("a", 1)));
        mailbox.push(MailboxItem::runtime(runtime_signal("b", 2)));

        let signals = mailbox.drain_all();
        assert_eq!(signals.len(), 2);
        assert!(
            matches!(signals[0], MailboxItem::Runtime(ref signal) if signal.payload["text"] == "a")
        );
        assert!(
            matches!(signals[1], MailboxItem::Runtime(ref signal) if signal.payload["text"] == "b")
        );
        assert!(mailbox.drain_all().is_empty());
    }

    #[test]
    fn drain_on_empty_returns_empty() {
        let mut mailbox = Mailbox::new();
        assert!(mailbox.drain_all().is_empty());
    }
}
