use magi_core::SessionId;

use crate::driver::{RoundOutcome, TurnDriver};
use crate::mailbox::{Mailbox, MailboxItem, RuntimeSignal, UserSignal};
use crate::turn::{Turn, TurnState, TurnTransitionError};

/// 一个 Conversation 绑定一个 SessionId 与其 Mailbox + 当前 Turn 槽位。
///
/// S2 起 Conversation 持有"当前 Turn"槽位，并强制"同 Conversation 不并发"：
/// 在已有未终态 Turn 时 `begin_turn` 会返回错误。
#[derive(Debug)]
pub struct Conversation {
    session_id: SessionId,
    mailbox: Mailbox,
    /// 当前 Turn 槽位。终态后归 None。S2 范围内仅 advance_turn 入口写。
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
                write!(
                    f,
                    "conversation already has an active turn in state {state}"
                )
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

    /// 推入运行时信号。代理回执、Coordinator 指令、系统 followup 等都通过
    /// Mailbox 进入下一次 Turn，而不是绕到事件总线里当作隐式业务通道。
    pub fn ingest_runtime_signal(&mut self, signal: RuntimeSignal) {
        self.mailbox.push(MailboxItem::runtime(signal));
    }

    /// 取出并消费 mailbox 中累积的 user 信号。
    pub fn drain_user_signals(&mut self) -> Vec<UserSignal> {
        self.mailbox.drain_user_signals()
    }

    /// Turn 边界的完整消费入口。driver 会把这批待处理信号注入下一轮 prompt。
    pub fn drain_mailbox_items(&mut self) -> Vec<MailboxItem> {
        self.mailbox.drain_all()
    }

    /// 开启一个新的 Turn——违反"同 Conversation 不并发"不变式时返回错误。
    pub fn begin_turn(&mut self) -> Result<(), BeginTurnError> {
        if let Some(turn) = &self.current_turn
            && !turn.is_terminal()
        {
            return Err(BeginTurnError::TurnAlreadyActive(turn.state()));
        }
        self.current_turn = Some(Turn::new());
        Ok(())
    }

    pub fn current_turn_state(&self) -> Option<TurnState> {
        self.current_turn.as_ref().map(Turn::state)
    }

    /// 改变当前 Turn 状态——必须在 `begin_turn` 之后。S2 范围内仅由上层适配器
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

    /// 跑完一个完整 Turn：持有 for-round 循环骨架与状态机推进，
    /// 把每一轮内部的"拼请求 / 调模型 / 工具执行 / 写 turn item"全部交给 driver。
    ///
    /// 这是 Tier 1 取代旧式单体调度中的
    /// `for round in 0..tool_call_round_limit` 段的唯一入口。Conversation 自己
    /// 调 `begin_turn` / `enter_modeling` / `enter_tool_calling` / `finish_done` /
    /// `finish_failed` / `end_turn`，业务侧拿不到中间状态。
    pub fn advance_turn<D>(&mut self, mut driver: D) -> Result<D::Outcome, AdvanceTurnError>
    where
        D: TurnDriver,
    {
        self.begin_turn().map_err(AdvanceTurnError::Begin)?;
        driver.accept_mailbox_items(self.drain_mailbox_items());

        if let Some(outcome) = driver.deterministic_shortcut() {
            // deterministic 路径：不进入 modeling，直接 done。
            self.advance_current_turn(|turn| turn.finish_done())
                .map_err(AdvanceTurnError::Advance)?;
            self.end_turn().map_err(AdvanceTurnError::Advance)?;
            return Ok(outcome);
        }

        self.advance_current_turn(|turn| turn.enter_modeling())
            .map_err(AdvanceTurnError::Advance)?;

        let limit = driver.round_limit();
        for round in 0..limit {
            // 每轮入口确保处于 Modeling（除第一轮已由上面 enter_modeling 进入外，
            // 后续轮次需要从 ToolCalling 回到 Modeling）。
            if round > 0 {
                self.advance_current_turn(|turn| turn.enter_modeling())
                    .map_err(AdvanceTurnError::Advance)?;
            }
            // before_round hook：driver 在新一轮模型调用前沉淀上一轮 ToolCalling
            // 的副作用（例如把 tool_messages 推入下轮请求构造）。
            driver.before_round(round);
            match driver.execute_round(round) {
                RoundOutcome::Continue => {
                    // driver 已经执行完工具批；状态推到 ToolCalling，下一轮开头再回 Modeling。
                    self.advance_current_turn(|turn| turn.enter_tool_calling())
                        .map_err(AdvanceTurnError::Advance)?;
                }
                RoundOutcome::Done => {
                    self.advance_current_turn(|turn| turn.finish_done())
                        .map_err(AdvanceTurnError::Advance)?;
                    self.end_turn().map_err(AdvanceTurnError::Advance)?;
                    return Ok(driver.finalize_success());
                }
                RoundOutcome::Failed(reason) => {
                    self.advance_current_turn(|turn| turn.finish_failed())
                        .map_err(AdvanceTurnError::Advance)?;
                    self.end_turn().map_err(AdvanceTurnError::Advance)?;
                    return Ok(driver.finalize_round_failure(reason));
                }
            }
        }

        // round_limit 耗尽——driver 自决最终 Outcome 形态（旧实现通常把这归到失败）。
        self.advance_current_turn(|turn| turn.finish_failed())
            .map_err(AdvanceTurnError::Advance)?;
        self.end_turn().map_err(AdvanceTurnError::Advance)?;
        Ok(driver.finalize_exhausted())
    }
}

/// `advance_turn` 失败原因。
#[derive(Debug)]
pub enum AdvanceTurnError {
    /// 进入 Turn 时违反"单 Conversation 不并发"不变式。
    Begin(BeginTurnError),
    /// 状态机推进时违反约束（例如 driver 返回 Continue 但 Turn 已是终态）。
    Advance(TurnAdvanceError),
}

impl std::fmt::Display for AdvanceTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Begin(err) => write!(f, "advance_turn begin: {err}"),
            Self::Advance(err) => write!(f, "advance_turn advance: {err}"),
        }
    }
}

impl std::error::Error for AdvanceTurnError {}

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
        conv.advance_current_turn(|turn| turn.finish_done())
            .unwrap();
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

    /// 用 fake driver 验证 advance_turn 的 for-round 循环骨架。
    struct FakeDriver {
        round_limit: usize,
        plan: Vec<RoundOutcome>,
        executed: Vec<usize>,
        finalize_call: std::cell::Cell<Option<&'static str>>,
        deterministic: Option<&'static str>,
    }

    impl FakeDriver {
        fn new(plan: Vec<RoundOutcome>) -> Self {
            Self {
                round_limit: plan.len().max(1),
                plan,
                executed: Vec::new(),
                finalize_call: std::cell::Cell::new(None),
                deterministic: None,
            }
        }

        fn with_round_limit(mut self, limit: usize) -> Self {
            self.round_limit = limit;
            self
        }

        fn with_deterministic(mut self, value: &'static str) -> Self {
            self.deterministic = Some(value);
            self
        }
    }

    impl crate::driver::TurnDriver for FakeDriver {
        type Outcome = String;

        fn round_limit(&self) -> usize {
            self.round_limit
        }

        fn deterministic_shortcut(&mut self) -> Option<Self::Outcome> {
            self.deterministic.map(|v| v.to_string())
        }

        fn execute_round(&mut self, round: usize) -> RoundOutcome {
            self.executed.push(round);
            // 每轮按 plan 顺序消费一个 outcome
            if self.plan.is_empty() {
                RoundOutcome::Continue
            } else {
                self.plan.remove(0)
            }
        }

        fn finalize_success(self) -> Self::Outcome {
            self.finalize_call.set(Some("success"));
            "success".to_string()
        }

        fn finalize_round_failure(self, reason: String) -> Self::Outcome {
            self.finalize_call.set(Some("failure"));
            format!("failure:{reason}")
        }

        fn finalize_exhausted(self) -> Self::Outcome {
            self.finalize_call.set(Some("exhausted"));
            "exhausted".to_string()
        }
    }

    #[test]
    fn advance_turn_done_after_single_round() {
        let mut conv = Conversation::new(SessionId::new("s"));
        let driver = FakeDriver::new(vec![RoundOutcome::Done]);
        let outcome = conv.advance_turn(driver).unwrap();
        assert_eq!(outcome, "success");
        // Turn slot 已释放，可以再开下一轮
        conv.begin_turn().unwrap();
    }

    #[test]
    fn advance_turn_continues_then_done() {
        let mut conv = Conversation::new(SessionId::new("s"));
        let driver = FakeDriver::new(vec![RoundOutcome::Continue, RoundOutcome::Done]);
        let outcome = conv.advance_turn(driver).unwrap();
        assert_eq!(outcome, "success");
    }

    #[test]
    fn advance_turn_failure_propagates_reason() {
        let mut conv = Conversation::new(SessionId::new("s"));
        let driver = FakeDriver::new(vec![RoundOutcome::Failed("boom".to_string())]);
        let outcome = conv.advance_turn(driver).unwrap();
        assert_eq!(outcome, "failure:boom");
    }

    #[test]
    fn advance_turn_exhausted_when_round_limit_hit() {
        let mut conv = Conversation::new(SessionId::new("s"));
        let driver = FakeDriver::new(vec![RoundOutcome::Continue, RoundOutcome::Continue])
            .with_round_limit(2);
        let outcome = conv.advance_turn(driver).unwrap();
        assert_eq!(outcome, "exhausted");
    }

    #[test]
    fn advance_turn_deterministic_shortcut_skips_modeling() {
        let mut conv = Conversation::new(SessionId::new("s"));
        let driver = FakeDriver::new(vec![RoundOutcome::Done]).with_deterministic("instant");
        let outcome = conv.advance_turn(driver).unwrap();
        assert_eq!(outcome, "instant");
        // 槽位归零
        assert_eq!(conv.current_turn_state(), None);
    }

    #[test]
    fn advance_turn_rejects_when_active_turn_exists() {
        let mut conv = Conversation::new(SessionId::new("s"));
        conv.begin_turn().unwrap();
        let driver = FakeDriver::new(vec![RoundOutcome::Done]);
        let err = conv.advance_turn(driver).unwrap_err();
        match err {
            AdvanceTurnError::Begin(BeginTurnError::TurnAlreadyActive(state)) => {
                assert_eq!(state, TurnState::Pending);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn advance_turn_passes_mailbox_items_to_driver() {
        struct CapturingDriver {
            captured_len: usize,
        }

        impl crate::driver::TurnDriver for CapturingDriver {
            type Outcome = usize;

            fn round_limit(&self) -> usize {
                1
            }

            fn accept_mailbox_items(&mut self, items: Vec<MailboxItem>) {
                self.captured_len = items.len();
            }

            fn execute_round(&mut self, _round: usize) -> RoundOutcome {
                RoundOutcome::Done
            }

            fn finalize_success(self) -> Self::Outcome {
                self.captured_len
            }

            fn finalize_round_failure(self, _reason: String) -> Self::Outcome {
                self.captured_len
            }

            fn finalize_exhausted(self) -> Self::Outcome {
                self.captured_len
            }
        }

        let mut conv = Conversation::new(SessionId::new("s-mailbox"));
        conv.ingest_user_signal(sample_signal("hello"));
        conv.ingest_runtime_signal(RuntimeSignal {
            author: crate::mailbox::MailboxAuthor::System,
            kind: crate::mailbox::MailboxKind::Followup,
            trigger_turn: true,
            payload: serde_json::json!({"note": "wake"}),
            enqueued_at: UtcMillis(2),
        });

        let outcome = conv
            .advance_turn(CapturingDriver { captured_len: 0 })
            .unwrap();
        assert_eq!(outcome, 2);
        assert!(conv.drain_mailbox_items().is_empty());
    }
}
