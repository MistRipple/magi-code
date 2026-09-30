//! GPT Web 运行时内部错误。错误码不进入 App Server wire schema。

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebModelErrorCode {
    WebSessionBusy,
    WebContextLost,
    WebSavedConversationUnavailable,
    WebSavedConversationConflict,
    WebSavedDeleteFailed,
    WebLoginExpired,
    WebSiteBlocked,
    WebSelectorsDrift,
    WebDesktopUnavailable,
    WebSendRejected,
    WebWriteNotConfirmed,
    WebTurnTimeout,
    WebQuotaExhausted,
    WebTunnelUnavailable,
    MagiToolTimeout,
    MagiToolDenied,
    MagiToolUnavailable,
}

impl WebModelErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::WebSessionBusy => "web_session_busy",
            Self::WebContextLost => "web_context_lost",
            Self::WebSavedConversationUnavailable => "web_saved_conversation_unavailable",
            Self::WebSavedConversationConflict => "web_saved_conversation_conflict",
            Self::WebSavedDeleteFailed => "web_saved_delete_failed",
            Self::WebLoginExpired => "web_login_expired",
            Self::WebSiteBlocked => "web_site_blocked",
            Self::WebSelectorsDrift => "web_selectors_drift",
            Self::WebDesktopUnavailable => "web_desktop_unavailable",
            Self::WebSendRejected => "web_send_rejected",
            Self::WebWriteNotConfirmed => "web_write_not_confirmed",
            Self::WebTurnTimeout => "web_turn_timeout",
            Self::WebQuotaExhausted => "web_quota_exhausted",
            Self::WebTunnelUnavailable => "web_tunnel_unavailable",
            Self::MagiToolTimeout => "magi_tool_timeout",
            Self::MagiToolDenied => "magi_tool_denied",
            Self::MagiToolUnavailable => "magi_tool_unavailable",
        }
    }

    pub const fn fails_turn(self) -> bool {
        !matches!(self, Self::MagiToolTimeout)
    }

    pub const fn engine_state(self) -> Option<EngineState> {
        match self {
            Self::WebLoginExpired => Some(EngineState::LoginRequired),
            Self::WebSiteBlocked | Self::WebSelectorsDrift => Some(EngineState::SiteBlocked),
            Self::WebDesktopUnavailable => Some(EngineState::DesktopUnavailable),
            Self::WebQuotaExhausted => Some(EngineState::QuotaExhausted),
            Self::WebTunnelUnavailable => Some(EngineState::ToolUnavailable),
            _ => None,
        }
    }

    pub const fn primary_action(self) -> Option<WebModelAction> {
        match self {
            Self::WebSessionBusy => Some(WebModelAction::ReleaseSession),
            Self::WebContextLost | Self::WebSavedConversationUnavailable => {
                Some(WebModelAction::SwitchModel)
            }
            Self::WebSavedConversationConflict => Some(WebModelAction::OpenHome),
            Self::WebSavedDeleteFailed
            | Self::WebSendRejected
            | Self::WebWriteNotConfirmed
            | Self::WebTurnTimeout => Some(WebModelAction::Retry),
            Self::WebLoginExpired => Some(WebModelAction::Login),
            Self::WebSiteBlocked | Self::WebSelectorsDrift => Some(WebModelAction::OpenHome),
            Self::WebDesktopUnavailable => Some(WebModelAction::LearnMore),
            Self::WebQuotaExhausted => Some(WebModelAction::SwitchModel),
            Self::WebTunnelUnavailable => Some(WebModelAction::ConfigureTunnel),
            Self::MagiToolTimeout | Self::MagiToolDenied | Self::MagiToolUnavailable => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelError {
    pub code: WebModelErrorCode,
    pub message: String,
    cancelled: bool,
}

impl WebModelError {
    pub fn new(code: WebModelErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), cancelled: false }
    }

    pub fn cancelled() -> Self {
        Self { code: WebModelErrorCode::WebTurnTimeout, message: "调用已取消".into(), cancelled: true }
    }

    pub fn is_cancelled(&self) -> bool { self.cancelled }

    pub fn into_bridge_error(self) -> magi_bridge_client::BridgeClientError {
        if self.cancelled {
            return magi_bridge_client::model_invocation_cancelled_error();
        }
        magi_bridge_client::BridgeClientError::CallFailed {
            layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
            code: None,
            message: format!("{}: {}", self.code.code(), self.message),
        }
    }
}

impl std::fmt::Display for WebModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.code(), self.message)
    }
}

impl std::error::Error for WebModelError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebModelAction {
    Retry,
    Login,
    OpenHome,
    SwitchModel,
    ReleaseSession,
    ConfigureTunnel,
    LearnMore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Available,
    ToolUnavailable,
    DesktopUnavailable,
    LoginRequired,
    SiteBlocked,
    QuotaExhausted,
}

impl EngineState {
    pub const fn visible_in_picker(self) -> bool {
        matches!(self, Self::Available | Self::ToolUnavailable)
    }

    pub const fn can_send(self) -> bool {
        matches!(self, Self::Available | Self::ToolUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_error_surface_has_no_legacy_text_protocol_codes() {
        assert_eq!(WebModelErrorCode::WebContextLost.code(), "web_context_lost");
        assert_eq!(WebModelErrorCode::MagiToolDenied.code(), "magi_tool_denied");
        assert_eq!(WebModelErrorCode::WebTunnelUnavailable.engine_state(), Some(EngineState::ToolUnavailable));
    }
}
