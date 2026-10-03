//! GPT Web 运行时内部错误。错误码不进入 App Server wire schema。

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebModelErrorCode {
    WebSessionBusy,
    WebContextLost,
    WebSavedConversationUnavailable,
    WebSavedConversationConflict,
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

    pub const ALL: [WebModelErrorCode; 16] = [
        Self::WebSessionBusy,
        Self::WebContextLost,
        Self::WebSavedConversationUnavailable,
        Self::WebSavedConversationConflict,
        Self::WebLoginExpired,
        Self::WebSiteBlocked,
        Self::WebSelectorsDrift,
        Self::WebDesktopUnavailable,
        Self::WebSendRejected,
        Self::WebWriteNotConfirmed,
        Self::WebTurnTimeout,
        Self::WebQuotaExhausted,
        Self::WebTunnelUnavailable,
        Self::MagiToolTimeout,
        Self::MagiToolDenied,
        Self::MagiToolUnavailable,
    ];

    /// 在一段错误文本里找出以 `code:` 形式出现的 GPT Web 错误码（桥接层会把码放在消息前缀）。
    pub fn find_in(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| text.contains(&format!("{}:", candidate.code())))
    }

    /// 对用户展示的固定说明（不含内部细节）；主行动由前端按错误码映射。
    pub const fn public_message(self) -> &'static str {
        match self {
            Self::WebSessionBusy => {
                "GPT Web 正被其他会话占用，请先在占用会话里停止，或让它切换到本地模型。"
            }
            Self::WebContextLost => {
                "网页对话已失效（页面被重新加载或 Magi 重启过）。可以切换本地模型继续，Magi 里的历史完整；也可以新建 GPT Web 会话。"
            }
            Self::WebSavedConversationUnavailable => {
                "已保存对话在 ChatGPT 侧不存在或账号不匹配，无法继续。"
            }
            Self::WebSavedConversationConflict => {
                "已保存对话在网页侧发生了无法安全映射的改动，请在 ChatGPT 中处理后重新同步。"
            }
            Self::WebLoginExpired => "ChatGPT 未登录或登录已过期，请先登录。",
            Self::WebSiteBlocked => {
                "ChatGPT 页面处于风险验证或被拦截，请在 GPT Web 页面里处理后再试。"
            }
            Self::WebSelectorsDrift => {
                "ChatGPT 页面结构可能已改版，Magi 无法安全操作，请重新检查。"
            }
            Self::WebDesktopUnavailable => "GPT Web 需要 Magi Desktop。",
            Self::WebSendRejected => "ChatGPT 未接受这条消息，本次没有产生任何副作用，可以重试。",
            Self::WebWriteNotConfirmed => "网页输入框回读未确认，本次消息未提交，可以重试。",
            Self::WebTurnTimeout => "消息已被 ChatGPT 接受，但回复没有在时限内完成。",
            Self::WebQuotaExhausted => "ChatGPT 账号额度已用尽，可以切换本地模型继续。",
            Self::WebTunnelUnavailable => "项目工具通道不可用，GPT Web 只能纯对话。",
            Self::MagiToolTimeout => "Magi 工具调用在等待窗口内未完成。",
            Self::MagiToolDenied => "Magi 工具调用被项目权限或用户审批拒绝。",
            Self::MagiToolUnavailable => "Magi 工具服务或通道不可用。",
        }
    }

    /// 用户可以直接重试且不会重复提交的情形。
    pub const fn user_retryable(self) -> bool {
        matches!(self, Self::WebSendRejected | Self::WebWriteNotConfirmed)
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelError {
    pub code: WebModelErrorCode,
    pub message: String,
    cancelled: bool,
}

impl WebModelError {
    pub fn new(code: WebModelErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cancelled: false,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            code: WebModelErrorCode::WebTurnTimeout,
            message: "调用已取消".into(),
            cancelled: true,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

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
pub enum EngineState {
    Available,
    ToolUnavailable,
    DesktopUnavailable,
    LoginRequired,
    SiteBlocked,
    QuotaExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_error_surface_has_no_legacy_text_protocol_codes() {
        assert_eq!(WebModelErrorCode::WebContextLost.code(), "web_context_lost");
        assert_eq!(WebModelErrorCode::MagiToolDenied.code(), "magi_tool_denied");
        assert_eq!(
            WebModelErrorCode::WebTunnelUnavailable.engine_state(),
            Some(EngineState::ToolUnavailable)
        );
    }
}
