//! ChatGPT Web 页面驱动契约。
//!
//! DOM selector、保存对话列表和消息同步事实由 worker 实现；本 crate 只消费
//! 这些事实，不在 Rust 侧复制页面上下文。

use std::future::Future;
use std::pin::Pin;

use crate::errors::{WebModelError, WebModelErrorCode};

pub type DriverFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, WebModelError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub confirmed: bool,
    pub char_count: u64,
    pub became_attachment: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitOutcome {
    pub submitted: bool,
    pub composer_empty: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    SignedIn,
    SignedOut,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebMessage {
    pub role: String,
    pub text: String,
    pub remote_id: Option<String>,
}

impl WebMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            text: text.into(),
            remote_id: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            text: text.into(),
            remote_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedConversationSnapshot {
    pub conversation_id: String,
    pub title: Option<String>,
    pub messages: Vec<WebMessage>,
    pub last_message_id: Option<String>,
    pub remote_updated_at: Option<String>,
    pub exists: bool,
}

/// ChatGPT 侧已保存对话列表里的一项。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConversationEntry {
    pub conversation_id: String,
    pub title: String,
    pub updated_at: Option<String>,
}

/// ChatGPT 侧 Magi 连接器的只读状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorStatus {
    pub supported: bool,
    pub exists: bool,
    pub enabled: bool,
    pub tool_count: Option<u64>,
    pub reason: Option<String>,
}

/// 创建 / 启用连接器并回读后的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorConfigOutcome {
    pub configured: bool,
    pub confirmed_enabled: bool,
    pub reason: Option<String>,
}

/// 页面图片的一块字节（控制通道单条消息有上限，图片按块读取）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageChunk {
    pub mime: String,
    /// 整张图片的字节数。
    pub total: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnState {
    pub login_state: LoginState,
    pub blocked: bool,
    pub generating: bool,
    pub composer_found: bool,
    pub user_message_count: u64,
    pub assistant_message_count: u64,
    pub assistant_text: String,
    pub thinking_text: String,
    pub last_message_role: Option<String>,
    pub last_message_text: Option<String>,
}

/// 唯一的页面驱动入口。实现不得在这些方法外维护第二套 DOM 控制链。
pub trait WebModelPageDriver: Send + Sync {
    fn open_temporary_chat<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, ()>;

    /// 分块读取页面上一张图片的字节（`source` 是页面上某个 `<img>` 的地址，如 `blob:`）。
    fn read_image_chunk<'a>(
        &'a self,
        page_id: &'a str,
        source: &'a str,
        offset: u64,
        length: u64,
    ) -> DriverFuture<'a, ImageChunk> {
        let _ = (page_id, source, offset, length);
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "页面驱动未提供读取图片的能力",
            ))
        })
    }

    /// 重新加载唯一 WebView 当前所在的页面（页面卡住 / 加载失败时的手动刷新）。
    /// 页面地址不可恢复（空白页、浏览器内部错误页）时回到 ChatGPT 主页。不改变槽位和对话。
    fn reload_page<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, ()> {
        let _ = page_id;
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "页面驱动未提供页面刷新能力",
            ))
        })
    }

    /// 把唯一的 WebView 导航到一个固定的页面（OpenAI 平台 Tunnels / 密钥页）。不等待对话页就绪。
    fn open_page<'a>(&'a self, page_id: &'a str, url: &'a str) -> DriverFuture<'a, ()> {
        let _ = (page_id, url);
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "页面驱动未提供页面导航能力",
            ))
        })
    }

    /// `conversation_id=None` 表示创建新的已保存对话，返回的远端 id 由
    /// `read_saved_conversation` 提供；普通继续发送必须传稳定 id。
    fn open_saved_chat<'a>(
        &'a self,
        page_id: &'a str,
        conversation_id: Option<&'a str>,
    ) -> DriverFuture<'a, ()> {
        let _ = (page_id, conversation_id);
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "页面驱动未提供已保存 GPT Web 对话能力",
            ))
        })
    }

    fn resume_page<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, bool>;

    /// 让 Authority 与 Desktop 页面的 Surface 绑定重新对齐（daemon 重启后两边的导航代次可能不一致）。
    /// 只重新登记当前页面，不导航、不改变页面内容。
    fn refresh_surface<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close_page<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, ()>;

    /// 只能把本轮 prompt 原样写入 composer；不支持追加、历史回放或分片。
    fn write_text<'a>(&'a self, page_id: &'a str, text: &'a str) -> DriverFuture<'a, WriteOutcome>;

    fn submit<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, SubmitOutcome>;

    fn cancel_generation<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn turn_state<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, TurnState>;

    /// 临时对话唯一的存活判定输入。返回 None 也必须被视为上下文不可验证。
    fn read_last_message<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, Option<WebMessage>> {
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebContextLost,
                "页面最后一条消息不可验证",
            ))
        })
    }

    /// 已保存对话的远端事实；worker 负责读取标题、消息 id 和增量消息。
    fn read_saved_conversation<'a>(
        &'a self,
        _page_id: &'a str,
        _conversation_id: &'a str,
    ) -> DriverFuture<'a, SavedConversationSnapshot> {
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "页面驱动未提供已保存对话同步能力",
            ))
        })
    }

    /// 已保存对话列表（侧栏历史）。只读。
    fn list_saved_conversations<'a>(
        &'a self,
        _page_id: &'a str,
    ) -> DriverFuture<'a, Vec<SavedConversationEntry>> {
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "页面驱动未提供已保存对话列表能力",
            ))
        })
    }

    /// 只读检查 ChatGPT 侧的 Magi 连接器。
    fn connector_status<'a>(
        &'a self,
        _page_id: &'a str,
        _name: &'a str,
    ) -> DriverFuture<'a, ConnectorStatus> {
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebTunnelUnavailable,
                "页面驱动未提供连接器检查能力",
            ))
        })
    }

    /// 创建 / 启用 Magi 连接器并回读确认。只写连接器设置。
    fn configure_connector<'a>(
        &'a self,
        _page_id: &'a str,
        _name: &'a str,
        _tunnel_id: &'a str,
    ) -> DriverFuture<'a, ConnectorConfigOutcome> {
        Box::pin(async {
            Err(WebModelError::new(
                WebModelErrorCode::WebTunnelUnavailable,
                "页面驱动未提供连接器配置能力",
            ))
        })
    }
}

/// 已保存对话在一次 turn 完成后的远端事实，由 client 回报给绑定的唯一写入方。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedProgress {
    pub remote_conversation_id: String,
    pub remote_title: Option<String>,
    pub last_remote_message_id: Option<String>,
    pub remote_updated_at: Option<String>,
}

/// 会话级 Web 绑定的写入方（daemon 侧实现）。client 不直接持久化任何状态。
pub trait SavedConversationSink: Send + Sync {
    fn record(&self, session_id: &str, progress: SavedProgress);
    /// 完成后的远端事实读取失败：同步指针不可信，下次发送前必须重新对齐。
    fn mark_stale(&self, session_id: &str);
    /// 本会话的一条消息已被 ChatGPT 接受：从这一刻起网页里才存在属于该会话的上下文。
    /// 之前失败的会话（没有任何消息进入网页）不是“网页对话已失效”，可以直接重试。
    fn message_accepted(&self, _session_id: &str) {}
    /// 一次发送以会改变引擎可用性的错误失败（登录过期 / 站点被拦 / 需要桌面 / 额度用尽）：
    /// 由 daemon 更新唯一的可用性投影，选择器随之隐藏入口。
    fn engine_state_changed(&self, _state: crate::errors::EngineState) {}
}
