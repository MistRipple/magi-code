//! GPT Web 的宿主驱动与单槽位 client 工厂。

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use magi_browser_authority::{
    BrowserAuthority, BrowserHostClient, BrowserHostClientError, BrowserHostCommand,
    BrowserHostCommandOutcome, BrowserHostCommandResult, BrowserLogicalViewport,
    BrowserTabLifecycle, BrowserWebWriteMode,
};
use magi_core::{BrowserSessionId, BrowserTabId, UtcMillis};
use magi_web_model::{
    new_web_slot_table, BrowserWebModelBridgeClient, DriverFuture, LoginState,
    SubmitOutcome, TurnState, WebConversationBinding, WebConversationMode, WebMessage,
    WebModelClientConfig, WebModelClientFactory, WebModelError, WebModelErrorCode,
    WebModelIdentity, WebModelInvocationSpec, WebModelPageDriver, WebModelRuntimeRegistry,
    WebSlotOwner, WebSlotTable, WriteOutcome,
};

const COMPOSER_SELECTOR_TOKEN: &str = "@composer";
const PAGE_READY_TIMEOUT: Duration = Duration::from_secs(60);
const PAGE_READY_POLL: Duration = Duration::from_millis(250);

pub struct HostWebModelPageDriver {
    host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    authority: Arc<Mutex<BrowserAuthority>>,
}

impl HostWebModelPageDriver {
    pub fn new(host_client: Arc<RwLock<Option<BrowserHostClient>>>, authority: Arc<Mutex<BrowserAuthority>>) -> Self {
        Self { host_client, authority }
    }

    fn client(&self) -> Result<BrowserHostClient, WebModelError> {
        self.host_client.read().ok().and_then(|value| value.clone()).ok_or_else(||
            WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, "Desktop 浏览器宿主未连接")
        )
    }

    fn app_session(&self) -> Result<BrowserSessionId, WebModelError> {
        self.authority.lock().map_err(|_| unavailable("浏览器权威状态不可用"))?
            .app_session_id().cloned().ok_or_else(||
                WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, "应用级 GPT Web 浏览器会话尚未创建")
            )
    }

    fn home_tab(&self) -> Result<BrowserTabId, WebModelError> {
        let authority = self.authority.lock().map_err(|_| unavailable("浏览器权威状态不可用"))?;
        authority.app_session().and_then(|session| session.tab_ids.first()).cloned().ok_or_else(||
            WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, "应用级 GPT Web 主页尚未创建")
        )
    }

    fn page_state(&self, tab_id: &BrowserTabId) -> Result<(u64, u64, String), WebModelError> {
        let authority = self.authority.lock().map_err(|_| unavailable("浏览器权威状态不可用"))?;
        let tab = authority.tab(tab_id).ok_or_else(|| WebModelError::new(WebModelErrorCode::WebContextLost, "GPT Web 页面不存在"))?;
        Ok((tab.navigation_revision, tab.snapshot_revision, tab.url.clone()))
    }

    async fn restore(&self, url: String) -> Result<BrowserTabId, WebModelError> {
        let client = self.client()?;
        let tab_id = self.home_tab()?;
        let (navigation_revision, snapshot_revision, _) = self.page_state(&tab_id)?;
        let reply = client.request(BrowserHostCommand::RestorePage {
            tab_id: tab_id.clone(),
            browser_session_id: self.app_session()?,
            initial_url: url,
            logical_viewport: BrowserLogicalViewport::Auto,
            navigation_revision,
            snapshot_revision,
            allow_page_eviction: false,
        }).await.map_err(host_error)?;
        match reply.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::PageState(page) => {
                let mut authority = self.authority.lock().map_err(|_| unavailable("浏览器权威状态不可用"))?;
                let _ = authority.transition_tab(&tab_id, BrowserTabLifecycle::Ready, UtcMillis::now());
                let _ = authority.apply_host_page_state(&tab_id, page.navigation_revision, page.url, page.origin, page.title, UtcMillis::now());
                }
                _ => return Err(invalid("物化页面返回结果无效")),
            },
            BrowserHostCommandOutcome::Failed(error) | BrowserHostCommandOutcome::Indeterminate(error) => return Err(WebModelError::new(WebModelErrorCode::WebSendRejected, error.message)),
            BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
        }
        Ok(tab_id)
    }

    async fn read_state(&self, tab_id: &BrowserTabId) -> Result<TurnState, WebModelError> {
        let reply = self.client()?.request(BrowserHostCommand::WebTurnState { tab_id: tab_id.clone() }).await.map_err(host_error)?;
        match reply.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::Json { value } => state_from_json(&value),
                _ => Err(invalid("回合状态结果无效")),
            },
            BrowserHostCommandOutcome::Failed(error) | BrowserHostCommandOutcome::Indeterminate(error) => Err(WebModelError::new(WebModelErrorCode::WebSendRejected, error.message)),
            BrowserHostCommandOutcome::Cancelled => Err(WebModelError::cancelled()),
        }
    }

    async fn wait_ready(&self, tab_id: &BrowserTabId) -> Result<(), WebModelError> {
        let deadline = tokio::time::Instant::now() + PAGE_READY_TIMEOUT;
        loop {
            let state = self.read_state(tab_id).await?;
            if state.blocked { return Err(WebModelError::new(WebModelErrorCode::WebSiteBlocked, "ChatGPT Web 当前处于风险或验证页面")); }
            if state.login_state == LoginState::SignedOut && state.composer_found {
                return Err(WebModelError::new(WebModelErrorCode::WebLoginExpired, "ChatGPT Web 尚未登录或登录已过期"));
            }
            if state.composer_found { return Ok(()); }
            if tokio::time::Instant::now() >= deadline { return Err(WebModelError::new(WebModelErrorCode::WebSelectorsDrift, "GPT Web 页面在时限内未就绪")); }
            tokio::time::sleep(PAGE_READY_POLL).await;
        }
    }
}

impl WebModelPageDriver for HostWebModelPageDriver {
    fn open_temporary_chat<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let tab = self.restore(magi_web_model::chatgpt_web_temporary_chat_url()).await?;
            self.wait_ready(&tab).await
        })
    }

    fn open_saved_chat<'a>(&'a self, _page_id: &'a str, conversation_id: Option<&'a str>) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let id = conversation_id.ok_or_else(|| WebModelError::new(WebModelErrorCode::WebSavedConversationUnavailable, "已保存对话缺少 conversation_id"))?;
            let url = format!("{}/c/{}", magi_web_model::chatgpt_web_origin(), id);
            let tab = self.restore(url).await?;
            self.wait_ready(&tab).await
        })
    }

    fn resume_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, bool> {
        Box::pin(async move {
            let tab = match self.home_tab() { Ok(tab) => tab, Err(_) => return Ok(false) };
            match self.wait_ready(&tab).await { Ok(()) => Ok(true), Err(WebModelError { code: WebModelErrorCode::WebLoginExpired, .. }) => Ok(false), Err(error) => Err(error) }
        })
    }

    fn close_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        // Tab 关闭只隐藏；真正停止由 API/设置路径释放宿主。不要在 client turn 收口时
        // 销毁应用级 WebView。
        Box::pin(async { Ok(()) })
    }

    fn write_text<'a>(&'a self, _page_id: &'a str, text: &'a str) -> DriverFuture<'a, WriteOutcome> {
        Box::pin(async move {
            let reply = self.client()?.request(BrowserHostCommand::WebWriteText {
                tab_id: self.home_tab()?, selector: COMPOSER_SELECTOR_TOKEN.to_string(), text: text.to_string(), mode: BrowserWebWriteMode::Replace, expect_text_digest: None, timeout_ms: None,
            }).await.map_err(host_error)?;
            let value = match reply.response.outcome { BrowserHostCommandOutcome::Succeeded(result) => match *result { BrowserHostCommandResult::Json { value } => value, _ => return Err(invalid("写入结果无效")) }, BrowserHostCommandOutcome::Failed(error) | BrowserHostCommandOutcome::Indeterminate(error) => return Err(WebModelError::new(WebModelErrorCode::WebWriteNotConfirmed, error.message)), BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()) };
            Ok(WriteOutcome { confirmed: value.get("confirmed").and_then(serde_json::Value::as_bool).unwrap_or(false), char_count: value.get("char_count").and_then(serde_json::Value::as_u64).unwrap_or_default(), became_attachment: value.get("became_attachment").and_then(serde_json::Value::as_bool).unwrap_or(false) })
        })
    }

    fn submit<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, SubmitOutcome> {
        Box::pin(async move {
            let reply = self.client()?.request(BrowserHostCommand::WebSubmit { tab_id: self.home_tab()? }).await.map_err(host_error)?;
            let value = match reply.response.outcome { BrowserHostCommandOutcome::Succeeded(result) => match *result { BrowserHostCommandResult::Json { value } => value, _ => return Err(invalid("提交结果无效")) }, BrowserHostCommandOutcome::Failed(error) | BrowserHostCommandOutcome::Indeterminate(error) => return Err(WebModelError::new(WebModelErrorCode::WebSendRejected, error.message)), BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()) };
            Ok(SubmitOutcome { submitted: value.get("submitted").and_then(serde_json::Value::as_bool).unwrap_or(false), composer_empty: value.get("composer_empty").and_then(serde_json::Value::as_bool).unwrap_or(false), reason: value.get("reason").and_then(serde_json::Value::as_str).map(str::to_string) })
        })
    }

    fn cancel_generation<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move { let reply = self.client()?.request(BrowserHostCommand::WebCancelGeneration { tab_id: self.home_tab()? }).await.map_err(host_error)?; match reply.response.outcome { BrowserHostCommandOutcome::Succeeded(_) => Ok(()), BrowserHostCommandOutcome::Failed(error) | BrowserHostCommandOutcome::Indeterminate(error) => Err(WebModelError::new(WebModelErrorCode::WebSendRejected, error.message)), BrowserHostCommandOutcome::Cancelled => Err(WebModelError::cancelled()), } })
    }

    fn turn_state<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, TurnState> {
        Box::pin(async move { self.read_state(&self.home_tab()?).await })
    }

    fn read_last_message<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, Option<WebMessage>> {
        Box::pin(async move { let state = self.turn_state(page_id).await?; Ok((!state.assistant_text.is_empty()).then(|| WebMessage::assistant(state.assistant_text))) })
    }
}

pub struct WebModelHostFactory {
    driver: Arc<dyn WebModelPageDriver>,
    slots: Arc<WebSlotTable>,
    host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    authority: Arc<Mutex<BrowserAuthority>>,
    runtime: Arc<WebModelRuntimeRegistry>,
}

impl WebModelHostFactory {
    pub fn new(host_client: Arc<RwLock<Option<BrowserHostClient>>>, authority: Arc<Mutex<BrowserAuthority>>) -> Result<Self, String> {
        Ok(Self { driver: Arc::new(HostWebModelPageDriver::new(Arc::clone(&host_client), Arc::clone(&authority))), slots: new_web_slot_table(), host_client, authority, runtime: Arc::new(WebModelRuntimeRegistry::new()) })
    }
    pub fn slots(&self) -> &Arc<WebSlotTable> { &self.slots }
    pub fn runtime(&self) -> &Arc<WebModelRuntimeRegistry> { &self.runtime }
    /// 旧装配入口改名后的语义：返回应用级单槽位，而不是会话绑定表。
    pub fn bindings(&self) -> &Arc<WebSlotTable> { &self.slots }
}

impl WebModelClientFactory for WebModelHostFactory {
    fn build_web_model_client(&self, spec: WebModelInvocationSpec) -> Result<Arc<dyn magi_bridge_client::ModelBridgeClient>, String> {
        if spec.engine_id != magi_web_model::WEB_MODEL_ENGINE_ID { return Err("GPT Web 只支持固定 chatgpt-web/default 入口".to_string()); }
        let identity = WebModelIdentity { session_id: spec.session_id.clone(), project_id: spec.project_id.clone(), thread_id: spec.thread_id, engine_id: spec.engine_id, effort: spec.effort };
        let client = BrowserWebModelBridgeClient::new(Arc::clone(&self.driver), Arc::clone(&self.slots), WebSlotOwner::new(spec.session_id, spec.project_id), identity, WebModelClientConfig { mode: spec.mode, remote_conversation_id: spec.remote_conversation_id, ..Default::default() }).with_runtime(Arc::clone(&self.runtime));
        Ok(Arc::new(client))
    }
}

fn unavailable(message: &str) -> WebModelError { WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, message) }
fn invalid(message: &str) -> WebModelError { WebModelError::new(WebModelErrorCode::WebWriteNotConfirmed, message) }
fn host_error(error: BrowserHostClientError) -> WebModelError { WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, error.to_string()) }

fn state_from_json(value: &serde_json::Value) -> Result<TurnState, WebModelError> {
    let login = match value.get("login_state").and_then(serde_json::Value::as_str).unwrap_or("signed_out") { "signed_in" => LoginState::SignedIn, "blocked" => LoginState::Blocked, _ => LoginState::SignedOut };
    Ok(TurnState { login_state: login, blocked: value.get("blocked").and_then(serde_json::Value::as_bool).unwrap_or(false), generating: value.get("generating").and_then(serde_json::Value::as_bool).unwrap_or(false), composer_found: value.get("composer_found").and_then(serde_json::Value::as_bool).unwrap_or(false), user_message_count: value.get("user_message_count").and_then(serde_json::Value::as_u64).unwrap_or_default(), assistant_message_count: value.get("assistant_message_count").and_then(serde_json::Value::as_u64).unwrap_or_default(), assistant_text: value.get("assistant_text").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(), thinking_text: value.get("thinking_text").and_then(serde_json::Value::as_str).unwrap_or_default().to_string() })
}
