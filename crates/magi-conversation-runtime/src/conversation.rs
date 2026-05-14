use magi_core::SessionId;

use crate::mailbox::{Mailbox, MailboxItem, UserSignal};

/// 一个 Conversation 绑定一个 SessionId 与其 Mailbox。
///
/// S1 中 Conversation 是骨架——仅承载"信号入栈通道"，不持有 Turn 状态、不持有
/// 模型 client、不直接 spawn 执行。下游执行仍由 routes 层调用 v1 dispatcher 完成。
///
/// 后续 slice：
/// - S2 引入 Turn 状态机与 advance_turn 主循环
/// - S3 接管模型 token / 工具事件流的派生订阅
/// - S6 引入 SpawnGraph 父子 Conversation 关系
#[derive(Debug)]
pub struct Conversation {
    session_id: SessionId,
    mailbox: Mailbox,
}

impl Conversation {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            mailbox: Mailbox::new(),
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 推入一个 user 信号。所有 user input（chat / task / continue /
    /// supplement_context 等）必须经此通道进入任务系统。
    pub fn ingest_user_signal(&mut self, signal: UserSignal) {
        self.mailbox.push(MailboxItem::user(signal));
    }

    /// 取出并消费 mailbox 中累积的 user 信号。
    pub fn drain_user_signals(&mut self) -> Vec<UserSignal> {
        self.mailbox.drain_user_signals()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::UtcMillis;

    fn sample_signal(text: &str) -> UserSignal {
        UserSignal {
            text: Some(text.to_string()),
            request_id: Some(format!("req-{}", text)),
            user_message_id: None,
            placeholder_message_id: None,
            accepted_at: UtcMillis(42),
        }
    }

    #[test]
    fn ingest_and_drain_yields_signal_in_order() {
        let mut conv = Conversation::new(SessionId::new("session-1"));
        conv.ingest_user_signal(sample_signal("hello"));
        conv.ingest_user_signal(sample_signal("world"));

        let signals = conv.drain_user_signals();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].text.as_deref(), Some("hello"));
        assert_eq!(signals[1].text.as_deref(), Some("world"));
        assert!(conv.drain_user_signals().is_empty());
    }

    #[test]
    fn drain_again_yields_empty() {
        let mut conv = Conversation::new(SessionId::new("session-2"));
        conv.ingest_user_signal(sample_signal("once"));
        let first = conv.drain_user_signals();
        assert_eq!(first.len(), 1);
        let second = conv.drain_user_signals();
        assert!(second.is_empty());
    }
}
