//! Task System v2 bridges: routes 与 magi-conversation-runtime 的通道。
//!
//! 该 helper 是 routes 层读取 user 信号字段与切换 Turn 生命周期的**唯一来源**：
//! routes 不再直接调 `SessionTurnRequestDto::trimmed_text` 等方法来驱动业务路径。
//!
//! S1：4 个 user-input 入口经 `ingest_user_input_to_conversation` 推 Mailbox + drain。
//! S2：执行路径外层经 `begin_session_turn` / `finalize_session_turn` 维护 Turn 生命周期。
//!
//! Turn 生命周期当前仍是"软不变式"——深度任务 worker 派发仍可能并发触发子路径，
//! 出现 `TurnAlreadyActive` 时降级为警告而非中断，避免误伤现有并发路径。

use magi_conversation_runtime::{BeginTurnError, TurnAdvanceError, TurnState, UserSignal};
use magi_core::{SessionId, UtcMillis};

use crate::dto::SessionTurnRequestDto;
use crate::state::ApiState;

/// 把 SessionTurnRequestDto 中 user 信号本体（text + 元数据）注入对应 Conversation 的
/// Mailbox 并立刻 drain，返回 [`UserSignal`] 给 routes 用于编排。
pub(super) fn ingest_user_input_to_conversation(
    state: &ApiState,
    session_id: &SessionId,
    request: &SessionTurnRequestDto,
    accepted_at: UtcMillis,
) -> UserSignal {
    let signal = UserSignal {
        text: request.trimmed_text(),
        request_id: request.request_id(),
        user_message_id: request.user_message_id(),
        placeholder_message_id: request.placeholder_message_id(),
        accepted_at,
    };
    let conv = state.conversation_registry.conversation_for(session_id);
    let mut guard = conv.lock().expect("conversation mailbox lock poisoned");
    guard.ingest_user_signal(signal);
    let drained = guard.drain_user_signals();
    drained
        .into_iter()
        .next()
        .expect("user signal just ingested but drain returned empty")
}

/// 在外层执行入口处尝试开启 Turn。若发现已有未结束 Turn（worker 并发等场景），
/// 当前以 warning 记录但不阻断——S6 引入子 Conversation 后回硬为强制错误。
pub(super) fn begin_session_turn(state: &ApiState, session_id: &SessionId) {
    let conv = state.conversation_registry.conversation_for(session_id);
    let mut guard = conv.lock().expect("conversation turn lock poisoned");
    if let Err(BeginTurnError::TurnAlreadyActive(active)) = guard.begin_turn() {
        tracing::warn!(
            %session_id,
            ?active,
            "begin_session_turn observed concurrent active turn (will be hardened in S6)"
        );
    }
}

/// 把 Turn 推进到给定终态并回收槽位。当前实现接受"未开始即 finalize"的边界
/// 情况（warning），同样是为兼容尚未完整接入的 worker 派发路径。
pub(super) fn finalize_session_turn(state: &ApiState, session_id: &SessionId, success: bool) {
    let conv = state.conversation_registry.conversation_for(session_id);
    let mut guard = conv.lock().expect("conversation turn lock poisoned");
    let target = if success {
        TurnState::Done
    } else {
        TurnState::Failed
    };
    let transition = guard.advance_current_turn(|turn| {
        if success {
            turn.finish_done()
        } else {
            turn.finish_failed()
        }
    });
    match transition {
        Ok(()) => {
            if let Err(err) = guard.end_turn() {
                tracing::warn!(%session_id, ?err, target = %target, "end_turn after finalize failed");
            }
        }
        Err(TurnAdvanceError::NoActiveTurn) => {
            tracing::warn!(
                %session_id,
                target = %target,
                "finalize_session_turn called without active turn (will be hardened in S6)"
            );
        }
        Err(other) => {
            tracing::warn!(%session_id, ?other, target = %target, "finalize_session_turn transition failed");
            let _ = guard.end_turn();
        }
    }
}
