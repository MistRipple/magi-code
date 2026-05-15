//! Task System v2 - Turn 状态机（Slice S2）。
//!
//! Turn 表示一轮"user 信号 → assistant 完成（含工具调用循环）"的对话推进。
//! 状态转换契约：
//!
//! ```text
//!     Pending ──advance──► Modeling ──tool_request──► ToolCalling
//!                            │                            │
//!                            │ ◄───────── tool_done ──────┘
//!                            │
//!                            ▼
//!                          Done | Failed
//! ```
//!
//! v2 由 Conversation 持有"状态推进契约"，`Modeling` / `ToolCalling` 的 IO
//! 通过 `TurnDriver` 注入；状态机本身不依赖具体任务执行实现。

use std::fmt;

use serde::{Deserialize, Serialize};

/// Turn 终态前的所有合法状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    /// drain 后但尚未交给 LLM。
    Pending,
    /// 已发起 LLM 调用，等待生成结束（包含 streaming）。
    Modeling,
    /// LLM 给出 tool_call，正在执行工具批次。
    ToolCalling,
    /// 已成功完成（含 deterministic 短路）。
    Done,
    /// 因 LLM 错误、工具拒绝、lease 失效等失败终止。
    Failed,
}

impl TurnState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

impl fmt::Display for TurnState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Pending => "pending",
            Self::Modeling => "modeling",
            Self::ToolCalling => "tool_calling",
            Self::Done => "done",
            Self::Failed => "failed",
        };
        f.write_str(label)
    }
}

/// Turn 状态机违规转换错误。同一 Conversation 内不允许两个并发 Turn，
/// 也不允许从终态再推进。
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct TurnTransitionError {
    pub from: TurnState,
    pub to: TurnState,
}

impl fmt::Display for TurnTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "illegal turn transition: {} -> {}", self.from, self.to)
    }
}

impl std::error::Error for TurnTransitionError {}

/// 一次对话推进的状态承载。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    state: TurnState,
}

impl Turn {
    /// 创建一个 Pending Turn——drain 出 user signal 后即可构造。
    pub fn new() -> Self {
        Self {
            state: TurnState::Pending,
        }
    }

    pub fn state(&self) -> TurnState {
        self.state
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// Pending → Modeling。LLM 调用刚发起时调用。
    pub fn enter_modeling(&mut self) -> Result<(), TurnTransitionError> {
        self.transition(TurnState::Modeling, |from| {
            matches!(from, TurnState::Pending | TurnState::ToolCalling)
        })
    }

    /// Modeling → ToolCalling。LLM 返回 tool_call 后调用。
    pub fn enter_tool_calling(&mut self) -> Result<(), TurnTransitionError> {
        self.transition(TurnState::ToolCalling, |from| {
            matches!(from, TurnState::Modeling)
        })
    }

    /// Modeling → Done。LLM 完成 final assistant message（无后续 tool_call）。
    pub fn finish_done(&mut self) -> Result<(), TurnTransitionError> {
        self.transition(TurnState::Done, |from| {
            matches!(from, TurnState::Pending | TurnState::Modeling)
        })
    }

    /// 任意非终态 → Failed。任意阶段失败时调用。
    pub fn finish_failed(&mut self) -> Result<(), TurnTransitionError> {
        self.transition(TurnState::Failed, |from| !from.is_terminal())
    }

    fn transition<F>(&mut self, to: TurnState, allowed: F) -> Result<(), TurnTransitionError>
    where
        F: FnOnce(TurnState) -> bool,
    {
        if !allowed(self.state) {
            return Err(TurnTransitionError {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }
}

impl Default for Turn {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_no_tool_call() {
        let mut turn = Turn::new();
        assert_eq!(turn.state(), TurnState::Pending);
        turn.enter_modeling().unwrap();
        assert_eq!(turn.state(), TurnState::Modeling);
        turn.finish_done().unwrap();
        assert!(turn.is_terminal());
        assert_eq!(turn.state(), TurnState::Done);
    }

    #[test]
    fn tool_call_then_done() {
        let mut turn = Turn::new();
        turn.enter_modeling().unwrap();
        turn.enter_tool_calling().unwrap();
        // 工具返回后回到 modeling 走第二轮
        turn.enter_modeling().unwrap();
        turn.finish_done().unwrap();
    }

    #[test]
    fn cannot_advance_terminal_turn() {
        let mut turn = Turn::new();
        turn.enter_modeling().unwrap();
        turn.finish_done().unwrap();
        let err = turn.finish_failed().unwrap_err();
        assert_eq!(err.from, TurnState::Done);
    }

    #[test]
    fn cannot_skip_modeling_to_tool_calling() {
        let mut turn = Turn::new();
        let err = turn.enter_tool_calling().unwrap_err();
        assert_eq!(err.from, TurnState::Pending);
        assert_eq!(err.to, TurnState::ToolCalling);
    }

    #[test]
    fn failure_from_modeling() {
        let mut turn = Turn::new();
        turn.enter_modeling().unwrap();
        turn.finish_failed().unwrap();
        assert_eq!(turn.state(), TurnState::Failed);
    }

    #[test]
    fn pending_can_short_circuit_to_done() {
        // deterministic 短路场景：planning 文本直接产出 final
        let mut turn = Turn::new();
        turn.finish_done().unwrap();
        assert_eq!(turn.state(), TurnState::Done);
    }
}
