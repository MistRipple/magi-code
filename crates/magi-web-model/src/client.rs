//! GPT Web 的最小桥接 client。
//!
//! ChatGPT Web 自己拥有上下文。本模块只把本轮 `prompt` 写进唯一页面的
//! composer，再把同一页产生的回复读回 Magi；绝不把 `messages`、工具清单或
//! 压缩结果拼进网页消息。

use std::sync::Arc;
use std::time::Duration;

use magi_bridge_client::{
    ModelBridgeClient, ModelInvocationRequest, ModelResponse, ModelResponseStatus,
    ModelRetryRuntimeEvent, ModelStreamingDelta,
};

use crate::binding::{WebConversationMode, WebSlotOwner, WebSlotTable};
use crate::driver::{LoginState, WebMessage, WebModelPageDriver};
use crate::errors::{WebModelError, WebModelErrorCode};
use crate::runtime::WebModelRuntimeRegistry;

pub const WEB_MODEL_PROVIDER: &str = "chatgpt_web";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelIdentity {
    pub session_id: String,
    pub project_id: String,
    pub thread_id: String,
    pub engine_id: String,
    pub effort: String,
}

#[derive(Clone, Debug)]
pub struct WebModelClientConfig {
    pub mode: WebConversationMode,
    pub remote_conversation_id: Option<String>,
    pub poll_interval: Duration,
    pub acceptance_timeout: Duration,
    pub completion_timeout: Duration,
}

impl Default for WebModelClientConfig {
    fn default() -> Self {
        Self {
            mode: WebConversationMode::Temporary,
            remote_conversation_id: None,
            poll_interval: Duration::from_millis(250),
            acceptance_timeout: Duration::from_secs(30),
            completion_timeout: Duration::from_secs(300),
        }
    }
}

/// 统一 composer 回读归一化。只处理浏览器编辑器的换行 / 空格序列化差异，
/// 不改变普通字符。
pub fn normalize_composer_text(text: &str) -> String {
    let normalized = text
        .chars()
        .map(|character| match character {
            '\u{00a0}'
            | '\u{1680}'
            | '\u{180e}'
            | '\u{2000}'..='\u{200a}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut output = String::with_capacity(normalized.len());
    let mut newlines = 0usize;
    for character in normalized.chars() {
        if character == '\n' {
            newlines += 1;
            if newlines <= 2 {
                output.push('\n');
            }
        } else {
            newlines = 0;
            output.push(character);
        }
    }
    while output.ends_with('\n') {
        output.pop();
    }
    output
}

pub struct BrowserWebModelBridgeClient {
    driver: Arc<dyn WebModelPageDriver>,
    slots: Arc<WebSlotTable>,
    owner: WebSlotOwner,
    identity: WebModelIdentity,
    config: WebModelClientConfig,
    runtime: Arc<WebModelRuntimeRegistry>,
    page_id: String,
}

impl BrowserWebModelBridgeClient {
    pub fn new(
        driver: Arc<dyn WebModelPageDriver>,
        slots: Arc<WebSlotTable>,
        owner: WebSlotOwner,
        identity: WebModelIdentity,
        config: WebModelClientConfig,
    ) -> Self {
        Self {
            driver,
            slots,
            owner,
            identity,
            config,
            runtime: Arc::new(WebModelRuntimeRegistry::new()),
            page_id: "browser-tab-web-model-home".to_string(),
        }
    }

    pub fn with_runtime(mut self, runtime: Arc<WebModelRuntimeRegistry>) -> Self {
        self.runtime = runtime;
        self
    }

    /// 保留 API 形状，但工具不经模型文本协议处理；工具由独立 MCP Gateway
    /// 连接器处理。
    pub fn with_harness(self, _harness: Arc<crate::harness::HarnessRegistry>) -> Self {
        self
    }

    pub fn runtime(&self) -> &Arc<WebModelRuntimeRegistry> {
        &self.runtime
    }

    pub fn slots(&self) -> &Arc<WebSlotTable> {
        &self.slots
    }

    pub fn identity(&self) -> &WebModelIdentity {
        &self.identity
    }

    pub fn release_slot(&self) -> bool {
        self.slots.release(&self.owner)
    }

    fn bridge_error(error: WebModelError) -> magi_bridge_client::BridgeClientError {
        error.into_bridge_error()
    }

    fn prior_message(request: &ModelInvocationRequest) -> Option<WebMessage> {
        let messages = request.messages.as_deref().unwrap_or_default();
        let current_index = messages
            .iter()
            .rposition(|message| message.role == "user" && message.content.as_deref() == Some(request.prompt.as_str()))
            .or_else(|| messages.iter().rposition(|message| message.role == "user"));
        let Some(index) = current_index else {
            return messages.last().map(|message| WebMessage {
                role: message.role.clone(),
                text: message.content.clone().unwrap_or_default(),
                remote_id: message.tool_call_id.clone(),
            });
        };
        messages[..index].iter().rev().find_map(|message| {
            let role = match message.role.as_str() {
                "user" | "assistant" => message.role.clone(),
                _ => return None,
            };
            Some(WebMessage {
                role,
                text: message.content.clone().unwrap_or_default(),
                remote_id: message.tool_call_id.clone(),
            })
        })
    }

    fn same_message(expected: Option<&WebMessage>, observed: Option<&WebMessage>) -> bool {
        match (expected, observed) {
            (None, None) => true,
            (None, Some(observed)) => observed.text.trim().is_empty(),
            (Some(expected), Some(observed)) => {
                expected.role == observed.role
                    && normalize_composer_text(&expected.text)
                        == normalize_composer_text(&observed.text)
            }
            _ => false,
        }
    }

    async fn open_page(&self, newly_claimed: bool) -> Result<(), WebModelError> {
        if !newly_claimed {
            if !self.driver.resume_page(&self.page_id).await? {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebContextLost,
                    "GPT Web 页面已重新加载或被销毁，当前 Web 对话无法恢复",
                ));
            }
            return Ok(());
        }
        match self.config.mode {
            WebConversationMode::Temporary => {
                self.driver.open_temporary_chat(&self.page_id).await
            }
            WebConversationMode::Saved => {
                self.driver
                    .open_saved_chat(
                        &self.page_id,
                        self.config.remote_conversation_id.as_deref(),
                    )
                    .await
            }
        }
    }

    async fn execute(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, WebModelError> {
        if request.prompt.is_empty() {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSendRejected,
                "GPT Web 本轮消息不能为空",
            ));
        }
        let binding = crate::binding::WebConversationBinding {
            mode: self.config.mode,
            engine_id: self.identity.engine_id.clone(),
            remote_conversation_id: self.config.remote_conversation_id.clone(),
            remote_title: None,
            last_synced_remote_message_id: None,
            remote_updated_at: None,
            sync_state: match self.config.mode {
                WebConversationMode::Temporary => crate::binding::WebConversationSyncState::Unbound,
                WebConversationMode::Saved => crate::binding::WebConversationSyncState::Active,
            },
        };
        let claim = self
            .slots
            .claim(self.owner.clone(), binding, self.page_id.clone())?;
        let lease = self.slots.begin_turn(&self.owner)?;
        self.runtime.mark_web_turn(
            &self.identity.session_id,
            &self.identity.thread_id,
            &self.identity.engine_id,
            &self.page_id,
        );

        let result = async {
            self.open_page(claim.newly_claimed).await?;
            let prior = Self::prior_message(&request);
            // 已保存对话首次绑定时先由 Web 读取远端事实；临时对话则必须是空页。
            if !claim.newly_claimed || self.config.mode == WebConversationMode::Temporary {
                let observed = self.driver.read_last_message(&self.page_id).await?;
                if !Self::same_message(prior.as_ref(), observed.as_ref()) {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebContextLost,
                        "网页最后一条消息与 Magi 会话不一致，未覆盖网页内容",
                    ));
                }
            }
            if is_cancelled() {
                return Err(WebModelError::cancelled());
            }
            let baseline = self.driver.turn_state(&self.page_id).await?;
            match baseline.login_state {
                LoginState::SignedIn => {}
                LoginState::Blocked => {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebSiteBlocked,
                        "ChatGPT Web 当前处于风险或验证页面",
                    ));
                }
                LoginState::SignedOut => {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebLoginExpired,
                        "ChatGPT Web 尚未登录或登录已过期",
                    ));
                }
            }
            let write = self.driver.write_text(&self.page_id, &request.prompt).await?;
            if !write.confirmed || write.became_attachment {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebWriteNotConfirmed,
                    "网页输入框回读未确认，本次消息未提交",
                ));
            }
            let submitted = self.driver.submit(&self.page_id).await?;
            if !submitted.submitted || !submitted.composer_empty {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSendRejected,
                    submitted.reason.unwrap_or_else(|| "ChatGPT Web 未接受本次消息".to_string()),
                ));
            }

            let acceptance_deadline = tokio::time::Instant::now() + self.config.acceptance_timeout;
            let mut last_assistant = String::new();
            let mut last_thinking = String::new();
            let mut stable_reads = 0u8;
            let mut emitted_assistant = 0usize;
            let mut emitted_thinking = 0usize;
            let mut accepted = false;
            let completion_deadline = tokio::time::Instant::now() + self.config.completion_timeout;
            loop {
                if is_cancelled() {
                    let _ = self.driver.cancel_generation(&self.page_id).await;
                    return Err(WebModelError::cancelled());
                }
                let state = self.driver.turn_state(&self.page_id).await?;
                if state.user_message_count > baseline.user_message_count {
                    accepted = true;
                }
                if !accepted && tokio::time::Instant::now() >= acceptance_deadline {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebSendRejected,
                        "ChatGPT Web 未确认已接受本次消息",
                    ));
                }
                if state.assistant_text != last_assistant || state.thinking_text != last_thinking {
                    if state.assistant_text.len() >= emitted_assistant {
                        on_delta(&ModelStreamingDelta {
                            content: state.assistant_text[emitted_assistant..].to_string(),
                            thinking: state.thinking_text[emitted_thinking..].to_string(),
                            tool_calls: Vec::new(),
                        });
                        emitted_assistant = state.assistant_text.len();
                        emitted_thinking = state.thinking_text.len();
                    }
                    last_assistant = state.assistant_text.clone();
                    last_thinking = state.thinking_text.clone();
                    stable_reads = 0;
                } else if accepted && !state.generating {
                    stable_reads = stable_reads.saturating_add(1);
                } else {
                    stable_reads = 0;
                }
                if accepted && !state.generating && stable_reads >= 2 {
                    break;
                }
                if tokio::time::Instant::now() >= completion_deadline {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebTurnTimeout,
                        "ChatGPT Web 回复在时限内未完成",
                    ));
                }
                tokio::time::sleep(self.config.poll_interval).await;
            }
            self.runtime.record_web_send(&self.identity.session_id);
            Ok(ModelResponse {
                status: ModelResponseStatus::Completed,
                content: Some(last_assistant),
                thinking: (!last_thinking.trim().is_empty()).then_some(last_thinking),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".to_string()),
                provider_context: Vec::new(),
            })
        }
        .await;
        drop(lease);
        self.runtime.finish_web_turn(&self.identity.session_id);
        result
    }
}

impl ModelBridgeClient for BrowserWebModelBridgeClient {
    fn invoke(
        &self,
        _request: ModelInvocationRequest,
    ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
        Err(Self::bridge_error(WebModelError::new(
            WebModelErrorCode::WebSendRejected,
            "GPT Web 只支持流式调用",
        )))
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
        self.invoke_streaming_with_cancellation(request, on_delta, &|_| {}, &|| false)
    }

    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
        self.invoke_streaming(request, on_delta)
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, magi_bridge_client::BridgeClientError> {
        let driver = Arc::clone(&self.driver);
        let _ = driver;
        let client = self;
        let result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| handle.block_on(client.execute(request, on_delta, is_cancelled)))
        } else {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("web model runtime")
                .block_on(client.execute(request, on_delta, is_cancelled))
        };
        result.map_err(Self::bridge_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_client_does_not_rewrite_context() {
        let request = ModelInvocationRequest {
            provider: WEB_MODEL_PROVIDER.to_string(),
            prompt: "本轮问题".to_string(),
            messages: Some(vec![magi_bridge_client::ChatMessage {
                role: "assistant".to_string(),
                content: Some("历史回答".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            }]),
            tools: Some(Vec::new()),
            tool_choice: None,
        };
        assert_eq!(BrowserWebModelBridgeClient::prior_message(&request), Some(WebMessage::assistant("历史回答")));
    }
}
