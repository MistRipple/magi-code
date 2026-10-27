//! GPT Web 的宿主驱动与单槽位 client 工厂。

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use magi_bridge_client::llm_types::ImageSource;
use magi_browser_authority::{
    AcquireBrowserLease, BrowserAuthority, BrowserHostClient, BrowserHostClientError,
    BrowserHostCommand, BrowserHostCommandOutcome, BrowserHostCommandResult, BrowserHostControl,
    BrowserHostControlUpdate, BrowserLogicalViewport, BrowserNavigation, BrowserTabLifecycle,
    BrowserWebWriteMode,
};
use magi_core::{BrowserLeaseId, BrowserSessionId, BrowserTabId, ExecutionOwnership, UtcMillis};
use magi_web_model::{
    BrowserWebModelBridgeClient, ConnectorConfigOutcome, ConnectorStatus, DriverFuture, LoginState,
    SavedConversationEntry, SavedConversationSnapshot, SubmitOutcome, TurnState, WebMessage,
    WebModelClientConfig, WebModelError, WebModelErrorCode, WebModelIdentity, WebModelPageDriver,
    WebModelRuntimeRegistry, WebSlotOwner, WebSlotTable, WriteOutcome, new_web_slot_table,
};

const COMPOSER_SELECTOR_TOKEN: &str = "@composer";
const PAGE_READY_TIMEOUT: Duration = Duration::from_secs(60);
// 「停止并释放」之后页面被销毁，右栏要重新挂载 webview 才能再注册 Surface：这段窗口里的
// `browser_surface_*` 交接错误是暂时的，要等得够久（约 9–12 秒），不能几秒就当作发送失败。
const READ_STATE_RETRIES: usize = 30;
const READ_STATE_RETRY_DELAY: Duration = Duration::from_millis(300);
const NAVIGATE_RETRIES: usize = 30;
const NAVIGATE_RETRY_DELAY: Duration = Duration::from_millis(400);
const PAGE_READY_POLL: Duration = Duration::from_millis(250);
/// 刚打开主页时侧栏历史是异步渲染的；空结果多读几次，仍为空才当作真的没有。
const SAVED_LIST_ATTEMPTS: usize = 8;
const SAVED_LIST_RETRY_DELAY: Duration = Duration::from_millis(500);
/// 连接器页面（设置里的「插件」页、`/plugins` 目录页）加载与脚本就绪的等待。
const CONNECTOR_PAGE_ATTEMPTS: usize = 24;
const CONNECTOR_PAGE_DELAY: Duration = Duration::from_millis(700);

static WEB_SETUP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    static WEB_SETUP_TURN: String;
}

pub(crate) async fn with_web_setup_turn<T>(
    turn_id: String,
    future: impl std::future::Future<Output = T>,
) -> T {
    WEB_SETUP_TURN.scope(turn_id, future).await
}

pub struct HostWebModelPageDriver {
    host_client: Arc<RwLock<Option<BrowserHostClient>>>,
    authority: Arc<Mutex<BrowserAuthority>>,
    /// 本轮图片落盘的暂存目录：页面文件输入只认磁盘路径，图片在 Magi 里只有 base64。
    upload_dir: PathBuf,
}

impl HostWebModelPageDriver {
    pub fn new(
        host_client: Arc<RwLock<Option<BrowserHostClient>>>,
        authority: Arc<Mutex<BrowserAuthority>>,
        upload_dir: PathBuf,
    ) -> Self {
        // 进程崩溃可能留下上次未清理的暂存图片；目录只服务当前进程，启动时清空。
        sweep_upload_dir(&upload_dir);
        Self {
            host_client,
            authority,
            upload_dir,
        }
    }

    fn client(&self) -> Result<BrowserHostClient, WebModelError> {
        self.host_client
            .read()
            .ok()
            .and_then(|value| value.clone())
            .ok_or_else(|| {
                WebModelError::new(
                    WebModelErrorCode::WebDesktopUnavailable,
                    "Desktop 浏览器宿主未连接",
                )
            })
    }

    fn app_session(&self) -> Result<BrowserSessionId, WebModelError> {
        self.authority
            .lock()
            .map_err(|_| unavailable("浏览器权威状态不可用"))?
            .app_session_id()
            .cloned()
            .ok_or_else(|| {
                WebModelError::new(
                    WebModelErrorCode::WebDesktopUnavailable,
                    "应用级 GPT Web 浏览器会话尚未创建",
                )
            })
    }

    fn home_tab(&self) -> Result<BrowserTabId, WebModelError> {
        let authority = self
            .authority
            .lock()
            .map_err(|_| unavailable("浏览器权威状态不可用"))?;
        authority
            .app_session()
            .and_then(|session| session.tab_ids.first())
            .cloned()
            .ok_or_else(|| {
                WebModelError::new(
                    WebModelErrorCode::WebDesktopUnavailable,
                    "应用级 GPT Web 主页尚未创建",
                )
            })
    }

    fn page_state(&self, tab_id: &BrowserTabId) -> Result<(u64, u64, String), WebModelError> {
        let authority = self
            .authority
            .lock()
            .map_err(|_| unavailable("浏览器权威状态不可用"))?;
        let tab = authority.tab(tab_id).ok_or_else(|| {
            WebModelError::new(WebModelErrorCode::WebContextLost, "GPT Web 页面不存在")
        })?;
        Ok((
            tab.navigation_revision,
            tab.snapshot_revision,
            tab.url.clone(),
        ))
    }

    /// 物化唯一页面并保证它就在 `url` 上：返回 tab id。
    ///
    /// `RestorePage` 对已存活的页面只是重新登记，不会导航：页面还停在上一条对话（用户自己打开的
    /// `/c/<id>`、上一次的临时对话）时，直接读取会把旧页面当作新对话，被误判为上下文丢失。所以
    /// 恢复后页面 URL 不是目标时必须显式导航；目标是新对话入口时即使 URL 相同也要重新加载，
    /// 保证拿到的是空白新对话。
    async fn restore(&self, url: String) -> Result<BrowserTabId, WebModelError> {
        let was_alive = self
            .home_tab()
            .ok()
            .and_then(|tab| {
                self.authority
                    .lock()
                    .ok()
                    .and_then(|authority| authority.tab(&tab).map(|t| t.lifecycle))
            })
            .is_some_and(|lifecycle| lifecycle == BrowserTabLifecycle::Ready);
        let tab_id = self.restore_page(url.clone()).await?;
        // 页面是这次才物化的（冷启动 / 停止后重开）：它自己就在加载目标地址，再显式导航只会
        // 和它自己的首次加载互相取代。只有对已存活的页面才需要显式导航。
        if was_alive {
            self.navigate_to(&tab_id, &url).await?;
        }
        Ok(tab_id)
    }

    fn accept_binding(
        &self,
        binding: &magi_browser_authority::BrowserSurfaceBinding,
    ) -> Result<bool, WebModelError> {
        let mut authority = self
            .authority
            .lock()
            .map_err(|_| unavailable("浏览器权威状态不可用"))?;
        let (mut accepted, mut tab, _) = authority
            .accept_primary_surface(binding.clone(), UtcMillis::now())
            .map_err(|error| unavailable(&error.to_string()))?;
        if !accepted && binding.navigation_revision > tab.navigation_revision {
            // ChatGPT 是单页应用：页面内路由变化会不断推进 Host 的导航代次，page_updated 事件到达
            // Authority 之前绑定就可能已经领先。Host 是页面事实的来源，先吸收这一代次再接受绑定。
            let now = UtcMillis::now();
            let _ = authority.apply_host_page_state(
                &binding.tab_id,
                binding.navigation_revision,
                tab.url.clone(),
                tab.origin.clone(),
                tab.title.clone(),
                now,
            );
            let retried = authority
                .accept_primary_surface(binding.clone(), now)
                .map_err(|error| unavailable(&error.to_string()))?;
            accepted = retried.0;
            tab = retried.1;
        }
        if !accepted {
            let current = authority.primary_surface(&binding.tab_id).cloned();
            tracing::warn!(
                host_surface = %binding.surface_id,
                host_surface_revision = binding.surface_revision,
                host_web_contents_id = binding.web_contents_id,
                host_epoch = %binding.desktop_epoch,
                authority_surface = ?current.as_ref().map(|c| c.surface_id.clone()),
                authority_surface_revision = ?current.as_ref().map(|c| c.surface_revision),
                authority_web_contents_id = ?current.as_ref().map(|c| c.web_contents_id),
                authority_epoch = ?current.as_ref().map(|c| c.desktop_epoch.clone()),
                host_navigation_revision = binding.navigation_revision,
                authority_navigation_revision = tab.navigation_revision,
                authority_lifecycle = ?tab.lifecycle,
                "GPT Web 页面绑定未被 Authority 接受（导航代次不一致）"
            );
        }
        Ok(accepted)
    }

    /// 提交之后绑定交接 / 结果不确定：先把绑定对齐到 Host 最新页面，再把「已提交」交给上层，
    /// 由消息计数证据决定是否真的被站点接受。
    async fn submitted_maybe(&self, reason: &str) -> Result<SubmitOutcome, WebModelError> {
        if let Ok(tab_id) = self.home_tab() {
            let _ = self.refresh_primary_binding(&tab_id).await;
        }
        Ok(SubmitOutcome {
            submitted: true,
            composer_empty: true,
            reason: Some(reason.to_string()),
        })
    }

    /// 取 Host 最新的 Surface 绑定并让 Authority 接受：导航 / 页面交接之后，Authority 记录的
    /// 绑定会比 Host 旧，直接用旧绑定下命令会得到 `browser_surface_stale`。
    async fn refresh_primary_binding(
        &self,
        tab_id: &BrowserTabId,
    ) -> Result<magi_browser_authority::BrowserSurfaceBinding, WebModelError> {
        let ensure = self
            .client()?
            .request(BrowserHostCommand::EnsureSurface {
                tab_id: tab_id.clone(),
            })
            .await
            .map_err(host_error)?;
        let binding = match ensure.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::SurfaceBinding(binding) => binding,
                _ => return Err(invalid("EnsureSurface 结果缺少 Surface binding")),
            },
            BrowserHostCommandOutcome::Failed(error)
            | BrowserHostCommandOutcome::Indeterminate(error) => {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSendRejected,
                    error.message,
                ));
            }
            BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
        };
        if self.accept_binding(&binding)? {
            return Ok(binding);
        }
        // Authority 记录的导航代次与 Host 页面不一致（常见于 daemon 重启而 Desktop 页面还活着）：
        // 用当前代次重新登记一次（RestorePage 对存活页面只重新登记，不导航），再取一次绑定。
        let url = {
            let authority = self
                .authority
                .lock()
                .map_err(|_| unavailable("浏览器权威状态不可用"))?;
            authority
                .tab(tab_id)
                .map(|tab| tab.url.clone())
                .filter(|url| !url.trim().is_empty())
                .unwrap_or_else(magi_web_model::chatgpt_web_home_url)
        };
        self.restore_page(url).await?;
        let retry = self
            .client()?
            .request(BrowserHostCommand::EnsureSurface {
                tab_id: tab_id.clone(),
            })
            .await
            .map_err(host_error)?;
        let binding = match retry.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::SurfaceBinding(binding) => binding,
                _ => return Err(invalid("EnsureSurface 结果缺少 Surface binding")),
            },
            BrowserHostCommandOutcome::Failed(error)
            | BrowserHostCommandOutcome::Indeterminate(error) => {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSendRejected,
                    error.message,
                ));
            }
            BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
        };
        if !self.accept_binding(&binding)? {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSendRejected,
                "browser_surface_stale",
            ));
        }
        Ok(binding)
    }

    /// 显式导航到 `url`（等待加载）。需要用户控制栅栏：应用级页面没有 agent 占用，取当前 Surface
    /// 的用户控制即可，不会打断任何在途 turn（调用时槽位一定空闲或正由本 turn 持有）。
    ///
    /// Renderer 的 `<webview>` 在 restore 之后才注册 / 重新绑定，Authority 记录的 Surface 可能比
    /// Host 旧，所以每次先 `EnsureSurface` 取最新绑定并让 Authority 接受；Surface 交接窗口里的
    /// `browser_surface_stale` 属于导航前的只读失败，允许有限次重试（和探测同一策略）。
    async fn navigate_to(&self, tab_id: &BrowserTabId, url: &str) -> Result<(), WebModelError> {
        let mut last =
            WebModelError::new(WebModelErrorCode::WebSendRejected, "browser_surface_stale");
        for attempt in 0..=NAVIGATE_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(NAVIGATE_RETRY_DELAY).await;
            }
            match self.navigate_once(tab_id, url).await {
                Ok(()) => return Ok(()),
                Err(error) if error.is_cancelled() => return Err(error),
                // 另一次导航（页面自己的加载 / 重定向）取代了本次：目标页仍会加载，交给 wait_ready。
                Err(error) if error.message.contains("browser_navigation_superseded") => {
                    return Ok(());
                }
                Err(error) if error.message.contains("browser_surface") => last = error,
                Err(error) => return Err(error),
            }
        }
        Err(last)
    }

    async fn navigate_once(&self, tab_id: &BrowserTabId, url: &str) -> Result<(), WebModelError> {
        let client = self.client()?;
        let binding = self.refresh_primary_binding(tab_id).await?;
        let fence = {
            let mut authority = self
                .authority
                .lock()
                .map_err(|_| unavailable("浏览器权威状态不可用"))?;
            if authority.user_control_held(tab_id) {
                return Err(unavailable("browser_user_control_held"));
            }
            let (control, _revoked) = authority
                .take_user_control(tab_id, &binding.surface_id, UtcMillis::now())
                .map_err(|error| unavailable(&error.to_string()))?;
            control.fence
        };
        let reply = client
            .request(BrowserHostCommand::Navigate {
                tab_id: tab_id.clone(),
                control: BrowserHostControl::User { fence },
                navigation: BrowserNavigation::Url {
                    url: url.to_string(),
                    handle_before_unload: None,
                    init_script: None,
                    timeout_ms: Some(30_000),
                },
            })
            .await
            .map_err(host_error)?;
        match reply.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => {
                if let BrowserHostCommandResult::PageState(page) = *result
                    && let Ok(mut authority) = self.authority.lock()
                {
                    let _ = authority.apply_host_page_state(
                        tab_id,
                        page.navigation_revision,
                        page.url,
                        page.origin,
                        page.title,
                        UtcMillis::now(),
                    );
                }
                Ok(())
            }
            BrowserHostCommandOutcome::Failed(error)
            | BrowserHostCommandOutcome::Indeterminate(error) => Err(WebModelError::new(
                WebModelErrorCode::WebSendRejected,
                error.message,
            )),
            BrowserHostCommandOutcome::Cancelled => Err(WebModelError::cancelled()),
        }
    }

    async fn restore_page(&self, url: String) -> Result<BrowserTabId, WebModelError> {
        let client = self.client()?;
        let tab_id = self.home_tab()?;
        let (navigation_revision, snapshot_revision, _) = self.page_state(&tab_id)?;
        let reply = client
            .request(BrowserHostCommand::RestorePage {
                tab_id: tab_id.clone(),
                browser_session_id: self.app_session()?,
                initial_url: url,
                logical_viewport: BrowserLogicalViewport::Auto,
                navigation_revision,
                snapshot_revision,
                allow_page_eviction: false,
            })
            .await
            .map_err(host_error)?;
        match reply.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::PageState(page) => {
                    let mut authority = self
                        .authority
                        .lock()
                        .map_err(|_| unavailable("浏览器权威状态不可用"))?;
                    let _ = authority.transition_tab(
                        &tab_id,
                        BrowserTabLifecycle::Ready,
                        UtcMillis::now(),
                    );
                    let _ = authority.apply_host_page_state(
                        &tab_id,
                        page.navigation_revision,
                        page.url,
                        page.origin,
                        page.title,
                        UtcMillis::now(),
                    );
                }
                _ => return Err(invalid("物化页面返回结果无效")),
            },
            BrowserHostCommandOutcome::Failed(error)
            | BrowserHostCommandOutcome::Indeterminate(error) => {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSendRejected,
                    error.message,
                ));
            }
            BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
        }
        Ok(tab_id)
    }

    async fn read_state(&self, tab_id: &BrowserTabId) -> Result<TurnState, WebModelError> {
        // 回合状态是只读命令：页面刚导航 / 交接时 Surface 绑定会短暂过期，刷新绑定后有限次重试
        // （写入和提交路径不走这里，避免重放副作用）。
        let mut last = None;
        for attempt in 0..=READ_STATE_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(READ_STATE_RETRY_DELAY).await;
                if let Err(error) = self.refresh_primary_binding(tab_id).await {
                    last = Some(error);
                    continue;
                }
            }
            let reply = self
                .client()?
                .request(BrowserHostCommand::WebTurnState {
                    tab_id: tab_id.clone(),
                })
                .await
                .map_err(host_error)?;
            match reply.response.outcome {
                BrowserHostCommandOutcome::Succeeded(result) => {
                    return match *result {
                        BrowserHostCommandResult::Json { value } => state_from_json(&value),
                        _ => Err(invalid("回合状态结果无效")),
                    };
                }
                BrowserHostCommandOutcome::Failed(error)
                | BrowserHostCommandOutcome::Indeterminate(error) => {
                    let retryable = error.message.contains("browser_surface")
                        || error.message.contains("browser_cdp_session_stale")
                        || error.message.contains("browser_worker_rebind_stale")
                        || error.message.contains("browser_debugger_detached");
                    let mapped =
                        WebModelError::new(WebModelErrorCode::WebSendRejected, error.message);
                    if !retryable {
                        return Err(mapped);
                    }
                    last = Some(mapped);
                }
                BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
            }
        }
        Err(last.unwrap_or_else(|| invalid("回合状态读取失败")))
    }

    /// 发一条返回 JSON 的宿主命令；失败 / 取消统一映射为带指定错误码的 `WebModelError`。
    async fn json_command(
        &self,
        command: BrowserHostCommand,
        failure_code: WebModelErrorCode,
    ) -> Result<serde_json::Value, WebModelError> {
        let reply = self.client()?.request(command).await.map_err(host_error)?;
        match reply.response.outcome {
            BrowserHostCommandOutcome::Succeeded(result) => match *result {
                BrowserHostCommandResult::Json { value } => Ok(value),
                _ => Err(invalid("宿主命令结果不是 JSON")),
            },
            BrowserHostCommandOutcome::Failed(error)
            | BrowserHostCommandOutcome::Indeterminate(error) => {
                tracing::warn!(code = %error.code, "GPT Web 页面命令未确认");
                Err(WebModelError::new(failure_code, error.message))
            }
            BrowserHostCommandOutcome::Cancelled => Err(WebModelError::cancelled()),
        }
    }

    /// 应用级配置也沿 Authority lease → Main 控制投影 → Worker 原生输入链执行。
    /// 每个窄命令结算后释放控制，等待登录、密钥或确认时不会残留 Agent 光标。
    async fn native_setup_command(
        &self,
        build: impl Fn(BrowserTabId, BrowserHostControl) -> BrowserHostCommand,
    ) -> Result<serde_json::Value, WebModelError> {
        let mut last = unavailable("browser_surface_stale");
        for attempt in 0..CONNECTOR_PAGE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(CONNECTOR_PAGE_DELAY).await;
            }
            let (result, retry_safe) = self.native_setup_attempt(&build).await?;
            if !retry_safe {
                return result;
            }
            last = result.expect_err("retryable setup attempt failed before input");
        }
        Err(last)
    }

    async fn native_setup_attempt(
        &self,
        build: &impl Fn(BrowserTabId, BrowserHostControl) -> BrowserHostCommand,
    ) -> Result<(Result<serde_json::Value, WebModelError>, bool), WebModelError> {
        let tab_id = self.home_tab()?;
        // 取得控制权之前的 Surface 交接没有任何输入副作用，也必须进入有界重试。
        // 原先这里的 ? 直接退出，导致刚导航到插件列表时偶发要求用户手动重试。
        let binding = match self.refresh_primary_binding(&tab_id).await {
            Ok(binding) => binding,
            Err(error) if is_surface_handoff(&error.message) => return Ok((Err(error), true)),
            Err(error) => return Err(error),
        };
        let client = self.client()?;
        let now = UtcMillis::now();
        let lease = {
            let mut authority = self
                .authority
                .lock()
                .map_err(|_| unavailable("浏览器权威状态不可用"))?;
            authority
                .acquire_lease(AcquireBrowserLease {
                    lease_id: BrowserLeaseId::new(format!(
                        "web-setup-{}-{}",
                        now.0,
                        WEB_SETUP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                    )),
                    tab_id: tab_id.clone(),
                    surface_id: binding.surface_id.clone(),
                    // App owner 不归属会话；配置 Turn 仅用于关联本次执行。
                    owner: ExecutionOwnership::default(),
                    turn_id: WEB_SETUP_TURN.try_with(Clone::clone).unwrap_or_else(|_| {
                        format!(
                            "web-model-settings-{}-{}",
                            now.0,
                            WEB_SETUP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                        )
                    }),
                    goal_binding: None,
                    acquired_at: now,
                    expires_at: UtcMillis(now.0.saturating_add(120_000)),
                })
                .map_err(|error| unavailable(&error.to_string()))?
        };
        let result = async {
            let reply = client
                .request(BrowserHostCommand::UpdateControl {
                    tab_id: tab_id.clone(),
                    surface_id: binding.surface_id.clone(),
                    control: BrowserHostControlUpdate::Agent {
                        lease_id: lease.lease_id.clone(),
                        fence: lease.fence,
                    },
                })
                .await
                .map_err(host_error)?;
            if !matches!(
                reply.response.outcome,
                BrowserHostCommandOutcome::Succeeded(_)
            ) {
                if let BrowserHostCommandOutcome::Failed(error)
                | BrowserHostCommandOutcome::Indeterminate(error) = &reply.response.outcome
                {
                    tracing::warn!(code = %error.code, "GPT Web 配置控制权未取得");
                }
                let retry_safe = can_retry_setup_before_input(&reply.response.outcome);
                return Ok((Err(unavailable("配置页面控制权未取得")), retry_safe));
            }
            let reply = client
                .request(build(
                    tab_id.clone(),
                    BrowserHostControl::Agent {
                        lease_id: lease.lease_id.clone(),
                        fence: lease.fence,
                    },
                ))
                .await
                .map_err(host_error)?;
            let retry_safe = can_retry_setup_before_input(&reply.response.outcome);
            Ok(match reply.response.outcome {
                BrowserHostCommandOutcome::Succeeded(result) => (
                    match *result {
                        BrowserHostCommandResult::Json { value } => Ok(value),
                        _ => Err(invalid("宿主命令结果不是 JSON")),
                    },
                    false,
                ),
                BrowserHostCommandOutcome::Failed(error)
                | BrowserHostCommandOutcome::Indeterminate(error) => {
                    // 只有宿主明确证明尚未开始输入时，才重取租约；未知结果绝不重放。
                    // 进入详情页的导航交接是预期路由变化，后续回读会继续确认，
                    // 不应在诊断中伪装成要求用户点击“重试”的失败。
                    if error.code == "browser_navigation_superseded" {
                        tracing::info!(
                            code = %error.code,
                            retry_safe,
                            "GPT Web 配置页面导航交接，等待新页面回读",
                        );
                    } else {
                        tracing::warn!(code = %error.code, retry_safe, "GPT Web 配置页面交接");
                    }
                    (
                        Err(WebModelError::new(
                            WebModelErrorCode::WebTunnelUnavailable,
                            error.message,
                        )),
                        retry_safe,
                    )
                }
                BrowserHostCommandOutcome::Cancelled => (Err(WebModelError::cancelled()), false),
            })
        }
        .await;
        let release = {
            let mut authority = self
                .authority
                .lock()
                .map_err(|_| unavailable("浏览器权威状态不可用"))?;
            let _ = authority.release_lease(&lease.lease_id, UtcMillis::now());
            // 只清理本次物理 Surface；不能撤销另一控制者或新 Surface 的控制。
            authority
                .surface_control_snapshot(&tab_id, &binding.surface_id)
                .ok()
                .filter(|snapshot| snapshot.lease_id.is_none())
        };
        if let Some(snapshot) = release {
            let _ = client
                .request(BrowserHostCommand::UpdateControl {
                    tab_id,
                    surface_id: binding.surface_id,
                    control: BrowserHostControlUpdate::Released {
                        fence: snapshot.fence,
                    },
                })
                .await;
        }
        result
    }

    /// 只读页面命令（已保存对话、连接器状态）：页面刚导航 / 交接时 Surface 绑定会短暂过期，
    /// 刷新绑定后有限次重试。写入类命令（配置连接器、提交）不走这里，避免重放副作用。
    async fn json_read_command(
        &self,
        command: BrowserHostCommand,
        failure_code: WebModelErrorCode,
    ) -> Result<serde_json::Value, WebModelError> {
        let mut last = None;
        for attempt in 0..=READ_STATE_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(READ_STATE_RETRY_DELAY).await;
                if let Ok(tab_id) = self.home_tab()
                    && let Err(refresh) = self.refresh_primary_binding(&tab_id).await
                {
                    tracing::warn!(attempt, error = %refresh.message, "只读页面命令重试前重新对齐绑定失败");
                }
            }
            match self.json_command(command.clone(), failure_code).await {
                Ok(value) => return Ok(value),
                Err(error) if is_surface_handoff(&error.message) => {
                    tracing::warn!(attempt, error = %error.message, "只读页面命令遇到页面交接，将重试");
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.expect("at least one attempt ran"))
    }

    async fn wait_ready(&self, tab_id: &BrowserTabId) -> Result<(), WebModelError> {
        let deadline = tokio::time::Instant::now() + PAGE_READY_TIMEOUT;
        loop {
            let state = match self.read_state(tab_id).await {
                Ok(state) => state,
                // 页面还停在 about:blank / 正在跳转到 ChatGPT：origin 暂时不是受支持站点，继续等待。
                Err(error)
                    if error.message.contains("不是受支持的 ChatGPT Web origin")
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(PAGE_READY_POLL).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if state.blocked {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSiteBlocked,
                    "ChatGPT Web 当前处于风险或验证页面",
                ));
            }
            if state.login_state == LoginState::SignedOut && state.composer_found {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebLoginExpired,
                    "ChatGPT Web 尚未登录或登录已过期",
                ));
            }
            if state.composer_found {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSelectorsDrift,
                    "GPT Web 页面在时限内未就绪",
                ));
            }
            tokio::time::sleep(PAGE_READY_POLL).await;
        }
    }
}

impl WebModelPageDriver for HostWebModelPageDriver {
    fn open_temporary_chat<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let tab = self
                .restore(magi_web_model::chatgpt_web_temporary_chat_url())
                .await?;
            self.wait_ready(&tab).await
        })
    }

    fn read_image_chunk<'a>(
        &'a self,
        _page_id: &'a str,
        source: &'a str,
        offset: u64,
        length: u64,
    ) -> DriverFuture<'a, magi_web_model::ImageChunk> {
        Box::pin(async move {
            use base64::Engine as _;
            let value = self
                .json_read_command(
                    BrowserHostCommand::WebReadImage {
                        tab_id: self.home_tab()?,
                        source: source.to_string(),
                        offset,
                        length,
                    },
                    WebModelErrorCode::WebSendRejected,
                )
                .await?;
            let field = |name: &str| value.get(name);
            let encoded = field("data_base64")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("图片分块缺少 data_base64"))?;
            let data = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| invalid("图片分块不是有效的 base64"))?;
            Ok(magi_web_model::ImageChunk {
                mime: field("mime")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                total: field("total")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| invalid("图片分块缺少 total"))?,
                offset: field("offset")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| invalid("图片分块缺少 offset"))?,
                data,
                done: field("done")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
    }

    fn reload_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            // 权威状态里的地址只保留可恢复的 http(s) 页面；加载失败的内部错误页会被归一成
            // about:blank，这种情况回到 ChatGPT 主页，而不是刷新一个空白页。
            let url = {
                let tab = self.home_tab()?;
                let (_, _, url) = self.page_state(&tab)?;
                if url.starts_with("http://") || url.starts_with("https://") {
                    url
                } else {
                    magi_web_model::chatgpt_web_home_url()
                }
            };
            self.restore(url).await.map(|_| ())
        })
    }

    fn open_page<'a>(&'a self, _page_id: &'a str, url: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            // 配置页交接后保留用户选择和已准备的表单，不因同地址继续而重载。
            if url == magi_web_model::openai_platform_tunnels_url()
                || url == magi_web_model::openai_platform_api_keys_url()
            {
                let tab = self.restore_page(url.to_string()).await?;
                if self.page_state(&tab)?.2 != url {
                    self.navigate_to(&tab, url).await?;
                }
                return Ok(());
            }
            self.restore(url.to_string()).await.map(|_| ())
        })
    }

    fn probe_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, magi_web_model::PageProbe> {
        Box::pin(async move {
            let value = self
                .json_read_command(
                    BrowserHostCommand::WebModelProbe {
                        tab_id: self.home_tab()?,
                    },
                    WebModelErrorCode::WebSendRejected,
                )
                .await?;
            let login_state = match value.get("login_state").and_then(serde_json::Value::as_str) {
                Some("signed_in") => LoginState::SignedIn,
                Some("signed_out") => LoginState::SignedOut,
                Some("blocked") => LoginState::Blocked,
                _ => return Err(invalid("页面登录探测结果无效")),
            };
            let composer_available = value
                .get("composer_available")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| invalid("页面输入框探测结果无效"))?;
            Ok(magi_web_model::PageProbe {
                login_state,
                composer_available,
            })
        })
    }

    fn open_saved_chat<'a>(
        &'a self,
        _page_id: &'a str,
        conversation_id: Option<&'a str>,
    ) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            // 没有 conversation_id = 新建已保存对话：打开普通（非临时）首页，ChatGPT 在第一条消息被
            // 接受后才把地址变成 `/c/<id>`，远端 id 由 turn 完成后的回读取得。
            let url = match conversation_id {
                Some(id) => format!("{}/c/{}", magi_web_model::chatgpt_web_origin(), id),
                None => magi_web_model::chatgpt_web_home_url(),
            };
            let tab = self.restore(url).await?;
            self.wait_ready(&tab).await
        })
    }

    fn refresh_surface<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let tab = self.home_tab()?;
            self.refresh_primary_binding(&tab).await.map(|_| ())
        })
    }

    fn resume_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, bool> {
        Box::pin(async move {
            let tab = match self.home_tab() {
                Ok(tab) => tab,
                Err(_) => return Ok(false),
            };
            match self.wait_ready(&tab).await {
                Ok(()) => Ok(true),
                Err(WebModelError {
                    code: WebModelErrorCode::WebLoginExpired,
                    ..
                }) => Ok(false),
                Err(error) => Err(error),
            }
        })
    }

    fn close_page<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        // Tab 关闭只隐藏；真正停止由 API/设置路径释放宿主。不要在 client turn 收口时
        // 销毁应用级 WebView。
        Box::pin(async { Ok(()) })
    }

    fn write_text<'a>(
        &'a self,
        _page_id: &'a str,
        text: &'a str,
    ) -> DriverFuture<'a, WriteOutcome> {
        Box::pin(async move {
            let reply = self
                .client()?
                .request(BrowserHostCommand::WebWriteText {
                    tab_id: self.home_tab()?,
                    selector: COMPOSER_SELECTOR_TOKEN.to_string(),
                    text: text.to_string(),
                    mode: BrowserWebWriteMode::Replace,
                    expect_text_digest: None,
                    timeout_ms: None,
                })
                .await
                .map_err(host_error)?;
            let value = match reply.response.outcome {
                BrowserHostCommandOutcome::Succeeded(result) => match *result {
                    BrowserHostCommandResult::Json { value } => value,
                    _ => return Err(invalid("写入结果无效")),
                },
                BrowserHostCommandOutcome::Failed(error)
                | BrowserHostCommandOutcome::Indeterminate(error) => {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebWriteNotConfirmed,
                        error.message,
                    ));
                }
                BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
            };
            Ok(WriteOutcome {
                confirmed: value
                    .get("confirmed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                char_count: value
                    .get("char_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
                became_attachment: value
                    .get("became_attachment")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
    }

    fn attach_images<'a>(
        &'a self,
        _page_id: &'a str,
        images: &'a [ImageSource],
    ) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let tab_id = self.home_tab()?;
            let staged = StagedUploads::stage(&self.upload_dir, images)?;
            // 暂存文件在命令返回（成败皆然）后立即删除：页面已把内容读入上传请求。
            let reply = self
                .client()?
                .request(BrowserHostCommand::WebAttachFiles {
                    tab_id,
                    file_paths: staged.paths(),
                })
                .await;
            let error = match reply {
                Ok(reply) => match reply.response.outcome {
                    BrowserHostCommandOutcome::Succeeded(result) => {
                        if let BrowserHostCommandResult::Json { value } = *result
                            && value.get("attached").and_then(serde_json::Value::as_u64)
                                == Some(images.len() as u64)
                        {
                            return Ok(());
                        }
                        WebModelError::new(WebModelErrorCode::WebAttachFailed, "附图结果未确认")
                    }
                    // Worker 明确失败前已完成回滚，或根本没设置文件。
                    BrowserHostCommandOutcome::Failed(error) => {
                        return Err(WebModelError::new(
                            WebModelErrorCode::WebAttachFailed,
                            error.message,
                        ));
                    }
                    BrowserHostCommandOutcome::Indeterminate(error) => {
                        WebModelError::new(WebModelErrorCode::WebAttachFailed, error.message)
                    }
                    BrowserHostCommandOutcome::Cancelled => WebModelError::cancelled(),
                },
                Err(error) => host_error(error),
            };
            // 当前 Worker 请求已取消，必须用新请求清理。页面适配器只允许撤销 Magi 自己的附件。
            if let Err(cleanup) = self.clear_attachments(_page_id).await {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebSelectorsDrift,
                    format!(
                        "图片上传结果未确认且撤销失败：{}；{}",
                        error.message, cleanup.message
                    ),
                ));
            }
            Err(error)
        })
    }

    fn clear_attachments<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let value = self
                .json_command(
                    BrowserHostCommand::WebClearAttachments {
                        tab_id: self.home_tab()?,
                    },
                    WebModelErrorCode::WebAttachFailed,
                )
                .await?;
            if value.get("remaining").and_then(serde_json::Value::as_u64) != Some(0) {
                return Err(WebModelError::new(
                    WebModelErrorCode::WebAttachFailed,
                    "网页附件清理未确认，未继续提交",
                ));
            }
            Ok(())
        })
    }

    fn answer_question<'a>(
        &'a self,
        _page_id: &'a str,
        question_id: &'a str,
        answer: &'a magi_web_model::WebQuestionAnswer,
    ) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            self.json_command(
                BrowserHostCommand::WebAnswerQuestion {
                    tab_id: self.home_tab()?,
                    question_id: question_id.to_string(),
                    answer: answer.clone(),
                },
                WebModelErrorCode::WebSelectorsDrift,
            )
            .await
            .map(|_| ())
        })
    }

    fn submit<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, SubmitOutcome> {
        Box::pin(async move {
            let reply = self
                .client()?
                .request(BrowserHostCommand::WebSubmit {
                    tab_id: self.home_tab()?,
                })
                .await
                .map_err(host_error)?;
            let value = match reply.response.outcome {
                BrowserHostCommandOutcome::Succeeded(result) => match *result {
                    BrowserHostCommandResult::Json { value } => value,
                    _ => return Err(invalid("提交结果无效")),
                },
                // 点击提交之后，ChatGPT 会立刻做页面内路由（新对话变成 `/c/<id>`）：导航代次随之推进，
                // Worker 校验绑定时报 `browser_surface_stale`，但点击已经生效，消息已经发出去了。
                // 结果不确定（Indeterminate）同理。这两种情况都不能报告「未被接受、可以重试」——
                // 重新对齐绑定后，是否被接受由 `turn_state` 的消息计数证据判定（接受窗口内始终没有
                // 新增用户消息才会报 `web_send_rejected`）。
                BrowserHostCommandOutcome::Indeterminate(_) => {
                    return self.submitted_maybe("indeterminate").await;
                }
                BrowserHostCommandOutcome::Failed(error) if is_surface_handoff(&error.message) => {
                    return self.submitted_maybe("surface_changed_during_submit").await;
                }
                BrowserHostCommandOutcome::Failed(error) => {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebSendRejected,
                        error.message,
                    ));
                }
                BrowserHostCommandOutcome::Cancelled => return Err(WebModelError::cancelled()),
            };
            Ok(SubmitOutcome {
                submitted: value
                    .get("submitted")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                composer_empty: value
                    .get("composer_empty")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                reason: value
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            })
        })
    }

    fn cancel_generation<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, ()> {
        Box::pin(async move {
            let reply = self
                .client()?
                .request(BrowserHostCommand::WebCancelGeneration {
                    tab_id: self.home_tab()?,
                })
                .await
                .map_err(host_error)?;
            match reply.response.outcome {
                BrowserHostCommandOutcome::Succeeded(_) => Ok(()),
                BrowserHostCommandOutcome::Failed(error)
                | BrowserHostCommandOutcome::Indeterminate(error) => Err(WebModelError::new(
                    WebModelErrorCode::WebSendRejected,
                    error.message,
                )),
                BrowserHostCommandOutcome::Cancelled => Err(WebModelError::cancelled()),
            }
        })
    }

    fn turn_state<'a>(&'a self, _page_id: &'a str) -> DriverFuture<'a, TurnState> {
        Box::pin(async move { self.read_state(&self.home_tab()?).await })
    }

    fn read_last_message<'a>(&'a self, page_id: &'a str) -> DriverFuture<'a, Option<WebMessage>> {
        Box::pin(async move {
            let state = self.turn_state(page_id).await?;
            Ok(state
                .last_message_role
                .zip(state.last_message_text)
                .map(|(role, text)| WebMessage {
                    role,
                    text,
                    remote_id: None,
                }))
        })
    }

    fn read_saved_conversation<'a>(
        &'a self,
        _page_id: &'a str,
        conversation_id: &'a str,
    ) -> DriverFuture<'a, SavedConversationSnapshot> {
        Box::pin(async move {
            let value = self
                .json_read_command(
                    BrowserHostCommand::WebSavedMessages {
                        tab_id: self.home_tab()?,
                    },
                    WebModelErrorCode::WebSavedConversationUnavailable,
                )
                .await?;
            let page_id = value
                .get("conversation_id")
                .and_then(serde_json::Value::as_str);
            // 页面不是请求的那条已保存对话：按“远端不存在或无法绑定”处理，不猜测。
            // `conversation_id` 为空表示“读取页面当前所在的已保存对话”（新建已保存对话的首条
            // 消息被接受后用来取得远端 id）。
            let exists = match page_id {
                Some(actual) => conversation_id.is_empty() || actual == conversation_id,
                None => false,
            };
            let messages = value
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            Some(WebMessage {
                                role: item.get("role")?.as_str()?.to_string(),
                                text: item.get("text")?.as_str()?.to_string(),
                                remote_id: item
                                    .get("remote_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(SavedConversationSnapshot {
                conversation_id: page_id.unwrap_or(conversation_id).to_string(),
                title: value
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                last_message_id: value
                    .get("last_message_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                remote_updated_at: value
                    .get("updated_at")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                messages,
                exists,
            })
        })
    }

    fn list_saved_conversations<'a>(
        &'a self,
        _page_id: &'a str,
    ) -> DriverFuture<'a, Vec<SavedConversationEntry>> {
        Box::pin(async move {
            let mut entries = Vec::new();
            for attempt in 0..SAVED_LIST_ATTEMPTS {
                if attempt > 0 {
                    tokio::time::sleep(SAVED_LIST_RETRY_DELAY).await;
                }
                let value = self
                    .json_read_command(
                        BrowserHostCommand::WebSavedConversations {
                            tab_id: self.home_tab()?,
                        },
                        WebModelErrorCode::WebSavedConversationUnavailable,
                    )
                    .await?;
                entries = value
                    .get("conversations")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                Some(SavedConversationEntry {
                                    conversation_id: item
                                        .get("conversation_id")?
                                        .as_str()?
                                        .to_string(),
                                    title: item.get("title")?.as_str()?.to_string(),
                                    updated_at: item
                                        .get("updated_at")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_string),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !entries.is_empty() {
                    break;
                }
            }
            Ok(entries)
        })
    }

    fn platform_setup<'a>(&'a self, action: &'a str) -> DriverFuture<'a, serde_json::Value> {
        Box::pin(async move {
            self.native_setup_command(|tab_id, control| BrowserHostCommand::WebPlatformSetup {
                tab_id,
                control,
                action: action.to_string(),
            })
            .await
        })
    }

    fn connector_status<'a>(
        &'a self,
        _page_id: &'a str,
        name: &'a str,
    ) -> DriverFuture<'a, ConnectorStatus> {
        Box::pin(async move {
            let status = self.read_connector_list(name).await;
            if status
                .as_ref()
                .is_ok_and(|value| value.supported && value.exists && value.enabled)
            {
                self.leave_connector_pages().await;
            }
            status
        })
    }

    fn configure_connector<'a>(
        &'a self,
        _page_id: &'a str,
        name: &'a str,
        tunnel_id: &'a str,
    ) -> DriverFuture<'a, ConnectorConfigOutcome> {
        Box::pin(async move {
            let outcome = self.create_connector(name, tunnel_id).await;
            if outcome
                .as_ref()
                .is_ok_and(|value| value.configured && value.confirmed_enabled)
            {
                self.leave_connector_pages().await;
            }
            outcome
        })
    }
}

impl HostWebModelPageDriver {
    /// 打开设置里的「插件」页并读取已安装列表。页面脚本不做路由跳转（跳转会让 Desktop 把命令结果
    /// 判为过期），所以由这里用 Host 的导航命令去到该页面，等脚本报告页面已就绪再读。
    async fn read_connector_list(&self, name: &str) -> Result<ConnectorStatus, WebModelError> {
        self.restore(connector_settings_url()).await?;
        let mut parsed = None;
        for attempt in 0..CONNECTOR_PAGE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(CONNECTOR_PAGE_DELAY).await;
            }
            let value = self
                .json_read_command(
                    BrowserHostCommand::WebConnectorStatus {
                        tab_id: self.home_tab()?,
                        name: name.to_string(),
                    },
                    WebModelErrorCode::WebTunnelUnavailable,
                )
                .await;
            let status = match value {
                Ok(value) => connector_status_from_json(&value),
                // 页面还在加载 / 交接：和「页面未就绪」一样继续等。
                Err(error) if is_surface_handoff(&error.message) => continue,
                Err(error) => return Err(error),
            };
            tracing::info!(
                attempt,
                supported = status.supported,
                exists = status.exists,
                enabled = status.enabled,
                reason = crate::web_model_setup::connector_failure_reason(status.reason.as_deref())
                    .unwrap_or("none"),
                "GPT Web 连接器列表回读"
            );
            if status.reason.as_deref() == Some("connector_page_not_open") {
                parsed = Some(status);
                continue;
            }
            return Ok(status);
        }
        Ok(parsed.unwrap_or_else(|| ConnectorStatus {
            supported: false,
            exists: false,
            enabled: false,
            tool_count: None,
            reason: Some("connector_page_not_open".to_string()),
        }))
    }

    /// 创建连接器：先读已安装列表（已存在就不重复创建），再去目录页创建，最后回读列表确认。
    async fn create_connector(
        &self,
        name: &str,
        tunnel_id: &str,
    ) -> Result<ConnectorConfigOutcome, WebModelError> {
        let before = self.read_connector_list(name).await?;
        if !before.supported {
            return Ok(ConnectorConfigOutcome {
                configured: false,
                confirmed_enabled: false,
                reason: before.reason,
            });
        }
        if before.exists {
            // 连接器已经存在：不重复创建，而是让 ChatGPT 重新拉取 Magi 当前的工具列表
            //（ChatGPT 缓存创建 / 上次刷新时的列表，Magi 目录变化后不会自动同步）。
            // 上面的回读已进入列表页；打开详情与刷新工具分成两个命令，
            // 导航交接不会重放刷新副作用。
            return self.refresh_connector_tools(name, tunnel_id).await;
        }
        self.restore(connector_directory_url()).await?;
        let mut created_reason = None;
        let mut connection_confirmed = false;
        for attempt in 0..CONNECTOR_PAGE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(CONNECTOR_PAGE_DELAY).await;
            }
            let value = self
                .native_setup_command(
                    |tab_id, control| BrowserHostCommand::WebConfigureConnector {
                        tab_id,
                        control,
                        name: name.to_string(),
                        tunnel_id: tunnel_id.to_string(),
                    },
                )
                .await;
            match value {
                Ok(value) => {
                    let reason = value
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    connection_confirmed = value
                        .get("confirmed_enabled")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                        && reason.as_deref() == Some("connection_confirmed");
                    // 页面还没落在目录页：脚本什么都没做，可以安全重试。
                    if reason.as_deref() == Some("connector_page_not_open") {
                        created_reason = reason;
                        continue;
                    }
                    created_reason = reason;
                    break;
                }
                // 点了“创建”之后页面会跳到新应用页：命令结果因导航而过期，但创建可能已经生效，
                // 是否成功只看下面回读的已安装列表。
                Err(error) if is_surface_handoff(&error.message) => break,
                Err(error) => return Err(error),
            }
        }
        // 明确的页面阻断保持在原表单，供用户接手；不能导航抹掉现场。
        if let Some(reason) = created_reason.filter(|reason| {
            reason != "connector_page_not_open" && reason != "connection_confirmed"
        }) {
            return Ok(ConnectorConfigOutcome {
                configured: false,
                confirmed_enabled: false,
                reason: Some(reason),
            });
        }
        let after = self.read_connector_list(name).await?;
        Ok(ConnectorConfigOutcome {
            configured: after.exists,
            confirmed_enabled: after.exists && (after.enabled || connection_confirmed),
            reason: if after.exists {
                (!after.enabled && !connection_confirmed)
                    .then(|| "connector_not_enabled_after_create".to_string())
            } else {
                Some("connector_not_listed_after_create".to_string())
            },
        })
    }

    /// 在已安装连接器的详情页点「Refresh tools」。成功时 `reason` 为 `tools_refreshed`。
    async fn refresh_connector_tools(
        &self,
        name: &str,
        tunnel_id: &str,
    ) -> Result<ConnectorConfigOutcome, WebModelError> {
        let mut last_reason = None;
        for attempt in 0..CONNECTOR_PAGE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(CONNECTOR_PAGE_DELAY).await;
            }
            // 列表上的脚本仅打开详情，不能执行刷新。只有这段导航可以安全自动接续；
            // 已在详情里开始刷新/关联后的不确定结果不能重放。
            let opening_detail = self.page_state(&self.home_tab()?)?.2.trim_end_matches('/')
                == connector_settings_url();
            let value = self
                .native_setup_command(
                    |tab_id, control| BrowserHostCommand::WebConfigureConnector {
                        tab_id,
                        control,
                        name: name.to_string(),
                        tunnel_id: tunnel_id.to_string(),
                    },
                )
                .await;
            let value = match value {
                Ok(value) => value,
                // 点进连接器详情页会换路由，命令结果因此过期；页面已在详情页，重试即可。
                Err(error) if opening_detail && is_surface_handoff(&error.message) => {
                    last_reason = Some("connector_detail_opening".to_string());
                    continue;
                }
                Err(error) => return Err(error),
            };
            let reason = value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            tracing::info!(
                attempt,
                reason = crate::web_model_setup::connector_failure_reason(reason.as_deref())
                    .unwrap_or("none"),
                confirmed = matches!(
                    reason.as_deref(),
                    Some("tools_refreshed" | "connection_confirmed")
                ),
                "GPT Web 连接器刷新回读"
            );
            // 页面还没落在设置页，或列表还没渲染：脚本什么都没做，可以安全重试。
            if matches!(
                reason.as_deref(),
                Some("connector_page_not_open")
                    | Some("connector_not_listed")
                    | Some("connector_detail_opening")
            ) {
                last_reason = reason;
                continue;
            }
            let refreshed = matches!(
                reason.as_deref(),
                Some("tools_refreshed" | "connection_confirmed")
            );
            return Ok(ConnectorConfigOutcome {
                configured: true,
                confirmed_enabled: refreshed,
                reason,
            });
        }
        Ok(ConnectorConfigOutcome {
            configured: true,
            confirmed_enabled: false,
            reason: last_reason.or_else(|| Some("connector_refresh_failed".to_string())),
        })
    }

    /// 连接器操作结束后回到 ChatGPT 主页（尽力而为，不等待）。
    async fn leave_connector_pages(&self) {
        if let Err(error) = self.restore(magi_web_model::chatgpt_web_home_url()).await {
            tracing::debug!(error = %error.message, "连接器操作后回到主页未成功");
        }
    }
}

fn connector_settings_url() -> String {
    format!(
        "{}/settings/plugins-settings",
        magi_web_model::chatgpt_web_origin()
    )
}

fn connector_directory_url() -> String {
    format!("{}/plugins", magi_web_model::chatgpt_web_origin())
}

fn connector_status_from_json(value: &serde_json::Value) -> ConnectorStatus {
    ConnectorStatus {
        supported: value
            .get("supported")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        exists: value
            .get("exists")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        enabled: value
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        tool_count: value.get("tool_count").and_then(serde_json::Value::as_u64),
        reason: value
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    }
}

pub struct WebModelHostFactory {
    driver: Arc<dyn WebModelPageDriver>,
    slots: Arc<WebSlotTable>,
    runtime: Arc<WebModelRuntimeRegistry>,
    plugin_manager: Arc<std::sync::Mutex<magi_plugin_system::PluginManager>>,
    saved_sink: Option<Arc<dyn magi_web_model::SavedConversationSink>>,
    image_sink: Option<Arc<dyn magi_web_model::WebImageSink>>,
    question_sink: Option<Arc<dyn magi_web_model::WebQuestionSink>>,
}

impl WebModelHostFactory {
    pub fn new(
        host_client: Arc<RwLock<Option<BrowserHostClient>>>,
        authority: Arc<Mutex<BrowserAuthority>>,
        plugin_manager: Arc<std::sync::Mutex<magi_plugin_system::PluginManager>>,
        upload_dir: PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            driver: Arc::new(HostWebModelPageDriver::new(
                host_client,
                authority,
                upload_dir,
            )),
            slots: new_web_slot_table(),
            runtime: Arc::new(WebModelRuntimeRegistry::new()),
            plugin_manager,
            saved_sink: None,
            image_sink: None,
            question_sink: None,
        })
    }
    /// 已保存对话进度的唯一写入方（daemon 装配）。
    pub fn with_saved_sink(mut self, sink: Arc<dyn magi_web_model::SavedConversationSink>) -> Self {
        self.saved_sink = Some(sink);
        self
    }
    /// 回复里的图片落地（保存到会话所属项目）。
    pub fn with_image_sink(mut self, sink: Arc<dyn magi_web_model::WebImageSink>) -> Self {
        self.image_sink = Some(sink);
        self
    }
    pub fn with_question_sink(mut self, sink: Arc<dyn magi_web_model::WebQuestionSink>) -> Self {
        self.question_sink = Some(sink);
        self
    }
    pub fn slots(&self) -> &Arc<WebSlotTable> {
        &self.slots
    }
    /// 宿主驱动：已保存对话列表 / 删除、连接器配置与推理通道共用同一份。
    pub fn driver(&self) -> &Arc<dyn WebModelPageDriver> {
        &self.driver
    }
    pub fn runtime(&self) -> &Arc<WebModelRuntimeRegistry> {
        &self.runtime
    }
    /// 旧装配入口改名后的语义：返回应用级单槽位，而不是会话绑定表。
    pub fn bindings(&self) -> &Arc<WebSlotTable> {
        &self.slots
    }
}

impl WebModelHostFactory {
    fn build_web_model_client(
        &self,
        spec: magi_plugin_system::SessionEngineInvocationSpec,
    ) -> Result<Arc<dyn magi_bridge_client::ModelBridgeClient>, String> {
        let binding: magi_web_model::WebConversationBinding = serde_json::from_value(spec.binding)
            .map_err(|_| "GPT Web 会话绑定不符合引擎合同".to_string())?;
        if !magi_web_model::is_chatgpt_web_engine_id(&spec.engine_id) {
            return Err("GPT Web 引擎 id 必须位于 GPT Web 插件命名空间".to_string());
        }
        let scope = magi_plugin_system::APPLICATION_SCOPE;
        let plugin_manager = self
            .plugin_manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        if !plugin_manager
            .is_active_for_scope(magi_web_model::WEB_MODEL_PLUGIN_ID, scope)
            .map_err(|error| error.to_string())?
        {
            return Err("GPT Web 插件未在当前作用域激活".to_string());
        }
        for (kind, target, denied) in [
            (
                magi_app_server_protocol::PluginPermissionKind::Models,
                "default",
                "GPT Web 插件未获模型权限",
            ),
            (
                magi_app_server_protocol::PluginPermissionKind::Browser,
                "gpt-web",
                "GPT Web 插件未获浏览器权限",
            ),
        ] {
            if !plugin_manager
                .permission_allowed(magi_web_model::WEB_MODEL_PLUGIN_ID, scope, kind, target)
                .map_err(|error| error.to_string())?
            {
                return Err(denied.to_string());
            }
        }
        let identity = WebModelIdentity {
            session_id: spec.session_id.clone(),
            project_id: spec.project_id.clone(),
            thread_id: spec.thread_id,
            engine_id: spec.engine_id,
        };
        let client = BrowserWebModelBridgeClient::new(
            Arc::clone(&self.driver),
            Arc::clone(&self.slots),
            WebSlotOwner::new(spec.session_id, spec.project_id),
            identity,
            WebModelClientConfig {
                mode: binding.mode,
                remote_conversation_id: binding.remote_conversation_id,
                context_established: binding.mode == magi_web_model::WebConversationMode::Temporary
                    && binding.sync_state == magi_web_model::WebConversationSyncState::Active,
                ..Default::default()
            },
        )
        .with_runtime(Arc::clone(&self.runtime));
        let client = match &self.saved_sink {
            Some(sink) => client.with_saved_sink(Arc::clone(sink)),
            None => client,
        };
        let client = match &self.image_sink {
            Some(sink) => client.with_image_sink(Arc::clone(sink)),
            None => client,
        };
        let client = match &self.question_sink {
            Some(sink) => client.with_question_sink(Arc::clone(sink)),
            None => client,
        };
        Ok(Arc::new(client))
    }
}

impl magi_plugin_system::SessionEngineFactory for WebModelHostFactory {
    fn supports(&self, engine_id: &str) -> bool {
        magi_web_model::is_chatgpt_web_engine_id(engine_id)
    }

    fn build_session_engine(
        &self,
        spec: magi_plugin_system::SessionEngineInvocationSpec,
    ) -> Result<Arc<dyn magi_bridge_client::ModelBridgeClient>, String> {
        self.build_web_model_client(spec)
    }
}

/// 一次发送的暂存图片文件；drop 时无条件删除。
struct StagedUploads {
    paths: Vec<PathBuf>,
}

impl StagedUploads {
    fn stage(dir: &Path, images: &[ImageSource]) -> Result<Self, WebModelError> {
        use base64::Engine as _;
        let attach_failed =
            |message: String| WebModelError::new(WebModelErrorCode::WebAttachFailed, message);
        std::fs::create_dir_all(dir)
            .map_err(|error| attach_failed(format!("无法创建图片暂存目录：{error}")))?;
        let mut staged = Self { paths: Vec::new() };
        for (index, image) in images.iter().enumerate() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(image.data.trim())
                .map_err(|_| attach_failed(format!("第 {} 张图片不是有效的 base64", index + 1)))?;
            // 跨进程也不复用文件名，避免站点给历史同名上传添加后缀后丢失附件归属。
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce)
                .map_err(|error| attach_failed(format!("无法生成图片暂存文件名：{error}")))?;
            let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = dir.join(format!("{name}.{}", image_extension(&image.media_type)));
            write_private_file(&path, &bytes)
                .map_err(|error| attach_failed(format!("无法写入图片暂存文件：{error}")))?;
            staged.paths.push(path);
        }
        Ok(staged)
    }

    fn paths(&self) -> Vec<String> {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }
}

impl Drop for StagedUploads {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn image_extension(media_type: &str) -> &'static str {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "img",
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

fn sweep_upload_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn unavailable(message: &str) -> WebModelError {
    WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, message)
}
fn invalid(message: &str) -> WebModelError {
    WebModelError::new(WebModelErrorCode::WebWriteNotConfirmed, message)
}
/// Surface 交接类错误：页面导航 / 调试会话换代，命令可能已经在页面里生效。
fn can_retry_setup_before_input(outcome: &BrowserHostCommandOutcome) -> bool {
    matches!(outcome, BrowserHostCommandOutcome::Failed(error)
        if !error.side_effect_started && is_surface_handoff(&error.code))
}

fn is_surface_handoff(message: &str) -> bool {
    [
        "browser_surface_stale",
        "browser_surface_not_found",
        "browser_cdp_session_stale",
        "browser_debugger_detached",
        "browser_navigation_superseded",
    ]
    .iter()
    .any(|code| message.contains(code))
}

fn host_error(error: BrowserHostClientError) -> WebModelError {
    WebModelError::new(WebModelErrorCode::WebDesktopUnavailable, error.to_string())
}

fn state_from_json(value: &serde_json::Value) -> Result<TurnState, WebModelError> {
    let login = match value
        .get("login_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("signed_out")
    {
        "signed_in" => LoginState::SignedIn,
        "blocked" => LoginState::Blocked,
        _ => LoginState::SignedOut,
    };
    Ok(TurnState {
        login_state: login,
        blocked: value
            .get("blocked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        generating: value
            .get("generating")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        composer_found: value
            .get("composer_found")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        user_message_count: value
            .get("user_message_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        assistant_message_count: value
            .get("assistant_message_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        assistant_text: value
            .get("assistant_text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        thinking_text: value
            .get("thinking_text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        question: value
            .get("question")
            .filter(|value| !value.is_null())
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|error| invalid(&format!("网页问题结构无效：{error}")))
            })
            .transpose()?,
        last_message_role: value
            .get("last_message_role")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        last_message_text: value
            .get("last_message_text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        StagedUploads, can_retry_setup_before_input, is_surface_handoff, sweep_upload_dir,
    };
    use magi_bridge_client::llm_types::ImageSource;
    use magi_web_model::WebModelErrorCode;

    #[test]
    fn setup_retries_only_explicit_pre_input_surface_failures() {
        use magi_browser_authority::{BrowserHostCommandError, BrowserHostCommandOutcome};
        let error = BrowserHostCommandError {
            code: "browser_surface_stale".into(),
            message: "stale".into(),
            recoverable: true,
            side_effect_started: false,
            diagnostic: None,
        };
        assert!(can_retry_setup_before_input(
            &BrowserHostCommandOutcome::Failed(error.clone())
        ));
        assert!(!can_retry_setup_before_input(
            &BrowserHostCommandOutcome::Indeterminate(error.clone())
        ));
        assert!(!can_retry_setup_before_input(
            &BrowserHostCommandOutcome::Failed(BrowserHostCommandError {
                side_effect_started: true,
                ..error.clone()
            })
        ));
        assert!(!can_retry_setup_before_input(
            &BrowserHostCommandOutcome::Failed(BrowserHostCommandError {
                code: "browser_user_control_held".into(),
                ..error
            })
        ));
        assert!(!can_retry_setup_before_input(
            &BrowserHostCommandOutcome::Cancelled
        ));
    }

    fn png(data: &str) -> ImageSource {
        ImageSource {
            kind: "base64".to_string(),
            media_type: "image/png".to_string(),
            data: data.to_string(),
        }
    }

    #[test]
    fn staged_uploads_are_private_files_removed_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staged = StagedUploads::stage(dir.path(), &[png("AQID"), png("AQID")]).expect("stage");
        let paths = staged.paths();
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1], "同一张图连发两次也不能共用文件");
        for path in &paths {
            assert_eq!(std::fs::read(path).expect("staged file"), vec![1, 2, 3]);
            assert!(path.ends_with(".png"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(path).expect("meta").permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
        drop(staged);
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0);
    }

    #[test]
    fn invalid_base64_fails_as_attach_failure_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = StagedUploads::stage(dir.path(), &[png("AQID"), png("not base64 !!")])
            .err()
            .expect("must fail");
        assert_eq!(error.code, WebModelErrorCode::WebAttachFailed);
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0);
    }

    #[test]
    fn startup_sweep_removes_leftovers_from_a_crashed_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("leftover.png"), b"x").expect("write");
        sweep_upload_dir(dir.path());
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0);
    }

    #[test]
    fn only_surface_handoff_errors_count_as_a_possibly_submitted_message() {
        // 点击提交后页面内路由换代：命令可能已经生效，不能当作「消息未被接受」。
        assert!(is_surface_handoff(
            "browser_surface_stale: binding is older than the current navigation"
        ));
        assert!(is_surface_handoff("browser_cdp_session_stale"));
        assert!(is_surface_handoff("browser_navigation_superseded"));
        // 页面脚本自己的失败（例如提交按钮被禁用）才是明确的拒绝。
        assert!(!is_surface_handoff(
            "web_submit_unavailable:submit button is disabled"
        ));
        assert!(!is_surface_handoff("web_composer_selection_unavailable"));
    }
}
