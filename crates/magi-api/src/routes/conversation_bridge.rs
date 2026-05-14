//! Task System v2 - Slice S1 桥接：routes 与 magi-conversation-runtime 的 Mailbox 通道。
//!
//! 4 个 user-input 入口（chat/execute、task、continue、supplement_context）统一通过
//! [`ingest_user_input_to_conversation`] 把 user 信号写入 Conversation 的 Mailbox，
//! 然后立刻 drain 拿回 [`UserSignal`] 用于后续编排。
//!
//! S1 中 Mailbox 的 push-then-drain 紧邻发生——这是建立通道边界的第一步；
//! S2 引入 Turn 状态机后，drain 将被 Turn 边界主循环接管。
//!
//! 该 helper 是 routes 层读取 user 信号字段（text / request_id / user_message_id /
//! placeholder_message_id）的**唯一来源**：routes 不再直接调
//! `SessionTurnRequestDto::trimmed_text` 等方法来驱动业务路径。

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
