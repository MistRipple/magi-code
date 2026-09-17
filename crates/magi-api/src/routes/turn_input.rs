//! Session Turn 输入字段的窄适配层。
//!
//! 普通 Session Turn 直接由 TurnService/Coordinator 接纳；Task worker 才拥有
//! `Conversation` 实例。本模块只负责把协议 DTO 映射为执行链需要的 `UserSignal`，
//! 不创建 Conversation，也不持有 Turn 生命周期。

use magi_conversation_runtime::UserSignal;
use magi_core::UtcMillis;

use crate::dto::SessionTurnRequestDto;

/// 把 `SessionTurnRequestDto` 中的用户输入字段转换成 [`UserSignal`]。
pub(super) fn user_signal_from_request(
    request: &SessionTurnRequestDto,
    accepted_at: UtcMillis,
) -> UserSignal {
    UserSignal {
        text: request.trimmed_text(),
        request_id: request.request_id(),
        user_message_id: request.user_message_id(),
        placeholder_message_id: request.placeholder_message_id(),
        accepted_at,
    }
}
