//! Turn 业务服务。
//!
//! HTTP 和 Desktop App Server 只能在传输层做解析、鉴权和序列化，接纳与幂等
//! 必须经过同一个服务入口，避免两个协议维护不同的 Turn 生命周期。

use crate::{
    dto::{SessionTurnRequestDto, SessionTurnResponseDto},
    errors::ApiError,
    routes::sessions,
    state::ApiState,
};

#[derive(Clone)]
pub(crate) struct TurnService {
    state: ApiState,
}

impl TurnService {
    pub(crate) fn new(state: ApiState) -> Self {
        Self { state }
    }

    pub(crate) async fn submit(
        &self,
        request: SessionTurnRequestDto,
    ) -> Result<SessionTurnResponseDto, ApiError> {
        sessions::submit_session_turn_internal(self.state.clone(), request).await
    }
}
