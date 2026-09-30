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
        Self { role: "user".into(), text: text.into(), remote_id: None }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: "assistant".into(), text: text.into(), remote_id: None }
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
}

/// 唯一的页面驱动入口。实现不得在这些方法外维护第二套 DOM 控制链。
pub trait WebModelPageDriver: Send + Sync {
    fn open_temporary_chat<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, ()>;

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
    fn close_page<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, ()>;

    /// 只能把本轮 prompt 原样写入 composer；不支持追加、历史回放或分片。
    fn write_text<'a>(&'a self, page_id: &'a str, text: &'a str)
        -> DriverFuture<'a, WriteOutcome>;

    fn submit<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, SubmitOutcome>;

    fn cancel_generation<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn turn_state<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, TurnState>;

    /// 临时对话唯一的存活判定输入。返回 None 也必须被视为上下文不可验证。
    fn read_last_message<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, Option<WebMessage>> {
        Box::pin(async { Ok(None) })
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
}
