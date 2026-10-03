//! GPT Web 的 daemon 侧运维操作：已保存对话（列表 / 绑定 / 同步 / 同步删除）、停止与槽位释放。
//!
//! 这些操作都经唯一的宿主页面驱动进入唯一的 WebView；状态事实只有两处：
//! 槽位表（进程内存）与会话级 `webConversation` 绑定（只存模式与远端引用）。
//! 已保存对话的历史只沿 **Web → Magi** 方向同步，绝不反向写回 Web（W2、W13）。

use std::sync::Arc;

use magi_conversation_runtime::model_config::{
    SESSION_WEB_CONVERSATION_SECTION, session_web_conversation_binding,
};
use magi_conversation_runtime::web_history_import::{
    RemoteWebExchange, import_remote_web_exchange,
};
use magi_core::SessionId;
use magi_web_model::{
    SavedConversationEntry, WebConversationBinding, WebConversationMode, WebConversationSyncState,
    WebMessage, WebModelError, WebModelErrorCode, WebModelPageDriver,
};

use magi_browser_authority::{BrowserHostCommand, BrowserHostCommandOutcome, BrowserTabLifecycle};
use magi_core::{BrowserTabId, UtcMillis, WorkspaceId};

use crate::ApiError;
use crate::routes::browser::WEB_MODEL_HOME_TAB_ID;
use crate::state::ApiState;

/// 驱动使用的页面 id：应用级只有一个 WebView，驱动实现忽略它。
const SLOT_PAGE_ID: &str = "browser-tab-web-model-home";
/// ChatGPT 侧连接器的显示名。
const WEB_CONNECTOR_NAME: &str = "Magi";
const HOME_RECOVERY_ATTEMPTS: usize = 4;
const HOME_RECOVERY_DELAY: std::time::Duration = std::time::Duration::from_millis(800);

/// 标签页快捷地址的目标页面。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebModelPageTarget {
    Chat,
    Tunnels,
    ApiKeys,
}

/// 把 GPT Web 运行时错误映射为 HTTP 错误：消息保持 `code: message`，前端按前缀识别错误码。
pub(crate) fn web_model_api_error(error: magi_web_model::WebModelError) -> ApiError {
    use magi_web_model::WebModelErrorCode as Code;
    match error.code {
        Code::WebSessionBusy | Code::WebSavedConversationConflict => {
            ApiError::Conflict(error.to_string())
        }
        Code::WebDesktopUnavailable => ApiError::CapabilityUnavailable {
            capability: "web_model".to_string(),
            platform: "headless".to_string(),
            message: error.to_string(),
        },
        _ => ApiError::InvalidInput(error.to_string()),
    }
}

/// 把线性的远端消息配成“用户 + 助手”往返。孤立的助手消息与结尾没有回复的用户消息不导入
/// （前者没有可展示的提问，后者还没有回复可同步）。
pub(crate) fn pair_exchanges(messages: &[WebMessage]) -> Vec<RemoteWebExchange> {
    let mut exchanges = Vec::new();
    let mut pending_user: Option<String> = None;
    for message in messages {
        match message.role.as_str() {
            "user" => pending_user = Some(message.text.clone()),
            "assistant" => {
                if let Some(user_text) = pending_user.take() {
                    exchanges.push(RemoteWebExchange {
                        user_text,
                        // 页面里的图片地址（blob:）在 Magi 里不可用；同步历史只带文字。
                        assistant_text: {
                            let text = magi_web_model::strip_page_images(&message.text);
                            if text.trim().is_empty() && !message.text.trim().is_empty() {
                                "（这条回复只有图片，同步时未带上图片）".to_string()
                            } else {
                                text
                            }
                        },
                        last_remote_id: message.remote_id.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    exchanges
}

/// 远端消息里 Magi 尚未同步的尾部。`last_synced` 为空表示全部缺失；
/// 指针对应的消息已不在远端（被删除 / 分支）返回 `None`，由调用方报告冲突。
pub(crate) fn unsynced_tail<'a>(
    messages: &'a [WebMessage],
    last_synced: Option<&str>,
) -> Option<&'a [WebMessage]> {
    match last_synced {
        None => Some(messages),
        Some(id) => messages
            .iter()
            .position(|message| message.remote_id.as_deref() == Some(id))
            .map(|index| &messages[index + 1..]),
    }
}

/// 已保存对话进度的唯一写入方：client 回报远端事实，这里写入会话级绑定。
struct ApiSavedSink {
    state: ApiState,
}

impl magi_web_model::SavedConversationSink for ApiSavedSink {
    fn record(&self, session_id: &str, progress: magi_web_model::SavedProgress) {
        self.state
            .record_saved_web_progress(&SessionId::new(session_id), progress);
    }

    fn message_accepted(&self, session_id: &str) {
        let session_id = SessionId::new(session_id);
        let mut binding = session_web_conversation_binding(&self.state.settings_store, &session_id);
        // 已保存模式的同步状态由 `record` 维护；这里只标记临时对话“网页里已有上下文”。
        if binding.mode == WebConversationMode::Temporary
            && binding.sync_state != WebConversationSyncState::Active
        {
            binding.sync_state = WebConversationSyncState::Active;
            if let Err(error) = self.state.write_web_binding(&session_id, &binding) {
                tracing::warn!(%session_id, %error, "标记临时对话已建立失败");
            }
        }
    }

    fn engine_state_changed(&self, state: magi_web_model::EngineState) {
        use magi_web_model::EngineState;
        // 工具通道不可用不改变入口可用性（只是没有项目工具）；其余状态让入口按唯一投影隐藏。
        let status = match state {
            EngineState::Available | EngineState::ToolUnavailable => return,
            EngineState::LoginRequired => "login_required",
            EngineState::SiteBlocked => "site_blocked",
            EngineState::DesktopUnavailable => "desktop_unavailable",
            EngineState::QuotaExhausted => "quota_exhausted",
        };
        let account_hint = self
            .state
            .web_model_probe_snapshot()
            .map(|probe| probe.account_hint)
            .unwrap_or_else(|| "unknown".to_string());
        self.state
            .record_web_model_probe(crate::state::WebModelProbeSnapshot {
                status: status.to_string(),
                account_hint,
                probed_at: magi_core::UtcMillis::now().0,
            });
    }

    fn mark_stale(&self, session_id: &str) {
        let session_id = SessionId::new(session_id);
        let mut binding = session_web_conversation_binding(&self.state.settings_store, &session_id);
        binding.sync_state = WebConversationSyncState::Stale;
        if let Err(error) = self.state.write_web_binding(&session_id, &binding) {
            tracing::warn!(%session_id, %error, "标记已保存对话同步指针失效失败");
        }
    }
}

/// GPT Web 回复里的图片：保存到会话所属项目的 `generated-images/`（与 Magi 自己的生图工具同一个位置），
/// 文件名取内容摘要，同一张图重复保存是幂等的。
struct ApiImageSink {
    state: ApiState,
}

impl magi_web_model::WebImageSink for ApiImageSink {
    fn store(
        &self,
        session_id: &str,
        project_id: &str,
        image: magi_web_model::WebImage,
    ) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let extension = match image.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "image/gif" => "gif",
            other => return Err(format!("不支持的图片类型：{other}")),
        };
        if image.bytes.is_empty() {
            return Err("图片内容为空".to_string());
        }
        // 项目会话落到项目目录；没有项目的个人会话落到 Magi 管理的私有目录。
        let root = self
            .state
            .workspace_root_path(&Some(WorkspaceId::new(project_id)))
            .unwrap_or_else(|| {
                self.state
                    .personal_session_execution_root_path(&SessionId::new(session_id))
            });
        let digest = Sha256::digest(&image.bytes);
        let name: String = digest
            .iter()
            .take(6)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let relative = format!("generated-images/web-{name}.{extension}");
        let target = root.join(&relative);
        if !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("创建目录失败：{error}"))?;
            }
            std::fs::write(&target, &image.bytes)
                .map_err(|error| format!("写入图片失败：{error}"))?;
        }
        Ok(relative)
    }
}

impl ApiState {
    pub fn web_image_sink(&self) -> Arc<dyn magi_web_model::WebImageSink> {
        Arc::new(ApiImageSink {
            state: self.clone(),
        })
    }

    pub fn web_saved_sink(&self) -> Arc<dyn magi_web_model::SavedConversationSink> {
        Arc::new(ApiSavedSink {
            state: self.clone(),
        })
    }

    /// turn 完成后把远端 id / 标题 / 同步指针写入绑定；标题以 Web 为准同步到会话名（W14）。
    fn record_saved_web_progress(
        &self,
        session_id: &SessionId,
        progress: magi_web_model::SavedProgress,
    ) {
        let mut binding = session_web_conversation_binding(&self.settings_store, session_id);
        binding.remote_conversation_id = Some(progress.remote_conversation_id);
        let title_changed =
            progress.remote_title.is_some() && progress.remote_title != binding.remote_title;
        if progress.remote_title.is_some() {
            binding.remote_title = progress.remote_title.clone();
        }
        binding.last_synced_remote_message_id = progress
            .last_remote_message_id
            .or(binding.last_synced_remote_message_id);
        binding.remote_updated_at = progress.remote_updated_at.or(binding.remote_updated_at);
        binding.sync_state = WebConversationSyncState::Active;
        if let Err(error) = self.write_web_binding(session_id, &binding) {
            tracing::warn!(%session_id, %error, "写入已保存对话绑定失败");
            return;
        }
        if title_changed
            && let Some(title) = progress
                .remote_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
            && let Err(error) = self.session_store.rename_session(session_id, title)
        {
            tracing::warn!(%session_id, %error, "已保存对话标题同步到会话名称失败");
        }
    }

    fn web_driver(&self) -> Result<Arc<dyn WebModelPageDriver>, WebModelError> {
        self.web_model.driver().ok_or_else(|| {
            WebModelError::new(
                WebModelErrorCode::WebDesktopUnavailable,
                "GPT Web 宿主驱动未装配（需要 Magi Desktop）",
            )
        })
    }

    fn write_web_binding(
        &self,
        session_id: &SessionId,
        binding: &WebConversationBinding,
    ) -> Result<(), WebModelError> {
        let value = serde_json::to_value(binding).map_err(|error| {
            WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                error.to_string(),
            )
        })?;
        self.settings_store
            .set_session_section(session_id, SESSION_WEB_CONVERSATION_SECTION, value)
            .map_err(|error| {
                WebModelError::new(
                    WebModelErrorCode::WebSavedConversationUnavailable,
                    format!("保存 GPT Web 对话绑定失败：{error}"),
                )
            })
    }

    /// 设置 GPT Web 对话方式。只在会话首条消息之前、且尚未绑定远端对话时允许（引擎与方式在
    /// 首条消息时固定，W3）。`override_fields` 是同一次请求里的其他会话配置，用来判断请求
    /// 是否同时选择了 GPT Web 入口。
    pub(crate) fn set_web_conversation_mode(
        &self,
        session_id: &SessionId,
        mode: &str,
        override_fields: &serde_json::Map<String, serde_json::Value>,
        is_new_session_first_request: bool,
    ) -> Result<(), ApiError> {
        let session_engine = self
            .settings_store
            .get_session_section(session_id, "orchestrator")
            .get("engineId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let engine_id = match override_fields
            .get("engineId")
            .and_then(serde_json::Value::as_str)
        {
            Some(requested) => Some(requested.to_owned()),
            None => session_engine,
        };
        if !engine_id
            .as_deref()
            .is_some_and(magi_web_model::is_chatgpt_web_engine_id)
        {
            return Err(ApiError::InvalidInput(
                "只有选择 GPT Web 入口的会话才能设置对话方式".to_string(),
            ));
        }
        let current = session_web_conversation_binding(&self.settings_store, session_id);
        // 新建会话的首条请求在配置保存前已写入用户消息：它本身不算“已有历史”。
        let has_history = !is_new_session_first_request
            && self
                .session_store
                .session(session_id)
                .and_then(|session| session.message_count)
                .is_some_and(|count| count > 0);
        let wanted_saved = mode == "saved";
        let already_saved = current.mode == WebConversationMode::Saved;
        if wanted_saved == already_saved {
            return Ok(());
        }
        if has_history || current.remote_conversation_id.is_some() {
            return Err(ApiError::InvalidInput(
                "GPT Web 对话方式在首条消息后不能再更改".to_string(),
            ));
        }
        let binding = if wanted_saved {
            WebConversationBinding::pending_saved()
        } else {
            WebConversationBinding::temporary()
        };
        self.write_web_binding(session_id, &binding)
            .map_err(web_model_api_error)
    }

    /// 让应用级主页的 Surface 绑定与 Desktop 重新对齐（探测 / 恢复前调用；失败不阻断调用方，
    /// 由后续命令自己报告具体错误）。
    pub(crate) async fn resync_web_home_surface(&self) {
        if let Ok(driver) = self.web_driver()
            && let Err(error) = driver.refresh_surface(SLOT_PAGE_ID).await
        {
            tracing::debug!(%error, "GPT Web 主页 Surface 重新对齐未成功");
        }
    }

    /// 物化应用级主页（普通首页，非临时对话）：重启 / 停止后右栏重新显示 GPT Web 时使用。
    /// 槽位被会话占用时页面本来就在，不重复导航。
    pub(crate) async fn restore_web_home_page(&self) -> Result<(), WebModelError> {
        if self
            .web_model
            .bindings()
            .is_some_and(|slots| slots.owner().is_some())
        {
            return Ok(());
        }
        let driver = self.web_driver()?;
        self.open_home_with_recovery(&driver).await
    }

    /// 回到 ChatGPT 主页。停止 / 重启之后页面绑定常常比 Host 旧（`browser_surface_stale`）：
    /// 这类交接错误先重新对齐绑定再重试几次，而不是直接把内部错误码抛给用户。
    async fn open_home_with_recovery(
        &self,
        driver: &Arc<dyn magi_web_model::WebModelPageDriver>,
    ) -> Result<(), WebModelError> {
        let mut last = None;
        for attempt in 0..HOME_RECOVERY_ATTEMPTS {
            if attempt > 0 {
                self.resync_web_home_surface().await;
                tokio::time::sleep(HOME_RECOVERY_DELAY).await;
            }
            match driver.open_saved_chat(SLOT_PAGE_ID, None).await {
                Ok(()) => return Ok(()),
                Err(error) if error.message.contains("browser_surface") => {
                    tracing::warn!(attempt, error = %error.message, "回到 ChatGPT 主页遇到页面交接，将重新对齐后重试");
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.expect("at least one attempt ran"))
    }

    /// 会导航唯一 WebView 的操作（连接器、读取历史列表）：槽位被会话占用时拒绝，不接管、不打断。
    fn require_free_slot_for_connector(&self) -> Result<(), WebModelError> {
        if let Some(owner) = self.web_model.bindings().and_then(|slots| slots.owner()) {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSessionBusy,
                format!(
                    "GPT Web 正被会话 {} 占用，请先停止后再操作",
                    owner.session_id
                ),
            ));
        }
        Ok(())
    }

    /// GPT Web 标签页顶部的快捷地址：把唯一的 WebView 切到对话页或 OpenAI 平台的固定页面。
    /// 目标是白名单枚举，不接受任意地址；槽位被会话占用时拒绝，不打断进行中的对话。
    pub async fn open_web_model_page(
        &self,
        target: WebModelPageTarget,
    ) -> Result<(), WebModelError> {
        self.require_free_slot_for_connector()?;
        let driver = self.web_driver()?;
        match target {
            WebModelPageTarget::Chat => self.open_home_with_recovery(&driver).await,
            WebModelPageTarget::Tunnels => {
                driver
                    .open_page(SLOT_PAGE_ID, &magi_web_model::openai_platform_tunnels_url())
                    .await
            }
            WebModelPageTarget::ApiKeys => {
                driver
                    .open_page(
                        SLOT_PAGE_ID,
                        &magi_web_model::openai_platform_api_keys_url(),
                    )
                    .await
            }
        }
    }

    /// GPT Web 标签页的手动刷新：重新加载唯一 WebView 当前的页面。
    ///
    /// 不要求槽位空闲：刷新不换页面、不改变 Web 对话，页面卡住或加载失败时恰恰是占用中最需要它。
    /// 遇到页面交接（`browser_surface_*`）先重新对齐绑定再重试，和回到主页同一策略。
    pub async fn reload_web_model_page(&self) -> Result<(), WebModelError> {
        let driver = self.web_driver()?;
        let mut last = None;
        for attempt in 0..HOME_RECOVERY_ATTEMPTS {
            if attempt > 0 {
                self.resync_web_home_surface().await;
                tokio::time::sleep(HOME_RECOVERY_DELAY).await;
            }
            match driver.reload_page(SLOT_PAGE_ID).await {
                Ok(()) => return Ok(()),
                Err(error) if error.message.contains("browser_surface") => last = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last.expect("at least one attempt ran"))
    }

    /// 只读检查 ChatGPT 侧的 Magi 连接器。
    pub async fn web_connector_status(
        &self,
    ) -> Result<magi_web_model::ConnectorStatus, WebModelError> {
        self.require_free_slot_for_connector()?;
        // 页面可能停在 OpenAI 平台页或设置页：连接器检查从 ChatGPT 主页开始。
        let driver = self.web_driver()?;
        self.open_home_with_recovery(&driver).await?;
        driver
            .connector_status(SLOT_PAGE_ID, WEB_CONNECTOR_NAME)
            .await
    }

    /// 在 ChatGPT 里创建 / 启用 Magi 连接器并回读确认。这是**显式用户动作**（会修改用户的
    /// ChatGPT 连接器设置，只写连接器设置）；需要工具通道已配置 Tunnel id。
    pub async fn configure_web_connector(
        &self,
    ) -> Result<magi_web_model::ConnectorConfigOutcome, WebModelError> {
        self.require_free_slot_for_connector()?;
        let status = self.web_model.status();
        if status.tunnel_id.trim().is_empty() {
            return Err(WebModelError::new(
                WebModelErrorCode::WebTunnelUnavailable,
                "请先在设置里配置 OpenAI Tunnel id",
            ));
        }
        let driver = self.web_driver()?;
        self.open_home_with_recovery(&driver).await?;
        driver
            .configure_connector(SLOT_PAGE_ID, WEB_CONNECTOR_NAME, &status.tunnel_id)
            .await
    }

    /// 已保存对话列表（侧栏历史）。只读，不导航、不改变槽位。
    pub async fn list_web_saved_conversations(
        &self,
    ) -> Result<Vec<SavedConversationEntry>, WebModelError> {
        self.require_free_slot_for_connector()?;
        // 侧栏历史只在普通（非临时）主页上可读；槽位空闲时先回到主页再读。
        let driver = self.web_driver()?;
        self.open_home_with_recovery(&driver).await?;
        driver.list_saved_conversations(SLOT_PAGE_ID).await
    }

    fn import_exchanges(
        &self,
        session_id: &SessionId,
        exchanges: &[RemoteWebExchange],
    ) -> Result<Option<String>, WebModelError> {
        let workspace_id = self
            .session_store
            .session(session_id)
            .and_then(|session| self.session_workspace_id(&session));
        let mut last_remote_id = None;
        for exchange in exchanges {
            let seq = self
                .session_store
                .canonical_turns_for_session(session_id)
                .len() as u64
                + 1;
            let turn_id = format!(
                "turn-web-sync-{}-{}",
                exchange.last_remote_id.as_deref().unwrap_or("none"),
                seq
            );
            import_remote_web_exchange(
                &self.session_store,
                self.conversation_registry.turn_coordinator(),
                session_id,
                workspace_id.as_ref(),
                &turn_id,
                seq,
                exchange,
            )
            .map_err(|error| {
                WebModelError::new(WebModelErrorCode::WebSavedConversationUnavailable, error)
            })?;
            if exchange.last_remote_id.is_some() {
                last_remote_id = exchange.last_remote_id.clone();
            }
        }
        Ok(last_remote_id)
    }

    /// 把一条已保存的 ChatGPT 对话绑定到一个**空白**的 Magi 会话：读取远端标题与可见消息，
    /// 记录绑定，并把已有历史单向导入 canonical。Magi 记录名称以 Web 标题为准（W14）。
    ///
    /// 槽位必须空闲，或已经由该会话持有；其他情况一律 `web_session_busy`，不接管。
    pub async fn bind_saved_web_conversation(
        &self,
        session_id: &SessionId,
        conversation_id: &str,
    ) -> Result<WebConversationBinding, WebModelError> {
        let slots = self.web_model.bindings().ok_or_else(|| {
            WebModelError::new(
                WebModelErrorCode::WebDesktopUnavailable,
                "GPT Web 槽位未装配",
            )
        })?;
        if let Some(owner) = slots.owner()
            && owner.session_id != session_id.as_str()
        {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSessionBusy,
                format!("GPT Web 正被会话 {} 占用", owner.session_id),
            ));
        }
        let message_count = self
            .session_store
            .session(session_id)
            .and_then(|session| session.message_count)
            .unwrap_or(0);
        if message_count > 0 {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "只有空白会话可以绑定已保存的 GPT Web 对话",
            ));
        }
        let driver = self.web_driver()?;
        driver
            .open_saved_chat(SLOT_PAGE_ID, Some(conversation_id))
            .await?;
        let snapshot = driver
            .read_saved_conversation(SLOT_PAGE_ID, conversation_id)
            .await?;
        if !snapshot.exists {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "该已保存对话在 ChatGPT 侧不存在或账号不匹配",
            ));
        }
        let exchanges = pair_exchanges(&snapshot.messages);
        let last_synced = self.import_exchanges(session_id, &exchanges)?;
        let mut binding = WebConversationBinding::saved(conversation_id);
        binding.remote_title = snapshot.title.clone();
        binding.last_synced_remote_message_id = last_synced.or(snapshot.last_message_id.clone());
        binding.remote_updated_at = snapshot.remote_updated_at.clone();
        self.write_web_binding(session_id, &binding)?;
        if let Some(title) = snapshot
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
        {
            // 名称只接受 Web 标题；失败只记日志，不影响绑定。
            if let Err(error) = self.session_store.rename_session(session_id, title) {
                tracing::warn!(%session_id, %error, "已保存对话标题同步到会话名称失败");
            }
        }
        Ok(binding)
    }

    /// 发送前的已保存对话同步：把远端 Magi 尚未同步的尾部写入 canonical。
    ///
    /// - 远端对话不存在 → `web_saved_conversation_unavailable`（绑定标记失效）；
    /// - 同步指针对应的消息不在远端（分支 / 删除）→ `web_saved_conversation_conflict`，**不覆盖远端**。
    pub async fn sync_saved_web_conversation(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, WebModelError> {
        let mut binding = session_web_conversation_binding(&self.settings_store, session_id);
        if binding.mode != WebConversationMode::Saved {
            return Ok(0);
        }
        let Some(conversation_id) = binding.remote_conversation_id.clone() else {
            // 新建已保存对话：还没有远端 id，第一条消息被接受后才会拿到。
            return Ok(0);
        };
        // 槽位已由本会话持有时页面是活的，指针由 turn 完成回报推进，无需重新打开页面；
        // 被其他会话占用时不得借用唯一 WebView。
        if let Some(owner) = self.web_model.bindings().and_then(|slots| slots.owner()) {
            if owner.session_id == session_id.as_str() {
                return Ok(0);
            }
            return Err(WebModelError::new(
                WebModelErrorCode::WebSessionBusy,
                format!("GPT Web 正被会话 {} 占用", owner.session_id),
            ));
        }
        let driver = self.web_driver()?;
        driver
            .open_saved_chat(SLOT_PAGE_ID, Some(&conversation_id))
            .await?;
        let snapshot = driver
            .read_saved_conversation(SLOT_PAGE_ID, &conversation_id)
            .await?;
        if !snapshot.exists {
            binding.sync_state = WebConversationSyncState::Stale;
            self.write_web_binding(session_id, &binding)?;
            return Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationUnavailable,
                "已保存对话在 ChatGPT 侧不存在，绑定已失效",
            ));
        }
        let Some(tail) = unsynced_tail(
            &snapshot.messages,
            binding.last_synced_remote_message_id.as_deref(),
        ) else {
            binding.sync_state = WebConversationSyncState::Conflict;
            self.write_web_binding(session_id, &binding)?;
            return Err(WebModelError::new(
                WebModelErrorCode::WebSavedConversationConflict,
                "已保存对话在 Web 侧出现无法安全映射的改动，请在 ChatGPT 中处理后重新同步",
            ));
        };
        let exchanges = pair_exchanges(tail);
        let imported = exchanges.len();
        if let Some(last) = self.import_exchanges(session_id, &exchanges)? {
            binding.last_synced_remote_message_id = Some(last);
        }
        binding.remote_title = snapshot.title.clone().or(binding.remote_title);
        binding.remote_updated_at = snapshot.remote_updated_at.or(binding.remote_updated_at);
        binding.sync_state = WebConversationSyncState::Active;
        self.write_web_binding(session_id, &binding)?;
        Ok(imported)
    }

    /// 释放某会话持有的 GPT Web 槽位（切到本地 / 会话删除 / 停止）：销毁页面、清运行态。
    /// 会话不是槽位拥有者时什么都不做。返回是否真的释放了槽位。
    pub async fn release_web_slot_for_session(&self, session_id: &SessionId) -> bool {
        let Some(slots) = self.web_model.bindings() else {
            return false;
        };
        let Some(owner) = slots.owner() else {
            return false;
        };
        if owner.session_id != session_id.as_str() {
            return false;
        }
        self.close_web_page().await;
        let released = slots.release_session(session_id.as_str());
        if let Some(runtime) = self.web_model.runtime() {
            runtime.forget_session(session_id.as_str());
        }
        released
    }

    /// 设置里的“停止”/ Tab 里的“退出”：取消进行中的推理、销毁页面、释放槽位。登录态保留。
    pub async fn stop_web_model(&self) {
        let owner = self.web_model.bindings().and_then(|slots| slots.owner());
        if let Some(owner) = owner {
            let session_id = SessionId::new(owner.session_id.clone());
            // 先中断 Magi 侧 turn（它会取消网页生成并收口为取消），再释放。
            let _ = crate::routes::sessions::interrupt_session_turn_for_browser_takeover(
                self,
                &session_id,
                None,
            )
            .await;
            self.release_web_slot_for_session(&session_id).await;
        } else {
            self.close_web_page().await;
        }
    }

    /// 销毁应用级 GPT Web 的物理页面，逻辑 Tab 保留（再次打开会重新物化主页，不是重新登录）。
    async fn close_web_page(&self) {
        if let Ok(driver) = self.web_driver() {
            let _ = driver.cancel_generation(SLOT_PAGE_ID).await;
        }
        let Some(client) = self.browser_host_client() else {
            return;
        };
        let tab_id = BrowserTabId::new(WEB_MODEL_HOME_TAB_ID);
        match client
            .request(BrowserHostCommand::ClosePage {
                tab_id: tab_id.clone(),
            })
            .await
        {
            Ok(reply)
                if matches!(
                    reply.response.outcome,
                    BrowserHostCommandOutcome::Succeeded(_)
                ) => {}
            other => tracing::warn!(?other, "销毁 GPT Web 物理页面未得到成功确认"),
        }
        let tab_exists = self
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .tab(&tab_id)
            .is_some_and(|tab| tab.lifecycle == BrowserTabLifecycle::Ready);
        if tab_exists {
            // 逻辑 Tab 保留为挂起：再次打开只重新物化主页，登录态在应用级 Profile 中不受影响。
            let _ = self.mutate_browser_authority(|authority| {
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Suspended, UtcMillis::now())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str, id: &str) -> WebMessage {
        WebMessage {
            role: role.to_string(),
            text: text.to_string(),
            remote_id: Some(id.to_string()),
        }
    }

    #[test]
    fn exchanges_pair_each_user_message_with_the_following_answer() {
        let messages = vec![
            message("assistant", "孤立的开场白", "a0"),
            message("user", "问题一", "u1"),
            message("assistant", "回答一", "a1"),
            message("user", "问题二", "u2"),
            message("assistant", "回答二", "a2"),
            message("user", "还没有回复的问题", "u3"),
        ];
        let exchanges = pair_exchanges(&messages);
        assert_eq!(exchanges.len(), 2);
        assert_eq!(exchanges[0].user_text, "问题一");
        assert_eq!(exchanges[0].last_remote_id.as_deref(), Some("a1"));
        assert_eq!(exchanges[1].assistant_text, "回答二");
    }

    #[test]
    fn unsynced_tail_is_everything_after_the_pointer_and_none_when_the_pointer_vanished() {
        let messages = vec![
            message("user", "q1", "u1"),
            message("assistant", "a1", "a1"),
            message("user", "q2", "u2"),
            message("assistant", "a2", "a2"),
        ];
        assert_eq!(unsynced_tail(&messages, None).unwrap().len(), 4);
        assert_eq!(unsynced_tail(&messages, Some("a1")).unwrap().len(), 2);
        assert_eq!(unsynced_tail(&messages, Some("a2")).unwrap().len(), 0);
        assert!(
            unsynced_tail(&messages, Some("gone")).is_none(),
            "指针消息不在远端：分支或删除，必须报告冲突而不是猜测"
        );
    }
}
