//! 网页原生选择卡片：页面拥有控件与步骤，daemon 只等待并转交用户回答。
use std::sync::{Arc, atomic::AtomicBool};

use serde::{Deserialize, Serialize};

use crate::{DriverFuture, WebModelIdentity};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebQuestionItem {
    pub question: String,
    pub options: Vec<WebQuestionOption>,
    pub multi_select: bool,
    pub allows_text: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebQuestion {
    pub id: String,
    pub questions: Vec<WebQuestionItem>,
    pub can_skip: bool,
}

pub use magi_browser_authority::{
    BrowserWebQuestionAnswer as WebQuestionAnswer,
    BrowserWebQuestionSelection as WebQuestionSelection,
};

pub trait WebQuestionSink: Send + Sync {
    fn ask<'a>(
        &'a self,
        identity: &'a WebModelIdentity,
        question: &'a WebQuestion,
        cancelled: Arc<AtomicBool>,
    ) -> DriverFuture<'a, WebQuestionAnswer>;
}
