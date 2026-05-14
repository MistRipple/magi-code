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

/// Mailbox 内的信号变体。S1 仅 User 一类；后续 slice 会扩展 ToolResult、ChildFinal、
/// SystemSignal 等运行时信号。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxItem {
    User(UserSignal),
}

impl MailboxItem {
    pub fn user(signal: UserSignal) -> Self {
        Self::User(signal)
    }
}

/// 按 FIFO 顺序累积的信号缓冲。Conversation 在 Turn 边界 drain，不向外暴露
/// 任意位置的 peek/pop（按 v2 设计，Conversation 不主动 pull）。
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

    /// 取出全部 user 信号。S1 中 Mailbox 仅 User 变体；
    /// 该方法在 S2+ 引入其他变体时仍保持类型精确。
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
            })
            .collect();
        self.items = remainder;
        signals
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
}
