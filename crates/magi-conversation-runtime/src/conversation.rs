use magi_core::SessionId;

use crate::mailbox::{Mailbox, MailboxItem, UserSignal};
use crate::turn::{Turn, TurnState, TurnTransitionError};

/// 一个 Conversation 绑定一个 SessionId 与其 Mailbox + 当前 Turn 槽位。
///
/// S2 起 Conversation 持有"当前 Turn"槽位，并强制"同 Conversation 不并发"：
/// 在已有未终态 Turn 时 `begin_turn` 会返回错误。
#[derive(Debug)]
pub struct Conversation {
    session_id: SessionId,
    mailbox: Mailbox,
    /// 当前 Turn 槽位。终态后归 None。S2 范围内仅 v2 advance_turn 入口写。
    current_turn: Option<Turn>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BeginTurnError {
    /// 已存在未结束的 Turn——违反"单 Conversation 不并发 Turn"不变式。
    TurnAlreadyActive(TurnState),
}

impl std::fmt::Display for BeginTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TurnAlreadyActive(state) => {
                write!(f, "conversation already has an active turn in state {state}")
            }
        }
    }
}

impl std::error::Error for BeginTurnError {}

impl Conversation {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            mailbox: Mailbox::new(),
            current_turn: None,
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

    /// 开启一个新的 Turn——违反"同 Conversation 不并发"不变式时返回错误。
    pub fn begin_turn(&mut self) -> Result<(), BeginTurnError> {
        if let Some(turn) = &self.current_turn {
            if !turn.is_terminal() {
                return Err(BeginTurnError::TurnAlreadyActive(turn.state()));
            }
        }
        self.current_turn = Some(Turn::new());
        Ok(())
    }

    pub fn current_turn_state(&self) -> Option<TurnState> {
        self.current_turn.as_ref().map(Turn::state)
    }

    /// 改变当前 Turn 状态——必须在 `begin_turn` 之后。S2 范围内仅由 v1 adapter
    /// 在每个状态切点上回调。
    pub fn advance_current_turn<F>(&mut self, op: F) -> Result<(), TurnAdvanceError>
    where
        F: FnOnce(&mut Turn) -> Result<(), TurnTransitionError>,
    {
        let turn = self
            .current_turn
            .as_mut()
            .ok_or(TurnAdvanceError::NoActiveTurn)?;
        op(turn).map_err(TurnAdvanceError::Transition)
    }

    /// 终结当前 Turn——回收槽位让下一轮可以开始。要求 Turn 已在终态。
    pub fn end_turn(&mut self) -> Result<TurnState, TurnAdvanceError> {
        let turn = self
            .current_turn
            .take()
            .ok_or(TurnAdvanceError::NoActiveTurn)?;
        if !turn.is_terminal() {
            // 回填以保留状态，避免悄悄丢失活跃 Turn
            let state = turn.state();
            self.current_turn = Some(turn);
            return Err(TurnAdvanceError::TurnNotTerminal(state));
        }
        Ok(turn.state())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TurnAdvanceError {
    NoActiveTurn,
    TurnNotTerminal(TurnState),
    Transition(TurnTransitionError),
}

impl std::fmt::Display for TurnAdvanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoActiveTurn => f.write_str("no active turn"),
            Self::TurnNotTerminal(state) => {
                write!(f, "cannot end turn in non-terminal state {state}")
            }
            Self::Transition(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for TurnAdvanceError {}

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

    #[test]
    fn begin_turn_then_end_turn_releases_slot() {
        let mut conv = Conversation::new(SessionId::new("s"));
        conv.begin_turn().unwrap();
        conv.advance_current_turn(|turn| turn.enter_modeling())
            .unwrap();
        conv.advance_current_turn(|turn| turn.finish_done()).unwrap();
        assert_eq!(conv.end_turn().unwrap(), TurnState::Done);
        // 下一轮可以正常开始
        conv.begin_turn().unwrap();
    }

    #[test]
    fn begin_turn_rejects_when_already_active() {
        let mut conv = Conversation::new(SessionId::new("s"));
        conv.begin_turn().unwrap();
        let err = conv.begin_turn().unwrap_err();
        assert_eq!(err, BeginTurnError::TurnAlreadyActive(TurnState::Pending));
    }

    #[test]
    fn end_turn_rejects_when_non_terminal() {
        let mut conv = Conversation::new(SessionId::new("s"));
        conv.begin_turn().unwrap();
        conv.advance_current_turn(|turn| turn.enter_modeling())
            .unwrap();
        let err = conv.end_turn().unwrap_err();
        assert_eq!(err, TurnAdvanceError::TurnNotTerminal(TurnState::Modeling));
        // 槽位仍占着
        assert_eq!(conv.current_turn_state(), Some(TurnState::Modeling));
    }

    #[test]
    fn advance_without_begin_errors() {
        let mut conv = Conversation::new(SessionId::new("s"));
        let err = conv
            .advance_current_turn(|turn| turn.enter_modeling())
            .unwrap_err();
        assert_eq!(err, TurnAdvanceError::NoActiveTurn);
    }
}
