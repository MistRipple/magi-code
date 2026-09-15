//! 任务系统 bridges: routes 与 magi-conversation-runtime 的通道。
//!
//! 该 helper 是 routes 层读取 user 信号字段与切换 Turn 生命周期的**唯一来源**：
//! routes 不再直接调 `SessionTurnRequestDto::trimmed_text` 等方法来驱动业务路径。
//!
//! S1：独立 Turn 的 user-input 入口经 `ingest_user_input_to_conversation` 推 Mailbox + drain；
//! 当前活跃 Turn 的引导由 ConversationRegistry 的 turn-id 绑定通道处理。
//! S2：执行路径的 Turn 生命周期由 SessionTurnCoordinator 与 SessionStore 共同维护。
//!
//! Turn 生命周期是 session 级 Conversation 的硬不变式：同一 Conversation 不允许
//! 并发 Turn。task/worker 执行使用 task 级 Conversation，不再占用 session 级槽位。

use magi_conversation_runtime::UserSignal;
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

/// 测试辅助：历史 Conversation 状态机已不再拥有 Session Turn 生命周期；真实入口
/// 由 SessionTurnCoordinator 接纳 Turn。保留此无副作用 helper 仅让旧协议测试继续
/// 覆盖中断路由，而不会建立第二个生命周期所有者。
#[cfg(test)]
pub(super) fn begin_session_turn(_state: &ApiState, _session_id: &SessionId) -> Result<(), ()> {
    Ok(())
}
