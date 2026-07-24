//! 任务系统 bridges: routes 与 magi-conversation-runtime 的通道。
//!
//! 该 helper 是 routes 层读取 user 信号字段与切换 Turn 生命周期的**唯一来源**：
//! routes 不再直接调 `SessionTurnRequestDto::trimmed_text` 等方法来驱动业务路径。
//!
//! S1：独立 Turn 的 user-input 入口经 `ingest_user_input_to_conversation` 推 Mailbox + drain；
//! 当前活跃 Turn 的引导由 ConversationRegistry 的 turn-id 绑定通道处理。
//! S2：执行路径外层经 `begin_session_turn` / `finalize_session_turn` 维护 Turn 生命周期。
//!
//! Turn 生命周期是 session 级 Conversation 的硬不变式：同一 Conversation 不允许
//! 并发 Turn。task/worker 执行使用 task 级 Conversation，不再占用 session 级槽位。

#[cfg(test)]
use magi_conversation_runtime::BeginTurnError;
use magi_conversation_runtime::{TurnAdvanceError, TurnState, UserSignal};
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

/// 在外层执行入口处开启 Turn。若发现已有未结束 Turn，调用方必须停止本次执行。
#[cfg(test)]
pub(super) fn begin_session_turn(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<(), BeginTurnError> {
    let conv = state.conversation_registry.conversation_for(session_id);
    let mut guard = conv.lock().expect("conversation turn lock poisoned");
    guard.begin_turn()
}

/// 把 Turn 推进到给定终态并回收槽位。当前实现接受"未开始即 finalize"的边界
/// 情况并记录 warning；新写入路径应先成功 begin 再 finalize。
pub(super) fn finalize_session_turn(
    state: &ApiState,
    session_id: &SessionId,
    success: bool,
) -> bool {
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
                return false;
            }
            true
        }
        Err(TurnAdvanceError::NoActiveTurn) => {
            tracing::debug!(
                %session_id,
                target = %target,
                "finalize_session_turn called without active turn"
            );
            false
        }
        Err(other) => {
            tracing::warn!(%session_id, ?other, target = %target, "finalize_session_turn transition failed");
            let _ = guard.end_turn();
            false
        }
    }
}
