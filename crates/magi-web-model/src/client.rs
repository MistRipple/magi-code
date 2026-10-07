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
use crate::driver::{
    LoginState, SavedConversationSink, SavedProgress, WebMessage, WebModelPageDriver,
};
use crate::errors::{WebModelError, WebModelErrorCode};
use crate::runtime::WebModelRuntimeRegistry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebModelIdentity {
    pub session_id: String,
    pub project_id: String,
    pub thread_id: String,
    pub engine_id: String,
}

#[derive(Clone, Debug)]
pub struct WebModelClientConfig {
    pub mode: WebConversationMode,
    pub remote_conversation_id: Option<String>,
    /// 临时对话在网页里已经有过上下文（曾有消息被 ChatGPT 接受）。槽位不在（重启 / 停止 / 页面销毁）
    /// 却还要继续时，上下文已经不可恢复：直接判失效，不去碰页面、不新开一条冒充续接。
    pub context_established: bool,
    pub poll_interval: Duration,
    pub acceptance_timeout: Duration,
    pub completion_timeout: Duration,
}

impl Default for WebModelClientConfig {
    fn default() -> Self {
        Self {
            mode: WebConversationMode::Temporary,
            remote_conversation_id: None,
            context_established: false,
            poll_interval: Duration::from_millis(250),
            acceptance_timeout: Duration::from_secs(30),
            // 一轮里可能夹着多次项目工具调用，每次都要等用户审批（页面在这期间一直是生成中）：
            // 5 分钟不够用。用户随时可以停止；超时只是兜底，防止卡死的生成永久占住槽位。
            completion_timeout: Duration::from_secs(1800),
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
    saved_sink: Option<Arc<dyn SavedConversationSink>>,
    image_sink: Option<Arc<dyn crate::images::WebImageSink>>,
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
            saved_sink: None,
            image_sink: None,
        }
    }

    pub fn with_saved_sink(mut self, sink: Arc<dyn SavedConversationSink>) -> Self {
        self.saved_sink = Some(sink);
        self
    }

    /// 回复里的图片落地：不配置时，回复里的页面图片引用会被去掉（页面地址在 Magi 里不可用）。
    pub fn with_image_sink(mut self, sink: Arc<dyn crate::images::WebImageSink>) -> Self {
        self.image_sink = Some(sink);
        self
    }

    pub fn with_runtime(mut self, runtime: Arc<WebModelRuntimeRegistry>) -> Self {
        self.runtime = runtime;
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

    fn bridge_error(error: WebModelError) -> magi_bridge_client::BridgeClientError {
        error.into_bridge_error()
    }

    /// 只描述不一致的形状（角色、长度、首个差异位置），不带正文，便于排查又不泄露内容。
    fn describe_mismatch(expected: Option<&WebMessage>, observed: Option<&WebMessage>) -> String {
        let shape = |message: Option<&WebMessage>| match message {
            None => "无".to_string(),
            Some(message) => format!(
                "{}:{}字",
                message.role,
                normalize_composer_text(&message.text).chars().count()
            ),
        };
        let divergence = match (expected, observed) {
            (Some(expected), Some(observed)) => {
                let left = normalize_composer_text(&expected.text);
                let right = normalize_composer_text(&observed.text);
                left.chars()
                    .zip(right.chars())
                    .position(|(a, b)| a != b)
                    .map(|index| format!("，首个差异位于第{}个字符", index + 1))
                    .unwrap_or_default()
            }
            _ => String::new(),
        };
        format!(
            "Magi 期望 {}，网页实际 {}{}",
            shape(expected),
            shape(observed),
            divergence
        )
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
            WebConversationMode::Temporary => self.driver.open_temporary_chat(&self.page_id).await,
            WebConversationMode::Saved => {
                self.driver
                    .open_saved_chat(&self.page_id, self.config.remote_conversation_id.as_deref())
                    .await
            }
        }
    }

    /// 已保存对话的 turn 完成后回读远端事实（id / 标题 / 最后消息 id），交给绑定写入方。
    /// 这一步让“Magi 已同步到哪里”的指针前移到本轮回复之后，避免下次发送前的同步重复导入本轮。
    async fn report_saved_progress(&self) {
        if self.config.mode != WebConversationMode::Saved {
            return;
        }
        let Some(sink) = &self.saved_sink else {
            return;
        };
        let requested = self.config.remote_conversation_id.as_deref().unwrap_or("");
        match self
            .driver
            .read_saved_conversation(&self.page_id, requested)
            .await
        {
            Ok(snapshot) if snapshot.exists => {
                self.slots
                    .bind_remote_conversation(&self.owner, &snapshot.conversation_id);
                sink.record(
                    &self.identity.session_id,
                    SavedProgress {
                        remote_conversation_id: snapshot.conversation_id,
                        remote_title: snapshot.title,
                        last_remote_message_id: snapshot.last_message_id,
                        remote_updated_at: snapshot.remote_updated_at,
                    },
                )
            }
            _ => sink.mark_stale(&self.identity.session_id),
        }
    }

    /// 单张图片的字节上限与单条回复的图片张数上限。
    const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
    const MAX_IMAGES_PER_REPLY: usize = 8;
    const IMAGE_CHUNK_BYTES: u64 = 768 * 1024;

    /// 把回复里的页面图片读出来保存到项目里，并把引用改写成项目内路径。
    /// 单张失败不让整轮失败：该处换成一句说明，其余图片和正文照常交付。
    async fn resolve_page_images(&self, text: &str) -> String {
        let refs = crate::images::page_image_refs(text);
        if refs.is_empty() {
            return text.to_string();
        }
        let Some(sink) = self.image_sink.clone() else {
            return crate::images::strip_page_images(text);
        };
        let mut resolved = Vec::with_capacity(refs.len());
        for (index, image) in refs.iter().enumerate() {
            let outcome = if index >= Self::MAX_IMAGES_PER_REPLY {
                Err("单条回复的图片太多，已跳过".to_string())
            } else {
                self.save_page_image(sink.as_ref(), &image.src, &image.alt)
                    .await
            };
            resolved.push(match outcome {
                Ok(path) => format!("![{}]({path})", image.alt),
                Err(reason) => {
                    tracing::warn!(reason = %reason, "GPT Web 回复里的图片未能保存");
                    format!("（图片未能保存：{}）", image.alt)
                }
            });
        }
        // 按出现顺序一一对应（同一地址出现两次也各自处理）。
        let mut next = 0usize;
        crate::images::rewrite_page_images(text, |_| {
            let replacement = resolved.get(next).cloned().unwrap_or_default();
            next += 1;
            replacement
        })
    }

    async fn save_page_image(
        &self,
        sink: &dyn crate::images::WebImageSink,
        source: &str,
        alt: &str,
    ) -> Result<String, String> {
        let mut bytes: Vec<u8> = Vec::new();
        let mut mime = String::new();
        loop {
            let chunk = self
                .driver
                .read_image_chunk(
                    &self.page_id,
                    source,
                    bytes.len() as u64,
                    Self::IMAGE_CHUNK_BYTES,
                )
                .await
                .map_err(|error| error.message)?;
            if chunk.total > Self::MAX_IMAGE_BYTES {
                return Err("图片超过大小上限".to_string());
            }
            if chunk.offset != bytes.len() as u64 {
                return Err("图片分块顺序不一致".to_string());
            }
            if mime.is_empty() {
                mime = chunk.mime.clone();
            }
            let empty = chunk.data.is_empty();
            bytes.extend_from_slice(&chunk.data);
            if chunk.done || bytes.len() as u64 >= chunk.total {
                break;
            }
            if empty {
                return Err("图片分块为空".to_string());
            }
        }
        sink.store(
            &self.identity.session_id,
            &self.identity.project_id,
            crate::images::WebImage {
                mime,
                bytes,
                alt: alt.to_string(),
            },
        )
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

        let mut message_accepted = false;
        let result = async {
            // 临时对话的上下文只存在于槽位的页面里。网页里曾经有过上下文、槽位却已不在
            // （重启 / 停止 / 页面销毁）：上下文不可恢复，直接判失效，不碰页面、不新开一条冒充续接。
            if claim.newly_claimed
                && self.config.mode == WebConversationMode::Temporary
                && self.config.context_established
            {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebContextLost,
                    "网页里的临时对话上下文已不存在（页面已销毁或 Magi 已重启），未新开对话冒充续接",
                ));
            }
            self.open_page(claim.newly_claimed).await?;
            // 比对对象是槽位记住的「网页最后一条消息」，不是请求里的历史：GPT Web 的请求
            // 按设计只带本轮用户消息。已保存对话首次绑定时先由 Web 读取远端事实；临时对话
            // 新建时必须是空页。
            let prior = if claim.newly_claimed {
                None
            } else {
                self.slots.last_message(&self.owner)
            };
            if !claim.newly_claimed || self.config.mode == WebConversationMode::Temporary {
                let observed = self.driver.read_last_message(&self.page_id).await?;
                if !Self::same_message(prior.as_ref(), observed.as_ref()) {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebContextLost,
                        format!(
                            "网页最后一条消息与 Magi 会话不一致，未覆盖网页内容（{}）",
                            Self::describe_mismatch(prior.as_ref(), observed.as_ref())
                        ),
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
            let write = self
                .driver
                .write_text(&self.page_id, &request.prompt)
                .await?;
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
                    submitted
                        .reason
                        .unwrap_or_else(|| "ChatGPT Web 未接受本次消息".to_string()),
                ));
            }
            message_accepted = true;
            if let Some(sink) = &self.saved_sink {
                sink.message_accepted(&self.identity.session_id);
            }
            self.runtime.record_web_send(&self.identity.session_id);

            let acceptance_deadline = tokio::time::Instant::now() + self.config.acceptance_timeout;
            let mut last_assistant = String::new();
            let mut last_thinking = String::new();
            let mut stable_reads = 0u8;
            // 已经作为增量发出去的全文（按内容记账：页面文本改写时不会在字符中间切片）。
            let mut emitted_assistant = String::new();
            let mut emitted_thinking = String::new();
            let mut baseline_users = baseline.user_message_count;
            let mut baseline_assistants = baseline.assistant_message_count;
            // 基线被重置（页面换代或重绘）之后，数量不再能证明「本轮回复已经出现」：页面上仍可能
            // 是提交前的最后一条助手消息。此时再要求它的文本和提交前不同，才算本轮回复。
            let mut baseline_reset = false;
            // 本轮是否观察到过「正在生成」：重置基线后，回复恰好和上一条相同时靠它收口。
            let mut saw_generating = false;
            let mut accepted = false;
            let completion_deadline = tokio::time::Instant::now() + self.config.completion_timeout;
            loop {
                if is_cancelled() {
                    let _ = self.driver.cancel_generation(&self.page_id).await;
                    return Err(WebModelError::cancelled());
                }
                let mut state = self.driver.turn_state(&self.page_id).await?;
                // 提交会让 ChatGPT 把页面换成一个新对话（`/` → `/c/<id>`）：新页面的消息计数从头算，
                // 比提交前的基线还小。此时基线已经不是同一个对话的计数，按 0 重新记。
                if state.user_message_count < baseline_users
                    || state.assistant_message_count < baseline_assistants
                {
                    baseline_users = 0;
                    baseline_assistants = 0;
                    baseline_reset = true;
                }
                if state.user_message_count > baseline_users {
                    accepted = true;
                }
                // 开始生成同样是消息已被接受的证据（页面换代时计数证据可能错过）。
                if state.generating && !baseline.generating {
                    accepted = true;
                }
                if state.generating {
                    saw_generating = true;
                }
                // 页面读到的「最后一条助手消息」在本轮回复出现之前还是**上一轮**的回复。连续对话里
                // 必须等到助手消息数量比提交前多，才把它当作本轮内容；否则上一轮的回答会被当成本轮
                // 的流式输出写进 Magi 当前对话，甚至在本轮回复还没开始时就被当作最终结果收口。
                let reply_started = state.assistant_message_count > baseline_assistants
                    && !(baseline_reset
                        && !(saw_generating && !state.generating)
                        && !baseline.assistant_text.is_empty()
                        && state.assistant_text == baseline.assistant_text);
                if !reply_started {
                    state.assistant_text.clear();
                    // 思考阶段先于助手消息节点出现：消息被接受之后读到的推理就是本轮的（页面只在进行中的
                    // 回合里显示它）；还没被接受时页面上不可能有本轮的推理，读到的是上一轮残留。
                    if !accepted {
                        state.thinking_text.clear();
                    }
                }
                // 回复完成后页面会把推理区块收起成「已思考 N 秒」，读到的推理文字变成空。已经读到并
                // 显示出来的推理不能因此被清掉（否则会被当成整段改写，连同正文一起重置）：沿用最后
                // 一次读到的推理。
                if state.thinking_text.trim().is_empty() && !last_thinking.is_empty() {
                    state.thinking_text = last_thinking.clone();
                }
                if !accepted && tokio::time::Instant::now() >= acceptance_deadline {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebSendRejected,
                        "ChatGPT Web 未确认已接受本次消息",
                    ));
                }
                // 页面图片在 Magi 里还不可用：流式阶段只发文字，图片在收口时随全文一起交付。
                let visible_text = crate::images::strip_page_images(&state.assistant_text);
                if state.assistant_text != last_assistant || state.thinking_text != last_thinking {
                    // 流式就是增量：页面文本是已发内容的延长时只发新增的后缀；页面把已显示的文本整段
                    // 改写（例如 ChatGPT 在多次工具调用之间不断重建同一条助手消息、或重新渲染 markdown）
                    // 时，追加无法表达，发一帧 `replace`（携带改写后的完整文字），下游据此重置。
                    let appended = visible_text.starts_with(&emitted_assistant)
                        && state.thinking_text.starts_with(&emitted_thinking);
                    let frame = if appended {
                        ModelStreamingDelta {
                            content: visible_text[emitted_assistant.len()..].to_string(),
                            thinking: state.thinking_text[emitted_thinking.len()..].to_string(),
                            ..Default::default()
                        }
                    } else {
                        ModelStreamingDelta {
                            content: visible_text.clone(),
                            thinking: state.thinking_text.clone(),
                            replace: true,
                            ..Default::default()
                        }
                    };
                    if frame.replace || !frame.content.is_empty() || !frame.thinking.is_empty() {
                        on_delta(&frame);
                        emitted_assistant = visible_text.clone();
                        emitted_thinking = state.thinking_text.clone();
                    }
                    last_assistant = state.assistant_text.clone();
                    last_thinking = state.thinking_text.clone();
                    stable_reads = 0;
                } else if accepted && reply_started && !state.generating {
                    stable_reads = stable_reads.saturating_add(1);
                } else {
                    stable_reads = 0;
                }
                if accepted && reply_started && !state.generating && stable_reads >= 2 {
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
            self.report_saved_progress().await;
            // 槽位记住的是页面上的原始文本（下一轮要和页面最后一条消息比对）；交给会话的是
            // 图片已落地的版本。
            let delivered = self.resolve_page_images(&last_assistant).await;
            self.slots.set_last_message(
                &self.owner,
                WebMessage {
                    role: "assistant".to_string(),
                    text: last_assistant.clone(),
                    remote_id: None,
                },
            );
            Ok(ModelResponse {
                status: ModelResponseStatus::Completed,
                content: Some(delivered),
                thinking: (!last_thinking.trim().is_empty()).then_some(last_thinking),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".to_string()),
                provider_context: Vec::new(),
            })
        }
        .await;
        // 首次 claim 在页面打开、登录或写入阶段失败时不能留下一个没有可用
        // 页面上下文的 owner；存活判定失败也按状态机释放槽位。已被站点接受的
        // turn 保留槽位，允许用户按同一网页上下文重试或切换到本地。
        if (claim.newly_claimed && !message_accepted)
            || result
                .as_ref()
                .is_err_and(|error| error.code == WebModelErrorCode::WebContextLost)
        {
            self.slots.release(&self.owner);
            self.runtime.forget_session(&self.identity.session_id);
        }
        drop(lease);
        self.runtime.finish_web_turn(&self.identity.session_id);
        if let (Err(error), Some(sink)) = (&result, &self.saved_sink)
            && let Some(state) = error.code.engine_state()
        {
            sink.engine_state_changed(state);
        }
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
            tokio::task::block_in_place(|| {
                handle.block_on(client.execute(request, on_delta, is_cancelled))
            })
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

    // ── 假页面驱动：脚本化一个 ChatGPT 页面 ───────────────────────────────

    use std::sync::Mutex;

    use crate::driver::{
        DriverFuture, SavedConversationSnapshot, SubmitOutcome, TurnState, WriteOutcome,
    };

    #[derive(Default)]
    struct FakePage {
        messages: Vec<WebMessage>,
        composer: String,
        generating_steps: Option<u32>,
        calls: Vec<String>,
        saved_id: Option<String>,
        signed_out: bool,
        /// 模拟提交时 ChatGPT 把页面换成新对话：旧消息清空，只剩刚提交的用户消息。
        replace_on_submit: bool,
    }

    struct FakeDriver {
        page: Mutex<FakePage>,
        reply: String,
        /// 模拟真实页面：新一轮提交后，助手消息节点出现之前读到的「最后一条助手消息」仍是上一轮的。
        leak_previous_reply: std::sync::atomic::AtomicBool,
        /// 与 `leak_previous_reply` 同时生效：页面重绘，漏出上一轮回答的同时消息计数回落到基线以下。
        collapse_count_on_leak: std::sync::atomic::AtomicBool,
        /// 模拟页面把流式中的文本整段改写：中途读到的文本不是最终回复的前缀。
        rewrite_midstream: std::sync::atomic::AtomicBool,
        /// 页面上图片的字节；`None` 时读取图片失败。
        image: Mutex<Option<Vec<u8>>>,
        /// 回复生成期间页面上显示的推理文字；空表示没有推理区块。
        thinking: Mutex<String>,
    }

    impl FakeDriver {
        fn new(reply: &str) -> Arc<Self> {
            Arc::new(Self {
                page: Mutex::new(FakePage::default()),
                reply: reply.to_string(),
                leak_previous_reply: std::sync::atomic::AtomicBool::new(false),
                collapse_count_on_leak: std::sync::atomic::AtomicBool::new(false),
                rewrite_midstream: std::sync::atomic::AtomicBool::new(false),
                image: Mutex::new(None),
                thinking: Mutex::new(String::new()),
            })
        }
        fn with_thinking(&self, text: &str) {
            *self.thinking.lock().unwrap() = text.to_string();
        }
        fn with_image(&self, bytes: Vec<u8>) {
            *self.image.lock().unwrap() = Some(bytes);
        }
        fn rewrite_midstream(&self) {
            self.rewrite_midstream
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn collapse_count_on_leak(&self) {
            self.collapse_count_on_leak
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn leak_previous_reply(&self) {
            self.leak_previous_reply
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn calls(&self) -> Vec<String> {
            self.page.lock().unwrap().calls.clone()
        }
        fn push_call(&self, call: &str) {
            self.page.lock().unwrap().calls.push(call.to_string());
        }
    }

    impl WebModelPageDriver for FakeDriver {
        fn open_temporary_chat<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async move {
                self.push_call("open_temporary");
                self.page.lock().unwrap().messages.clear();
                Ok(())
            })
        }
        fn open_saved_chat<'a>(
            &'a self,
            _page_id: &'a str,
            conversation_id: Option<&'a str>,
        ) -> DriverFuture<'a, ()> {
            Box::pin(async move {
                self.push_call(&format!("open_saved:{}", conversation_id.unwrap_or("new")));
                let mut page = self.page.lock().unwrap();
                page.saved_id = Some(conversation_id.unwrap_or("conv-new").to_string());
                Ok(())
            })
        }
        fn resume_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, bool> {
            Box::pin(async move { Ok(true) })
        }
        fn close_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async move { Ok(()) })
        }
        fn write_text<'a>(
            &'a self,
            _page_id: &'a str,
            text: &'a str,
        ) -> DriverFuture<'a, WriteOutcome> {
            Box::pin(async move {
                self.push_call(&format!("write:{text}"));
                self.page.lock().unwrap().composer = text.to_string();
                Ok(WriteOutcome {
                    confirmed: true,
                    char_count: text.chars().count() as u64,
                    became_attachment: false,
                })
            })
        }
        fn submit<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, SubmitOutcome> {
            Box::pin(async move {
                self.push_call("submit");
                let mut page = self.page.lock().unwrap();
                let text = std::mem::take(&mut page.composer);
                if page.replace_on_submit {
                    page.messages.clear();
                }
                let id = format!("u{}", page.messages.len());
                page.messages.push(WebMessage {
                    role: "user".into(),
                    text,
                    remote_id: Some(id),
                });
                page.generating_steps = Some(0);
                Ok(SubmitOutcome {
                    submitted: true,
                    composer_empty: true,
                    reason: None,
                })
            })
        }
        fn cancel_generation<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async move {
                self.push_call("cancel");
                self.page.lock().unwrap().generating_steps = None;
                Ok(())
            })
        }
        fn read_image_chunk<'a>(
            &'a self,
            _page_id: &'a str,
            _source: &'a str,
            offset: u64,
            length: u64,
        ) -> DriverFuture<'a, crate::driver::ImageChunk> {
            Box::pin(async move {
                let image = self.image.lock().unwrap().clone().ok_or_else(|| {
                    WebModelError::new(WebModelErrorCode::WebSendRejected, "web_image_not_on_page")
                })?;
                let start = (offset as usize).min(image.len());
                let end = (start + length as usize).min(image.len());
                Ok(crate::driver::ImageChunk {
                    mime: "image/png".to_string(),
                    total: image.len() as u64,
                    offset: start as u64,
                    data: image[start..end].to_vec(),
                    done: end >= image.len(),
                })
            })
        }
        fn turn_state<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, TurnState> {
            Box::pin(async move {
                let mut page = self.page.lock().unwrap();
                let mut generating = false;
                let mut assistant_text = String::new();
                // 本轮的助手消息节点是否已经出现在页面上（出现后才计入助手消息数）。
                let mut reply_node_present = false;
                let mut collapsed_count = false;
                if let Some(step) = page.generating_steps {
                    if step < 2 {
                        generating = true;
                        let previous = page
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.role == "assistant")
                            .map(|m| m.text.clone());
                        match previous {
                            Some(previous)
                                if step == 0
                                    && self
                                        .leak_previous_reply
                                        .load(std::sync::atomic::Ordering::SeqCst) =>
                            {
                                assistant_text = previous;
                                collapsed_count = self
                                    .collapse_count_on_leak
                                    .load(std::sync::atomic::Ordering::SeqCst);
                            }
                            _ => {
                                reply_node_present = true;
                                assistant_text = if self
                                    .rewrite_midstream
                                    .load(std::sync::atomic::Ordering::SeqCst)
                                {
                                    "A旧。草稿".to_string()
                                } else {
                                    self.reply.chars().take(1).collect()
                                };
                            }
                        }
                        page.generating_steps = Some(step + 1);
                    } else {
                        let id = format!("a{}", page.messages.len());
                        page.messages.push(WebMessage {
                            role: "assistant".into(),
                            text: self.reply.clone(),
                            remote_id: Some(id),
                        });
                        page.generating_steps = None;
                        assistant_text = self.reply.clone();
                    }
                } else if let Some(last) = page.messages.last().filter(|m| m.role == "assistant") {
                    assistant_text = last.text.clone();
                }
                let users = page.messages.iter().filter(|m| m.role == "user").count() as u64;
                let assistants = page
                    .messages
                    .iter()
                    .filter(|m| m.role == "assistant")
                    .count() as u64
                    + u64::from(reply_node_present);
                let assistants = if collapsed_count { 1 } else { assistants };
                Ok(TurnState {
                    login_state: if page.signed_out {
                        LoginState::SignedOut
                    } else {
                        LoginState::SignedIn
                    },
                    blocked: false,
                    generating,
                    composer_found: true,
                    user_message_count: users,
                    assistant_message_count: assistants,
                    assistant_text,
                    thinking_text: if generating || reply_node_present {
                        self.thinking.lock().unwrap().clone()
                    } else {
                        String::new()
                    },
                    last_message_role: page.messages.last().map(|m| m.role.clone()),
                    last_message_text: page.messages.last().map(|m| m.text.clone()),
                })
            })
        }
        fn read_last_message<'a>(
            &'a self,
            _page_id: &'a str,
        ) -> DriverFuture<'a, Option<WebMessage>> {
            Box::pin(async move { Ok(self.page.lock().unwrap().messages.last().cloned()) })
        }
        fn read_saved_conversation<'a>(
            &'a self,
            _page_id: &'a str,
            conversation_id: &'a str,
        ) -> DriverFuture<'a, SavedConversationSnapshot> {
            Box::pin(async move {
                let page = self.page.lock().unwrap();
                let id = page.saved_id.clone().unwrap_or_default();
                Ok(SavedConversationSnapshot {
                    conversation_id: id.clone(),
                    title: Some("网页标题".to_string()),
                    messages: page.messages.clone(),
                    last_message_id: page.messages.last().and_then(|m| m.remote_id.clone()),
                    remote_updated_at: None,
                    exists: conversation_id.is_empty() || conversation_id == id,
                })
            })
        }
    }

    fn client_with(
        driver: Arc<FakeDriver>,
        slots: Arc<WebSlotTable>,
        session: &str,
        config: WebModelClientConfig,
    ) -> BrowserWebModelBridgeClient {
        BrowserWebModelBridgeClient::new(
            driver,
            slots,
            WebSlotOwner::new(session, "project-1"),
            WebModelIdentity {
                session_id: session.to_string(),
                project_id: "project-1".to_string(),
                thread_id: "orchestrator".to_string(),
                engine_id: crate::site::WEB_MODEL_ENGINE_ID.to_string(),
            },
            WebModelClientConfig {
                poll_interval: Duration::from_millis(1),
                ..config
            },
        )
    }

    fn request(prompt: &str, history: Vec<(&str, &str)>) -> ModelInvocationRequest {
        let mut messages: Vec<magi_bridge_client::ChatMessage> = history
            .into_iter()
            .map(|(role, text)| magi_bridge_client::ChatMessage {
                role: role.to_string(),
                content: Some(text.to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            })
            .collect();
        messages.push(magi_bridge_client::ChatMessage {
            role: "user".to_string(),
            content: Some(prompt.to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        });
        ModelInvocationRequest {
            provider: "chatgpt_web".to_string(),
            prompt: prompt.to_string(),
            messages: Some(messages),
            tools: Some(Vec::new()),
            tool_choice: None,
        }
    }

    #[tokio::test]
    async fn temporary_turn_sends_only_the_raw_prompt_and_reads_the_reply() {
        let driver = FakeDriver::new("你好");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        let response = client
            .execute(request("你好吗", vec![]), &|_| {}, &|| false)
            .await
            .expect("turn completes");
        assert_eq!(response.content.as_deref(), Some("你好"));
        let calls = driver.calls();
        assert_eq!(calls[0], "open_temporary");
        assert!(
            calls.contains(&"write:你好吗".to_string()),
            "只写本轮原文: {calls:?}"
        );
        assert_eq!(slots.owner().map(|o| o.session_id), Some("s1".to_string()));
        assert_eq!(client.runtime().snapshot(None)[0].sent_messages, 1);
    }

    #[derive(Default)]
    struct RecordingImageSink {
        stored: Mutex<Vec<(String, String, crate::images::WebImage)>>,
    }

    impl crate::images::WebImageSink for RecordingImageSink {
        fn store(
            &self,
            session_id: &str,
            project_id: &str,
            image: crate::images::WebImage,
        ) -> Result<String, String> {
            let mut stored = self.stored.lock().unwrap();
            stored.push((session_id.to_string(), project_id.to_string(), image));
            Ok(format!("generated-images/web-{}.png", stored.len()))
        }
    }

    const IMAGE_REPLY: &str = "好的\n\n![已生成图像 1](blob:https://chatgpt.com/a)\n\n![已生成图像 2](blob:https://chatgpt.com/a)";

    #[tokio::test]
    async fn page_images_are_saved_to_the_project_and_never_streamed_as_page_urls() {
        let driver = FakeDriver::new(IMAGE_REPLY);
        // 比一个分块大：必须按块读完整。
        let bytes: Vec<u8> = (0..(768 * 1024 + 10)).map(|i| (i % 251) as u8).collect();
        driver.with_image(bytes.clone());
        let sink = Arc::new(RecordingImageSink::default());
        let client = client_with(
            driver.clone(),
            Arc::new(WebSlotTable::new()),
            "s1",
            WebModelClientConfig::default(),
        )
        .with_image_sink(sink.clone());
        let streamed = Mutex::new(Vec::<ModelStreamingDelta>::new());
        let response = client
            .execute(
                request("画一张图", vec![]),
                &|delta| streamed.lock().unwrap().push(delta.clone()),
                &|| false,
            )
            .await
            .expect("turn completes");
        assert_eq!(
            response.content.as_deref(),
            Some(
                "好的\n\n![已生成图像 1](generated-images/web-1.png)\n\n![已生成图像 2](generated-images/web-2.png)"
            )
        );
        assert!(
            !streamed.lock().unwrap().iter().any(|frame| frame.content.contains("blob:")),
            "流式阶段不能出现页面地址"
        );
        let stored = sink.stored.lock().unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].0, "s1");
        assert_eq!(stored[0].1, "project-1");
        assert_eq!(stored[0].2.bytes, bytes);
        assert_eq!(stored[0].2.mime, "image/png");
        assert_eq!(stored[0].2.alt, "已生成图像 1");
    }

    #[tokio::test]
    async fn an_unreadable_image_becomes_a_note_instead_of_failing_the_turn() {
        let driver = FakeDriver::new(IMAGE_REPLY);
        let sink = Arc::new(RecordingImageSink::default());
        let client = client_with(
            driver,
            Arc::new(WebSlotTable::new()),
            "s1",
            WebModelClientConfig::default(),
        )
        .with_image_sink(sink.clone());
        let response = client
            .execute(request("画一张图", vec![]), &|_| {}, &|| false)
            .await
            .expect("turn completes");
        let content = response.content.unwrap();
        assert!(content.starts_with("好的"));
        assert!(content.contains("（图片未能保存：已生成图像 1）"));
        assert!(!content.contains("blob:"));
        assert!(sink.stored.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn without_an_image_sink_page_images_are_dropped() {
        let driver = FakeDriver::new(IMAGE_REPLY);
        driver.with_image(vec![1, 2, 3]);
        let client = client_with(
            driver,
            Arc::new(WebSlotTable::new()),
            "s1",
            WebModelClientConfig::default(),
        );
        let response = client
            .execute(request("画一张图", vec![]), &|_| {}, &|| false)
            .await
            .expect("turn completes");
        assert_eq!(response.content.as_deref(), Some("好的"));
    }

    #[tokio::test]
    async fn second_session_is_rejected_while_the_slot_is_held() {
        let driver = FakeDriver::new("ok");
        let slots = Arc::new(WebSlotTable::new());
        let first = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        first
            .execute(request("a", vec![]), &|_| {}, &|| false)
            .await
            .unwrap();
        let before = driver.calls().len();
        let second = client_with(
            driver.clone(),
            slots.clone(),
            "s2",
            WebModelClientConfig::default(),
        );
        let error = second
            .execute(request("b", vec![]), &|_| {}, &|| false)
            .await
            .expect_err("busy");
        assert_eq!(error.code, WebModelErrorCode::WebSessionBusy);
        assert_eq!(driver.calls().len(), before, "被拒绝的会话不得触碰唯一页面");
        assert_eq!(slots.owner().map(|o| o.session_id), Some("s1".to_string()));
    }

    #[tokio::test]
    async fn continuing_a_temporary_chat_loses_context_when_the_page_changed() {
        let driver = FakeDriver::new("回复");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        client
            .execute(request("第一问", vec![]), &|_| {}, &|| false)
            .await
            .unwrap();
        // 用户在网页里自己追加了一条消息：最后一条不再是 Magi 的上一条回复。
        driver.page.lock().unwrap().messages.push(WebMessage {
            role: "user".into(),
            text: "网页里手动发的".into(),
            remote_id: None,
        });
        let error = client
            .execute(
                request("第二问", vec![("user", "第一问"), ("assistant", "回复")]),
                &|_| {},
                &|| false,
            )
            .await
            .expect_err("context lost");
        assert_eq!(error.code, WebModelErrorCode::WebContextLost);
        assert!(slots.owner().is_none(), "存活判定失败必须释放槽位");
        assert!(
            !driver.calls().iter().any(|call| call == "write:第二问"),
            "判定失败时不得覆盖网页内容"
        );
    }

    #[tokio::test]
    async fn cancellation_stops_generation_and_keeps_the_slot() {
        let driver = FakeDriver::new("很长的回复");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        let polls = std::sync::atomic::AtomicU32::new(0);
        let error = client
            .execute(request("问", vec![]), &|_| {}, &|| {
                polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 3
            })
            .await
            .expect_err("cancelled");
        assert!(error.is_cancelled());
        assert!(driver.calls().contains(&"cancel".to_string()));
        assert_eq!(slots.owner().map(|o| o.session_id), Some("s1".to_string()));
    }

    #[tokio::test]
    async fn signed_out_page_fails_before_writing_and_releases_a_fresh_claim() {
        let driver = FakeDriver::new("x");
        driver.page.lock().unwrap().signed_out = true;
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        let error = client
            .execute(request("问", vec![]), &|_| {}, &|| false)
            .await
            .expect_err("login expired");
        assert_eq!(error.code, WebModelErrorCode::WebLoginExpired);
        assert!(!driver.calls().iter().any(|call| call.starts_with("write:")));
        assert!(slots.owner().is_none());
    }

    #[derive(Default)]
    struct RecordingSink {
        records: Mutex<Vec<(String, SavedProgress)>>,
        stale: Mutex<Vec<String>>,
        states: Mutex<Vec<crate::errors::EngineState>>,
    }

    impl SavedConversationSink for RecordingSink {
        fn record(&self, session_id: &str, progress: SavedProgress) {
            self.records
                .lock()
                .unwrap()
                .push((session_id.to_string(), progress));
        }
        fn mark_stale(&self, session_id: &str) {
            self.stale.lock().unwrap().push(session_id.to_string());
        }
        fn engine_state_changed(&self, state: crate::errors::EngineState) {
            self.states.lock().unwrap().push(state);
        }
    }

    #[tokio::test]
    async fn saved_new_chat_reports_remote_id_title_and_sync_pointer_after_the_turn() {
        let driver = FakeDriver::new("好");
        let slots = Arc::new(WebSlotTable::new());
        let sink = Arc::new(RecordingSink::default());
        let client = client_with(
            driver.clone(),
            slots,
            "s1",
            WebModelClientConfig {
                mode: WebConversationMode::Saved,
                remote_conversation_id: None,
                ..Default::default()
            },
        )
        .with_saved_sink(sink.clone());
        client
            .execute(request("问", vec![]), &|_| {}, &|| false)
            .await
            .unwrap();
        assert_eq!(driver.calls()[0], "open_saved:new");
        let records = sink.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        let (session, progress) = &records[0];
        assert_eq!(session, "s1");
        assert_eq!(progress.remote_conversation_id, "conv-new");
        assert_eq!(progress.remote_title.as_deref(), Some("网页标题"));
        assert_eq!(
            progress.last_remote_message_id.as_deref(),
            Some("a1"),
            "指针前移到本轮回复"
        );
        assert!(sink.stale.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_new_saved_chat_continues_on_its_second_turn_once_the_remote_id_is_known() {
        let driver = FakeDriver::new("好");
        let slots = Arc::new(WebSlotTable::new());
        let sink = Arc::new(RecordingSink::default());
        let first = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig {
                mode: WebConversationMode::Saved,
                remote_conversation_id: None,
                ..Default::default()
            },
        )
        .with_saved_sink(sink.clone());
        first
            .execute(request("问", vec![]), &|_| {}, &|| false)
            .await
            .unwrap();
        // 第二轮：会话绑定里已经记下首轮取得的远端 id（槽位里不能还是空的）。
        let second = client_with(
            driver,
            slots,
            "s1",
            WebModelClientConfig {
                mode: WebConversationMode::Saved,
                remote_conversation_id: Some("conv-new".to_string()),
                ..Default::default()
            },
        )
        .with_saved_sink(sink);
        second
            .execute(request("再问", vec![]), &|_| {}, &|| false)
            .await
            .expect("第二轮在同一个已保存对话里继续");
    }

    #[tokio::test]
    async fn login_expiry_during_a_send_reports_the_engine_state_for_the_picker() {
        let driver = FakeDriver::new("x");
        driver.page.lock().unwrap().signed_out = true;
        let sink = Arc::new(RecordingSink::default());
        let client = client_with(
            driver,
            Arc::new(WebSlotTable::new()),
            "s1",
            WebModelClientConfig::default(),
        )
        .with_saved_sink(sink.clone());
        client
            .execute(request("问", vec![]), &|_| {}, &|| false)
            .await
            .expect_err("login expired");
        assert_eq!(
            *sink.states.lock().unwrap(),
            vec![crate::errors::EngineState::LoginRequired]
        );
    }

    #[tokio::test]
    async fn a_second_turn_continues_on_the_same_page_without_any_request_history() {
        let driver = FakeDriver::new("好的");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        client
            .execute(request("第一问", vec![]), &|_| {}, &|| false)
            .await
            .unwrap();
        // GPT Web 的请求按设计只带本轮用户消息：没有历史，存活判定靠槽位记住的最后一条消息。
        let second = client
            .execute(request("第二问", vec![]), &|_| {}, &|| false)
            .await
            .expect("同一槽位续接不应判失效");
        assert_eq!(second.content.as_deref(), Some("好的"));
        let opens = driver
            .calls()
            .iter()
            .filter(|call| call.starts_with("open_"))
            .count();
        assert_eq!(opens, 1, "续接不得重新打开 / 新建对话");
    }

    /// 按下游的方式把增量帧累积成全文。
    fn accumulate(frames: &[ModelStreamingDelta]) -> (String, String) {
        let (mut content, mut thinking) = (String::new(), String::new());
        for frame in frames {
            frame.accumulate_into(&mut content, &mut thinking);
        }
        (content, thinking)
    }

    #[tokio::test]
    async fn a_second_turn_never_streams_the_previous_reply_as_its_own_content() {
        let driver = FakeDriver::new("新的回答");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        client
            .execute(request("第一问", vec![]), &|_| {}, &|| false)
            .await
            .unwrap();
        // 第二轮：本轮助手节点出现之前，页面上「最后一条助手消息」还是第一轮的回答。
        driver.leak_previous_reply();
        let streamed = std::sync::Mutex::new(Vec::<ModelStreamingDelta>::new());
        let second = client
            .execute(
                request("第二问", vec![]),
                &|delta| streamed.lock().unwrap().push(delta.clone()),
                &|| false,
            )
            .await
            .unwrap();
        assert_eq!(second.content.as_deref(), Some("新的回答"));
        let frames = streamed.lock().unwrap();
        let (content, _) = accumulate(&frames);
        assert_eq!(content, "新的回答", "上一轮的回答不能被当成本轮的流式输出：{frames:?}");
        assert!(frames.iter().all(|frame| !frame.replace), "追加不需要改写帧");
    }

    #[tokio::test]
    async fn a_page_rebuild_that_resets_the_baseline_still_never_streams_the_previous_reply() {
        let driver = FakeDriver::new("新的回答");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        for question in ["第一问", "第二问"] {
            client
                .execute(request(question, vec![]), &|_| {}, &|| false)
                .await
                .unwrap();
        }
        // 第三轮：页面重绘，消息计数回落到提交前的基线以下，同时仍显示上一轮的回答。
        driver.leak_previous_reply();
        driver.collapse_count_on_leak();
        let streamed = std::sync::Mutex::new(Vec::<ModelStreamingDelta>::new());
        let third = client
            .execute(
                request("第三问", vec![]),
                &|delta| streamed.lock().unwrap().push(delta.clone()),
                &|| false,
            )
            .await
            .unwrap();
        assert_eq!(third.content.as_deref(), Some("新的回答"));
        let frames = streamed.lock().unwrap();
        let (content, _) = accumulate(&frames);
        assert_eq!(
            content, "新的回答",
            "基线重置后，上一轮的回答也不能被当成本轮的流式输出：{frames:?}"
        );
    }

    #[tokio::test]
    async fn page_reasoning_streams_as_thinking_increments() {
        let driver = FakeDriver::new("最终回答");
        driver.with_thinking("先拆解问题，再逐步推导。");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(driver.clone(), slots.clone(), "s1", WebModelClientConfig::default());
        let frames = std::sync::Mutex::new(Vec::<ModelStreamingDelta>::new());
        client
            .execute(request("问", vec![]), &|delta| frames.lock().unwrap().push(delta.clone()), &|| false)
            .await
            .unwrap();
        let frames = frames.lock().unwrap();
        let (content, thinking) = accumulate(&frames);
        assert_eq!(content, "最终回答");
        assert_eq!(thinking, "先拆解问题，再逐步推导。", "页面推理必须流进 thinking：{frames:?}");
        assert!(frames.iter().all(|frame| !frame.replace));
    }

    #[tokio::test]
    async fn streaming_frames_are_increments_never_the_full_text_again() {
        let driver = FakeDriver::new("这是一段会被分成多帧发出的较长回答。");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(driver.clone(), slots.clone(), "s1", WebModelClientConfig::default());
        let frames = std::sync::Mutex::new(Vec::<ModelStreamingDelta>::new());
        client
            .execute(request("问", vec![]), &|delta| frames.lock().unwrap().push(delta.clone()), &|| false)
            .await
            .unwrap();
        let frames = frames.lock().unwrap();
        assert!(frames.len() >= 2, "应该分成多帧：{frames:?}");
        let mut seen = String::new();
        for frame in frames.iter() {
            assert!(!frame.replace);
            assert!(!seen.contains(&frame.content) || frame.content.is_empty(), "帧不能重复已发内容：{frames:?}");
            seen.push_str(&frame.content);
        }
        assert_eq!(seen, "这是一段会被分成多帧发出的较长回答。");
    }

    #[tokio::test]
    async fn a_rewritten_midstream_text_never_panics_on_multibyte_content() {
        let driver = FakeDriver::new("新的回答，已经重新生成完毕，这一段足够长。");
        driver.rewrite_midstream();
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        let frames = std::sync::Mutex::new(Vec::<ModelStreamingDelta>::new());
        let response = client
            .execute(
                request("问", vec![]),
                &|delta| frames.lock().unwrap().push(delta.clone()),
                &|| false,
            )
            .await
            .expect("改写中途文本不能让执行线程 panic");
        assert_eq!(
            response.content.as_deref(),
            Some("新的回答，已经重新生成完毕，这一段足够长。")
        );
        // 流式帧是增量：页面把文本整段改写时发一帧 replace（完整文字），之后继续照常发出，
        // 消费方累积出的内容与收口全文一致，界面不会停住到收口。
        let frames = frames.lock().unwrap();
        assert!(frames.iter().any(|frame| frame.replace), "改写必须以 replace 帧表达");
        let (content, _) = accumulate(&frames);
        assert_eq!(content, "新的回答，已经重新生成完毕，这一段足够长。");
    }

    #[tokio::test]
    async fn a_page_replaced_by_a_fresh_conversation_on_submit_still_counts_as_accepted() {
        // 提交前页面上还留着上一个对话（1 条用户 + 1 条助手）；提交后 ChatGPT 换成新对话，计数从头算。
        let driver = FakeDriver::new("新对话里的回答");
        {
            let mut page = driver.page.lock().unwrap();
            page.messages.push(WebMessage {
                role: "user".into(),
                text: "旧问".into(),
                remote_id: Some("u0".into()),
            });
            page.messages.push(WebMessage {
                role: "assistant".into(),
                text: "旧答".into(),
                remote_id: Some("a0".into()),
            });
            page.replace_on_submit = true;
        }
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig::default(),
        );
        let response = client
            .execute(request("新问", vec![]), &|_| {}, &|| false)
            .await
            .expect("页面换成新对话后，消息仍应被判为已接受");
        assert_eq!(response.content.as_deref(), Some("新对话里的回答"));
    }

    #[tokio::test]
    async fn established_temporary_context_without_a_slot_is_lost_without_touching_the_page() {
        let driver = FakeDriver::new("x");
        let slots = Arc::new(WebSlotTable::new());
        let client = client_with(
            driver.clone(),
            slots.clone(),
            "s1",
            WebModelClientConfig {
                context_established: true,
                ..Default::default()
            },
        );
        let error = client
            .execute(request("续接", vec![]), &|_| {}, &|| false)
            .await
            .expect_err("槽位已不在，上下文不可恢复");
        assert_eq!(error.code, WebModelErrorCode::WebContextLost);
        assert!(
            driver.calls().is_empty(),
            "不得打开页面或写入任何内容: {:?}",
            driver.calls()
        );
        assert!(slots.owner().is_none());
    }
}
