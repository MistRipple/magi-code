use std::collections::VecDeque;

use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};

/// 用户信号载荷。S1 仅含从前端进入的 user input，不区分 chat / task / continue /
/// supplement_context 路由——路由分流由 routes 层在 drain 后判定。
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
    User,
    Agent(String),
    System,
    Parent(String),
    Child(String),
}

/// Mailbox 信号类型。运行时信号统一按这个枚举分类。
///
/// 注：代理初始任务、追问、系统 followup 等都统一进入 Conversation
/// mailbox；代理终态结果由 agent_wait 从 TaskStore 读取。
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

/// Mailbox 内的信号变体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxItem {
    User(UserSignal),
    Runtime(RuntimeSignal),
}

impl MailboxItem {
    pub fn user(signal: UserSignal) -> Self {
        Self::User(signal)
    }

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

    /// 取出全部 user 信号，并保留非 user 运行时信号等待 Turn 边界消费。
    pub fn drain_user_signals(&mut self) -> Vec<UserSignal> {
        let mut signals = Vec::with_capacity(self.items.len());
        let remainder: VecDeque<MailboxItem> = self
            .items
            .drain(..)
            .filter_map(|item| match item {
                MailboxItem::User(signal) => {
                    signals.push(signal);
                    None
                }
                other => Some(other),
            })
            .collect();
        self.items = remainder;
        signals
    }

    /// Turn 边界唯一消费入口：按 FIFO 取出全部待处理信号。
    pub fn drain_all(&mut self) -> Vec<MailboxItem> {
        self.items.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_signal(text: &str) -> UserSignal {
        UserSignal {
            text: Some(text.to_string()),
            request_id: None,
            user_message_id: None,
            placeholder_message_id: None,
            accepted_at: UtcMillis(0),
        }
    }

    #[test]
    fn push_and_drain_preserves_order() {
        let mut mailbox = Mailbox::new();
        mailbox.push(MailboxItem::user(sample_signal("a")));
        mailbox.push(MailboxItem::user(sample_signal("b")));

        let signals = mailbox.drain_user_signals();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].text.as_deref(), Some("a"));
        assert_eq!(signals[1].text.as_deref(), Some("b"));

        // 再次 drain 应为空。
        assert!(mailbox.drain_user_signals().is_empty());
    }

    #[test]
    fn drain_on_empty_returns_empty() {
        let mut mailbox = Mailbox::new();
        assert!(mailbox.drain_user_signals().is_empty());
    }

    #[test]
    fn drain_user_signals_preserves_runtime_items() {
        let mut mailbox = Mailbox::new();
        mailbox.push(MailboxItem::runtime(RuntimeSignal {
            author: MailboxAuthor::Parent("task-parent".to_string()),
            kind: MailboxKind::Message,
            trigger_turn: true,
            payload: serde_json::json!({"text": "继续执行"}),
            enqueued_at: UtcMillis(10),
        }));
        mailbox.push(MailboxItem::user(sample_signal("user")));

        let users = mailbox.drain_user_signals();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].text.as_deref(), Some("user"));
        let pending = mailbox.drain_all();
        assert!(matches!(pending.as_slice(), [MailboxItem::Runtime(_)]));
        assert!(mailbox.drain_all().is_empty());
    }
}
